use super::RunnerSpec;
use proto::jobworkerp::data::RunnerType;

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
    fn output_as_stream(&self) -> Option<bool> {
        Some(false)
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

    fn output_as_stream(&self) -> Option<bool> {
        SimpleWorkflowRunnerSpec::output_as_stream(self)
    }
}
