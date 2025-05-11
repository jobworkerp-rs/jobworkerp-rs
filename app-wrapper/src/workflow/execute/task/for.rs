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
                let do_executor = Arc::new(DoTaskExecutor::new(
                    &do_task,
                    self.job_executor_wrapper.clone(),
                    self.http_client.clone(),
                ));
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
                        break;
                    }
                    let do_executor = do_executor.clone();
                    let workflow_context = workflow_context.clone();
                    let task_name_formatted = format!("{}_{}", task_name, &(i - 1));
                    tasks.push(Box::pin(async move {
                        do_executor
                            .execute(&task_name_formatted, workflow_context, prepared_context)
                            .await
                    }));
                }
                let results = join_all(tasks).await;
                for result in results {
                    match result {
                        Ok(r) => {
                            tracing::debug!("do result: {:#?}", &r.raw_output);
                            out_vec.push((*r.raw_output).clone());
                        }
                        Err(e) => return Err(e),
                    }
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
                        break;
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
