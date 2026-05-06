use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, ForOnError, tasks::TaskTrait},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStatus, WorkflowStreamEvent},
        expression::UseExpression,
        task::{
            ExecutionId, StreamTaskExecutorTrait, stream::do_::DoTaskStreamExecutor,
            trace::TaskTracing,
        },
    },
};
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use command_utils::trace::Tracing;
use futures::stream::{self, Stream, StreamExt};
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::trace::TraceContextExt;
use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{Mutex, RwLock, mpsc};

pub struct ForTaskStreamExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_timeout: Duration,
    task: workflow::ForTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    checkpoint_repository: Option<
        Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
    >,
    execution_id: Option<Arc<ExecutionId>>,
    // Metadata for the task, can be used for logging or tracing
    metadata: Arc<HashMap<String, String>>,
    emit_streaming_data: bool,
}
impl UseExpression for ForTaskStreamExecutor {}
impl UseJqAndTemplateTransformer for ForTaskStreamExecutor {}
impl UseExpressionTransformer for ForTaskStreamExecutor {}
impl Tracing for ForTaskStreamExecutor {}
impl TaskTracing for ForTaskStreamExecutor {}

impl ForTaskStreamExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        workflow_context: Arc<RwLock<WorkflowContext>>,
        default_timeout: Duration,
        task: workflow::ForTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<HashMap<String, String>>,
        emit_streaming_data: bool,
    ) -> Self {
        Self {
            workflow_context,
            default_timeout,
            task,
            job_executor_wrapper,
            checkpoint_repository,
            execution_id,
            metadata,
            emit_streaming_data,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_for_item(
        &self,
        item: &serde_json::Value,
        i: usize,
        item_name: &str,
        index_name: &str,
        task_context: &TaskContext,
        while_: &Option<String>,
        copy_deeply: bool,
    ) -> Result<(TaskContext, serde_json::Value), Box<workflow::Error>> {
        // For parallel execution, create deep copy to ensure isolation.
        // For sequential execution, share context_variables so the final state
        // accumulates across iterations, but isolate `position` so the inner
        // task's pushes don't pile up across iterations (e.g. when try.catch
        // swallows an error via onError=continue without popping back).
        let task_context = if copy_deeply {
            task_context.deep_copy().await
        } else {
            task_context.clone_with_isolated_position().await
        };

        task_context
            .add_context_value(item_name.to_string(), item.clone())
            .await;
        task_context
            .add_context_value(
                index_name.to_string(),
                serde_json::Value::Number(serde_json::Number::from(i)),
            )
            .await;

        let expression = match Self::expression(
            &*(self.workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let pos = pos.read().await;
                e.position(&pos);
                return Err(e);
            }
        };
        let input = task_context.input.clone();
        let while_cond = match while_
            .as_ref()
            .map(|w| {
                Self::transform_value(input, serde_json::Value::String(w.clone()), &expression)
            })
            .unwrap_or(Ok(serde_json::Value::Bool(true)))
        {
            Ok(cond) => cond,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let mut pos = pos.write().await;
                pos.push("while".to_string());
                e.position(&pos);
                return Err(e);
            }
        };

        Ok((task_context, while_cond))
    }

    // Wrap a per-item failure as an Ok(TaskCompleted) event so onError=continue
    // doesn't abort the whole workflow. The error is preserved inside the
    // event's output payload for downstream observability.
    async fn build_item_error_event(
        task_name: &str,
        error: &workflow::Error,
        index: usize,
        item: &serde_json::Value,
        base_context: &TaskContext,
    ) -> WorkflowStreamEvent {
        let mut ctx = base_context.clone_with_isolated_position().await;
        let error_json = serde_json::to_value(error)
            .unwrap_or_else(|_| serde_json::json!({ "detail": error.to_string() }));
        let payload = serde_json::json!({
            "error": error_json,
            "index": index,
            "item": item,
        });
        ctx.set_raw_output(payload);
        let pos = ctx.position.read().await.as_json_pointer();
        WorkflowStreamEvent::task_completed_with_position("forTask", task_name, &pos, ctx)
    }

    async fn initialize_execution(
        &self,
        task_name: &str,
        task_context: TaskContext,
    ) -> Result<(TaskContext, serde_json::Value, workflow::DoTask), Box<workflow::Error>> {
        tracing::debug!("ForStreamTaskExecutor: {}", task_name);
        let workflow::ForTask {
            for_,
            do_,
            metadata,
            ..
        } = &self.task;

        task_context
            .add_position_name(self.task.task_type().to_string())
            .await;

        let expression = match Self::expression(
            &*(self.workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let pos = pos.read().await;
                e.position(&pos);
                return Err(e);
            }
        };

        let transformed_in_items = match Self::transform_value(
            task_context.input.clone(),
            serde_json::Value::String(for_.in_.clone()),
            &expression,
        ) {
            Ok(items) => items,
            Err(mut e) => {
                let pos = task_context.position.clone();
                let mut pos = pos.write().await;
                pos.push("in".to_string());
                e.position(&pos);
                return Err(e);
            }
        };

        let do_task = workflow::DoTask {
            do_: do_.clone(),
            metadata: metadata.clone(),
            ..Default::default()
        };

        tracing::debug!("for in items: {:#?}", transformed_in_items);

        Ok((task_context, transformed_in_items, do_task))
    }

    // Process items in parallel and return a real-time stream of results
    #[allow(clippy::too_many_arguments)]
    async fn process_items_in_parallel_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        items: &[serde_json::Value],
        item_name: &str,
        index_name: &str,
        task_context: &TaskContext,
        do_task: workflow::DoTask,
        task_name: &str,
        while_: &Option<String>,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send>>,
        Box<workflow::Error>,
    > {
        tracing::debug!(
            "[FOR]Processing {} items in parallel (streaming)",
            items.len()
        );
        let original_context = task_context.clone();

        // Larger buffer to reduce back-pressure
        let (tx, rx) = mpsc::channel(128);

        // Channel for cancelling all tasks if onError: break and an error occurs
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let cancel_tx = Arc::new(cancel_tx);

        let mut has_items = false;
        // Track prepare-time DSL failures so we don't take the empty-stream early-return
        // path (which would silently swallow the error sent into `tx`).
        let mut prepare_error_pending = false;

        // First pass - prepare all items and determine which ones should be processed
        let mut items_to_process = Vec::new();

        for (i, item) in items.iter().enumerate() {
            match self
                .prepare_for_item(item, i, item_name, index_name, task_context, while_, true)
                .await
            {
                Ok((prepared_context, while_cond)) => {
                    if Self::eval_as_bool(&while_cond) {
                        has_items = true;
                        // Carry the raw item value alongside the prepared context so the
                        // spawned task can include it in onError=continue error events.
                        items_to_process.push((i, prepared_context, item.clone()));
                    } else {
                        tracing::debug!("for: while condition is false, skipping item {}", i);
                        break;
                    }
                }
                Err(e) => {
                    // prepare_for_item failures are DSL-level errors (broken jq expression
                    // or malformed `while` clause), not per-item runtime errors. They are
                    // outside the scope of `onError: continue`. Propagate the error so the
                    // workflow faults; the spawn loop below will skip launching the items
                    // that were already prepared, so no per-item side effects fire.
                    tracing::error!(
                        "Error preparing item {} (DSL/expression error, faulting regardless of onError): {:?}",
                        i,
                        e
                    );
                    prepare_error_pending = true;
                    let _ = cancel_tx.send(true);
                    let tx_err = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx_err.send(Err(e)).await;
                    });
                    break;
                }
            };
        }

        // If no items to process AND no prepare error is pending, short-circuit with the
        // empty-completion event. When a prepare error was already queued onto `tx`,
        // fall through to the receiver-driven path so that error reaches the consumer.
        if !has_items && !prepare_error_pending {
            let mut final_ctx = original_context;
            final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
            final_ctx.remove_context_value(item_name).await;
            final_ctx.remove_context_value(index_name).await;
            final_ctx.remove_position().await;
            let task_name = task_name.to_string();
            return Ok(stream::once(async move {
                Ok(WorkflowStreamEvent::task_completed(
                    "forTask", &task_name, final_ctx,
                ))
            })
            .boxed());
        }

        // Log detailed parallel execution information
        tracing::debug!(
            "Starting parallel execution of {} tasks with {} worker threads",
            items_to_process.len(),
            tokio::runtime::Handle::current().metrics().num_workers()
        );
        let mut join_set = tokio::task::JoinSet::new();
        // If a prepare-time DSL error was queued, do NOT launch the items that
        // were prepared successfully before the failure. Spawning them and
        // relying on `cancel_tx.send(true)` to stop them is racy: tokio's
        // `select!` resolves a ready watch-cancel against a ready stream poll
        // pseudo-randomly, so the stream side can win and the item executes
        // its side effects even though we already decided to fault the
        // workflow. Skipping the spawn entirely makes the failure deterministic.
        let items_to_spawn = if prepare_error_pending {
            Vec::new()
        } else {
            items_to_process
        };
        for (i, prepared_context, item_value) in items_to_spawn {
            // Clone all resources needed for this task
            let tx = tx.clone();
            let do_task_clone = do_task.clone();
            let job_executor_wrapper_clone = self.job_executor_wrapper.clone();
            let workflow_context = self.workflow_context.clone();
            let task_name_formatted = Arc::new(format!("{task_name}_{i}"));
            let item_name_clone = item_name.to_string();
            let cx = cx.clone();
            let meta = self.metadata.clone();
            let checkpoint_repository = self.checkpoint_repository.clone();
            let execution_id = self.execution_id.clone();
            let default_timeout = self.default_timeout;
            let on_error = self.task.on_error;
            let cancel_tx_clone = cancel_tx.clone();
            let mut cancel_rx_clone = cancel_rx.clone();
            let emit_streaming_data = self.emit_streaming_data;
            let item_value_for_err = item_value.clone();
            let task_name_for_err = task_name.to_string();
            let original_ctx_for_err = original_context.clone();

            // Spawn this task asynchronously
            join_set.spawn(async move {
                let span =
                    Self::start_child_otel_span(&cx, APP_WORKER_NAME, item_name_clone.clone());
                let ccx = Arc::new(opentelemetry::Context::current_with_span(span));
                let ccx_clone = ccx.clone();
                let mut span = ccx_clone.span();
                // Record per-iteration metadata (item value + index) on the
                // branch span. The inner do-task spans record their own real
                // input via TaskExecutor::execute, so we deliberately avoid
                // setting `workflow.task.input` here (it would otherwise carry
                // the for-task-level input, i.e. the entire array).
                Self::record_for_item(
                    &mut span,
                    format!(
                        "for_parallel_task:{}_{}",
                        &task_name_formatted, &item_name_clone
                    ),
                    &item_value,
                    i,
                    prepared_context.position.read().await.as_json_pointer(),
                );

                let start_time = std::time::Instant::now();

                tracing::debug!("[PARALLEL] Task {} starting at t=0ms", &task_name_formatted);

                let do_stream_executor = DoTaskStreamExecutor::new(
                    workflow_context.clone(),
                    default_timeout,
                    meta.clone(),
                    do_task_clone,
                    job_executor_wrapper_clone,
                    checkpoint_repository.clone(),
                    execution_id,
                    emit_streaming_data,
                );

                // Save position for potential Wait error reporting
                let item_position = prepared_context.position.clone();

                // Execute the stream and track results
                let stream = do_stream_executor
                    .execute_stream(ccx, task_name_formatted.clone(), prepared_context)
                    .boxed();

                tokio::pin!(stream);

                let mut result_count = 0;
                loop {
                    tokio::select! {
                        _ = cancel_rx_clone.changed() => {
                            tracing::info!("Task {} cancelled due to error in sibling task", task_name_formatted);
                            break;
                        }
                        // Process next result from stream
                        maybe_result = stream.next() => {
                            match maybe_result {
                                Some(result) => {
                                    result_count += 1;
                                    let elapsed_ms = start_time.elapsed().as_millis();

                                    tracing::debug!(
                                        "[PARALLEL] Task {} yielding result #{} at t={}ms",
                                        task_name_formatted.clone(),
                                        result_count,
                                        elapsed_ms
                                    );

                                    // Check for unsupported Wait inside ForTask (parallel).
                                    // This is a structural DSL error, not a per-item runtime
                                    // failure, so it must NOT be swallowed by onError=continue.
                                    // Send the Err directly and stop draining this branch, so
                                    // the workflow faults the same way the sequential and
                                    // post-stream-end paths do.
                                    if let Ok(ref event) = result {
                                        let wf_status = workflow_context.read().await.status.clone();
                                        if wf_status == WorkflowStatus::Waiting {
                                            tracing::error!(
                                                "Wait directive inside ForTask is not supported (parallel): task={}",
                                                task_name_formatted
                                            );
                                            workflow_context.write().await.status = WorkflowStatus::Running;
                                            let pos_instance = if let Some(ctx) = event.context() {
                                                Some(ctx.position.read().await.as_error_instance())
                                            } else {
                                                None
                                            };
                                            let error = workflow::errors::ErrorFactory::new().bad_argument(
                                                "Wait directive inside ForTask is not currently supported. \
                                                 Move the 'then: wait' to a task outside of the for loop.".to_string(),
                                                pos_instance,
                                                None,
                                            );
                                            // Cancel siblings so the whole for-task tears down
                                            // promptly, regardless of onError policy.
                                            let _ = cancel_tx_clone.send(true);
                                            Self::record_error(&span, &error.to_string());
                                            let _ = tx.send(Err(error)).await;
                                            break;
                                        }
                                    }

                                    // On error, react per onError policy: Break signals cancel
                                    // and propagates Err; Continue records the error on the
                                    // span and wraps the Err into an Ok(TaskCompleted) so
                                    // upstream stream consumers do not treat it as a fatal
                                    // abort. The span status set by record_error stays put
                                    // because record_result no longer overwrites it on Ok.
                                    let result = match result {
                                        Ok(event) => Ok(event),
                                        Err(e) => match on_error {
                                            ForOnError::Break => {
                                                let _ = cancel_tx_clone.send(true);
                                                tracing::warn!("Error occurred, cancelling all parallel tasks due to onError=break");
                                                Err(e)
                                            }
                                            ForOnError::Continue => {
                                                Self::record_error(&span, &e.to_string());
                                                tracing::warn!(
                                                    "Continuing past error in parallel for-item {} due to onError=continue",
                                                    i
                                                );
                                                let event = ForTaskStreamExecutor::build_item_error_event(
                                                    &task_name_for_err,
                                                    &e,
                                                    i,
                                                    &item_value_for_err,
                                                    &original_ctx_for_err,
                                                )
                                                .await;
                                                Ok(event)
                                            }
                                        },
                                    };
                                    let is_error = result.is_err();

                                    Self::record_result(&span, result.as_ref());
                                    if tx.send(result).await.is_err() {
                                        tracing::error!(
                                            "Channel closed while sending results for task {}",
                                            &task_name_formatted
                                        );
                                        Self::record_error(&span, "Channel closed while sending results");
                                        break;
                                    }

                                    // If error and break mode, stop this task
                                    if is_error && on_error == ForOnError::Break {
                                        break;
                                    }
                                }
                                None => {
                                    // Stream completed - check for Wait state.
                                    // Wait inside ForTask is a structural DSL
                                    // error, not a per-item runtime failure, so
                                    // it must NOT honour onError=continue here:
                                    // cancel siblings unconditionally to mirror
                                    // the in-stream Wait detection above.
                                    let wf_status = workflow_context.read().await.status.clone();
                                    if wf_status == WorkflowStatus::Waiting {
                                        tracing::error!(
                                            "Wait directive inside ForTask is not supported (parallel, detected after stream end): task={}",
                                            task_name_formatted
                                        );
                                        workflow_context.write().await.status = WorkflowStatus::Running;
                                        let error = workflow::errors::ErrorFactory::new().bad_argument(
                                            "Wait directive inside ForTask is not currently supported. \
                                             Move the 'then: wait' to a task outside of the for loop.".to_string(),
                                            Some(item_position.read().await.as_error_instance()),
                                            None,
                                        );
                                        let _ = cancel_tx_clone.send(true);
                                        Self::record_error(&span, &error.to_string());
                                        let _ = tx.send(Err(error)).await;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }

                let elapsed_ms = start_time.elapsed().as_millis();
                tracing::debug!(
                    "[PARALLEL] Task {} completed in {}ms with {} results",
                    &task_name_formatted,
                    elapsed_ms,
                    result_count
                );
            });
        }
        drop(tx);

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let join_set = Arc::new(Mutex::new(join_set));
        let item_name = item_name.to_string();
        let index_name = index_name.to_string();
        let original_context = original_context.clone();
        let task_name = task_name.to_string();

        let final_stream = stream
            .chain(stream::once(async move {
                // XXX workaround: wait for last result sent
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

                // Wait for all tasks to complete before sending the final cleanup
                let mut set = join_set.lock().await;
                while let Some(result) = set.join_next().await {
                    let _ = result;
                }

                // cleanup
                let final_ctx = original_context;
                final_ctx.remove_context_value(&item_name).await;
                final_ctx.remove_context_value(&index_name).await;
                final_ctx.remove_position().await;

                Ok(WorkflowStreamEvent::task_completed(
                    "forTask", &task_name, final_ctx,
                ))
            }))
            .boxed();

        Ok(final_stream)
    }

    // Process items sequentially and return a real-time stream of results
    // This function streams each result in real-time as it becomes available
    // and processes items in sequential order (unlike parallel processing)
    #[allow(clippy::too_many_arguments)]
    async fn process_items_sequentially_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        items: Vec<serde_json::Value>,
        item_name: String,
        index_name: String,
        task_context: TaskContext,
        do_task: workflow::DoTask,
        task_name: String,
        while_: Option<String>,
        original_context: TaskContext, // Original context for finalizing
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send + '_>>,
        Box<workflow::Error>,
    > {
        tracing::debug!(
            "[FOR] Processing {} items sequentially (streaming)",
            items.len()
        );
        // No items to process, return a stream with just the final result
        if items.is_empty() {
            let mut final_ctx = original_context;
            final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
            final_ctx.remove_context_value(&item_name).await;
            final_ctx.remove_context_value(&index_name).await;
            final_ctx.remove_position().await;
            return Ok::<
                Pin<
                    Box<
                        dyn Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send,
                    >,
                >,
                Box<workflow::Error>,
            >(
                stream::once(async move {
                    Ok(WorkflowStreamEvent::task_completed(
                        "forTask", &task_name, final_ctx,
                    ))
                })
                .boxed(),
            );
        }

        let on_error = self.task.on_error;
        let mut has_items = false;

        let stream = Box::pin(async_stream::stream! {
            // Process each item completely before moving to the next (true sequential execution)
            for (i, item) in items.iter().enumerate() {
                // Prepare each item individually and execute immediately
                match self
                    .prepare_for_item(item, i, &item_name, &index_name, &task_context, &while_, false)
                    .await
                {
                    Ok((prepared_context, while_cond)) => {
                        if !Self::eval_as_bool(&while_cond) {
                            tracing::debug!("for: while condition is false, skipping item {}", i);
                            break;
                        }
                        has_items = true;

                        let span = Self::start_child_otel_span(
                            &cx.clone(),
                            APP_WORKER_NAME,
                            format!("for_task_{task_name}:{item_name}_{i}"),
                        );
                        let item_cx = Arc::new(opentelemetry::Context::current_with_span(span));
                        // Mirror the parallel branch: tag this iteration's span
                        // with item/index so traces can identify which value of
                        // `for.in` this branch corresponds to. The inner do/run
                        // task spans record their own real input.
                        {
                            let mut span_ref = item_cx.span();
                            Self::record_for_item(
                                &mut span_ref,
                                format!("for_task:{task_name}:{item_name}_{i}"),
                                item,
                                i,
                                prepared_context.position.read().await.as_json_pointer(),
                            );
                        }

                        let task_name_formatted = Arc::new(format!("{task_name}_{i}"));
                        let do_stream_executor = DoTaskStreamExecutor::new(
                            self.workflow_context.clone(),
                            self.default_timeout,
                            self.metadata.clone(),
                            do_task.clone(),
                            self.job_executor_wrapper.clone(),
                            self.checkpoint_repository.clone(),
                            self.execution_id.clone(),
                            self.emit_streaming_data,
                        );

                        // Save position for potential Wait error reporting
                        let item_position = prepared_context.position.clone();

                        // Execute this item's stream completely before moving to next
                        let mut item_stream = do_stream_executor.execute_stream(
                            item_cx,
                            task_name_formatted,
                            prepared_context,
                        );

                        // Process all results from this item before continuing
                        while let Some(result) = item_stream.next().await {
                            match result {
                                Ok(event) => {
                                    // Check for unsupported Wait inside ForTask
                                    let wf_status = self.workflow_context.read().await.status.clone();
                                    if wf_status == WorkflowStatus::Waiting {
                                        tracing::error!(
                                            "Wait directive inside ForTask is not supported: task={}",
                                            task_name
                                        );
                                        // Reset status to Running to allow error propagation
                                        self.workflow_context.write().await.status = WorkflowStatus::Running;
                                        let pos_instance = if let Some(ctx) = event.context() {
                                            Some(ctx.position.read().await.as_error_instance())
                                        } else {
                                            None
                                        };
                                        let error = workflow::errors::ErrorFactory::new().bad_argument(
                                            "Wait directive inside ForTask is not currently supported. \
                                             Move the 'then: wait' to a task outside of the for loop.".to_string(),
                                            pos_instance,
                                            None,
                                        );
                                        yield Err(error);
                                        return;
                                    }
                                    yield Ok(event);
                                }
                                Err(e) => {
                                    tracing::error!("Error executing task in sequential for loop: {:?}", e);

                                    match on_error {
                                        ForOnError::Continue => {
                                            // Surface the failed item as a TaskCompleted event carrying
                                            // error info, so observers can see the failure without the
                                            // upstream stream consumer treating Err as a fatal abort.
                                            tracing::warn!("Continuing to next item due to onError=continue");
                                            let event = Self::build_item_error_event(
                                                &task_name, &e, i, item, &task_context,
                                            ).await;
                                            yield Ok(event);
                                            // Stop draining this item's stream and advance to next.
                                            break;
                                        }
                                        ForOnError::Break => {
                                            // Stop processing all remaining items
                                            yield Err(e);
                                            return;
                                        }
                                    }
                                }
                            }
                        }

                        // Check for Wait after stream ended (DoTaskStreamExecutor ends stream on wait)
                        let wf_status = self.workflow_context.read().await.status.clone();
                        if wf_status == WorkflowStatus::Waiting {
                            tracing::error!(
                                "Wait directive inside ForTask is not supported (detected after stream end): task={}",
                                task_name
                            );
                            // Reset status to Running to allow error propagation
                            self.workflow_context.write().await.status = WorkflowStatus::Running;
                            let error = workflow::errors::ErrorFactory::new().bad_argument(
                                "Wait directive inside ForTask is not currently supported. \
                                 Move the 'then: wait' to a task outside of the for loop.".to_string(),
                                Some(item_position.read().await.as_error_instance()),
                                None,
                            );
                            yield Err(error);
                            return;
                        }
                    }
                    Err(e) => {
                        // prepare_for_item failures are DSL-level errors (broken jq expression
                        // or malformed `while` clause), not per-item runtime errors. They are
                        // outside the scope of `onError: continue`, which only governs failures
                        // inside the iteration body. Always fault here.
                        tracing::error!(
                            "Error preparing item {} (DSL/expression error, faulting regardless of onError): {:?}",
                            i, e
                        );
                        yield Err(e);
                        return;
                    }
                }
            }

            // No items were processed
            if !has_items {
                let mut final_ctx = original_context;
                final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
                final_ctx.remove_context_value(&item_name).await;
                final_ctx.remove_context_value(&index_name).await;
                let pos_str = final_ctx.position.read().await.as_json_pointer();
                final_ctx.remove_position().await;
                yield Ok(WorkflowStreamEvent::task_completed_with_position("forTask", &task_name, &pos_str, final_ctx));
            } else {
                // Final cleanup
                let final_ctx = original_context;
                final_ctx.remove_context_value(&item_name).await;
                final_ctx.remove_context_value(&index_name).await;
                let pos_str = final_ctx.position.read().await.as_json_pointer();
                final_ctx.remove_position().await;
                yield Ok(WorkflowStreamEvent::task_completed_with_position("forTask", &task_name, &pos_str, final_ctx));
            }
        });

        Ok(stream)
    }
}

impl StreamTaskExecutorTrait<'_> for ForTaskStreamExecutor {
    fn execute_stream(
        &self,
        cx: Arc<opentelemetry::Context>,
        task_name: Arc<String>,
        task_context: TaskContext,
    ) -> impl futures::Stream<Item = Result<WorkflowStreamEvent, Box<workflow::Error>>> + Send {
        let this = self;
        let task_name = task_name.to_string();

        Box::pin(async_stream::stream! {
            // Initialize execution
            let init_result = this
                .initialize_execution(&task_name, task_context)
                .await;

            // Handle initialization errors
            match init_result {
                Ok((task_context, transformed_in_items, do_task)) => {
                    // Handle invalid (non-array) input
                    if !transformed_in_items.is_array() {
                        tracing::warn!(
                            "Invalid for 'in' items(not array): {:#?}",
                            transformed_in_items
                        );
                        let mut final_ctx = task_context;
                        final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
                        let pos_str = final_ctx.position.read().await.as_json_pointer();
                        final_ctx.remove_position().await;

                        yield Ok(WorkflowStreamEvent::task_completed_with_position("forTask", &task_name, &pos_str, final_ctx));
                        return;
                    }

                    let workflow::ForTask {
                        for_,
                        while_,
                        in_parallel,
                        ..
                    } = &this.task;

                    let item_name = if for_.each.is_empty() {
                        "item"
                    } else {
                        &for_.each
                    };
                    let index_name = if for_.at.is_empty() {
                        "index"
                    } else {
                        &for_.at
                    };

                    let items = transformed_in_items.as_array().unwrap().clone();

                    // Process based on execution mode
                    if *in_parallel {
                        // Parallel processing with real-time streaming
                        match this
                            .process_items_in_parallel_stream(
                                cx.clone(),
                                &items,
                                item_name,
                                index_name,
                                &task_context,
                                do_task,
                                &task_name,
                                while_,
                            )
                            .await
                        {
                            Ok(mut stream) => {
                                // Forward all results from the inner stream, regardless of success/error
                                while let Some(result) = stream.next().await {
                                    yield result;
                                }
                            }
                            Err(e) => {
                                yield Err(e);
                            }
                        }
                    } else {
                        // Sequential processing
                        match this
                            .process_items_sequentially_stream(
                                cx.clone(),
                                items,
                                item_name.to_string(),
                                index_name.to_string(),
                                task_context.clone(),
                                do_task,
                                task_name,
                                while_.clone(),
                                task_context,
                            )
                            .await
                        {
                            Ok(mut stream) => {
                                // Forward all results from the inner stream, regardless of success/error
                                while let Some(result) = stream.next().await {
                                    yield result;
                                }
                            }
                            Err(e) => {
                                yield Err(e);
                            }
                        }
                    }
                }
                Err(e) => {
                    yield Err(e);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::definition::workflow::WorkflowSchema;
    use crate::workflow::execute::context::WorkflowContext;
    use app::module::test::create_hybrid_test_app;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn test_for_task_sequential_continue_on_error() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        // Test that with onError: continue, processing continues after errors
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-continue-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "onError": "continue",
                            "do": [
                                {
                                    "item_router": {
                                        "switch": [
                                            {
                                                "error_case": {
                                                    "when": "${$index == 1}",
                                                    "then": "fail_item"
                                                }
                                            },
                                            {
                                                "success_case": {
                                                    "then": "process_item"
                                                }
                                            }
                                        ]
                                    }
                                },
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}",
                                            "processed_index": "${$index}"
                                        }
                                    }
                                },
                                {
                                    "fail_item": {
                                        "raise": {
                                            "error": {
                                                "type": "https://serverlessworkflow.io/errors/generic",
                                                "status": 500,
                                                "title": "Intentional error for testing"
                                            }
                                        }
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            println!("Creating workflow from JSON: {}", serde_json::to_string_pretty(&workflow_json).unwrap());

            let workflow = Arc::new(
                serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap()
            );

            // Test input with 4 items - explicitly provide the data
            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1", "item2", "item3"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));

            let executor = WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: workflow.clone(),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            println!("Starting workflow execution...");

            let workflow_stream = executor.execute_workflow(
                Arc::new(opentelemetry::Context::current()),
            );

            tokio::pin!(workflow_stream);

            // Collect results with timeout
            let timeout_duration = tokio::time::Duration::from_secs(10);
            let timeout_result = tokio::time::timeout(timeout_duration, async {
                let mut local_results = Vec::new();
                let mut success_results = Vec::new();
                let mut error_results = Vec::new();

                while let Some(result) = workflow_stream.next().await {
                    match &result {
                        Ok(wc) => {
                            println!("Success result: status={:?}, output keys={:?}", wc.status,
                wc.output.as_ref().map(|v| v.as_object().map(|o| o.keys().collect::<Vec<_>>())));
                            if let Some(output) = wc.output.as_ref()
                                && let Some(obj) = output.as_object()
                                    && obj.contains_key("processed_item") && obj.contains_key("processed_index") {
                                        println!("  → Item processed successfully: {} at index {}",
                                                obj.get("processed_item").unwrap(),
                                                obj.get("processed_index").unwrap());
                                    }
                            success_results.push(wc.clone());
                        }
                        Err(e) => {
                            println!("Error result: {:?}", e);
                            error_results.push(format!("{}", e));
                        }
                    }
                    local_results.push(result);

                    // Safety limit
                    if local_results.len() > 20 {
                        println!("Too many results, stopping collection");
                        break;
                    }
                }
                (local_results, success_results, error_results)
            }).await;

            let (results, success_results, _error_results) = match timeout_result {
                Ok(res) => res,
                Err(_) => {
                    println!("Test timed out after 10 seconds");
                    (Vec::new(), Vec::new(), Vec::new())
                }
            };

            println!("====Final results received: {} total results", results.len());
            println!("Success results: {}", success_results.len());

            for (i, r) in results.iter().enumerate() {
                match r {
                    Ok(wc) => println!("Result {}: status={:?}, has_error_output={}, output={:?}", i, wc.status,
                        wc.output.as_ref().map(|o| o.get("error").is_some()).unwrap_or(false),
                        wc.output.as_ref().map(|o| o.as_object().map(|obj| obj.keys().collect::<Vec<_>>()))),
                    Err(e) => println!("Result {}: Error - {}", i, e),
                }
            }

            // Per-item events that surfaced the failure as Ok with `error` in output.
            let error_event_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc
                        .output
                        .as_ref()
                        .map(|o| o.get("error").is_some())
                        .unwrap_or(false),
                    Err(_) => false,
                }
            }).count();

            let faulted_status_count = results.iter().filter(|r| {
                matches!(r, Ok(wc) if wc.status == WorkflowStatus::Faulted)
            }).count();

            let top_level_err_count = results.iter().filter(|r| r.is_err()).count();

            // Count successfully processed items (have processed_item in output)
            let processed_item_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.output.as_ref()
                        .and_then(|o| o.get("processed_item"))
                        .is_some(),
                    Err(_) => false,
                }
            }).count();

            println!(
                "Results summary - error_events: {}, faulted_status: {}, top_level_err: {}, processed_items: {}",
                error_event_count, faulted_status_count, top_level_err_count, processed_item_count
            );

            // CONTINUE MODE: with the fix, item-level failures must NOT abort the workflow.
            assert!(!results.is_empty(), "Expected some results, but got none");

            // No top-level Err should escape under onError=continue.
            assert_eq!(
                top_level_err_count, 0,
                "Expected no top-level Err under onError=continue, got {}", top_level_err_count
            );

            // The workflow as a whole must not be Faulted.
            let final_status = workflow_context.read().await.status.clone();
            assert_ne!(
                final_status,
                WorkflowStatus::Faulted,
                "Workflow must not be Faulted under onError=continue"
            );
            assert_eq!(
                faulted_status_count, 0,
                "No yielded WorkflowContext should carry Faulted status under continue, got {}",
                faulted_status_count
            );

            // The fixture's switch falls through to the next task without `then: end`,
            // so every iteration emits a `process_item` event AND eventually hits
            // `fail_item` (after switch routes back). What matters here is:
            //   1. The loop completed every iteration (4 iterations × 4 items processed),
            //   2. Each iteration's failure surfaced as an Ok event with `error` payload,
            //   3. No top-level Err escaped and the workflow did not Fault.
            assert_eq!(
                processed_item_count, 4,
                "Expected all 4 iterations to reach process_item, got {}",
                processed_item_count
            );
            assert_eq!(
                error_event_count, 4,
                "Expected one Ok error event per iteration, got {}",
                error_event_count
            );

            println!("CONTINUE MODE VERIFICATION (fixed):");
            println!("- Loop ran all 4 iterations under onError=continue");
            println!("- Each per-iteration failure surfaced as Ok event with error info");
            println!("- No top-level Err escaped; workflow status remained non-Faulted");
        });
    }

    #[test]
    fn test_for_task_parallel_continue_on_error() {
        // Same shape as the sequential continue test, but with `inParallel: true`.
        // Verifies that the parallel path also converts per-item Err into Ok events
        // under onError=continue and does not abort the workflow.
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-parallel-continue-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "inParallel": true,
                            "onError": "continue",
                            "do": [
                                {
                                    "item_router": {
                                        "switch": [
                                            {
                                                "error_case": {
                                                    "when": "${$index == 1}",
                                                    "then": "fail_item"
                                                }
                                            },
                                            {
                                                "success_case": {
                                                    "then": "process_item"
                                                }
                                            }
                                        ]
                                    }
                                },
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}",
                                            "processed_index": "${$index}"
                                        }
                                    }
                                },
                                {
                                    "fail_item": {
                                        "raise": {
                                            "error": {
                                                "type": "https://serverlessworkflow.io/errors/generic",
                                                "status": 500,
                                                "title": "Intentional error for testing"
                                            }
                                        }
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow = Arc::new(
                serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap()
            );

            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1", "item2", "item3"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));

            let executor = WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: workflow.clone(),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            let workflow_stream = executor.execute_workflow(
                Arc::new(opentelemetry::Context::current()),
            );
            tokio::pin!(workflow_stream);

            let timeout_duration = tokio::time::Duration::from_secs(15);
            let timeout_result = tokio::time::timeout(timeout_duration, async {
                let mut local_results = Vec::new();
                while let Some(result) = workflow_stream.next().await {
                    local_results.push(result);
                    if local_results.len() > 30 {
                        break;
                    }
                }
                local_results
            }).await;

            let results = timeout_result.expect("parallel continue test timed out");

            let top_level_err_count = results.iter().filter(|r| r.is_err()).count();
            let faulted_status_count = results.iter().filter(|r| {
                matches!(r, Ok(wc) if wc.status == WorkflowStatus::Faulted)
            }).count();
            let error_event_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.output.as_ref()
                        .map(|o| o.get("error").is_some()).unwrap_or(false),
                    Err(_) => false,
                }
            }).count();
            let processed_item_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.output.as_ref()
                        .and_then(|o| o.get("processed_item")).is_some(),
                    Err(_) => false,
                }
            }).count();

            println!(
                "[parallel continue] errors={} faulted={} top_err={} processed={}",
                error_event_count, faulted_status_count, top_level_err_count, processed_item_count
            );

            assert!(!results.is_empty(), "Expected some results, got none");
            assert_eq!(top_level_err_count, 0, "No top-level Err under onError=continue");
            assert_eq!(faulted_status_count, 0, "No Faulted status events under continue");

            let final_status = workflow_context.read().await.status.clone();
            assert_ne!(final_status, WorkflowStatus::Faulted,
                "Workflow must not be Faulted under onError=continue (parallel)");

            // Same fixture as the sequential case: switch falls through, so all 4
            // iterations both reach `process_item` and then `fail_item`.
            // Parallel ordering doesn't change those totals.
            assert_eq!(processed_item_count, 4,
                "Expected all 4 iterations to reach process_item, got {}", processed_item_count);
            assert_eq!(error_event_count, 4,
                "Expected one Ok error event per iteration, got {}", error_event_count);
        });
    }

    // prepare_for_item failures (broken jq in `while`) are DSL-level errors and must
    // fault the workflow even when onError=continue is set. Tests both sequential
    // and parallel paths to lock in the new contract.
    #[test]
    fn test_for_task_prepare_error_faults_even_with_continue() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            for in_parallel in [false, true] {
                let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

                // `while` references an undefined jq function so transform_value fails
                // during prepare_for_item for every iteration.
                let workflow_json = serde_json::json!({
                    "document": {
                        "dsl": "1.0.0",
                        "namespace": "test",
                        "name": "for-task-prepare-error",
                        "version": "1.0.0",
                        "metadata": {}
                    },
                    "input": {
                        "schema": {
                            "document": {
                                "type": "object",
                                "properties": {
                                    "items": {"type": "array", "items": {"type": "string"}}
                                }
                            }
                        }
                    },
                    "do": [
                        {
                            "process_items": {
                                "for": {
                                    "in": "${.items}",
                                    "each": "item",
                                    "at": "index"
                                },
                                "while": "${ this_function_does_not_exist(.) }",
                                "inParallel": in_parallel,
                                "onError": "continue",
                                "do": [
                                    {
                                        "noop": {
                                            "set": { "x": 1 }
                                        }
                                    }
                                ]
                            }
                        }
                    ]
                });

                let workflow = Arc::new(
                    serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap()
                );
                let input = Arc::new(serde_json::json!({"items": ["a", "b", "c"]}));
                let context = Arc::new(serde_json::json!({}));

                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &workflow,
                    input.clone(),
                    context,
                    None,
                )));

                let executor = WorkflowExecutor {
                    default_task_timeout_sec: 30,
                    job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                    workflow: workflow.clone(),
                    workflow_context: workflow_context.clone(),
                    execution_id: None,
                    metadata: Arc::new(HashMap::new()),
                    checkpoint_repository: None,
                };

                let workflow_stream = executor.execute_workflow(
                    Arc::new(opentelemetry::Context::current()),
                );
                tokio::pin!(workflow_stream);

                let timeout_duration = tokio::time::Duration::from_secs(10);
                let timeout_result = tokio::time::timeout(timeout_duration, async {
                    let mut local_results = Vec::new();
                    while let Some(result) = workflow_stream.next().await {
                        local_results.push(result);
                        if local_results.len() > 30 { break; }
                    }
                    local_results
                }).await;

                let results = timeout_result
                    .unwrap_or_else(|_| panic!("prepare-error test timed out (in_parallel={})", in_parallel));

                let final_status = workflow_context.read().await.status.clone();
                let top_level_err_count = results.iter().filter(|r| r.is_err()).count();
                let faulted_status_count = results.iter().filter(|r| {
                    matches!(r, Ok(wc) if wc.status == WorkflowStatus::Faulted)
                }).count();

                println!(
                    "[prepare-error in_parallel={}] final_status={:?} top_err={} faulted_status={}",
                    in_parallel, final_status, top_level_err_count, faulted_status_count
                );

                // Either an explicit Err propagated, or the workflow ended Faulted.
                // Both are acceptable evidence that the failure was not swallowed.
                assert!(
                    top_level_err_count >= 1
                        || faulted_status_count >= 1
                        || final_status == WorkflowStatus::Faulted,
                    "Prepare-time DSL error must fault the workflow even with onError=continue (in_parallel={}); status={:?}, top_err={}, faulted_events={}",
                    in_parallel, final_status, top_level_err_count, faulted_status_count
                );
            }
        });
    }

    /// In parallel mode, when prepare succeeds for the first few items but
    /// fails for a later item, the previously-prepared items must NOT execute
    /// their `do` body. Relying on the cancel watch channel to stop them is
    /// racy because tokio::select! can pick the stream side over an
    /// already-set cancel signal pseudo-randomly. The implementation must
    /// skip spawning entirely once a prepare error is queued so the failure
    /// is deterministic.
    #[test]
    fn test_for_task_parallel_prepare_error_blocks_pending_spawns() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            // `while` succeeds for index < 2 and triggers an undefined-jq-fn
            // error at index 2, so items 0 and 1 are pushed into
            // items_to_process before the prepare loop bails. Without the
            // fix, those two items would still get spawned and may run their
            // `set` task before the cancel signal lands.
            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-parallel-prepare-late-error",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {"type": "array", "items": {"type": "string"}}
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "while": "${ if $index < 2 then true else this_function_does_not_exist(.) end }",
                            "inParallel": true,
                            "onError": "continue",
                            "do": [
                                {
                                    "side_effect": {
                                        "set": { "processed_item": "${ $item }" }
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow = Arc::new(
                serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap()
            );
            let input = Arc::new(serde_json::json!({
                "items": ["a", "b", "c", "d"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));

            let executor = WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: workflow.clone(),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            let workflow_stream = executor.execute_workflow(
                Arc::new(opentelemetry::Context::current()),
            );
            tokio::pin!(workflow_stream);

            let timeout_duration = tokio::time::Duration::from_secs(10);
            let timeout_result = tokio::time::timeout(timeout_duration, async {
                let mut local_results = Vec::new();
                while let Some(result) = workflow_stream.next().await {
                    local_results.push(result);
                    if local_results.len() > 30 { break; }
                }
                local_results
            }).await;

            let results = timeout_result.expect("test timed out");

            // The workflow must fault.
            let final_status = workflow_context.read().await.status.clone();
            let top_level_err_count = results.iter().filter(|r| r.is_err()).count();
            let faulted_status_count = results.iter().filter(|r| {
                matches!(r, Ok(wc) if wc.status == WorkflowStatus::Faulted)
            }).count();
            assert!(
                top_level_err_count >= 1
                    || faulted_status_count >= 1
                    || final_status == WorkflowStatus::Faulted,
                "Prepare-time DSL error must fault the workflow; status={:?}, top_err={}, faulted_events={}",
                final_status, top_level_err_count, faulted_status_count
            );

            // None of the previously-prepared items must have produced a
            // `processed_item` output. If the spawn loop is letting them run,
            // their `set` task surfaces it via TaskCompleted before the
            // workflow tears down, even with --test-threads=1.
            let processed_item_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.output.as_ref()
                        .and_then(|o| o.get("processed_item"))
                        .is_some(),
                    Err(_) => false,
                }
            }).count();
            assert_eq!(
                processed_item_count, 0,
                "No prepared item should run its `do` body once a later item's prepare failed; got {} processed_item events",
                processed_item_count
            );
        });
    }

    #[test]
    fn test_for_task_sequential_break_on_error() {
        // command_utils::util::tracing::tracing_init_test(tracing::Level::DEBUG);
        // Test that with onError: break, processing stops after first error
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-break-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "onError": "break",
                            "do": [
                                {
                                    "item_router": {
                                        "switch": [
                                            {
                                                "error_case": {
                                                    "when": "${$index == 1}",
                                                    "then": "fail_item"
                                                }
                                            },
                                            {
                                                "success_case": {
                                                    "then": "process_item"
                                                }
                                            }
                                        ]
                                    }
                                },
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}",
                                            "processed_index": "${$index}"
                                        }
                                    }
                                },
                                {
                                    "fail_item": {
                                        "raise": {
                                            "error": {
                                                "type": "https://serverlessworkflow.io/errors/generic",
                                                "status": 500,
                                                "title": "Intentional break error for testing"
                                            }
                                        }
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow: WorkflowSchema = serde_json::from_value(workflow_json).unwrap();

            // Test data - 4 items, error should occur at index 1 (2nd item) and stop processing
            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1", "item2", "item3"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context.clone(),
                None,
            )));

            println!("[DEBUG BREAK MODE] Creating WorkflowExecutor for break test...");
            let executor = crate::workflow::execute::workflow::WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: Arc::new(workflow.clone()),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            println!("Starting workflow execution for break mode...");

            let workflow_stream = executor.execute_workflow(
                Arc::new(opentelemetry::Context::current()),
            );

            tokio::pin!(workflow_stream);

            // Collect results with timeout
            let timeout_duration = tokio::time::Duration::from_secs(10);
            let timeout_result = tokio::time::timeout(timeout_duration, async {
                let mut local_results = Vec::new();
                let mut success_results = Vec::new();

                while let Some(result) = workflow_stream.next().await {
                    match &result {
                        Ok(wc) => {
                            println!("Result: status={:?}, output keys={:?}", wc.status,
                wc.output.as_ref().map(|v| v.as_object().map(|o| o.keys().collect::<Vec<_>>())));
                            if let Some(output) = wc.output.as_ref()
                                && let Some(obj) = output.as_object()
                                    && obj.contains_key("processed_item") && obj.contains_key("processed_index") {
                                        println!("  → Item processed successfully: {} at index {}",
                                                obj.get("processed_item").unwrap(),
                                                obj.get("processed_index").unwrap());
                                    }
                            success_results.push(wc.clone());
                        }
                        Err(e) => {
                            println!("Error result: {:?}", e);
                        }
                    }
                    local_results.push(result);

                    // Safety limit
                    if local_results.len() > 20 {
                        println!("Too many results, stopping collection");
                        break;
                    }
                }
                (local_results, success_results)
            }).await;

            let (results, _success_results) = match timeout_result {
                Ok(res) => res,
                Err(_) => {
                    println!("Test timed out after 10 seconds");
                    (Vec::new(), Vec::new())
                }
            };

            println!("====Final results received for break mode: {} total results", results.len());

            for (i, r) in results.iter().enumerate() {
                match r {
                    Ok(wc) => println!("Result {}: status={:?}, has_error_output={}, output={:?}", i, wc.status,
                        wc.output.as_ref().map(|o| o.get("error").is_some()).unwrap_or(false),
                        wc.output.as_ref().map(|o| o.as_object().map(|obj| obj.keys().collect::<Vec<_>>()))),
                    Err(e) => println!("Result {}: Error - {}", i, e),
                }
            }

            // Count faulted results (workflow errors are now returned as Ok with Faulted status)
            let faulted_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.status == WorkflowStatus::Faulted ||
                        wc.output.as_ref().map(|o| o.get("error").is_some()).unwrap_or(false),
                    Err(_) => true,
                }
            }).count();

            // Count successfully processed items (have processed_item in output)
            let processed_item_count = results.iter().filter(|r| {
                match r {
                    Ok(wc) => wc.output.as_ref()
                        .and_then(|o| o.get("processed_item"))
                        .is_some(),
                    Err(_) => false,
                }
            }).count();

            println!("Results summary - faulted: {}, processed_items: {}", faulted_count, processed_item_count);

            // BREAK MODE: Detailed verification
            // Expected behavior: item0 succeeds, item1 fails and stops processing, item2 and item3 not processed

            // Should have received some results
            assert!(!results.is_empty(), "Expected some results, but got none");

            // Should have at least one faulted result that caused the break
            assert!(faulted_count >= 1, "Expected at least 1 faulted result in break mode, got {}", faulted_count);

            // Should have fewer successfully processed items than continue mode (only item0, not item2&item3)
            // In break mode, we expect only item0 to succeed before error at item1
            assert!(processed_item_count <= 1, "Expected at most 1 processed item in break mode (only item0), got {}", processed_item_count);
        });
    }

    /// Test that wait directive inside sequential ForTask returns an error
    /// ForTask internal wait is not supported due to complexity of parallel iteration state management
    #[test]
    fn test_for_task_sequential_wait_not_supported() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::modules::test::create_test_app_wrapper_module;
            use crate::workflow::execute::workflow::WorkflowExecutor;
            use futures_util::pin_mut;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));

            // Create workflow with wait inside sequential for loop
            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-sequential-wait-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "do": [
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}"
                                        },
                                        "then": "wait"  // Wait inside for loop - not supported
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow =
                Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());

            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1", "item2"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::init(
                app_wrapper_module,
                app_module,
                workflow,
                input,
                None,
                context,
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut found_error = false;
            let mut error_message = String::new();

            while let Some(result) = workflow_stream.next().await {
                match result {
                    Err(e) => {
                        error_message = format!("{}", e);
                        found_error = true;
                        break;
                    }
                    Ok(wc) => {
                        // Check if error is in the output (workflow errors are now returned as Ok with Faulted status)
                        if wc.status == WorkflowStatus::Faulted
                            && let Some(output) = wc.output.as_ref()
                            && let Some(err) = output.get("error")
                        {
                            error_message = err.to_string();
                            found_error = true;
                            break;
                        }
                    }
                }
            }

            assert!(
                found_error,
                "Expected error when wait is used inside ForTask"
            );
            assert!(
                error_message.contains("Wait directive inside ForTask is not currently supported")
                    || error_message.contains("wait"),
                "Error message should indicate wait inside ForTask is not supported, got: {}",
                error_message
            );
        });
    }

    /// Test that wait directive inside parallel ForTask returns an error
    #[test]
    fn test_for_task_parallel_wait_not_supported() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::modules::test::create_test_app_wrapper_module;
            use crate::workflow::execute::workflow::WorkflowExecutor;
            use futures_util::pin_mut;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));

            // Create workflow with wait inside parallel for loop
            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-parallel-wait-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "parallel": true,  // Parallel execution
                            "do": [
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}"
                                        },
                                        "then": "wait"  // Wait inside parallel for loop - not supported
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow =
                Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());

            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1", "item2"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::init(
                app_wrapper_module,
                app_module,
                workflow,
                input,
                None,
                context,
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut found_error = false;
            let mut error_message = String::new();

            while let Some(result) = workflow_stream.next().await {
                match result {
                    Err(e) => {
                        error_message = format!("{}", e);
                        found_error = true;
                        break;
                    }
                    Ok(wc) => {
                        // Check if error is in the output (workflow errors are now returned as Ok with Faulted status)
                        if wc.status == WorkflowStatus::Faulted
                            && let Some(output) = wc.output.as_ref()
                            && let Some(err) = output.get("error")
                        {
                            error_message = err.to_string();
                            found_error = true;
                            break;
                        }
                    }
                }
            }

            assert!(
                found_error,
                "Expected error when wait is used inside parallel ForTask"
            );
            assert!(
                error_message.contains("Wait directive inside ForTask is not currently supported"),
                "Error message should indicate wait inside ForTask is not supported, got: {}",
                error_message
            );
        });
    }

    /// `then: wait` inside a ForTask is structurally unsupported. `onError:
    /// continue` only governs per-item runtime failures and must not silently
    /// swallow this DSL-level error. Both sequential and parallel paths must
    /// surface a faulted workflow even when continue is set.
    #[test]
    fn test_for_task_wait_not_supported_even_with_continue() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::modules::test::create_test_app_wrapper_module;
            use crate::workflow::execute::workflow::WorkflowExecutor;
            use futures_util::pin_mut;

            for in_parallel in [false, true] {
                let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
                let app_wrapper_module =
                    Arc::new(create_test_app_wrapper_module(app_module.clone()));

                let workflow_json = serde_json::json!({
                    "document": {
                        "dsl": "1.0.0",
                        "namespace": "test",
                        "name": "for-task-wait-with-continue",
                        "version": "1.0.0",
                        "metadata": {}
                    },
                    "input": {
                        "schema": {
                            "document": {
                                "type": "object",
                                "properties": {
                                    "items": {"type": "array", "items": {"type": "string"}}
                                }
                            }
                        }
                    },
                    "do": [
                        {
                            "process_items": {
                                "for": {
                                    "in": "${.items}",
                                    "each": "item",
                                    "at": "index"
                                },
                                "inParallel": in_parallel,
                                "onError": "continue",
                                "do": [
                                    {
                                        "process_item": {
                                            "set": { "processed_item": "${$item}" },
                                            "then": "wait"
                                        }
                                    }
                                ]
                            }
                        }
                    ]
                });

                let workflow =
                    Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());
                let input = Arc::new(serde_json::json!({
                    "items": ["item0", "item1", "item2"]
                }));
                let context = Arc::new(serde_json::json!({}));

                let executor = WorkflowExecutor::init(
                    app_wrapper_module,
                    app_module,
                    workflow,
                    input,
                    None,
                    context,
                    Arc::new(HashMap::new()),
                    None,
                )
                .await
                .unwrap();

                let workflow_stream =
                    executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
                pin_mut!(workflow_stream);

                let mut surfaced_unsupported_wait = false;
                let mut last_status = WorkflowStatus::Running;
                let timeout_duration = tokio::time::Duration::from_secs(15);
                let _ = tokio::time::timeout(timeout_duration, async {
                    while let Some(result) = workflow_stream.next().await {
                        match result {
                            Err(e) => {
                                let msg = format!("{}", e);
                                if msg.contains(
                                    "Wait directive inside ForTask is not currently supported",
                                ) {
                                    surfaced_unsupported_wait = true;
                                }
                                break;
                            }
                            Ok(wc) => {
                                last_status = wc.status.clone();
                                if wc.status == WorkflowStatus::Faulted
                                    && let Some(output) = wc.output.as_ref()
                                    && let Some(err) = output.get("error")
                                {
                                    let msg = err.to_string();
                                    if msg.contains(
                                        "Wait directive inside ForTask is not currently supported",
                                    ) {
                                        surfaced_unsupported_wait = true;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                })
                .await;

                assert!(
                    surfaced_unsupported_wait,
                    "in_parallel={in_parallel}: unsupported `then: wait` inside ForTask must surface as a faulting error even with onError=continue (last_status={last_status:?})"
                );
            }
        });
    }

    /// Test that wait directive works correctly AFTER ForTask completes
    #[test]
    fn test_wait_after_for_task_works() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::modules::test::create_test_app_wrapper_module;
            use crate::workflow::execute::context::WorkflowStatus;
            use crate::workflow::execute::workflow::WorkflowExecutor;
            use futures_util::pin_mut;

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());
            let app_wrapper_module = Arc::new(create_test_app_wrapper_module(app_module.clone()));

            // Create workflow: ForTask -> WaitTask (wait is OUTSIDE for loop)
            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "wait-after-for-test",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {
                                    "type": "array",
                                    "items": {"type": "string"}
                                }
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "do": [
                                {
                                    "process_item": {
                                        "set": {
                                            "processed_item": "${$item}"
                                        }
                                    }
                                }
                            ]
                        }
                    },
                    {
                        "wait_for_user": {
                            "set": {
                                "waiting": true
                            },
                            "then": "wait"  // Wait AFTER for loop - should work
                        }
                    },
                    {
                        "after_wait": {
                            "set": {
                                "completed": true
                            }
                        }
                    }
                ]
            });

            let workflow =
                Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());

            let input = Arc::new(serde_json::json!({
                "items": ["item0", "item1"]
            }));
            let context = Arc::new(serde_json::json!({}));

            let executor = WorkflowExecutor::init(
                app_wrapper_module,
                app_module,
                workflow,
                input,
                None,
                context,
                Arc::new(HashMap::new()),
                None,
            )
            .await
            .unwrap();

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            pin_mut!(workflow_stream);

            let mut last_status = WorkflowStatus::Running;
            let mut has_error = false;

            while let Some(result) = workflow_stream.next().await {
                match result {
                    Err(e) => {
                        println!("Unexpected error: {:?}", e);
                        has_error = true;
                        break;
                    }
                    Ok(wc) => {
                        last_status = wc.status.clone();
                        // Check if after_wait task executed (it shouldn't)
                        if let Some(output) = wc.output.as_ref()
                            && let Some(obj) = output.as_object()
                        {
                            assert!(
                                !obj.contains_key("completed"),
                                "Task after wait should not execute"
                            );
                        }
                    }
                }
            }

            assert!(!has_error, "Should not have any errors");
            assert_eq!(
                last_status,
                WorkflowStatus::Waiting,
                "Workflow should be in Waiting status after wait directive"
            );
        });
    }

    /// Verify span attributes for the for-task tracing fix:
    /// - Each inner do_task span records its own real input (post-`from`)
    ///   rather than the for-task-level input (the whole `in` array).
    /// - The for branch span carries `workflow.task.for.item` and
    ///   `workflow.task.for.index` matching the iteration value.
    ///
    /// Covers both sequential and parallel paths in one fixture by running
    /// each mode against an identical workflow shape.
    #[test]
    fn test_for_task_span_attributes_record_per_item_input() {
        use opentelemetry::Value as OtelValue;
        use opentelemetry::global;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

        // Snapshot the previous global provider and restore it via a RAII
        // guard so a panic anywhere inside the test still puts the global
        // back. This test relies on the workspace policy of running tests
        // with --test-threads=1 (CLAUDE.md); without serial execution the
        // global provider swap would race with any other test that observes
        // it, so do not parallelise this test in isolation.
        struct ProviderGuard {
            saved: Option<opentelemetry::global::GlobalTracerProvider>,
        }
        impl Drop for ProviderGuard {
            fn drop(&mut self) {
                if let Some(p) = self.saved.take() {
                    opentelemetry::global::set_tracer_provider(p);
                }
            }
        }
        let _guard = ProviderGuard {
            saved: Some(global::tracer_provider()),
        };

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            for in_parallel in [false, true] {
                // Each iteration starts with its own exporter/provider so the
                // first run's spans don't pollute the second run's assertions.
                let exporter = InMemorySpanExporter::default();
                let provider = SdkTracerProvider::builder()
                    .with_simple_exporter(exporter.clone())
                    .build();
                global::set_tracer_provider(provider.clone());

                let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

                // `do.process_item` uses `input.from` to project the iteration
                // item; the post-from input must be the single string element,
                // not the parent array.
                let workflow_json = serde_json::json!({
                    "document": {
                        "dsl": "1.0.0",
                        "namespace": "test",
                        "name": "for-task-span-attr-test",
                        "version": "1.0.0",
                        "metadata": {}
                    },
                    "input": {
                        "schema": {
                            "document": {
                                "type": "object",
                                "properties": {
                                    "items": {"type": "array", "items": {"type": "string"}}
                                }
                            }
                        }
                    },
                    "do": [
                        {
                            "process_items": {
                                "for": {
                                    "in": "${.items}",
                                    "each": "item",
                                    "at": "index"
                                },
                                "inParallel": in_parallel,
                                "do": [
                                    {
                                        "process_item": {
                                            "input": { "from": "${ $item }" },
                                            "set": { "processed": "${ . }" }
                                        }
                                    }
                                ]
                            }
                        }
                    ]
                });

                let workflow =
                    Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());
                let input = Arc::new(serde_json::json!({
                    "items": ["alpha", "beta", "gamma"]
                }));
                let context = Arc::new(serde_json::json!({}));
                let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                    &workflow,
                    input.clone(),
                    context,
                    None,
                )));

                let executor = WorkflowExecutor {
                    default_task_timeout_sec: 30,
                    job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                    workflow: workflow.clone(),
                    workflow_context: workflow_context.clone(),
                    execution_id: None,
                    metadata: Arc::new(HashMap::new()),
                    checkpoint_repository: None,
                };

                let workflow_stream =
                    executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
                tokio::pin!(workflow_stream);
                while let Some(_r) = workflow_stream.next().await {}

                // Force flush so all spans land in the exporter before assertion.
                let _ = provider.force_flush();
                let spans = exporter.get_finished_spans().expect("get spans");

                // Helper: pull a string-valued attribute by key.
                let attr_str = |s: &opentelemetry_sdk::trace::SpanData, key: &str| -> Option<String> {
                    s.attributes.iter().find_map(|kv| {
                        if kv.key.as_str() == key {
                            match &kv.value {
                                OtelValue::String(v) => Some(v.to_string()),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                };
                let attr_i64 = |s: &opentelemetry_sdk::trace::SpanData, key: &str| -> Option<i64> {
                    s.attributes.iter().find_map(|kv| {
                        if kv.key.as_str() == key {
                            match &kv.value {
                                OtelValue::I64(v) => Some(*v),
                                _ => None,
                            }
                        } else {
                            None
                        }
                    })
                };

                // 1) Each `process_item` task span must carry the per-iteration
                //    string as its workflow.task.input — not the source array.
                //    Match the full task-name suffix so the outer `process_items`
                //    span (note the trailing `s`) is excluded.
                let process_item_inputs: Vec<String> = spans
                    .iter()
                    .filter(|s| s.name.ends_with(":process_item"))
                    .filter_map(|s| attr_str(s, "workflow.task.input"))
                    .collect();
                assert!(
                    !process_item_inputs.is_empty(),
                    "expected at least one process_item span (in_parallel={in_parallel})"
                );
                for input_json in &process_item_inputs {
                    assert!(
                        input_json.contains("alpha")
                            || input_json.contains("beta")
                            || input_json.contains("gamma"),
                        "process_item span input should contain a single iteration value, got: {input_json} (in_parallel={in_parallel})"
                    );
                    // The bug used to surface the whole array; fail loudly if it
                    // ever regresses.
                    assert!(
                        !(input_json.contains("alpha")
                            && input_json.contains("beta")
                            && input_json.contains("gamma")),
                        "process_item span input must NOT be the entire array, got: {input_json} (in_parallel={in_parallel})"
                    );
                }

                // 2) For-iteration branch spans must carry item + index attrs.
                let mut for_item_attrs: Vec<(i64, String)> = spans
                    .iter()
                    .filter_map(|s| {
                        let idx = attr_i64(s, "workflow.task.for.index")?;
                        let item = attr_str(s, "workflow.task.for.item")?;
                        Some((idx, item))
                    })
                    .collect();
                for_item_attrs.sort_by_key(|(i, _)| *i);
                for_item_attrs.dedup();
                assert_eq!(
                    for_item_attrs.len(),
                    3,
                    "expected 3 distinct iteration spans, got {:?} (in_parallel={in_parallel})",
                    for_item_attrs
                );
                let expected = ["alpha", "beta", "gamma"];
                for (i, (idx, item)) in for_item_attrs.iter().enumerate() {
                    assert_eq!(*idx as usize, i);
                    assert!(
                        item.contains(expected[i]),
                        "iteration {i} item attr should contain {} (got {item}, in_parallel={in_parallel})",
                        expected[i]
                    );
                }

                // Drain in-flight exports for THIS iteration's provider before
                // installing the next one, so we don't leak spans across runs.
                let _ = provider.shutdown();
            }
        });
        // ProviderGuard restores the original global on drop.
    }

    /// In parallel + onError=continue mode, a failed iteration is exposed
    /// upstream as Ok(TaskCompleted) so the workflow does not abort, but the
    /// span for that iteration must NOT report success: OpenTelemetry status
    /// is partial-ordered Ok > Error > Unset, so calling
    /// `record_result(Ok)` after `record_error` would silently flip the span
    /// status back to Ok and break alerting/error-rate dashboards.
    #[test]
    fn test_for_task_parallel_continue_keeps_error_span_status() {
        use opentelemetry::global;
        use opentelemetry::trace::Status;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

        struct ProviderGuard {
            saved: Option<opentelemetry::global::GlobalTracerProvider>,
        }
        impl Drop for ProviderGuard {
            fn drop(&mut self) {
                if let Some(p) = self.saved.take() {
                    opentelemetry::global::set_tracer_provider(p);
                }
            }
        }
        let _guard = ProviderGuard {
            saved: Some(global::tracer_provider()),
        };

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            global::set_tracer_provider(provider.clone());

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            // Item at index 1 raises; the for-task uses inParallel + continue
            // so the workflow keeps running. Each iteration's branch span
            // should still expose Status::Error for index 1. Avoid `then: end`
            // in the success path so the workflow doesn't complete and cancel
            // siblings before the failing iteration's span is finalised.
            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "for-task-parallel-continue-error-span",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": {
                                "items": {"type": "array", "items": {"type": "string"}}
                            }
                        }
                    }
                },
                "do": [
                    {
                        "process_items": {
                            "for": {
                                "in": "${.items}",
                                "each": "item",
                                "at": "index"
                            },
                            "inParallel": true,
                            "onError": "continue",
                            "do": [
                                {
                                    "maybe_fail": {
                                        "if": "${ $index == 1 }",
                                        "raise": {
                                            "error": {
                                                "type": "https://serverlessworkflow.io/errors/generic",
                                                "status": 500,
                                                "title": "test failure"
                                            }
                                        }
                                    }
                                }
                            ]
                        }
                    }
                ]
            });

            let workflow =
                Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());
            let input = Arc::new(serde_json::json!({"items": ["a", "b", "c"]}));
            let context = Arc::new(serde_json::json!({}));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            let executor = WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: workflow.clone(),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            let workflow_stream = executor
                .execute_workflow(Arc::new(opentelemetry::Context::current()));
            tokio::pin!(workflow_stream);
            while (workflow_stream.next().await).is_some() {}

            let _ = provider.force_flush();
            let spans = exporter.get_finished_spans().expect("get spans");

            // Find the index-1 iteration's branch span. The parallel branch
            // span name is the for `each` value (here: "item"). Its
            // workflow.task.for.index attribute pinpoints which iteration.
            let mut item1_span_status: Option<Status> = None;
            for s in &spans {
                let mut idx = None;
                for kv in &s.attributes {
                    if kv.key.as_str() == "workflow.task.for.index"
                        && let opentelemetry::Value::I64(v) = &kv.value
                    {
                        idx = Some(*v);
                    }
                }
                if idx == Some(1) {
                    item1_span_status = Some(s.status.clone());
                    break;
                }
            }

            let status = item1_span_status.expect(
                "parallel for-task should produce a branch span tagged with workflow.task.for.index=1",
            );
            assert!(
                matches!(status, Status::Error { .. }),
                "parallel for branch span for the failing iteration must report Status::Error even with onError=continue, got {:?}",
                status
            );

            let _ = provider.shutdown();
        });
    }

    /// `if: false` skip path returns before update_context_by_input, so the
    /// post-`from` recording does not run. The skipped task's span must
    /// still carry workflow.task.input / workflow.task.position so traces
    /// can show *which* task was skipped and what its input would have been.
    #[test]
    fn test_skip_if_task_records_span_input_and_position() {
        use opentelemetry::Value as OtelValue;
        use opentelemetry::global;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

        struct ProviderGuard {
            saved: Option<opentelemetry::global::GlobalTracerProvider>,
        }
        impl Drop for ProviderGuard {
            fn drop(&mut self) {
                if let Some(p) = self.saved.take() {
                    opentelemetry::global::set_tracer_provider(p);
                }
            }
        }
        let _guard = ProviderGuard {
            saved: Some(global::tracer_provider()),
        };

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            use crate::workflow::execute::workflow::WorkflowExecutor;

            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            global::set_tracer_provider(provider.clone());

            let app_module = Arc::new(create_hybrid_test_app().await.unwrap());

            let workflow_json = serde_json::json!({
                "document": {
                    "dsl": "1.0.0",
                    "namespace": "test",
                    "name": "skip-if-tracing",
                    "version": "1.0.0",
                    "metadata": {}
                },
                "input": {
                    "schema": {
                        "document": {
                            "type": "object",
                            "properties": { "k": {"type": "string"} }
                        }
                    }
                },
                "do": [
                    {
                        "always_skipped": {
                            "if": "${ false }",
                            "set": { "x": 1 }
                        }
                    }
                ]
            });

            let workflow =
                Arc::new(serde_json::from_value::<WorkflowSchema>(workflow_json).unwrap());
            let input = Arc::new(serde_json::json!({ "k": "v" }));
            let context = Arc::new(serde_json::json!({}));
            let workflow_context = Arc::new(RwLock::new(WorkflowContext::new(
                &workflow,
                input.clone(),
                context,
                None,
            )));
            let executor = WorkflowExecutor {
                default_task_timeout_sec: 30,
                job_executors: Arc::new(JobExecutorWrapper::new(app_module)),
                workflow: workflow.clone(),
                workflow_context: workflow_context.clone(),
                execution_id: None,
                metadata: Arc::new(HashMap::new()),
                checkpoint_repository: None,
            };

            let workflow_stream =
                executor.execute_workflow(Arc::new(opentelemetry::Context::current()));
            tokio::pin!(workflow_stream);
            while (workflow_stream.next().await).is_some() {}

            let _ = provider.force_flush();
            let spans = exporter.get_finished_spans().expect("get spans");

            let skipped_span = spans
                .iter()
                .find(|s| s.name.as_ref().contains("always_skipped"))
                .expect("expected a span for the skipped task");

            let mut has_input = false;
            let mut has_position = false;
            for kv in &skipped_span.attributes {
                if kv.key.as_str() == "workflow.task.input"
                    && let OtelValue::String(_) = &kv.value
                {
                    has_input = true;
                }
                if kv.key.as_str() == "workflow.task.position"
                    && let OtelValue::String(_) = &kv.value
                {
                    has_position = true;
                }
            }
            assert!(
                has_input,
                "skipped task span must still carry workflow.task.input"
            );
            assert!(
                has_position,
                "skipped task span must still carry workflow.task.position"
            );

            let _ = provider.shutdown();
        });
    }
}
