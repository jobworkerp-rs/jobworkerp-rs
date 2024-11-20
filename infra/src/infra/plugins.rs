use super::runner::plugins::loader::RunnerPluginLoader;
use anyhow::Result;
use command_utils::util::option::Exists;
use itertools::Itertools;
use std::{
    env,
    fs::{self, ReadDir},
    path::Path,
    sync::Arc,
};
use tokio::sync::RwLock as TokioRwLock;

pub(super) trait PluginLoader: Send + Sync {
    fn load_path(&mut self, path: &Path) -> Result<String>;
    fn unload(&mut self, name: &str) -> Result<bool>;
    #[allow(dead_code)]
    fn clear(&mut self) -> Result<()>;
}

#[derive(Debug)]
enum PluginType {
    Runner,
}

#[derive(Debug)]
pub(super) struct Plugins {
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

    pub async fn load_plugin_files_from_env(&self) -> Vec<(String, String)> {
        // default: current dir
        let runner_dir_str = env::var("PLUGINS_RUNNER_DIR").unwrap_or("./".to_string());
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

    fn get_library_extension() -> &'static str {
        if cfg!(target_os = "windows") {
            ".dll"
        } else if cfg!(target_os = "macos") {
            ".dylib"
        } else if cfg!(target_os = "linux")
            || cfg!(target_os = "freebsd")
            || cfg!(target_os = "openbsd")
            || cfg!(target_os = "netbsd")
        {
            ".so"
        } else {
            tracing::error!("Unsupported operating system");
            ".so"
        }
    }

    // return: (name, file_name)
    async fn load_plugin_files_from(
        &self,
        dir: ReadDir,
        ptype: PluginType,
    ) -> Vec<(String, String)> {
        let mut loaded = Vec::new();
        for file in dir.flatten() {
            if file.path().is_file()
                && file
                    .file_name()
                    .to_str()
                    .exists(|n| n.ends_with(Self::get_library_extension()))
            {
                tracing::info!("load {:?} plugin file: {}", ptype, file.path().display());
                match ptype {
                    PluginType::Runner => {
                        match self
                            .runner_loader
                            .write()
                            .await
                            .load_path(file.path().as_path())
                        {
                            Ok(name) => {
                                tracing::info!("runner plugin loaded: {}", file.path().display());
                                loaded.push((name, file.file_name().to_string_lossy().to_string()));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "cannot load runner plugin: {}: {:?}",
                                    file.path().display(),
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
        loaded
    }
    pub fn runner_plugins(&self) -> Arc<TokioRwLock<RunnerPluginLoader>> {
        self.runner_loader.clone()
    }
}
