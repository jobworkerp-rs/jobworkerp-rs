use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use infra_utils::infra::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use infra_utils::infra::trace::impls::GenericOtelClient;
use infra_utils::infra::trace::otel_span::GenAIOtelClient;
use jobworkerp_base::error::JobWorkerError;
use ollama_rs::generation::chat::{ChatMessage, ChatMessageFinalResponseData, ChatMessageResponse};
use ollama_rs::generation::tools::ToolInfo;
use ollama_rs::models::ModelOptions;
use serde_json::json;
use tokio::sync::Mutex;

/// Trait for OpenTelemetry tracing functionality in Ollama services
pub trait OllamaTracingHelper {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>>;

    /// Convert ChatMessage vector to proper tracing input format
    fn convert_messages_to_input(messages: &[ChatMessage]) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| {
                let mut msg_json = serde_json::json!({
                    "role": format!("{:?}", m.role).to_lowercase(),
                    "content": m.content
                });

                // Add tool_calls if present
                if !m.tool_calls.is_empty() {
                    msg_json["tool_calls"] = serde_json::json!(m
                        .tool_calls
                        .iter()
                        .map(|tc| serde_json::json!({
                            "function": {
                                "name": tc.function.name,
                                "arguments": tc.function.arguments
                            }
                        }))
                        .collect::<Vec<_>>());
                }

                // Add images if present
                if let Some(images) = &m.images {
                    if !images.is_empty() {
                        msg_json["images"] = serde_json::json!(images.len());
                    }
                }

                msg_json
            })
            .collect::<Vec<_>>())
    }

    /// Convert ModelOptions to proper model parameters format
    fn convert_model_options_to_parameters(
        options: &ModelOptions,
    ) -> HashMap<String, serde_json::Value> {
        let mut parameters = HashMap::new();

        // Serialize ModelOptions to get all fields
        if let Ok(value) = serde_json::to_value(options) {
            if let Some(obj) = value.as_object() {
                for (key, val) in obj {
                    // Only include non-null values
                    if !val.is_null() {
                        parameters.insert(key.clone(), val.clone());
                    }
                }
            }
        }

        parameters
    }

    /// Create span attributes for chat completion
    fn create_chat_completion_span_attributes(
        &self,
        model: &str,
        input_messages: serde_json::Value,
        model_parameters: Option<&HashMap<String, serde_json::Value>>,
        tools: &[ToolInfo],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new("ollama.chat.completions")
            .span_type(OtelSpanType::Generation)
            .model(model.to_string())
            .system("ollama")
            .operation_name("chat")
            .input(input_messages)
            .openinference_span_kind("LLM");
        if let Some(sid) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(sid);
        }
        if let Some(uid) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(uid);
        }
        if let Some(params) = model_parameters {
            span_builder = span_builder.model_parameters(params.clone());
        }
        if !tools.is_empty() {
            let tools_json = serde_json::json!(tools
                .iter()
                .map(|t| t.function.name.clone())
                .collect::<Vec<_>>());
            let mut metadata = HashMap::new();
            metadata.insert("tools".to_string(), tools_json);
            span_builder = span_builder.metadata(metadata);
        }
        span_builder.build()
    }

    /// Create span attributes for tool call tracing
    fn create_tool_call_span_attributes(
        &self,
        function_name: &str,
        arguments: serde_json::Value,
        metadata: &HashMap<String, String>, // request metadata
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new(format!("ollama.tool.{function_name}"))
            .span_type(OtelSpanType::Span)
            .system("ollama")
            .operation_name("tool_calls")
            .input(arguments);

        // Add tool name to telemetry metadata for better observability
        let mut m = HashMap::new();
        m.insert("gen_ai.tool.name".to_string(), json!(function_name));
        span_builder = span_builder.metadata(m);

        if let Some(session_id) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(session_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(user_id.clone());
        }
        span_builder.build()
    }

    /// Create tool call span attributes with enhanced context
    fn create_tool_call_span_from_call(
        &self,
        call: &ollama_rs::generation::tools::ToolCall,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        self.create_tool_call_span_attributes(
            &call.function.name,
            call.function.arguments.clone(),
            metadata,
        )
    }

    /// Execute chat action with proper parent-child span tracing and response recording
    fn with_chat_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(ChatMessageResponse, opentelemetry::Context)>> + Send
    where
        F: std::future::Future<Output = Result<ChatMessageResponse, JobWorkerError>>
            + Send
            + 'static,
    {
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();

        async move {
            let (result, context) = if let Some(client) = &otel_client {
                // Create response parser function to record chat response results
                let response_parser = {
                    let session_id = session_id.clone();
                    let user_id = user_id.clone();
                    move |response: &ChatMessageResponse| -> Option<OtelSpanAttributes> {
                        let response_output = serde_json::json!({
                            "content": response.message.content,
                            "model": response.model,
                            "done": response.done,
                            "tool_calls_count": response.message.tool_calls.len(),
                            "tool_calls": response.message.tool_calls,
                            "created_at": response.created_at
                        });

                        let mut response_span_builder =
                            OtelSpanBuilder::new("ollama.chat.response")
                                .span_type(OtelSpanType::Event)
                                .output(response_output)
                                .level("INFO");

                        let mut metadata = HashMap::new();
                        metadata
                            .insert("event_type".to_string(), serde_json::json!("chat_response"));
                        response_span_builder = response_span_builder.metadata(metadata);

                        if let Some(session_id) = &session_id {
                            response_span_builder =
                                response_span_builder.session_id(session_id.clone());
                        }
                        if let Some(user_id) = &user_id {
                            response_span_builder = response_span_builder.user_id(user_id.clone());
                        }

                        Some(response_span_builder.build())
                    }
                };

                // Execute action within parent context using with_context
                let current_context = parent_context.clone();
                let result = client
                    .with_span_result_and_response_parser(
                        span_attributes,
                        parent_context,
                        action,
                        Some(response_parser),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced span: {}", e))?;

                (
                    result,
                    current_context.unwrap_or(opentelemetry::Context::current()),
                )
            } else {
                let result = action.await?;
                let context = opentelemetry::Context::current();
                (result, context)
            };

            // Record response information in logs
            tracing::info!(
                response_content = %result.message.content,
                response_model = %result.model,
                response_done = result.done,
                tool_calls_count = result.message.tool_calls.len(),
                "Chat API response received"
            );

            Ok((result, context))
        }
    }

    /// Execute tool action with child span tracing and response recording
    fn with_tool_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        tool_attributes: OtelSpanAttributes,
        call: &ollama_rs::generation::tools::ToolCall,
        action: F,
    ) -> impl std::future::Future<Output = Result<(serde_json::Value, opentelemetry::Context)>>
           + Send
           + 'static
    where
        F: std::future::Future<Output = Result<serde_json::Value, JobWorkerError>> + Send + 'static,
    {
        // Extract all necessary values from self before entering the async block
        let function_name = call.function.name.clone();
        let arguments = call.function.arguments.clone();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();

        async move {
            let (result, context) = if let Some(client) = otel_client {
                // Create response parser function to record tool response results
                let response_parser = {
                    let function_name = function_name.clone();
                    let arguments = arguments.clone();
                    let session_id = session_id.clone();
                    let user_id = user_id.clone();

                    move |result: &serde_json::Value| -> Option<OtelSpanAttributes> {
                        // Observation output: external tool call result (the complete response)
                        let observation_output = serde_json::json!({
                            "function_name": function_name,
                            "arguments": arguments,
                            "result": result,
                            "execution_status": "completed"
                        });

                        // Completion output: the tool result value only
                        let completion_output = result.clone();

                        // Trace output: structured tool execution result
                        let trace_output = serde_json::json!({
                            "tool_name": function_name,
                            "output": result,
                            "success": true
                        });

                        let mut response_span_builder =
                            OtelSpanBuilder::new(format!("ollama.tool.{function_name}.response"))
                                .span_type(OtelSpanType::Event)
                                .observation_output(observation_output)
                                .completion_output(completion_output)
                                .trace_output(trace_output)
                                .level("INFO");

                        // Create metadata using a more explicit approach to ensure Send safety
                        let mut metadata = std::collections::HashMap::new();
                        metadata
                            .insert("event_type".to_string(), serde_json::json!("tool_response"));
                        metadata.insert("tool_name".to_string(), serde_json::json!(function_name));
                        metadata.insert(
                            "execution_status".to_string(),
                            serde_json::json!("completed"),
                        );

                        response_span_builder = response_span_builder.metadata(metadata);

                        if let Some(ref session_id) = session_id {
                            response_span_builder =
                                response_span_builder.session_id(session_id.clone());
                        }
                        if let Some(ref user_id) = user_id {
                            response_span_builder = response_span_builder.user_id(user_id.clone());
                        }

                        Some(response_span_builder.build())
                    }
                };

                let result = async {
                    client
                        .with_span_result_and_response_parser(
                            tool_attributes,
                            Some(parent_context),
                            action,
                            Some(response_parser),
                        )
                        .await
                }
                .await
                .map_err(|e| anyhow::anyhow!("Error in traced tool span: {}", e))?;

                // Create new context with the executed span for chaining
                let new_context = opentelemetry::Context::current();

                (result, new_context)
            } else {
                let result = action.await?;
                (result, parent_context)
            };

            // Record tool response information in logs
            tracing::info!(
                function_name = %function_name,
                tool_response = %result,
                arguments = ?arguments,
                "Tool call response received"
            );

            Ok((result, context))
        }
    }

    /// Create chat completion span attributes from Ollama request components
    #[allow(async_fn_in_trait)]
    async fn create_chat_span_from_request(
        &self,
        model: &str,
        messages: Arc<Mutex<Vec<ChatMessage>>>,
        options: &ModelOptions,
        tools: &[ToolInfo],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let input_messages = Self::convert_messages_to_input(messages.lock().await.as_ref());
        let model_parameters = Self::convert_model_options_to_parameters(options);

        self.create_chat_completion_span_attributes(
            model,
            input_messages,
            Some(&model_parameters),
            tools,
            metadata,
        )
    }

    fn trace_usage(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        final_data: &ChatMessageFinalResponseData,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        let otel_client = self.get_otel_client().cloned();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let name = name.to_string();
        let final_data = final_data.clone();
        let content = content.map(|s| s.to_string());

        async move {
            if let Some(otel_client) = otel_client {
                let mut usage = HashMap::new();
                usage.insert(
                    "prompt_tokens".to_string(),
                    final_data.prompt_eval_count as i64,
                );
                usage.insert(
                    "completion_tokens".to_string(),
                    final_data.eval_count as i64,
                );
                usage.insert(
                    "total_tokens".to_string(),
                    (final_data.prompt_eval_count + final_data.eval_count) as i64,
                );
                let output = serde_json::json!({
                    "content": content.as_deref().unwrap_or_default(),
                    "usage": {
                        "prompt_tokens": final_data.prompt_eval_count,
                        "completion_tokens": final_data.eval_count,
                        "total_tokens": final_data.prompt_eval_count + final_data.eval_count
                    }
                });

                let mut span_builder = OtelSpanBuilder::new(&name)
                    .span_type(OtelSpanType::Event)
                    .usage(usage)
                    .output(output)
                    .level("INFO");
                if let Some(session_id) = session_id {
                    span_builder = span_builder.session_id(session_id);
                }
                if let Some(user_id) = user_id {
                    span_builder = span_builder.user_id(user_id);
                }
                let span_attributes = span_builder.build();

                // XXX context None
                otel_client
                    .with_span_result(span_attributes, Some(parent_context), async {
                        Ok::<(), JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced span: {}", e))?;
            }
            Ok(())
        }
    }
}
