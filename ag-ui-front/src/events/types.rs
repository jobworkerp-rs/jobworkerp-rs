//! AG-UI event type definitions.
//!
//! References AG-UI Rust SDK (ag-ui-core) event.rs
//! Phase 1: Basic enum definition only. Builder methods added in Phase 2.
//!
//! AG-UI Interrupts (Draft) support:
//! - RUN_FINISHED with outcome and interrupt fields for HITL workflows

use crate::types::{message::Role, state::WorkflowState};
use serde::{Deserialize, Serialize};

// ============================================================================
// AG-UI Interrupts (Draft) Types
// ============================================================================

/// Run outcome for RUN_FINISHED event (AG-UI Interrupts Draft)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// Run completed successfully
    Success,
    /// Run paused waiting for user input (HITL)
    Interrupt,
}

/// Interrupt information for HITL workflows (AG-UI Interrupts Draft)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptInfo {
    /// Unique identifier for this interrupt
    pub id: String,
    /// Reason for the interrupt (e.g., "tool_approval_required")
    pub reason: String,
    /// Payload containing interrupt-specific data
    pub payload: InterruptPayload,
}

/// Interrupt payload containing pending tool calls and context
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterruptPayload {
    /// Pending tool calls awaiting approval
    pub pending_tool_calls: Vec<PendingToolCall>,
    /// Checkpoint position for resuming the workflow
    pub checkpoint_position: String,
    /// Name of the workflow that was interrupted
    pub workflow_name: String,
}

/// Pending tool call awaiting user approval
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolCall {
    /// Tool call ID
    pub call_id: String,
    /// Function name to be called
    pub fn_name: String,
    /// Function arguments as JSON string
    pub fn_arguments: String,
}

/// AG-UI event types.
/// Based on AG-UI protocol specification.
///
/// Note: Large variant size difference is expected - StateSnapshot contains full workflow state
/// while other events are lightweight. This is intentional for the AG-UI protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum AgUiEvent {
    // === Lifecycle Events ===
    /// Run started event
    #[serde(rename = "RUN_STARTED")]
    RunStarted {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },

    /// Run finished (success or interrupt)
    /// Extended with AG-UI Interrupts (Draft) fields
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
        /// Run outcome: "success" or "interrupt" (AG-UI Interrupts Draft)
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<RunOutcome>,
        /// Interrupt information when outcome is "interrupt" (AG-UI Interrupts Draft)
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<InterruptInfo>,
    },

    /// Run error
    #[serde(rename = "RUN_ERROR")]
    RunError {
        #[serde(rename = "runId")]
        run_id: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },

    /// Step (task) started
    #[serde(rename = "STEP_STARTED")]
    StepStarted {
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "stepName", skip_serializing_if = "Option::is_none")]
        step_name: Option<String>,
        #[serde(rename = "parentStepId", skip_serializing_if = "Option::is_none")]
        parent_step_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        metadata: Option<serde_json::Value>,
    },

    /// Step (task) finished
    #[serde(rename = "STEP_FINISHED")]
    StepFinished {
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
    },

    // === Message Events ===
    /// Text message start
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(rename = "messageId")]
        message_id: String,
        role: Role,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Text message content chunk
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Text message end
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    // === Tool Call Events ===
    /// Tool call start
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
        #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Tool call arguments chunk
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Tool call end
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Tool call result
    #[serde(rename = "TOOL_CALL_RESULT")]
    ToolCallResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        result: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    // === State Events ===
    /// State snapshot
    #[serde(rename = "STATE_SNAPSHOT")]
    StateSnapshot {
        snapshot: WorkflowState,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// State delta (JSON Patch)
    #[serde(rename = "STATE_DELTA")]
    StateDelta {
        delta: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    // === Special Events ===
    /// Raw event for external system integration
    #[serde(rename = "RAW")]
    Raw {
        event: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },

    /// Custom event for extensions
    #[serde(rename = "CUSTOM")]
    Custom {
        name: String,
        data: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        timestamp: Option<f64>,
    },
}

impl AgUiEvent {
    /// Get the event type as a string
    pub fn event_type(&self) -> &'static str {
        match self {
            AgUiEvent::RunStarted { .. } => "RUN_STARTED",
            AgUiEvent::RunFinished { .. } => "RUN_FINISHED",
            AgUiEvent::RunError { .. } => "RUN_ERROR",
            AgUiEvent::StepStarted { .. } => "STEP_STARTED",
            AgUiEvent::StepFinished { .. } => "STEP_FINISHED",
            AgUiEvent::TextMessageStart { .. } => "TEXT_MESSAGE_START",
            AgUiEvent::TextMessageContent { .. } => "TEXT_MESSAGE_CONTENT",
            AgUiEvent::TextMessageEnd { .. } => "TEXT_MESSAGE_END",
            AgUiEvent::ToolCallStart { .. } => "TOOL_CALL_START",
            AgUiEvent::ToolCallArgs { .. } => "TOOL_CALL_ARGS",
            AgUiEvent::ToolCallEnd { .. } => "TOOL_CALL_END",
            AgUiEvent::ToolCallResult { .. } => "TOOL_CALL_RESULT",
            AgUiEvent::StateSnapshot { .. } => "STATE_SNAPSHOT",
            AgUiEvent::StateDelta { .. } => "STATE_DELTA",
            AgUiEvent::Raw { .. } => "RAW",
            AgUiEvent::Custom { .. } => "CUSTOM",
        }
    }

    /// Get current timestamp in milliseconds as f64 (AG-UI SDK compliant)
    pub fn now_timestamp() -> f64 {
        chrono::Utc::now().timestamp_millis() as f64
    }

    // === Builder Methods ===

    /// Create a RUN_STARTED event
    pub fn run_started(run_id: impl Into<String>, thread_id: impl Into<String>) -> Self {
        AgUiEvent::RunStarted {
            run_id: run_id.into(),
            thread_id: thread_id.into(),
            timestamp: Some(Self::now_timestamp()),
            metadata: None,
        }
    }

    /// Create a RUN_FINISHED event with success outcome
    pub fn run_finished(run_id: impl Into<String>, result: Option<serde_json::Value>) -> Self {
        AgUiEvent::RunFinished {
            run_id: run_id.into(),
            timestamp: Some(Self::now_timestamp()),
            result,
            outcome: Some(RunOutcome::Success),
            interrupt: None,
        }
    }

    /// Create a RUN_FINISHED event with success outcome (explicit)
    pub fn run_finished_success(
        run_id: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> Self {
        Self::run_finished(run_id, result)
    }

    /// Create a RUN_FINISHED event with interrupt outcome (HITL)
    pub fn run_finished_with_interrupt(
        run_id: impl Into<String>,
        interrupt: InterruptInfo,
    ) -> Self {
        AgUiEvent::RunFinished {
            run_id: run_id.into(),
            timestamp: Some(Self::now_timestamp()),
            result: None,
            outcome: Some(RunOutcome::Interrupt),
            interrupt: Some(interrupt),
        }
    }

    /// Create a RUN_ERROR event
    pub fn run_error(
        run_id: impl Into<String>,
        message: impl Into<String>,
        code: Option<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        AgUiEvent::RunError {
            run_id: run_id.into(),
            message: message.into(),
            code,
            timestamp: Some(Self::now_timestamp()),
            details,
        }
    }

    /// Create a STEP_STARTED event
    pub fn step_started(step_id: impl Into<String>, step_name: impl Into<String>) -> Self {
        AgUiEvent::StepStarted {
            step_id: step_id.into(),
            step_name: Some(step_name.into()),
            parent_step_id: None,
            timestamp: Some(Self::now_timestamp()),
            metadata: None,
        }
    }

    /// Create a STEP_FINISHED event
    pub fn step_finished(step_id: impl Into<String>, result: Option<serde_json::Value>) -> Self {
        AgUiEvent::StepFinished {
            step_id: step_id.into(),
            timestamp: Some(Self::now_timestamp()),
            result,
        }
    }

    /// Create a TEXT_MESSAGE_START event
    pub fn text_message_start(message_id: impl Into<String>, role: Role) -> Self {
        AgUiEvent::TextMessageStart {
            message_id: message_id.into(),
            role,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TEXT_MESSAGE_CONTENT event
    pub fn text_message_content(message_id: impl Into<String>, delta: impl Into<String>) -> Self {
        AgUiEvent::TextMessageContent {
            message_id: message_id.into(),
            delta: delta.into(),
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TEXT_MESSAGE_END event
    pub fn text_message_end(message_id: impl Into<String>) -> Self {
        AgUiEvent::TextMessageEnd {
            message_id: message_id.into(),
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TOOL_CALL_START event
    pub fn tool_call_start(
        tool_call_id: impl Into<String>,
        tool_call_name: impl Into<String>,
        parent_message_id: Option<String>,
    ) -> Self {
        AgUiEvent::ToolCallStart {
            tool_call_id: tool_call_id.into(),
            tool_call_name: tool_call_name.into(),
            parent_message_id,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TOOL_CALL_ARGS event
    pub fn tool_call_args(tool_call_id: impl Into<String>, delta: impl Into<String>) -> Self {
        AgUiEvent::ToolCallArgs {
            tool_call_id: tool_call_id.into(),
            delta: delta.into(),
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TOOL_CALL_END event
    pub fn tool_call_end(tool_call_id: impl Into<String>) -> Self {
        AgUiEvent::ToolCallEnd {
            tool_call_id: tool_call_id.into(),
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a TOOL_CALL_RESULT event
    pub fn tool_call_result(tool_call_id: impl Into<String>, result: serde_json::Value) -> Self {
        AgUiEvent::ToolCallResult {
            tool_call_id: tool_call_id.into(),
            result,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a STATE_SNAPSHOT event
    pub fn state_snapshot(snapshot: WorkflowState) -> Self {
        AgUiEvent::StateSnapshot {
            snapshot,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a STATE_DELTA event
    pub fn state_delta(delta: Vec<serde_json::Value>) -> Self {
        AgUiEvent::StateDelta {
            delta,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a RAW event
    pub fn raw(event: serde_json::Value) -> Self {
        AgUiEvent::Raw {
            event,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a CUSTOM event
    pub fn custom(name: impl Into<String>, data: serde_json::Value) -> Self {
        AgUiEvent::Custom {
            name: name.into(),
            data,
            timestamp: Some(Self::now_timestamp()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_started_serialization() {
        let event = AgUiEvent::RunStarted {
            run_id: "run_123".to_string(),
            thread_id: "thread_456".to_string(),
            timestamp: Some(1234567890123.0),
            metadata: None,
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "RUN_STARTED");
        assert_eq!(json["runId"], "run_123");
        assert_eq!(json["threadId"], "thread_456");
        assert_eq!(json["timestamp"], 1234567890123.0);
    }

    #[test]
    fn test_run_error_serialization() {
        let event = AgUiEvent::RunError {
            run_id: "run_123".to_string(),
            message: "Task failed".to_string(),
            code: Some("TASK_FAILED".to_string()),
            timestamp: Some(1234567890123.0),
            details: Some(serde_json::json!({ "taskName": "http_request" })),
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "RUN_ERROR");
        assert_eq!(json["message"], "Task failed");
        assert_eq!(json["code"], "TASK_FAILED");
    }

    #[test]
    fn test_text_message_events() {
        let start = AgUiEvent::TextMessageStart {
            message_id: "msg_1".to_string(),
            role: Role::Assistant,
            timestamp: None,
        };

        let content = AgUiEvent::TextMessageContent {
            message_id: "msg_1".to_string(),
            delta: "Hello".to_string(),
            timestamp: None,
        };

        let end = AgUiEvent::TextMessageEnd {
            message_id: "msg_1".to_string(),
            timestamp: None,
        };

        assert_eq!(start.event_type(), "TEXT_MESSAGE_START");
        assert_eq!(content.event_type(), "TEXT_MESSAGE_CONTENT");
        assert_eq!(end.event_type(), "TEXT_MESSAGE_END");

        let start_json = serde_json::to_value(&start).unwrap();
        assert_eq!(start_json["role"], "assistant");
    }

    #[test]
    fn test_tool_call_events() {
        let start = AgUiEvent::ToolCallStart {
            tool_call_id: "call_1".to_string(),
            tool_call_name: "http_request".to_string(),
            parent_message_id: Some("msg_1".to_string()),
            timestamp: None,
        };

        let json = serde_json::to_value(&start).unwrap();
        assert_eq!(json["type"], "TOOL_CALL_START");
        assert_eq!(json["toolCallId"], "call_1");
        assert_eq!(json["toolCallName"], "http_request");
    }

    #[test]
    fn test_state_snapshot() {
        let state = WorkflowState::new("my_workflow");
        let event = AgUiEvent::StateSnapshot {
            snapshot: state,
            timestamp: Some(1234567890123.0),
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "STATE_SNAPSHOT");
        assert_eq!(json["snapshot"]["workflowName"], "my_workflow");
    }

    #[test]
    fn test_state_delta() {
        let delta =
            vec![serde_json::json!({ "op": "replace", "path": "/status", "value": "completed" })];
        let event = AgUiEvent::StateDelta {
            delta,
            timestamp: None,
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "STATE_DELTA");
        assert!(json["delta"].is_array());
    }

    #[test]
    fn test_event_deserialization() {
        let json = r#"{
            "type": "RUN_STARTED",
            "runId": "run_xyz",
            "threadId": "thread_abc"
        }"#;

        let event: AgUiEvent = serde_json::from_str(json).unwrap();
        match event {
            AgUiEvent::RunStarted {
                run_id, thread_id, ..
            } => {
                assert_eq!(run_id, "run_xyz");
                assert_eq!(thread_id, "thread_abc");
            }
            _ => panic!("Expected RunStarted"),
        }
    }

    #[test]
    fn test_custom_event() {
        let event = AgUiEvent::Custom {
            name: "my_custom_event".to_string(),
            data: serde_json::json!({ "key": "value" }),
            timestamp: None,
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "CUSTOM");
        assert_eq!(json["name"], "my_custom_event");
    }

    // === Builder Method Tests ===

    #[test]
    fn test_builder_run_started() {
        let event = AgUiEvent::run_started("run_1", "thread_1");
        match event {
            AgUiEvent::RunStarted {
                run_id,
                thread_id,
                timestamp,
                ..
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(thread_id, "thread_1");
                assert!(timestamp.is_some());
            }
            _ => panic!("Expected RunStarted"),
        }
    }

    #[test]
    fn test_builder_run_finished() {
        let event = AgUiEvent::run_finished("run_1", Some(serde_json::json!({"result": "ok"})));
        match event {
            AgUiEvent::RunFinished {
                run_id,
                result,
                timestamp,
                outcome,
                interrupt,
            } => {
                assert_eq!(run_id, "run_1");
                assert!(result.is_some());
                assert!(timestamp.is_some());
                assert_eq!(outcome, Some(RunOutcome::Success));
                assert!(interrupt.is_none());
            }
            _ => panic!("Expected RunFinished"),
        }
    }

    #[test]
    fn test_builder_run_finished_with_interrupt() {
        let interrupt_info = InterruptInfo {
            id: "int_123".to_string(),
            reason: "tool_approval_required".to_string(),
            payload: InterruptPayload {
                pending_tool_calls: vec![PendingToolCall {
                    call_id: "call_1".to_string(),
                    fn_name: "COMMAND___run".to_string(),
                    fn_arguments: r#"{"command":"date"}"#.to_string(),
                }],
                checkpoint_position: "llm_chat_task".to_string(),
                workflow_name: "test_workflow".to_string(),
            },
        };

        let event = AgUiEvent::run_finished_with_interrupt("run_1", interrupt_info.clone());
        match &event {
            AgUiEvent::RunFinished {
                run_id,
                result,
                timestamp,
                outcome,
                interrupt,
            } => {
                assert_eq!(run_id, "run_1");
                assert!(result.is_none());
                assert!(timestamp.is_some());
                assert_eq!(outcome, &Some(RunOutcome::Interrupt));
                assert!(interrupt.is_some());
                let int = interrupt.as_ref().unwrap();
                assert_eq!(int.id, "int_123");
                assert_eq!(int.reason, "tool_approval_required");
                assert_eq!(int.payload.pending_tool_calls.len(), 1);
            }
            _ => panic!("Expected RunFinished"),
        }

        // Test JSON serialization
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "RUN_FINISHED");
        assert_eq!(json["outcome"], "interrupt");
        assert_eq!(json["interrupt"]["id"], "int_123");
        assert_eq!(json["interrupt"]["reason"], "tool_approval_required");
        assert_eq!(
            json["interrupt"]["payload"]["pendingToolCalls"][0]["fnName"],
            "COMMAND___run"
        );
    }

    #[test]
    fn test_builder_run_error() {
        let event = AgUiEvent::run_error(
            "run_1",
            "Something went wrong",
            Some("INTERNAL_ERROR".to_string()),
            None,
        );
        match event {
            AgUiEvent::RunError {
                run_id,
                message,
                code,
                ..
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(message, "Something went wrong");
                assert_eq!(code, Some("INTERNAL_ERROR".to_string()));
            }
            _ => panic!("Expected RunError"),
        }
    }

    #[test]
    fn test_builder_step_events() {
        let started = AgUiEvent::step_started("step_1", "initialize");
        assert_eq!(started.event_type(), "STEP_STARTED");

        let finished = AgUiEvent::step_finished("step_1", Some(serde_json::json!({"done": true})));
        assert_eq!(finished.event_type(), "STEP_FINISHED");
    }

    #[test]
    fn test_builder_text_message_lifecycle() {
        let start = AgUiEvent::text_message_start("msg_1", Role::Assistant);
        let content = AgUiEvent::text_message_content("msg_1", "Hello");
        let end = AgUiEvent::text_message_end("msg_1");

        assert_eq!(start.event_type(), "TEXT_MESSAGE_START");
        assert_eq!(content.event_type(), "TEXT_MESSAGE_CONTENT");
        assert_eq!(end.event_type(), "TEXT_MESSAGE_END");
    }

    #[test]
    fn test_builder_tool_call_lifecycle() {
        let start = AgUiEvent::tool_call_start("call_1", "http_request", None);
        let args = AgUiEvent::tool_call_args("call_1", r#"{"url":"https://example.com"}"#);
        let end = AgUiEvent::tool_call_end("call_1");
        let result = AgUiEvent::tool_call_result("call_1", serde_json::json!({"status": 200}));

        assert_eq!(start.event_type(), "TOOL_CALL_START");
        assert_eq!(args.event_type(), "TOOL_CALL_ARGS");
        assert_eq!(end.event_type(), "TOOL_CALL_END");
        assert_eq!(result.event_type(), "TOOL_CALL_RESULT");

        // Test with parent_message_id
        let start_with_parent =
            AgUiEvent::tool_call_start("call_2", "command", Some("msg_123".to_string()));
        match start_with_parent {
            AgUiEvent::ToolCallStart {
                parent_message_id, ..
            } => {
                assert_eq!(parent_message_id, Some("msg_123".to_string()));
            }
            _ => panic!("Expected ToolCallStart"),
        }
    }

    #[test]
    fn test_builder_state_snapshot() {
        let state = WorkflowState::new("my_workflow");
        let event = AgUiEvent::state_snapshot(state);
        assert_eq!(event.event_type(), "STATE_SNAPSHOT");
    }

    #[test]
    fn test_builder_state_delta() {
        let delta =
            vec![serde_json::json!({"op": "replace", "path": "/status", "value": "completed"})];
        let event = AgUiEvent::state_delta(delta);
        assert_eq!(event.event_type(), "STATE_DELTA");
    }

    #[test]
    fn test_builder_raw_and_custom() {
        let raw = AgUiEvent::raw(serde_json::json!({"external": "data"}));
        assert_eq!(raw.event_type(), "RAW");

        let custom = AgUiEvent::custom("my_event", serde_json::json!({"key": "value"}));
        assert_eq!(custom.event_type(), "CUSTOM");
    }
}
