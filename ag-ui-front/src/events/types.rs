//! AG-UI event type definitions.
//!
//! References AG-UI Rust SDK (ag-ui-core) event.rs
//! Phase 1: Basic enum definition only. Builder methods added in Phase 2.
//!
//! AG-UI Interrupts (Draft) support:
//! - RUN_FINISHED with outcome and interrupt fields for HITL workflows

use crate::types::message::Role;
use serde::{Deserialize, Serialize};

// ============================================================================
// AG-UI Interrupts (Draft) Types
// ============================================================================

/// Run outcome for RUN_FINISHED event (AG-UI Interrupts Draft)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunOutcome {
    /// Run completed successfully
    Success,
    /// Run paused waiting for user input (HITL)
    Interrupt {
        #[serde(
            serialize_with = "serialize_non_empty_interrupts",
            deserialize_with = "deserialize_non_empty_interrupts"
        )]
        interrupts: Vec<AgUiInterrupt>,
    },
    #[serde(rename = "cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgUiInterrupt {
    pub id: String,
    pub reason: String,
}

fn serialize_non_empty_interrupts<S>(
    interrupts: &[AgUiInterrupt],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if interrupts.is_empty() {
        return Err(serde::ser::Error::custom(
            "interrupt outcome requires at least one interrupt",
        ));
    }
    interrupts.serialize(serializer)
}

fn deserialize_non_empty_interrupts<'de, D>(deserializer: D) -> Result<Vec<AgUiInterrupt>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let interrupts = Vec::<AgUiInterrupt>::deserialize(deserializer)?;
    if interrupts.is_empty() {
        return Err(serde::de::Error::custom(
            "interrupt outcome requires at least one interrupt",
        ));
    }
    Ok(interrupts)
}

fn serialize_non_null_value_option<S>(
    value: &Option<serde_json::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    if matches!(value, Some(serde_json::Value::Null)) {
        return Err(serde::ser::Error::custom(
            "null is not valid for this AG-UI field",
        ));
    }
    value.serialize(serializer)
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
/// State snapshots contain only the explicitly projected public JSON state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
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
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    },

    /// Run finished (success or interrupt)
    /// Extended with AG-UI Interrupts (Draft) fields
    #[serde(rename = "RUN_FINISHED")]
    RunFinished {
        #[serde(rename = "runId")]
        run_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            serialize_with = "serialize_non_null_value_option",
            skip_serializing_if = "Option::is_none"
        )]
        result: Option<serde_json::Value>,
        /// Run outcome: "success" or "interrupt" (AG-UI Interrupts Draft)
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        outcome: Option<RunOutcome>,
        /// Interrupt information when outcome is "interrupt" (AG-UI Interrupts Draft)
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(skip)]
        interrupt: Option<InterruptInfo>,
    },

    /// Run error
    #[serde(rename = "RUN_ERROR")]
    RunError {
        #[serde(skip)]
        run_id: String,
        message: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        code: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
        #[serde(skip)]
        details: Option<serde_json::Value>,
    },

    /// Step (task) started
    #[serde(rename = "STEP_STARTED")]
    StepStarted {
        #[serde(skip)]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(skip)]
        parent_step_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Map<String, serde_json::Value>>,
    },

    /// Step (task) finished
    #[serde(rename = "STEP_FINISHED")]
    StepFinished {
        #[serde(skip)]
        step_id: String,
        #[serde(rename = "stepName")]
        step_name: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
        #[serde(skip)]
        result: Option<serde_json::Value>,
    },

    // === Message Events ===
    /// Text message start
    #[serde(rename = "TEXT_MESSAGE_START")]
    TextMessageStart {
        #[serde(rename = "messageId")]
        message_id: String,
        role: Role,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Text message content chunk
    #[serde(rename = "TEXT_MESSAGE_CONTENT")]
    TextMessageContent {
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Text message end
    #[serde(rename = "TEXT_MESSAGE_END")]
    TextMessageEnd {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    // === Tool Call Events ===
    /// Tool call start
    #[serde(rename = "TOOL_CALL_START")]
    ToolCallStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolCallName")]
        tool_call_name: String,
        #[serde(
            default,
            rename = "parentMessageId",
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        parent_message_id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Tool call arguments chunk
    #[serde(rename = "TOOL_CALL_ARGS")]
    ToolCallArgs {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        delta: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Tool call end
    #[serde(rename = "TOOL_CALL_END")]
    ToolCallEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Tool call result
    #[serde(rename = "TOOL_CALL_RESULT")]
    ToolCallResult {
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        content: String,
        #[serde(skip)]
        result: serde_json::Value,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    // === State Events ===
    /// State snapshot
    #[serde(rename = "STATE_SNAPSHOT")]
    StateSnapshot {
        snapshot: serde_json::Value,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// State delta (JSON Patch)
    #[serde(rename = "STATE_DELTA")]
    StateDelta {
        delta: Vec<serde_json::Value>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    // === Special Events ===
    /// Raw event for external system integration
    #[serde(rename = "RAW")]
    Raw {
        event: serde_json::Value,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
    },

    /// Custom event for extensions
    #[serde(rename = "CUSTOM")]
    Custom {
        name: String,
        value: serde_json::Value,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        timestamp: Option<i64>,
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

    /// Get current timestamp in milliseconds as a safe JSON integer.
    pub fn now_timestamp() -> i64 {
        chrono::Utc::now().timestamp_millis()
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
    pub fn run_finished(
        run_id: impl Into<String>,
        thread_id: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> Self {
        AgUiEvent::RunFinished {
            run_id: run_id.into(),
            thread_id: thread_id.into(),
            timestamp: Some(Self::now_timestamp()),
            result,
            outcome: Some(RunOutcome::Success),
            interrupt: None,
        }
    }

    /// Create a RUN_FINISHED event with success outcome (explicit)
    pub fn run_finished_success(
        run_id: impl Into<String>,
        thread_id: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> Self {
        Self::run_finished(run_id, thread_id, result)
    }

    /// Create a RUN_FINISHED event with interrupt outcome (HITL)
    pub fn run_finished_with_interrupt(
        run_id: impl Into<String>,
        thread_id: impl Into<String>,
        interrupt: InterruptInfo,
    ) -> Self {
        let mut event = Self::run_finished_with_interrupts(
            run_id,
            thread_id,
            vec![AgUiInterrupt {
                id: interrupt.id.clone(),
                reason: interrupt.reason.clone(),
            }],
        );
        if let AgUiEvent::RunFinished {
            interrupt: legacy, ..
        } = &mut event
        {
            *legacy = Some(interrupt);
        }
        event
    }

    /// Create a RUN_FINISHED event with standard interrupt outcomes.
    pub fn run_finished_with_interrupts(
        run_id: impl Into<String>,
        thread_id: impl Into<String>,
        interrupts: Vec<AgUiInterrupt>,
    ) -> Self {
        AgUiEvent::RunFinished {
            run_id: run_id.into(),
            thread_id: thread_id.into(),
            timestamp: Some(Self::now_timestamp()),
            result: None,
            outcome: Some(RunOutcome::Interrupt { interrupts }),
            interrupt: None,
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

    /// Create the RUN_ERROR that ends a run its client cancelled
    pub fn run_cancelled(run_id: impl Into<String>) -> Self {
        Self::run_error(
            run_id,
            "Run cancelled by user",
            Some(crate::error::AgUiError::Cancelled.error_code().to_string()),
            None,
        )
    }

    /// Whether this event ends its run
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            AgUiEvent::RunFinished { .. } | AgUiEvent::RunError { .. }
        )
    }

    /// Create a STEP_STARTED event
    pub fn step_started(step_id: impl Into<String>, step_name: impl Into<String>) -> Self {
        AgUiEvent::StepStarted {
            step_id: step_id.into(),
            step_name: step_name.into(),
            parent_step_id: None,
            timestamp: Some(Self::now_timestamp()),
            metadata: None,
        }
    }

    /// Create a STEP_FINISHED event
    pub fn step_finished(
        step_id: impl Into<String>,
        step_name: impl Into<String>,
        result: Option<serde_json::Value>,
    ) -> Self {
        AgUiEvent::StepFinished {
            step_id: step_id.into(),
            step_name: step_name.into(),
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
            message_id: crate::types::MessageId::random().to_string(),
            tool_call_id: tool_call_id.into(),
            content: result.to_string(),
            result,
            timestamp: Some(Self::now_timestamp()),
        }
    }

    /// Create a STATE_SNAPSHOT event
    pub fn state_snapshot(snapshot: serde_json::Value) -> Self {
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
            value: data,
            timestamp: Some(Self::now_timestamp()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official_schema() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/ag-ui-1.0-schema.json")).unwrap()
    }

    #[test]
    fn test_run_started_serialization() {
        let event = AgUiEvent::RunStarted {
            run_id: "run_123".to_string(),
            thread_id: "thread_456".to_string(),
            timestamp: Some(1234567890123),
            metadata: None,
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "RUN_STARTED");
        assert_eq!(json["runId"], "run_123");
        assert_eq!(json["threadId"], "thread_456");
        assert_eq!(json["timestamp"], 1234567890123_i64);
    }

    #[test]
    fn test_run_error_serialization() {
        let event = AgUiEvent::RunError {
            run_id: "run_123".to_string(),
            message: "Task failed".to_string(),
            code: Some("TASK_FAILED".to_string()),
            timestamp: Some(1234567890123),
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
        let event = AgUiEvent::StateSnapshot {
            snapshot: serde_json::json!({"public":"my_workflow"}),
            timestamp: Some(1234567890123),
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "STATE_SNAPSHOT");
        assert_eq!(
            json["snapshot"],
            serde_json::json!({"public":"my_workflow"})
        );
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
            value: serde_json::json!({ "key": "value" }),
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
        let event = AgUiEvent::run_finished(
            "run_1",
            "thread_1",
            Some(serde_json::json!({"result": "ok"})),
        );
        match event {
            AgUiEvent::RunFinished {
                run_id,
                result,
                timestamp,
                outcome,
                thread_id,
                interrupt,
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(thread_id, "thread_1");
                assert!(result.is_some());
                assert!(timestamp.is_some());
                assert_eq!(outcome, Some(RunOutcome::Success));
                assert!(interrupt.is_none());
            }
            _ => panic!("Expected RunFinished"),
        }
    }

    #[test]
    fn lifecycle_events_serialize_standard_wire_fields() {
        let finished = serde_json::to_value(AgUiEvent::run_finished("r", "t", None)).unwrap();
        assert_eq!(finished["threadId"], "t");
        assert_eq!(finished["outcome"]["type"], "success");
        assert!(finished.get("interrupt").is_none());
        let error = serde_json::to_value(AgUiEvent::run_error("r", "failed", None, None)).unwrap();
        assert_eq!(error["message"], "failed");
        assert!(error.get("runId").is_none());
        let step =
            serde_json::to_value(AgUiEvent::step_finished("internal", "named", None)).unwrap();
        assert_eq!(step["stepName"], "named");
        assert!(step.get("stepId").is_none());
    }

    #[test]
    fn emitted_lifecycle_and_tool_events_validate_against_pinned_ag_ui_schema() {
        let schema = official_schema();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let events = [
            AgUiEvent::run_started("run", "thread"),
            AgUiEvent::run_finished("run", "thread", Some(serde_json::json!({"ok":true}))),
            AgUiEvent::run_error(
                "run",
                "failed",
                Some("ERR".into()),
                Some(serde_json::json!({"private":"details"})),
            ),
            AgUiEvent::step_started("step", "step"),
            AgUiEvent::StepStarted {
                step_id: "internal".into(),
                step_name: "named".into(),
                parent_step_id: Some("parent".into()),
                timestamp: None,
                metadata: None,
            },
            AgUiEvent::step_finished("step", "step", Some(serde_json::json!({"result":true}))),
            AgUiEvent::text_message_start("message", Role::Assistant),
            AgUiEvent::text_message_content("message", "hello"),
            AgUiEvent::text_message_end("message"),
            AgUiEvent::tool_call_start("call", "lookup", None),
            AgUiEvent::tool_call_args("call", "{}"),
            AgUiEvent::tool_call_end("call"),
            AgUiEvent::tool_call_result("call", serde_json::json!({"ok":true})),
            AgUiEvent::StateSnapshot {
                snapshot: serde_json::json!(["public"]),
                timestamp: None,
            },
            AgUiEvent::state_delta(vec![
                serde_json::json!({"op":"replace","path":"","value":{}}),
            ]),
            AgUiEvent::Raw {
                event: serde_json::json!({"provider":"raw"}),
                timestamp: Some(1),
            },
            AgUiEvent::Custom {
                name: "notice".into(),
                value: serde_json::json!({"text":"hello"}),
                timestamp: None,
            },
            AgUiEvent::run_finished_with_interrupt(
                "run",
                "thread",
                InterruptInfo {
                    id: "interrupt".into(),
                    reason: "approval".into(),
                    payload: InterruptPayload {
                        pending_tool_calls: vec![],
                        checkpoint_position: "task".into(),
                        workflow_name: "workflow".into(),
                    },
                },
            ),
        ];
        for event in events {
            let value = serde_json::to_value(event).unwrap();
            assert!(validator.is_valid(&value), "invalid event: {value}");
        }
    }

    #[test]
    fn interrupt_outcome_requires_at_least_one_interrupt() {
        assert!(
            serde_json::from_str::<RunOutcome>(r#"{"type":"interrupt","interrupts":[]}"#).is_err()
        );
        let event = AgUiEvent::run_finished_with_interrupts("r", "t", vec![]);
        assert!(serde_json::to_value(event).is_err());
    }

    #[test]
    fn optional_event_fields_reject_null() {
        assert!(
            serde_json::from_str::<AgUiEvent>(
                r#"{"type":"RUN_STARTED","runId":"r","threadId":"t","timestamp":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<AgUiEvent>(
                r#"{"type":"RUN_STARTED","runId":"r","threadId":"t","metadata":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<AgUiEvent>(
                r#"{"type":"RUN_FINISHED","runId":"r","threadId":"t","result":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::to_value(AgUiEvent::run_finished(
                "r",
                "t",
                Some(serde_json::Value::Null)
            ))
            .is_err()
        );
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

        let event =
            AgUiEvent::run_finished_with_interrupt("run_1", "thread_1", interrupt_info.clone());
        match &event {
            AgUiEvent::RunFinished {
                run_id,
                result,
                timestamp,
                outcome,
                interrupt,
                thread_id,
            } => {
                assert_eq!(run_id, "run_1");
                assert_eq!(thread_id, "thread_1");
                assert!(result.is_none());
                assert!(timestamp.is_some());
                assert!(matches!(outcome, Some(RunOutcome::Interrupt { .. })));
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
        assert_eq!(json["outcome"]["type"], "interrupt");
        assert_eq!(json["outcome"]["interrupts"][0]["id"], "int_123");
        assert_eq!(
            json["outcome"]["interrupts"][0]["reason"],
            "tool_approval_required"
        );
        assert_eq!(json["threadId"], "thread_1");
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

        let finished =
            AgUiEvent::step_finished("step_1", "step_1", Some(serde_json::json!({"done": true})));
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
        let state = serde_json::json!({"public":"my_workflow"});
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
