//! Unified LLM Runner with multi-method support
//!
//! This module provides a unified LLM runner that supports both 'completion' and 'chat' methods
//! via the `using` parameter. This replaces the deprecated LLM_COMPLETION and LLM_CHAT runners.
//!
//! # Methods
//! - `completion`: Text completion using LLM (prompt-based)
//! - `chat`: Chat conversation with message history
//!
//! # Usage
//! The `using` parameter is **required** for this runner. Calling without specifying a method
//! will result in an error.

use super::llm::{LLMCompletionRunnerSpec, LLMCompletionRunnerSpecImpl};
use super::llm_chat::LLMChatRunnerSpecImpl;
use super::{CollectStreamFuture, MethodJsonSchema, RunnerSpec};
use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;

/// Method name for completion
pub const METHOD_COMPLETION: &str = "completion";
/// Method name for chat
pub const METHOD_CHAT: &str = "chat";

/// Unified LLM Runner specification implementation
///
/// This runner supports two methods:
/// - `completion`: Uses LLMCompletionArgs/LLMCompletionResult
/// - `chat`: Uses LLMChatArgs/LLMChatResult
pub struct LLMUnifiedRunnerSpecImpl {
    completion_spec: LLMCompletionRunnerSpecImpl,
    chat_spec: LLMChatRunnerSpecImpl,
}

impl LLMUnifiedRunnerSpecImpl {
    pub fn new() -> Self {
        Self {
            completion_spec: LLMCompletionRunnerSpecImpl::new(),
            chat_spec: LLMChatRunnerSpecImpl::new(),
        }
    }

    /// Resolve the method name from `using` parameter
    ///
    /// Returns an error if `using` is None or an unknown method
    pub fn resolve_method(using: Option<&str>) -> Result<&str> {
        match using {
            Some(METHOD_COMPLETION) => Ok(METHOD_COMPLETION),
            Some(METHOD_CHAT) => Ok(METHOD_CHAT),
            Some(other) => Err(anyhow!(
                "Unknown method '{}' for LLM runner. Available methods: {}, {}",
                other,
                METHOD_COMPLETION,
                METHOD_CHAT
            )),
            None => Err(anyhow!(
                "Method specification required for LLM runner. Use '{}' or '{}'",
                METHOD_COMPLETION,
                METHOD_CHAT
            )),
        }
    }
}

impl Default for LLMUnifiedRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for LLMUnifiedRunnerSpecImpl {
    fn name(&self) -> String {
        RunnerType::Llm.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        // Both completion and chat use the same runner settings
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();

        // completion method
        schemas.insert(
            METHOD_COMPLETION.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/completion_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/completion_result.proto"
                )
                .to_string(),
                description: Some("Generate text completion using LLM".to_string()),
                output_type: StreamingOutputType::Both as i32,
            },
        );

        // chat method
        schemas.insert(
            METHOD_CHAT.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/llm/chat_args.proto")
                    .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/llm/chat_result.proto"
                )
                .to_string(),
                description: Some(
                    "Generate chat response using LLM with conversation history".to_string(),
                ),
                output_type: StreamingOutputType::Both as i32,
            },
        );

        schemas
    }

    fn method_json_schema_map(&self) -> HashMap<String, MethodJsonSchema> {
        let mut schemas = HashMap::new();

        // Get schemas from underlying implementations
        let completion_schemas =
            LLMCompletionRunnerSpec::method_json_schema_map(&self.completion_spec);
        let chat_schemas = self.chat_spec.method_json_schema_map();

        // Map "run" to method-specific names
        if let Some(completion_schema) = completion_schemas.get(proto::DEFAULT_METHOD_NAME) {
            schemas.insert(METHOD_COMPLETION.to_string(), completion_schema.clone());
        }
        if let Some(chat_schema) = chat_schemas.get(proto::DEFAULT_METHOD_NAME) {
            schemas.insert(METHOD_CHAT.to_string(), chat_schema.clone());
        }

        schemas
    }

    fn settings_schema(&self) -> String {
        // Both methods use the same settings schema
        LLMCompletionRunnerSpec::settings_schema(&self.completion_spec)
    }

    /// Collect streaming output based on the method specified
    ///
    /// Delegates to the appropriate underlying runner's collect_stream
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> CollectStreamFuture {
        match Self::resolve_method(using) {
            Ok(METHOD_COMPLETION) => self.completion_spec.collect_stream(stream, using),
            Ok(METHOD_CHAT) => self.chat_spec.collect_stream(stream, using),
            Ok(_) => {
                // Should not reach here due to resolve_method validation
                Box::pin(
                    async move { Err(anyhow!("Internal error: unknown method after validation")) },
                )
            }
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_method_completion() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(Some("completion"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "completion");
    }

    #[test]
    fn test_resolve_method_chat() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(Some("chat"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "chat");
    }

    #[test]
    fn test_resolve_method_unknown() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(Some("unknown"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown method 'unknown'"));
    }

    #[test]
    fn test_resolve_method_none_is_error() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Method specification required"));
    }

    #[test]
    fn test_runner_name() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        assert_eq!(runner.name(), "LLM");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_proto_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
        assert_eq!(schemas.len(), 2);

        // Verify completion method
        let completion = schemas.get("completion").unwrap();
        assert!(completion
            .description
            .as_ref()
            .unwrap()
            .contains("completion"));
        assert!(!completion.args_proto.is_empty());
        assert!(!completion.result_proto.is_empty());

        // Verify chat method
        let chat = schemas.get("chat").unwrap();
        assert!(chat.description.as_ref().unwrap().contains("chat"));
        assert!(!chat.args_proto.is_empty());
        assert!(!chat.result_proto.is_empty());
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_json_schema_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
        assert_eq!(schemas.len(), 2);
    }
}
