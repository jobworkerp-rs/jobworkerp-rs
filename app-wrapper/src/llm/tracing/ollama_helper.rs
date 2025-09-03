use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use command_utils::trace::attr::OtelSpanAttributes;
use jobworkerp_base::error::JobWorkerError;
use ollama_rs::generation::chat::{ChatMessage, ChatMessageFinalResponseData, ChatMessageResponse};
use ollama_rs::generation::tools::ToolInfo;
use ollama_rs::models::ModelOptions;
use tokio::sync::Mutex;

use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    ToolInfo as GenericToolInfo, UsageData,
};

// Trait implementations for Ollama-specific types
impl LLMMessage for ChatMessage {
    fn get_role(&self) -> &str {
        match self.role {
            ollama_rs::generation::chat::MessageRole::User => "user",
            ollama_rs::generation::chat::MessageRole::Assistant => "assistant",
            ollama_rs::generation::chat::MessageRole::System => "system",
            ollama_rs::generation::chat::MessageRole::Tool => "tool",
            // _ => "unknown",
        }
    }

    fn get_content(&self) -> &str {
        &self.content
    }
}

impl GenericModelOptions for ModelOptions {}

impl GenericToolInfo for ToolInfo {
    fn get_name(&self) -> &str {
        &self.function.name
    }
}

impl ChatResponse for ChatMessageResponse {
    fn to_json(&self) -> serde_json::Value {
        // Follow MistralRS approach - include comprehensive response information
        let has_tool_calls = !self.message.tool_calls.is_empty();

        if has_tool_calls {
            // For tool calls, include complete information as JSON
            let tool_calls = self
                .message
                .tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments
                        }
                    })
                })
                .collect::<Vec<_>>();

            // Return tool calls as JSON structure with available fields
            let mut response = serde_json::json!({
                "role": "assistant",
                "tool_calls": tool_calls,
                "content": self.message.content,
                "model": self.model,
                "created_at": self.created_at,
                "done": self.done
            });

            // Add final_data information if available
            if let Some(ref final_data) = self.final_data {
                response["total_duration"] = serde_json::json!(final_data.total_duration);
                response["load_duration"] = serde_json::json!(final_data.load_duration);
                response["prompt_eval_count"] = serde_json::json!(final_data.prompt_eval_count);
                response["prompt_eval_duration"] =
                    serde_json::json!(final_data.prompt_eval_duration);
                response["eval_count"] = serde_json::json!(final_data.eval_count);
                response["eval_duration"] = serde_json::json!(final_data.eval_duration);
            }

            response
        } else {
            // For regular content, include full response information with available fields
            let mut response = serde_json::json!({
                "role": "assistant",
                "content": self.message.content,
                "model": self.model,
                "created_at": self.created_at,
                "done": self.done
            });

            // Add final_data information if available
            if let Some(ref final_data) = self.final_data {
                response["total_duration"] = serde_json::json!(final_data.total_duration);
                response["load_duration"] = serde_json::json!(final_data.load_duration);
                response["prompt_eval_count"] = serde_json::json!(final_data.prompt_eval_count);
                response["prompt_eval_duration"] =
                    serde_json::json!(final_data.prompt_eval_duration);
                response["eval_count"] = serde_json::json!(final_data.eval_count);
                response["eval_duration"] = serde_json::json!(final_data.eval_duration);
            }

            response
        }
    }
}

impl UsageData for ChatMessageFinalResponseData {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert("prompt_tokens".to_string(), self.prompt_eval_count as i64);
        usage.insert("completion_tokens".to_string(), self.eval_count as i64);
        usage.insert(
            "total_tokens".to_string(),
            (self.prompt_eval_count + self.eval_count) as i64,
        );
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_eval_count,
            "completion_tokens": self.eval_count,
            "total_tokens": self.prompt_eval_count + self.eval_count
        })
    }
}

/// Trait for OpenTelemetry tracing functionality in Ollama services
pub trait OllamaTracingHelper: GenericLLMTracingHelper {
    /// Convert ChatMessage vector to proper tracing input format
    fn convert_messages_to_input_ollama(messages: &[ChatMessage]) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| {
                let role_str = match m.role {
                    ollama_rs::generation::chat::MessageRole::User => "user",
                    ollama_rs::generation::chat::MessageRole::Assistant => "assistant",
                    ollama_rs::generation::chat::MessageRole::System => "system",
                    ollama_rs::generation::chat::MessageRole::Tool => "tool",
                };
                let mut msg_json = serde_json::json!({
                    "role": role_str,
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
    fn convert_model_options_to_parameters_ollama(
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
        GenericLLMTracingHelper::with_chat_response_tracing(
            self,
            metadata,
            parent_context,
            span_attributes,
            action,
        )
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
        GenericLLMTracingHelper::with_tool_response_tracing(
            self,
            metadata,
            parent_context,
            tool_attributes,
            &call.function.name,
            call.function.arguments.clone(),
            action,
        )
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
        let input_messages = Self::convert_messages_to_input_ollama(messages.lock().await.as_ref());
        let model_parameters = Self::convert_model_options_to_parameters_ollama(options);

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
        GenericLLMTracingHelper::trace_usage(
            self,
            metadata,
            parent_context,
            name,
            final_data,
            content,
        )
    }
}
