//! Message types for AG-UI protocol.
//!
//! References AG-UI Rust SDK (ag-ui-core) message.rs

use crate::types::ids::MessageId;
use serde::{Deserialize, Serialize};

/// Role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// User/human message
    #[default]
    User,
    /// Assistant/AI message
    Assistant,
    /// System message
    System,
    /// Tool response message
    Tool,
}

/// A message in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Unique identifier for this message
    pub id: MessageId,

    /// Role of the message sender
    pub role: Role,

    /// Message content (text or structured)
    pub content: MessageContent,

    /// Optional name for tool messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Tool call ID for tool response messages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// Create a new user message
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::User,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    /// Create a new assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::Assistant,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    /// Create a new system message
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::System,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    /// Create a new tool response message
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            id: MessageId::random(),
            role: Role::Tool,
            content: MessageContent::Text(content.into()),
            name: None,
            tool_call_id: Some(tool_call_id_str),
        }
    }

    /// Set message ID
    pub fn with_id(mut self, id: MessageId) -> Self {
        self.id = id;
        self
    }
}

/// Content of a message - text or structured parts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Simple text content
    Text(String),
    /// Structured content parts
    Parts(Vec<ContentPart>),
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

/// A part of structured content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// Text content part
    Text { text: String },
    /// Image content part
    Image { url: String },
    /// Tool use content part
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// Tool result content part
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            "\"assistant\""
        );
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), "\"system\"");
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), "\"tool\"");
    }

    #[test]
    fn test_message_user() {
        let msg = Message::user("Hello, world!");
        assert_eq!(msg.role, Role::User);
        assert!(matches!(msg.content, MessageContent::Text(ref s) if s == "Hello, world!"));
    }

    #[test]
    fn test_message_serialization() {
        let msg = Message::user("Test message");
        let json = serde_json::to_value(&msg).unwrap();

        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Test message");
        assert!(json["id"].is_string());
    }

    #[test]
    fn test_message_tool() {
        let msg = Message::tool("call_123", "Tool result");
        assert_eq!(msg.role, Role::Tool);
        assert_eq!(msg.tool_call_id, Some("call_123".to_string()));
    }

    #[test]
    fn test_content_parts() {
        let parts = vec![
            ContentPart::Text {
                text: "Hello".to_string(),
            },
            ContentPart::Image {
                url: "https://example.com/image.png".to_string(),
            },
        ];
        let content = MessageContent::Parts(parts);
        let json = serde_json::to_value(&content).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 2);
    }
}
