use crate::{
    jobworkerp::runner::llm::{LlmChatArgs, LlmChatResult, LlmRunnerSettings},
    schema_to_json_string, schema_to_json_string_option,
};

use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;

pub struct LLMChatRunnerSpecImpl {}

impl LLMChatRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LLMChatRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait LLMChatRunnerSpec {
    fn name(&self) -> String {
        RunnerType::LlmChat.as_str_name().to_string()
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/llm/runner.proto").to_string()
    }
    fn job_args_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/llm/chat_args.proto").to_string())
    }
    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/llm/chat_result.proto").to_string())
    }
    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::Both
    }
    fn settings_schema(&self) -> String {
        // include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
        schema_to_json_string!(LlmRunnerSettings, "settings_schema")
    }
    fn arguments_schema(&self) -> String {
        // include_str!("../../schema/llm/LLMCompletionArgs.json").to_string()
        schema_to_json_string!(LlmChatArgs, "arguments_schema")
    }
    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(LlmChatResult, "output_schema")
    }
}

impl LLMChatRunnerSpec for LLMChatRunnerSpecImpl {}

impl RunnerSpec for LLMChatRunnerSpecImpl {
    fn name(&self) -> String {
        LLMChatRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMChatRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> Option<String> {
        LLMChatRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMChatRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMChatRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
    }
    fn arguments_schema(&self) -> String {
        LLMChatRunnerSpec::arguments_schema(self)
    }
    fn output_schema(&self) -> Option<String> {
        LLMChatRunnerSpec::output_schema(self)
    }
}
