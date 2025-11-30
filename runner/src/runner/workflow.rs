use crate::jobworkerp::runner::{
    Empty, ReusableWorkflowArgs, ReusableWorkflowRunnerSettings, WorkflowResult,
};
use crate::{schema_to_json_string, schema_to_json_string_option};
use proto::DEFAULT_METHOD_NAME;

use super::RunnerSpec;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType};
use std::collections::HashMap;

pub trait InlineWorkflowRunnerSpec: RunnerSpec {
    fn name(&self) -> String {
        RunnerType::InlineWorkflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../protobuf/jobworkerp/runner/workflow_args.proto")
                    .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/workflow_result.proto"
                )
                .to_string(),
                description: Some("Execute inline workflow (multi-job orchestration)".to_string()),
                output_type: StreamingOutputType::Both as i32,
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(Empty, "settings_schema")
    }

    fn arguments_schema(&self) -> String {
        include_str!("../../schema/WorkflowArgs.json").to_string()
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(WorkflowResult, "output_schema")
    }
}

pub struct InlineWorkflowRunnerSpecImpl {}

impl InlineWorkflowRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for InlineWorkflowRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineWorkflowRunnerSpec for InlineWorkflowRunnerSpecImpl {}

impl RunnerSpec for InlineWorkflowRunnerSpecImpl {
    fn name(&self) -> String {
        InlineWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        InlineWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        InlineWorkflowRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        InlineWorkflowRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        InlineWorkflowRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        InlineWorkflowRunnerSpec::output_schema(self)
    }
}

/////////////////////////////////////////////////////////////////////
// ReusableWorkflowRunnerSpec
///////////////////////////////////////////////////////////////////////

pub trait ReusableWorkflowRunnerSpec: RunnerSpec {
    fn name(&self) -> String {
        RunnerType::ReusableWorkflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/reusable_workflow_runner.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/reusable_workflow_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/workflow_result.proto"
                )
                .to_string(),
                description: Some("Execute reusable workflow from external source".to_string()),
                output_type: StreamingOutputType::Both as i32,
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(ReusableWorkflowRunnerSettings, "settings_schema")
    }

    fn arguments_schema(&self) -> String {
        schema_to_json_string!(ReusableWorkflowArgs, "arguments_schema")
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(WorkflowResult, "output_schema")
    }
}

pub struct ReusableWorkflowRunnerSpecImpl {}

impl ReusableWorkflowRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ReusableWorkflowRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl ReusableWorkflowRunnerSpec for ReusableWorkflowRunnerSpecImpl {}

impl RunnerSpec for ReusableWorkflowRunnerSpecImpl {
    fn name(&self) -> String {
        ReusableWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        ReusableWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        ReusableWorkflowRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        ReusableWorkflowRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        ReusableWorkflowRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        ReusableWorkflowRunnerSpec::output_schema(self)
    }
}
