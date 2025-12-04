//! Common types for MistralRS plugin

use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;

/// Message type for MistralRS
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MistralRSMessage {
    pub role: TextMessageRole,
    pub content: String,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<MistralRSToolCall>>,
}

/// Tool call type for MistralRS
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MistralRSToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String, // JSON string
}

/// Serializable wrapper struct for OpenTelemetry tracing
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializableChatResponse {
    pub content: String,
    pub tool_calls_count: usize,
    pub finish_reason: String,
    pub usage_info: Option<String>,
    pub usage: SerializableUsage,
    pub model: Option<String>,
    pub response_id: Option<String>,
}

/// Serializable usage information for tracing
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializableUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl From<&mistralrs::ChatCompletionResponse> for SerializableChatResponse {
    fn from(response: &mistralrs::ChatCompletionResponse) -> Self {
        let first_choice = response.choices.first();
        Self {
            content: first_choice
                .and_then(|choice| choice.message.content.as_ref())
                .cloned()
                .unwrap_or_default(),
            tool_calls_count: first_choice
                .map(|choice| {
                    choice
                        .message
                        .tool_calls
                        .as_ref()
                        .map_or(0, |calls| calls.len())
                })
                .unwrap_or(0),
            finish_reason: first_choice
                .map(|choice| choice.finish_reason.clone())
                .unwrap_or_else(|| "unknown".to_string()),
            usage_info: Some(format!(
                "prompt_tokens: {}, completion_tokens: {}, total_tokens: {}",
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                response.usage.total_tokens
            )),
            usage: SerializableUsage {
                prompt_tokens: response.usage.prompt_tokens as u32,
                completion_tokens: response.usage.completion_tokens as u32,
                total_tokens: response.usage.total_tokens as u32,
            },
            model: Some(response.model.clone()),
            response_id: Some(response.id.clone()),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializableToolResults {
    pub messages: Vec<MistralRSMessage>,
    pub execution_count: usize,
    pub success_count: usize,
    pub error_count: usize,
}

impl From<&Vec<MistralRSMessage>> for SerializableToolResults {
    fn from(messages: &Vec<MistralRSMessage>) -> Self {
        let success_count = messages
            .iter()
            .filter(|msg| !msg.content.starts_with("Error"))
            .count();
        Self {
            messages: messages.clone(),
            execution_count: messages.len(),
            success_count,
            error_count: messages.len() - success_count,
        }
    }
}

/// Enhanced tool execution error handling
#[derive(Debug, thiserror::Error)]
pub enum ToolExecutionError {
    #[error("Tool function not found: {function_name}")]
    FunctionNotFound { function_name: String },
    #[error("Invalid tool arguments: {reason}")]
    InvalidArguments { reason: String },
    #[error("Tool execution timeout: {function_name} (timeout: {timeout_sec}s)")]
    Timeout {
        function_name: String,
        timeout_sec: u32,
    },
    #[error("Tool execution failed: {function_name} - {source}")]
    ExecutionFailed {
        function_name: String,
        source: anyhow::Error,
    },
    #[error("Too many tool iterations: {max_iterations}")]
    MaxIterationsExceeded { max_iterations: usize },
    #[error("Tool execution cancelled")]
    Cancelled,
}

/// Tool calling configuration
#[derive(Debug, Clone)]
pub struct ToolCallingConfig {
    pub max_iterations: usize,
    pub tool_timeout_sec: u32,
    pub parallel_execution: bool,
    pub grpc_endpoint: Option<String>,
    pub connection_timeout_sec: u32,
}

impl Default for ToolCallingConfig {
    fn default() -> Self {
        Self {
            max_iterations: std::env::var("MISTRAL_MAX_ITERATIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3),
            tool_timeout_sec: std::env::var("MISTRAL_TOOL_TIMEOUT_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            parallel_execution: std::env::var("MISTRAL_PARALLEL_TOOLS")
                .ok()
                .map(|v| v.to_lowercase() != "false")
                .unwrap_or(true),
            grpc_endpoint: std::env::var("MISTRAL_GRPC_ENDPOINT").ok(),
            connection_timeout_sec: std::env::var("MISTRAL_CONNECTION_TIMEOUT_SEC")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
        }
    }
}

impl ToolCallingConfig {
    /// Create config from proto settings (with environment variable fallback)
    pub fn from_proto(proto: &crate::mistral_proto::MistralRunnerSettings) -> Self {
        let default = Self::default();

        if let Some(tc) = &proto.tool_calling {
            Self {
                max_iterations: if tc.max_iterations > 0 {
                    tc.max_iterations as usize
                } else {
                    default.max_iterations
                },
                tool_timeout_sec: if tc.tool_timeout_sec > 0 {
                    tc.tool_timeout_sec
                } else {
                    default.tool_timeout_sec
                },
                parallel_execution: tc.parallel_execution,
                grpc_endpoint: if tc.grpc_endpoint.is_empty() {
                    default.grpc_endpoint
                } else {
                    Some(tc.grpc_endpoint.clone())
                },
                connection_timeout_sec: if tc.connection_timeout_sec > 0 {
                    tc.connection_timeout_sec
                } else {
                    default.connection_timeout_sec
                },
            }
        } else {
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole as TextMessageRole;

    #[test]
    fn test_mistral_rs_message_creation() {
        let msg = MistralRSMessage {
            role: TextMessageRole::User,
            content: "Hello".to_string(),
            tool_call_id: None,
            tool_calls: None,
        };

        assert_eq!(msg.content, "Hello");
        assert!(msg.tool_call_id.is_none());
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn test_mistral_rs_message_with_tool_calls() {
        let tool_call = MistralRSToolCall {
            id: "call-123".to_string(),
            function_name: "get_weather".to_string(),
            arguments: r#"{"city": "Tokyo"}"#.to_string(),
        };

        let msg = MistralRSMessage {
            role: TextMessageRole::Assistant,
            content: String::new(),
            tool_call_id: None,
            tool_calls: Some(vec![tool_call]),
        };

        assert!(msg.tool_calls.is_some());
        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function_name, "get_weather");
    }

    #[test]
    fn test_mistral_rs_tool_call() {
        let tool_call = MistralRSToolCall {
            id: "call-456".to_string(),
            function_name: "search".to_string(),
            arguments: r#"{"query": "rust programming"}"#.to_string(),
        };

        assert_eq!(tool_call.id, "call-456");
        assert_eq!(tool_call.function_name, "search");
        assert!(tool_call.arguments.contains("rust programming"));
    }

    #[test]
    fn test_tool_calling_config_default() {
        // Clear environment variables for consistent test
        std::env::remove_var("MISTRAL_GRPC_ENDPOINT");
        std::env::remove_var("MISTRAL_MAX_ITERATIONS");
        std::env::remove_var("MISTRAL_TOOL_TIMEOUT_SEC");
        std::env::remove_var("MISTRAL_PARALLEL_TOOLS");
        std::env::remove_var("MISTRAL_CONNECTION_TIMEOUT_SEC");

        let config = ToolCallingConfig::default();

        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.tool_timeout_sec, 30);
        assert!(config.parallel_execution);
        assert!(config.grpc_endpoint.is_none());
        assert_eq!(config.connection_timeout_sec, 10);
    }

    #[test]
    fn test_tool_calling_config_from_env() {
        // Set environment variables
        std::env::set_var("MISTRAL_GRPC_ENDPOINT", "http://localhost:50051");
        std::env::set_var("MISTRAL_MAX_ITERATIONS", "5");
        std::env::set_var("MISTRAL_TOOL_TIMEOUT_SEC", "60");
        std::env::set_var("MISTRAL_PARALLEL_TOOLS", "false");
        std::env::set_var("MISTRAL_CONNECTION_TIMEOUT_SEC", "20");

        let config = ToolCallingConfig::default();

        assert_eq!(config.max_iterations, 5);
        assert_eq!(config.tool_timeout_sec, 60);
        assert!(!config.parallel_execution);
        assert_eq!(
            config.grpc_endpoint,
            Some("http://localhost:50051".to_string())
        );
        assert_eq!(config.connection_timeout_sec, 20);

        // Clean up
        std::env::remove_var("MISTRAL_GRPC_ENDPOINT");
        std::env::remove_var("MISTRAL_MAX_ITERATIONS");
        std::env::remove_var("MISTRAL_TOOL_TIMEOUT_SEC");
        std::env::remove_var("MISTRAL_PARALLEL_TOOLS");
        std::env::remove_var("MISTRAL_CONNECTION_TIMEOUT_SEC");
    }

    #[test]
    fn test_tool_calling_config_from_proto() {
        let proto = crate::mistral_proto::MistralRunnerSettings {
            tool_calling: Some(
                crate::mistral_proto::mistral_runner_settings::ToolCallingConfig {
                    grpc_endpoint: "http://custom:8080".to_string(),
                    max_iterations: 10,
                    tool_timeout_sec: 120,
                    parallel_execution: false,
                    connection_timeout_sec: 30,
                },
            ),
        };

        let config = ToolCallingConfig::from_proto(&proto);

        assert_eq!(config.max_iterations, 10);
        assert_eq!(config.tool_timeout_sec, 120);
        assert!(!config.parallel_execution);
        assert_eq!(config.grpc_endpoint, Some("http://custom:8080".to_string()));
        assert_eq!(config.connection_timeout_sec, 30);
    }

    #[test]
    fn test_tool_calling_config_from_proto_empty() {
        let proto = crate::mistral_proto::MistralRunnerSettings { tool_calling: None };

        // Clear environment variables
        std::env::remove_var("MISTRAL_GRPC_ENDPOINT");
        std::env::remove_var("MISTRAL_MAX_ITERATIONS");

        let config = ToolCallingConfig::from_proto(&proto);

        // Should fall back to defaults
        assert_eq!(config.max_iterations, 3);
        assert_eq!(config.tool_timeout_sec, 30);
    }

    #[test]
    fn test_tool_execution_error_display() {
        let err = ToolExecutionError::FunctionNotFound {
            function_name: "unknown_fn".to_string(),
        };
        assert!(err.to_string().contains("unknown_fn"));

        let err = ToolExecutionError::Timeout {
            function_name: "slow_fn".to_string(),
            timeout_sec: 30,
        };
        assert!(err.to_string().contains("slow_fn"));
        assert!(err.to_string().contains("30"));

        let err = ToolExecutionError::MaxIterationsExceeded { max_iterations: 5 };
        assert!(err.to_string().contains("5"));

        let err = ToolExecutionError::Cancelled;
        assert!(err.to_string().contains("cancelled"));
    }

    #[test]
    fn test_serializable_chat_response() {
        let response = SerializableChatResponse {
            content: "Hello".to_string(),
            tool_calls_count: 0,
            finish_reason: "stop".to_string(),
            usage_info: Some("tokens: 10".to_string()),
            usage: SerializableUsage {
                prompt_tokens: 5,
                completion_tokens: 5,
                total_tokens: 10,
            },
            model: Some("test-model".to_string()),
            response_id: Some("resp-123".to_string()),
        };

        assert_eq!(response.content, "Hello");
        assert_eq!(response.usage.total_tokens, 10);
    }

    #[test]
    fn test_serializable_tool_results() {
        let messages = vec![
            MistralRSMessage {
                role: TextMessageRole::Tool,
                content: "Success result".to_string(),
                tool_call_id: Some("call-1".to_string()),
                tool_calls: None,
            },
            MistralRSMessage {
                role: TextMessageRole::Tool,
                content: "Error: failed".to_string(),
                tool_call_id: Some("call-2".to_string()),
                tool_calls: None,
            },
        ];

        let results = SerializableToolResults::from(&messages);

        assert_eq!(results.execution_count, 2);
        assert_eq!(results.success_count, 1);
        assert_eq!(results.error_count, 1);
    }
}
