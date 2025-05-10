use crate::workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        job::{JobExecutorWrapper, UseJobExecutorHelper},
    },
};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::{workflow_result::WorkflowStatus, InlineWorkflowArgs};
use prost::Message;
use proto::jobworkerp::data::RunnerType;
use std::sync::Arc;
use tokio::sync::RwLock;
use super::task::TaskExecutorTrait;

// unused
// TODO remove if not needed
pub struct DoAsExtWorkflowTaskExecutor<'a> {
    task: &'a workflow::DoTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
}
impl UseJqAndTemplateTransformer for DoAsExtWorkflowTaskExecutor<'_> {}
impl UseExpression for DoAsExtWorkflowTaskExecutor<'_> {}

impl<'a> DoAsExtWorkflowTaskExecutor<'a> {
    const TIMEOUT_SEC: u32 = 1800; // 30 minutes
    pub fn new(task: &'a workflow::DoTask, job_executor_wrapper: Arc<JobExecutorWrapper>) -> Self {
        Self {
            task,
            job_executor_wrapper,
        }
    }
    async fn execute_by_jobworkerp(
        &self,
        json_or_yaml: &str,
        // XXX runner settings and options in metadata
        metadata: &serde_json::Value,
        input: serde_json::Value,
        context: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // XXX timeout_sec: None
        let worker_params = metadata.get(crate::workflow::definition::WORKER_PARAMS_METADATA_LABEL);
        let args = InlineWorkflowArgs {
            workflow_source: Some(
                jobworkerp_runner::jobworkerp::runner::inline_workflow_args::WorkflowSource::WorkflowData(
                    json_or_yaml.to_string(),
                ),
            ),
            input: input.to_string(),
            workflow_context: if context.is_null() {
                None
            } else {
                Some(context.to_string())
            },
        };
        let worker_data = self
            .job_executor_wrapper
            .create_worker_data_from(
                RunnerType::InlineWorkflow.as_str_name(),
                worker_params.cloned(),
                vec![],
            )
            .await?;
        // workflow result
        let result = self
            .job_executor_wrapper
            .setup_worker_and_enqueue(
                RunnerType::InlineWorkflow.as_str_name(),
                worker_data,
                args.encode_to_vec(),
                Self::TIMEOUT_SEC,
            )
            .await?;
        match result {
            serde_json::Value::Object(mut map) => {
                let status = map.remove("status");
                if status.is_none_or(|s| s == WorkflowStatus::Completed.as_str_name()) {
                    Ok(map.remove("output").unwrap_or_default())
                } else {
                    Err(anyhow::anyhow!(
                        "Failed to execute by WORKFLOW runner: {:#?}",
                        map
                    ))
                }
            }
            _ => Err(anyhow::anyhow!(
                "Illegal WORKFLOW runner result: {:#?}",
                result
            )),
        }
    }
}

// XXX runner settings and options in metadata
impl TaskExecutorTrait<'_> for DoAsExtWorkflowTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::debug!("DoTaskExecutor: {}", task_name);
        task_context.add_position_name("do".to_string()).await;
        let do_yaml = match serde_yaml::to_string(self.task) {
            Ok(yaml) => yaml,
            Err(e) => {
                let pos = task_context.position.lock().await.clone();
                return Err(workflow::errors::ErrorFactory::new().bad_argument(
                    format!("Failed to serialize do task: {:#?}", self.task),
                    Some(&pos),
                    Some(e.to_string()),
                ));
            }
        };
        let expression = match Self::expression(
            &*(workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.lock().await.clone();
                e.position(&pos);
                return Err(e);
            }
        };
        let expression = match serde_json::to_value(expression) {
            Ok(expression) => expression,
            Err(e) => {
                let pos = task_context.position.lock().await.clone();
                return Err(workflow::errors::ErrorFactory::create_from_serde_json(
                    &e,
                    Some("Failed to serialize expression to json"),
                    Some(workflow::errors::ErrorCode::BadArgument),
                    Some(&pos),
                ));
            }
        };

        let metadata = match serde_json::to_value(&self.task.metadata) {
            Ok(metadata) => metadata,
            Err(e) => {
                let mut pos = task_context.position.lock().await.clone();
                pos.push("metadata".to_string());
                return Err(workflow::errors::ErrorFactory::create_from_serde_json(
                    &e,
                    Some(
                        format!(
                            "Failed to serialize do task metadata: {:#?}",
                            self.task.metadata
                        )
                        .as_str(),
                    ),
                    Some(workflow::errors::ErrorCode::BadArgument),
                    Some(&pos),
                ));
            }
        };
        let output = match self
            .execute_by_jobworkerp(
                &do_yaml,
                &metadata,
                task_context.input.as_ref().clone(),
                expression,
            )
            .await
        {
            Ok(output) => output,
            Err(e) => {
                tracing::warn!("Failed to execute by jobworkerp: {:#?}", e);
                // TODO get position from jobworkerp result and add inner position
                let pos = task_context.position.lock().await.clone();
                return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                    format!("Failed to execute do task: {:#?}", e),
                    Some(&pos),
                    Some(format!("{:?}", e)),
                ));
            }
        };
        task_context.set_raw_output(output);

        Ok(task_context)
    }
}
