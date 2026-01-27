use crate::jobworkerp::runner::McpServerResult;
use crate::jobworkerp::runner::mcp_server_result;
use crate::jobworkerp::runner::mcp_server_result::BlobResourceContents;
use crate::jobworkerp::runner::mcp_server_result::TextResourceContents;
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use crate::runner::cancellation::CancelMonitoring;
use crate::runner::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use crate::schema_to_json_string_option;
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_base::codec::ProstMessageCodec;
use jobworkerp_base::codec::UseProstCodec;
use jobworkerp_base::error::JobWorkerError;
use opentelemetry::Context;
use opentelemetry::trace::TraceContextExt;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::StreamingOutputType;
use proto::jobworkerp::data::{JobData, JobId, JobResult};
use proxy::McpServerProxy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod config;
#[cfg(any(test, feature = "test-utils"))]
pub mod integration_tests;
pub mod proxy;
pub mod schema_converter;

/// Merge TextContent items by concatenating their text
///
/// Strategy:
/// - TextContent items are concatenated
/// - TextResourceContents items with same uri are concatenated
/// - Non-text content items (ImageContent, AudioContent, BlobResourceContents) are kept as-is
/// - TODO: Proper handling for Image/Audio/Blob content types
fn merge_text_contents(
    contents: Vec<mcp_server_result::Content>,
) -> Vec<mcp_server_result::Content> {
    use mcp_server_result::content::RawContent;
    use mcp_server_result::{Content, TextContent};

    let mut merged = Vec::new();
    let mut text_buffer = String::new();

    for content in contents {
        match &content.raw_content {
            Some(RawContent::Text(tc)) => {
                text_buffer.push_str(&tc.text);
            }
            Some(RawContent::Resource(resource)) => {
                // Handle TextResourceContents - concatenate text
                if let Some(mcp_server_result::embedded_resource::Resource::Text(text_res)) =
                    &resource.resource
                {
                    text_buffer.push_str(&text_res.text);
                } else {
                    // BlobResourceContents - flush buffer and keep as-is
                    if !text_buffer.is_empty() {
                        merged.push(Content {
                            raw_content: Some(RawContent::Text(TextContent {
                                text: std::mem::take(&mut text_buffer),
                            })),
                        });
                    }
                    merged.push(content);
                }
            }
            _ => {
                // ImageContent, AudioContent, etc. - flush buffer and keep as-is
                // TODO: Proper handling for these content types
                if !text_buffer.is_empty() {
                    merged.push(Content {
                        raw_content: Some(RawContent::Text(TextContent {
                            text: std::mem::take(&mut text_buffer),
                        })),
                    });
                }
                merged.push(content);
            }
        }
    }

    // Flush remaining text buffer
    if !text_buffer.is_empty() {
        merged.push(Content {
            raw_content: Some(RawContent::Text(TextContent { text: text_buffer })),
        });
    }

    merged
}

/// Tool information for using support
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// Tool name (using name)
    pub name: String,
    /// Tool description
    pub description: Option<String>,
    /// JSON Schema for the tool's input
    pub input_schema: serde_json::Value,
    /// Generated Protobuf schema string for arguments
    pub args_proto_schema: String,
    /// Generated Protobuf schema string for result
    /// For MCP servers, this contains the common output schema (duplicated across tools)
    /// For Plugins, this can contain method-specific output schema
    pub result_proto_schema: String,
}

/**
 * MCP Server Runner implementation with using support
 *
 * Each MCP tool is exposed as a separate "using" value.
 * Tool-specific Protobuf schemas are generated from MCP tool JSON schemas.
 */
#[derive(Debug)]
pub struct McpServerRunnerImpl {
    mcp_server: Arc<McpServerProxy>,
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
    /// Available tools with their schemas
    /// Key: tool name (using), Value: ToolInfo
    available_tools: HashMap<String, ToolInfo>,
}

impl McpServerRunnerImpl {
    /// Constructor for MCP server runner (async required for tool loading)
    ///
    /// This async constructor creates a runner and initializes it with tools from the MCP server.
    /// Each tool has its own Protobuf schema generated from its JSON Schema.
    pub async fn new(
        server: McpServerProxy,
        cancel_helper: Option<CancelMonitoringHelper>,
    ) -> Result<Self> {
        let tools = server.load_tools().await?;
        let server_name = &server.name;

        let mut available_tools = HashMap::new();
        for tool in tools {
            let tool_name = tool.name.into_owned();

            if let Err(e) = schema_converter::validate_using_name(&tool_name) {
                tracing::warn!(
                    "Skipping tool '{}' in MCP server '{}': {}",
                    tool_name,
                    server_name,
                    e
                );
                continue;
            }

            let input_schema_value = serde_json::Value::Object(tool.input_schema.as_ref().clone());

            let proto_schema = match schema_converter::json_schema_to_protobuf(
                &input_schema_value,
                server_name,
                &tool_name,
            ) {
                Ok(schema) => schema,
                Err(e) => {
                    tracing::warn!(
                        "Failed to generate Protobuf schema for tool '{}': {}",
                        tool_name,
                        e
                    );
                    continue;
                }
            };

            available_tools.insert(
                tool_name.clone(),
                ToolInfo {
                    name: tool_name,
                    description: tool.description.map(|d| d.into_owned()),
                    input_schema: input_schema_value,
                    args_proto_schema: proto_schema,
                    // MCP servers use common output schema (duplicated for each tool for type safety)
                    result_proto_schema: include_str!(
                        "../../protobuf/jobworkerp/runner/mcp_server_result.proto"
                    )
                    .to_string(),
                },
            );
        }

        if available_tools.is_empty() {
            return Err(anyhow!(
                "No valid tools found in MCP server '{}'",
                server_name
            ));
        }

        tracing::info!(
            "MCP runner '{}' initialized with {} tools",
            server_name,
            available_tools.len()
        );

        Ok(Self {
            mcp_server: Arc::new(server),
            cancel_helper,
            available_tools,
        })
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    /// Get available tool names (usings)
    pub fn available_tool_names(&self) -> Vec<String> {
        self.available_tools.keys().cloned().collect()
    }

    /// Get tool info by name
    pub fn get_tool_info(&self, tool_name: &str) -> Option<&ToolInfo> {
        self.available_tools.get(tool_name)
    }

    // McpTool type no longer exists - use method_proto_map() from RunnerSpec trait instead

    /// Resolve using to actual tool name
    ///
    /// - If using is specified: validate and return it
    /// - If using is None and only 1 tool: auto-select
    /// - If using is None and multiple tools: error
    fn resolve_using(&self, using: Option<&str>) -> Result<String> {
        match using {
            Some(name) => {
                if self.available_tools.contains_key(name) {
                    Ok(name.to_string())
                } else {
                    Err(anyhow!(
                        "Unknown tool '{}' in MCP server '{}'. Available: {:?}",
                        name,
                        self.mcp_server.name,
                        self.available_tools.keys().collect::<Vec<_>>()
                    ))
                }
            }
            None => {
                if self.available_tools.len() == 1 {
                    Ok(self.available_tools.keys().next().unwrap().clone())
                } else {
                    Err(anyhow!(
                        "using is required for MCP server '{}'. Available tools: {:?}",
                        self.mcp_server.name,
                        self.available_tools.keys().collect::<Vec<_>>()
                    ))
                }
            }
        }
    }

    /// Decode Protobuf binary args to JSON Value using tool's schema
    ///
    /// MCP runner receives Protobuf-encoded arguments from workflow execution.
    /// This function decodes them using the tool's dynamically generated schema.
    ///
    /// Note: Uses proto field names (snake_case) instead of JSON field names (camelCase)
    /// because MCP tools expect the original JSON Schema field names.
    fn decode_args_to_json(&self, tool_name: &str, args: &[u8]) -> Result<serde_json::Value> {
        let tool_info = self
            .get_tool_info(tool_name)
            .ok_or_else(|| anyhow!("Tool '{}' not found", tool_name))?;

        if !tool_info.args_proto_schema.is_empty() {
            let descriptor = ProtobufDescriptor::new(&tool_info.args_proto_schema)?;
            let message_desc = descriptor
                .get_messages()
                .into_iter()
                .next()
                .ok_or_else(|| {
                    anyhow!(
                        "No message descriptor found in args_proto_schema for tool '{}'",
                        tool_name
                    )
                })?;
            let dynamic_msg = ProtobufDescriptor::get_message_from_bytes(message_desc, args)?;
            // Use proto field names (snake_case) to preserve original MCP JSON Schema field names
            ProtobufDescriptor::message_to_json_value_with_proto_names(&dynamic_msg)
        } else {
            // Fallback: try to interpret as JSON string (backward compatibility)
            let arg_json = String::from_utf8(args.to_vec())?;
            Ok(serde_json::from_str(&arg_json)?)
        }
    }
}

impl RunnerSpec for McpServerRunnerImpl {
    fn name(&self) -> String {
        self.mcp_server.name.clone()
    }

    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        self.available_tools
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    proto::jobworkerp::data::MethodSchema {
                        args_proto: info.args_proto_schema.clone(),
                        result_proto: info.result_proto_schema.clone(),
                        description: info.description.clone(),
                        output_type: StreamingOutputType::Both as i32,
                    },
                )
            })
            .collect()
    }

    // Uses existing JSON Schema from available_tools
    fn method_json_schema_map(&self) -> HashMap<String, crate::runner::MethodJsonSchema> {
        self.available_tools
            .iter()
            .map(|(name, info)| {
                (
                    name.clone(),
                    crate::runner::MethodJsonSchema {
                        // MCP tool's JSON Schema (already available)
                        args_schema: serde_json::to_string(&info.input_schema)
                            .unwrap_or_else(|_| "{}".to_string()),
                        // Common output schema for all MCP tools
                        result_schema: schema_to_json_string_option!(
                            McpServerResult,
                            "mcp_server_output_schema"
                        ),
                        // Note: description is not cached - retrieve from method_proto_map instead
                    },
                )
            })
            .collect()
    }

    fn settings_schema(&self) -> String {
        "{}".to_string() // Empty JSON object (no settings required)
    }

    /// Collect streaming MCP results into a single McpServerResult
    ///
    /// Strategy:
    /// - Collects all McpServerResult chunks from the stream
    /// - Merges TextContent items by concatenating their text
    /// - Merges TextResourceContents items by concatenating their text
    /// - Non-text content items (ImageContent, AudioContent, BlobResourceContents) are kept as-is
    /// - is_error is set to true if any chunk has is_error=true
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> super::CollectStreamFuture {
        use futures::StreamExt;
        use prost::Message;
        use proto::jobworkerp::data::result_output_item;

        Box::pin(async move {
            let mut collected_contents: Vec<mcp_server_result::Content> = Vec::new();
            let mut is_error = false;
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        if let Ok(chunk) = McpServerResult::decode(data.as_slice()) {
                            is_error = is_error || chunk.is_error;
                            for content in chunk.content {
                                collected_contents.push(content);
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    Some(result_output_item::Item::FinalCollected(_)) | None => {}
                }
            }

            // Merge text contents
            let merged_contents = merge_text_contents(collected_contents);

            let result = McpServerResult {
                content: merged_contents,
                is_error,
            };
            let bytes = result.encode_to_vec();
            Ok((bytes, metadata))
        })
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
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Resolve tool name and execute
        let tool_name = match self.resolve_using(using) {
            Ok(name) => name,
            Err(e) => return (Err(e), metadata),
        };
        self.run_using(args, metadata, &tool_name).await
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Resolve tool name and execute
        let tool_name = self.resolve_using(using)?;
        self.run_stream_using(arg, metadata, &tool_name).await
    }
}

impl McpServerRunnerImpl {
    /// Execute MCP tool call (internal implementation)
    async fn run_using(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        tool_name: &str,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            if cancellation_token.is_cancelled() {
                return Err(JobWorkerError::CancelledError(
                    "MCP tool call was cancelled before execution".to_string(),
                )
                .into());
            }

            let span = Self::otel_span_from_metadata(
                &metadata,
                APP_WORKER_NAME,
                "McpServerRunnerImpl::run_using",
            );
            let cx = Context::current_with_span(span);
            let mut metadata = metadata.clone();
            Self::inject_metadata_from_context(&mut metadata, &cx);
            // ref
            let span = cx.span();

            // Decode Protobuf binary args to JSON using tool's schema
            let args_value = self
                .decode_args_to_json(tool_name, args)
                .inspect_err(|e| tracing::error!("Failed to decode arguments: {}", e))?;
            let arg_json = serde_json::to_string(&args_value).unwrap_or_default();

            span.set_attribute(opentelemetry::KeyValue::new(
                "input.tool_name",
                tool_name.to_string(),
            ));
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.arg_json",
                arg_json.clone(),
            ));

            tracing::debug!("Calling MCP tool '{}' with args: {}", tool_name, arg_json);
            let res = self
                .mcp_server
                .call_tool_with_cancellation(tool_name, args_value, cancellation_token.clone())
                .await?;

            tracing::debug!("MCP tool '{}' call completed", tool_name);

            if res.is_error.unwrap_or_default() {
                let error = anyhow!("Tool call failed: {}", serde_json::json!(res.content));
                span.record_error(error.as_ref());
            } else {
                span.set_attribute(opentelemetry::KeyValue::new(
                    "output",
                    serde_json::json!(res.content).to_string(),
                ));
            }
            // map res to McpServerResult and encode to Vec<u8>
            let mut mcp_contents = Vec::new();
            let contents = res.content;
            span.set_attribute(opentelemetry::KeyValue::new(
                "output.content_length",
                contents.len() as i64,
            ));
            for content in contents {
                match content.raw {
                    rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text, .. }) => {
                        mcp_contents.push(mcp_server_result::Content {
                            raw_content: Some(mcp_server_result::content::RawContent::Text(
                                mcp_server_result::TextContent { text },
                            )),
                        });
                    }
                    rmcp::model::RawContent::Image(rmcp::model::RawImageContent {
                        data,
                        mime_type,
                        ..
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
                                ..
                            },
                        ..
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
                                ..
                            },
                        ..
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
                    rmcp::model::RawContent::ResourceLink(_) => {
                        // ResourceLink is not supported in proto definition yet
                        tracing::warn!("ResourceLink content not supported yet");
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

    /// Execute MCP tool call with streaming (internal implementation)
    async fn run_stream_using(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        tool_name: &str,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        if cancellation_token.is_cancelled() {
            return Err(JobWorkerError::CancelledError(
                "MCP stream request was cancelled before execution".to_string(),
            )
            .into());
        }

        // Decode Protobuf binary args to JSON using tool's schema (before entering stream)
        let args_value = self
            .decode_args_to_json(tool_name, arg)
            .inspect_err(|e| tracing::error!("Failed to decode arguments: {}", e))?;

        tracing::debug!(
            "Calling MCP tool '{}' (streaming) with args: {}",
            tool_name,
            serde_json::to_string(&args_value).unwrap_or_default()
        );

        // Clone Arc<McpServerProxy> to keep MCP connection alive during stream execution
        let mcp_server = Arc::clone(&self.mcp_server);
        let tool_name_owned = tool_name.to_string();

        use async_stream::stream;
        use proto::jobworkerp::data::{Trailer, result_output_item::Item};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            // Call MCP tool with cancellation support via proxy
            let call_result = mcp_server
                .call_tool_with_cancellation(&tool_name_owned, args_value, cancellation_token.clone())
                .await;

            match call_result {
                Ok(res) => {
                    // Map response to McpServerResult
                    let mut mcp_contents = Vec::new();
                    let contents = res.content;
                    for content in contents {
                        match content.raw {
                            rmcp::model::RawContent::Text(rmcp::model::RawTextContent { text, .. }) => {
                                    mcp_contents.push(mcp_server_result::Content {
                                        raw_content: Some(mcp_server_result::content::RawContent::Text(
                                            mcp_server_result::TextContent { text },
                                        )),
                                    });
                            }
                            rmcp::model::RawContent::Image(rmcp::model::RawImageContent {
                                data,
                                mime_type,
                                ..
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
                                    uri, mime_type, text, ..
                                },
                                ..
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
                                    uri, mime_type, blob, ..
                                },
                                ..
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
                            rmcp::model::RawContent::ResourceLink(_) => {
                                // ResourceLink is not supported in proto definition yet
                                tracing::warn!("ResourceLink content not supported yet");
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
                    // Check if it's a cancellation error
                    if e.downcast_ref::<JobWorkerError>()
                        .is_some_and(|je| matches!(je, JobWorkerError::CancelledError(_)))
                    {
                        tracing::info!("MCP stream request was cancelled");
                        yield ResultOutputItem {
                            item: Some(Item::End((*trailer).clone())),
                        };
                        return;
                    }
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

// MCP Runner integration tests are in integration_tests.rs

#[cfg(test)]
mod tests {
    use super::*;
    use mcp_server_result::content::RawContent;
    use mcp_server_result::{Content, TextContent};

    fn text_content(text: &str) -> Content {
        Content {
            raw_content: Some(RawContent::Text(TextContent {
                text: text.to_string(),
            })),
        }
    }

    #[test]
    fn test_merge_text_contents_single_text() {
        let contents = vec![text_content("Hello, World!")];
        let merged = merge_text_contents(contents);

        assert_eq!(merged.len(), 1);
        match &merged[0].raw_content {
            Some(RawContent::Text(tc)) => assert_eq!(tc.text, "Hello, World!"),
            _ => panic!("Expected TextContent"),
        }
    }

    #[test]
    fn test_merge_text_contents_multiple_texts_concatenates() {
        let contents = vec![
            text_content("Hello, "),
            text_content("World!"),
            text_content(" How are you?"),
        ];
        let merged = merge_text_contents(contents);

        assert_eq!(merged.len(), 1);
        match &merged[0].raw_content {
            Some(RawContent::Text(tc)) => assert_eq!(tc.text, "Hello, World! How are you?"),
            _ => panic!("Expected TextContent"),
        }
    }

    #[test]
    fn test_merge_text_contents_empty() {
        let contents: Vec<Content> = vec![];
        let merged = merge_text_contents(contents);

        assert!(merged.is_empty());
    }

    #[test]
    fn test_merge_text_contents_with_non_text_interspersed() {
        let contents = vec![
            text_content("Part1 "),
            Content {
                raw_content: Some(RawContent::Image(mcp_server_result::ImageContent {
                    data: "base64data".to_string(),
                    mime_type: "image/png".to_string(),
                })),
            },
            text_content("Part2"),
        ];
        let merged = merge_text_contents(contents);

        // Should have: Text("Part1 "), Image, Text("Part2")
        assert_eq!(merged.len(), 3);
        match &merged[0].raw_content {
            Some(RawContent::Text(tc)) => assert_eq!(tc.text, "Part1 "),
            _ => panic!("Expected TextContent at index 0"),
        }
        match &merged[1].raw_content {
            Some(RawContent::Image(_)) => {}
            _ => panic!("Expected ImageContent at index 1"),
        }
        match &merged[2].raw_content {
            Some(RawContent::Text(tc)) => assert_eq!(tc.text, "Part2"),
            _ => panic!("Expected TextContent at index 2"),
        }
    }

    // Note: collect_stream tests for MCP runner require a running MCP server connection.
    // Integration tests for collect_stream are in integration_tests.rs module.
    // The merge_text_contents function (which is the core logic) is tested above.
}
