pub mod impls;
pub mod loader;

use self::loader::RunnerPluginLoader;
use crate::schema_to_json_string;
use anyhow::Result;
use itertools::Itertools;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::StreamingOutputType;
use std::{
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

pub trait PluginLoader: Send + Sync {
    fn load_path(
        &mut self,
        name: Option<&str>,
        path: &Path,
        overwrite: bool,
    ) -> Result<(String, String)>;
    fn unload(&mut self, name: &str) -> Result<bool>;
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
                    .map(|(name, description)| PluginMetadata {
                        name,
                        description,
                        filename: path.to_string_lossy().to_string(),
                    }),
            }
        } else {
            Err(JobWorkerError::InvalidParameter(format!("not a file: {:?}", path)).into())
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
    fn run(&mut self, args: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    // run for generating stream
    fn begin_stream(&mut self, arg: Vec<u8>) -> Result<()> {
        // default implementation (return empty)
        let _ = arg;
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
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::NonStreaming
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "arguments_schema")
    }
    fn output_json_schema(&self) -> Option<String> {
        None
    }
}
