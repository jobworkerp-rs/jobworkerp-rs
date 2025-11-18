use super::PluginLoader;
use crate::runner::plugins::{impls::PluginRunnerWrapperImpl, PluginRunner};
use crate::runner::timeout_config::RunnerTimeoutConfig;
use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use libloading::{Library, Symbol};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock as TokioRwLock;

#[allow(improper_ctypes_definitions)]
type LoaderFunc<'a> = Symbol<'a, extern "C" fn() -> Box<dyn PluginRunner + Send + Sync>>;

// Global library cache for plugin libraries (physical memory management)
static PLUGIN_LIBRARY_CACHE: OnceLock<TokioRwLock<HashMap<PathBuf, &'static Library>>> =
    OnceLock::new();

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

    // find plugin (not loaded. reference only. cannot run)
    pub async fn find_plugin_runner_by_name(&self, name: &str) -> Option<PluginRunnerWrapperImpl> {
        // Search only in logically "available" plugins
        let path = self
            .active_plugins
            .iter()
            .find(|p| p.0.as_str() == name)
            .map(|(_, _, path)| path.clone())?;

        // Get from cache (should be already loaded)
        let lib = Self::get_or_load_library(&path).await.ok()?;

        // Instantiate with timeout
        let timeout_config = RunnerTimeoutConfig::global();
        let name_clone = name.to_string();
        tokio::time::timeout(
            timeout_config.plugin_instantiate,
            tokio::task::spawn_blocking(move || unsafe {
                let load_plugin: Symbol<LoaderFunc> = lib.get(b"load_plugin\0").ok()?;
                let plugin = load_plugin();
                // Use tokio::sync::RwLock for PluginRunnerWrapperImpl
                Some(PluginRunnerWrapperImpl::new(Arc::new(TokioRwLock::new(
                    plugin,
                ))))
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
        let timeout_config = RunnerTimeoutConfig::global();
        let (plugin_name, description) = tokio::time::timeout(
            timeout_config.plugin_instantiate,
            tokio::task::spawn_blocking(move || unsafe {
                let load_plugin: Symbol<LoaderFunc> = lib.get(b"load_plugin\0")?;
                let plugin = load_plugin();
                Ok::<_, anyhow::Error>((plugin.name(), plugin.description()))
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
