// use std::sync::{Arc, RwLock};
// use super::super::Runner;
// use anyhow::{anyhow, Result};
// use async_trait::async_trait;
// use command_utils::util::result::Flatten;
// use infra::infra::plugins::runner::PluginRunner;

// /**
//  * PluginRunner wrapper
//  * (for self mutability (run(), cancel()))
//  */
// #[derive(Clone)]
// pub struct PluginRunnerWrapperImpl {
//     #[allow(clippy::borrowed_box)]
//     plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>,
// }

// impl PluginRunnerWrapperImpl {
//     #[allow(clippy::borrowed_box)]
//     pub fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
//         Self { plugin_runner }
//     }
// }

// #[async_trait]
// impl Runner for PluginRunnerWrapperImpl {
//     async fn name(&self) -> String {
//         let plugin_runner = Arc::clone(&self.plugin_runner);
//         let n = plugin_runner
//             .read()
//             .map(|p| p.name())
//             .unwrap_or_else(|e| format!("Error occurred: {:}", e));
//         format!("PluginRunnerWrapper: {}", &n)
//     }
//     // arg: assumed as utf-8 string, specify multiple arguments with \n separated
//     #[allow(unstable_name_collisions)]
//     async fn run<'a>(&'a mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
//         // XXX clone
//         let plugin_runner = self.plugin_runner.clone();
//         let arg1 = arg.to_vec();
//         tokio::task::spawn_blocking(move || {
//             match plugin_runner
//                 .write()
//                 .map_err(|e| anyhow::anyhow!("plugin runner lock error: {:?}", e))
//             {
//                 Ok(mut runner) => runner.run(arg1).map_err(|e| {
//                     tracing::warn!("in running pluginRunner: {:?}", e);
//                     anyhow!("in running pluginRunner: {:?}", e)
//                 }),
//                 Err(e) => Err(e),
//             }
//         })
//         .await
//         .map_err(|e| e.into())
//         .flatten()
//     }

//     async fn cancel(&mut self) {
//         let _ = self.plugin_runner.write().map(|r| r.cancel());
//     }
//     fn operation_proto(&self) -> String {
//         let plugin_runner = Arc::clone(&self.plugin_runner);
//         plugin_runner
//             .read()
//             .map(|p| p.operation_proto())
//             .unwrap_or_else(|e| format!("Error occurred: {:}", e))
//     }
//     fn job_args_proto(&self) -> String {
//         let plugin_runner = Arc::clone(&self.plugin_runner);
//         plugin_runner
//             .read()
//             .map(|p| p.job_args_proto())
//             .unwrap_or_else(|e| format!("Error occurred: {:}", e))
//     }
//     fn use_job_result(&self) -> bool {
//         let plugin_runner = Arc::clone(&self.plugin_runner);
//         plugin_runner
//             .read()
//             .map(|p| p.use_job_result())
//             .unwrap_or_else(|e| {
//                 tracing::warn!("Error occurred: {:}", e);
//                 false
//             })
//     }
// }
