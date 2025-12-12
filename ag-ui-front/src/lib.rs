//! AG-UI Front - AG-UI protocol implementation for jobworkerp-rs.
//!
//! This crate provides AG-UI (Agent User Interaction Protocol) server functionality,
//! enabling workflow execution to be streamed as Server-Sent Events (SSE) to UI clients.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                        UI Client                            │
//! │              (Web Browser / Mobile App / CLI)               │
//! └─────────────────────────┬───────────────────────────────────┘
//!                           │ HTTP SSE / HTTP POST
//!                           ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     ag-ui-front                             │
//! │  ┌────────────────┐  ┌────────────────┐  ┌───────────────┐ │
//! │  │ HTTP Endpoint  │  │ Session Manager│  │ Event Encoder │ │
//! │  │ (axum 0.8)     │  │                │  │               │ │
//! │  └───────┬────────┘  └───────┬────────┘  └───────┬───────┘ │
//! │          │                   │                   │         │
//! │  ┌───────┴───────────────────┴───────────────────┴───────┐ │
//! │  │                    AgUiHandler                        │ │
//! │  │  - jobworkerp events → AG-UI events conversion        │ │
//! │  └───────────────────────────┬───────────────────────────┘ │
//! └──────────────────────────────┼──────────────────────────────┘
//!                                │
//!         ┌──────────────────────┼──────────────────────┐
//!         ▼                      ▼                      ▼
//! ┌───────────────┐  ┌───────────────────┐  ┌──────────────────┐
//! │   app-wrapper │  │       app         │  │      infra       │
//! │  (Workflow)   │  │   (Job Execute)   │  │  (Pub/Sub)       │
//! └───────────────┘  └───────────────────┘  └──────────────────┘
//! ```
//!
//! # Modules
//!
//! - `config`: Server configuration
//! - `error`: Error types
//! - `types`: AG-UI type definitions (IDs, messages, tools, etc.)
//! - `events`: AG-UI event types and utilities
//!
//! # Environment Variables
//!
//! - `AG_UI_ADDR`: Server bind address (default: 127.0.0.1:8001)
//! - `AG_UI_AUTH_ENABLED`: Enable Bearer authentication (default: false)
//! - `AG_UI_AUTH_TOKENS`: Valid tokens, comma-separated (default: demo-token)
//! - `AG_UI_REQUEST_TIMEOUT_SEC`: Request timeout (default: 60)
//! - `AG_UI_MAX_EVENTS_PER_RUN`: Max events to store per run (default: 1000)
//! - `AG_UI_SESSION_TTL_SEC`: Session TTL in seconds (default: 3600)

pub mod config;
pub mod error;
pub mod events;
pub mod handler;
pub mod handler_async;
pub mod pubsub;
pub mod server;
pub mod session;
pub mod types;

// Re-export main types
pub use config::{AgUiServerConfig, AuthConfig, ServerConfig};
pub use error::{AgUiError, Result};
pub use events::{
    encode_comment, encode_retry, result_output_stream_to_ag_ui_events,
    result_output_stream_to_ag_ui_events_with_end_guarantee, shared_adapter, AgUiEvent,
    EventEncoder, LlmStreamingResult, SharedWorkflowEventAdapter, WorkflowEventAdapter,
};
pub use pubsub::{
    convert_result_stream_to_events, merge_workflow_and_llm_streams,
    subscribe_job_result_as_tool_call, subscribe_llm_stream,
};
pub use server::boot_embedded_server;
pub use session::{
    EventStore, InMemoryEventStore, InMemorySessionManager, RedisEventStore, RedisSessionManager,
    Session, SessionManager, SessionState,
};
pub use types::{
    Context, JobworkerpFwdProps, Message, MessageId, Role, RunAgentInput, RunId, StepId, TaskState,
    ThreadId, Tool, ToolCall, ToolCallId, ToolCallResult, WorkflowState, WorkflowStatus,
};
