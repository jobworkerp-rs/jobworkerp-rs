//! ID newtypes for AG-UI protocol.
//!
//! These types correspond to AG-UI Rust SDK (ag-ui-core) ID definitions.

use app_wrapper::workflow::execute::task::ExecutionId;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Run ID - unique identifier for a workflow execution.
/// Corresponds to jobworkerp-rs ExecutionId.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(String);

impl RunId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Generate a random RunId for new workflow executions
    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Convert to jobworkerp-rs ExecutionId for checkpoint operations.
    /// Returns None only if the RunId is empty (which should not happen).
    pub fn to_execution_id(&self) -> Option<ExecutionId> {
        ExecutionId::new(self.0.clone())
    }

    /// Create RunId from ExecutionId
    pub fn from_execution_id(execution_id: &ExecutionId) -> Self {
        Self(execution_id.value.clone())
    }

    /// Convert to owned String
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RunId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RunId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Thread ID - identifies a conversation thread.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ThreadId(String);

impl ThreadId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ThreadId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ThreadId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Message ID - unique identifier for a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(String);

impl MessageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn random() -> Self {
        Self(format!("msg_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MessageId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<MessageId> for String {
    fn from(id: MessageId) -> Self {
        id.0
    }
}

impl From<&MessageId> for String {
    fn from(id: &MessageId) -> Self {
        id.0.clone()
    }
}

/// Step ID - unique identifier for a workflow step/task.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(String);

impl StepId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn random() -> Self {
        Self(format!("step_{}", uuid::Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for StepId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Tool Call ID - unique identifier for a tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolCallId(String);

impl ToolCallId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn random() -> Self {
        Self(format!("call_{}", uuid::Uuid::new_v4()))
    }

    /// Create ToolCallId from jobworkerp-rs JobId
    pub fn from_job_id(job_id: i64) -> Self {
        Self(format!("call_{:016x}", job_id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ToolCallId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ToolCallId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_id_serialization() {
        let run_id = RunId::new("test-run-123");
        let json = serde_json::to_string(&run_id).unwrap();
        assert_eq!(json, "\"test-run-123\"");

        let deserialized: RunId = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, run_id);
    }

    #[test]
    fn test_run_id_random() {
        let run_id1 = RunId::random();
        let run_id2 = RunId::random();
        assert_ne!(run_id1, run_id2);
    }

    #[test]
    fn test_thread_id_display() {
        let thread_id = ThreadId::new("thread-456");
        assert_eq!(format!("{}", thread_id), "thread-456");
    }

    #[test]
    fn test_message_id_random() {
        let msg_id = MessageId::random();
        assert!(msg_id.as_str().starts_with("msg_"));
    }

    #[test]
    fn test_tool_call_id_from_job_id() {
        let tool_call_id = ToolCallId::from_job_id(12345);
        assert!(tool_call_id.as_str().starts_with("call_"));
        assert!(tool_call_id.as_str().contains("3039")); // hex for 12345
    }

    #[test]
    fn test_step_id_serialization() {
        let step_id = StepId::new("task_init");
        let json = serde_json::to_string(&step_id).unwrap();
        assert_eq!(json, "\"task_init\"");
    }
}
