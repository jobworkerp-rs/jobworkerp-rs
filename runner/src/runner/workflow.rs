use crate::jobworkerp::runner::workflow_result::WorkflowStatus;
use crate::jobworkerp::runner::{Empty, ReusableWorkflowRunnerSettings, WorkflowResult};
use crate::runner::CollectStreamFuture;
use crate::{schema_to_json_string, schema_to_json_string_option};
use futures::StreamExt;
use futures::stream::BoxStream;
use prost::Message;
use proto::DEFAULT_METHOD_NAME;
use proto::jobworkerp::data::{ResultOutputItem, RunnerType, StreamingOutputType};
use std::collections::HashMap;

use super::RunnerSpec;

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
                ..Default::default()
            },
        );
        schemas
    }

    // Reason: Protobuf oneof field workflow_source requires oneOf constraint
    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            DEFAULT_METHOD_NAME.to_string(),
            super::MethodJsonSchema {
                args_schema: include_str!("../../schema/WorkflowArgs.json").to_string(),
                result_schema: schema_to_json_string_option!(WorkflowResult, "output_schema"),
                feed_data_schema: None,
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(Empty, "settings_schema")
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

    fn method_json_schema_map(&self) -> HashMap<String, super::MethodJsonSchema> {
        InlineWorkflowRunnerSpec::method_json_schema_map(self)
    }

    fn settings_schema(&self) -> String {
        InlineWorkflowRunnerSpec::settings_schema(self)
    }

    /// Collect streaming workflow results into a single WorkflowResult
    ///
    /// Strategy:
    /// - If FinalCollected is received, use that as the final result
    /// - Otherwise, keeps only the last WorkflowResult (represents final workflow state)
    /// - Intermediate results are discarded
    /// - Returns error if data items were received but all decodes failed
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
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
                                    "Failed to decode WorkflowResult in InlineWorkflow collect_stream ({}/{}): {:?}",
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

            // FinalCollected がある場合はそこからステータスをチェック
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

            // Return error if we received data items but all decodes failed
            if data_item_count > 0 && final_result.is_none() {
                return Err(anyhow::anyhow!(
                    "All {} WorkflowResult decode attempts failed in InlineWorkflow collect_stream",
                    decode_failure_count
                ));
            }

            // final_result のステータスをチェック
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
                description: Some("Execute reusable workflow".to_string()),
                output_type: StreamingOutputType::Both as i32,
                ..Default::default()
            },
        );
        schemas
    }

    fn settings_schema(&self) -> String {
        schema_to_json_string!(ReusableWorkflowRunnerSettings, "settings_schema")
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

    /// Collect streaming workflow results into a single WorkflowResult
    ///
    /// Strategy:
    /// - If FinalCollected is received, use that as the final result
    /// - Otherwise, keeps only the last WorkflowResult (represents final workflow state)
    /// - Intermediate results are discarded
    /// - Returns error if data items were received but all decodes failed
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        _using: Option<&str>,
    ) -> CollectStreamFuture {
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
                                    "Failed to decode WorkflowResult in ReusableWorkflow collect_stream ({}/{}): {:?}",
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

            // FinalCollected がある場合はそこからステータスをチェック
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

            // Return error if we received data items but all decodes failed
            if data_item_count > 0 && final_result.is_none() {
                return Err(anyhow::anyhow!(
                    "All {} WorkflowResult decode attempts failed in ReusableWorkflow collect_stream",
                    decode_failure_count
                ));
            }

            // final_result のステータスをチェック
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use proto::jobworkerp::data::{Trailer, result_output_item};

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
        ResultOutputItem {
            item: Some(result_output_item::Item::Data(result.encode_to_vec())),
        }
    }

    fn create_end_item(metadata: HashMap<String, String>) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer { metadata })),
        }
    }

    fn create_final_collected_item(data: Vec<u8>) -> ResultOutputItem {
        ResultOutputItem {
            item: Some(result_output_item::Item::FinalCollected(data)),
        }
    }

    #[tokio::test]
    async fn test_inline_workflow_collect_stream_single_result() {
        let runner = InlineWorkflowRunnerSpecImpl::new();
        let result = create_workflow_result(
            "wf-1",
            r#"{"result": "success"}"#,
            WorkflowStatus::Completed,
        );

        let mut metadata = HashMap::new();
        metadata.insert("trace_id".to_string(), "abc123".to_string());

        let items = vec![create_data_item(&result), create_end_item(metadata.clone())];
        let stream = stream::iter(items).boxed();

        let (bytes, returned_metadata) = runner.collect_stream(stream, None).await.unwrap();

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
    async fn test_inline_workflow_collect_stream_multiple_results_keeps_last() {
        let runner = InlineWorkflowRunnerSpecImpl::new();
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
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.output, r#"{"step": 3, "final": true}"#);
        assert_eq!(decoded.status, WorkflowStatus::Completed as i32);
    }

    #[tokio::test]
    async fn test_inline_workflow_collect_stream_final_collected_takes_precedence() {
        let runner = InlineWorkflowRunnerSpecImpl::new();
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
        let stream = stream::iter(items).boxed();

        let (bytes, returned_metadata) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.output, r#"{"final": true}"#);
        assert_eq!(decoded.status, WorkflowStatus::Completed as i32);
        assert_eq!(returned_metadata.get("key"), Some(&"value".to_string()));
    }

    #[tokio::test]
    async fn test_inline_workflow_collect_stream_empty_returns_empty() {
        let runner = InlineWorkflowRunnerSpecImpl::new();

        let items = vec![create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();
        assert!(bytes.is_empty());
    }

    #[tokio::test]
    async fn test_inline_workflow_collect_stream_faulted_status() {
        let runner = InlineWorkflowRunnerSpecImpl::new();
        let mut result = create_workflow_result(
            "wf-err",
            r#"{"error": "something failed"}"#,
            WorkflowStatus::Faulted,
        );
        result.error_message = Some("Task failed".to_string());

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        // Faulted ステータスの場合は Err が返ることを確認
        let result = runner.collect_stream(stream, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Task failed"),
            "Error message should contain 'Task failed': {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_reusable_workflow_collect_stream_single_result() {
        let runner = ReusableWorkflowRunnerSpecImpl::new();
        let result =
            create_workflow_result("rwf-1", r#"{"reusable": true}"#, WorkflowStatus::Completed);

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.id, "rwf-1");
        assert_eq!(decoded.output, r#"{"reusable": true}"#);
    }

    #[tokio::test]
    async fn test_reusable_workflow_collect_stream_final_collected() {
        let runner = ReusableWorkflowRunnerSpecImpl::new();
        let final_result = create_workflow_result(
            "rwf-final",
            r#"{"collected": true}"#,
            WorkflowStatus::Completed,
        );

        let items = vec![
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        let (bytes, _) = runner.collect_stream(stream, None).await.unwrap();

        let decoded = WorkflowResult::decode(bytes.as_slice()).unwrap();
        assert_eq!(decoded.id, "rwf-final");
        assert_eq!(decoded.output, r#"{"collected": true}"#);
    }

    #[tokio::test]
    async fn test_reusable_workflow_collect_stream_faulted_status() {
        let runner = ReusableWorkflowRunnerSpecImpl::new();
        let mut result = create_workflow_result(
            "rwf-err",
            r#"{"error": "reusable workflow failed"}"#,
            WorkflowStatus::Faulted,
        );
        result.error_message = Some("Reusable task failed".to_string());

        let items = vec![create_data_item(&result), create_end_item(HashMap::new())];
        let stream = stream::iter(items).boxed();

        // Faulted ステータスの場合は Err が返ることを確認
        let result = runner.collect_stream(stream, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Reusable task failed"),
            "Error message should contain 'Reusable task failed': {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_reusable_workflow_collect_stream_faulted_via_final_collected() {
        let runner = ReusableWorkflowRunnerSpecImpl::new();
        let mut final_result = create_workflow_result(
            "rwf-err-final",
            r#"{"error": "final error"}"#,
            WorkflowStatus::Faulted,
        );
        final_result.error_message = Some("FinalCollected error".to_string());

        let items = vec![
            create_final_collected_item(final_result.encode_to_vec()),
            create_end_item(HashMap::new()),
        ];
        let stream = stream::iter(items).boxed();

        // FinalCollected 経由の Faulted も Err になることを確認
        let result = runner.collect_stream(stream, None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("FinalCollected error"),
            "Error message should contain 'FinalCollected error': {}",
            err_msg
        );
    }
}
