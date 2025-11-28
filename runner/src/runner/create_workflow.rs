use crate::{
    jobworkerp::runner::create_workflow_args::WorkerOptions, schema_to_json_string,
    schema_to_json_string_option,
};
use proto::jobworkerp::data::RunnerType;

use super::RunnerSpec;

// Proto-generated types
use crate::jobworkerp::runner::CreateWorkflowResult;

pub struct CreateWorkflowRunnerSpecImpl {}

impl CreateWorkflowRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for CreateWorkflowRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

pub trait CreateWorkflowRunnerSpec {
    fn name(&self) -> String {
        RunnerType::CreateWorkflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        "".to_string()
    }

    fn job_args_proto(&self) -> Option<String> {
        Some(
            include_str!("../../protobuf/jobworkerp/runner/create_workflow_args.proto").to_string(),
        )
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(
            include_str!("../../protobuf/jobworkerp/runner/create_workflow_result.proto")
                .to_string(),
        )
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::NonStreaming
    }

    fn settings_schema(&self) -> String {
        // XXX WORKFLOW settings: WorkerOptions (not runner_settings though)
        schema_to_json_string!(WorkerOptions, "settings_schema")
    }

    fn arguments_schema(&self) -> String {
        // XXX WORKFLOW JSON schema
        include_str!("../../schema/workflow.json").to_string()
    }

    fn output_schema(&self) -> Option<String> {
        schema_to_json_string_option!(CreateWorkflowResult, "output_schema")
    }
}

impl CreateWorkflowRunnerSpec for CreateWorkflowRunnerSpecImpl {}

impl RunnerSpec for CreateWorkflowRunnerSpecImpl {
    fn name(&self) -> String {
        CreateWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        CreateWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> Option<String> {
        CreateWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        CreateWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        CreateWorkflowRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        CreateWorkflowRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        CreateWorkflowRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        CreateWorkflowRunnerSpec::output_schema(self)
    }
}
