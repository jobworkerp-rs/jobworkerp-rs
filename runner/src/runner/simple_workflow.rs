use crate::jobworkerp::runner::{Empty, WorkflowArgs, WorkflowResult};

use super::RunnerSpec;
use proto::jobworkerp::data::{RunnerType, StreamingOutputType};

pub struct SimpleWorkflowRunnerSpecImpl {}
impl SimpleWorkflowRunnerSpecImpl {
    pub fn new() -> Self {
        Self {}
    }
}
impl Default for SimpleWorkflowRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}
pub trait SimpleWorkflowRunnerSpec: RunnerSpec {
    fn name(&self) -> String {
        RunnerType::SimpleWorkflow.as_str_name().to_string()
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
        StreamingOutputType::NonStreaming
    }
}
impl SimpleWorkflowRunnerSpec for SimpleWorkflowRunnerSpecImpl {}

impl RunnerSpec for SimpleWorkflowRunnerSpecImpl {
    fn name(&self) -> String {
        SimpleWorkflowRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        SimpleWorkflowRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        SimpleWorkflowRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        SimpleWorkflowRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> StreamingOutputType {
        SimpleWorkflowRunnerSpec::output_type(self)
    }
    fn settings_schema(&self) -> String {
        // plain string with title
        let schema = schemars::schema_for!(Empty);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    // TODO add schema for workflow yaml as json schema
    fn arguments_schema(&self) -> String {
        // plain string with title
        let schema = schemars::schema_for!(WorkflowArgs);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn output_schema(&self) -> Option<String> {
        // plain string with title
        let schema = schemars::schema_for!(WorkflowResult);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_json_schema: {:?}", e);
                None
            }
        }
    }
}
