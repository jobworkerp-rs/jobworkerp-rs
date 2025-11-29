pub mod impls;
pub mod loader;

use self::loader::RunnerPluginLoader;
use crate::schema_to_json_string;
use anyhow::Result;
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use std::{
    collections::HashMap,
    fs::{self, ReadDir},
    path::Path,
    sync::Arc,
};
use tokio::sync::RwLock as TokioRwLock;

pub struct PluginMetadata {
    pub name: String,
    pub description: String,
    pub filename: String,
}

/// Plugin loader trait for managing dynamic library loading
///
/// # Important Notes on Memory Management
///
/// Due to the unsafe nature of dynamic library unloading (see libloading discussion:
/// https://users.rust-lang.org/t/how-to-avoid-library-and-symbol-drops-in-crate-libloading/85701),
/// implementations of this trait typically keep loaded libraries in memory indefinitely.
///
/// This means:
/// - `unload()`: Only removes logical registration, physical library remains in memory
/// - `overwrite=true` in `load_path()`: Creates duplicate physical libraries in memory
/// - Memory accumulation is intentional to prevent crashes from dangling references
#[async_trait::async_trait]
pub trait PluginLoader: Send + Sync {
    /// Load a plugin from the specified path
    ///
    /// # Parameters
    /// - `name`: Optional custom name for the plugin (uses plugin's internal name if None)
    /// - `path`: Path to the dynamic library file
    /// - `overwrite`: If true, allows re-registering a plugin with the same name
    ///
    /// # Overwrite Behavior Warning
    /// When `overwrite=true`, only the logical plugin registration is updated.
    /// The physical library file from the old path remains loaded in memory.
    /// This is a safety tradeoff to prevent crashes but leads to memory accumulation.
    ///
    /// # Returns
    /// Tuple of (plugin_name, plugin_description) on success
    async fn load_path(
        &mut self,
        name: Option<&str>,
        path: &Path,
        overwrite: bool,
    ) -> Result<(String, String)>;

    /// Unload a plugin by name (logical removal only)
    ///
    /// NOTE: This only removes the logical registration. The physical library
    /// remains in memory to prevent crashes from dangling references.
    fn unload(&mut self, name: &str) -> Result<bool>;

    /// Clear all logical plugin registrations
    ///
    /// NOTE: Physical libraries remain in memory
    #[allow(dead_code)]
    fn clear(&mut self) -> Result<()>;
}

#[derive(Debug)]
enum PluginType {
    Runner,
}

#[derive(Debug)]
pub struct Plugins {
    runner_loader: Arc<TokioRwLock<RunnerPluginLoader>>,
}
impl Default for Plugins {
    fn default() -> Self {
        Self::new()
    }
}
impl Plugins {
    pub fn new() -> Self {
        Plugins {
            runner_loader: Arc::new(TokioRwLock::new(RunnerPluginLoader::new())),
        }
    }
    pub async fn load_plugin_files(&self, runner_dir_str: &str) -> Vec<PluginMetadata> {
        // default: current dir
        let runner_dirs: Vec<&str> = runner_dir_str.split(',').collect_vec();
        let mut loaded = Vec::new();
        for runner_dir in runner_dirs {
            if let Ok(runner_path) = fs::read_dir(runner_dir) {
                loaded.extend(
                    self.load_plugin_files_from(runner_path, PluginType::Runner)
                        .await,
                );
            } else {
                tracing::warn!("runner plugin dir not found: {}", runner_dir);
            }
        }
        loaded
    }
    pub async fn load_plugin_file(
        &self,
        name: Option<&str>,
        file: &str,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        let path = Path::new(file);
        self.load_plugin_from_path(name, path, &PluginType::Runner, overwrite)
            .await
    }

    fn get_library_extension() -> &'static str {
        if cfg!(target_os = "windows") {
            ".dll"
        } else if cfg!(target_os = "macos") {
            ".dylib"
        } else if cfg!(target_os = "linux") {
            ".so"
        } else {
            tracing::error!("Unsupported operating system");
            ".so"
        }
    }

    // return: (name, file_name)
    async fn load_plugin_files_from(&self, dir: ReadDir, ptype: PluginType) -> Vec<PluginMetadata> {
        let mut loaded = Vec::new();
        for file in dir.flatten() {
            if file.path().is_file()
                && file
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(Self::get_library_extension()))
            {
                // use plugin name defined in plugin as plugin name
                if let Ok(plugin) = self
                    .load_plugin_from_path(None, &file.path(), &ptype, false)
                    .await
                {
                    loaded.push(plugin);
                } else {
                    tracing::warn!("cannot load plugin: {:?}", file.path());
                }
            } else if !file.path().exists() {
                tracing::warn!("file not found: {:?}", file.path());
            } else if !file.path().is_file() {
                tracing::warn!("not a file: {:?}", file.path());
            } else if file.file_name().to_str().is_none() {
                tracing::warn!("cannot convert file name to str: {:?}", file.path());
            } else if !file
                .file_name()
                .to_str()
                .unwrap()
                .ends_with(Self::get_library_extension())
            {
                tracing::warn!(
                    "not a plugin file (wrong extension): expect: {}, real:{:?}",
                    Self::get_library_extension(),
                    file.path()
                );
            } else {
                tracing::warn!("not a file: {:?}", file.path());
            }
        }
        loaded
    }
    async fn load_plugin_from_path(
        &self,
        name: Option<&str>,
        path: &Path,
        ptype: &PluginType,
        overwrite: bool,
    ) -> Result<PluginMetadata> {
        if path.is_file()
            && path
                .file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|n| n.ends_with(Self::get_library_extension()))
        {
            tracing::info!("load {:?} plugin file: {}", ptype, path.display());
            match ptype {
                PluginType::Runner => self
                    .runner_loader
                    .write()
                    .await
                    .load_path(name, path, overwrite)
                    .await
                    .map(|(name, description)| PluginMetadata {
                        name,
                        description,
                        filename: path.to_string_lossy().to_string(),
                    }),
            }
        } else {
            Err(JobWorkerError::InvalidParameter(format!("not a file: {path:?}")).into())
        }
    }
    pub fn runner_plugins(&self) -> Arc<TokioRwLock<RunnerPluginLoader>> {
        self.runner_loader.clone()
    }
}

//TODO function load, run, cancel to async
pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>);
    // run for generating stream
    fn begin_stream(&mut self, arg: Vec<u8>, metadata: HashMap<String, String>) -> Result<()> {
        // default implementation (return empty)
        let (_, _) = (arg, metadata);
        Err(anyhow::anyhow!("not implemented"))
    }
    // receive stream generated by run_stream()
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        // default implementation (return empty)
        Err(anyhow::anyhow!("not implemented"))
    }
    fn cancel(&mut self) -> bool;
    fn is_canceled(&self) -> bool;
    fn runner_settings_proto(&self) -> String;

    /// Phase 6.6.4: Returns the method protobuf schema map for all plugins
    /// Key: method name (typically DEFAULT_METHOD_NAME ("run")), Value: MethodSchema (input and output schemas)
    /// This is the unified approach for defining plugin method schemas
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        HashMap::new()
    }

    /// Phase 6.7: Returns JSON Schema map for plugin methods
    ///
    /// **Default implementation**: Returns None to use automatic Protobuf→JSON Schema conversion
    ///
    /// **Override when**: Plugin has oneof fields that require oneOf constraints in JSON Schema
    ///
    /// # Example
    /// ```rust
    /// fn method_json_schema_map(&self) -> Option<HashMap<String, jobworkerp_runner::runner::MethodJsonSchema>> {
    ///     let mut schemas = HashMap::new();
    ///     schemas.insert(
    ///         "run".to_string(),
    ///         jobworkerp_runner::runner::MethodJsonSchema {
    ///             args_schema: include_str!("../schema/MyPluginArgs.json").to_string(),
    ///             result_schema: Some(include_str!("../schema/MyPluginResult.json").to_string()),
    ///         },
    ///     );
    ///     Some(schemas)
    /// }
    /// ```
    fn method_json_schema_map(&self) -> Option<HashMap<String, crate::runner::MethodJsonSchema>> {
        None // Default: use automatic conversion from method_proto_map()
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plugins_initialization() {
        let plugins = Plugins::new();

        // Test that runner loader is initialized
        let runner_loader = plugins.runner_plugins();
        let loader_guard = runner_loader.read().await;

        // Should not panic and should be empty initially
        drop(loader_guard);
    }

    #[tokio::test]
    async fn test_plugin_loader_directory_scan() {
        let plugins = Plugins::new();

        // Test scanning a non-existent directory
        let loaded = plugins.load_plugin_files("/non_existent_path").await;

        // Should return empty vec for non-existent directory
        assert!(loaded.is_empty());
    }

    #[tokio::test]
    async fn test_plugin_metadata() {
        let metadata = PluginMetadata {
            name: "test_plugin".to_string(),
            description: "Test plugin for unit testing".to_string(),
            filename: "test_plugin.so".to_string(),
        };

        assert_eq!(metadata.name, "test_plugin");
        assert_eq!(metadata.description, "Test plugin for unit testing");
        assert_eq!(metadata.filename, "test_plugin.so");
    }

    #[tokio::test]
    async fn test_plugin_system_initialization() {
        // Test basic plugin system initialization without loading actual plugins
        let plugins = Plugins::new();

        // Test that we can access the runner loader
        let loader = plugins.runner_plugins();
        let loader_guard = loader.read().await;
        drop(loader_guard);

        // Test that plugin files loading works with invalid path
        let loaded = plugins
            .load_plugin_files("invalid/path/that/does/not/exist")
            .await;
        assert!(
            loaded.is_empty(),
            "Should return empty vec for non-existent directory"
        );
    }
}
