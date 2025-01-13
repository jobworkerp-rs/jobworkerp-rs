use crate::infra::runner::RunnerTrait;
use std::sync::{Arc, RwLock};

use super::PluginRunner;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::util::result::{Flatten, TapErr};

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Clone)]
pub struct PluginRunnerWrapperImpl {
    #[allow(clippy::borrowed_box)]
    plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>,
}

impl PluginRunnerWrapperImpl {
    // #[allow(clippy::borrowed_box)]
    pub(super) fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
        Self { plugin_runner }
    }
    async fn create(&self, settings: Vec<u8>) -> Result<()> {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(move || {
            plugin_runner
                .write()
                .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))
                .and_then(|mut r| r.load(settings))
        })
        .await
        .map_err(|e| e.into())
        .flatten()?;
        Ok(())
    }
}

#[async_trait]
impl RunnerTrait for PluginRunnerWrapperImpl {
    fn name(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        let n = plugin_runner
            .read()
            .map(|p| p.name())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e));
        n
    }
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        self.create(settings).await?;
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    #[allow(unstable_name_collisions)]
    async fn run(&mut self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        // XXX clone
        let plugin_runner = self.plugin_runner.clone();
        let arg1 = arg.to_vec();
        tokio::task::spawn_blocking(move || {
            match plugin_runner
                .write()
                .map_err(|e| anyhow::anyhow!("plugin runner lock error: {:?}", e))
            {
                Ok(mut runner) => runner.run(arg1).map_err(|e| {
                    tracing::warn!("in running pluginRunner: {:?}", e);
                    anyhow!("in running pluginRunner: {:?}", e)
                }),
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(|e| e.into())
        .flatten()
    }

    async fn cancel(&mut self) {
        let _ = self.plugin_runner.write().map(|mut r| r.cancel());
    }
    fn runner_settings_proto(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.runner_settings_proto())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e))
    }
    fn job_args_proto(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.job_args_proto())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e))
    }
    fn result_output_proto(&self) -> Option<String> {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.result_output_proto())
            .tap_err(|e| tracing::warn!("Error occurred: {:}", e))
            .unwrap_or_default()
    }
    fn use_job_result(&self) -> bool {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        plugin_runner
            .read()
            .map(|p| p.use_job_result())
            .unwrap_or_else(|e| {
                tracing::warn!("Error occurred: {:}", e);
                false
            })
    }
}
