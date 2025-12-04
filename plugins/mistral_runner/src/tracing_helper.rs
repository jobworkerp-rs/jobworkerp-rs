//! OpenTelemetry tracing helper for MistralRS plugin
//!
//! Provides OpenTelemetry integration for LLM chat, completion, and tool calling operations.

use command_utils::trace::attr::{OtelSpanAttributes, OtelSpanBuilder, OtelSpanType};
use command_utils::trace::impls::GenericOtelClient;
use command_utils::trace::otel_span::GenAIOtelClient;
use opentelemetry::global;
use opentelemetry::Context;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::Level;

/// Initialize tracing for the plugin
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .try_init();
}

/// Extract parent OpenTelemetry context from metadata
///
/// Uses W3C TraceContext propagator to extract traceparent/tracestate headers
pub fn extract_parent_context(metadata: &HashMap<String, String>) -> Context {
    global::get_text_map_propagator(|prop| prop.extract(metadata))
}

/// OpenTelemetry client wrapper for MistralRS plugin
#[derive(Clone)]
pub struct MistralOtelClient {
    client: Arc<GenericOtelClient>,
}

impl MistralOtelClient {
    /// Create a new OpenTelemetry client for MistralRS
    pub fn new() -> Self {
        Self {
            client: Arc::new(GenericOtelClient::new("mistralrs.plugin")),
        }
    }

    /// Create chat completion span attributes
    pub fn create_chat_span_attributes(
        &self,
        model: &str,
        messages: serde_json::Value,
        options: Option<&HashMap<String, serde_json::Value>>,
        tools: &[mistralrs::Tool],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new("mistralrs.chat")
            .span_type(OtelSpanType::Generation)
            .model(model)
            .input(messages)
            .system("mistralrs")
            .operation_name("chat")
            .level("INFO");

        if let Some(params) = options {
            span_builder = span_builder.model_parameters(params.clone());
        }

        if !tools.is_empty() {
            let tools_json = serde_json::json!(tools
                .iter()
                .map(|t| serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description
                }))
                .collect::<Vec<_>>());

            let mut meta = HashMap::new();
            meta.insert("tools".to_string(), tools_json);
            span_builder = span_builder.metadata(meta);
        }

        if let Some(session_id) = metadata.get("session_id") {
            span_builder = span_builder.session_id(session_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        span_builder.build()
    }

    /// Create completion span attributes
    pub fn create_completion_span_attributes(
        &self,
        model: &str,
        prompt: &str,
        options: Option<&HashMap<String, serde_json::Value>>,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new("mistralrs.completion")
            .span_type(OtelSpanType::Generation)
            .model(model)
            .input(serde_json::json!(prompt))
            .system("mistralrs")
            .operation_name("completion")
            .level("INFO");

        if let Some(params) = options {
            span_builder = span_builder.model_parameters(params.clone());
        }

        if let Some(session_id) = metadata.get("session_id") {
            span_builder = span_builder.session_id(session_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        span_builder.build()
    }

    /// Create tool call span attributes
    pub fn create_tool_call_span_attributes(
        &self,
        function_name: &str,
        arguments: serde_json::Value,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let mut span_builder = OtelSpanBuilder::new(format!("mistralrs.tool.{}", function_name))
            .span_type(OtelSpanType::Event)
            .input(arguments)
            .level("INFO")
            .openinference_span_kind("TOOL");

        if let Some(session_id) = metadata.get("session_id") {
            span_builder = span_builder.session_id(session_id.clone());
        }
        if let Some(user_id) = metadata.get("user_id") {
            span_builder = span_builder.user_id(user_id.clone());
        }

        span_builder.build()
    }

    /// Execute an async action within a traced span with manual span management
    ///
    /// This method is designed for use with anyhow::Result which doesn't implement std::error::Error.
    /// It uses direct span management rather than the trait-based approach.
    /// The span is explicitly ended after the action completes.
    pub async fn execute_with_span<F, T>(
        &self,
        attributes: OtelSpanAttributes,
        parent_context: Option<Context>,
        action: F,
    ) -> anyhow::Result<T>
    where
        F: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        use opentelemetry::trace::{Span, Status};

        let mut span = if let Some(ctx) = parent_context {
            self.client.start_with_context(attributes, ctx)
        } else {
            self.client.start_new_span(attributes)
        };

        // Execute the action
        let result = action.await;

        // Set span status and end it
        match &result {
            Ok(_) => {
                span.set_status(Status::Ok);
                tracing::debug!("Span completed successfully");
            }
            Err(e) => {
                span.set_status(Status::error(e.to_string()));
                tracing::warn!("Span completed with error: {}", e);
            }
        }
        span.end();

        result
    }

    /// Record chat response in span
    pub fn record_chat_response(
        &self,
        response: &mistralrs::ChatCompletionResponse,
    ) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert(
            "prompt_tokens".to_string(),
            response.usage.prompt_tokens as i64,
        );
        usage.insert(
            "completion_tokens".to_string(),
            response.usage.completion_tokens as i64,
        );
        usage.insert(
            "total_tokens".to_string(),
            response.usage.total_tokens as i64,
        );
        usage
    }

    /// Record completion response in span
    pub fn record_completion_response(
        &self,
        response: &mistralrs::CompletionResponse,
    ) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert(
            "prompt_tokens".to_string(),
            response.usage.prompt_tokens as i64,
        );
        usage.insert(
            "completion_tokens".to_string(),
            response.usage.completion_tokens as i64,
        );
        usage.insert(
            "total_tokens".to_string(),
            response.usage.total_tokens as i64,
        );
        usage
    }

    /// Get inner client reference
    pub fn inner(&self) -> &Arc<GenericOtelClient> {
        &self.client
    }
}

impl Default for MistralOtelClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert MistralRS messages to JSON for tracing
pub fn messages_to_json(messages: &[crate::core::types::MistralRSMessage]) -> serde_json::Value {
    serde_json::json!(messages
        .iter()
        .map(|m| {
            let role_str = match m.role {
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::User => "user",
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Assistant => {
                    "assistant"
                }
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::System => {
                    "system"
                }
                jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool => "tool",
                _ => "user",
            };

            let mut msg_json = serde_json::json!({
                "role": role_str,
                "content": m.content
            });

            if let Some(tool_calls) = &m.tool_calls {
                if !tool_calls.is_empty() {
                    msg_json["tool_calls"] = serde_json::json!(tool_calls
                        .iter()
                        .map(|tc| serde_json::json!({
                            "id": tc.id,
                            "function": {
                                "name": tc.function_name,
                                "arguments": tc.arguments
                            }
                        }))
                        .collect::<Vec<_>>());
                }
            }

            if let Some(tool_call_id) = &m.tool_call_id {
                msg_json["tool_call_id"] = serde_json::json!(tool_call_id);
            }

            msg_json
        })
        .collect::<Vec<_>>())
}

/// Convert chat response to JSON for tracing output
pub fn chat_response_to_json(response: &mistralrs::ChatCompletionResponse) -> serde_json::Value {
    let first_choice = response.choices.first();

    if let Some(choice) = first_choice {
        let has_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty());

        if has_tool_calls {
            let tool_calls = choice
                .message
                .tool_calls
                .as_ref()
                .map(|calls| {
                    calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "function": {
                                    "name": tc.function.name,
                                    "arguments": tc.function.arguments
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            serde_json::json!({
                "role": "assistant",
                "tool_calls": tool_calls,
                "finish_reason": choice.finish_reason
            })
        } else {
            let content = choice.message.content.clone().unwrap_or_default();
            serde_json::json!(content)
        }
    } else {
        serde_json::json!("")
    }
}

/// Convert completion response to JSON for tracing output
pub fn completion_response_to_json(response: &mistralrs::CompletionResponse) -> serde_json::Value {
    if let Some(choice) = response.choices.first() {
        serde_json::json!(choice.text)
    } else {
        serde_json::json!("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mistral_otel_client_creation() {
        let client = MistralOtelClient::new();
        assert!(client.inner().get_tracer_name().contains("mistralrs"));
    }

    #[test]
    fn test_create_chat_span_attributes() {
        let client = MistralOtelClient::new();
        let messages = serde_json::json!([{"role": "user", "content": "Hello"}]);
        let metadata = HashMap::new();

        let attrs =
            client.create_chat_span_attributes("test-model", messages, None, &[], &metadata);

        assert_eq!(attrs.name, "mistralrs.chat");
        assert_eq!(attrs.model, Some("test-model".to_string()));
    }

    #[test]
    fn test_create_completion_span_attributes() {
        let client = MistralOtelClient::new();
        let metadata = HashMap::new();

        let attrs =
            client.create_completion_span_attributes("test-model", "Hello", None, &metadata);

        assert_eq!(attrs.name, "mistralrs.completion");
        assert_eq!(attrs.model, Some("test-model".to_string()));
    }

    #[test]
    fn test_create_tool_call_span_attributes() {
        let client = MistralOtelClient::new();
        let args = serde_json::json!({"city": "Tokyo"});
        let metadata = HashMap::new();

        let attrs = client.create_tool_call_span_attributes("get_weather", args, &metadata);

        assert_eq!(attrs.name, "mistralrs.tool.get_weather");
        assert_eq!(attrs.openinference_span_kind, Some("TOOL".to_string()));
    }

    #[test]
    fn test_messages_to_json() {
        use crate::core::types::MistralRSMessage;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole;

        let messages = vec![
            MistralRSMessage {
                role: ChatRole::User,
                content: "Hello".to_string(),
                tool_call_id: None,
                tool_calls: None,
            },
            MistralRSMessage {
                role: ChatRole::Assistant,
                content: "Hi there!".to_string(),
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let json = messages_to_json(&messages);
        assert!(json.is_array());

        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["role"], "user");
        assert_eq!(arr[0]["content"], "Hello");
        assert_eq!(arr[1]["role"], "assistant");
    }
}
