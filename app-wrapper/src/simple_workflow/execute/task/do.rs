use crate::simple_workflow::{
    definition::{
        transform::UseJqAndTemplateTransformer,
        workflow::{self},
    },
    execute::{
        context::{TaskContext, UseExpression, WorkflowContext},
        job::{JobExecutorWrapper, UseJobExecutorHelper},
    },
};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::{workflow_result::WorkflowStatus, WorkflowArg};
use prost::Message;
use proto::jobworkerp::data::RunnerType;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::TaskExecutorTrait;

pub struct DoTaskExecutor<'a> {
    task: &'a workflow::DoTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
}
impl UseJqAndTemplateTransformer for DoTaskExecutor<'_> {}
impl UseExpression for DoTaskExecutor<'_> {}

impl<'a> DoTaskExecutor<'a> {
    const TIMEOUT_SEC: u32 = 1800; // 30 minutes
    pub fn new(task: &'a workflow::DoTask, job_executor_wrapper: Arc<JobExecutorWrapper>) -> Self {
        Self {
            task,
            job_executor_wrapper,
        }
    }
    async fn execute_by_jobworkerp(
        &self,
        yaml: &str,
        metadata: &serde_json::Value,
        input: serde_json::Value,
        context: serde_json::Value,
    ) -> Result<serde_json::Value> {
        // XXX timeout_sec: None
        let worker_params =
            metadata.get(crate::simple_workflow::definition::WORKER_PARAMS_METADATA_LABEL);
        let args = WorkflowArg {
            workflow_url: None,
            workflow_yaml: Some(yaml.to_string()),
            input: input.to_string(),
            workflow_context: if context.is_null() {
                None
            } else {
                Some(context.to_string())
            },
        };

        // workflow result
        let result = self
            .job_executor_wrapper
            .setup_worker_and_enqueue(
                RunnerType::SimpleWorkflow.as_str_name(),
                vec![],
                worker_params.cloned(),
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

impl TaskExecutorTrait for DoTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::debug!("DoTaskExecutor: {}", task_name);
        let do_yaml = serde_yaml::to_string(self.task)?;
        let expression = self
            .expression(
                &*(workflow_context.read().await),
                Arc::new(task_context.clone()),
            )
            .await?;

        let output = self
            .execute_by_jobworkerp(
                &do_yaml,
                &serde_json::to_value(&self.task.metadata)?,
                task_context.input.as_ref().clone(),
                serde_json::to_value(expression)?,
            )
            .await
            .inspect_err(|e| tracing::warn!("Failed to execute by jobworkerp: {:#?}", e))?;
        task_context.set_raw_output(output);

        Ok(task_context)
    }
}
