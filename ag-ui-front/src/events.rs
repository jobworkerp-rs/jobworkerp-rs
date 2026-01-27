//! Event types and utilities for AG-UI protocol.

pub mod adapter;
pub mod encoder;
pub mod llm;
pub mod state_diff;
pub mod types;

// Re-export main types
pub use adapter::{SharedWorkflowEventAdapter, WorkflowEventAdapter, shared_adapter};
pub use encoder::{EventEncoder, encode_comment, encode_retry};
pub use llm::{
    ExtractedToolCall, ExtractedToolCalls, LlmStreamingResult, extract_text_from_llm_chat_result,
    extract_tool_calls_from_llm_result, result_output_stream_to_ag_ui_events,
    result_output_stream_to_ag_ui_events_with_end_guarantee, tool_calls_to_ag_ui_events,
};
pub use state_diff::{StateTracker, calculate_state_diff, create_state_delta_event};
pub use types::{AgUiEvent, InterruptInfo, InterruptPayload, PendingToolCall, RunOutcome};
