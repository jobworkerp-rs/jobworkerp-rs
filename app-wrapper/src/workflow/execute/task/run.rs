pub mod python;

use super::{StreamTaskExecutorTrait, TaskExecutorTrait};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, RunJobRunner, RunJobWorker},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        expression::UseExpression,
    },
};
use anyhow::Result;
use app::app::{
    job::execute::{JobExecutorWrapper, UseJobExecutor},
    runner::UseRunnerApp,
};
use async_stream::stream;
use command_utils::trace::Tracing;
use proto::jobworkerp::data::{
    JobId, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
};
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
                StreamingType::None,
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
                        using,
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
                        StreamingType::None,
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
                        using,
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
                        StreamingType::None,
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

/// RunStreamTaskExecutor - Streaming version of RunTaskExecutor
///
/// This executor implements `TaskExecutorTrait` and internally uses streaming execution.
/// It enqueues jobs with `streaming=true` and `broadcast_results=true`, allowing:
/// - Intermediate results to be accessed via `JobResultService/ListenStream`
/// - Final results to be collected and stored in `TaskContext.raw_output`
pub struct RunStreamTaskExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_task_timeout: Duration,
    task: workflow::RunTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
}

impl UseExpression for RunStreamTaskExecutor {}
impl UseJqAndTemplateTransformer for RunStreamTaskExecutor {}
impl UseExpressionTransformer for RunStreamTaskExecutor {}
impl Tracing for RunStreamTaskExecutor {}

impl RunStreamTaskExecutor {
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

    /// Convert workflow QueueType to proto QueueType (same as RunTaskExecutor)
    fn convert_queue_type(qt: workflow::QueueType) -> i32 {
        match qt {
            workflow::QueueType::Normal => QueueType::Normal as i32,
            workflow::QueueType::WithBackup => QueueType::WithBackup as i32,
            workflow::QueueType::DbOnly => QueueType::DbOnly as i32,
        }
    }

    /// Convert workflow ResponseType to proto ResponseType (same as RunTaskExecutor)
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
}

/// Handle for a started streaming job, containing all info needed to collect results
/// NOTE: stream field was removed - use listen_result_by_job_id to subscribe to job results
pub struct StreamingJobHandle {
    pub job_id: JobId,
    pub runner_id: RunnerId,
    pub runner_name: String,
    pub runner_data: proto::jobworkerp::data::RunnerData,
    pub worker_name: String,
    pub runner_spec: Option<Box<dyn jobworkerp_runner::runner::RunnerSpec + Send + Sync>>,
    pub using: Option<String>,
    pub timeout_sec: u32,
}

impl StreamTaskExecutorTrait<'_> for RunStreamTaskExecutor {
    fn execute_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
        // Clone all required values for the stream
        let workflow_context = self.workflow_context.clone();
        let job_executor_wrapper = self.job_executor_wrapper.clone();
        let task = self.task.clone();
        let base_metadata = self.metadata.clone();
        let default_timeout = self.default_task_timeout;

        stream! {
            let mut task_context = task_context;

            let workflow::RunTask {
                metadata: task_metadata,
                timeout,
                run,
                ..
            } = &task;

            // === 1. Timeout setting ===
            let timeout_sec = if let Some(workflow::TaskTimeout::Timeout(duration)) = timeout {
                (duration.after.to_millis() / 1000) as u32
            } else {
                default_timeout.as_secs() as u32
            };

            // === 2. Metadata merge ===
            let mut metadata = (*base_metadata).clone();
            for (k, v) in task_metadata {
                if !metadata.contains_key(k) {
                    if let Some(v) = v.as_str() {
                        metadata.insert(k.clone(), v.to_string());
                    }
                }
            }
            RunStreamTaskExecutor::inject_metadata_from_context(&mut metadata, &cx);
            let metadata = Arc::new(metadata);

            // === 3. Position setup ("run" added) ===
            task_context.add_position_name("run".to_string()).await;

            // === 4. Expression evaluation ===
            let expression = match RunStreamTaskExecutor::expression(
                &*workflow_context.read().await,
                Arc::new(task_context.clone()),
            ).await {
                Ok(e) => e,
                Err(mut e) => {
                    let pos = task_context.position.read().await;
                    e.position(&pos);
                    yield Err(e);
                    return;
                }
            };

            // === 5. Start job based on configuration ===
            let handle_result: Result<StreamingJobHandle, Box<workflow::Error>> = match run {
                // Worker configuration
                workflow::RunTaskConfiguration::Worker(workflow::RunWorker {
                    worker: RunJobWorker { arguments, name: worker_name, using },
                }) => {
                    task_context.add_position_name("worker".to_string()).await;

                    tracing::debug!("raw arguments: {:#?}", arguments);
                    let args = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        arguments.clone(),
                        &expression,
                    ) {
                        Ok(args) => args,
                        Err(mut e) => {
                            task_context.position.write().await.push("arguments".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };
                    tracing::debug!("transformed arguments: {:#?}", args);

                    // Start worker streaming job
                    match start_worker_streaming_job_static(
                        &job_executor_wrapper,
                        metadata.clone(),
                        worker_name,
                        args,
                        timeout_sec,
                        using.clone(),
                    ).await {
                        Ok(h) => Ok(h),
                        Err(e) => {
                            let pos = task_context.position.read().await.as_error_instance();
                            Err(workflow::errors::ErrorFactory::new().service_unavailable(
                                "Failed to start streaming job by jobworkerp".to_string(),
                                Some(pos),
                                Some(format!("{e:?}")),
                            ))
                        }
                    }
                }

                // Function(WorkerFunction) configuration
                workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                    function: workflow::RunJobFunction::WorkerFunction { arguments, using, worker_name },
                }) => {
                    task_context.add_position_name("function".to_string()).await;

                    tracing::debug!("raw arguments: {:#?}", arguments);
                    let args = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        arguments.clone(),
                        &expression,
                    ) {
                        Ok(args) => args,
                        Err(mut e) => {
                            task_context.position.write().await.push("arguments".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };
                    tracing::debug!("transformed arguments: {:#?}", args);

                    // Start worker streaming job
                    match start_worker_streaming_job_static(
                        &job_executor_wrapper,
                        metadata.clone(),
                        worker_name,
                        args,
                        timeout_sec,
                        using.clone(),
                    ).await {
                        Ok(h) => Ok(h),
                        Err(e) => {
                            let pos = task_context.position.read().await.as_error_instance();
                            Err(workflow::errors::ErrorFactory::new().service_unavailable(
                                "Failed to start streaming job by jobworkerp".to_string(),
                                Some(pos),
                                Some(format!("{e:?}")),
                            ))
                        }
                    }
                }

                // Runner configuration
                workflow::RunTaskConfiguration::Runner(workflow::RunRunner {
                    runner: RunJobRunner { arguments, name: runner_name, options, settings, using },
                }) => {
                    task_context.add_position_name("runner".to_string()).await;

                    tracing::debug!("raw arguments: {:#?}", arguments);
                    let args = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        arguments.clone(),
                        &expression,
                    ) {
                        Ok(args) => args,
                        Err(mut e) => {
                            task_context.position.write().await.push("arguments".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };
                    tracing::debug!("transformed arguments: {:#?}", args);

                    let transformed_settings = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        settings.clone(),
                        &expression,
                    ) {
                        Ok(s) => s,
                        Err(mut e) => {
                            task_context.position.write().await.push("settings".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };

                    // Start runner streaming job
                    match start_runner_streaming_job_static(
                        &job_executor_wrapper,
                        metadata.clone(),
                        timeout_sec,
                        runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        &task_name,
                        using.clone(),
                    ).await {
                        Ok(h) => Ok(h),
                        Err(e) => {
                            let pos = task_context.position.read().await.as_error_instance();
                            Err(workflow::errors::ErrorFactory::new().service_unavailable(
                                "Failed to start streaming runner job by jobworkerp".to_string(),
                                Some(pos),
                                Some(format!("{e:?}")),
                            ))
                        }
                    }
                }

                // Function(RunnerFunction) configuration
                workflow::RunTaskConfiguration::Function(workflow::RunFunction {
                    function: workflow::RunJobFunction::RunnerFunction { arguments, options, runner_name, settings, using },
                }) => {
                    task_context.add_position_name("function".to_string()).await;

                    tracing::debug!("raw arguments: {:#?}", arguments);
                    let args = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        arguments.clone(),
                        &expression,
                    ) {
                        Ok(args) => args,
                        Err(mut e) => {
                            task_context.position.write().await.push("arguments".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };
                    tracing::debug!("transformed arguments: {:#?}", args);

                    let transformed_settings = match RunStreamTaskExecutor::transform_map(
                        task_context.input.clone(),
                        settings.clone(),
                        &expression,
                    ) {
                        Ok(s) => s,
                        Err(mut e) => {
                            task_context.position.write().await.push("settings".to_string());
                            e.position(&*task_context.position.read().await);
                            yield Err(e);
                            return;
                        }
                    };

                    // Start runner streaming job
                    match start_runner_streaming_job_static(
                        &job_executor_wrapper,
                        metadata.clone(),
                        timeout_sec,
                        runner_name,
                        Some(transformed_settings),
                        options.clone(),
                        args,
                        &task_name,
                        using.clone(),
                    ).await {
                        Ok(h) => Ok(h),
                        Err(e) => {
                            let pos = task_context.position.read().await.as_error_instance();
                            Err(workflow::errors::ErrorFactory::new().service_unavailable(
                                "Failed to start streaming runner job by jobworkerp".to_string(),
                                Some(pos),
                                Some(format!("{e:?}")),
                            ))
                        }
                    }
                }

                // Script configuration is not supported for streaming
                workflow::RunTaskConfiguration::Script(_) => {
                    let pos = task_context.position.read().await.as_error_instance();
                    yield Err(workflow::errors::ErrorFactory::new().not_implemented(
                        "RunStreamTaskExecutor does not support Script configurations".to_string(),
                        Some(pos),
                        Some("Use RunTaskExecutor for Script configurations".to_string()),
                    ));
                    return;
                }
            };

            // Handle job start result
            let handle = match handle_result {
                Ok(h) => h,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };

            // === 6. Spawn collect task and emit StreamingJobStarted event ===
            // NOTE: We must spawn the collect task BEFORE yielding StreamingJobStarted,
            // then yield the event, then wait for the result. This ensures the event
            // is delivered to consumers immediately while collection happens in parallel.
            let position = task_context.position.read().await.as_json_pointer();
            let job_id = handle.job_id.value;
            let runner_name = handle.runner_name.clone();
            let worker_name = handle.worker_name.clone();

            // Spawn collection task in background
            let job_executor_wrapper_clone = job_executor_wrapper.clone();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let result = collect_streaming_result_static(&job_executor_wrapper_clone, handle).await;
                let _ = result_tx.send(result);
            });

            // Now yield the event - this will be delivered immediately since
            // collect_streaming_result_static is running in a separate task
            yield Ok(WorkflowStreamEvent::streaming_job_started(
                job_id,
                &runner_name,
                &worker_name,
                &position,
            ));

            // === 7. Wait for stream result ===
            let output = match result_rx.await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => {
                    let pos = task_context.position.read().await.as_error_instance();
                    yield Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to collect streaming result".to_string(),
                        Some(pos),
                        Some(format!("{e:?}")),
                    ));
                    return;
                }
                Err(_recv_err) => {
                    let pos = task_context.position.read().await.as_error_instance();
                    yield Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Streaming result collection task was dropped".to_string(),
                        Some(pos),
                        None,
                    ));
                    return;
                }
            };
            task_context.set_raw_output(output);

            // === 8. Position cleanup ===
            task_context.remove_position().await; // "worker"/"runner"/"function"
            task_context.remove_position().await; // "run"

            // === 9. Emit StreamingJobCompleted event ===
            yield Ok(WorkflowStreamEvent::streaming_job_completed(
                job_id,
                None, // job_result_id
                &position,
                task_context,
            ));
        }
    }
}

/// Start a worker streaming job and return handle with job_id (static version for use in stream)
/// NOTE: Uses existing worker, which may have response_type=Direct causing wait behavior.
/// Consider using runner-based streaming for immediate StreamingJobStarted events.
async fn start_worker_streaming_job_static(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
    worker_name: &str,
    args: serde_json::Value,
    timeout_sec: u32,
    using: Option<String>,
) -> Result<StreamingJobHandle> {
    use app::app::job::execute::{UseJobExecutor, UseRunnerSpecFactory};
    use app::app::runner::UseRunnerApp;
    use app::app::worker::UseWorkerApp;
    use infra::infra::runner::rows::RunnerWithSchema;
    use proto::jobworkerp::data::Worker;

    // Find worker by name
    let worker = job_executor_wrapper
        .worker_app()
        .find_by_name(worker_name)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to find worker '{}': {:#?}", worker_name, e))?
        .ok_or_else(|| anyhow::anyhow!("Worker '{}' not found", worker_name))?;

    let (wid, worker_data) = match worker {
        Worker {
            id: Some(wid),
            data: Some(worker_data),
        } => (wid, worker_data),
        _ => {
            return Err(anyhow::anyhow!(
                "Worker '{}' has no id or data",
                worker_name
            ))
        }
    };

    // Find runner for this worker
    let runner = job_executor_wrapper
        .runner_app()
        .find_runner(
            worker_data
                .runner_id
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Worker '{}' has no runner_id", worker_name))?,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to find runner for worker '{}': {:#?}",
                worker_name,
                e
            )
        })?;

    let (rid, rdata) = match runner {
        Some(RunnerWithSchema {
            id: Some(rid),
            data: Some(rdata),
            ..
        }) => (rid, rdata),
        _ => {
            return Err(anyhow::anyhow!(
                "Runner for worker '{}' not found",
                worker_name
            ))
        }
    };

    // Transform job args
    let job_args = job_executor_wrapper
        .transform_job_args(&rid, &rdata, &args, using.as_deref())
        .await?;

    // Enqueue with streaming - stream is not used here, we use listen_result_by_job_id later
    let (job_id, _job_result, _stream) = job_executor_wrapper
        .enqueue_with_worker_or_temp(
            metadata,
            Some(wid),
            worker_data,
            job_args,
            None, // uniq_key
            timeout_sec,
            StreamingType::Internal,
            using.clone(),
        )
        .await?;

    // Get RunnerSpec for stream collection
    let runner_spec = job_executor_wrapper
        .runner_spec_factory()
        .create_runner_spec_by_name(&rdata.name, false)
        .await;

    Ok(StreamingJobHandle {
        job_id,
        runner_id: rid,
        runner_name: rdata.name.clone(),
        runner_data: rdata,
        worker_name: worker_name.to_string(),
        runner_spec,
        using,
        timeout_sec,
    })
}

/// Start a runner streaming job and return handle with job_id (static version for use in stream)
/// Uses NoResult response_type to avoid waiting for job completion, allowing StreamingJobStarted
/// to be yielded immediately after enqueue. Stream is obtained via listen_result_by_job_id.
#[allow(clippy::too_many_arguments)]
async fn start_runner_streaming_job_static(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    metadata: Arc<HashMap<String, String>>,
    timeout_sec: u32,
    runner_name: &str,
    settings: Option<serde_json::Value>,
    options: Option<workflow::WorkerOptions>,
    args: serde_json::Value,
    task_name: &str,
    using: Option<String>,
) -> Result<StreamingJobHandle> {
    use app::app::job::execute::{UseJobExecutor, UseRunnerSpecFactory};
    use app::app::runner::UseRunnerApp;
    use infra::infra::runner::rows::RunnerWithSchema;

    let runner = job_executor_wrapper
        .runner_app()
        .find_runner_by_name(runner_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("Runner '{}' not found", runner_name))?;

    let (rid, rdata) = match &runner {
        RunnerWithSchema {
            id: Some(rid),
            data: Some(rdata),
            ..
        } => (*rid, rdata.clone()),
        _ => {
            return Err(anyhow::anyhow!(
                "Runner '{}' has no id or data",
                runner_name
            ))
        }
    };

    // Setup runner settings
    let runner_settings = job_executor_wrapper
        .setup_runner_and_settings(&runner, settings)
        .await?;

    // Create temporary worker for streaming
    // Use NoResult response_type to avoid waiting for job completion
    // This allows StreamingJobStarted to be yielded immediately after enqueue
    let worker_name = format!("{}_streaming_{}", task_name, runner_name);
    let mut worker_data =
        RunStreamTaskExecutor::function_options_to_worker_data(options, &worker_name).unwrap_or(
            WorkerData {
                name: worker_name.clone(),
                description: String::new(),
                runner_id: None,
                runner_settings: vec![],
                periodic_interval: 0,
                channel: None,
                queue_type: QueueType::Normal as i32,
                response_type: ResponseType::NoResult as i32,
                store_success: true,
                store_failure: true,
                use_static: false,
                retry_policy: None,
                broadcast_results: true,
            },
        );
    worker_data.runner_id = Some(rid);
    worker_data.runner_settings = runner_settings;
    // Override response_type and store settings for streaming
    worker_data.response_type = ResponseType::NoResult as i32;
    worker_data.store_success = true;
    worker_data.store_failure = true;
    worker_data.broadcast_results = true;

    // Enqueue with streaming - stream is not used here, we use listen_result_by_job_id later
    // setup_worker_and_enqueue_with_json_full_output handles transform_job_args internally
    let (job_id, _job_result, _stream) = job_executor_wrapper
        .setup_worker_and_enqueue_with_json_full_output(
            metadata,
            runner_name,
            worker_data.clone(),
            args,
            None, // uniq_key
            timeout_sec,
            StreamingType::Internal,
            using.clone(),
        )
        .await?;

    // Get RunnerSpec for stream collection
    let runner_spec = job_executor_wrapper
        .runner_spec_factory()
        .create_runner_spec_by_name(&rdata.name, false)
        .await;

    Ok(StreamingJobHandle {
        job_id,
        runner_id: rid,
        runner_name: rdata.name.clone(),
        runner_data: rdata,
        worker_name,
        runner_spec,
        using,
        timeout_sec,
    })
}

/// Collect streaming result from handle (static version for use in stream)
/// Uses listen_result_by_job_id to subscribe to the job result stream.
/// Falls back to JobResult.data.output if stream is unavailable (job already completed).
async fn collect_streaming_result_static(
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    handle: StreamingJobHandle,
) -> Result<serde_json::Value> {
    use app::app::job::execute::UseJobExecutor;
    use app::app::job_result::UseJobResultApp;

    // Subscribe to job result via listen_result_by_job_id
    let timeout = Some(handle.timeout_sec as u64 * 1000); // Convert to milliseconds
    let (job_result, stream_opt) = job_executor_wrapper
        .job_result_app()
        .listen_result_by_job_id(&handle.job_id, timeout, true)
        .await?;

    if let Some(stream) = stream_opt {
        // Stream is available - collect it
        // Get runner spec (use the one from handle if available, otherwise create new)
        let runner_spec = match handle.runner_spec {
            Some(spec) => spec,
            None => {
                use app::app::job::execute::UseRunnerSpecFactory;
                job_executor_wrapper
                    .runner_spec_factory()
                    .create_runner_spec_by_name(&handle.runner_name, false)
                    .await
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Failed to create RunnerSpec for runner: {}",
                            &handle.runner_name
                        )
                    })?
            }
        };

        // Collect stream using RunnerSpec::collect_stream
        let (collected_bytes, _metadata) = runner_spec.collect_stream(stream).await?;

        tracing::debug!(
            "Stream collected for job {}: {} bytes",
            handle.job_id.value,
            collected_bytes.len()
        );

        // Transform output to JSON
        job_executor_wrapper
            .transform_raw_output(
                &handle.runner_id,
                &handle.runner_data,
                collected_bytes.as_slice(),
                handle.using.as_deref(),
            )
            .await
    } else {
        // Stream unavailable (job already completed) - fallback to JobResult.data.output
        tracing::debug!(
            "No stream available for job {} (already completed), using JobResult.data.output as fallback",
            handle.job_id.value
        );

        if let Some(data) = &job_result.data {
            if let Some(output) = &data.output {
                if !output.items.is_empty() {
                    // Transform output to JSON
                    job_executor_wrapper
                        .transform_raw_output(
                            &handle.runner_id,
                            &handle.runner_data,
                            output.items.as_slice(),
                            handle.using.as_deref(),
                        )
                        .await
                } else {
                    // Output items is empty - return error or empty JSON
                    Err(anyhow::anyhow!(
                        "Job {} completed but output items is empty",
                        handle.job_id.value
                    ))
                }
            } else {
                Err(anyhow::anyhow!(
                    "Job {} completed but output is None",
                    handle.job_id.value
                ))
            }
        } else {
            Err(anyhow::anyhow!(
                "Job {} completed but has no result data",
                handle.job_id.value
            ))
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
