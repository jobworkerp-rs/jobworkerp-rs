use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        job::JobExecutorWrapper,
        task::{stream::do_::DoTaskStreamExecutor, StreamTaskExecutorTrait},
    },
};
use anyhow::Result;
use futures::stream::{self, Stream, StreamExt};
use infra_utils::infra::net::reqwest;
use std::{pin::Pin, sync::Arc};
use tokio::sync::{mpsc, RwLock};

pub struct ForStreamTaskExecutor {
    task: workflow::ForTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    http_client: reqwest::ReqwestClient,
}
impl UseExpression for ForStreamTaskExecutor {}
impl UseJqAndTemplateTransformer for ForStreamTaskExecutor {}
impl UseExpressionTransformer for ForStreamTaskExecutor {}

impl ForStreamTaskExecutor {
    pub fn new(
        task: workflow::ForTask,
        job_executor_wrapper: Arc<JobExecutorWrapper>,
        http_client: reqwest::ReqwestClient,
    ) -> Self {
        Self {
            task,
            job_executor_wrapper,
            http_client,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_for_item(
        &self,
        item: &serde_json::Value,
        i: usize,
        item_name: &str,
        index_name: &str,
        workflow_context: &Arc<RwLock<WorkflowContext>>,
        task_context: &TaskContext,
        while_: &Option<String>,
    ) -> Result<(TaskContext, serde_json::Value), Box<workflow::Error>> {
        let task_context = task_context.clone();
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
            &*(workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.lock().await.clone();
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
                let mut pos = task_context.position.lock().await.clone();
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
        workflow_context: &Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> Result<(TaskContext, serde_json::Value, workflow::DoTask), Box<workflow::Error>> {
        tracing::debug!("ForStreamTaskExecutor: {}", task_name);
        let workflow::ForTask {
            for_,
            do_,
            metadata,
            ..
        } = &self.task;

        task_context.add_position_name("for".to_string()).await;

        let expression = match Self::expression(
            &*(workflow_context.read().await),
            Arc::new(task_context.clone()),
        )
        .await
        {
            Ok(e) => e,
            Err(mut e) => {
                let pos = task_context.position.lock().await.clone();
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
            Err(e) => return Err(e),
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
    async fn process_items_in_parallel_stream(
        &self,
        items: &[serde_json::Value],
        item_name: &str,
        index_name: &str,
        workflow_context: &Arc<RwLock<WorkflowContext>>,
        task_context: &TaskContext,
        do_task: workflow::DoTask,
        task_name: &str,
        while_: &Option<String>,
        original_context: TaskContext, // Original context for finalizing
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
        Box<workflow::Error>,
    > {
        tracing::debug!("Processing {} items in parallel (streaming)", items.len());

        // Create an mpsc channel to collect results from multiple streams
        let (tx, rx) = mpsc::channel(32); // Buffer size of 32 should be sufficient for most use cases

        let mut has_items = false;

        // Create tasks for each item that will be processed in parallel
        let mut tasks = Vec::new();

        for (i, item) in items.iter().enumerate() {
            let (prepared_context, while_cond) = match self
                .prepare_for_item(
                    item,
                    i,
                    item_name,
                    index_name,
                    workflow_context,
                    task_context,
                    while_,
                )
                .await
            {
                Ok(result) => result,
                Err(e) => return Err(e),
            };

            if !Self::eval_as_bool(&while_cond) {
                tracing::debug!("for: while condition is false, skipping item {}", i);
                continue;
            }

            has_items = true;

            // Create a clone of the sender for each task
            let tx = tx.clone();

            // Create executor with all required values
            let do_stream_executor = DoTaskStreamExecutor::new(
                do_task.clone(),
                self.job_executor_wrapper.clone(),
                self.http_client.clone(),
            );

            let task_name_formatted = format!("{}_{}", task_name, i);
            let workflow_ctx_clone = workflow_context.clone();

            // Spawn a task for each item
            let task = tokio::spawn(async move {
                // Execute the stream for this item
                let mut stream = do_stream_executor.execute_stream(
                    &task_name_formatted,
                    workflow_ctx_clone,
                    prepared_context,
                );

                // Forward all results from this stream to the channel
                while let Some(result) = stream.next().await {
                    if tx.send(result).await.is_err() {
                        // Channel closed, receiver dropped
                        break;
                    }
                }
            });

            tasks.push(task);
        }

        // If no items to process, return a stream with just the final result
        if !has_items {
            let mut original_context = original_context;
            original_context.set_raw_output(serde_json::Value::Array(vec![]));
            original_context.remove_context_value(item_name).await;
            original_context.remove_context_value(index_name).await;
            original_context.remove_position().await;
            return Ok(stream::once(async move { Ok(original_context) }).boxed());
        }

        // Drop the original sender so the receiver will close when all tasks are done
        drop(tx);

        // Spawn a task to await all the processing tasks and ensure they complete
        let cleanup = tokio::spawn(async move {
            // Wait for all tasks to complete
            for task in tasks {
                // Ignore errors, we're just ensuring tasks complete
                let _ = task.await;
            }
        });

        // Convert the receiver into a stream
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);

        // For parallel processing, we don't need to collect all outputs
        // Instead, we'll just append a final item to the stream that resets the context
        let item_name = item_name.to_string();
        let index_name = index_name.to_string();

        // Chain with the final result that will just clean up the context
        // This final item doesn't replace previous results, it just adds one more to the stream
        // after all parallel tasks have completed
        let final_stream = stream
            .chain(stream::once(async move {
                // Ensure cleanup task completes
                let _ = cleanup.await;

                // Create final result that just cleans up the context
                // This doesn't override or replace any of the previous results that were streamed
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
    async fn process_items_sequentially_stream(
        &self,
        items: Vec<serde_json::Value>,
        item_name: String,
        index_name: String,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
        do_task: workflow::DoTask,
        task_name: String,
        while_: Option<String>,
        original_context: TaskContext, // Original context for finalizing
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send>>,
        Box<workflow::Error>,
    > {
        // No items to process, return a stream with just the final result
        if items.is_empty() {
            let mut final_ctx = original_context;
            final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
            final_ctx.remove_context_value(&item_name).await;
            final_ctx.remove_context_value(&index_name).await;
            final_ctx.remove_position().await;
            return Ok(Box::pin(stream::once(async move { Ok(final_ctx) })));
        }

        // Create streams for each item that passes the while condition
        // We'll process these streams one after another in sequence
        let mut item_streams = Vec::new();
        let mut keeps = Vec::new();
        let mut has_items = false;

        for (i, item) in items.iter().enumerate() {
            let (prepared_context, while_cond) = match self
                .prepare_for_item(
                    item,
                    i,
                    &item_name,
                    &index_name,
                    &workflow_context,
                    &task_context,
                    &while_,
                )
                .await
            {
                Ok(result) => result,
                Err(e) => return Err(e),
            };

            if !Self::eval_as_bool(&while_cond) {
                tracing::debug!("for: while condition is false, skipping item {}", i);
                continue;
            }

            has_items = true;

            // Create a do task executor for this item
            let task_name_formatted = Arc::new(format!("{}_{}", task_name, i));
            let do_stream_executor = Arc::new(DoTaskStreamExecutor::new(
                do_task.clone(),
                self.job_executor_wrapper.clone(),
                self.http_client.clone(),
            ));

            let tnf = task_name_formatted.clone();
            // Clone the workflow context for lifetime management
            keeps.push((tnf.clone(), do_stream_executor.clone()));
            // Create a stream for this item
            // let stream = do_stream_executor.execute_stream(
            //     tnf.as_str(),
            //     workflow_context.clone(),
            //     prepared_context,
            // );
            let tnf_clone = tnf.clone();
            let workflow_context_clone = workflow_context.clone();
            let prepared_context_clone = prepared_context.clone();
            let stream = Box::pin(async_stream::stream! {
                let mut inner_stream = do_stream_executor.execute_stream(
                    tnf_clone.as_str(),
                    workflow_context_clone,
                    prepared_context_clone,
                );
                while let Some(item) = inner_stream.next().await {
                    yield item;
                }
            });

            // Add this item's stream to our collection
            item_streams.push(stream);
        }

        // No items passed the while condition check
        if !has_items {
            let mut final_ctx = original_context;
            final_ctx.set_raw_output(serde_json::Value::Array(vec![]));
            final_ctx.remove_context_value(&item_name).await;
            final_ctx.remove_context_value(&index_name).await;
            final_ctx.remove_position().await;
            return Ok(Box::pin(stream::once(async move { Ok(final_ctx) })));
        }

        // Combine all streams in sequence by:
        // 1. Converting our Vec of streams into a stream of streams
        // 2. Flattening it to a single stream (keeping the order)
        // 3. Adding a final cleanup item
        let stream = futures::stream::iter(item_streams)
            .flatten()
            .chain(stream::once(async move {
                // Final cleanup context - just clean up variables and context
                let final_ctx = original_context;
                let _exec = keeps;

                // No need to set any array output - we're streaming results as they come
                // We'll just clean up our context variables
                final_ctx.remove_context_value(&item_name).await;
                final_ctx.remove_context_value(&index_name).await;
                final_ctx.remove_position().await;

                Ok(final_ctx)
            }));

        // Return the combined stream - it provides real-time results while processing
        Ok(stream.boxed())
    }
}

impl StreamTaskExecutorTrait<'_> for ForStreamTaskExecutor {
    fn execute_stream(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        task_context: TaskContext,
    ) -> impl Stream<Item = Result<TaskContext, Box<workflow::Error>>> + Send {
        let this = self;
        let task_name = task_name.to_string();

        // Create a one-shot async stream that handles initialization and sets up the real processing
        stream::once(async move {
            // Initialize execution
            let init_result = this
                .initialize_execution(&task_name, &workflow_context, task_context)
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

                        // Return a single-item stream with empty result
                        return stream::once(async move { Ok(final_ctx) }).boxed();
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
                                &items,
                                item_name,
                                index_name,
                                &workflow_context,
                                &task_context,
                                do_task,
                                &task_name,
                                while_,
                                task_context.clone(),
                            )
                            .await
                        {
                            Ok(stream) => stream.boxed(),
                            Err(e) => stream::once(async move { Err(e) }).boxed(),
                        }
                    } else {
                        // Sequential processing with real-time streaming
                        match this
                            .process_items_sequentially_stream(
                                items,
                                item_name.to_string(),
                                index_name.to_string(),
                                workflow_context,
                                task_context.clone(),
                                do_task,
                                task_name,
                                while_.clone(),
                                task_context,
                            )
                            .await
                        {
                            Ok(stream) => stream.boxed(),
                            Err(e) => stream::once(async move { Err(e) }).boxed(),
                        }
                    }
                }
                Err(e) => {
                    // Return error as a single item stream
                    stream::once(async move { Err(e) }).boxed()
                }
            }
        })
        .flatten() // Flatten the nested streams
    }
}
