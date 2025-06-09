use std::sync::Arc;

use crate::workflow::{
    definition::workflow,
    execute::context::{TaskContext, Then, WorkflowContext, WorkflowPosition},
};
use uuid::Uuid;

pub mod repository;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CheckPointContext {
    pub workflow: WorkflowCheckPointContext,
    pub task: TaskCheckPointContext,
    pub position: WorkflowPosition,
}
impl CheckPointContext {
    pub async fn new(workflow: WorkflowContext, task: TaskContext) -> Self {
        let position = task.position.read().await.clone();
        Self {
            workflow: WorkflowCheckPointContext::new(workflow).await,
            task: TaskCheckPointContext::new(task).await,
            position,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowCheckPointContext {
    pub id: Uuid,
    pub definition: Arc<workflow::WorkflowSchema>,
    pub input: Arc<serde_json::Value>,
    pub output: Option<Arc<serde_json::Value>>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
}
impl WorkflowCheckPointContext {
    pub async fn new(workflow: WorkflowContext) -> Self {
        Self {
            id: workflow.id,
            definition: workflow.definition,
            input: workflow.input,
            output: workflow.output,
            context_variables: Arc::new(workflow.context_variables.lock().await.clone()),
        }
    }
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TaskCheckPointContext {
    pub raw_input: Arc<serde_json::Value>,
    pub input: Arc<serde_json::Value>,
    pub raw_output: Arc<serde_json::Value>,
    pub output: Arc<serde_json::Value>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
    pub flow_directive: Then,
}
impl TaskCheckPointContext {
    pub async fn new(task: TaskContext) -> Self {
        Self {
            raw_input: task.raw_input,
            input: task.input,
            raw_output: task.raw_output,
            output: task.output,
            context_variables: Arc::new(task.context_variables.lock().await.clone()),
            flow_directive: task.flow_directive,
        }
    }
}
