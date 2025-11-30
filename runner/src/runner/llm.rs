use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;
use proto::DEFAULT_METHOD_NAME;
use std::collections::HashMap;

pub struct LLMCompletionRunnerSpecImpl {}

impl LLMCompletionRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMCompletionRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMCompletionRunnerSpec {
    fn name(&self) -> String {
        RunnerType::LlmCompletion.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
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
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );
        schemas
    }

    // Phase 6.7: Override method_json_schema_map() to use hand-crafted JSON Schema
    // Reason: Protobuf oneof fields in GenerationContext require oneOf constraints
    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            super::MethodJsonSchema {
                args_schema: include_str!("../../schema/llm/LLMCompletionArgs.json").to_string(),
                result_schema: Some(
                    include_str!("../../schema/llm/LLMCompletionResult.json").to_string(),
                ),
                description: None,
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
    }
}

impl LLMCompletionRunnerSpec for LLMCompletionRunnerSpecImpl {}

impl RunnerSpec for LLMCompletionRunnerSpecImpl {
    fn name(&self) -> String {
        LLMCompletionRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMCompletionRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMCompletionRunnerSpec::method_proto_map(self)
    }

    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        LLMCompletionRunnerSpec::method_json_schema_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }
}
