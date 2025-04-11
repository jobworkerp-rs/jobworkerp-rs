use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;

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
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/completion_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(
            include_str!("../../protobuf/jobworkerp/runner/llm/completion_result.proto")
                .to_string(),
        )
    }
    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::Both
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

    fn job_args_proto(&self) -> String {
        LLMCompletionRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMCompletionRunnerSpec::result_output_proto(self)
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
