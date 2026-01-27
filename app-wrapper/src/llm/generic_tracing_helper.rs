use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use command_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use command_utils::trace::impls::GenericOtelClient;
use command_utils::trace::otel_span::GenAIOtelClient;
use jobworkerp_base::error::JobWorkerError;
use serde_json::json;

/// Generic trait for OpenTelemetry tracing functionality in LLM services
pub trait GenericLLMTracingHelper {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>>;

    /// Convert messages to proper tracing input format (provider-specific implementation)
    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value;

    /// Get provider name for span naming
    fn get_provider_name(&self) -> &str;

    /// Create span attributes for chat completion
    fn create_chat_completion_span_attributes(
        &self,
        model: &str,
        input_messages: serde_json::Value,
        model_parameters: Option<&HashMap<String, serde_json::Value>>,
        tools: &[impl ToolInfo],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let provider = self.get_provider_name();
        let mut span_builder = OtelSpanBuilder::new(format!("{provider}.chat.completions"))
            .span_type(OtelSpanType::Generation)
            .model(model.to_string())
            .system(provider)
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
            let tools_json = serde_json::json!(
                tools
                    .iter()
                    .map(|t| t.get_name().to_string())
                    .collect::<Vec<_>>()
            );
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
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let provider = self.get_provider_name();
        let mut span_builder = OtelSpanBuilder::new(format!("{provider}.tool.{function_name}"))
            .span_type(OtelSpanType::Span)
            .system(provider)
            .operation_name("tool_calls")
            .input(arguments);

        let mut m = HashMap::new();
        m.insert("gen_ai.tool.name".to_string(), json!(function_name));
        span_builder = span_builder.metadata(m);

        if let Some(session_id) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(session_id);
        }
        if let Some(user_id) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(user_id);
        }
        span_builder.build()
    }

    /// Execute chat action with proper parent-child span tracing and response recording
    fn with_chat_response_tracing<F, R>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        mut span_attributes: OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(R, opentelemetry::Context)>> + Send
    where
        F: std::future::Future<Output = Result<R, JobWorkerError>> + Send + 'static,
        R: ChatResponse + Send + 'static + serde::Serialize,
    {
        let _session_id = metadata.get("session_id").cloned();
        let _user_id = metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let _provider = self.get_provider_name().to_string();

        async move {
            let (result, context) = if let Some(client) = &otel_client {
                // Execute action first to get response
                let result = action.await.map_err(|e| {
                    tracing::error!("LLM action failed: {:?}", e);
                    anyhow::anyhow!("Action failed: {}", e)
                })?;

                let response_output = result.to_json();
                span_attributes.data.output = Some(response_output);

                let response_parser = |_: &R| -> Option<OtelSpanAttributes> {
                    None // Return None to skip default output processing
                };

                let current_context = parent_context.clone();

                let dummy_action = async move { Ok::<R, JobWorkerError>(result) };

                let final_result = client
                    .with_span_result_and_response_parser(
                        span_attributes,
                        parent_context,
                        dummy_action,
                        Some(response_parser),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced span: {}", e))?;

                (
                    final_result,
                    current_context.unwrap_or(opentelemetry::Context::current()),
                )
            } else {
                let result = action.await?;
                let context = opentelemetry::Context::current();
                (result, context)
            };

            Ok((result, context))
        }
    }

    /// Execute tool action with child span tracing and response recording
    fn with_tool_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        tool_attributes: OtelSpanAttributes,
        function_name: &str,
        arguments: serde_json::Value,
        action: F,
    ) -> impl std::future::Future<Output = Result<(serde_json::Value, opentelemetry::Context)>>
    + Send
    + 'static
    where
        F: std::future::Future<Output = Result<serde_json::Value, JobWorkerError>> + Send + 'static,
    {
        let function_name = function_name.to_string();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let otel_client = self.get_otel_client().cloned();
        let provider = self.get_provider_name().to_string();

        async move {
            let (result, context) = if let Some(client) = otel_client {
                let response_parser = {
                    let function_name = function_name.clone();
                    let arguments = arguments.clone();
                    let session_id = session_id.clone();
                    let user_id = user_id.clone();
                    let provider = provider.clone();

                    move |result: &serde_json::Value| -> Option<OtelSpanAttributes> {
                        let observation_output = serde_json::json!({
                            "function_name": function_name,
                            "arguments": arguments,
                            "result": result,
                            "execution_status": "completed"
                        });

                        let completion_output = result.clone();
                        let trace_output = serde_json::json!({
                            "tool_name": function_name,
                            "output": result,
                            "success": true
                        });

                        let mut response_span_builder = OtelSpanBuilder::new(format!(
                            "{provider}.tool.{function_name}.response"
                        ))
                        .span_type(OtelSpanType::Event)
                        .observation_output(observation_output)
                        .completion_output(completion_output)
                        .trace_output(trace_output)
                        .level("INFO");

                        let mut metadata = HashMap::new();
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

                let result = client
                    .with_span_result_and_response_parser(
                        tool_attributes,
                        Some(parent_context),
                        action,
                        Some(response_parser),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced tool span: {}", e))?;

                let new_context = opentelemetry::Context::current();
                (result, new_context)
            } else {
                let result = action.await?;
                (result, parent_context)
            };

            Ok((result, context))
        }
    }

    /// Record usage with tracing
    fn trace_usage<U>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        usage_data: &U,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static
    where
        U: UsageData + Clone + Send + 'static,
    {
        let otel_client = self.get_otel_client().cloned();
        let session_id = metadata.get("session_id").cloned();
        let user_id = metadata.get("user_id").cloned();
        let name = name.to_string();
        let usage_data = usage_data.clone();
        let content = content.map(|s| s.to_string());

        async move {
            if let Some(otel_client) = otel_client {
                let usage = usage_data.to_usage_map();
                let output = serde_json::json!({
                    "content": content.as_deref().unwrap_or_default(),
                    "usage": usage_data.to_json()
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

/// Trait for LLM message types
pub trait LLMMessage {
    fn get_role(&self) -> &str;
    fn get_content(&self) -> &str;
}

/// Trait for model options
pub trait ModelOptions {}

/// Trait for tool information
pub trait ToolInfo {
    fn get_name(&self) -> &str;
}

/// Trait for chat response types
pub trait ChatResponse {
    fn to_json(&self) -> serde_json::Value;
}

/// Trait for usage data
pub trait UsageData {
    fn to_usage_map(&self) -> HashMap<String, i64>;
    fn to_json(&self) -> serde_json::Value;
}
