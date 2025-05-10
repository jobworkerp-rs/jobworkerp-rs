use super::TaskExecutorTrait;
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self},
    },
    execute::{
        context::{TaskContext, WorkflowContext},
        expression::UseExpression,
        job::JobExecutorWrapper,
        task::do_::DoTaskExecutor,
    },
};
use anyhow::Result;
use futures::future::join_all;
use infra_utils::infra::net::reqwest;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ForTaskExecutor<'a> {
    task: &'a workflow::ForTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
    http_client: reqwest::ReqwestClient,
}
impl UseExpression for ForTaskExecutor<'_> {}
impl UseJqAndTemplateTransformer for ForTaskExecutor<'_> {}
impl UseExpressionTransformer for ForTaskExecutor<'_> {}

impl<'a> ForTaskExecutor<'a> {
    pub fn new(
        task: &'a workflow::ForTask,
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
}

impl TaskExecutorTrait<'_> for ForTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::debug!("ForTaskExecutor: {}", task_name);
        let workflow::ForTask {
            for_,
            do_,
            while_,
            in_parallel,
            metadata,
            ..
        } = self.task;
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
        let transformed_in_items = Self::transform_value(
            task_context.input.clone(),
            serde_json::Value::String(for_.in_.clone()),
            &expression,
        )?;
        let do_task = Arc::new(workflow::DoTask {
            do_: do_.clone(),
            metadata: metadata.clone(),
            ..Default::default()
        });
        tracing::debug!("for in items: {:#?}", transformed_in_items);
        let mut out_vec = Vec::new();
        if transformed_in_items.is_array() {
            let mut i = 0;
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
            let mut tasks = Vec::new();
            if *in_parallel {
                for item in transformed_in_items.as_array().unwrap() {
                    let (prepared_context, while_cond) = self
                        .prepare_for_item(
                            item,
                            i,
                            item_name,
                            index_name,
                            &workflow_context,
                            &task_context,
                            while_,
                        )
                        .await?;
                    i += 1;
                    if !Self::eval_as_bool(&while_cond) {
                        tracing::debug!("for: while condition is false, skipping item {}", i - 1);
                        continue;
                    }
                    // Clone Arc to be owned by each task
                    let do_task_cloned = do_task.clone();
                    let job_executor_wrapper = self.job_executor_wrapper.clone();
                    let http_client = self.http_client.clone();
                    let workflow_context = workflow_context.clone();
                    let task_index = i - 1;
                    let task_name_formatted = format!("{}_{}", task_name, &task_index);
                    tasks.push(async move {
                        let start = std::time::Instant::now();
                        tracing::debug!("Task {}: Starting execution", task_name_formatted);
                        // Create DoTaskExecutor inside the task to ensure ownership
                        // Convert Arc<DoTask> to Box<DoTask> with 'static lifetime to satisfy the lifetime requirement
                        let owned_do_task = Box::leak(Box::new((*do_task_cloned).clone()));
                        let do_executor =
                            DoTaskExecutor::new(owned_do_task, job_executor_wrapper, http_client);
                        let result = do_executor
                            .execute(&task_name_formatted, workflow_context, prepared_context)
                            .await;
                        let elapsed = start.elapsed();
                        tracing::debug!(
                            "Task {}: Finished execution in {:?}",
                            task_name_formatted,
                            elapsed
                        );
                        result
                    });
                }
                // Execute all tasks in parallel
                let results = join_all(tasks).await;
                // Variable to collect errors
                let mut errors = Vec::new();

                // Process all results
                for result in results {
                    match result {
                        Ok(r) => {
                            tracing::debug!("do result: {:#?}", &r.raw_output);
                            out_vec.push((*r.raw_output).clone());
                        }
                        Err(e) => {
                            tracing::error!("Task execution failed: {:?}", e);
                            errors.push(format!("{:?}", e));
                        }
                    }
                }

                // Report errors if any
                if !errors.is_empty() {
                    let pos = task_context.position.lock().await.clone();
                    return Err(workflow::errors::ErrorFactory::new().service_unavailable(
                        "Failed to execute task(s) in for".to_string(),
                        Some(&pos),
                        Some(format!("Errors: {}", errors.join("; "))),
                    ));
                }
            } else {
                for item in transformed_in_items.as_array().unwrap() {
                    let (prepared_context, while_cond) = self
                        .prepare_for_item(
                            item,
                            i,
                            item_name,
                            index_name,
                            &workflow_context,
                            &task_context,
                            while_,
                        )
                        .await?;
                    i += 1;
                    if !Self::eval_as_bool(&while_cond) {
                        tracing::debug!("for: while condition is false, skipping item {}", i - 1);
                        continue;
                    }
                    let do_executor = Arc::new(DoTaskExecutor::new(
                        &do_task,
                        self.job_executor_wrapper.clone(),
                        self.http_client.clone(),
                    ));
                    let task_name_formatted = format!("{}_{}", task_name, &(i - 1));
                    let r = do_executor
                        .execute(
                            task_name_formatted.as_str(),
                            workflow_context.clone(),
                            prepared_context.clone(),
                        )
                        .await?;
                    tracing::debug!("do result[{}]: {:#?}", i - 1, &r.raw_output);
                    out_vec.push((*r.raw_output).clone());
                    task_context = r;
                }
            }
            task_context.remove_context_value(item_name).await;
            task_context.remove_context_value(index_name).await;
        } else {
            tracing::warn!(
                "Invalid for 'in' items(not array): {:#?}",
                transformed_in_items
            );
        }
        tracing::debug!("for result: {:#?}", &out_vec);
        task_context.set_raw_output(serde_json::Value::Array(out_vec));
        task_context.remove_position().await;

        Ok(task_context)
    }
}
