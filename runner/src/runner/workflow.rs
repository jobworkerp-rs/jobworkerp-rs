use crate::jobworkerp::runner::{
    Empty, ReusableWorkflowArgs, ReusableWorkflowRunnerSettings, WorkflowResult,
};
use crate::{schema_to_json_string, schema_to_json_string_option};

use super::RunnerSpec;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType};

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
pub trait InlineWorkflowRunnerSpec: RunnerSpec {
    fn name(&self) -> String {
        RunnerType::InlineWorkflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }

    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/workflow_args.proto").to_string()
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/workflow_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::Both
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

    fn job_args_proto(&self) -> String {
        InlineWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        InlineWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> StreamingOutputType {
        InlineWorkflowRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(Empty, "settings_schema")
    }

    // TODO add schema for workflow yaml as json schema
    fn arguments_schema(&self) -> String {
        // XXX for right oneof structure in json schema
        include_str!("../../schema/WorkflowArgs.json").to_string()
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(WorkflowResult, "output_schema")
    }
}

/////////////////////////////////////////////////////////////////////
// SavedWorkflowRunnerSpec
///////////////////////////////////////////////////////////////////////

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
pub trait ReusableWorkflowRunnerSpec: RunnerSpec {
    fn name(&self) -> String {
        RunnerType::ReusableWorkflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/reusable_workflow_runner.proto").to_string()
    }

    fn job_args_proto(&self) -> String {
        include_str!("../../protobuf/jobworkerp/runner/reusable_workflow_args.proto").to_string()
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../../protobuf/jobworkerp/runner/workflow_result.proto").to_string())
    }
    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::Both
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

    fn job_args_proto(&self) -> String {
        ReusableWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        ReusableWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> StreamingOutputType {
        ReusableWorkflowRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(ReusableWorkflowRunnerSettings, "settings_schema")
    }

    // TODO add schema for workflow yaml as json schema
    fn arguments_schema(&self) -> String {
        schema_to_json_string!(ReusableWorkflowArgs, "arguments_schema")
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(WorkflowResult, "output_schema")
    }
}
