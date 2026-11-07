//! Type definitions for AG-UI protocol.
//!
//! These types correspond to AG-UI Rust SDK (ag-ui-core) type definitions.

pub(crate) fn deserialize_non_null_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    if value.is_null() {
        return Err(serde::de::Error::custom(
            "null is not valid for this AG-UI field",
        ));
    }
    <T as serde::Deserialize>::deserialize(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

pub mod context;
pub mod ids;
pub mod input;
pub mod message;
pub mod state;
pub mod tool;

// Re-export commonly used types
pub use context::Context;
pub use ids::{MessageId, RunId, StepId, ThreadId, ToolCallId};
pub use input::{ResumeEntry, ResumeStatus, RunAgentInput};
pub use message::{ContentPart, Message, MessageContent, Role};
pub use state::{TaskState, WorkflowState, WorkflowStatus};
pub use tool::{Tool, ToolCall, ToolCallResult};
