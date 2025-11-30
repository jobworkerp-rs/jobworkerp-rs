use crate::{jobworkerp::runner::llm::LlmRunnerSettings, schema_to_json_string};
use proto::DEFAULT_METHOD_NAME;

use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;
use std::collections::HashMap;

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
    // Phase 6.6: Unified method_proto_map for all runners
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
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
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );
        schemas
    }
    fn settings_schema(&self) -> String {
        // include_str!("../../schema/llm/LLMRunnerSettings.json").to_string()
        schema_to_json_string!(LlmRunnerSettings, "settings_schema")
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

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMChatRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
    }
}
