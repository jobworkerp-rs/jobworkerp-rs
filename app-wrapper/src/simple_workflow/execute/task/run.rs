use super::TaskExecutorTrait;
use crate::{
    simple_workflow::definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self},
    },
    simple_workflow::execute::{
        context::{TaskContext, UseExpression, WorkflowContext},
        job::{JobExecutorWrapper, UseJobExecutorHelper},
        DEFAULT_REQUEST_TIMEOUT_SEC,
    },
};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RunTaskExecutor<'a> {
    task: &'a workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
}
impl UseExpression for RunTaskExecutor<'_> {}
impl UseJqAndTemplateTransformer for RunTaskExecutor<'_> {}
impl UseExpressionTransformer for RunTaskExecutor<'_> {}

impl<'a> RunTaskExecutor<'a> {
    pub fn new(job_executor_wrapper: Arc<JobExecutorWrapper>, task: &'a workflow::RunTask) -> Self {
        Self {
            task,
            job_executor_wrapper,
        }
    }
    async fn execute_by_jobworkerp(
        &self,
        runner_name: &str,
        metadata: &serde_json::Value,
        job_args: serde_json::Value,
        worker_name: Option<&str>,
    ) -> Result<serde_json::Value> {
        let settings =
            metadata.get(crate::simple_workflow::definition::RUNNER_SETTINGS_METADATA_LABEL);
        let mut worker_params = metadata
            .get(crate::simple_workflow::definition::WORKER_PARAMS_METADATA_LABEL)
            .cloned();
        if let Some(worker_name) = worker_name {
            if let Some(params) = worker_params.as_ref() {
                if let serde_json::Value::Object(mut map) = params.clone() {
                    map.insert(
                        crate::simple_workflow::definition::WORKER_NAME_METADATA_LABEL.to_string(),
                        serde_json::Value::String(worker_name.to_string()),
                    );
                    worker_params = Some(serde_json::Value::Object(map));
                }
            } else {
                worker_params = Some(serde_json::json!({
                    crate::simple_workflow::definition::WORKER_NAME_METADATA_LABEL: worker_name,
                }));
            }
        }

        self.job_executor_wrapper
            .setup_worker_and_enqueue_with_json(
                runner_name,
                settings.cloned(),
                worker_params,
                job_args,
                DEFAULT_REQUEST_TIMEOUT_SEC,
            )
            .await

        // self.jobworkerp_client
        //     .setup_worker_and_enqueue_with_json(
        //         runner_name,
        //         settings.cloned(),
        //         worker_params,
        //         job_args,
        //         DEFAULT_REQUEST_TIMEOUT_SEC,
        //     )
        //     .await
    }
}
impl TaskExecutorTrait for RunTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext> {
        let workflow::RunTask {
            // export,   //: ::std::option::Option<Export>,
            metadata, //: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            // output,   //: ::std::option::Option<Output>,
            // timeout,  //: ::std::option::Option<TaskTimeout>,
            run,
            ..
        } = self.task;
        match run {
            workflow::RunTaskConfiguration::Variant1 {
                await_: _await_,
                script:
                    workflow::Script::Variant0 {
                        arguments,
                        code: runner_name,
                        environment: _env,
                        language,
                    },
            } => {
                if language.as_str() == "jobworkerp" {
                    let expression = self
                        .expression(
                            &*(workflow_context.read().await),
                            Arc::new(task_context.clone()),
                        )
                        .await?;
                    let transformed_meta = Self::transform_map(
                        task_context.input.clone(),
                        metadata.clone(),
                        &expression,
                    )?;

                    tracing::debug!("raw arguments: {:#?}", arguments);
                    let args = Self::transform_map(
                        task_context.input.clone(),
                        arguments.clone(),
                        &expression,
                    )?;
                    // let args = serde_json::Value::Object(arguments.clone());
                    tracing::debug!("transformed arguments: {:#?}", args);

                    let output = self
                        .execute_by_jobworkerp(
                            runner_name,
                            &transformed_meta,
                            args,
                            Some(task_name),
                        )
                        .await
                        .inspect_err(|e| {
                            tracing::warn!("Failed to execute by jobworkerp: {:#?}", e)
                        })?;
                    task_context.set_raw_output(output);

                    Ok(task_context)
                } else {
                    Err(anyhow::anyhow!("Not supported language: {:#?}", language))
                }
            }
            r => Err(anyhow::anyhow!(
                "Not implemented now: RunTaskConfiguration: {:#?}",
                r
            )),
        }
    }
}
