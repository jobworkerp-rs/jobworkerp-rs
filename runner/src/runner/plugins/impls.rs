use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use std::collections::HashMap;
use std::sync::Arc;

use super::PluginRunner;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::Trailer;
use tokio::sync::RwLock;

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
    pub fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
        Self { plugin_runner }
    }
    async fn create(&self, settings: Vec<u8>) -> Result<()> {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(|| async move {
            plugin_runner.write().await.load(settings)
            // .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))
            // .and_then(|mut r| r.load(settings))
        })
        .await
        .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))?
        .await?;
        Ok(())
    }
}

impl RunnerSpec for PluginRunnerWrapperImpl {
    fn name(&self) -> String {
        let plugin_runner = Arc::clone(&self.plugin_runner);
        let n = block_on(plugin_runner.read()).name();
        // .map(|p| p.name())
        // .unwrap_or_else(|e| format!("Error occurred: {:}", e));
        n
    }
    fn runner_settings_proto(&self) -> String {
        // let plugin_runner = Arc::clone(&self.plugin_runner);
        block_on(self.plugin_runner.read()).runner_settings_proto()
        // .map(|p| p.runner_settings_proto())
        // .unwrap_or_else(|e| format!("Error occurred: {:}", e))
    }
    fn job_args_proto(&self) -> String {
        block_on(self.plugin_runner.read()).job_args_proto()
    }
    fn result_output_proto(&self) -> Option<String> {
        block_on(self.plugin_runner.read()).result_output_proto()
    }
    fn output_type(&self) -> StreamingOutputType {
        block_on(self.plugin_runner.read()).output_type()
    }
    fn settings_schema(&self) -> String {
        block_on(self.plugin_runner.read()).settings_schema()
    }
    fn arguments_schema(&self) -> String {
        block_on(self.plugin_runner.read()).arguments_schema()
    }
    fn output_schema(&self) -> Option<String> {
        block_on(self.plugin_runner.read()).output_json_schema()
    }
}
#[async_trait]
impl RunnerTrait for PluginRunnerWrapperImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        self.create(settings).await?;
        Ok(())
    }
    // arg: assumed as utf-8 string, specify multiple arguments with \n separated
    #[allow(unstable_name_collisions)]
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // XXX clone
        // let plugin_runner = Arc::clone(&self.plugin_runner);
        // let arg1 = arg.to_vec();

        // tokio::task::spawn_blocking(move || {
        //     let mut runner = futures::executor::block_on(plugin_runner.write());
        //     runner.run(arg1).map_err(|e| {
        //         tracing::warn!("in running pluginRunner: {:?}", e);
        //         e
        //     })
        // })
        // .await?
        // .map_err(|e| anyhow!("Join error: {:?}", e))

        let plugin_runner = self.plugin_runner.clone();
        let arg1 = arg.to_vec();
        let mut runner = plugin_runner.write().await;

        let (r, meta) = runner.run(arg1, metadata);
        (
            r.map_err(|e| {
                tracing::warn!("in running pluginRunner: {:?}", e);
                // anyhow!("in running pluginRunner: {:?}", e)
                e
            }),
            meta,
        )
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // XXX clone
        let plugin_runner = self.plugin_runner.clone();
        let arg1 = arg.to_vec();
        let mut runner = plugin_runner.write().await;
        // .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))?;
        let r1 = runner.as_mut();
        // begin stream (set argument and setup stream)
        r1.begin_stream(arg1, metadata.clone()).map_err(|e| {
            tracing::warn!("in running pluginRunner: {:?}", e);
            anyhow!("in running pluginRunner: {:?}", e)
        })?;
        let plugin_runner = self.plugin_runner.clone();
        let st = async_stream::stream! {
            let mut runner = plugin_runner.write().await;
            loop {
                let maybe_v = {
                    // run and receive from stream iteratively
                    runner.receive_stream()
                };
                match maybe_v {
                    Ok(Some(v)) => {
                        yield ResultOutputItem { item: Some(result_output_item::Item::Data(v)) }
                    },
                    Ok(None) => {
                        yield ResultOutputItem { item: Some(result_output_item::Item::End(Trailer{
                            metadata
                        })) };
                        break
                    },
                    Err(e) => {
                        tracing::warn!("Error occurred: {:}", e);
                        yield ResultOutputItem { item: Some(result_output_item::Item::End(Trailer{metadata})) };
                        break
                    },
                }
                if runner.is_canceled() {
                    break;
                }
            }
        }
        .boxed();
        Ok(st)
    }

    async fn cancel(&mut self) {
        let _ = self.plugin_runner.write().await.cancel(); //.map(|mut r| r.cancel());
    }
}
