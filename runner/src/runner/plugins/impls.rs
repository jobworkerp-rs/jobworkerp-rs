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

use super::super::cancellation::CancelMonitoring;
use super::super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use proto::jobworkerp::data::{JobData, JobId, JobResult};

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Clone)]
pub struct PluginRunnerWrapperImpl {
    #[allow(clippy::borrowed_box)]
    plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl PluginRunnerWrapperImpl {
    // #[allow(clippy::borrowed_box)]
    pub fn new(plugin_runner: Arc<RwLock<Box<dyn PluginRunner + Send + Sync>>>) -> Self {
        Self {
            plugin_runner,
            cancel_helper: None,
        }
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
    fn job_args_proto(&self) -> Option<String> {
        block_on(self.plugin_runner.read()).job_args_proto()
    }

    fn method_proto_map(
        &self,
    ) -> Option<std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema>> {
        block_on(self.plugin_runner.read()).method_proto_map()
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
        _using: Option<&str>,
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
        _using: Option<&str>,
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

        // Get cancellation token if available
        let cancel_token = if let Some(helper) = &self.cancel_helper {
            Some(helper.get_cancellation_token().await)
        } else {
            None
        };

        let st = async_stream::stream! {
            let mut runner = plugin_runner.write().await;
            loop {
                let receive_future = async {
                    runner.receive_stream()
                };

                let maybe_v = if let Some(ref token) = cancel_token {
                    tokio::select! {
                        result = receive_future => result,
                        _ = token.cancelled() => {
                            tracing::info!("Plugin stream cancelled by token");
                            break;
                        }
                    }
                } else {
                    receive_future.await
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
}

#[async_trait]
impl CancelMonitoring for PluginRunnerWrapperImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        // Clear branching based on helper availability
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!("No cancel monitoring configured for job {}", job_id.value);
            Ok(None)
        }
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    /// Signals cancellation token for PluginRunnerWrapperImpl
    async fn request_cancellation(&mut self) -> Result<()> {
        // Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("PluginRunnerWrapperImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("PluginRunnerWrapperImpl: no cancellation helper available");
        }

        // No additional resource cleanup needed
        Ok(())
    }

    /// Reset for pooling
    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await
        } else {
            Ok(())
        }
    }
}

impl UseCancelMonitoringHelper for PluginRunnerWrapperImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}
