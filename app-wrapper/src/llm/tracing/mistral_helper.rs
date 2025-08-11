use anyhow::Result;
use jobworkerp_base::error::JobWorkerError;
use mistralrs::{ChatCompletionResponse, Tool};
use net_utils::trace::attr::OtelSpanAttributes;
use net_utils::trace::otel_span::GenAIOtelClient;
use std::collections::HashMap;

use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, ModelOptions as GenericModelOptions,
    ToolInfo as GenericToolInfo, UsageData,
};
use crate::llm::mistral::{MistralRSMessage, SerializableChatResponse, SerializableToolResults};

// Import for unified LLM tracing
use crate::llm::tracing::{
    LLMInput, LLMOptions, LLMRequestData, LLMResponseData, LLMTool, UsageData as LLMUsageData,
};
use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::{message_content, ChatRole};
use jobworkerp_runner::jobworkerp::runner::llm::LlmChatArgs;

// Trait implementations for MistralRS-specific types
impl LLMMessage for MistralRSMessage {
    fn get_role(&self) -> &str {
        match self.role {
            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::User => "user",
            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Assistant => {
                "assistant"
            }
            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::System => "system",
            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool => "tool",
            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Unspecified => {
                "user"
            }
        }
    }

    fn get_content(&self) -> &str {
        &self.content
    }
}

// ModelOptions placeholder - MistralRS doesn't have a direct equivalent
#[derive(Debug, Clone)]
pub struct MistralModelOptions {
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f32>,
}

impl GenericModelOptions for MistralModelOptions {}

impl GenericToolInfo for Tool {
    fn get_name(&self) -> &str {
        &self.function.name
    }
}

impl ChatResponse for SerializableChatResponse {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "content": self.content,
            "tool_calls_count": self.tool_calls_count,
            "finish_reason": self.finish_reason,
            "usage_info": self.usage_info
        })
    }
}

impl ChatResponse for ChatCompletionResponse {
    fn to_json(&self) -> serde_json::Value {
        let serializable = SerializableChatResponse::from(self);
        serializable.to_json()
    }
}

// Usage data placeholder - MistralRS usage info
#[derive(Debug, Clone)]
pub struct MistralUsageData {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl UsageData for MistralUsageData {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert("prompt_tokens".to_string(), self.prompt_tokens as i64);
        usage.insert(
            "completion_tokens".to_string(),
            self.completion_tokens as i64,
        );
        usage.insert("total_tokens".to_string(), self.total_tokens as i64);
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens
        })
    }
}

/// Trait for OpenTelemetry tracing functionality in MistralRS services
pub trait MistralTracingHelper: GenericLLMTracingHelper {
    /// Convert MistralRSMessage vector to proper tracing input format
    fn convert_messages_to_input_mistral(messages: &[MistralRSMessage]) -> serde_json::Value {
        serde_json::json!(messages
            .iter()
            .map(|m| {
                let role_str = m.get_role();
                let mut msg_json = serde_json::json!({
                    "role": role_str,
                    "content": m.content
                });

                // Add tool_calls if present
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

                // Add tool_call_id if present
                if let Some(tool_call_id) = &m.tool_call_id {
                    msg_json["tool_call_id"] = serde_json::json!(tool_call_id);
                }

                msg_json
            })
            .collect::<Vec<_>>())
    }

    /// Convert MistralModelOptions to proper model parameters format
    fn convert_model_options_to_parameters_mistral(
        options: &MistralModelOptions,
    ) -> HashMap<String, serde_json::Value> {
        let mut parameters = HashMap::new();

        if let Some(temp) = options.temperature {
            parameters.insert("temperature".to_string(), serde_json::json!(temp));
        }
        if let Some(max_tokens) = options.max_tokens {
            parameters.insert("max_tokens".to_string(), serde_json::json!(max_tokens));
        }
        if let Some(top_p) = options.top_p {
            parameters.insert("top_p".to_string(), serde_json::json!(top_p));
        }

        parameters
    }

    /// Create tool call span attributes from MistralRS tool call information
    fn create_tool_call_span_from_mistral_call(
        &self,
        function_name: &str,
        arguments: &str,
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let arguments_json =
            serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!(arguments));

        self.create_tool_call_span_attributes(function_name, arguments_json, metadata)
    }

    /// Execute chat action with proper parent-child span tracing and response recording
    fn with_chat_response_tracing<F>(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: Option<opentelemetry::Context>,
        span_attributes: OtelSpanAttributes,
        action: F,
    ) -> impl std::future::Future<Output = Result<(SerializableChatResponse, opentelemetry::Context)>>
           + Send
    where
        F: std::future::Future<Output = Result<SerializableChatResponse, JobWorkerError>>
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
        function_name: &str,
        arguments: &str,
        action: F,
    ) -> impl std::future::Future<Output = Result<(serde_json::Value, opentelemetry::Context)>>
           + Send
           + 'static
    where
        F: std::future::Future<Output = Result<serde_json::Value, JobWorkerError>> + Send + 'static,
    {
        let arguments_json =
            serde_json::from_str(arguments).unwrap_or_else(|_| serde_json::json!(arguments));

        GenericLLMTracingHelper::with_tool_response_tracing(
            self,
            metadata,
            parent_context,
            tool_attributes,
            function_name,
            arguments_json,
            action,
        )
    }

    /// Create chat completion span attributes from MistralRS request components
    fn create_chat_span_from_request(
        &self,
        model: &str,
        messages: &[MistralRSMessage],
        options: &MistralModelOptions,
        tools: &[Tool],
        metadata: &HashMap<String, String>,
    ) -> OtelSpanAttributes {
        let input_messages = Self::convert_messages_to_input_mistral(messages);
        let model_parameters = Self::convert_model_options_to_parameters_mistral(options);

        self.create_chat_completion_span_attributes(
            model,
            input_messages,
            Some(&model_parameters),
            tools,
            metadata,
        )
    }

    /// Trace tool execution results with proper serialization
    fn trace_tool_results(
        &self,
        metadata: &HashMap<String, String>,
        parent_context: opentelemetry::Context,
        name: &str,
        tool_results: &SerializableToolResults,
        content: Option<&str>,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'static {
        // Convert SerializableToolResults to an owned struct to avoid lifetime issues
        #[derive(Clone)]
        struct OwnedToolResultsUsage {
            execution_count: usize,
            success_count: usize,
            error_count: usize,
            data: SerializableToolResults,
        }

        impl UsageData for OwnedToolResultsUsage {
            fn to_usage_map(&self) -> HashMap<String, i64> {
                let mut usage = HashMap::new();
                usage.insert("execution_count".to_string(), self.execution_count as i64);
                usage.insert("success_count".to_string(), self.success_count as i64);
                usage.insert("error_count".to_string(), self.error_count as i64);
                usage
            }

            fn to_json(&self) -> serde_json::Value {
                serde_json::to_value(&self.data).unwrap_or_default()
            }
        }

        let metadata_owned = metadata.clone();
        let content_owned = content.map(|s| s.to_string());
        let name_owned = name.to_string();
        let wrapper = OwnedToolResultsUsage {
            execution_count: tool_results.execution_count,
            success_count: tool_results.success_count,
            error_count: tool_results.error_count,
            data: tool_results.clone(),
        };

        let otel_client = self.get_otel_client().cloned();
        let _provider = self.get_provider_name().to_string();

        async move {
            if let Some(client) = otel_client {
                let usage = wrapper.to_usage_map();
                let output = serde_json::json!({
                    "content": content_owned.as_deref().unwrap_or_default(),
                    "usage": wrapper.to_json()
                });

                let mut span_builder = net_utils::trace::attr::OtelSpanBuilder::new(&name_owned)
                    .span_type(net_utils::trace::attr::OtelSpanType::Event)
                    .usage(usage)
                    .output(output)
                    .level("INFO");

                if let Some(session_id) = metadata_owned.get("session_id") {
                    span_builder = span_builder.session_id(session_id.clone());
                }
                if let Some(user_id) = metadata_owned.get("user_id") {
                    span_builder = span_builder.user_id(user_id.clone());
                }
                let span_attributes = span_builder.build();

                client
                    .with_span_result(span_attributes, Some(parent_context), async {
                        Ok::<(), jobworkerp_base::error::JobWorkerError>(())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Error in traced span: {}", e))?;
            }
            Ok(())
        }
    }
}

// === Unified LLM Tracing Implementation ===

/// Unified tracing data for MistralRS using actual data
pub struct MistralTracingData {
    pub args: LlmChatArgs,                    // Original request arguments
    pub resolved_tools: Vec<mistralrs::Tool>, // Pre-resolved actual tool information
    pub model_name: String,                   // Actual model name
}

impl LLMRequestData for MistralTracingData {
    /// Extract tracing input from actual messages
    fn extract_input(&self) -> LLMInput {
        LLMInput {
            messages: serde_json::json!(self
                .args
                .messages
                .iter()
                .map(|m| {
                    let role_str = match ChatRole::try_from(m.role).unwrap_or(ChatRole::User) {
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                        ChatRole::System => "system",
                        ChatRole::Tool => "tool",
                        _ => "user",
                    };

                    let content = m
                        .content
                        .as_ref()
                        .and_then(|c| c.content.as_ref())
                        .map(|content| match content {
                            message_content::Content::Text(text) => text.clone(),
                            message_content::Content::ToolCalls(_) => String::new(), // Do not display tool calls content
                            message_content::Content::Image(_) => String::new(), // Do not display image content
                        })
                        .unwrap_or_default();

                    serde_json::json!({
                        "role": role_str,
                        "content": content
                    })
                })
                .collect::<Vec<_>>()),
            prompt: None, // MistralRS uses message-based interface
        }
    }

    /// Extract actual LLM options
    fn extract_options(&self) -> Option<LLMOptions> {
        self.args.options.as_ref().map(|opts| LLMOptions {
            parameters: [
                ("max_tokens", opts.max_tokens.map(|t| serde_json::json!(t))),
                (
                    "temperature",
                    opts.temperature.map(|t| serde_json::json!(t)),
                ),
                ("top_p", opts.top_p.map(|t| serde_json::json!(t))),
                ("seed", opts.seed.map(|s| serde_json::json!(s))),
            ]
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k.to_string(), val.clone())))
            .collect(),
        })
    }

    /// Use actually resolved tool information (excluding dummy data)
    fn extract_tools(&self) -> Vec<LLMTool> {
        self.resolved_tools
            .iter()
            .map(|tool| LLMTool {
                name: tool.function.name.clone(),
                description: tool.function.description.clone().unwrap_or_default(),
                parameters: if let Some(params) = tool.function.parameters.clone() {
                    serde_json::to_value(&params)
                        .unwrap_or_else(|_| serde_json::json!({"type": "object"}))
                } else {
                    serde_json::json!({"type": "object"})
                },
            })
            .collect()
    }

    /// Use actual model name
    fn extract_model(&self) -> Option<String> {
        Some(self.model_name.clone())
    }
}

/// MistralRS usage data using actual response data
impl LLMUsageData for mistralrs::Usage {
    fn to_usage_map(&self) -> std::collections::HashMap<String, i64> {
        let mut usage = std::collections::HashMap::new();
        usage.insert("prompt_tokens".to_string(), self.prompt_tokens as i64);
        usage.insert(
            "completion_tokens".to_string(),
            self.completion_tokens as i64,
        );
        usage.insert("total_tokens".to_string(), self.total_tokens as i64);
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "total_tokens": self.total_tokens
        })
    }
}

/// MistralRS response data using actual response content
impl LLMResponseData for mistralrs::ChatCompletionResponse {
    fn to_trace_output(&self) -> serde_json::Value {
        self.choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .map(|content| serde_json::json!(content))
            .unwrap_or_else(|| serde_json::json!(""))
    }

    fn extract_usage(&self) -> Option<Box<dyn LLMUsageData>> {
        Some(Box::new(self.usage.clone()) as Box<dyn LLMUsageData>)
    }

    fn extract_content(&self) -> Option<String> {
        self.choices
            .first()
            .and_then(|choice| choice.message.content.clone())
    }
}
