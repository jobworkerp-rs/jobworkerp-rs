//! State types and traits for AG-UI protocol.
//!
//! References AG-UI Rust SDK (ag-ui-core) state.rs

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Workflow execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStatus {
    /// Not yet started
    #[default]
    Pending,
    /// Currently executing
    Running,
    /// Waiting for external input (Human-in-the-Loop)
    Waiting,
    /// Successfully completed
    Completed,
    /// Failed with error
    Faulted,
    /// Cancelled by user
    Cancelled,
}

/// Workflow state snapshot for STATE_SNAPSHOT events.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowState {
    /// Workflow name
    pub workflow_name: String,

    /// Current status
    pub status: WorkflowStatus,

    /// Workflow input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,

    /// Context variables
    #[serde(default)]
    pub context_variables: HashMap<String, serde_json::Value>,

    /// Current task state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task: Option<TaskState>,

    /// Completed tasks
    #[serde(default)]
    pub completed_tasks: Vec<TaskState>,
}

impl WorkflowState {
    /// Create a new workflow state
    pub fn new(workflow_name: impl Into<String>) -> Self {
        Self {
            workflow_name: workflow_name.into(),
            status: WorkflowStatus::Pending,
            ..Default::default()
        }
    }

    /// Set status
    pub fn with_status(mut self, status: WorkflowStatus) -> Self {
        self.status = status;
        self
    }

    /// Set input
    pub fn with_input(mut self, input: serde_json::Value) -> Self {
        self.input = Some(input);
        self
    }

    /// Set current task
    pub fn with_current_task(mut self, task: TaskState) -> Self {
        self.current_task = Some(task);
        self
    }

    /// Set context variables
    pub fn with_context_variables(mut self, vars: HashMap<String, serde_json::Value>) -> Self {
        self.context_variables = vars;
        self
    }

    /// Add completed task
    pub fn add_completed_task(mut self, task: TaskState) -> Self {
        self.completed_tasks.push(task);
        self
    }
}

/// Task state for workflow snapshots.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskState {
    /// Task name
    pub name: String,

    /// Task input
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,

    /// Task output
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,

    /// Flow directive (continue, exit, end, or task name)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl TaskState {
    /// Create a new task state
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Set input
    pub fn with_input(mut self, input: serde_json::Value) -> Self {
        self.input = Some(input);
        self
    }

    /// Set output
    pub fn with_output(mut self, output: serde_json::Value) -> Self {
        self.output = Some(output);
        self
    }

    /// Set status/flow directive
    pub fn with_status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_status_serialization() {
        assert_eq!(
            serde_json::to_string(&WorkflowStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&WorkflowStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&WorkflowStatus::Faulted).unwrap(),
            "\"faulted\""
        );
    }

    #[test]
    fn test_workflow_state_builder() {
        let state = WorkflowState::new("my_workflow")
            .with_status(WorkflowStatus::Running)
            .with_input(serde_json::json!({ "key": "value" }))
            .with_current_task(TaskState::new("task1"));

        assert_eq!(state.workflow_name, "my_workflow");
        assert_eq!(state.status, WorkflowStatus::Running);
        assert!(state.input.is_some());
        assert!(state.current_task.is_some());
    }

    #[test]
    fn test_workflow_state_serialization() {
        let state = WorkflowState::new("test_workflow")
            .with_status(WorkflowStatus::Running)
            .with_current_task(TaskState::new("task1").with_input(serde_json::json!({"x": 1})));

        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["workflowName"], "test_workflow");
        assert_eq!(json["status"], "running");
        assert_eq!(json["currentTask"]["name"], "task1");
    }

    #[test]
    fn test_task_state_builder() {
        let task = TaskState::new("http_request")
            .with_input(serde_json::json!({ "url": "https://example.com" }))
            .with_output(serde_json::json!({ "status": 200 }))
            .with_status("continue");

        assert_eq!(task.name, "http_request");
        assert!(task.input.is_some());
        assert!(task.output.is_some());
        assert_eq!(task.status, Some("continue".to_string()));
    }

    #[test]
    fn test_completed_tasks() {
        let state = WorkflowState::new("workflow")
            .add_completed_task(TaskState::new("task1").with_output(serde_json::json!(1)))
            .add_completed_task(TaskState::new("task2").with_output(serde_json::json!(2)));

        assert_eq!(state.completed_tasks.len(), 2);
        assert_eq!(state.completed_tasks[0].name, "task1");
        assert_eq!(state.completed_tasks[1].name, "task2");
    }
}
