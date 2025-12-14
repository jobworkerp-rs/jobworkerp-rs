//! Event types and utilities for AG-UI protocol.

pub mod adapter;
pub mod encoder;
pub mod llm;
pub mod state_diff;
pub mod types;

// Re-export main types
pub use adapter::{shared_adapter, SharedWorkflowEventAdapter, WorkflowEventAdapter};
pub use encoder::{encode_comment, encode_retry, EventEncoder};
pub use llm::{
    extract_text_from_llm_chat_result, result_output_stream_to_ag_ui_events,
    result_output_stream_to_ag_ui_events_with_end_guarantee, LlmStreamingResult,
};
pub use state_diff::{calculate_state_diff, create_state_delta_event, StateTracker};
pub use types::AgUiEvent;
