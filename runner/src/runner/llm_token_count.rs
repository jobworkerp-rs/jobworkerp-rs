//! Token-count runner specification used by the unified LLM runner.
//!
//! `TokenCountArgs` imports completion and chat protos; method schemas are compiled in
//! isolation, so the import must be resolved before the schema is published.

use super::RunnerSpec;
use crate::jobworkerp::runner::llm::LlmRunnerSettings;
use crate::schema_to_json_string;
use command_utils::protobuf::resolve::resolve_proto_imports;
use std::collections::HashMap;

const CHAT_ARGS_PROTO: &str = include_str!("../../protobuf/jobworkerp/runner/llm/chat_args.proto");
const COMPLETION_ARGS_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/llm/completion_args.proto");
const TOKEN_COUNT_ARGS_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/llm/token_count_args.proto");
const TOKEN_COUNT_RESULT_PROTO: &str =
    include_str!("../../protobuf/jobworkerp/runner/llm/token_count_result.proto");
const CHAT_ARGS_IMPORT_PATH: &str = "jobworkerp/runner/llm/chat_args.proto";
const COMPLETION_ARGS_IMPORT_PATH: &str = "jobworkerp/runner/llm/completion_args.proto";

static RESOLVED_TOKEN_COUNT_ARGS_PROTO: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| {
        resolve_proto_imports(
            TOKEN_COUNT_ARGS_PROTO,
            &[
                (COMPLETION_ARGS_IMPORT_PATH, COMPLETION_ARGS_PROTO),
                (CHAT_ARGS_IMPORT_PATH, CHAT_ARGS_PROTO),
            ],
        )
        .unwrap_or_else(|e| panic!("failed to resolve token-count proto imports: {e}"))
    });

pub struct LLMTokenCountRunnerSpecImpl;

impl LLMTokenCountRunnerSpecImpl {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LLMTokenCountRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMTokenCountRunnerSpec {
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        HashMap::from([(
            proto::DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: RESOLVED_TOKEN_COUNT_ARGS_PROTO.clone(),
                result_proto: TOKEN_COUNT_RESULT_PROTO.to_string(),
                description: Some("Count exact input tokens for an LLM request".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        )])
    }
}

impl LLMTokenCountRunnerSpec for LLMTokenCountRunnerSpecImpl {}

impl RunnerSpec for LLMTokenCountRunnerSpecImpl {
    fn name(&self) -> String {
        "LLM_TOKEN_COUNT".to_string()
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMTokenCountRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(LlmRunnerSettings, "settings_schema")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use command_utils::protobuf::ProtobufDescriptor;

    #[test]
    fn args_proto_is_self_contained_and_compiles() {
        let schema = RunnerSpec::method_proto_map(&LLMTokenCountRunnerSpecImpl::new())
            .remove(proto::DEFAULT_METHOD_NAME)
            .unwrap();
        assert!(
            !schema
                .args_proto
                .lines()
                .any(|line| line.trim().starts_with("import "))
        );
        let descriptor = ProtobufDescriptor::new(&schema.args_proto).unwrap();
        assert_eq!(descriptor.get_messages()[0].name(), "TokenCountArgs");
    }

    #[test]
    fn args_proto_keeps_the_public_target_tags() {
        let schema = RunnerSpec::method_proto_map(&LLMTokenCountRunnerSpecImpl::new())
            .remove(proto::DEFAULT_METHOD_NAME)
            .unwrap();
        assert!(schema.args_proto.contains("TextCountRequest text = 1"));
        assert!(
            schema
                .args_proto
                .contains("LLMCompletionArgs completion = 2")
        );
        assert!(schema.args_proto.contains("ChatCountRequest chat = 3"));
    }
}
