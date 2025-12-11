//! Type definitions for AG-UI protocol.
//!
//! These types correspond to AG-UI Rust SDK (ag-ui-core) type definitions.

pub mod context;
pub mod ids;
pub mod input;
pub mod message;
pub mod state;
pub mod tool;

// Re-export commonly used types
pub use context::Context;
pub use ids::{MessageId, RunId, StepId, ThreadId, ToolCallId};
pub use input::{JobworkerpFwdProps, RunAgentInput};
pub use message::{ContentPart, Message, MessageContent, Role};
pub use state::{TaskState, WorkflowState, WorkflowStatus};
pub use tool::{Tool, ToolCall, ToolCallResult};
