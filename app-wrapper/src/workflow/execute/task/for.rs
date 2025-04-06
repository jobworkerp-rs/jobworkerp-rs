use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow::{self},
    },
    execute::{
        context::{TaskContext, UseExpression, WorkflowContext},
        job::JobExecutorWrapper,
        task::do_::DoTaskExecutor,
    },
};
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::TaskExecutorTrait;

pub struct ForTaskExecutor<'a> {
    task: &'a workflow::ForTask,
    job_executor_wrapper: Arc<JobExecutorWrapper>,
}
impl UseExpression for ForTaskExecutor<'_> {}
impl UseJqAndTemplateTransformer for ForTaskExecutor<'_> {}
impl UseExpressionTransformer for ForTaskExecutor<'_> {}

impl<'a> ForTaskExecutor<'a> {
    pub fn new(task: &'a workflow::ForTask, job_executor_wrapper: Arc<JobExecutorWrapper>) -> Self {
        Self {
            task,
            job_executor_wrapper,
        }
    }
}
impl TaskExecutorTrait for ForTaskExecutor<'_> {
    async fn execute(
        &self,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext> {
        tracing::debug!("ForTaskExecutor: {}", task_name);
        let workflow::ForTask {
            for_,
            do_,
            while_,
            // export,
            metadata,
            // input,
            // output,
            // then,
            // timeout,
            ..
        } = self.task;
        let expression = self
            .expression(
                &*(workflow_context.read().await),
                Arc::new(task_context.clone()),
            )
            .await?;
        tracing::debug!("expression: {:#?}", &expression);
        let transformed_in_items = Self::transform_value(
            task_context.input.clone(),
            serde_json::Value::String(for_.in_.clone()), // XXX clone loop items
            &expression,
        )?;
        let do_task = Arc::new(workflow::DoTask {
            do_: do_.clone(),
            metadata: metadata.clone(),
            ..Default::default()
        });
        tracing::debug!("do task: {:#?}", &do_task);
        tracing::debug!("for in items: {:#?}", transformed_in_items);
        let mut out_vec = Vec::new();
        if transformed_in_items.is_array() {
            let mut i = 0;
            let item_name = if for_.each.is_empty() {
                "item".to_string()
            } else {
                for_.each.clone()
            };
            let index_name = if for_.at.is_empty() {
                "index".to_string()
            } else {
                for_.at.clone()
            };
            for item in transformed_in_items.as_array().unwrap() {
                tracing::debug!("item[{}]: {:#?}", i, &item);
                task_context
                    .add_context_value(item_name.clone(), item.clone())
                    .await; // XXX clone loop item twice
                task_context
                    .add_context_value(
                        index_name.clone(),
                        serde_json::Value::Number(serde_json::Number::from(i)),
                    )
                    .await;
                // XXX clone task context twice
                let expression = self
                    .expression(
                        &*(workflow_context.read().await),
                        Arc::new(task_context.clone()),
                    )
                    .await?;
                i += 1;
                let input = task_context.input.clone();
                let while_cond = while_
                    .as_ref()
                    .map(|w| {
                        Self::transform_value(
                            input,
                            serde_json::Value::String(w.clone()),
                            &expression,
                        )
                    })
                    .unwrap_or(Ok(serde_json::Value::Bool(true)))?;
                tracing::debug!("while: {:?}: {}", &while_, &while_cond);
                if !Self::eval_as_bool(&while_cond) {
                    break;
                }
                let do_executor = DoTaskExecutor::new(&do_task, self.job_executor_wrapper.clone());
                let r = do_executor
                    .execute(
                        format!("{}_{}", task_name, &(i - 1)).as_str(),
                        workflow_context.clone(),
                        task_context,
                    )
                    .await?;
                tracing::debug!("do result[{}]: {:#?}", i - 1, &r.raw_output);
                out_vec.push((*r.raw_output).clone());
                task_context = r;
            }
            task_context.remove_context_value(&item_name).await;
            task_context.remove_context_value(&index_name).await;
        } else {
            tracing::warn!(
                "Invalid for 'in' items(not array): {:#?}",
                transformed_in_items
            );
        }
        tracing::debug!("for result: {:#?}", &out_vec);
        task_context.set_raw_output(serde_json::Value::Array(out_vec));

        Ok(task_context)
    }
}
