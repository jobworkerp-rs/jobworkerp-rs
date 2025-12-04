//! Tool calling processor for MistralRS plugin
//!
//! Handles tool execution via gRPC FunctionService with OpenTelemetry tracing

use crate::core::types::{MistralRSMessage, MistralRSToolCall, ToolCallingConfig};
use crate::grpc::FunctionServiceClient;
use crate::tracing_helper::{extract_parent_context, MistralOtelClient};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Processor for tool calling operations
pub struct ToolCallingProcessor {
    grpc_client: Arc<FunctionServiceClient>,
    config: ToolCallingConfig,
    otel_client: Option<MistralOtelClient>,
}

impl ToolCallingProcessor {
    pub fn new(
        grpc_client: Arc<FunctionServiceClient>,
        config: ToolCallingConfig,
        otel_client: Option<MistralOtelClient>,
    ) -> Self {
        Self {
            grpc_client,
            config,
            otel_client,
        }
    }

    /// Load tools from FunctionSet via gRPC
    pub async fn load_tools_from_set(
        &self,
        function_set_name: &str,
    ) -> Result<Vec<mistralrs::Tool>> {
        let specs = self
            .grpc_client
            .find_functions_by_set(function_set_name)
            .await?;

        let tools = crate::conversion::RequestConverter::convert_function_specs_to_tools(&specs)?;
        tracing::debug!(
            "Loaded {} tools from set '{}'",
            tools.len(),
            function_set_name
        );
        Ok(tools)
    }

    /// Execute tool calls (parallel or sequential based on config)
    pub async fn execute_tool_calls(
        &self,
        tool_calls: &[MistralRSToolCall],
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<MistralRSMessage>> {
        if self.config.parallel_execution {
            self.execute_parallel(tool_calls, cancel_token, metadata)
                .await
        } else {
            self.execute_sequential(tool_calls, cancel_token, metadata)
                .await
        }
    }

    async fn execute_parallel(
        &self,
        tool_calls: &[MistralRSToolCall],
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<MistralRSMessage>> {
        use futures::future::join_all;

        // Extract parent context for distributed tracing
        let parent_context = extract_parent_context(&metadata);

        let handles: Vec<_> = tool_calls
            .iter()
            .map(|call| {
                let client = self.grpc_client.clone();
                let call = call.clone();
                let timeout = self.config.tool_timeout_sec;
                let token = cancel_token.clone();
                let metadata = metadata.clone();
                let otel_client = self.otel_client.clone();
                let parent_ctx = parent_context.clone();

                async move {
                    // Check cancellation before execution
                    if let Some(ref t) = token {
                        if t.is_cancelled() {
                            return MistralRSMessage {
                                role: TextMessageRole::Tool,
                                content: "Execution cancelled".to_string(),
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            };
                        }
                    }

                    tracing::debug!(
                        "Executing tool call: {} with args: {}",
                        call.function_name,
                        call.arguments
                    );

                    // Execute with or without tracing
                    let result = if let Some(ref otel) = otel_client {
                        let args_json: serde_json::Value =
                            serde_json::from_str(&call.arguments).unwrap_or_default();
                        let span_attrs = otel.create_tool_call_span_attributes(
                            &call.function_name,
                            args_json,
                            &metadata,
                        );

                        otel.execute_with_span(span_attrs, Some(parent_ctx), async {
                            client
                                .call_function(
                                    &call.function_name,
                                    &call.arguments,
                                    timeout,
                                    metadata.clone(),
                                )
                                .await
                        })
                        .await
                    } else {
                        client
                            .call_function(&call.function_name, &call.arguments, timeout, metadata)
                            .await
                    };

                    match result {
                        Ok(result) => {
                            tracing::debug!(
                                "Tool call {} succeeded: {}",
                                call.function_name,
                                result
                            );
                            MistralRSMessage {
                                role: TextMessageRole::Tool,
                                content: result,
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Tool call {} failed: {:?}", call.function_name, e);
                            MistralRSMessage {
                                role: TextMessageRole::Tool,
                                content: format!("Error executing {}: {}", call.function_name, e),
                                tool_call_id: Some(call.id.clone()),
                                tool_calls: None,
                            }
                        }
                    }
                }
            })
            .collect();

        Ok(join_all(handles).await)
    }

    async fn execute_sequential(
        &self,
        tool_calls: &[MistralRSToolCall],
        cancel_token: Option<CancellationToken>,
        metadata: HashMap<String, String>,
    ) -> Result<Vec<MistralRSMessage>> {
        let mut results = Vec::new();

        // Extract parent context for distributed tracing
        let parent_context = extract_parent_context(&metadata);

        for call in tool_calls {
            // Check cancellation
            if let Some(ref token) = cancel_token {
                if token.is_cancelled() {
                    results.push(MistralRSMessage {
                        role: TextMessageRole::Tool,
                        content: "Execution cancelled".to_string(),
                        tool_call_id: Some(call.id.clone()),
                        tool_calls: None,
                    });
                    break;
                }
            }

            tracing::debug!(
                "Executing tool call: {} with args: {}",
                call.function_name,
                call.arguments
            );

            // Execute with or without tracing
            let result = if let Some(ref otel) = self.otel_client {
                let args_json: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or_default();
                let span_attrs = otel.create_tool_call_span_attributes(
                    &call.function_name,
                    args_json,
                    &metadata,
                );

                let grpc_client = self.grpc_client.clone();
                let function_name = call.function_name.clone();
                let arguments = call.arguments.clone();
                let timeout = self.config.tool_timeout_sec;
                let meta = metadata.clone();

                match otel
                    .execute_with_span(span_attrs, Some(parent_context.clone()), async move {
                        grpc_client
                            .call_function(&function_name, &arguments, timeout, meta)
                            .await
                    })
                    .await
                {
                    Ok(output) => output,
                    Err(e) => {
                        tracing::warn!("Tool call {} failed: {:?}", call.function_name, e);
                        format!("Error executing {}: {}", call.function_name, e)
                    }
                }
            } else {
                match self
                    .grpc_client
                    .call_function(
                        &call.function_name,
                        &call.arguments,
                        self.config.tool_timeout_sec,
                        metadata.clone(),
                    )
                    .await
                {
                    Ok(output) => output,
                    Err(e) => {
                        tracing::warn!("Tool call {} failed: {:?}", call.function_name, e);
                        format!("Error executing {}: {}", call.function_name, e)
                    }
                }
            };

            results.push(MistralRSMessage {
                role: TextMessageRole::Tool,
                content: result,
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            });
        }

        Ok(results)
    }

    /// Get config
    pub fn config(&self) -> &ToolCallingConfig {
        &self.config
    }
}
