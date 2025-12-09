use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use std::collections::HashMap;
use std::sync::Arc;

use super::PluginRunnerVariant;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::stream::StreamExt;
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::Trailer;
use tokio::sync::RwLock;

use super::super::cancellation::CancelMonitoring;
use super::super::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use proto::jobworkerp::data::{JobData, JobId, JobResult};

/// Variant type for lock-free collect_stream dispatch
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginVariantType {
    Legacy,
    MultiMethod,
}

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Clone)]
pub struct PluginRunnerWrapperImpl {
    variant: Arc<RwLock<PluginRunnerVariant>>,
    /// Cached variant type for lock-free collect_stream dispatch
    variant_type: PluginVariantType,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl PluginRunnerWrapperImpl {
    pub fn new(variant: Arc<RwLock<PluginRunnerVariant>>) -> Self {
        // Determine variant type at construction time
        let variant_type = {
            let guard = block_on(variant.read());
            match &*guard {
                PluginRunnerVariant::Legacy(_) => PluginVariantType::Legacy,
                PluginRunnerVariant::MultiMethod(_) => PluginVariantType::MultiMethod,
            }
        };
        Self {
            variant,
            variant_type,
            cancel_helper: None,
        }
    }
    async fn create(&self, settings: Vec<u8>) -> Result<()> {
        let variant = Arc::clone(&self.variant);
        #[allow(unstable_name_collisions)]
        tokio::task::spawn_blocking(|| async move {
            let mut guard = variant.write().await;
            match &mut *guard {
                super::PluginRunnerVariant::Legacy(plugin) => plugin.load(settings),
                super::PluginRunnerVariant::MultiMethod(plugin) => plugin.load(settings),
            }
        })
        .await
        .map_err(|e| anyhow!("plugin runner lock error: {:?}", e))?
        .await?;
        Ok(())
    }
}

impl RunnerSpec for PluginRunnerWrapperImpl {
    fn name(&self) -> String {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.name(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.name(),
        }
    }
    fn runner_settings_proto(&self) -> String {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.runner_settings_proto(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.runner_settings_proto(),
        }
    }
    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => {
                // Auto-convert from legacy methods
                let mut map = HashMap::new();
                map.insert(
                    proto::DEFAULT_METHOD_NAME.to_string(),
                    proto::jobworkerp::data::MethodSchema {
                        args_proto: plugin.job_args_proto(),
                        result_proto: plugin.result_output_proto().unwrap_or_default(),
                        description: Some(plugin.description()),
                        output_type: plugin.output_type() as i32,
                    },
                );
                tracing::debug!(
                    "Auto-converted legacy plugin '{}' to method_proto_map with '{}' method",
                    plugin.name(),
                    proto::DEFAULT_METHOD_NAME
                );
                map
            }
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.method_proto_map(),
        }
    }

    fn method_json_schema_map(&self) -> HashMap<String, crate::runner::MethodJsonSchema> {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => {
                // This is a legacy plugin - use arguments_schema() and output_json_schema()
                let mut map = HashMap::new();
                map.insert(
                    proto::DEFAULT_METHOD_NAME.to_string(),
                    crate::runner::MethodJsonSchema {
                        args_schema: plugin.arguments_schema(),
                        result_schema: plugin.output_json_schema(),
                    },
                );
                tracing::debug!(
                    "Auto-converted legacy plugin '{}' JSON schemas with '{}' method",
                    plugin.name(),
                    proto::DEFAULT_METHOD_NAME
                );
                map
            }
            super::PluginRunnerVariant::MultiMethod(plugin) => {
                if let Some(custom_schemas) = plugin.method_json_schema_map() {
                    custom_schemas
                } else {
                    // Fall back to automatic Protobuf→JSON Schema conversion
                    crate::runner::MethodJsonSchema::from_proto_map(self.method_proto_map())
                }
            }
        }
    }

    fn settings_schema(&self) -> String {
        let guard = block_on(self.variant.read());
        match &*guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.settings_schema(),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.settings_schema(),
        }
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
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // 1. Get method map for legacy plugin detection
        let method_map = self.method_proto_map();

        // 2. Legacy plugin detection: single method named DEFAULT_METHOD_NAME
        let is_legacy =
            method_map.len() == 1 && method_map.contains_key(proto::DEFAULT_METHOD_NAME);

        // 3. Handle 'using' parameter
        if let Some(u) = using {
            if is_legacy {
                tracing::warn!(
                    "Plugin '{}' is legacy (single method '{}'), ignoring 'using' parameter: {}",
                    self.name(),
                    proto::DEFAULT_METHOD_NAME,
                    u
                );
            } else if !method_map.contains_key(u) {
                return (
                    Err(anyhow!(
                        "Unknown method '{}' for plugin '{}'. Available methods: {:?}",
                        u,
                        self.name(),
                        method_map.keys().collect::<Vec<_>>()
                    )),
                    metadata,
                );
            }
        }

        // 4. Execute plugin
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        let mut guard = variant.write().await;

        let (r, meta) = match &mut *guard {
            super::PluginRunnerVariant::Legacy(plugin) => plugin.run(arg1, metadata),
            super::PluginRunnerVariant::MultiMethod(plugin) => plugin.run(arg1, metadata, using),
        };
        (
            r.map_err(|e| {
                tracing::warn!("in running pluginRunner: {:?}", e);
                e
            }),
            meta,
        )
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // 1. Get method map for legacy plugin detection
        let method_map = self.method_proto_map();

        // 2. Legacy plugin detection: single method named DEFAULT_METHOD_NAME
        let is_legacy =
            method_map.len() == 1 && method_map.contains_key(proto::DEFAULT_METHOD_NAME);

        // 3. Handle 'using' parameter
        if let Some(u) = using {
            if is_legacy {
                tracing::warn!(
                    "Plugin '{}' is legacy (single method '{}'), ignoring 'using' parameter: {}",
                    self.name(),
                    proto::DEFAULT_METHOD_NAME,
                    u
                );
            } else if !method_map.contains_key(u) {
                return Err(anyhow!(
                    "Unknown method '{}' for plugin '{}'. Available methods: {:?}",
                    u,
                    self.name(),
                    method_map.keys().collect::<Vec<_>>()
                ));
            }
        }

        // 4. Begin stream
        let variant = self.variant.clone();
        let arg1 = arg.to_vec();
        {
            let mut guard = variant.write().await;
            // begin stream (set argument and setup stream)
            let result = match &mut *guard {
                super::PluginRunnerVariant::Legacy(plugin) => {
                    plugin.begin_stream(arg1, metadata.clone())
                }
                super::PluginRunnerVariant::MultiMethod(plugin) => {
                    plugin.begin_stream(arg1, metadata.clone(), using)
                }
            };
            result.map_err(|e| {
                tracing::warn!("in running pluginRunner: {:?}", e);
                anyhow!("in running pluginRunner: {:?}", e)
            })?;
        }
        let variant_clone = self.variant.clone();

        let cancel_token = if let Some(helper) = &self.cancel_helper {
            Some(helper.get_cancellation_token().await)
        } else {
            None
        };

        let st = async_stream::stream! {
            loop {
                let mut guard = variant_clone.write().await;

                let receive_future = async {
                    match &mut *guard {
                        super::PluginRunnerVariant::Legacy(plugin) => plugin.receive_stream(),
                        super::PluginRunnerVariant::MultiMethod(plugin) => plugin.receive_stream(),
                    }
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

                let is_cancelled = match &*guard {
                    super::PluginRunnerVariant::Legacy(plugin) => plugin.is_canceled(),
                    super::PluginRunnerVariant::MultiMethod(plugin) => plugin.is_canceled(),
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
                if is_cancelled {
                    break;
                }
            }
        }
        .boxed();
        Ok(st)
    }

    /// Collect streaming output for plugin runners
    ///
    /// For MultiMethod plugins, delegates to the plugin's collect_stream.
    /// For Legacy plugins, keeps only the last data chunk (protobuf binary concatenation is invalid).
    ///
    /// Note: Uses cached variant_type to dispatch without locking.
    /// For MultiMethod, we get the plugin's collect_stream future while briefly
    /// holding the lock, then release the lock before awaiting. This allows the stream
    /// (which needs write lock internally) to be processed without deadlock.
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
    ) -> crate::runner::CollectStreamFuture {
        let variant = self.variant.clone();
        let variant_type = self.variant_type;

        Box::pin(async move {
            match variant_type {
                PluginVariantType::Legacy => {
                    // Legacy plugins: keep only the last chunk (no custom collect_stream)
                    // Process stream without holding any lock
                    let mut last_data: Option<Vec<u8>> = None;
                    let mut metadata = HashMap::new();
                    let mut stream = stream;

                    while let Some(item) = stream.next().await {
                        match item.item {
                            Some(result_output_item::Item::Data(data)) => {
                                last_data = Some(data);
                            }
                            Some(result_output_item::Item::End(trailer)) => {
                                metadata = trailer.metadata;
                                break;
                            }
                            None => {}
                        }
                    }
                    Ok((last_data.unwrap_or_default(), metadata))
                }
                PluginVariantType::MultiMethod => {
                    // Get the collect_stream future from plugin (brief lock)
                    let future = {
                        let guard = variant.read().await;
                        if let super::PluginRunnerVariant::MultiMethod(plugin) = &*guard {
                            plugin.collect_stream(stream)
                        } else {
                            unreachable!("variant_type mismatch")
                        }
                    };
                    // Lock released, now await the future which processes the stream
                    future.await
                }
            }
        })
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
