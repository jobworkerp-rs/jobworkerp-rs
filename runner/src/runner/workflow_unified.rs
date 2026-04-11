//! Unified Workflow Runner with multi-method support
//!
//! This module provides a unified Workflow runner that supports 'run' and 'create' methods
//! via the `using` parameter.
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

use super::{CollectStreamFuture, RunnerSpec};
use crate::jobworkerp::runner::WorkflowResult;
use crate::jobworkerp::runner::workflow_result::WorkflowStatus;
use crate::schema_to_json_string;
use anyhow::{Result, anyhow};
use futures::StreamExt;
use futures::stream::BoxStream;
use prost::Message;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;

impl crate::jobworkerp::runner::WorkflowRunnerSettings {
    /// Parse workflow definition from workflow_data to extract schema (document/input).
    pub fn schema(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        use crate::jobworkerp::runner::workflow_runner_settings::WorkflowSource;
        match &self.workflow_source {
            Some(WorkflowSource::WorkflowData(data)) => {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(data).ok()
            }
            _ => None,
        }
    }
}

/// Method name for workflow execution
pub const METHOD_RUN: &str = "run";
/// Method name for workflow worker creation
pub const METHOD_CREATE: &str = "create";

/// Unified Workflow Runner specification implementation
///
/// This runner supports two methods:
/// - `run`: Execute workflow (uses WorkflowRunArgs/WorkflowResult)
/// - `create`: Create workflow worker (uses CreateWorkflowArgs/CreateWorkflowResult)
pub struct WorkflowUnifiedRunnerSpecImpl;

impl WorkflowUnifiedRunnerSpecImpl {
    pub fn new() -> Self {
        Self
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
        include_str!("../../protobuf/jobworkerp/runner/workflow_runner_settings.proto").to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();

        // run method
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

        // create method
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
        schema_to_json_string!(
            crate::jobworkerp::runner::WorkflowRunnerSettings,
            "settings_schema"
        )
    }

    /// Collect streaming output based on the method specified
    ///
    /// - run: Collects WorkflowResult items, returns the final one
    /// - create: Non-streaming, uses default collect behavior
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> CollectStreamFuture {
        match Self::resolve_method(using) {
            Ok(METHOD_RUN) => collect_workflow_stream(stream),
            Ok(METHOD_CREATE) => {
                // create is non-streaming, use default collect (keep last data item)
                collect_keep_last(stream)
            }
            Ok(_) => {
                Box::pin(
                    async move { Err(anyhow!("Internal error: unknown method after validation")) },
                )
            }
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }
}

/// Default collect: keep last data item (for non-streaming methods like create).
fn collect_keep_last(stream: BoxStream<'static, ResultOutputItem>) -> CollectStreamFuture {
    use proto::jobworkerp::data::result_output_item;

    Box::pin(async move {
        let mut last_data: Option<Vec<u8>> = None;
        let mut metadata = HashMap::new();
        let mut stream = stream;

        while let Some(item) = stream.next().await {
            match item.item {
                Some(result_output_item::Item::Data(data)) => {
                    last_data = Some(data);
                }
                Some(result_output_item::Item::End(trailer)) => {
                    metadata = trailer.metadata;
                    break;
                }
                Some(result_output_item::Item::FinalCollected(data)) => {
                    return Ok((data, metadata));
                }
                None => {}
            }
        }

        Ok((last_data.unwrap_or_default(), metadata))
    })
}

/// Collect workflow stream: decode WorkflowResult items and return the final result.
fn collect_workflow_stream(stream: BoxStream<'static, ResultOutputItem>) -> CollectStreamFuture {
    use proto::jobworkerp::data::result_output_item;

    Box::pin(async move {
        let mut final_result: Option<WorkflowResult> = None;
        let mut metadata = HashMap::new();
        let mut stream = stream;
        let mut final_collected: Option<Vec<u8>> = None;
        let mut decode_failure_count = 0;
        let mut data_item_count = 0;

        while let Some(item) = stream.next().await {
            match item.item {
                Some(result_output_item::Item::Data(data)) => {
                    data_item_count += 1;
                    match WorkflowResult::decode(data.as_slice()) {
                        Ok(workflow_result) => {
                            final_result = Some(workflow_result);
                        }
                        Err(e) => {
                            decode_failure_count += 1;
                            tracing::warn!(
                                "Failed to decode WorkflowResult in collect_stream ({}/{}): {:?}",
                                decode_failure_count,
                                data_item_count,
                                e
                            );
                        }
                    }
                }
                Some(result_output_item::Item::End(trailer)) => {
                    metadata = trailer.metadata;
                    break;
                }
                Some(result_output_item::Item::FinalCollected(data)) => {
                    final_collected = Some(data);
                }
                None => {}
            }
        }

        if let Some(ref data) = final_collected {
            if let Ok(result) = WorkflowResult::decode(data.as_slice())
                && result.status == WorkflowStatus::Faulted as i32
            {
                return Err(anyhow::anyhow!(
                    "Workflow execution failed: {}",
                    result.error_message.as_deref().unwrap_or("Unknown error")
                ));
            }
            return Ok((data.clone(), metadata));
        }

        if data_item_count > 0 && final_result.is_none() {
            return Err(anyhow::anyhow!(
                "All {} WorkflowResult decode attempts failed in collect_stream",
                decode_failure_count
            ));
        }

        if let Some(ref result) = final_result
            && result.status == WorkflowStatus::Faulted as i32
        {
            return Err(anyhow::anyhow!(
                "Workflow execution failed: {}",
                result.error_message.as_deref().unwrap_or("Unknown error")
            ));
        }

        let bytes = final_result.map(|r| r.encode_to_vec()).unwrap_or_default();
        Ok((bytes, metadata))
    })
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

    // --- collect_workflow_stream tests (migrated from deprecated workflow.rs) ---

    fn create_workflow_result(id: &str, output: &str, status: WorkflowStatus) -> WorkflowResult {
        WorkflowResult {
            id: id.to_string(),
            output: output.to_string(),
            position: "/test".to_string(),
            status: status as i32,
            error_message: None,
        }
    }

    fn create_data_item(result: &WorkflowResult) -> ResultOutputItem {
        use proto::jobworkerp::data::result_output_item;
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(result.encode_to_vec())),
        }
    }

    fn create_end_item(metadata: HashMap<String, String>) -> ResultOutputItem {
        use proto::jobworkerp::data::{Trailer, result_output_item};
        ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer { metadata })),
        }
    }

    fn create_final_collected_item(data: Vec<u8>) -> ResultOutputItem {
        use proto::jobworkerp::data::result_output_item;
        ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(data)),
        }
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_single_result() {
        let result = create_workflow_result(
            "wf-1",
            r#"{"result": "success"}"#,
            WorkflowStatus::Completed,
        );

        let mut metadata = HashMap::new();
        metadata.insert("trace_id".to_string(), "abc123".to_string());

        let items = vec![create_data_item(&result), create_end_item(metadata.clone())];
        let stream = futures::stream::iter(items).boxed();

        let (bytes, returned_metadata) = collect_workflow_stream(stream).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.id, "wf-1");
        assert_eq!(decoded.output, r#"{"result": "success"}"#);
        assert_eq!(decoded.status, WorkflowStatus::Completed as i32);
        assert_eq!(
            returned_metadata.get("trace_id"),
            Some(&"abc123".to_string())
        );
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_multiple_results_keeps_last() {
        let result1 = create_workflow_result("wf-1", r#"{"step": 1}"#, WorkflowStatus::Running);
        let result2 = create_workflow_result("wf-1", r#"{"step": 2}"#, WorkflowStatus::Running);
        let result3 = create_workflow_result(
            "wf-1",
            r#"{"step": 3, "final": true}"#,
            WorkflowStatus::Completed,
        );

        let items = vec![
            create_data_item(&result1),
            create_data_item(&result2),
            create_data_item(&result3),
            create_end_item(HashMap::new()),
        ];
        let stream = futures::stream::iter(items).boxed();

        let (bytes, _) = collect_workflow_stream(stream).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.output, r#"{"step": 3, "final": true}"#);
        assert_eq!(decoded.status, WorkflowStatus::Completed as i32);
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_final_collected_takes_precedence() {
        let result =
            create_workflow_result("wf-1", r#"{"intermediate": true}"#, WorkflowStatus::Running);
        let final_result =
            create_workflow_result("wf-1", r#"{"final": true}"#, WorkflowStatus::Completed);

        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), "value".to_string());

        let items = vec![
            create_data_item(&result),
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(metadata.clone()),
        ];
        let stream = futures::stream::iter(items).boxed();

        let (bytes, returned_metadata) = collect_workflow_stream(stream).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.output, r#"{"final": true}"#);
        assert_eq!(decoded.status, WorkflowStatus::Completed as i32);
        assert_eq!(returned_metadata.get("key"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_empty_returns_empty() {
        let items = vec![create_end_item(HashMap::new())];
        let stream = futures::stream::iter(items).boxed();

        let (bytes, _) = collect_workflow_stream(stream).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_faulted_status() {
        let mut result = create_workflow_result(
            "wf-err",
            r#"{"error": "something failed"}"#,
            WorkflowStatus::Faulted,
        );
        result.error_message = Some("Task failed".to_string());

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = futures::stream::iter(items).boxed();

        let result = collect_workflow_stream(stream).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Task failed"),
            "Error message should contain 'Task failed': {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_collect_workflow_stream_faulted_via_final_collected() {
        let mut final_result = create_workflow_result(
            "wf-err-final",
            r#"{"error": "final error"}"#,
            WorkflowStatus::Faulted,
        );
        final_result.error_message = Some("FinalCollected error".to_string());

        let items = vec![
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(HashMap::new()),
        ];
        let stream = futures::stream::iter(items).boxed();

        let result = collect_workflow_stream(stream).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FinalCollected error"),
            "Error message should contain 'FinalCollected error': {}",
            err_msg
        );
    }
}
