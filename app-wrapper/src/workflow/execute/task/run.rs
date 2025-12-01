pub mod python;

use super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, RunJobRunner, RunJobWorker},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
    },
};
use anyhow::Result;
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use command_utils::trace::Tracing;
use proto::jobworkerp::data::{QueueType, ResponseType, WorkerData};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

pub struct RunTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}
impl UseExpression for RunTaskExecutor {}
impl UseJqAndTemplateTransformer for RunTaskExecutor {}
impl UseExpressionTransformer for RunTaskExecutor {}
impl Tracing for RunTaskExecutor {}
impl RunTaskExecutor {
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_task_timeout: Duration,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        task: workflow::RunTask,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_task_timeout,
            task,
            job_executor_wrapper,
            metadata,
        }
    }

    /// Convert workflow QueueType to proto QueueType
    fn convert_queue_type(qt: workflow::QueueType) -> i32 {
        match qt {
            workflow::QueueType::Normal => QueueType::Normal as i32,
            workflow::QueueType::WithBackup => QueueType::WithBackup as i32,
            workflow::QueueType::DbOnly => QueueType::DbOnly as i32,
        }
    }

    /// Convert workflow ResponseType to proto ResponseType
    fn convert_response_type(rt: workflow::ResponseType) -> i32 {
        match rt {
            workflow::ResponseType::NoResult => ResponseType::NoResult as i32,
            workflow::ResponseType::Direct => ResponseType::Direct as i32,
        }
    }

    fn function_options_to_worker_data(
        options: Option<workflow::WorkerOptions>,
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
                queue_type: options
                    .queue_type
                    .map(Self::convert_queue_type)
                    .unwrap_or(QueueType::Normal as i32),
                channel: options.channel,
                retry_policy: options.retry.map(|r| r.to_jobworkerp()),
                response_type: options
                    .response_type
                    .map(Self::convert_response_type)
                    .unwrap_or(ResponseType::Direct as i32),
                ..Default::default()
            };
            Some(worker_data)
        } else {
            None
        }
    }
    #[allow(clippy::too_many_arguments)]
    async fn execute_by_jobworkerp(
        &self,
        cx: Arc<opentelemetry::Context>,
        runner_name: &str,
        settings: Option<serde_json::Value>,
        options: Option<workflow::WorkerOptions>,
        job_args: serde_json::Value,
        worker_name: &str,
        using: Option<String>,
    ) -> Result<serde_json::Value> {
        let runner = self
            .job_executor_wrapper
            .runner_app()
            .find_runner_by_name(runner_name)
            .await
            .map_err(|e| {
                anyhow::anyhow!("Failed to find runner by name '{}': {:#?}", runner_name, e)
            })?
            .ok_or(anyhow::anyhow!("Runner '{}' not found", runner_name))?;
        let settings = self
            .job_executor_wrapper
            .setup_runner_and_settings(&runner, settings)
            .await?;
        let mut worker_data = Self::function_options_to_worker_data(options, worker_name)
            .unwrap_or(WorkerData {
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
        worker_data.runner_id = runner.id;
        worker_data.runner_settings = settings;
        // inject metadata from opentelemetry context for remote job
        let mut metadata = (*self.metadata).clone();
        Self::inject_metadata_from_context(&mut metadata, &cx);
        self.job_executor_wrapper
            .setup_worker_and_enqueue_with_json(
                Arc::new(metadata),
                runner_name,
                worker_data,
                job_args,
                None, // XXX no uniq_key,
                self.default_task_timeout.as_secs() as u32,
                false, // no streaming
                using,
            )
            .await
    }
}
impl TaskExecutorTrait<'_> for RunTaskExecutor {
    async fn execute(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: &str,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        // TODO: add other task types
        let workflow::RunTask {
            // TODO
            // export,   //: ::std::option::Option<Export>,
            metadata: task_metadata, //: ::serde_json::Map<::std::string::String, ::serde_json::Value>,
            // output,   //: ::std::option::Option<Output>,
            timeout, //: ::std::option::Option<TaskTimeout>,
            // then, //  ::std::option::Option<FlowDirective>
            run,
            ..
        } = &self.task;
        let timeout_sec = if let Some(workflow::TaskTimeout::Timeout(duration)) = timeout {
            (duration.after.to_millis() / 1000) as u32
        } else {
            self.default_task_timeout.as_secs() as u32
        };

        // merge metadata: create new metadata with both self.metadata and task metadata
        let mut metadata = (*self.metadata).clone();
        for (k, v) in task_metadata {
            // if key already exists, append value
            if metadata.contains_key(k) {
                continue; // skip if key already exists
            } else if let Some(v) = v.as_str() {
                // add only if value is a string
                metadata.insert(k.clone(), v.to_string());
            }
        }
        let metadata = Arc::new(metadata);

        // setup task context
        task_context.add_position_name("run".to_string()).await;

        let expression = Self::expression(
            &*(self.workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await;

        // tracing::debug!("expression: {:#?}", expression);

        let expression = match expression {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let pos = pos.read().await;
                e.position(&pos);
                return Err(e);
            }
        };

        // TODO: add other task types
        // currently support only RunTaskConfiguration
        match run {
            workflow::RunTaskConfiguration::Worker(workflow::RunWorker {
                worker:
                    RunJobWorker {
                        arguments,
                        name: worker_name,
                    },
            }) => {
                task_context.add_position_name("worker".to_string()).await;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                let output = match self
                    .job_executor_wrapper
                    .enqueue_with_worker_name_and_output_json(
                        metadata,
                        worker_name,
                        &args,
                        None,
                        timeout_sec,
                        false,
                        None, // using not used in workflow tasks
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(format!("{e:?}")),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Runner(workflow::RunRunner {
                runner:
                    RunJobRunner {
                        arguments,
                        name: runner_name,
                        options,
                        settings,
                        using,
                    },
            }) => {
                task_context.add_position_name("runner".to_string()).await;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                // enter run.function
                let transformed_settings = match Self::transform_map(
                    task_context.input.clone(),
                    settings.clone(),
                    &expression,
                ) {
                    Ok(settings) => settings,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("settings".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };

                let output = match self
                    .execute_by_jobworkerp(
                        cx.clone(),
                        runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        task_name,
                        using.clone(),
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(format!("{e:?}")),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                function:
                    workflow::RunJobFunction::WorkerFunction {
                        arguments,
                        worker_name,
                    },
            }) => {
                task_context.add_position_name("function".to_string()).await;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                let output = match self
                    .job_executor_wrapper
                    .enqueue_with_worker_name_and_output_json(
                        metadata,
                        worker_name,
                        &args,
                        None,
                        timeout_sec,
                        false,
                        None, // using not used in workflow tasks
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(format!("{e:?}")),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                function:
                    workflow::RunJobFunction::RunnerFunction {
                        arguments,
                        options,
                        runner_name,
                        settings,
                        using,
                    },
            }) => {
                task_context.add_position_name("function".to_string()).await;

                tracing::debug!("raw arguments: {:#?}", arguments);
                let args = match Self::transform_map(
                    task_context.input.clone(),
                    arguments.clone(),
                    &expression,
                ) {
                    Ok(args) => args,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("arguments".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                // let args = serde_json::Value::Object(arguments.clone());
                tracing::debug!("transformed arguments: {:#?}", args);

                // enter run.function
                let transformed_settings = match Self::transform_map(
                    task_context.input.clone(),
                    settings.clone(),
                    &expression,
                ) {
                    Ok(settings) => settings,
                    Err(mut e) => {
                        let pos = task_context.position.clone();
                        let mut pos = pos.write().await;
                        pos.push("settings".to_string());
                        e.position(&pos);
                        return Err(e);
                    }
                };
                tracing::debug!("transformed settings: {:#?}", &settings);

                let output = match self
                    .execute_by_jobworkerp(
                        cx.clone(),
                        runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        task_name,
                        using.clone(),
                    )
                    .await
                {
                    Ok(output) => Ok(output),
                    Err(e) => {
                        let pos = task_context.position.clone();
                        let pos = pos.read().await.as_error_instance();
                        Err(workflow::errors::ErrorFactory::new().service_unavailable(
                            "Failed to execute by jobworkerp".to_string(),
                            Some(pos),
                            Some(format!("{e:?}")),
                        ))
                    }
                }?;
                task_context.set_raw_output(output);

                // out of run.function
                task_context.remove_position().await;
                task_context.remove_position().await;

                Ok(task_context)
            }
            workflow::RunTaskConfiguration::Script(run_script) => {
                task_context.add_position_name("script".to_string()).await;

                let executor = python::PythonTaskExecutor::new(
                    self.workflow_context.clone(),
                    Duration::from_secs(timeout_sec as u64),
                    self.job_executor_wrapper.clone(),
                    run_script.clone(),
                    metadata.clone(),
                );
                let result = executor.execute(cx, task_name, task_context).await;

                match result {
                    Ok(tc) => {
                        tc.remove_position().await; // Remove "script"
                        tc.remove_position().await; // Remove "run"
                        Ok(tc)
                    }
                    Err(e) => Err(e),
                }
            } // _ => {
              //     let pos = task_context.position.clone();
              //     let mut pos = pos.write().await;
              //     pos.push("run".to_string());
              //     Err(workflow::errors::ErrorFactory::new().not_implemented(
              //         "RunTaskConfiguration".to_string(),
              //         Some(pos.as_error_instance()),
              //         Some(format!("Not implemented now: {:#?}", run)),
              //     ))
              // }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunTaskExecutor;
    use crate::workflow::definition::workflow;
    use proto::jobworkerp::data::{QueueType, ResponseType};

    fn worker_options_with_queue_and_response(
        queue_type: workflow::QueueType,
        response_type: workflow::ResponseType,
    ) -> workflow::WorkerOptions {
        workflow::WorkerOptions {
            broadcast_results: Some(false),
            channel: None,
            queue_type: Some(queue_type),
            response_type: Some(response_type),
            retry: None,
            store_failure: Some(true),
            store_success: Some(false),
            use_static: Some(false),
        }
    }

    #[test]
    fn worker_data_respects_queue_and_response_type() {
        let options = worker_options_with_queue_and_response(
            workflow::QueueType::DbOnly,
            workflow::ResponseType::NoResult,
        );

        let worker_data =
            RunTaskExecutor::function_options_to_worker_data(Some(options), "test_worker")
                .expect("worker data should be generated");

        assert_eq!(worker_data.queue_type, QueueType::DbOnly as i32);
        assert_eq!(worker_data.response_type, ResponseType::NoResult as i32);
    }

    #[test]
    fn worker_data_defaults_to_normal_queue_and_direct_response() {
        let options = workflow::WorkerOptions {
            broadcast_results: None,
            channel: None,
            queue_type: None,
            response_type: None,
            retry: None,
            store_failure: None,
            store_success: None,
            use_static: None,
        };

        let worker_data =
            RunTaskExecutor::function_options_to_worker_data(Some(options), "legacy_worker")
                .expect("worker data should be generated");

        assert_eq!(worker_data.queue_type, QueueType::Normal as i32);
        assert_eq!(worker_data.response_type, ResponseType::Direct as i32);
    }
}
