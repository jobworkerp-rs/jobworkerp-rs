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
use super::llm_embedding::{LLMEmbeddingRunnerSpec, LLMEmbeddingRunnerSpecImpl};
use super::{CollectStreamFuture, RunnerSpec};
use anyhow::{Result, anyhow};
use futures::stream::BoxStream;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;

/// Method name for completion
pub const METHOD_COMPLETION: &str = "completion";
/// Method name for chat
pub const METHOD_CHAT: &str = "chat";
/// Method name for embedding
pub const METHOD_EMBEDDING: &str = "embedding";

/// Unified LLM Runner specification implementation
///
/// This runner supports two methods:
/// - `completion`: Uses LLMCompletionArgs/LLMCompletionResult
/// - `chat`: Uses LLMChatArgs/LLMChatResult
pub struct LLMUnifiedRunnerSpecImpl {
    completion_spec: LLMCompletionRunnerSpecImpl,
    chat_spec: LLMChatRunnerSpecImpl,
    embedding_spec: LLMEmbeddingRunnerSpecImpl,
}

impl LLMUnifiedRunnerSpecImpl {
    pub fn new() -> Self {
        Self {
            completion_spec: LLMCompletionRunnerSpecImpl::new(),
            chat_spec: LLMChatRunnerSpecImpl::new(),
            embedding_spec: LLMEmbeddingRunnerSpecImpl::new(),
        }
    }

    /// Resolve the method name from `using` parameter
    ///
    /// Returns an error if `using` is None or an unknown method
    pub fn resolve_method(using: Option<&str>) -> Result<&str> {
        match using {
            Some(METHOD_COMPLETION) => Ok(METHOD_COMPLETION),
            Some(METHOD_CHAT) => Ok(METHOD_CHAT),
            Some(METHOD_EMBEDDING) => Ok(METHOD_EMBEDDING),
            Some(other) => Err(anyhow!(
                "Unknown method '{}' for LLM runner. Available methods: {}, {}, {}",
                other,
                METHOD_COMPLETION,
                METHOD_CHAT,
                METHOD_EMBEDDING
            )),
            None => Err(anyhow!(
                "Method specification required for LLM runner. Use '{}', '{}' or '{}'",
                METHOD_COMPLETION,
                METHOD_CHAT,
                METHOD_EMBEDDING
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
                ..Default::default()
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
                ..Default::default()
            },
        );

        // embedding method (delegated to embedding_spec, which stores an
        // import-resolved, self-contained args_proto)
        if let Some(embedding) = LLMEmbeddingRunnerSpec::method_proto_map(&self.embedding_spec)
            .remove(proto::DEFAULT_METHOD_NAME)
        {
            schemas.insert(METHOD_EMBEDDING.to_string(), embedding);
        }

        schemas
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        let mut schemas = HashMap::new();

        // Get schemas from underlying implementations
        let completion_schemas =
            LLMCompletionRunnerSpec::method_json_schema_map(&self.completion_spec);
        let chat_schemas = self.chat_spec.method_json_schema_map();
        // Embedding uses the RunnerSpec default (from_proto_map), which works
        // because embedding_spec's args_proto is import-resolved.
        let embedding_schemas = RunnerSpec::method_json_schema_map(&self.embedding_spec);

        // Map "run" to method-specific names
        if let Some(completion_schema) = completion_schemas.get(proto::DEFAULT_METHOD_NAME) {
            schemas.insert(METHOD_COMPLETION.to_string(), completion_schema.clone());
        }
        if let Some(chat_schema) = chat_schemas.get(proto::DEFAULT_METHOD_NAME) {
            schemas.insert(METHOD_CHAT.to_string(), chat_schema.clone());
        }
        if let Some(embedding_schema) = embedding_schemas.get(proto::DEFAULT_METHOD_NAME) {
            schemas.insert(METHOD_EMBEDDING.to_string(), embedding_schema.clone());
        }

        schemas
    }

    fn settings_schema(&self) -> String {
        // All three methods share the same LlmRunnerSettings. Use the
        // embedding spec's runtime-generated schema (method A) rather than the
        // completion spec's static JSON (method B): the static file predates
        // `embedding_chunking` (and the genai variant), so a client reading
        // runner metadata for a settings UI would not see those fields.
        // Generating from the proto keeps the settings schema in sync.
        LLMEmbeddingRunnerSpec::settings_schema(&self.embedding_spec)
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
            // Embedding is non-streaming; use the default RunnerSpec collect
            // (returns the single item / error passthrough).
            Ok(METHOD_EMBEDDING) => self.embedding_spec.collect_stream(stream, using),
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
    fn test_resolve_method_embedding() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(Some("embedding"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "embedding");
    }

    #[test]
    fn test_resolve_method_unknown() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(Some("unknown"));
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("Unknown method 'unknown'"));
        // Error message must list all three available methods.
        assert!(msg.contains("embedding"), "should list embedding: {msg}");
    }

    #[test]
    fn test_resolve_method_none_is_error() {
        let result = LLMUnifiedRunnerSpecImpl::resolve_method(None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Method specification required")
        );
    }

    #[test]
    fn test_runner_name() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        assert_eq!(runner.name(), "LLM");
    }

    #[test]
    fn test_method_proto_map_has_three_methods() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_proto_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
        assert!(schemas.contains_key("embedding"));
        assert_eq!(schemas.len(), 3);

        // Verify completion method
        let completion = schemas.get("completion").unwrap();
        assert!(
            completion
                .description
                .as_ref()
                .unwrap()
                .contains("completion")
        );
        assert!(!completion.args_proto.is_empty());
        assert!(!completion.result_proto.is_empty());

        // Verify chat method
        let chat = schemas.get("chat").unwrap();
        assert!(chat.description.as_ref().unwrap().contains("chat"));
        assert!(!chat.args_proto.is_empty());
        assert!(!chat.result_proto.is_empty());

        // Verify embedding method: args_proto must be import-resolved
        // (self-contained), otherwise schema generation silently degrades.
        let embedding = schemas.get("embedding").unwrap();
        assert!(!embedding.args_proto.is_empty());
        assert!(!embedding.result_proto.is_empty());
        assert!(
            !embedding
                .args_proto
                .lines()
                .any(|l| l.trim().starts_with("import ")),
            "embedding args_proto must be import-resolved:\n{}",
            embedding.args_proto
        );
    }

    #[test]
    fn test_method_json_schema_map_has_three_methods() {
        let runner = LLMUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_json_schema_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
        assert!(schemas.contains_key("embedding"));
        assert_eq!(schemas.len(), 3);

        // Embedding schema must not be the silent-degradation sentinel.
        let embedding = schemas.get("embedding").unwrap();
        assert_ne!(embedding.args_schema, "{}");
    }

    #[test]
    fn test_settings_schema_includes_embedding_chunking_and_genai() {
        // The unified settings schema must be the runtime-generated (method A)
        // version, not the stale static JSON, so newly-added settings fields
        // are visible to clients reading runner metadata.
        let runner = LLMUnifiedRunnerSpecImpl::new();
        let schema = runner.settings_schema();
        assert!(
            schema.contains("embedding_chunking") || schema.contains("embeddingChunking"),
            "settings_schema must include embedding_chunking:\n{schema}"
        );
        assert!(
            schema.contains("genai") || schema.contains("Genai"),
            "settings_schema must include the genai variant:\n{schema}"
        );
    }
}
