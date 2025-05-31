use crate::workflow::execute::{
    context::{TaskContext, WorkflowContext},
    task::{Result, TaskExecutorTrait},
};
use crate::workflow::{
    definition::{
        transform::{UseExpressionTransformer, UseJqAndTemplateTransformer},
        workflow,
    },
    execute::expression::UseExpression,
};
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct SetTaskExecutor {
    task: workflow::SetTask,
}
impl SetTaskExecutor {
    pub fn new(task: workflow::SetTask) -> Self {
        Self { task }
    }
}

impl UseExpression for SetTaskExecutor {}
impl UseExpressionTransformer for SetTaskExecutor {}
impl UseJqAndTemplateTransformer for SetTaskExecutor {}

impl TaskExecutorTrait<'_> for SetTaskExecutor {
    async fn execute(
        &self,
        _cx: Arc<opentelemetry::Context>,
        task_name: &str,
        workflow_context: Arc<RwLock<WorkflowContext>>,
        mut task_context: TaskContext,
    ) -> Result<TaskContext, Box<workflow::Error>> {
        tracing::debug!("SetTaskExecutor: {}", task_name);
        task_context.add_position_name("set".to_string());
        let expression = Self::expression(
            &*workflow_context.read().await,
            Arc::new(task_context.clone()),
        )
        .await?;

        // export output to workflow context
        let set_values = match Self::transform_ref_map(
            task_context.input.clone(),
            &self.task.set,
            &expression,
        ) {
            Ok(v) => v,
            Err(mut e) => {
                let pos = task_context.position.clone();
                e.position(&pos);
                return Err(e);
            }
        };
        tracing::debug!("Transformed set task: {}: {:#?}", task_name, &set_values);
        match set_values.as_ref() {
            serde_json::Value::Object(map) => {
                for (key, value) in map.iter() {
                    workflow_context
                        .write()
                        .await
                        .add_context_value(key.clone(), value.clone())
                        .await;
                }
            }
            _ => {
                tracing::warn!("Export is not a map: {:#?}", &set_values);
            }
        }
        task_context.raw_output = set_values;
        task_context.remove_position();
        Ok(task_context)
    }
}
