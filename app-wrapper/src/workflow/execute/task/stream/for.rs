use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self, tasks::TaskTrait},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        task::{
            stream::do_::DoTaskStreamExecutor, trace::TaskTracing, ExecutionId,
            StreamTaskExecutorTrait,
        },
    },
};
use anyhow::Result;
use app::app::job::execute::JobExecutorWrapper;
use futures::stream::{self, Stream, StreamExt};
use net_utils::{net::reqwest, trace::Tracing};
use jobworkerp_base::APP_WORKER_NAME;
use opentelemetry::trace::TraceContextExt;
use std::{collections::HashMap, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{mpsc, Mutex, RwLock};

pub struct ForTaskStreamExecutor {
    workflow_context: Arc<RwLock<WorkflowContext>>,
    default_timeout: Duration,
    task: workflow::ForTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    http_client: reqwest::ReqwestClient,
    checkpoint_repository: Option<
        Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
    >,
    execution_id: Option<Arc<ExecutionId>>,
    // Metadata for the task, can be used for logging or tracing
    metadata: Arc<HashMap<String, String>>,
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
        http_client: reqwest::ReqwestClient,
        checkpoint_repository: Option<
            Arc<dyn crate::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId>,
        >,
        execution_id: Option<Arc<ExecutionId>>,
        metadata: Arc<HashMap<String, String>>,
    ) -> Self {
        Self {
            workflow_context,
            default_timeout,
            task,
            job_executor_wrapper,
            http_client,
            checkpoint_repository,
            execution_id,
            metadata,
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
    ) -> Result<(TaskContext, serde_json::Value), Box<workflow::Error>> {
        // Create a completely independent deep copy to ensure each iteration has its own isolated context
        // This uses our copy() method which creates new Arc instances for all values
        let task_context = task_context.deep_copy().await;

        // Add the current item to the context
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
        Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
        Box<workflow::Error>,
    > {
        tracing::debug!(
            "[FOR]Processing {} items in parallel (streaming)",
            items.len()
        );
        let original_context = task_context.clone();

        // Larger buffer to reduce back-pressure
        let (tx, rx) = mpsc::channel(128);

        let mut has_items = false;

        // First pass - prepare all items and determine which ones should be processed
        let mut items_to_process = Vec::new();

        for (i, item) in items.iter().enumerate() {
            match self
                .prepare_for_item(item, i, item_name, index_name, task_context, while_)
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
            return Ok(stream::once(async move { Ok(final_ctx) }).boxed());
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
            let http_client_clone = self.http_client.clone();
            let workflow_context = self.workflow_context.clone();
            let task_name_formatted = Arc::new(format!("{task_name}_{i}"));
            let item_name_clone = item_name.to_string();
            let cx = cx.clone();
            let meta = self.metadata.clone();
            let checkpoint_repository = self.checkpoint_repository.clone();
            let execution_id = self.execution_id.clone();
            let default_timeout = self.default_timeout;

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

                // let span = Self::create_task_span(
                //     &cx,
                //     item_name_clone.clone(),
                //     &format!(
                //         "for_parallel_task:{}_{}",
                //         &task_name_formatted, &item_name_clone
                //     ),
                //     &prepared_context,
                // );
                // let _ = span.enter();
                // let ccx = span.context();

                let start_time = std::time::Instant::now();

                tracing::debug!("[PARALLEL] Task {} starting at t=0ms", &task_name_formatted);

                // Create executor for this task
                let do_stream_executor = DoTaskStreamExecutor::new(
                    workflow_context.clone(),
                    default_timeout,
                    meta.clone(),
                    do_task_clone,
                    job_executor_wrapper_clone,
                    http_client_clone,
                    checkpoint_repository.clone(),
                    execution_id,
                );

                // Execute the stream and track results
                let stream = do_stream_executor
                    .execute_stream(ccx, task_name_formatted.clone(), prepared_context)
                    .boxed();

                tokio::pin!(stream);

                let mut result_count = 0;
                while let Some(result) = stream.next().await {
                    result_count += 1;
                    let elapsed_ms = start_time.elapsed().as_millis();

                    tracing::debug!(
                        "[PARALLEL] Task {} yielding result #{} at t={}ms",
                        task_name_formatted.clone(),
                        result_count,
                        elapsed_ms
                    );
                    Self::record_result(&span, result.as_ref());
                    if tx.send(result).await.is_err() {
                        tracing::error!(
                            "Channel closed while sending results for task {}",
                            &task_name_formatted
                        );
                        Self::record_error(&span, "Channel closed while sending results");
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

        // Convert the receiver into a stream
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let join_set = Arc::new(Mutex::new(join_set));
        let item_name = item_name.to_string();
        let index_name = index_name.to_string();
        let original_context = original_context.clone();

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

                Ok(final_ctx)
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
        Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
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
                Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
                Box<workflow::Error>,
            >(stream::once(async move { Ok(final_ctx) }).boxed());
        }

        // Create streams for each item that passes the while condition
        // We'll process these streams one after another in sequence
        let mut item_streams = Vec::new();
        let mut keeps = Vec::new();
        let mut has_items = false;

        for (i, item) in items.iter().enumerate() {
            match self
                .prepare_for_item(item, i, &item_name, &index_name, &task_context, &while_)
                .await
            {
                Ok((prepared_context, while_cond)) => {
                    // let span = Self::child_tracing_span(
                    //     &cx.clone(),
                    //     APP_NAME,
                    //     format!("for_task_{}:{}_{}", task_name, item_name, i),
                    // );
                    // let _ = span.enter();
                    // let cx = span.context();
                    let span = Self::start_child_otel_span(
                        &cx.clone(),
                        APP_WORKER_NAME,
                        format!("for_task_{task_name}:{item_name}_{i}"),
                    );
                    let cx = opentelemetry::Context::current_with_span(span);

                    if !Self::eval_as_bool(&while_cond) {
                        tracing::debug!("for: while condition is false, skipping item {}", i);
                        break;
                    }

                    has_items = true;

                    // Create a do task executor for this item
                    let task_name_formatted = Arc::new(format!("{task_name}_{i}"));
                    let do_stream_executor = Arc::new(DoTaskStreamExecutor::new(
                        self.workflow_context.clone(),
                        self.default_timeout,
                        self.metadata.clone(),
                        do_task.clone(), //XXX clone
                        self.job_executor_wrapper.clone(),
                        self.http_client.clone(),
                        self.checkpoint_repository.clone(),
                        self.execution_id.clone(),
                    ));

                    let tnf = task_name_formatted.clone();
                    keeps.push((tnf.clone(), do_stream_executor.clone()));

                    let tnf_clone = tnf.clone();
                    let do_stream_executor_clone = do_stream_executor.clone();
                    let cxc = Arc::new(cx);

                    let stream: Pin<
                        Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>,
                    > = Box::pin(async_stream::stream! {
                        let mut inner_stream = do_stream_executor_clone.execute_stream(
                            cxc.clone(),
                            tnf_clone,
                            prepared_context,
                        );
                        while let Some(item) = inner_stream.next().await {
                            yield item;
                        }
                    });

                    // Add this item's stream to our collection
                    item_streams.push(stream);
                }
                Err(e) => {
                    tracing::error!("Error preparing item {}: {:?}", i, e);

                    let e_clone = e.clone();
                    let error_stream: Pin<
                        Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>,
                    > = Box::pin(async_stream::stream! {
                        yield Err(e_clone);
                    });

                    // Add this error stream to our collection
                    // Continue with other items
                    item_streams.push(error_stream);
                    continue;
                }
            };
        }

        // No items passed the while condition check
        if !has_items {
            let mut final_ctx = original_context;
            final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
            final_ctx.remove_context_value(&item_name).await;
            final_ctx.remove_context_value(&index_name).await;
            final_ctx.remove_position().await;
            return Ok::<
                Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
                Box<workflow::Error>,
            >(stream::once(async move { Ok(final_ctx) }).boxed());
        }

        let stream = Box::pin(async_stream::stream! {
            // Process each item's stream completely before moving to the next
            for stream in item_streams {
                tokio::pin!(stream);
                while let Some(result) = stream.next().await {
                    yield result;
                }
            }
            // XXX workaround: for last result sent
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            let final_ctx = original_context;
            let _exec = keeps;
            final_ctx.remove_context_value(&item_name).await;
            final_ctx.remove_context_value(&index_name).await;
            final_ctx.remove_position().await;

            yield Ok(final_ctx);
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
    ) -> impl futures::Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send {
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
                        final_ctx.remove_position().await;

                        // Return the single result and exit
                        yield Ok(final_ctx);
                        return;
                    }

                    // Extract parameters from task
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

                    // Get items from the transformed input
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
