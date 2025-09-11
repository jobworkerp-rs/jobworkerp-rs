use super::super::generic_tracing_helper::GenericLLMTracingHelper;
use crate::llm::generic_tracing_helper::LLMMessage;
use anyhow::Result;
use genai::chat::{ChatMessage, ChatOptions, ChatRequest, Tool};
use jobworkerp_base::error::JobWorkerError;
use std::collections::HashMap;

/// Trait for OpenTelemetry tracing functionality in GenAI services
pub trait GenaiTracingHelper: GenericLLMTracingHelper {
    /// Convert ChatMessage vector to proper tracing input format
    fn convert_messages_to_input_genai(messages: &[ChatMessage]) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| {
                let mut msg_json = serde_json::json!({
                    "role": m.get_role(),
                    "content": m.get_content()
                });

                // Add additional content info for non-text messages
                let parts = m.content.parts();
                if parts.len() > 1 {
                    msg_json["parts_count"] = serde_json::json!(parts.len());
                }

                let tool_calls = m.content.tool_calls();
                if !tool_calls.is_empty() {
                    msg_json["tool_calls"] = serde_json::json!(tool_calls
                        .iter()
                        .map(|tc| serde_json::json!({
                            "call_id": tc.call_id,
                            "fn_name": tc.fn_name,
                            "fn_arguments": tc.fn_arguments
                        }))
                        .collect::<Vec<_>>());
                }

                msg_json
            })
            .collect::<Vec<_>>())
    }

    /// Convert ChatOptions to proper model parameters format
    fn convert_model_options_to_parameters_genai(
        options: &Option<ChatOptions>,
    ) -> HashMap<String, serde_json::Value> {
        let mut parameters = HashMap::new();

        if let Some(opts) = options {
            if let Some(temp) = opts.temperature {
                parameters.insert("temperature".to_string(), serde_json::json!(temp));
            }
            if let Some(max_tokens) = opts.max_tokens {
                parameters.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
            }
            if let Some(top_p) = opts.top_p {
                parameters.insert("top_p".to_string(), serde_json::json!(top_p));
            }
            if let Some(normalize) = opts.normalize_reasoning_content {
                parameters.insert(
                    "normalize_reasoning_content".to_string(),
                    serde_json::json!(normalize),
                );
            }
        }

        parameters
    }

    /// Create chat completion span attributes from GenAI request components
    #[allow(async_fn_in_trait)]
    async fn create_chat_span_from_request(
        &self,
        model: &str,
        chat_req: &ChatRequest,
        options: &Option<ChatOptions>,
        tools: &[Tool],
        metadata: &HashMap<String, String>,
    ) -> command_utils::trace::attr::OtelSpanAttributes {
        let input_messages = Self::convert_messages_to_input_genai(&chat_req.messages);
        let model_parameters = Self::convert_model_options_to_parameters_genai(options);

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
        usage_data: &genai::chat::Usage,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        GenericLLMTracingHelper::trace_usage(
            self,
            metadata,
            parent_context,
            name,
            usage_data,
            content,
        )
    }
}

impl crate::llm::tracing::LLMRequestData for genai::chat::ChatRequest {
    fn extract_input(&self) -> crate::llm::tracing::LLMInput {
        crate::llm::tracing::LLMInput {
            messages: serde_json::json!(self
                .messages
                .iter()
                .map(|m| {
                    let mut msg_json = serde_json::json!({
                        "role": match m.role {
                            genai::chat::ChatRole::User => "user",
                            genai::chat::ChatRole::Assistant => "assistant",
                            genai::chat::ChatRole::System => "system",
                            genai::chat::ChatRole::Tool => "tool",
                        },
                        "content": m.content.joined_texts().unwrap_or_else(|| "[non-text content]".to_string())
                    });

                    // Add additional content info for non-text messages
                    let parts = m.content.parts();
                    if parts.len() > 1 {
                        msg_json["parts_count"] = serde_json::json!(parts.len());
                    }

                    let tool_calls = m.content.tool_calls();
                    if !tool_calls.is_empty() {
                        msg_json["tool_calls"] = serde_json::json!(tool_calls
                            .iter()
                            .map(|tc| serde_json::json!({
                                "call_id": tc.call_id,
                                "fn_name": tc.fn_name,
                                "fn_arguments": tc.fn_arguments
                            }))
                            .collect::<Vec<_>>());
                    }

                    msg_json
                })
                .collect::<Vec<_>>()),
            prompt: None,
        }
    }

    fn extract_options(&self) -> Option<crate::llm::tracing::LLMOptions> {
        // For GenAI, options are not directly included in ChatRequest
        // ChatOptions are passed separately, so return None here
        None
    }

    fn extract_tools(&self) -> Vec<crate::llm::tracing::LLMTool> {
        self.tools.as_ref().map_or(vec![], |tools| {
            tools
                .iter()
                .map(|tool| crate::llm::tracing::LLMTool {
                    name: tool.name.clone(),
                    description: tool.description.clone().unwrap_or_default(),
                    parameters: serde_json::json!(tool),
                })
                .collect()
        })
    }

    fn extract_model(&self) -> Option<String> {
        None // For GenAI, the model name is not included in the request
    }
}

impl crate::llm::tracing::LLMResponseData for genai::chat::ChatResponse {
    fn to_trace_output(&self) -> serde_json::Value {
        // Return only the message content for trace output, not the full structure
        serde_json::json!(self.first_text())
    }

    fn extract_usage(&self) -> Option<Box<dyn crate::llm::tracing::UsageData>> {
        Some(Box::new(self.usage.clone()) as Box<dyn crate::llm::tracing::UsageData>)
    }

    fn extract_content(&self) -> Option<String> {
        self.first_text().map(|s| s.to_string())
    }
}

///
///
/// Trait for OpenTelemetry tracing functionality in GenAI completion services
pub trait GenaiCompletionTracingHelper: GenericLLMTracingHelper {
    /// Create completion span attributes from GenAI request components
    #[allow(async_fn_in_trait)]
    async fn create_completion_span_from_request(
        &self,
        model: &str,
        chat_req: &ChatRequest,
        options: &Option<ChatOptions>,
        metadata: &HashMap<String, String>,
    ) -> command_utils::trace::attr::OtelSpanAttributes {
        // use super::super::chat::genai::GenaiTracingHelper;
        let input_messages =
            super::super::chat::genai::GenaiChatService::convert_messages_to_input_genai(
                &chat_req.messages,
            );
        let model_parameters =
            super::super::chat::genai::GenaiChatService::convert_model_options_to_parameters_genai(
                options,
            );

        // Create completion-specific span attributes
        let mut span_builder = command_utils::trace::attr::OtelSpanBuilder::new(format!(
            "{}.completions",
            self.get_provider_name()
        ))
        .span_type(command_utils::trace::attr::OtelSpanType::Generation)
        .model(model.to_string())
        .system(self.get_provider_name())
        .operation_name("completion")
        .input(input_messages)
        .openinference_span_kind("LLM");

        if let Some(sid) = metadata.get("session_id").cloned() {
            span_builder = span_builder.session_id(sid);
        }
        if let Some(uid) = metadata.get("user_id").cloned() {
            span_builder = span_builder.user_id(uid);
        }
        if !model_parameters.is_empty() {
            span_builder = span_builder.model_parameters(model_parameters);
        }

        span_builder.build()
    }

    /// Execute completion action with proper parent-child span tracing and response recording
    fn with_completion_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: command_utils::trace::attr::OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(genai::chat::ChatResponse, opentelemetry::Context)>>
           + Send
    where
        F: std::future::Future<Output = Result<genai::chat::ChatResponse, JobWorkerError>>
            + Send
            + 'static,
    {
        GenericLLMTracingHelper::with_chat_response_tracing(
            self,
            metadata,
            parent_context,
            span_attributes,
            action,
        )
    }

    fn trace_usage(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        usage_data: &genai::chat::Usage,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        GenericLLMTracingHelper::trace_usage(
            self,
            metadata,
            parent_context,
            name,
            usage_data,
            content,
        )
    }
}
