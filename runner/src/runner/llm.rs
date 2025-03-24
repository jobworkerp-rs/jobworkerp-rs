use super::RunnerSpec;
use crate::jobworkerp::runner::{LlmArgs, LlmResult, LlmRunnerSettings};
use proto::jobworkerp::data::RunnerType;
use schemars::JsonSchema;

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
    fn output_as_stream(&self) -> Option<bool> {
        Some(true)
    }
}

impl LLMRunnerSpec for LLMRunnerSpecImpl {}

#[derive(Debug, JsonSchema, serde::Deserialize, serde::Serialize)]
struct LLMRunnerInputSchema {
    settings: LlmRunnerSettings,
    args: LlmArgs,
}

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

    fn output_as_stream(&self) -> Option<bool> {
        LLMRunnerSpec::output_as_stream(self)
    }
    fn input_json_schema(&self) -> String {
        let schema = schemars::schema_for!(LLMRunnerInputSchema);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn output_json_schema(&self) -> Option<String> {
        // plain string with title
        let schema = schemars::schema_for!(LlmResult);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_json_schema: {:?}", e);
                None
            }
        }
    }
}
