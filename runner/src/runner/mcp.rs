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
pub mod schema_converter;

/// Tool information for sub_method support
#[derive(Debug, Clone)]
pub struct ToolInfo {
    /// Tool name (sub_method name)
    pub name: String,
    /// Tool description
    pub description: Option<String>,
    /// JSON Schema for the tool's input
    pub input_schema: serde_json::Value,
    /// Generated Protobuf schema string
    pub proto_schema: String,
}

/// MCP Server Runner implementation
///
/// Supports two modes:
/// 1. **Legacy mode** (McpServerArgs): Uses tool_name and arg_json fields
/// 2. **Sub-method mode**: Uses sub_method parameter with tool-specific Protobuf args
///
/// The mode is determined by whether `available_tools` is populated (sub_method mode)
/// or empty (legacy mode).
#[derive(Debug)]
pub struct McpServerRunnerImpl {
    mcp_server: McpServerProxy,
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
    /// Available tools with their schemas (populated in sub_method mode)
    /// Key: tool name (sub_method), Value: ToolInfo
    available_tools: HashMap<String, ToolInfo>,
    /// Whether this runner uses legacy McpServerArgs mode
    /// true = legacy mode (available_tools empty), false = sub_method mode
    is_legacy_mode: bool,
}

impl McpServerRunnerImpl {
    /// Constructor without cancellation monitoring (legacy mode for backward compatibility)
    pub fn new(server: McpServerProxy) -> Self {
        Self {
            mcp_server: server,
            cancel_helper: None,
            available_tools: HashMap::new(),
            is_legacy_mode: true, // Legacy mode by default
        }
    }

    /// Constructor with cancellation monitoring (legacy mode, DI integration version)
    pub fn new_with_cancel_monitoring(
        server: McpServerProxy,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            mcp_server: server,
            cancel_helper: Some(cancel_helper),
            available_tools: HashMap::new(),
            is_legacy_mode: true, // Legacy mode by default
        }
    }

    /// Constructor for sub_method mode with pre-loaded tools
    ///
    /// This constructor creates a runner in sub_method mode where each tool
    /// has its own Protobuf schema.
    pub fn new_with_tools(
        server: McpServerProxy,
        cancel_helper: Option<CancelMonitoringHelper>,
        tools: HashMap<String, ToolInfo>,
    ) -> Self {
        Self {
            mcp_server: server,
            cancel_helper,
            available_tools: tools,
            is_legacy_mode: false, // Sub-method mode
        }
    }

    /// Initialize tools from MCP server and switch to sub_method mode
    ///
    /// This method fetches tools from the MCP server, generates Protobuf schemas,
    /// and switches the runner to sub_method mode.
    pub async fn initialize_sub_method_mode(&mut self) -> Result<()> {
        let tools = self.mcp_server.load_tools().await?;
        let server_name = &self.mcp_server.name;

        let mut available_tools = HashMap::new();
        for tool in tools {
            let tool_name = tool.name.into_owned();

            // Validate tool name
            if let Err(e) = schema_converter::validate_sub_method_name(&tool_name) {
                tracing::warn!(
                    "Skipping tool '{}' in MCP server '{}': {}",
                    tool_name,
                    server_name,
                    e
                );
                continue;
            }

            // Convert Arc<JsonObject> to serde_json::Value::Object
            let input_schema_value = serde_json::Value::Object(tool.input_schema.as_ref().clone());

            // Generate Protobuf schema from JSON Schema
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
                    proto_schema,
                },
            );
        }

        if available_tools.is_empty() {
            return Err(anyhow!(
                "No valid tools found in MCP server '{}'",
                server_name
            ));
        }

        self.available_tools = available_tools;
        self.is_legacy_mode = false;

        tracing::info!(
            "MCP runner '{}' initialized with {} tools in sub_method mode",
            server_name,
            self.available_tools.len()
        );

        Ok(())
    }

    /// Check if this runner is in legacy mode
    pub fn is_legacy_mode(&self) -> bool {
        self.is_legacy_mode
    }

    /// Get available tool names (sub_methods)
    pub fn available_tool_names(&self) -> Vec<String> {
        self.available_tools.keys().cloned().collect()
    }

    /// Get tool info by name
    pub fn get_tool_info(&self, tool_name: &str) -> Option<&ToolInfo> {
        self.available_tools.get(tool_name)
    }

    /// Resolve sub_method to actual tool name
    ///
    /// - If sub_method is specified: validate and return it
    /// - If sub_method is None and only 1 tool: auto-select
    /// - If sub_method is None and multiple tools: error
    fn resolve_sub_method(&self, sub_method: Option<&str>) -> Result<String> {
        match sub_method {
            Some(name) => {
                if self.available_tools.contains_key(name) {
                    Ok(name.to_string())
                } else {
                    Err(anyhow!(
                        "Unknown tool '{}' in MCP server '{}'. Available: {:?}",
                        name,
                        self.mcp_server.name,
                        self.available_tool_names()
                    ))
                }
            }
            None => {
                if self.available_tools.len() == 1 {
                    // Auto-select the only available tool
                    Ok(self.available_tools.keys().next().unwrap().clone())
                } else {
                    Err(anyhow!(
                        "sub_method is required for MCP server '{}'. Available tools: {:?}",
                        self.mcp_server.name,
                        self.available_tool_names()
                    ))
                }
            }
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

    /// Get tools as McpTool proto messages
    pub async fn tools(&self) -> Result<Vec<McpTool>> {
        // If in sub_method mode, use cached tools
        if !self.is_legacy_mode && !self.available_tools.is_empty() {
            return Ok(self
                .available_tools
                .values()
                .map(|tool| McpTool {
                    name: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: serde_json::to_string(&tool.input_schema).unwrap_or_default(),
                    annotations: None, // TODO: preserve annotations
                })
                .collect());
        }

        // Legacy mode: fetch from MCP server
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

    /// Execute tool call in legacy mode (using McpServerArgs)
    async fn run_legacy(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            if cancellation_token.is_cancelled() {
                return Err(anyhow!("MCP tool call was cancelled before execution"));
            }

            let span = Self::otel_span_from_metadata(
                &metadata,
                APP_WORKER_NAME,
                "McpServerRunnerImpl::run_legacy",
            );
            let cx = Context::current_with_span(span);
            let mut metadata = metadata.clone();
            Self::inject_metadata_from_context(&mut metadata, &cx);
            let span = cx.span();

            let arg = ProstMessageCodec::deserialize_message::<McpServerArgs>(args)?;
            span.set_attribute(opentelemetry::KeyValue::new(
                "input.tool_name",
                arg.tool_name.clone(),
            ));

            tracing::debug!(
                "Calling MCP tool '{}' with args (legacy mode): {}",
                arg.tool_name,
                arg.arg_json
            );

            let res = tokio::select! {
                call_result = self.mcp_server.transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(arg.tool_name.clone()),
                    arguments: serde_json::from_str(arg.arg_json.as_str())
                        .inspect_err(|e| {
                            tracing::error!("Failed to parse arguments: {}", e);
                        })
                        .ok(),
                }) => {
                    call_result.map_err(|e| {
                        tracing::error!("MCP call_tool failed for tool '{}': {}", arg.tool_name, e);
                        anyhow!("MCP tool '{}' failed: {}", arg.tool_name, e)
                    })?
                },
                _ = cancellation_token.cancelled() => {
                    return Err(anyhow!("MCP tool call was cancelled"));
                }
            };

            self.encode_mcp_result_with_span(res, &span)
        }
        .await;

        (result, metadata)
    }

    /// Execute tool call in sub_method mode
    async fn run_sub_method(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        tool_name: &str,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        let result = async {
            if cancellation_token.is_cancelled() {
                return Err(anyhow!("MCP tool call was cancelled before execution"));
            }

            let span = Self::otel_span_from_metadata(
                &metadata,
                APP_WORKER_NAME,
                "McpServerRunnerImpl::run_sub_method",
            );
            let cx = Context::current_with_span(span);
            let mut metadata = metadata.clone();
            Self::inject_metadata_from_context(&mut metadata, &cx);
            let span = cx.span();

            span.set_attribute(opentelemetry::KeyValue::new(
                "input.tool_name",
                tool_name.to_string(),
            ));

            // In sub_method mode, args is a JSON-serialized object
            // (because Protobuf schemas are generated dynamically)
            // For now, we treat args as JSON bytes
            let args_json: serde_json::Value = serde_json::from_slice(args).map_err(|e| {
                anyhow!(
                    "Failed to parse arguments as JSON for tool '{}': {}",
                    tool_name,
                    e
                )
            })?;

            tracing::debug!(
                "Calling MCP tool '{}' with args (sub_method mode): {:?}",
                tool_name,
                args_json
            );

            let res = tokio::select! {
                call_result = self.mcp_server.transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(tool_name.to_string()),
                    arguments: args_json.as_object().cloned(),
                }) => {
                    call_result.map_err(|e| {
                        tracing::error!("MCP call_tool failed for tool '{}': {}", tool_name, e);
                        anyhow!("MCP tool '{}' failed: {}", tool_name, e)
                    })?
                },
                _ = cancellation_token.cancelled() => {
                    return Err(anyhow!("MCP tool call was cancelled"));
                }
            };

            self.encode_mcp_result_with_span(res, &span)
        }
        .await;

        (result, metadata)
    }

    /// Execute stream tool call in legacy mode (using McpServerArgs)
    async fn run_stream_legacy(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        if cancellation_token.is_cancelled() {
            return Err(anyhow!("MCP stream request was cancelled before execution"));
        }

        let parsed_arg = ProstMessageCodec::deserialize_message::<McpServerArgs>(arg)?;
        let mcp_transport = self.mcp_server.transport.clone();
        let tool_name = parsed_arg.tool_name.clone();
        let arg_json = parsed_arg.arg_json.clone();

        use async_stream::stream;
        use proto::jobworkerp::data::{result_output_item::Item, Trailer};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            let call_result = tokio::select! {
                result = mcp_transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(tool_name.clone()),
                    arguments: serde_json::from_str(arg_json.as_str())
                        .inspect_err(|e| {
                            tracing::error!("Failed to parse arguments: {}", e);
                        })
                        .ok(),
                }) => {
                    match result {
                        Ok(res) => Ok(res),
                        Err(e) => {
                            tracing::error!("MCP call_tool failed for tool '{}': {}", tool_name, e);
                            Err(anyhow!("MCP tool '{}' failed: {}", tool_name, e))
                        }
                    }
                },
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
                    let mcp_result = Self::convert_call_result_to_proto(res);
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

            yield ResultOutputItem {
                item: Some(Item::End((*trailer).clone())),
            };
        };

        Ok(Box::pin(stream))
    }

    /// Execute stream tool call in sub_method mode
    async fn run_stream_sub_method(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
        tool_name: &str,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        if cancellation_token.is_cancelled() {
            return Err(anyhow!("MCP stream request was cancelled before execution"));
        }

        let args_json: serde_json::Value = serde_json::from_slice(args).map_err(|e| {
            anyhow!(
                "Failed to parse arguments as JSON for tool '{}': {}",
                tool_name,
                e
            )
        })?;

        let mcp_transport = self.mcp_server.transport.clone();
        let tool_name_owned = tool_name.to_string();

        use async_stream::stream;
        use proto::jobworkerp::data::{result_output_item::Item, Trailer};

        let trailer = Arc::new(Trailer {
            metadata: metadata.clone(),
        });

        let stream = stream! {
            let call_result = tokio::select! {
                result = mcp_transport.call_tool(CallToolRequestParam {
                    name: std::borrow::Cow::Owned(tool_name_owned.clone()),
                    arguments: args_json.as_object().cloned(),
                }) => {
                    match result {
                        Ok(res) => Ok(res),
                        Err(e) => {
                            tracing::error!("MCP call_tool failed for tool '{}': {}", tool_name_owned, e);
                            Err(anyhow!("MCP tool '{}' failed: {}", tool_name_owned, e))
                        }
                    }
                },
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
                    let mcp_result = Self::convert_call_result_to_proto(res);
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

            yield ResultOutputItem {
                item: Some(Item::End((*trailer).clone())),
            };
        };

        Ok(Box::pin(stream))
    }

    /// Convert MCP CallToolResult to protobuf McpServerResult
    fn convert_call_result_to_proto(res: rmcp::model::CallToolResult) -> McpServerResult {
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
                    tracing::warn!("ResourceLink content not supported yet");
                }
            }
        }

        McpServerResult {
            content: mcp_contents,
            is_error: res.is_error.unwrap_or(false),
        }
    }

    /// Encode MCP call result to protobuf bytes with span tracing
    fn encode_mcp_result_with_span(
        &self,
        res: rmcp::model::CallToolResult,
        span: &opentelemetry::trace::SpanRef<'_>,
    ) -> Result<Vec<u8>> {
        if res.is_error.unwrap_or_default() {
            let error = anyhow!("Tool call failed: {}", serde_json::json!(res.content));
            span.record_error(error.as_ref());
        } else {
            span.set_attribute(opentelemetry::KeyValue::new(
                "output",
                serde_json::json!(res.content).to_string(),
            ));
        }
        span.set_attribute(opentelemetry::KeyValue::new(
            "output.content_length",
            res.content.len() as i64,
        ));

        // Delegate to proto conversion
        let mcp_result = Self::convert_call_result_to_proto(res);
        ProstMessageCodec::serialize_message(&mcp_result)
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
        // In sub_method mode, return empty string (use job_args_proto_map instead)
        if !self.is_legacy_mode {
            return "".to_string();
        }
        include_str!("../../protobuf/jobworkerp/runner/mcp_server_args.proto").to_string()
    }

    /// Returns the job arguments protobuf schema map for sub-method mode
    /// Key: tool name (sub_method), Value: protobuf schema string
    fn job_args_proto_map(&self) -> Option<std::collections::HashMap<String, String>> {
        if self.is_legacy_mode || self.available_tools.is_empty() {
            return None;
        }

        Some(
            self.available_tools
                .iter()
                .map(|(name, info)| (name.clone(), info.proto_schema.clone()))
                .collect(),
        )
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
        // In sub_method mode, return empty (each tool has its own schema)
        if !self.is_legacy_mode {
            return "{}".to_string();
        }
        schema_to_json_string!(McpServerArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(McpServerResult, "output_schema")
    }

    /// Returns the JSON schema for a specific sub-method (tool)
    fn get_sub_method_json_schema(&self, sub_method: &str) -> Result<String> {
        if self.is_legacy_mode {
            return Err(anyhow!(
                "Runner '{}' is in legacy mode and does not support sub_method",
                self.mcp_server.name
            ));
        }

        let tool_info = self.available_tools.get(sub_method).ok_or_else(|| {
            anyhow!(
                "Unknown sub_method '{}' for MCP runner '{}'. Available: {:?}",
                sub_method,
                self.mcp_server.name,
                self.available_tool_names()
            )
        })?;

        serde_json::to_string(&tool_info.input_schema).map_err(|e| {
            anyhow!(
                "Failed to serialize JSON schema for sub_method '{}': {}",
                sub_method,
                e
            )
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
        sub_method: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Dispatch based on mode
        if self.is_legacy_mode {
            // Legacy mode: use McpServerArgs
            self.run_legacy(args, metadata).await
        } else {
            // Sub-method mode: resolve tool name and execute
            let tool_name = match self.resolve_sub_method(sub_method) {
                Ok(name) => name,
                Err(e) => return (Err(e), metadata),
            };
            self.run_sub_method(args, metadata, &tool_name).await
        }
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        sub_method: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Dispatch based on mode
        if self.is_legacy_mode {
            // Legacy mode: use McpServerArgs
            self.run_stream_legacy(arg, metadata).await
        } else {
            // Sub-method mode: resolve tool name and execute
            let tool_name = self.resolve_sub_method(sub_method)?;
            self.run_stream_sub_method(arg, metadata, &tool_name).await
        }
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
