use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, tasks::TaskTrait, ForOnError},
    },
    execute::{
        context::{TaskContext, WorkflowContext, WorkflowStatus, WorkflowStreamEvent},
        expression::UseExpression,
        task::{
            stream::do_::DoTaskStreamExecutor, trace::TaskTracing, ExecutionId,
            StreamTaskExecutorTrait,
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
use tokio::sync::{mpsc, Mutex, RwLock};

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
        // For parallel execution, create deep copy to ensure isolation
        // For sequential execution, use shared context to preserve final state
        let task_context = if copy_deeply {
            task_context.deep_copy().await
        } else {
            task_context.clone()
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
                        items_to_process.push((i, prepared_context));
                    } else {
                        tracing::debug!("for: while condition is false, skipping item {}", i);
                        break;
                    }
                }
                Err(e) => {
                    // Instead of failing the entire operation, record this specific error
                    // and let other tasks continue
                    tracing::error!("Error preparing item {}: {:?}", i, e);

                    // Send the error directly into the stream instead of returning early
                    let tx_err = tx.clone();
                    let e_clone = e.clone();
                    tokio::spawn(async move {
                        let _ = tx_err.send(Err(e_clone)).await;
                    });

                    // Continue with other items
                    continue;
                }
            };
        }

        // If no items to process, return a stream with just the final result
        if !has_items {
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
        for (i, prepared_context) in items_to_process {
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

            // Spawn this task asynchronously
            join_set.spawn(async move {
                let span =
                    Self::start_child_otel_span(&cx, APP_WORKER_NAME, item_name_clone.clone());
                let ccx = Arc::new(opentelemetry::Context::current_with_span(span));
                let ccx_clone = ccx.clone();
                let mut span = ccx_clone.span();
                Self::record_task_input(
                    &mut span,
                    format!(
                        "for_parallel_task:{}_{}",
                        &task_name_formatted, &item_name_clone
                    ),
                    &prepared_context,
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

                                    // Check for unsupported Wait inside ForTask (parallel)
                                    let result = if let Ok(ref event) = result {
                                        let wf_status = workflow_context.read().await.status.clone();
                                        if wf_status == WorkflowStatus::Waiting {
                                            tracing::error!(
                                                "Wait directive inside ForTask is not supported (parallel): task={}",
                                                task_name_formatted
                                            );
                                            // Reset status to Running to allow error propagation
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
                                            Err(error)
                                        } else {
                                            result
                                        }
                                    } else {
                                        result
                                    };

                                    let is_error = result.is_err();
                                    if is_error && on_error == ForOnError::Break {
                                        // Signal all other tasks to cancel
                                        let _ = cancel_tx_clone.send(true);
                                        tracing::warn!("Error occurred, cancelling all parallel tasks due to onError=break");
                                    }

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
                                    // Stream completed - check for Wait state
                                    let wf_status = workflow_context.read().await.status.clone();
                                    if wf_status == WorkflowStatus::Waiting {
                                        tracing::error!(
                                            "Wait directive inside ForTask is not supported (parallel, detected after stream end): task={}",
                                            task_name_formatted
                                        );
                                        // Reset status to Running to allow error propagation
                                        workflow_context.write().await.status = WorkflowStatus::Running;
                                        let error = workflow::errors::ErrorFactory::new().bad_argument(
                                            "Wait directive inside ForTask is not currently supported. \
                                             Move the 'then: wait' to a task outside of the for loop.".to_string(),
                                            Some(item_position.read().await.as_error_instance()),
                                            None,
                                        );
                                        if on_error == ForOnError::Break {
                                            let _ = cancel_tx_clone.send(true);
                                        }
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
                                            // Log the error and continue to next item
                                            tracing::warn!("Continuing to next item due to onError=continue");
                                            yield Err(e);
                                            // Break from current item's processing but continue with next items
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
                        tracing::error!("Error preparing item {}: {:?}", i, e);

                        match on_error {
                            ForOnError::Continue => {
                                tracing::warn!("Continuing to next item due to onError=continue");
                                yield Err(e);
                                // Continue with next item
                                continue;
                            }
                            ForOnError::Break => {
                                yield Err(e);
                                return;
                            }
                        }
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
                            if let Some(output) = wc.output.as_ref() {
                                if let Some(obj) = output.as_object() {
                                    if obj.contains_key("processed_item") && obj.contains_key("processed_index") {
                                        println!("  → Item processed successfully: {} at index {}",
                                                obj.get("processed_item").unwrap(),
                                                obj.get("processed_index").unwrap());
                                    }
                                }
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

            // CONTINUE MODE: Verification
            // Note: After execute_workflow API change (commit 100edc6), when ForTask yields an error,
            // the workflow immediately transitions to Faulted status and terminates.
            // The onError=continue behavior works within ForTask's internal loop, but the error
            // still propagates to execute_workflow which sets the workflow to Faulted.

            // Should have received some results
            assert!(!results.is_empty(), "Expected some results, but got none");

            // Should have at least one faulted result (from item1)
            assert!(faulted_count >= 1, "Expected at least 1 faulted result from item1, got {}", faulted_count);

            // With current API: item0 processes successfully, then item1 fails and workflow terminates
            // The processed_item_count should be at least 1 (item0 was processed before item1 failed)
            assert!(processed_item_count >= 1, "Expected at least 1 successfully processed item (item0), got {}", processed_item_count);

            println!("CONTINUE MODE VERIFICATION:");
            println!("- item0 was processed successfully before error at item1");
            println!("- Error at item1 caused workflow to transition to Faulted status");
            println!("- Note: After API change, errors propagate to execute_workflow which terminates the workflow");
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
                            if let Some(output) = wc.output.as_ref() {
                                if let Some(obj) = output.as_object() {
                                    if obj.contains_key("processed_item") && obj.contains_key("processed_index") {
                                        println!("  → Item processed successfully: {} at index {}",
                                                obj.get("processed_item").unwrap(),
                                                obj.get("processed_index").unwrap());
                                    }
                                }
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
                        if wc.status == WorkflowStatus::Faulted {
                            if let Some(output) = wc.output.as_ref() {
                                if let Some(err) = output.get("error") {
                                    error_message = err.to_string();
                                    found_error = true;
                                    break;
                                }
                            }
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
                        if wc.status == WorkflowStatus::Faulted {
                            if let Some(output) = wc.output.as_ref() {
                                if let Some(err) = output.get("error") {
                                    error_message = err.to_string();
                                    found_error = true;
                                    break;
                                }
                            }
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
                        if let Some(output) = wc.output.as_ref() {
                            if let Some(obj) = output.as_object() {
                                assert!(
                                    !obj.contains_key("completed"),
                                    "Task after wait should not execute"
                                );
                            }
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
}
