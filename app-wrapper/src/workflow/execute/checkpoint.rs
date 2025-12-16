use crate::workflow::execute::context::{TaskContext, WorkflowContext, WorkflowPosition};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::{
    inline_workflow_args, reusable_workflow_args, workflow_run_args,
};
use std::sync::Arc;

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

    /// Create checkpoint with explicit position (for HITL wait)
    /// Used when saving checkpoint with next task's position instead of current
    pub async fn new_with_position(
        workflow: &WorkflowContext,
        task: &TaskContext,
        position: WorkflowPosition,
    ) -> Self {
        Self {
            workflow: WorkflowCheckPointContext::new(workflow).await,
            task: TaskCheckPointContext::new(task).await,
            position,
        }
    }
    pub fn from_inline(checkpoint: &inline_workflow_args::Checkpoint) -> Result<Self> {
        match checkpoint.data.as_ref() {
            Some(d) => Ok(CheckPointContext {
                workflow: d
                    .workflow
                    .as_ref()
                    .map(|w| WorkflowCheckPointContext {
                        name: w.name.clone(),
                        input: serde_json::from_str(&w.input).unwrap_or_default(),
                        context_variables: serde_json::from_str(&w.context_variables)
                            .unwrap_or_default(),
                    })
                    .ok_or(anyhow::anyhow!("Workflow context is missing"))?,
                task: d
                    .task
                    .as_ref()
                    .map(|t| TaskCheckPointContext {
                        input: serde_json::from_str(&t.input).unwrap_or_default(),
                        output: serde_json::from_str(&t.output).unwrap_or_default(),
                        context_variables: serde_json::from_str(&t.context_variables)
                            .unwrap_or_default(),
                        flow_directive: t.flow_directive.clone(),
                    })
                    .ok_or(anyhow::anyhow!("Task context is missing"))?,
                position: WorkflowPosition::parse(&checkpoint.position)?,
            }),
            None => Err(anyhow::anyhow!("Checkpoint data is missing")),
        }
    }
    pub fn from_reusable(checkpoint: &reusable_workflow_args::Checkpoint) -> Result<Self> {
        match checkpoint.data.as_ref() {
            Some(d) => Ok(CheckPointContext {
                workflow: d
                    .workflow
                    .as_ref()
                    .map(|w| WorkflowCheckPointContext {
                        name: w.name.clone(),
                        input: serde_json::from_str(&w.input).unwrap_or_default(),
                        context_variables: serde_json::from_str(&w.context_variables)
                            .unwrap_or_default(),
                    })
                    .ok_or(anyhow::anyhow!("Workflow context is missing"))?,
                task: d
                    .task
                    .as_ref()
                    .map(|t| TaskCheckPointContext {
                        input: serde_json::from_str(&t.input).unwrap_or_default(),
                        output: serde_json::from_str(&t.output).unwrap_or_default(),
                        context_variables: serde_json::from_str(&t.context_variables)
                            .unwrap_or_default(),
                        flow_directive: t.flow_directive.clone(),
                    })
                    .ok_or(anyhow::anyhow!("Task context is missing"))?,
                position: WorkflowPosition::parse(&checkpoint.position)?,
            }),
            None => Err(anyhow::anyhow!("Checkpoint data is missing")),
        }
    }

    pub fn from_workflow_run(checkpoint: &workflow_run_args::Checkpoint) -> Result<Self> {
        match checkpoint.data.as_ref() {
            Some(d) => Ok(CheckPointContext {
                workflow: d
                    .workflow
                    .as_ref()
                    .map(|w| WorkflowCheckPointContext {
                        name: w.name.clone(),
                        input: serde_json::from_str(&w.input).unwrap_or_default(),
                        context_variables: serde_json::from_str(&w.context_variables)
                            .unwrap_or_default(),
                    })
                    .ok_or(anyhow::anyhow!("Workflow context is missing"))?,
                task: d
                    .task
                    .as_ref()
                    .map(|t| TaskCheckPointContext {
                        input: serde_json::from_str(&t.input).unwrap_or_default(),
                        output: serde_json::from_str(&t.output).unwrap_or_default(),
                        context_variables: serde_json::from_str(&t.context_variables)
                            .unwrap_or_default(),
                        flow_directive: t.flow_directive.clone(),
                    })
                    .ok_or(anyhow::anyhow!("Task context is missing"))?,
                position: WorkflowPosition::parse(&checkpoint.position)?,
            }),
            None => Err(anyhow::anyhow!("Checkpoint data is missing")),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct WorkflowCheckPointContext {
    pub name: String,
    pub input: Arc<serde_json::Value>,
    pub context_variables: Arc<serde_json::Map<String, serde_json::Value>>,
}
impl WorkflowCheckPointContext {
    pub async fn new(workflow: &WorkflowContext) -> Self {
        Self {
            name: workflow.name.clone(),
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
    pub flow_directive: String,
}
impl TaskCheckPointContext {
    pub async fn new(task: &TaskContext) -> Self {
        Self {
            input: task.input.clone(),
            output: task.output.clone(),
            context_variables: Arc::new(task.context_variables.lock().await.clone()),
            flow_directive: task.flow_directive.to_string(),
        }
    }
}
