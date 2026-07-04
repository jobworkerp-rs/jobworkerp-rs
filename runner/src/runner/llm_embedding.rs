//! LLM embedding runner spec (helper for the unified LLM runner).
//!
//! This spec backs the `embedding` method of `RunnerType::Llm`. It is not
//! registered as an independent runner (see `factory.rs`); `llm_unified.rs`
//! delegates the `embedding` method here.
//!
//! # Proto import resolution
//! `embedding_args.proto` is the first LLM proto that `import`s another proto
//! (`runner.proto`, for `LLMRunnerSettings.ChunkingConfig`). Both
//! `proto::from_proto_map` (JSON schema generation) and
//! `parse_job_args_schema_descriptor` (runtime args decode) compile the raw
//! `args_proto` string in an isolated tempdir, so an unresolved `import` line
//! would silently degrade the schema to `"{}"` or hard-error at job execution.
//! To avoid that, `method_proto_map()` stores the import-inlined, self-contained
//! proto produced by `command_utils::protobuf::resolve::resolve_proto_imports`.

use super::RunnerSpec;
use crate::{jobworkerp::runner::llm::LlmRunnerSettings, schema_to_json_string};
use command_utils::protobuf::resolve::resolve_proto_imports;
use proto::DEFAULT_METHOD_NAME;
use std::collections::HashMap;

const RUNNER_PROTO: &str = include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto");
const EMBEDDING_ARGS_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/llm/embedding_args.proto");
const EMBEDDING_RESULT_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/llm/embedding_result.proto");

/// Import path referenced by `embedding_args.proto` (must match its `import`
/// line exactly for `resolve_proto_imports` to strip it).
const RUNNER_PROTO_IMPORT_PATH: &str = "jobworkerp/runner/llm/runner.proto";

/// Import-resolved (self-contained) `embedding_args.proto`. Inputs are
/// compile-time constants, so the resolution (regex compilation + string
/// rewriting) is done once at first access and cached, rather than on every
/// `method_proto_map()` call.
static RESOLVED_EMBEDDING_ARGS_PROTO: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| {
        resolve_proto_imports(
            EMBEDDING_ARGS_PROTO,
            &[(RUNNER_PROTO_IMPORT_PATH, RUNNER_PROTO)],
        )
        .unwrap_or_else(|e| {
            // Should never happen: the only import is runner.proto, provided above.
            panic!("failed to resolve embedding_args.proto imports: {e}")
        })
    });

pub struct LLMEmbeddingRunnerSpecImpl {}

impl LLMEmbeddingRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMEmbeddingRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMEmbeddingRunnerSpec {
    fn name(&self) -> String {
        "LLM_EMBEDDING".to_string()
    }
    fn runner_settings_proto(&self) -> String {
        RUNNER_PROTO.to_string()
    }
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                // Self-contained proto: import lines are inlined so descriptor
                // compilation in an isolated tempdir succeeds (cached; see
                // RESOLVED_EMBEDDING_ARGS_PROTO).
                args_proto: RESOLVED_EMBEDDING_ARGS_PROTO.clone(),
                result_proto: EMBEDDING_RESULT_PROTO.to_string(),
                description: Some("Generate embeddings using LLM (with chunking)".to_string()),
                // Embedding produces a single batched result, not a stream.
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );
        schemas
    }
    fn settings_schema(&self) -> String {
        schema_to_json_string!(LlmRunnerSettings, "settings_schema")
    }
}

impl LLMEmbeddingRunnerSpec for LLMEmbeddingRunnerSpecImpl {}

impl RunnerSpec for LLMEmbeddingRunnerSpecImpl {
    fn name(&self) -> String {
        LLMEmbeddingRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMEmbeddingRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMEmbeddingRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMEmbeddingRunnerSpec::settings_schema(self)
    }

    // method_json_schema_map() uses the RunnerSpec default (from_proto_map),
    // which now works because args_proto is import-resolved.

    // Embedding is non-streaming; collect_stream uses the default no-op impl.
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_utils::protobuf::ProtobufDescriptor;

    #[test]
    fn test_name_is_embedding_helper() {
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        assert_eq!(RunnerSpec::name(&spec), "LLM_EMBEDDING");
    }

    #[test]
    fn test_method_proto_map_args_proto_is_self_contained() {
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schemas = RunnerSpec::method_proto_map(&spec);
        let method = schemas
            .get(DEFAULT_METHOD_NAME)
            .expect("default method must exist");

        // The stored args_proto must NOT contain import lines (they would break
        // isolated-tempdir descriptor compilation). A weak "non-empty" check
        // would pass even when from_proto_map silently degrades to "{}", so we
        // assert the actual self-containment contract instead.
        assert!(
            !method
                .args_proto
                .lines()
                .any(|l| l.trim().starts_with("import ")),
            "args_proto must be import-resolved (self-contained):\n{}",
            method.args_proto
        );
    }

    #[test]
    fn test_args_proto_compiles_with_embedding_args_first() {
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schemas = RunnerSpec::method_proto_map(&spec);
        let args_proto = &schemas.get(DEFAULT_METHOD_NAME).unwrap().args_proto;

        // Must compile in isolation (proves imports are resolved), and the
        // first (primary) message must be LLMEmbeddingArgs.
        let descriptor =
            ProtobufDescriptor::new(args_proto).expect("args_proto must compile in isolation");
        let first = descriptor
            .get_messages()
            .into_iter()
            .next()
            .expect("at least one message");
        assert_eq!(
            first.name(),
            "LLMEmbeddingArgs",
            "first (primary) message must be LLMEmbeddingArgs"
        );
    }

    #[test]
    fn test_method_json_schema_map_args_schema_not_empty() {
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schemas = RunnerSpec::method_json_schema_map(&spec);
        let method = schemas
            .get(DEFAULT_METHOD_NAME)
            .expect("default method must exist");

        // "{}" is the silent-degradation sentinel from from_proto_map when the
        // descriptor fails to build. Assert it is a real schema containing the
        // embedding-specific fields (inputs, chunking).
        assert_ne!(method.args_schema, "{}", "args_schema must not be degraded");
        assert!(
            method.args_schema.contains("inputs"),
            "args_schema must describe inputs field:\n{}",
            method.args_schema
        );
        assert!(
            method.args_schema.contains("chunking"),
            "args_schema must describe chunking field:\n{}",
            method.args_schema
        );
        // Embedding options passthrough (genai): encoding_format / user must
        // surface so clients can discover them.
        assert!(
            method.args_schema.contains("encoding_format")
                || method.args_schema.contains("encodingFormat"),
            "args_schema must describe encoding_format option:\n{}",
            method.args_schema
        );
        assert!(
            method.args_schema.contains("user"),
            "args_schema must describe user option:\n{}",
            method.args_schema
        );
    }

    #[test]
    fn test_settings_schema_contains_embedding_chunking_and_genai() {
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schema = RunnerSpec::settings_schema(&spec);
        assert!(
            schema.contains("embedding_chunking") || schema.contains("embeddingChunking"),
            "settings_schema must include the embedding_chunking field:\n{schema}"
        );
        assert!(
            schema.contains("genai") || schema.contains("Genai"),
            "settings_schema must include the genai variant:\n{schema}"
        );
    }

    #[test]
    fn test_settings_schema_contains_hf_tokenizer_source_fields() {
        // Phase 3: the HF tokenizer source fields (repo / file path) added to
        // ChunkingConfig must surface in the generated settings schema so a
        // client configuring HF_TOKENIZER estimation can discover them.
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schema = RunnerSpec::settings_schema(&spec);
        assert!(
            schema.contains("tokenizer_hf_repo") || schema.contains("tokenizerHfRepo"),
            "settings_schema must include tokenizer_hf_repo:\n{schema}"
        );
        assert!(
            schema.contains("tokenizer_file_path") || schema.contains("tokenizerFilePath"),
            "settings_schema must include tokenizer_file_path:\n{schema}"
        );
    }

    #[test]
    fn test_settings_schema_contains_tiktoken_encoding_field() {
        // Phase 2: the tiktoken encoding selector added to ChunkingConfig must
        // surface in the generated settings schema so a client configuring
        // TIKTOKEN estimation can discover it.
        let spec = LLMEmbeddingRunnerSpecImpl::new();
        let schema = RunnerSpec::settings_schema(&spec);
        assert!(
            schema.contains("tiktoken_encoding") || schema.contains("tiktokenEncoding"),
            "settings_schema must include tiktoken_encoding:\n{schema}"
        );
    }
}
