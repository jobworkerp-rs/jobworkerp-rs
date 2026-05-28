use super::PluginLoader;
use crate::runner::plugins::ffi::{
    PLUGIN_V2_ABI_MAJOR, PLUGIN_V2_ABI_MINOR, PluginInstance, PluginInstanceRaw, PluginVtable,
    VTABLE_SIZE_MAX, VTABLE_SIZE_MIN,
};
use crate::runner::plugins::{
    MultiMethodPluginRunner, PluginRunner, PluginRunnerVariant, impls::PluginRunnerWrapperImpl,
};
use crate::runner::timeout_config::RunnerTimeoutConfig;
use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock as TokioRwLock;

/// FFI symbol for legacy plugins (origin/main compatible)
#[allow(improper_ctypes_definitions)]
type LoaderFunc<'a> = Symbol<'a, extern "C" fn() -> Box<dyn PluginRunner + Send + Sync>>;

/// FFI symbol for multi-method plugins
#[allow(improper_ctypes_definitions)]
type MultiMethodLoaderFunc<'a> =
    Symbol<'a, extern "C" fn() -> Box<dyn MultiMethodPluginRunner + Send + Sync>>;

/// FFI symbol for V2 multi-method plugins. Returns a `#[repr(C)]`
/// `PluginInstanceRaw`; the loader validates the vtable header before
/// wrapping in a `PluginInstance`. The symbol name is kept stable so
/// existing plugin source code does not have to change, but the return
/// type is incompatible with the previous V2 trait object — `.so` files
/// built against the old V2 ABI are rejected by the vtable validation
/// below.
#[allow(improper_ctypes_definitions)]
type MultiMethodLoaderFuncV2<'a> = Symbol<'a, extern "C" fn() -> PluginInstanceRaw>;

// Global library cache for plugin libraries (physical memory management)
static PLUGIN_LIBRARY_CACHE: OnceLock<TokioRwLock<HashMap<PathBuf, &'static Library>>> =
    OnceLock::new();

/// Validate a V2 plugin's vtable header before trusting the rest of the
/// instance. Rejects:
///   * null or misaligned vtable pointers (most common signal of an old
///     `Box<dyn Trait>` V2 plugin being misinterpreted under the new
///     symbol contract).
///   * `vtable_size` outside the documented range — anything below the
///     minimum or above the maximum points at memory corruption rather
///     than a legitimate plugin.
///   * ABI major mismatch — plugin must match the host's major version.
///   * Plugin minor version newer than host: host cannot promise to
///     support method slots the plugin assumes are present.
///   * Plugin's `vtable_size` larger than the host's struct size — host
///     would interpret out-of-bounds memory as method pointers.
fn validate_v2_vtable(raw: &PluginInstanceRaw) -> Result<()> {
    let vtable_ptr = raw.vtable as *const PluginVtable;
    if vtable_ptr.is_null() || (vtable_ptr as usize) < 4096 {
        return Err(anyhow::anyhow!(
            "V2 vtable pointer looks invalid ({:p}); likely an old V2 plugin",
            vtable_ptr
        ));
    }
    let plugin_major = (raw.vtable.abi_version >> 16) as u16;
    let plugin_minor = (raw.vtable.abi_version & 0xFFFF) as u16;
    let host_size = std::mem::size_of::<PluginVtable>() as u32;
    let plugin_size = raw.vtable.vtable_size;

    if plugin_major != PLUGIN_V2_ABI_MAJOR {
        return Err(anyhow::anyhow!(
            "V2 plugin major ABI mismatch: plugin {}, host {}",
            plugin_major,
            PLUGIN_V2_ABI_MAJOR
        ));
    }
    if plugin_minor > PLUGIN_V2_ABI_MINOR {
        return Err(anyhow::anyhow!(
            "V2 plugin minor ABI too new: plugin {}, host {}",
            plugin_minor,
            PLUGIN_V2_ABI_MINOR
        ));
    }
    if plugin_size > host_size {
        return Err(anyhow::anyhow!(
            "V2 plugin vtable_size too large: plugin {}, host {}",
            plugin_size,
            host_size
        ));
    }
    if !(VTABLE_SIZE_MIN..=VTABLE_SIZE_MAX).contains(&plugin_size) {
        return Err(anyhow::anyhow!(
            "V2 plugin vtable_size out of range: {} (allowed {}..={})",
            plugin_size,
            VTABLE_SIZE_MIN,
            VTABLE_SIZE_MAX
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub struct RunnerPluginLoader {
    // Logically "available" plugins list (name, description, path)
    active_plugins: Vec<(String, String, PathBuf)>,
}

impl RunnerPluginLoader {
    pub fn new() -> Self {
        RunnerPluginLoader {
            active_plugins: Vec::new(),
        }
    }

    /// Get list of active plugin names
    pub fn active_plugin_names(&self) -> Vec<String> {
        self.active_plugins
            .iter()
            .map(|(name, _, _)| name.clone())
            .collect()
    }

    /// Get list of active plugin info (name, description, path)
    pub fn active_plugin_info(&self) -> &Vec<(String, String, PathBuf)> {
        &self.active_plugins
    }

    pub fn exists(&self, name: &str) -> bool {
        self.active_plugins.iter().any(|p| p.0.as_str() == name)
    }

    // Helper: Get or load library from global cache
    async fn get_or_load_library(path: &Path) -> Result<&'static Library> {
        let cache = PLUGIN_LIBRARY_CACHE.get_or_init(|| TokioRwLock::new(HashMap::new()));

        // Read lock to check cache
        {
            let cache_guard = cache.read().await;
            if let Some(lib) = cache_guard.get(path) {
                tracing::debug!("Plugin library cache hit: {:?}", path);
                return Ok(*lib);
            }
        }

        // Write lock to load new library
        let mut cache_guard = cache.write().await;

        // Double-check (another thread might have loaded it)
        if let Some(lib) = cache_guard.get(path) {
            return Ok(*lib);
        }

        tracing::info!("Loading plugin library (will remain in memory): {:?}", path);

        let path_clone = path.to_path_buf();
        let timeout_config = RunnerTimeoutConfig::global();
        let lib = tokio::time::timeout(
            timeout_config.plugin_load,
            tokio::task::spawn_blocking(move || unsafe {
                let lib = Library::new(&path_clone)?;
                // Box::leak() to promote to 'static lifetime
                Ok::<_, anyhow::Error>(Box::leak(Box::new(lib)) as &'static Library)
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Library load timeout after {:?}: {:?}",
                timeout_config.plugin_load,
                path
            )
        })???;

        cache_guard.insert(path.to_path_buf(), lib);
        Ok(lib)
    }

    /// Find plugin by name and create wrapper instance
    ///
    /// Priority order for FFI symbol detection:
    /// 1. `load_multi_method_plugin_v2` - V2 multi-method plugins (cooperative cancellation)
    /// 2. `load_multi_method_plugin` - Multi-method plugins (supports multiple methods + collect_stream)
    /// 3. `load_plugin` - Legacy plugins (origin/main compatible, no collect_stream)
    pub async fn find_plugin_runner_by_name(&self, name: &str) -> Option<PluginRunnerWrapperImpl> {
        // Search only in logically "available" plugins
        let path = self
            .active_plugins
            .iter()
            .find(|p| p.0.as_str() == name)
            .map(|(_, _, path)| path.clone())?;

        let lib = Self::get_or_load_library(&path).await.ok()?;

        // Instantiate with timeout
        let timeout_config = RunnerTimeoutConfig::global();
        let name_clone = name.to_string();
        tokio::time::timeout(
            timeout_config.plugin_instantiate,
            tokio::task::spawn_blocking(move || unsafe {
                // Try V2 multi-method plugin first (highest priority: cooperative cancellation)
                if let Ok(load_v2) =
                    lib.get::<MultiMethodLoaderFuncV2>(b"load_multi_method_plugin_v2\0")
                {
                    let raw = load_v2();
                    match validate_v2_vtable(&raw) {
                        Ok(()) => {
                            let inst = PluginInstance {
                                state: raw.state,
                                vtable: raw.vtable,
                                library: lib,
                            };
                            let variant = PluginRunnerVariant::MultiMethodV2(inst);
                            return Some(PluginRunnerWrapperImpl::new(Arc::new(TokioRwLock::new(
                                variant,
                            ))));
                        }
                        Err(e) => {
                            tracing::error!("V2 plugin rejected: {e}");
                            // Best-effort cleanup if the vtable pointer is valid
                            // enough to invoke. We only call drop_state when the
                            // pointer passes basic sanity, otherwise leaving it
                            // is safer than dereferencing garbage.
                            if (raw.vtable as *const PluginVtable as usize) >= 4096 {
                                (raw.vtable.drop_state)(raw.state);
                            }
                            // fall through to V1/legacy probing in case the dylib
                            // exports multiple loader symbols
                        }
                    }
                }

                // Fall back to v1 multi-method plugin
                if let Ok(load_multi) =
                    lib.get::<MultiMethodLoaderFunc>(b"load_multi_method_plugin\0")
                {
                    let plugin = load_multi();
                    let variant = PluginRunnerVariant::MultiMethod(plugin);
                    return Some(PluginRunnerWrapperImpl::new(Arc::new(TokioRwLock::new(
                        variant,
                    ))));
                }

                // Fall back to legacy plugin
                if let Ok(load_legacy) = lib.get::<LoaderFunc>(b"load_plugin\0") {
                    let plugin = load_legacy();
                    let variant = PluginRunnerVariant::Legacy(plugin);
                    return Some(PluginRunnerWrapperImpl::new(Arc::new(TokioRwLock::new(
                        variant,
                    ))));
                }

                None
            }),
        )
        .await
        .map_err(|_| {
            tracing::error!(
                "Plugin '{}' instantiation timeout after {:?}",
                name_clone,
                timeout_config.plugin_instantiate
            );
        })
        .ok()?
        .ok()?
    }
}

impl Default for RunnerPluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PluginLoader for RunnerPluginLoader {
    /// Load a plugin from the specified path
    ///
    /// # Memory Management and Overwrite Behavior
    ///
    /// **IMPORTANT**: This implementation uses `Box::leak()` to intentionally keep loaded
    /// libraries in memory for the lifetime of the process. This is based on the discussion
    /// in the Rust Users Forum:
    /// https://users.rust-lang.org/t/how-to-avoid-library-and-symbol-drops-in-crate-libloading/85701
    ///
    /// Key points from the discussion:
    /// - `libloading::Library` automatically unloads when dropped, which can cause crashes
    ///   if any references to the library's code or data still exist
    /// - Platform-specific issues: On macOS, libraries using thread-local storage cannot
    ///   be safely unloaded
    /// - Alternative approaches (Arc<Library>) provide flexibility but don't guarantee
    ///   safety across all platforms
    /// - The consensus is to accept memory leaks for plugin systems to ensure safety
    ///
    /// **Overwrite Parameter Limitation**:
    /// - `overwrite=true`: Only updates the logical plugin registration (name, path mapping)
    /// - The physical library from the old path **remains in memory** indefinitely
    /// - Both old and new library files will be loaded in `PLUGIN_LIBRARY_CACHE`
    /// - This is a safety tradeoff - memory usage increases, but avoids potential crashes
    /// - In practice, `overwrite` should be used sparingly, as it leads to memory accumulation
    async fn load_path(
        &mut self,
        name: Option<&str>,
        path: &Path,
        overwrite: bool,
    ) -> Result<(String, String)> {
        // 1. Load library physically (or get from cache)
        let lib = Self::get_or_load_library(path).await?;

        // 2. Get plugin info with timeout
        // Priority: load_multi_method_plugin > load_plugin
        let timeout_config = RunnerTimeoutConfig::global();
        let (plugin_name, description) = tokio::time::timeout(
            timeout_config.plugin_instantiate,
            tokio::task::spawn_blocking(move || unsafe {
                // Try V2 multi-method plugin first
                if let Ok(load_v2) =
                    lib.get::<MultiMethodLoaderFuncV2>(b"load_multi_method_plugin_v2\0")
                {
                    let raw = load_v2();
                    if let Err(e) = validate_v2_vtable(&raw) {
                        tracing::error!("V2 plugin rejected during load_path: {e}");
                        if (raw.vtable as *const PluginVtable as usize) >= 4096 {
                            (raw.vtable.drop_state)(raw.state);
                        }
                        // fall through to V1/legacy probing
                    } else {
                        let name_bytes = (raw.vtable.name)(raw.state);
                        let desc_bytes = (raw.vtable.description)(raw.state);
                        let name = String::from_utf8_lossy(name_bytes.as_slice()).into_owned();
                        let desc = String::from_utf8_lossy(desc_bytes.as_slice()).into_owned();
                        // Drop the temporary instance state; the actual instance
                        // for the runner is created lazily inside
                        // `find_plugin_runner_by_name`.
                        (raw.vtable.drop_state)(raw.state);
                        return Ok::<_, anyhow::Error>((name, desc));
                    }
                }

                // Fall back to v1 multi-method plugin
                if let Ok(load_multi) =
                    lib.get::<MultiMethodLoaderFunc>(b"load_multi_method_plugin\0")
                {
                    let plugin = load_multi();
                    return Ok::<_, anyhow::Error>((plugin.name(), plugin.description()));
                }

                // Fall back to legacy plugin
                if let Ok(load_legacy) = lib.get::<LoaderFunc>(b"load_plugin\0") {
                    let plugin = load_legacy();
                    return Ok::<_, anyhow::Error>((plugin.name(), plugin.description()));
                }

                Err(anyhow::anyhow!(
                    "Plugin missing all FFI symbols (load_plugin, load_multi_method_plugin, load_multi_method_plugin_v2)"
                ))
            }),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Plugin info fetch timeout after {:?}: {:?}",
                timeout_config.plugin_instantiate,
                path
            )
        })???;

        let name = name.map(|s| s.to_string()).unwrap_or(plugin_name);

        // 3. Logical duplicate check
        if self
            .active_plugins
            .iter()
            .any(|p| p.0.as_str() == name.as_str())
            && !overwrite
        {
            return Err(JobWorkerError::AlreadyExists(format!(
                "plugin already loaded: {} ({})",
                &name,
                path.display()
            ))
            .into());
        }

        // 4. Overwrite: logical unload
        if overwrite {
            self.unload(&name)?;
        }

        // 5. Add to logical plugin list
        self.active_plugins
            .push((name.clone(), description.clone(), path.to_path_buf()));

        tracing::info!(
            "Plugin '{}' registered (library may be cached in memory)",
            name
        );

        Ok((name, description))
    }

    fn unload(&mut self, name: &str) -> Result<bool> {
        // Logical unload only (memory not freed)
        // Search from the latest loaded plugin
        if let Some((idx, _)) = self
            .active_plugins
            .iter()
            .enumerate()
            .rev()
            .find(|(_, p)| p.0.as_str() == name)
        {
            let (removed_name, _, removed_path) = self.active_plugins.remove(idx);

            tracing::info!(
                "Plugin '{}' unregistered (library remains in memory at {:?})",
                removed_name,
                removed_path
            );

            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn clear(&mut self) -> Result<()> {
        self.active_plugins.clear();
        Ok(())
    }
}

impl Drop for RunnerPluginLoader {
    fn drop(&mut self) {
        // Clear logical plugin list.
        // Physical libraries remain in PLUGIN_LIBRARY_CACHE until program termination.
        self.active_plugins.clear();
    }
}
