use std::sync::{Arc, RwLock};

use super::super::Runner;
use crate::plugins::runner::PluginRunner;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use common::util::result::Flatten;

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
    #[allow(clippy::borrowed_box)]
    pub fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
        Self { plugin_runner }
    }
}

#[async_trait]
impl Runner for PluginRunnerWrapperImpl {
    async fn name(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        let n = plugin_runner
            .read()
            .map(|p| p.name())
            .unwrap_or_else(|e| format!("Error occurred: {:}", e));
        format!("PluginRunnerWrapper: {}", &n)
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    #[allow(unstable_name_collisions)]
    async fn run<'a>(&'a mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let plugin_runner = self.plugin_runner.clone();
        tokio::task::spawn_blocking(move || {
            match plugin_runner
                .write()
                .map_err(|e| anyhow::anyhow!("plugin runner lock error: {:?}", e))
            {
                Ok(mut runner) => runner.run(arg).map_err(|e| {
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
        let _ = self.plugin_runner.write().map(|r| r.cancel());
    }
}
