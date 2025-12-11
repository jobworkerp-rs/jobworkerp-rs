//! SSE (Server-Sent Events) encoder for AG-UI events.

use crate::events::AgUiEvent;
use std::sync::atomic::{AtomicU64, Ordering};

/// SSE event encoder that tracks event IDs for reconnection support.
pub struct EventEncoder {
    event_id: AtomicU64,
}

impl EventEncoder {
    /// Create a new encoder starting at event ID 0
    pub fn new() -> Self {
        Self {
            event_id: AtomicU64::new(0),
        }
    }

    /// Create a new encoder starting at a specific event ID
    pub fn with_start_id(start_id: u64) -> Self {
        Self {
            event_id: AtomicU64::new(start_id),
        }
    }

    /// Encode an AG-UI event to SSE format.
    /// Returns (event_id, formatted SSE string)
    pub fn encode(&self, event: &AgUiEvent) -> Result<(u64, String), serde_json::Error> {
        let event_id = self.event_id.fetch_add(1, Ordering::SeqCst);
        let data = serde_json::to_string(event)?;
        let event_type = event.event_type();

        // SSE format:
        // id: <event_id>
        // event: <event_type>
        // data: <json>
        // (empty line to end event)
        let sse = format!(
            "id: {}\nevent: {}\ndata: {}\n\n",
            event_id, event_type, data
        );

        Ok((event_id, sse))
    }

    /// Encode an AG-UI event to SSE format without incrementing the event ID.
    /// Used for replaying events.
    pub fn encode_with_id(
        &self,
        event: &AgUiEvent,
        event_id: u64,
    ) -> Result<String, serde_json::Error> {
        let data = serde_json::to_string(event)?;
        let event_type = event.event_type();

        let sse = format!(
            "id: {}\nevent: {}\ndata: {}\n\n",
            event_id, event_type, data
        );

        Ok(sse)
    }

    /// Get the current event ID (next ID to be assigned)
    pub fn current_event_id(&self) -> u64 {
        self.event_id.load(Ordering::SeqCst)
    }

    /// Set the event ID (for reconnection scenarios)
    pub fn set_event_id(&self, id: u64) {
        self.event_id.store(id, Ordering::SeqCst);
    }
}

impl Default for EventEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode a comment for SSE keep-alive
pub fn encode_comment(comment: &str) -> String {
    format!(": {}\n\n", comment)
}

/// Encode a retry directive for SSE
pub fn encode_retry(milliseconds: u32) -> String {
    format!("retry: {}\n\n", milliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Role;

    #[test]
    fn test_encode_run_started() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::RunStarted {
            run_id: "run_123".to_string(),
            thread_id: "thread_456".to_string(),
            timestamp: Some(1234567890123.0),
            metadata: None,
        };

        let (id, sse) = encoder.encode(&event).unwrap();
        assert_eq!(id, 0);
        assert!(sse.contains("id: 0"));
        assert!(sse.contains("event: RUN_STARTED"));
        assert!(sse.contains("\"runId\":\"run_123\""));
        assert!(sse.ends_with("\n\n"));
    }

    #[test]
    fn test_encode_increments_id() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::RunStarted {
            run_id: "run_1".to_string(),
            thread_id: "thread_1".to_string(),
            timestamp: None,
            metadata: None,
        };

        let (id1, _) = encoder.encode(&event).unwrap();
        let (id2, _) = encoder.encode(&event).unwrap();
        let (id3, _) = encoder.encode(&event).unwrap();

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);
        assert_eq!(id3, 2);
    }

    #[test]
    fn test_encode_with_start_id() {
        let encoder = EventEncoder::with_start_id(100);
        let event = AgUiEvent::RunFinished {
            run_id: "run_1".to_string(),
            timestamp: None,
            result: None,
        };

        let (id, sse) = encoder.encode(&event).unwrap();
        assert_eq!(id, 100);
        assert!(sse.contains("id: 100"));
    }

    #[test]
    fn test_encode_text_message() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::TextMessageContent {
            message_id: "msg_1".to_string(),
            delta: "Hello, world!".to_string(),
            timestamp: None,
        };

        let (_, sse) = encoder.encode(&event).unwrap();
        assert!(sse.contains("event: TEXT_MESSAGE_CONTENT"));
        assert!(sse.contains("\"delta\":\"Hello, world!\""));
    }

    #[test]
    fn test_encode_with_id() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::StepStarted {
            step_id: "step_1".to_string(),
            step_name: Some("initialize".to_string()),
            parent_step_id: None,
            timestamp: None,
            metadata: None,
        };

        let sse = encoder.encode_with_id(&event, 42).unwrap();
        assert!(sse.contains("id: 42"));
        assert!(sse.contains("event: STEP_STARTED"));

        // Should not affect the internal counter
        assert_eq!(encoder.current_event_id(), 0);
    }

    #[test]
    fn test_set_event_id() {
        let encoder = EventEncoder::new();
        encoder.set_event_id(50);
        assert_eq!(encoder.current_event_id(), 50);

        let event = AgUiEvent::RunFinished {
            run_id: "run_1".to_string(),
            timestamp: None,
            result: None,
        };
        let (id, _) = encoder.encode(&event).unwrap();
        assert_eq!(id, 50);
    }

    #[test]
    fn test_encode_comment() {
        let comment = encode_comment("keep-alive");
        assert_eq!(comment, ": keep-alive\n\n");
    }

    #[test]
    fn test_encode_retry() {
        let retry = encode_retry(3000);
        assert_eq!(retry, "retry: 3000\n\n");
    }

    #[test]
    fn test_encode_complex_event() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::ToolCallResult {
            tool_call_id: "call_123".to_string(),
            result: serde_json::json!({
                "status": 200,
                "body": { "data": [1, 2, 3] }
            }),
            timestamp: Some(1234567890123.0),
        };

        let (_, sse) = encoder.encode(&event).unwrap();
        assert!(sse.contains("event: TOOL_CALL_RESULT"));
        assert!(sse.contains("\"toolCallId\":\"call_123\""));
        assert!(sse.contains("\"status\":200"));
    }

    #[test]
    fn test_encode_role_serialization() {
        let encoder = EventEncoder::new();
        let event = AgUiEvent::TextMessageStart {
            message_id: "msg_1".to_string(),
            role: Role::Assistant,
            timestamp: None,
        };

        let (_, sse) = encoder.encode(&event).unwrap();
        assert!(sse.contains("\"role\":\"assistant\""));
    }
}
