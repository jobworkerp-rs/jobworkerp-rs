//! Unified Workflow Runner with multi-method support
//!
//! This module provides a unified Workflow runner that supports 'run' and 'create' methods
//! via the `using` parameter. This replaces the deprecated REUSABLE_WORKFLOW and CREATE_WORKFLOW runners.
//!
//! # Methods
//! - `run`: Execute workflow with pre-configured or runtime-specified definition (default if not specified)
//! - `create`: Create new workflow worker from definition
//!
//! # Settings
//! Uses `WorkflowRunnerSettings` with optional `workflow_source`:
//! - If workflow_source is specified in settings, 'run' method can use it without args override
//! - If workflow_source is not in settings, 'run' method args must include workflow_source
//! - 'create' method always uses workflow_source from args (ignores settings)
//!
//! # Usage
//! The `using` parameter defaults to "run" if not specified.

use super::create_workflow::CreateWorkflowRunnerSpecImpl;
use super::workflow::ReusableWorkflowRunnerSpecImpl;
use super::{CollectStreamFuture, RunnerSpec};
use crate::schema_to_json_string;
use anyhow::{Result, anyhow};
use futures::stream::BoxStream;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;

/// Method name for workflow execution
pub const METHOD_RUN: &str = "run";
/// Method name for workflow worker creation
pub const METHOD_CREATE: &str = "create";

/// Unified Workflow Runner specification implementation
///
/// This runner supports two methods:
/// - `run`: Execute workflow (uses ReusableWorkflowArgs/WorkflowResult)
/// - `create`: Create workflow worker (uses CreateWorkflowArgs/CreateWorkflowResult)
pub struct WorkflowUnifiedRunnerSpecImpl {
    run_spec: ReusableWorkflowRunnerSpecImpl,
    create_spec: CreateWorkflowRunnerSpecImpl,
}

impl WorkflowUnifiedRunnerSpecImpl {
    pub fn new() -> Self {
        Self {
            run_spec: ReusableWorkflowRunnerSpecImpl::new(),
            create_spec: CreateWorkflowRunnerSpecImpl::new(),
        }
    }

    /// Resolve the method name from `using` parameter
    ///
    /// Returns "run" as default if `using` is None
    pub fn resolve_method(using: Option<&str>) -> Result<&str> {
        match using {
            Some(METHOD_RUN) | None => Ok(METHOD_RUN), // Default to "run"
            Some(METHOD_CREATE) => Ok(METHOD_CREATE),
            Some(other) => Err(anyhow!(
                "Unknown method '{}' for WORKFLOW runner. Available methods: {}, {}",
                other,
                METHOD_RUN,
                METHOD_CREATE
            )),
        }
    }
}

impl Default for WorkflowUnifiedRunnerSpecImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnerSpec for WorkflowUnifiedRunnerSpecImpl {
    fn name(&self) -> String {
        RunnerType::Workflow.as_str_name().to_string()
    }

    fn runner_settings_proto(&self) -> String {
        // Use new WorkflowRunnerSettings with optional workflow_source
        // 'run' method can use workflow_source from settings or args
        // 'create' method ignores settings (uses args for workflow_source)
        include_str!("../../protobuf/jobworkerp/runner/workflow_runner_settings.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();

        // run method - use new WorkflowRunArgs
        schemas.insert(
            METHOD_RUN.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/workflow_run_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/workflow_result.proto"
                )
                .to_string(),
                description: Some(
                    "Execute workflow with pre-configured or runtime-specified definition"
                        .to_string(),
                ),
                output_type: StreamingOutputType::Both as i32,
                ..Default::default()
            },
        );

        // create method (from CreateWorkflow)
        schemas.insert(
            METHOD_CREATE.to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/create_workflow_args.proto"
                )
                .to_string(),
                result_proto: include_str!(
                    "../../protobuf/jobworkerp/runner/create_workflow_result.proto"
                )
                .to_string(),
                description: Some(
                    "Create WORKFLOW runner worker from workflow definition".to_string(),
                ),
                output_type: StreamingOutputType::NonStreaming as i32,
                ..Default::default()
            },
        );

        schemas
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        proto::jobworkerp::data::MethodJsonSchema::from_proto_map(self.method_proto_map())
    }

    fn settings_schema(&self) -> String {
        // Use new WorkflowRunnerSettings schema
        schema_to_json_string!(
            crate::jobworkerp::runner::WorkflowRunnerSettings,
            "settings_schema"
        )
    }

    /// Collect streaming output based on the method specified
    ///
    /// - run: Delegates to ReusableWorkflow's collect_stream
    /// - create: Non-streaming, uses default implementation
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> CollectStreamFuture {
        match Self::resolve_method(using) {
            Ok(METHOD_RUN) => self.run_spec.collect_stream(stream, using),
            Ok(METHOD_CREATE) => {
                // create is non-streaming, use default implementation (keep last)
                self.create_spec.collect_stream(stream, using)
            }
            Ok(_) => {
                // Should not reach here due to resolve_method validation
                Box::pin(
                    async move { Err(anyhow!("Internal error: unknown method after validation")) },
                )
            }
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_method_run() {
        let result = WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("run"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "run");
    }

    #[test]
    fn test_resolve_method_create() {
        let result = WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("create"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "create");
    }

    #[test]
    fn test_resolve_method_none_defaults_to_run() {
        let result = WorkflowUnifiedRunnerSpecImpl::resolve_method(None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "run");
    }

    #[test]
    fn test_resolve_method_unknown() {
        let result = WorkflowUnifiedRunnerSpecImpl::resolve_method(Some("unknown"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Unknown method 'unknown'")
        );
    }

    #[test]
    fn test_runner_name() {
        let runner = WorkflowUnifiedRunnerSpecImpl::new();
        assert_eq!(runner.name(), "WORKFLOW");
    }

    #[test]
    fn test_method_proto_map_has_both_methods() {
        let runner = WorkflowUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_proto_map();

        assert!(schemas.contains_key("run"));
        assert!(schemas.contains_key("create"));
        assert_eq!(schemas.len(), 2);

        // Verify run method
        let run = schemas.get("run").unwrap();
        assert!(
            run.description
                .as_ref()
                .unwrap()
                .contains("Execute workflow")
        );
        assert!(!run.args_proto.is_empty());
        assert!(!run.result_proto.is_empty());
        assert_eq!(run.output_type, StreamingOutputType::Both as i32);

        // Verify create method
        let create = schemas.get("create").unwrap();
        assert!(create.description.as_ref().unwrap().contains("Create"));
        assert!(!create.args_proto.is_empty());
        assert!(!create.result_proto.is_empty());
        assert_eq!(create.output_type, StreamingOutputType::NonStreaming as i32);
    }

    #[test]
    fn test_method_json_schema_map_has_both_methods() {
        let runner = WorkflowUnifiedRunnerSpecImpl::new();
        let schemas = runner.method_json_schema_map();

        assert!(schemas.contains_key("run"));
        assert!(schemas.contains_key("create"));
        assert_eq!(schemas.len(), 2);
    }
}
