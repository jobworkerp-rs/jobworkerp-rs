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
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
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
    fn function_options_to_worker_data(
        options: Option<workflow::FunctionOptions>,
        name: &str,
    ) -> Option<WorkerData> {
        if let Some(options) = options {
            let mut worker_data = WorkerData::default();
            worker_data.name = name.to_string();

            // Access the fields directly from FunctionOptions struct
            worker_data.broadcast_results = options.broadcast_results_to_listener.unwrap_or(false);

            if let Some(channel) = options.channel {
                worker_data.channel = Some(channel);
            }

            // Handle retry options
            if let Some(retry_opts) = options.retry_options {
                let mut worker_retry_policy = proto::jobworkerp::data::RetryPolicy::default();

                if let Some(basis) = retry_opts.basis {
                    worker_retry_policy.basis = basis as f32;
                }

                if let Some(interval) = retry_opts.interval {
                    worker_retry_policy.interval = interval as u32;
                }

                if let Some(max_count) = retry_opts.max_count {
                    worker_retry_policy.max_retry = max_count as u32;
                }

                if let Some(max_interval) = retry_opts.max_interval {
                    worker_retry_policy.max_interval = max_interval as u32;
                }

                if let Some(retry_type) = retry_opts.retry_type {
                    worker_retry_policy.r#type = match retry_type {
                        workflow::RetryType::Exponential => {
                            proto::jobworkerp::data::RetryType::Exponential as i32
                        }
                        workflow::RetryType::Linear => {
                            proto::jobworkerp::data::RetryType::Linear as i32
                        }
                        workflow::RetryType::Constant => {
                            proto::jobworkerp::data::RetryType::Constant as i32
                        }
                        _ => proto::jobworkerp::data::RetryType::None as i32,
                    };
                }

                worker_data.retry_policy = Some(worker_retry_policy);
            }

            worker_data.store_failure = options.store_failure.unwrap_or(false);
            worker_data.store_success = options.store_success.unwrap_or(false);
            worker_data.use_static = options.use_static.unwrap_or(false);
            worker_data.queue_type = if options.with_backup.unwrap_or(false) {
                proto::jobworkerp::data::QueueType::WithBackup as i32
            } else {
                proto::jobworkerp::data::QueueType::Normal as i32
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
impl TaskExecutorTrait for RunTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext> {
        let workflow::RunTask {
            // TODO
            // export,   //: ::std::option::Option<Export>,
            metadata: _metadata, //: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            // output,   //: ::std::option::Option<Output>,
            // timeout,  //: ::std::option::Option<TaskTimeout>,
            // then, //  ::std::option::Option<FlowDirective>
            run,
            ..
        } = self.task;
        match run { // TODO: add other task types
            // currently support only RunTaskConfiguration
            workflow::RunTaskConfiguration {
                await_: _await_,   // TODO
                return_: _return_, // TODO
                function:
                    workflow::Function {
                        arguments,
                        options,
                        runner_name,
                        settings,
                    },
            } => {
                let expression = self
                    .expression(
                        &*(workflow_context.read().await),
                        Arc::new(task_context.clone()),
                    )
                    .await?;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                )?;
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                let transformed_settings =
                    Self::transform_map(task_context.input.clone(), settings.clone(), &expression)?;

                let output = self
                    .execute_by_jobworkerp(
                        runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        task_name,
                    )
                    .await
                    .inspect_err(|e| tracing::warn!("Failed to execute by jobworkerp: {:#?}", e))?;
                task_context.set_raw_output(output);

                Ok(task_context)
            } // r => Err(anyhow::anyhow!(
              //     "Not implemented now: RunTaskConfiguration: {:#?}",
              //     r
              // )),
        }
    }
}
