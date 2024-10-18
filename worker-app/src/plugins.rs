pub mod runner;

// use anyhow::Result;
// use command_utils::util::option::Exists;
// use libloading::Library;
// use std::{
//     env,
//     fs::{self, ReadDir},
//     path::Path,
// };

// use self::runner::RunnerPluginLoader;

// trait PluginLoader: Send + Sync {
//     fn load(&mut self, path: &Path) -> Result<()>;
// }

// #[derive(Debug)]
// enum PluginType {
//     Runner,
// }

// pub struct Plugins {
//     runner_loader: RunnerPluginLoader,
// }
// impl Plugins {
//     pub fn new() -> Self {
//         Plugins {
//             runner_loader: RunnerPluginLoader::new(),
//         }
//     }
// }
// impl Default for Plugins {
//     fn default() -> Self {
//         Self::new()
//     }
// }

// impl Plugins {
//     pub fn load_plugins_from_env(&mut self) -> Result<()> {
//         match env::var("PLUGINS_RUNNER_DIR") {
//             Ok(runner_dir) => {
//                 if let Ok(runner_path) = fs::read_dir(runner_dir.clone()) {
//                     self.load_plugins_from(runner_path, PluginType::Runner)?;
//                 } else {
//                     tracing::info!("runner plugin dir not found: {}", &runner_dir);
//                 }
//             }
//             Err(e) => {
//                 tracing::info!("cannot read environment var:'PLUGINS_RUNNER_DIR': {:?}", e);
//             }
//         }
//         Ok(())
//     }

//     fn load_plugins_from(&mut self, dir: ReadDir, ptype: PluginType) -> Result<()> {
//         for file in dir.flatten() {
//             if file.path().is_file() && file.file_name().to_str().exists(|n| n.ends_with(".so")) {
//                 tracing::info!("load {:?} plugin file: {}", ptype, file.path().display());
//                 match ptype {
//                     PluginType::Runner => self.runner_loader.load(file.path().as_path())?,
//                 }
//             }
//         }
//         Ok(())
//     }
//     pub fn runner_plugins(&self) -> &Vec<(String, Library)> {
//         self.runner_loader.plugin_loaders()
//     }
// }
