use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;

pub struct LLMRunnerSpecImpl {}

impl LLMRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMRunnerSpec {
    fn name(&self) -> String {
        RunnerType::HttpRequest.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/llm_result.proto").to_string())
    }
    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::Both
    }
    fn settings_schema(&self) -> String {
        include_str!("../../schema/LLMRunnerSettings.json").to_string()
    }
    fn arguments_schema(&self) -> String {
        include_str!("../../schema/LLMArgs.json").to_string()
    }
    fn output_schema(&self) -> Option<String> {
        Some(include_str!("../../schema/LLMResult.json").to_string())
    }
}

impl LLMRunnerSpec for LLMRunnerSpecImpl {}

impl RunnerSpec for LLMRunnerSpecImpl {
    fn name(&self) -> String {
        LLMRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        LLMRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        LLMRunnerSpec::settings_schema(self)
    }
    fn arguments_schema(&self) -> String {
        LLMRunnerSpec::arguments_schema(self)
    }
    fn output_schema(&self) -> Option<String> {
        LLMRunnerSpec::output_schema(self)
    }
}
