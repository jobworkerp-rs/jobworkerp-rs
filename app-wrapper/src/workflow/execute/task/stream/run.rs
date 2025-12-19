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
use app::app::job::execute::JobExecutorWrapper;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
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

/// Preparation result containing everything needed for streaming
struct StreamingPreparation {
    handle: StreamingJobHandle,
    position: String,
    task_context: TaskContext,
    /// Stream subscription - subscribed immediately after job enqueue to avoid race condition
    stream: BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
}

impl StreamTaskExecutorTrait<'_> for RunStreamTaskExecutor {
    fn execute_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
        // Clone all required values
        let workflow_context = self.workflow_context.clone();
        let job_executor_wrapper = self.job_executor_wrapper.clone();
        let task = self.task.clone();
        let base_metadata = self.metadata.clone();
        let default_timeout = self.default_task_timeout;

        // Create channel for streaming events
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel::<
            Result<WorkflowStreamEvent, Box<workflow::Error>>,
        >();

        // Spawn a task that handles the entire execution
        // This avoids the stream! macro's blocking yield behavior
        tokio::spawn(async move {
            // Run preparation phase
            let prep_result = prepare_streaming_job(
                &workflow_context,
                &job_executor_wrapper,
                &task,
                &base_metadata,
                default_timeout,
                &cx,
                &task_name,
                task_context,
            )
            .await;

            let prep = match prep_result {
                Ok(p) => p,
                Err(e) => {
                    let _ = event_tx.send(Err(e));
                    return;
                }
            };

            let job_id = prep.handle.job_id.value;
            let position = prep.position.clone();
            let mut task_context = prep.task_context;
            // Stream is already subscribed in prepare_streaming_job to avoid race condition
            let stream = prep.stream;

            // Emit StreamingJobStarted immediately
            let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_job_started(
                job_id,
                &prep.handle.runner_name,
                &prep.handle.worker_name,
                &position,
            )));

            // Process stream and forward events
            let output = process_stream(
                stream,
                job_id,
                &prep.handle,
                &job_executor_wrapper,
                &event_tx,
            )
            .await;

            // Set output on task context
            match output {
                Ok(o) => {
                    task_context.set_raw_output(o);
                }
                Err(e) => {
                    tracing::error!("Failed to process stream for job {}: {:?}", job_id, e);
                    let _ = event_tx.send(Err(workflow::errors::ErrorFactory::new()
                        .service_unavailable(
                            "Failed to process streaming result".to_string(),
                            None,
                            Some(format!("{e:?}")),
                        )));
                    return;
                }
            }

            // Position cleanup
            task_context.remove_position().await; // "worker"/"runner"/"function"
            task_context.remove_position().await; // "run"

            // Emit StreamingJobCompleted
            let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_job_completed(
                job_id,
                None, // job_result_id
                &position,
                task_context,
            )));

            // Channel is dropped when this task ends, signaling end of stream
        });

        // Return stream from channel receiver
        tokio_stream::wrappers::UnboundedReceiverStream::new(event_rx)
    }
}

/// Prepare streaming job: evaluate expressions, start job, return handle
#[allow(clippy::too_many_arguments)]
async fn prepare_streaming_job(
    workflow_context: &Arc<RwLock<WorkflowContext>>,
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    task: &workflow::RunTask,
    base_metadata: &Arc<HashMap<String, String>>,
    default_timeout: Duration,
    cx: &Arc<opentelemetry::Context>,
    task_name: &Arc<String>,
    task_context: TaskContext,
) -> Result<StreamingPreparation, Box<workflow::Error>> {
    let workflow::RunTask {
        metadata: task_metadata,
        timeout,
        run,
        ..
    } = task;

    // === 1. Timeout setting ===
    let timeout_sec = if let Some(workflow::TaskTimeout::Timeout(duration)) = timeout {
        std::cmp::max(1, duration.after.to_millis().div_ceil(1000) as u32)
    } else {
        default_timeout.as_secs() as u32
    };

    // === 2. Metadata merge ===
    let mut metadata = (**base_metadata).clone();
    for (k, v) in task_metadata {
        if !metadata.contains_key(k) {
            if let Some(v) = v.as_str() {
                metadata.insert(k.clone(), v.to_string());
            }
        }
    }
    RunStreamTaskExecutor::inject_metadata_from_context(&mut metadata, cx);
    let metadata = Arc::new(metadata);

    // === 3. Position setup ===
    task_context.add_position_name("run".to_string()).await;

    // === 4. Expression evaluation ===
    let expression = match RunStreamTaskExecutor::expression(
        &*workflow_context.read().await,
        Arc::new(task_context.clone()),
    )
    .await
    {
        Ok(e) => e,
        Err(mut e) => {
            let pos = task_context.position.read().await;
            e.position(&pos);
            return Err(e);
        }
    };

    // === 5. Start job based on configuration ===
    let handle = match run {
        // Worker configuration
        workflow::RunTaskConfiguration::Worker(workflow::RunWorker {
            worker:
                RunJobWorker {
                    arguments,
                    name: worker_name,
                    using,
                },
        }) => {
            task_context.add_position_name("worker".to_string()).await;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            start_worker_streaming_job_static(
                job_executor_wrapper,
                metadata.clone(),
                worker_name,
                args,
                timeout_sec,
                using.clone(),
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to start streaming job by jobworkerp".to_string(),
                    Some(pos_for_err),
                    Some(format!("{e:?}")),
                )
            })?
        }

        // Function(WorkerFunction) configuration
        workflow::RunTaskConfiguration::Function(workflow::RunFunction {
            function:
                workflow::RunJobFunction::WorkerFunction {
                    arguments,
                    using,
                    worker_name,
                },
        }) => {
            task_context.add_position_name("function".to_string()).await;

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            start_worker_streaming_job_static(
                job_executor_wrapper,
                metadata.clone(),
                worker_name,
                args,
                timeout_sec,
                using.clone(),
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to start streaming job by jobworkerp".to_string(),
                    Some(pos_for_err),
                    Some(format!("{e:?}")),
                )
            })?
        }

        // Runner configuration
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

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            let transformed_settings = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                settings.clone(),
                &expression,
            ) {
                Ok(s) => s,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("settings".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            start_runner_streaming_job_static(
                job_executor_wrapper,
                metadata.clone(),
                timeout_sec,
                runner_name,
                Some(transformed_settings),
                options.clone(),
                args,
                task_name,
                using.clone(),
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to start streaming runner job by jobworkerp".to_string(),
                    Some(pos_for_err),
                    Some(format!("{e:?}")),
                )
            })?
        }

        // Function(RunnerFunction) configuration
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

            let args = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                arguments.clone(),
                &expression,
            ) {
                Ok(args) => args,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("arguments".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            let transformed_settings = match RunStreamTaskExecutor::transform_map(
                task_context.input.clone(),
                settings.clone(),
                &expression,
            ) {
                Ok(s) => s,
                Err(mut e) => {
                    task_context
                        .position
                        .write()
                        .await
                        .push("settings".to_string());
                    e.position(&*task_context.position.read().await);
                    return Err(e);
                }
            };

            // Capture position before await to avoid blocking_read() in async context
            let pos_for_err = task_context.position.read().await.as_error_instance();
            start_runner_streaming_job_static(
                job_executor_wrapper,
                metadata.clone(),
                timeout_sec,
                runner_name,
                Some(transformed_settings),
                options.clone(),
                args,
                task_name,
                using.clone(),
            )
            .await
            .map_err(|e| {
                workflow::errors::ErrorFactory::new().service_unavailable(
                    "Failed to start streaming runner job by jobworkerp".to_string(),
                    Some(pos_for_err),
                    Some(format!("{e:?}")),
                )
            })?
        }

        // Script configuration is not supported for streaming
        workflow::RunTaskConfiguration::Script(_) => {
            let pos = task_context.position.read().await.as_error_instance();
            return Err(workflow::errors::ErrorFactory::new().not_implemented(
                "RunStreamTaskExecutor does not support Script configurations".to_string(),
                Some(pos),
                Some("Use RunTaskExecutor for Script configurations".to_string()),
            ));
        }
    };

    // Capture position for both success and error cases
    let pos_for_err = task_context.position.read().await.as_error_instance();
    let position = task_context.position.read().await.as_json_pointer();

    // Subscribe to stream IMMEDIATELY after job enqueue to avoid race condition
    // This must happen before worker completes and publishes stream data
    use infra::infra::job_result::pubsub::JobResultSubscriber;
    let timeout_ms = Some(timeout_sec as u64 * 1000);

    // Try redis repository first (Scalable mode), then channel repository (Standalone mode)
    let stream =
        if let Some(pubsub_repo) = job_executor_wrapper.redis_job_result_pubsub_repository() {
            pubsub_repo
                .subscribe_result_stream(&handle.job_id, timeout_ms)
                .await
        } else if let Some(pubsub_repo) = job_executor_wrapper.chan_job_result_pubsub_repository() {
            pubsub_repo
                .subscribe_result_stream(&handle.job_id, timeout_ms)
                .await
        } else {
            Err(anyhow::anyhow!(
                "No pubsub repository available for streaming"
            ))
        }
        .map_err(|e| {
            workflow::errors::ErrorFactory::new().service_unavailable(
                "Failed to subscribe to result stream".to_string(),
                Some(pos_for_err),
                Some(format!("{e:?}")),
            )
        })?;

    Ok(StreamingPreparation {
        handle,
        position,
        task_context,
        stream,
    })
}

/// Process stream: forward events to channel and collect output
async fn process_stream(
    mut stream: BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>,
    job_id: i64,
    handle: &StreamingJobHandle,
    job_executor_wrapper: &Arc<JobExecutorWrapper>,
    event_tx: &tokio::sync::mpsc::UnboundedSender<
        Result<WorkflowStreamEvent, Box<workflow::Error>>,
    >,
) -> Result<serde_json::Value> {
    use app::app::job::execute::UseJobExecutor;
    use futures::StreamExt;
    use proto::jobworkerp::data::result_output_item::Item;

    let mut final_collected_bytes: Option<Vec<u8>> = None;
    let mut all_data_chunks: Vec<Vec<u8>> = Vec::new();

    // Use timeout for each stream item to prevent hanging indefinitely
    let item_timeout = std::time::Duration::from_secs(handle.timeout_sec as u64);

    loop {
        match tokio::time::timeout(item_timeout, stream.next()).await {
            Ok(Some(item)) => {
                match item.item {
                    Some(Item::Data(data)) => {
                        // Forward to UI for real-time display
                        let _ = event_tx.send(Ok(WorkflowStreamEvent::streaming_data(
                            job_id,
                            data.clone(),
                        )));
                        // Collect all chunks for later aggregation
                        all_data_chunks.push(data);
                    }
                    Some(Item::End(_trailer)) => {
                        tracing::debug!("Stream ended for job {}", job_id);
                        break;
                    }
                    Some(Item::FinalCollected(data)) => {
                        // Use FinalCollected as the authoritative task output
                        final_collected_bytes = Some(data);
                    }
                    None => {}
                }
            }
            Ok(None) => {
                // Stream ended without End message
                tracing::debug!("Stream closed without End message for job {}", job_id);
                break;
            }
            Err(_) => {
                // Timeout waiting for next item
                tracing::warn!(
                    "Timeout waiting for stream item for job {} ({}s), ending stream processing",
                    job_id,
                    handle.timeout_sec
                );
                break;
            }
        }
    }

    // Prefer FinalCollected (properly aggregated by runner)
    // Otherwise, use runner_spec.collect_stream to properly aggregate Data chunks
    let output_bytes = if let Some(bytes) = final_collected_bytes {
        tracing::debug!("Using FinalCollected for job {}", job_id);
        bytes
    } else if !all_data_chunks.is_empty() {
        // No FinalCollected - use runner_spec to aggregate chunks
        if let Some(runner_spec) = handle.runner_spec.as_ref() {
            tracing::debug!(
                "Using runner_spec.collect_stream for job {} ({} chunks)",
                job_id,
                all_data_chunks.len()
            );
            // Create a stream from collected chunks
            let chunk_stream = futures::stream::iter(all_data_chunks.into_iter().map(|data| {
                proto::jobworkerp::data::ResultOutputItem {
                    item: Some(Item::Data(data)),
                }
            }))
            .chain(futures::stream::once(async {
                proto::jobworkerp::data::ResultOutputItem {
                    item: Some(Item::End(proto::jobworkerp::data::Trailer::default())),
                }
            }));
            match runner_spec
                .collect_stream(Box::pin(chunk_stream), handle.using.as_deref())
                .await
            {
                Ok((bytes, _metadata)) => bytes,
                Err(e) => {
                    tracing::warn!("Failed to collect stream for job {}: {:?}", job_id, e);
                    Vec::new()
                }
            }
        } else {
            // No runner_spec available - concatenate all chunks as fallback
            // This may not produce semantically correct output for all runner types,
            // but preserves all data rather than losing chunks
            tracing::warn!(
                "No runner_spec available for job {}, concatenating {} chunks as fallback",
                job_id,
                all_data_chunks.len()
            );
            all_data_chunks.concat()
        }
    } else {
        tracing::warn!(
            "No FinalCollected or Data received for job {}, using empty output",
            job_id
        );
        Vec::new()
    };

    tracing::debug!(
        "Stream collected for job {}: {} bytes",
        job_id,
        output_bytes.len()
    );

    // Transform collected bytes to JSON output
    job_executor_wrapper
        .transform_raw_output(
            &handle.runner_id,
            &handle.runner_data,
            output_bytes.as_slice(),
            handle.using.as_deref(),
        )
        .await
}

/// Start a worker streaming job and return handle with job_id
///
/// For streaming jobs, we need to return immediately after enqueue to subscribe
/// to stream before any data is published. Therefore, even if the worker has
/// response_type=Direct, we override it to NoResult for enqueue to avoid blocking.
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

    let (_wid, worker_data) = match worker {
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

    // For streaming jobs, use the existing worker (with worker_id) to preserve pooling.
    // This is critical for heavy resources like local LLMs where use_static=true enables
    // runner instance reuse without re-initialization.
    //
    // StreamingType::Internal is used here because:
    // 1. We want the runner to use run_stream() internally for streaming output
    // 2. The app layer returns immediately (even for Direct response_type workers)
    //    allowing us to subscribe to the stream before data is published
    // 3. The final result is collected via collect_stream() and sent as FinalCollected
    // 4. Workflow steps receive a single aggregated result, not raw stream chunks
    let (job_id, _job_result, _stream) = job_executor_wrapper
        .enqueue_with_worker_or_temp(
            metadata,
            Some(_wid), // Use existing worker to preserve pooling (use_static)
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

/// Start a runner streaming job and return handle with job_id
/// Uses NoResult response_type to avoid waiting for job completion, allowing
/// stream subscription immediately after enqueue.
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

    // Enqueue with streaming - returns immediately (NoResult)
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
