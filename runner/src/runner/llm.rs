use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;
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
            "run".to_string(),
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
    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        // Phase 6.6.5: Use method_proto_map's output_type instead of deprecated RunnerData.output_type
        self.method_proto_map()
            .get("run")
            .cloned()
            .and_then(|s| {
                proto::jobworkerp::data::StreamingOutputType::try_from(s.output_type).ok()
            })
            .unwrap_or(proto::jobworkerp::data::StreamingOutputType::NonStreaming)
    }
    fn settings_schema(&self) -> String {
        include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
    }
    fn arguments_schema(&self) -> String {
        include_str!("../../schema/llm/LLMCompletionArgs.json").to_string()
    }
    fn output_schema(&self) -> Option<String> {
        Some(include_str!("../../schema/llm/LLMCompletionResult.json").to_string())
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

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMCompletionRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }
    fn arguments_schema(&self) -> String {
        LLMCompletionRunnerSpec::arguments_schema(self)
    }
    fn output_schema(&self) -> Option<String> {
        LLMCompletionRunnerSpec::output_schema(self)
    }
}
