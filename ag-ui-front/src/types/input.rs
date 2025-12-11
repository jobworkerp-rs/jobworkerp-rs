//! Input types for AG-UI protocol.
//!
//! References AG-UI Rust SDK (ag-ui-core) run_agent_input.rs

use crate::types::{context::Context, message::Message, tool::Tool};
use serde::{Deserialize, Serialize};

/// Input for running an agent/workflow.
/// Based on AG-UI RunAgentInput with jobworkerp-rs extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAgentInput<F = JobworkerpFwdProps>
where
    F: Default + Clone + Serialize,
{
    /// Thread ID for the conversation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

    /// Run ID (optional, auto-generated if not provided)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,

    /// Input messages for the workflow
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Frontend-defined tools (for Human-in-the-Loop)
    #[serde(default)]
    pub tools: Vec<Tool>,

    /// Context information
    #[serde(default)]
    pub context: Vec<Context>,

    /// Forwarded properties (jobworkerp-rs specific)
    #[serde(default)]
    pub forwarded_props: F,
}

impl<F> Default for RunAgentInput<F>
where
    F: Default + Clone + Serialize,
{
    fn default() -> Self {
        Self {
            thread_id: None,
            run_id: None,
            messages: Vec::new(),
            tools: Vec::new(),
            context: Vec::new(),
            forwarded_props: F::default(),
        }
    }
}

impl<F> RunAgentInput<F>
where
    F: Default + Clone + Serialize,
{
    /// Create a new RunAgentInput with a workflow name
    pub fn with_workflow(workflow_name: impl Into<String>) -> Self {
        Self {
            context: vec![Context::workflow(workflow_name)],
            ..Default::default()
        }
    }

    /// Set the thread ID
    pub fn thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    /// Set the run ID
    pub fn run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = Some(run_id.into());
        self
    }

    /// Add a message
    pub fn message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    /// Add a tool
    pub fn tool(mut self, tool: Tool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Add a context
    pub fn context(mut self, context: Context) -> Self {
        self.context.push(context);
        self
    }

    /// Extract workflow name from context
    pub fn get_workflow_name(&self) -> Option<&str> {
        self.context.iter().find_map(|ctx| match ctx {
            Context::WorkflowDefinition { workflow_name, .. } => Some(workflow_name.as_str()),
            _ => None,
        })
    }

    /// Extract checkpoint resume info from context
    pub fn get_checkpoint_resume(&self) -> Option<(&str, &str)> {
        self.context.iter().find_map(|ctx| match ctx {
            Context::CheckpointResume {
                execution_id,
                position,
                ..
            } => Some((execution_id.as_str(), position.as_str())),
            _ => None,
        })
    }
}

/// Jobworkerp-rs specific forwarded properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobworkerpFwdProps {
    /// Worker ID to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<i64>,

    /// Worker name to use
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_name: Option<String>,

    /// Job priority (higher = more urgent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,

    /// Job timeout in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// Unique key for deduplication
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uniq_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_agent_input_default() {
        let input: RunAgentInput = RunAgentInput::default();
        assert!(input.thread_id.is_none());
        assert!(input.run_id.is_none());
        assert!(input.messages.is_empty());
    }

    #[test]
    fn test_run_agent_input_with_workflow() {
        let input: RunAgentInput = RunAgentInput::with_workflow("my_workflow")
            .thread_id("thread-123")
            .run_id("run-456");

        assert_eq!(input.thread_id, Some("thread-123".to_string()));
        assert_eq!(input.run_id, Some("run-456".to_string()));
        assert_eq!(input.get_workflow_name(), Some("my_workflow"));
    }

    #[test]
    fn test_run_agent_input_serialization() {
        let input: RunAgentInput =
            RunAgentInput::with_workflow("test_workflow").message(Message::user("Hello"));

        let json = serde_json::to_value(&input).unwrap();
        assert!(json["context"].is_array());
        assert_eq!(json["messages"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn test_jobworkerp_fwd_props() {
        let props = JobworkerpFwdProps {
            worker_id: Some(12345),
            priority: Some(10),
            ..Default::default()
        };

        let json = serde_json::to_value(&props).unwrap();
        assert_eq!(json["workerId"], 12345);
        assert_eq!(json["priority"], 10);
        assert!(json.get("workerName").is_none());
    }

    #[test]
    fn test_checkpoint_resume_extraction() {
        let input: RunAgentInput = RunAgentInput::default()
            .context(Context::checkpoint_resume("exec-123", "/tasks/task1"));

        let (exec_id, position) = input.get_checkpoint_resume().unwrap();
        assert_eq!(exec_id, "exec-123");
        assert_eq!(position, "/tasks/task1");
    }
}
