// Streaming version of RunTaskExecutor
// Separated from run.rs for better code organization

use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, RunJobRunner, RunJobWorker},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStreamEvent},
        expression::UseExpression,
        task::StreamTaskExecutorTrait,
    },
};
use anyhow::Result;
use app::app::job::execute::{JobExecutorWrapper, UseJobExecutor};
use async_stream::stream;
use command_utils::trace::Tracing;
use proto::jobworkerp::data::{
    JobId, QueueType, ResponseType, RunnerId, StreamingType, WorkerData,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;

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

    pub(super) fn function_options_to_worker_data(
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

        // Use a wrapper stream that first runs preparation, then chains event and completion streams
        stream! {
            let mut task_context = task_context;

            let workflow::RunTask {
                metadata: task_metadata,
                timeout,
                run,
                ..
            } = &task;

            // === 1. Timeout setting ===
            // Round up to at least 1 second to avoid immediate timeouts for sub-second durations
            let timeout_sec = if let Some(workflow::TaskTimeout::Timeout(duration)) = timeout {
                std::cmp::max(1, duration.after.to_millis().div_ceil(1000) as u32)
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

            // === 6. Prepare streaming ===
            let position = task_context.position.read().await.as_json_pointer();
            let job_id = handle.job_id.value;

            // === 7. Spawn background task for streaming and result collection ===
            // This architecture solves the async_stream blocking problem:
            // - Spawn a task that subscribes to Pub/Sub and sends events via channel
            // - Main stream yields events from channel (non-blocking)
            // - Background task also collects bytes for downstream workflow
            use app::app::job_result::UseJobResultApp;
            use proto::jobworkerp::data::result_output_item::Item;

            // NOTE: Using unbounded_channel for simplicity. In practice, LLM token generation
            // is slower than network transmission, so backpressure is unlikely. If OOM becomes
            // a concern under extreme load, consider switching to bounded channel with backpressure.
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Result<WorkflowStreamEvent, Box<workflow::Error>>>();
            let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Result<serde_json::Value, anyhow::Error>>();

            let job_executor_wrapper_clone = job_executor_wrapper.clone();
            let handle_clone = handle;
            let position_clone = position.clone();
            let task_context_clone = task_context.clone();

            tokio::spawn(async move {
                let timeout_ms = Some(handle_clone.timeout_sec as u64 * 1000);

                // First, emit StreamingJobStarted
                let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_job_started(
                    job_id,
                    &handle_clone.runner_name,
                    &handle_clone.worker_name,
                    &position_clone,
                )));

                // Subscribe to stream and forward events
                let output = match job_executor_wrapper_clone
                    .job_result_app()
                    .listen_result_by_job_id(&handle_clone.job_id, timeout_ms, true)
                    .await
                {
                    Ok((_job_result, Some(mut stream))) => {
                        use futures::StreamExt;
                        // FinalCollected contains the aggregated result from collect_stream()
                        // and should be used as the task output
                        let mut final_collected_bytes: Option<Vec<u8>> = None;
                        // Keep only the last Data chunk (consistent with runner's collect_stream())
                        let mut last_data_bytes: Option<Vec<u8>> = None;

                        while let Some(item) = stream.next().await {
                            match item.item {
                                Some(Item::Data(data)) => {
                                    // Keep only the last Data (consistent with workflow runner's collect_stream())
                                    // Each Data is forwarded to UI for real-time display
                                    let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_data(job_id, data.clone())));
                                    last_data_bytes = Some(data);
                                }
                                Some(Item::End(_trailer)) => {
                                    break;
                                }
                                Some(Item::FinalCollected(data)) => {
                                    // Use FinalCollected as the authoritative task output
                                    final_collected_bytes = Some(data);
                                }
                                None => {}
                            }
                        }

                        // Prefer FinalCollected (properly aggregated by runner), fall back to last Data
                        let output_bytes = final_collected_bytes.or(last_data_bytes).unwrap_or_else(|| {
                            tracing::warn!(
                                "No FinalCollected or Data received for job {}, using empty output",
                                job_id
                            );
                            Vec::new()
                        });

                        tracing::debug!(
                            "Stream collected for job {}: {} bytes",
                            job_id,
                            output_bytes.len()
                        );

                        // Transform collected bytes to JSON output
                        job_executor_wrapper_clone
                            .transform_raw_output(
                                &handle_clone.runner_id,
                                &handle_clone.runner_data,
                                output_bytes.as_slice(),
                                handle_clone.using.as_deref(),
                            )
                            .await
                    }
                    Ok((_job_result, None)) => {
                        tracing::warn!("No stream available for job {}, using fallback", job_id);
                        collect_streaming_result_static(&job_executor_wrapper_clone, handle_clone).await
                    }
                    Err(e) => {
                        Err(anyhow::anyhow!("Failed to subscribe to stream: {:?}", e))
                    }
                };

                // Send result back
                let _ = result_tx.send(output);

                // Signal end of events
                drop(event_tx);
            });

            // === 8. Yield events from channel (non-blocking) ===
            while let Some(event) = event_rx.recv().await {
                yield event;
            }

            // === 9. Get result and complete ===
            let output = match result_rx.await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => {
                    let pos = task_context_clone.position.read().await.as_error_instance();
                    yield Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to collect streaming result".to_string(),
                        Some(pos),
                        Some(format!("{e:?}")),
                    ));
                    return;
                }
                Err(_recv_err) => {
                    let pos = task_context_clone.position.read().await.as_error_instance();
                    yield Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Streaming result collection task was dropped".to_string(),
                        Some(pos),
                        None,
                    ));
                    return;
                }
            };
            task_context.set_raw_output(output);

            // === 10. Position cleanup ===
            task_context.remove_position().await; // "worker"/"runner"/"function"
            task_context.remove_position().await; // "run"

            // === 11. Emit StreamingJobCompleted event ===
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

    // Warn if worker has response_type=Direct which may cause blocking behavior
    if worker_data.response_type == proto::jobworkerp::data::ResponseType::Direct as i32 {
        tracing::warn!(
            "Worker '{}' has response_type=Direct which may block streaming. \
             Consider using runner-based streaming or setting response_type=NoResult for immediate event delivery.",
            worker_name
        );
    }

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
        let (collected_bytes, _metadata) = runner_spec
            .collect_stream(stream, handle.using.as_deref())
            .await?;

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
