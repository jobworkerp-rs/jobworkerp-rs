// MCP Tool Runner Implementation
//
// This module implements the McpToolRunnerImpl, which represents a single tool
// from an MCP server as an individual runner (1 runner = 1 tool design).

#[cfg(test)]
mod integration_tests;

use crate::jobworkerp::runner::McpServerResult;
use crate::runner::cancellation::CancelMonitoring;
use crate::runner::cancellation_helper::{CancelMonitoringHelper, UseCancelMonitoringHelper};
use crate::runner::mcp::proxy::McpServerProxy;
use crate::runner::mcp::schema_converter::JsonSchemaToProtobufConverter;
use crate::runner::mcp::schema_converter::ValidationError;
use crate::runner::RunnerSpec;
use crate::runner::RunnerTrait;
use crate::{schema_to_json_string, schema_to_json_string_option};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use command_utils::protobuf::ProtobufDescriptor;
use command_utils::text::TextUtil;
use futures::stream::BoxStream;
use jobworkerp_base::codec::ProstMessageCodec;
use jobworkerp_base::codec::UseProstCodec;
use proto::jobworkerp::data::ResultOutputItem;
use proto::jobworkerp::data::StreamingOutputType;
use rmcp::model::{CallToolRequestParam, CallToolResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;

/// MCP Tool Runner Implementation
///
/// Represents a single tool from an MCP server as an individual runner.
/// Each tool has its own runner instance with dedicated JSON Schema validation.
///
/// # Design
/// - 1 runner = 1 tool (fine-grained granularity)
/// - Lazy initialization of ProtobufDescriptor via OnceCell
/// - Shared McpServerProxy across tools from the same server
/// - Original tool name preservation (no sanitization)
#[derive(Debug)]
pub struct McpToolRunnerImpl {
    /// MCP server proxy (shared with other tools from same server)
    mcp_server: Arc<McpServerProxy>,

    /// MCP server name (e.g., "filesystem")
    server_name: String,

    /// Tool name from MCP server (e.g., "read_file")
    /// IMPORTANT: Keep original name without sanitization
    tool_name: String,

    /// JSON Schema to Protobuf converter
    schema_converter: JsonSchemaToProtobufConverter,

    /// Cached ProtobufDescriptor (lazy initialization on load())
    cached_descriptor: Arc<OnceCell<Arc<ProtobufDescriptor>>>,

    /// Cancellation monitoring helper (optional)
    cancel_helper: Option<CancelMonitoringHelper>,
}

// Security constant
const MAX_MCP_RESPONSE_SIZE: usize = 50 * 1024 * 1024; // 50MB

impl McpToolRunnerImpl {
    /// Constructor without cancellation monitoring
    pub fn new(
        server: Arc<McpServerProxy>,
        server_name: String,
        tool_name: String,
        tool_schema: serde_json::Value,
    ) -> Result<Self> {
        let schema_converter = JsonSchemaToProtobufConverter::new(&tool_schema)?;

        Ok(Self {
            mcp_server: server,
            server_name,
            tool_name,
            schema_converter,
            cached_descriptor: Arc::new(OnceCell::new()),
            cancel_helper: None,
        })
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(
        server: Arc<McpServerProxy>,
        server_name: String,
        tool_name: String,
        tool_schema: serde_json::Value,
        cancel_helper: CancelMonitoringHelper,
    ) -> Result<Self> {
        let mut instance = Self::new(server, server_name, tool_name, tool_schema)?;
        instance.cancel_helper = Some(cancel_helper);
        Ok(instance)
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }

    /// Convert MCP CallToolResult to McpServerResult
    ///
    /// # Implementation Notes (v1.11)
    /// This is a direct port from McpServerRunnerImpl::convert_mcp_response()
    /// located at runner/src/runner/mcp.rs
    ///
    /// # Error Handling
    /// - content is empty: Returns error
    /// - Converts rmcp Content to proto Content types
    /// - Non-text content: Converts to appropriate proto types
    fn convert_mcp_response(&self, res: CallToolResult) -> Result<McpServerResult> {
        use crate::jobworkerp::runner::mcp_server_result;

        let mut mcp_contents = Vec::new();

        for content in res.content {
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
                rmcp::model::RawContent::Resource(rmcp::model::RawEmbeddedResource {
                    resource: rmcp::model::ResourceContents::TextResourceContents {
                        uri,
                        mime_type,
                        text,
                        ..
                    },
                    ..
                }) => {
                    use crate::jobworkerp::runner::mcp_server_result::TextResourceContents;
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
                    resource: rmcp::model::ResourceContents::BlobResourceContents {
                        uri,
                        mime_type,
                        blob,
                        ..
                    },
                    ..
                }) => {
                    use crate::jobworkerp::runner::mcp_server_result::BlobResourceContents;
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
                rmcp::model::RawContent::Audio(_audio) => {
                    tracing::warn!(
                        "Audio content not supported yet for tool '{}'",
                        self.tool_name
                    );
                }
                rmcp::model::RawContent::ResourceLink(_) => {
                    tracing::warn!(
                        "ResourceLink content not supported yet for tool '{}'",
                        self.tool_name
                    );
                }
            }
        }

        if mcp_contents.is_empty() {
            return Err(anyhow!(
                "MCP response has no content for tool '{}'",
                self.tool_name
            ));
        }

        Ok(McpServerResult {
            content: mcp_contents,
            is_error: res.is_error.unwrap_or(false),
        })
    }
}

impl RunnerSpec for McpToolRunnerImpl {
    fn name(&self) -> String {
        // Format: {server_name}___{tool_name}
        // Note: tool_name is guaranteed not to contain "___" by validation
        format!("{}___{}", self.server_name, self.tool_name)
    }

    fn runner_settings_proto(&self) -> String {
        "".to_string() // No settings required
    }

    fn job_args_proto(&self) -> String {
        // JSON Schema to Protobuf definition dynamic conversion
        self.schema_converter
            .to_proto_definition(&self.server_name, &self.tool_name)
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
        // Function API: Return JSON Schema as-is
        serde_json::to_string(self.schema_converter.schema()).unwrap_or_default()
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(McpServerResult, "output_schema")
    }
}

#[async_trait]
impl RunnerTrait for McpToolRunnerImpl {
    /// load() caches ProtobufDescriptor (lazy initialization)
    async fn load(&mut self, _settings: Vec<u8>) -> Result<()> {
        // Build Proto definition and cache it
        self.cached_descriptor
            .get_or_try_init(|| async {
                let proto_def = self
                    .schema_converter
                    .to_proto_definition(&self.server_name, &self.tool_name);
                let descriptor = ProtobufDescriptor::new(&proto_def)
                    .map_err(|e| anyhow!("Failed to build ProtobufDescriptor: {}", e))?;
                Ok::<Arc<ProtobufDescriptor>, anyhow::Error>(Arc::new(descriptor))
            })
            .await?;

        tracing::debug!(
            "Cached ProtobufDescriptor for MCP tool '{}:::{}'",
            self.server_name,
            self.tool_name
        );

        Ok(())
    }

    async fn run(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            // 1. Cancellation check
            if cancellation_token.is_cancelled() {
                return Err(anyhow!("MCP tool call was cancelled before execution"));
            }

            // 2. Get cached Descriptor
            let descriptor = self.cached_descriptor.get().ok_or_else(|| {
                anyhow!("ProtobufDescriptor not initialized. Did you call load()?")
            })?;

            // 3. Build message name
            let message_name = format!(
                "{}Args",
                TextUtil::to_pascal_case(&format!("{}_{}", self.server_name, self.tool_name))
            );
            let full_message_name = format!(
                "jobworkerp.runner.mcp.{}.{}",
                self.server_name, message_name
            );

            // 4. Deserialize Protobuf binary
            let dynamic_message =
                descriptor.get_message_by_name_from_bytes(&full_message_name, args)?;

            // 5. Convert DynamicMessage to JSON
            let json_args = ProtobufDescriptor::message_to_json_value(&dynamic_message)?;

            // 6. JSON Schema validation
            // v1.6: Type-safe error handling with ValidationError
            // Note: Log output is centralized in run() method (not in validate())
            if let Err(e) = self.schema_converter.validate(&json_args).await {
                match &e {
                    ValidationError::Timeout { .. }
                    | ValidationError::DeserializationError { .. } => {
                        // Internal error - ERROR level
                        tracing::error!(
                            tool_name = %self.tool_name,
                            server_name = %self.server_name,
                            error = %e,
                            "JSON Schema validation failed for MCP tool"
                        );
                    }
                    ValidationError::SchemaViolation { .. } => {
                        // User input error - WARN level
                        tracing::warn!(
                            tool_name = %self.tool_name,
                            server_name = %self.server_name,
                            error = %e,
                            "Invalid job arguments for MCP tool"
                        );
                    }
                }
                return Err(anyhow::Error::from(e));
            }

            // 7. Call MCP tool with cancellation support
            let res = tokio::select! {
                call_result = self.mcp_server.transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(self.tool_name.clone()),
                    arguments: json_args.as_object().cloned(),
                }) => {
                    call_result.map_err(|e| anyhow!("MCP tool '{}' failed: {}", self.tool_name, e))?
                },
                _ = cancellation_token.cancelled() => {
                    return Err(anyhow!("MCP tool call was cancelled"));
                }
            };

            // 8. Convert result to McpServerResult
            let mcp_result = self.convert_mcp_response(res)?;
            let encoded = ProstMessageCodec::serialize_message(&mcp_result)?;

            // 9. Security: Response size limit check
            if encoded.len() > MAX_MCP_RESPONSE_SIZE {
                return Err(anyhow!(
                    "MCP response too large: {} bytes (max: {} bytes)",
                    encoded.len(),
                    MAX_MCP_RESPONSE_SIZE
                ));
            }

            Ok(encoded)
        }
        .await;

        (result, metadata)
    }

    async fn run_stream(
        &mut self,
        _arg: &[u8],
        _metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Streaming support - future enhancement
        Err(anyhow!(
            "Streaming is not yet supported for MCP tool '{}'",
            self.tool_name
        ))
    }
}

impl UseCancelMonitoringHelper for McpToolRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

#[async_trait]
impl CancelMonitoring for McpToolRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<proto::jobworkerp::data::JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        let token = self.get_cancellation_token().await;
        if !token.is_cancelled() {
            token.cancel();
            tracing::info!(
                "Cancelled MCP tool '{}' from server '{}'",
                self.tool_name,
                self.server_name
            );
        }
        Ok(())
    }
}

