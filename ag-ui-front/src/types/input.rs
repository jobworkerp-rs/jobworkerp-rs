//! Input types for AG-UI protocol.
//!
//! References AG-UI Rust SDK (ag-ui-core) run_agent_input.rs
//!
//! AG-UI Interrupts (Draft) support:
//! - ResumeInfo for resuming interrupted runs

use crate::types::{context::Context, message::Message, tool::Tool};
use serde::{Deserialize, Serialize};

// ============================================================================
// AG-UI Interrupts (Draft) Resume Types
// ============================================================================

/// Resume information for continuing an interrupted run (AG-UI Interrupts Draft)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeInfo {
    /// ID of the interrupt to resume from
    pub interrupt_id: String,
    /// Payload containing the resume action
    pub payload: ResumePayload,
}

/// Resume payload for interrupted runs (AG-UI Interrupts Draft)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResumePayload {
    /// Approve the pending tool calls and execute them
    #[serde(rename_all = "camelCase")]
    Approve {
        /// Optional tool results (for client-side executed tools)
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_results: Option<Vec<ToolCallResult>>,
    },
    /// Reject the pending tool calls
    #[serde(rename_all = "camelCase")]
    Reject {
        /// Optional reason for rejection
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// Result of a tool call (for client-side executed tools)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// Tool call ID
    pub call_id: String,
    /// Result value
    pub result: serde_json::Value,
}

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

    /// Resume information for continuing an interrupted run (AG-UI Interrupts Draft)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeInfo>,
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
            resume: None,
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

    /// Check if this is a resume request
    pub fn is_resume(&self) -> bool {
        self.resume.is_some()
    }

    /// Get the resume info if present
    pub fn get_resume(&self) -> Option<&ResumeInfo> {
        self.resume.as_ref()
    }

    /// Set resume info for continuing an interrupted run
    pub fn resume(mut self, resume: ResumeInfo) -> Self {
        self.resume = Some(resume);
        self
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

    #[test]
    fn test_resume_info_approve() {
        let resume = ResumeInfo {
            interrupt_id: "int_123".to_string(),
            payload: ResumePayload::Approve { tool_results: None },
        };

        let input: RunAgentInput = RunAgentInput::with_workflow("my_workflow").resume(resume);

        assert!(input.is_resume());
        let resume = input.get_resume().unwrap();
        assert_eq!(resume.interrupt_id, "int_123");
        assert!(matches!(resume.payload, ResumePayload::Approve { .. }));

        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["resume"]["interruptId"], "int_123");
        assert_eq!(json["resume"]["payload"]["type"], "approve");
    }

    #[test]
    fn test_resume_info_reject() {
        let resume = ResumeInfo {
            interrupt_id: "int_456".to_string(),
            payload: ResumePayload::Reject {
                reason: Some("User cancelled".to_string()),
            },
        };

        let json = serde_json::to_value(&resume).unwrap();
        assert_eq!(json["interruptId"], "int_456");
        assert_eq!(json["payload"]["type"], "reject");
        assert_eq!(json["payload"]["reason"], "User cancelled");
    }

    #[test]
    fn test_resume_info_deserialization() {
        let json = r#"{
            "interruptId": "int_789",
            "payload": {
                "type": "approve",
                "toolResults": [
                    { "callId": "call_1", "result": {"data": "test"} }
                ]
            }
        }"#;

        let resume: ResumeInfo = serde_json::from_str(json).unwrap();
        assert_eq!(resume.interrupt_id, "int_789");
        match resume.payload {
            ResumePayload::Approve { tool_results } => {
                let results = tool_results.unwrap();
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].call_id, "call_1");
            }
            _ => panic!("Expected Approve payload"),
        }
    }
}
