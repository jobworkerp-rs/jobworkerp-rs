use super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        job::{JobExecutorWrapper, UseJobExecutorHelper},
        DEFAULT_REQUEST_TIMEOUT_SEC,
    },
};
use anyhow::Result;
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct RunTaskExecutor {
    task: workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
}
impl UseExpression for RunTaskExecutor {}
impl UseJqAndTemplateTransformer for RunTaskExecutor {}
impl UseExpressionTransformer for RunTaskExecutor {}

impl RunTaskExecutor {
    pub fn new(job_executor_wrapper: Arc<JobExecutorWrapper>, task: workflow::RunTask) -> Self {
        Self {
            task,
            job_executor_wrapper,
        }
    }
    fn function_options_to_worker_data(
        options: Option<workflow::FunctionOptions>,
        name: &str,
    ) -> Option<WorkerData> {
        if let Some(options) = options {
            // Initialize with struct syntax instead of default() + field assignments
            let worker_data = WorkerData {
                name: name.to_string(),
                description: String::new(),
                broadcast_results: options.broadcast_results.unwrap_or(false),
                store_failure: options.store_failure.unwrap_or(false),
                store_success: options.store_success.unwrap_or(false),
                use_static: options.use_static.unwrap_or(false),
                queue_type: if options.with_backup.unwrap_or(false) {
                    proto::jobworkerp::data::QueueType::WithBackup as i32
                } else {
                    proto::jobworkerp::data::QueueType::Normal as i32
                },
                channel: options.channel,
                retry_policy: options.retry.map(|r| r.to_jobworkerp()),
                response_type: ResponseType::Direct as i32,
                ..Default::default()
            };
            Some(worker_data)
        } else {
            None
        }
    }
    async fn execute_by_jobworkerp(
        &self,
        runner_name: &str,
        settings: Option<serde_json::Value>,
        options: Option<workflow::FunctionOptions>,
        job_args: serde_json::Value,
        worker_name: &str,
    ) -> Result<serde_json::Value> {
        let worker_data =
            Self::function_options_to_worker_data(options, worker_name).unwrap_or(WorkerData {
                name: worker_name.to_string().clone(),
                description: "".to_string(),
                runner_id: None,         // replace after
                runner_settings: vec![], // replace after
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: true, //
                use_static: false,
                retry_policy: None, //TODO
                broadcast_results: true,
            });

        self.job_executor_wrapper
            .setup_worker_and_enqueue_with_json(
                runner_name,
                settings,
                worker_data,
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
impl TaskExecutorTrait<'_> for RunTaskExecutor {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // TODO: add other task types
        let workflow::RunTask {
            // TODO
            // export,   //: ::std::option::Option<Export>,
            metadata: _metadata, //: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            // output,   //: ::std::option::Option<Output>,
            // timeout,  //: ::std::option::Option<TaskTimeout>,
            // then, //  ::std::option::Option<FlowDirective>
            run,
            ..
        } = &self.task;
        // TODO: add other task types
        // currently support only RunTaskConfiguration
        let workflow::RunTaskConfiguration {
            await_: _await_,   // TODO
            return_: _return_, // TODO
            function:
                workflow::Function {
                    arguments,
                    options,
                    runner_name,
                    settings,
                },
        } = run;
        {
            // enter run.function
            task_context.add_position_name("run".to_string()).await;
            task_context.add_position_name("function".to_string()).await;

            let expression = Self::expression(
                &*(workflow_context.read().await),
                Arc::new(task_context.clone()),
            )
            .await;

            // tracing::debug!("expression: {:#?}", expression);

            let expression = match expression {
                Ok(e) => e,
                Err(mut e) => {
                    let pos = task_context.position.lock().await.clone();
                    e.position(&pos);
                    return Err(e);
                }
            };

            tracing::debug!("raw arguments: {:#?}", arguments);
            let args = match Self::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    let mut pos = task_context.position.lock().await.clone();
                    pos.push("arguments".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            };
            // let args = serde_json::Value::Object(arguments.clone());
            tracing::debug!("transformed arguments: {:#?}", args);

            let transformed_settings = match Self::transform_map(
                task_context.input.clone(),
                settings.clone(),
                &expression,
            ) {
                Ok(settings) => settings,
                Err(mut e) => {
                    let mut pos = task_context.position.lock().await.clone();
                    pos.push("settings".to_string());
                    e.position(&pos);
                    return Err(e);
                }
            };

            let output = match self
                .execute_by_jobworkerp(
                    runner_name,
                    Some(transformed_settings),
                    options.clone(),
                    args,
                    task_name,
                )
                .await
            {
                Ok(output) => Ok(output),
                Err(e) => {
                    let pos = task_context.position.lock().await.clone();
                    Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to execute by jobworkerp".to_string(),
                        Some(&pos),
                        Some(format!("{:?}", e)),
                    ))
                }
            }?;
            task_context.set_raw_output(output);

            // out of run.function
            task_context.remove_position().await;
            task_context.remove_position().await;

            Ok(task_context)
        } // r => Err(anyhow::anyhow!(
          //     "Not implemented now: RunTaskConfiguration: {:#?}",
          //     r
          // )),
    }
}
