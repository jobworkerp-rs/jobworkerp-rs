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
    Developer,
    Activity,
    Reasoning,
}

/// A message in the conversation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    /// Unique identifier for this message
    pub id: MessageId,

    /// Role of the message sender
    pub role: Role,

    /// Message content (text or structured)
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub content: Option<MessageContent>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_calls: Option<Vec<crate::types::tool::ToolCall>>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub encrypted_value: Option<String>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub subagent_run_id: Option<String>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub activity_type: Option<String>,

    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<String>,

    /// Optional display name for base message roles.
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub name: Option<String>,

    /// Tool call ID for tool response messages
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tool_call_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MessageWire {
    id: MessageId,
    role: Role,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    content: Option<MessageContent>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    tool_calls: Option<Vec<crate::types::tool::ToolCall>>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    name: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    tool_call_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    encrypted_value: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    metadata: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    subagent_run_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    activity_type: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option"
    )]
    error: Option<String>,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = MessageWire::deserialize(deserializer)?;
        use serde::de::Error;
        let valid = match wire.role {
            Role::Assistant => {
                wire.content
                    .as_ref()
                    .is_none_or(|content| matches!(content, MessageContent::Text(_)))
                    && wire.activity_type.is_none()
                    && wire.tool_call_id.is_none()
                    && wire.error.is_none()
            }
            Role::Activity => {
                wire.activity_type.is_some()
                    && matches!(wire.content, Some(MessageContent::Object(_)))
                    && wire.tool_calls.is_none()
                    && wire.name.is_none()
                    && wire.encrypted_value.is_none()
                    && wire.tool_call_id.is_none()
                    && wire.error.is_none()
            }
            Role::Reasoning => {
                matches!(wire.content, Some(MessageContent::Text(_)))
                    && wire.activity_type.is_none()
                    && wire.tool_calls.is_none()
                    && wire.name.is_none()
                    && wire.tool_call_id.is_none()
                    && wire.error.is_none()
            }
            Role::Tool => {
                wire.content.as_ref().is_some_and(|c| {
                    matches!(c, MessageContent::Text(_) | MessageContent::Parts(_))
                }) && wire.tool_call_id.is_some()
                    && wire.name.is_none()
                    && wire.activity_type.is_none()
                    && wire.tool_calls.is_none()
            }
            Role::User => {
                wire.content.as_ref().is_some_and(|c| {
                    matches!(c, MessageContent::Text(_) | MessageContent::Parts(_))
                }) && wire.tool_calls.is_none()
                    && wire.activity_type.is_none()
                    && wire.tool_call_id.is_none()
                    && wire.error.is_none()
            }
            Role::System | Role::Developer => {
                matches!(wire.content, Some(MessageContent::Text(_)))
                    && wire.tool_calls.is_none()
                    && wire.activity_type.is_none()
                    && wire.tool_call_id.is_none()
                    && wire.error.is_none()
            }
        };
        if !valid {
            return Err(D::Error::custom(
                "message fields do not match the AG-UI role variant",
            ));
        }
        Ok(Self {
            id: wire.id,
            role: wire.role,
            content: wire.content,
            tool_calls: wire.tool_calls,
            name: wire.name,
            tool_call_id: wire.tool_call_id,
            encrypted_value: wire.encrypted_value,
            metadata: wire.metadata,
            subagent_run_id: wire.subagent_run_id,
            activity_type: wire.activity_type,
            error: wire.error,
        })
    }
}

impl Message {
    /// Create a new user message
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::User,
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            encrypted_value: None,
            metadata: None,
            subagent_run_id: None,
            activity_type: None,
            name: None,
            tool_call_id: None,
            error: None,
        }
    }

    /// Create a new assistant message
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::Assistant,
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            encrypted_value: None,
            metadata: None,
            subagent_run_id: None,
            activity_type: None,
            name: None,
            tool_call_id: None,
            error: None,
        }
    }

    /// Create a new system message
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::random(),
            role: Role::System,
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            encrypted_value: None,
            metadata: None,
            subagent_run_id: None,
            activity_type: None,
            name: None,
            tool_call_id: None,
            error: None,
        }
    }

    /// Create a new tool response message
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        let tool_call_id_str = tool_call_id.into();
        Self {
            id: MessageId::random(),
            role: Role::Tool,
            content: Some(MessageContent::Text(content.into())),
            tool_calls: None,
            encrypted_value: None,
            metadata: None,
            subagent_run_id: None,
            activity_type: None,
            name: None,
            tool_call_id: Some(tool_call_id_str),
            error: None,
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
    /// Open JSON object used by AG-UI activity messages.
    Object(serde_json::Map<String, serde_json::Value>),
}

impl MessageContent {
    /// Extract text content from the message.
    /// For Text variant, returns the text directly.
    /// For Parts variant, concatenates all text parts.
    pub fn extract_text(&self) -> String {
        match self {
            Self::Text(t) => t.clone(),
            Self::Object(value) => serde_json::Value::Object(value.clone()).to_string(),
            Self::Parts(parts) => parts
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
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
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContentPart {
    /// Text content part
    Text {
        text: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Value>,
    },
    Image {
        source: PartSource,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Value>,
    },
    Audio {
        source: PartSource,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Value>,
    },
    Video {
        source: PartSource,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Value>,
    },
    Document {
        source: PartSource,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        id: Option<String>,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PartSource {
    Data {
        value: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Url {
        value: String,
        #[serde(
            default,
            rename = "mimeType",
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        mime_type: Option<String>,
    },
    File {
        value: String,
        #[serde(
            default,
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        provider: Option<String>,
        #[serde(
            default,
            rename = "mimeType",
            deserialize_with = "crate::types::deserialize_non_null_option",
            skip_serializing_if = "Option::is_none"
        )]
        mime_type: Option<String>,
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
        assert!(matches!(msg.content, Some(MessageContent::Text(ref s)) if s == "Hello, world!"));
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
                id: None,
                metadata: None,
            },
            ContentPart::Image {
                source: PartSource::Url {
                    value: "https://example.com/image.png".to_string(),
                    mime_type: None,
                },
                id: None,
                metadata: None,
            },
        ];
        let content = MessageContent::Parts(parts);
        let json = serde_json::to_value(&content).unwrap();
        assert!(json.is_array());
        assert_eq!(json.as_array().unwrap().len(), 2);
    }

    #[test]
    fn role_variants_require_their_standard_fields() {
        assert!(
            serde_json::from_str::<Message>(r#"{"id":"m","role":"activity","content":{}}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Message>(
                r#"{"id":"m","role":"activity","activityType":"progress","content":{"step":1}}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<Message>(
                r#"{"id":"m","role":"reasoning","content":"thinking"}"#
            )
            .is_ok()
        );
        assert!(serde_json::from_str::<Message>(r#"{"id":"m","role":"assistant","toolCalls":[{"id":"c","type":"function","function":{"name":"f","arguments":"{}"}}]}"#).is_ok());
        assert!(serde_json::from_str::<Message>(r#"{"id":"m","role":"assistant"}"#).is_ok());
        assert!(
            serde_json::from_str::<Message>(
                r#"{"id":"m","role":"user","content":"x","toolCallId":"c"}"#
            )
            .is_err()
        );
        assert!(serde_json::from_str::<Message>(r#"{"id":"m","role":"activity","activityType":"progress","content":{},"encryptedValue":"opaque"}"#).is_err());
    }

    #[test]
    fn message_variants_validate_against_the_pinned_schema() {
        let mut schema: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ag-ui-1.0-schema.json"))
                .unwrap();
        schema["$ref"] = serde_json::json!("#/$defs/Message");
        let validator = jsonschema::validator_for(&schema).unwrap();
        let messages = [
            serde_json::json!({"id":"u","role":"user","content":"hello"}),
            serde_json::json!({"id":"a","role":"assistant","encryptedValue":"opaque","metadata":{"source":null},"toolCalls":[{"id":"c","type":"function","function":{"name":"lookup","arguments":"{}"},"encryptedValue":"call-opaque","metadata":{"trace":1}}]}),
            serde_json::json!({"id":"t","role":"tool","toolCallId":"c","content":"done","error":"partial"}),
            serde_json::json!({"id":"d","role":"developer","content":"instructions"}),
            serde_json::json!({"id":"s","role":"system","content":"system"}),
            serde_json::json!({"id":"p","role":"activity","activityType":"progress","content":{"step":1}}),
            serde_json::json!({"id":"r","role":"reasoning","content":"hidden","encryptedValue":"opaque"}),
        ];
        for message in messages {
            let parsed: Message = serde_json::from_value(message.clone()).unwrap();
            let emitted = serde_json::to_value(parsed).unwrap();
            assert!(validator.is_valid(&emitted), "invalid message: {emitted}");
        }
        let bad = serde_json::json!({"id":"p","role":"activity","content":{}});
        assert!(!validator.is_valid(&bad));
        assert!(serde_json::from_value::<Message>(bad).is_err());
    }

    #[test]
    fn optional_message_and_part_fields_reject_null() {
        for message in [
            r#"{"id":"m","role":"assistant","content":null}"#,
            r#"{"id":"m","role":"assistant","metadata":null}"#,
            r#"{"id":"m","role":"assistant","toolCalls":null}"#,
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"hello","metadata":null}]}"#,
            r#"{"id":"m","role":"user","content":[{"type":"image","source":{"type":"url","value":"https://example.test/image","mimeType":null}}]}"#,
        ] {
            assert!(
                serde_json::from_str::<Message>(message).is_err(),
                "accepted {message}"
            );
        }
    }

    #[test]
    fn standard_message_parts_validate_against_the_pinned_schema() {
        let mut schema: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ag-ui-1.0-schema.json"))
                .unwrap();
        schema["$ref"] = serde_json::json!("#/$defs/Message");
        let validator = jsonschema::validator_for(&schema).unwrap();
        let message = serde_json::json!({
            "id":"u",
            "role":"user",
            "content":[
                {"type":"text","text":"caption","metadata":42},
                {"type":"image","source":{"type":"data","value":"aGVsbG8=","mimeType":"image/png"}},
                {"type":"audio","source":{"type":"url","value":"https://example.test/audio"}},
                {"type":"document","source":{"type":"file","value":"file-1","provider":"upload"}}
            ]
        });
        let parsed: Message = serde_json::from_value(message).unwrap();
        let emitted = serde_json::to_value(parsed).unwrap();
        assert!(validator.is_valid(&emitted), "invalid message: {emitted}");
    }
}
