use crate::jobworkerp::runner::mcp_server_result;
use crate::jobworkerp::runner::mcp_server_result::BlobResourceContents;
use crate::jobworkerp::runner::mcp_server_result::TextResourceContents;
use crate::jobworkerp::runner::McpServerArgs;
use crate::jobworkerp::runner::McpServerResult;
use crate::runner::cancellation::CancelMonitoring;
use crate::runner::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::codec::ProstMessageCodec;
use jobworkerp_base::codec::UseProstCodec;
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{JobData, JobId, JobResult};
use proto::jobworkerp::function::data::McpTool;
use proto::jobworkerp::function::data::ToolAnnotations;
use proxy::McpServerProxy;
use rmcp::model::CallToolRequestParam;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod config;
#[cfg(any(test, feature = "test-utils"))]
pub mod integration_tests;
pub mod proxy;

/**
 * PluginRunner wrapper
 * (for self mutability (run(), cancel()))
 */
#[derive(Debug)]
pub struct McpServerRunnerImpl {
    mcp_server: McpServerProxy,
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl McpServerRunnerImpl {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new(server: McpServerProxy) -> Self {
        Self {
            mcp_server: server,
            cancel_helper: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(
        server: McpServerProxy,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            mcp_server: server,
            cancel_helper: Some(cancel_helper),
        }
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }
    pub async fn tools(&self) -> Result<Vec<McpTool>> {
        self.mcp_server.load_tools().await.map(|list| {
            list.into_iter()
                .map(|tool| McpTool {
                    name: tool.name.into_owned(),
                    description: tool.description.map(|r| r.into_owned()),
                    input_schema: serde_json::to_string(&tool.input_schema)
                        .inspect_err(|e| {
                            tracing::error!("Failed to serialize tool input schema: {}", e)
                        })
                        .unwrap_or_default(),
                    annotations: tool.annotations.map(|an| ToolAnnotations {
                        title: an.title,
                        read_only_hint: an.read_only_hint,
                        destructive_hint: an.destructive_hint,
                        idempotent_hint: an.idempotent_hint,
                        open_world_hint: an.open_world_hint,
                    }),
                })
                .collect()
        })
    }
}

impl RunnerSpec for McpServerRunnerImpl {
    fn name(&self) -> String {
        self.mcp_server.name.clone()
    }
    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/mcp_server_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/mcp_server_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::Both
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(crate::jobworkerp::runner::Empty, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(McpServerArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(McpServerResult, "output_schema")
    }
}

impl Tracing for McpServerRunnerImpl {}

#[async_trait]
impl RunnerTrait for McpServerRunnerImpl {
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        Ok(())
    }
    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            // Check for cancellation before starting
            if cancellation_token.is_cancelled() {
                return Err(anyhow!("MCP tool call was cancelled before execution"));
            }

            let span = Self::otel_span_from_metadata(
                &metadata,
                APP_WORKER_NAME,
                "McpServerRunnerImpl::run",
            );
            let cx = Context::current_with_span(span);
            let mut metadata = metadata.clone();
            Self::inject_metadata_from_context(&mut metadata, &cx);
            // ref
            let span = cx.span();

            let arg = ProstMessageCodec::deserialize_message::<McpServerArgs>(args)?;
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.tool_name",
                arg.tool_name.clone(),
            ));
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.arg_json",
                arg.arg_json.clone(), // XXX clone
            ));
            // Call MCP tool with cancellation support
            let res = tokio::select! {
                call_result = self
                    .mcp_server
                    .transport
                    .call_tool(CallToolRequestParam {
                        name: std::borrow::Cow::Owned(arg.tool_name),
                        arguments: serde_json::from_str(arg.arg_json.as_str())
                            .inspect_err(|e| {
                                tracing::error!("Failed to parse arguments: {}", e);
                            })
                            .ok(),
                    }) => call_result?,
                _ = cancellation_token.cancelled() => {
                    return Err(anyhow!("MCP tool call was cancelled"));
                }
            };

            if res.is_error.unwrap_or_default() {
                let error = anyhow!(
                    "Tool call failed: {}",
                    serde_json::json!(res.content).to_string()
                );
                span.record_error(error.as_ref());
            } else {
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output",
                    serde_json::json!(res.content).to_string(),
                ));
            }
            // map res to McpServerResult and encode to Vec<u8>
            let mut mcp_contents = Vec::new();
            if let Some(contents) = res.content {
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output.content_length",
                    contents.len() as i64,
                ));
                for content in contents {
                    match content.raw {
                        rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) => {
                            mcp_contents.push(mcp_server_result::Content {
                                raw_content: Some(mcp_server_result::content::RawContent::Text(
                                    mcp_server_result::TextContent { text },
                                )),
                            });
                        }
                        rmcp::model::RawContent::Image(rmcp::model::RawImageContent {
                            data,
                            mime_type,
                        }) => {
                            mcp_contents.push(mcp_server_result::Content {
                                raw_content: Some(mcp_server_result::content::RawContent::Image(
                                    mcp_server_result::ImageContent { data, mime_type },
                                )),
                            });
                        }
                        // wait for raw audio content of raw content
                        // https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/src/model/content.rs#L55
                        rmcp::model::RawContent::Audio(_audio) => {
                            // mcp_contents.push(mcp_server_result::Content {
                            //     raw_content: Some(mcp_server_result::content::RawContent::Audio(
                            //         mcp_server_result::AudioContent { data, mime_type },
                            //     )),
                            // });
                            tracing::error!("Audio content not supported yet");
                        }
                        rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                            resource:
                                rmcp::model::ResourceContents::TextResourceContents {
                                    uri,
                                    mime_type,
                                    text,
                                },
                        }) => {
                            mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                mcp_server_result::EmbeddedResource {
                                    resource: Some(
                                        mcp_server_result::embedded_resource::Resource::Text(
                                            TextResourceContents {
                                                uri,
                                                mime_type,
                                                text,
                                            },
                                        ),
                                    ),
                                },
                            )),
                        });
                        }
                        rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                            resource:
                                rmcp::model::ResourceContents::BlobResourceContents {
                                    uri,
                                    mime_type,
                                    blob,
                                },
                        }) => {
                            mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                mcp_server_result::EmbeddedResource {
                                    resource: Some(
                                        mcp_server_result::embedded_resource::Resource::Blob(
                                            BlobResourceContents {
                                                uri,
                                                mime_type,
                                                blob,
                                            },
                                        ),
                                    ),
                                },
                            )),
                        });
                        }
                    }
                }
            }

            let mcp_result = McpServerResult {
                content: mcp_contents,
                is_error: res.is_error.unwrap_or(false),
            };

            // Encode the result as protobuf
            let encoded = ProstMessageCodec::serialize_message(&mcp_result)?;
            Ok(encoded)
        }
        .await;

        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        // Check for cancellation before starting
        if cancellation_token.is_cancelled() {
            return Err(anyhow!("MCP stream request was cancelled before execution"));
        }

        let arg = ProstMessageCodec::deserialize_message::<McpServerArgs>(arg)?;

        // Extract needed data from self to avoid lifetime issues
        let mcp_transport = self.mcp_server.transport.clone();
        let tool_name = arg.tool_name.clone();
        let arg_json = arg.arg_json.clone();

        use async_stream::stream;
        use proto::jobworkerp::data::{result_output_item::Item, Trailer};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            // Call MCP tool with cancellation support
            let call_result = tokio::select! {
                result = mcp_transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(tool_name),
                    arguments: serde_json::from_str(arg_json.as_str())
                        .inspect_err(|e| {
                            tracing::error!("Failed to parse arguments: {}", e);
                        })
                        .ok(),
                }) => result,
                _ = cancellation_token.cancelled() => {
                    tracing::info!("MCP stream request was cancelled");
                    yield ResultOutputItem {
                        item: Some(Item::End((*trailer).clone())),
                    };
                    return;
                }
            };

            match call_result {
                Ok(res) => {
                    // Map response to McpServerResult
                    let mut mcp_contents = Vec::new();
                    if let Some(contents) = res.content {
                        for content in contents {
                            match content.raw {
                                rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text }) => {
                                    mcp_contents.push(mcp_server_result::Content {
                                        raw_content: Some(mcp_server_result::content::RawContent::Text(
                                            mcp_server_result::TextContent { text },
                                        )),
                                    });
                                }
                                rmcp::model::RawContent::Image(rmcp::model::RawImageContent {
                                    data,
                                    mime_type,
                                }) => {
                                    mcp_contents.push(mcp_server_result::Content {
                                        raw_content: Some(mcp_server_result::content::RawContent::Image(
                                            mcp_server_result::ImageContent { data, mime_type },
                                        )),
                                    });
                                }
                                rmcp::model::RawContent::Audio(_audio) => {
                                    tracing::error!("Audio content not supported yet");
                                }
                                rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                                    resource: rmcp::model::ResourceContents::TextResourceContents {
                                        uri, mime_type, text,
                                    },
                                }) => {
                                    mcp_contents.push(mcp_server_result::Content {
                                        raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                            mcp_server_result::EmbeddedResource {
                                                resource: Some(
                                                    mcp_server_result::embedded_resource::Resource::Text(
                                                        TextResourceContents { uri, mime_type, text },
                                                    ),
                                                ),
                                            },
                                        )),
                                    });
                                }
                                rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                                    resource: rmcp::model::ResourceContents::BlobResourceContents {
                                        uri, mime_type, blob,
                                    },
                                }) => {
                                    mcp_contents.push(mcp_server_result::Content {
                                        raw_content: Some(mcp_server_result::content::RawContent::Resource(
                                            mcp_server_result::EmbeddedResource {
                                                resource: Some(
                                                    mcp_server_result::embedded_resource::Resource::Blob(
                                                        BlobResourceContents { uri, mime_type, blob },
                                                    ),
                                                ),
                                            },
                                        )),
                                    });
                                }
                            }
                        }
                    }

                    let mcp_result = McpServerResult {
                        content: mcp_contents,
                        is_error: res.is_error.unwrap_or(false),
                    };

                    // Serialize and yield the result
                    match ProstMessageCodec::serialize_message(&mcp_result) {
                        Ok(serialized) => {
                            yield ResultOutputItem {
                                item: Some(Item::Data(serialized)),
                            };
                        }
                        Err(e) => {
                            tracing::error!("Failed to serialize MCP result: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("MCP tool call failed: {}", e);
                }
            }

            // Send end marker
            yield ResultOutputItem {
                item: Some(Item::End((*trailer).clone())),
            };
        };

        // Keep cancellation token for potential mid-stream cancellation
        // Note: The token will be cleared when cancel() is called
        Ok(Box::pin(stream))
    }
}

// CancelMonitoring implementation for McpServerRunnerImpl
#[async_trait]
impl CancelMonitoring for McpServerRunnerImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for MCP job {}",
                job_id.value
            );
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

    /// Signals cancellation token for McpServerRunnerImpl
    async fn request_cancellation(&mut self) -> Result<()> {
        // Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("McpServerRunnerImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("McpServerRunnerImpl: no cancellation helper available");
        }

        // No additional resource cleanup needed
        Ok(())
    }

    /// Complete state reset during pool recycling
    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        tracing::debug!("McpServerRunnerImpl reset for pooling");
        Ok(())
    }
}

// DI trait implementation (with optional support)
impl UseCancelMonitoringHelper for McpServerRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

// MCP Runner tests are already implemented in integration_tests.rs
