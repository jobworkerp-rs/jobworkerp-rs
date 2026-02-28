use crate::{jobworkerp::runner::create_workflow_args::WorkerOptions, schema_to_json_string};
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::RunnerType;
use std::collections::HashMap;

use super::RunnerSpec;

// Proto-generated types

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
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/create_workflow_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/create_workflow_result.proto"
                )
                .to_string(),
                description: Some("Create and register new workflow definition".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        // XXX WORKFLOW settings: WorkerOptions (not runner_settings though)
        schema_to_json_string!(WorkerOptions, "settings_schema")
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

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        CreateWorkflowRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        CreateWorkflowRunnerSpec::settings_schema(self)
    }
}
