use std::sync::Arc;

use crate::workflow::execute::context::{TaskContext, WorkflowContext, WorkflowPosition};
use uuid::Uuid;

pub mod repository;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CheckPointContext {
    pub workflow: WorkflowCheckPointContext,
    pub task: TaskCheckPointContext,
    pub position: WorkflowPosition,
}
impl CheckPointContext {
    pub async fn new(workflow: &WorkflowContext, task: &TaskContext) -> Self {
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
    pub input: Arc<serde_json::Value>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
}
impl WorkflowCheckPointContext {
    pub async fn new(workflow: &WorkflowContext) -> Self {
        Self {
            id: workflow.id.clone(),
            input: workflow.input.clone(),
            context_variables: Arc::new(workflow.context_variables.lock().await.clone()),
        }
    }
}
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TaskCheckPointContext {
    pub input: Arc<serde_json::Value>,
    pub output: Arc<serde_json::Value>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
}
impl TaskCheckPointContext {
    pub async fn new(task: &TaskContext) -> Self {
        Self {
            input: task.input.clone(),
            output: task.output.clone(),
            context_variables: Arc::new(task.context_variables.lock().await.clone()),
        }
    }
}
