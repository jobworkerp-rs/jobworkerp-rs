use crate::jobworkerp::runner::llm::llm_chat_args::{
    ChatMessage, MessageContent, MessageParts, chat_message::Body,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub(crate) struct ChatMessageSerde {
    role: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parts: Option<MessageParts>,
}

impl TryFrom<ChatMessageSerde> for ChatMessage {
    type Error = String;

    fn try_from(value: ChatMessageSerde) -> Result<Self, Self::Error> {
        let body = match (value.content, value.parts) {
            (Some(_), Some(_)) => {
                return Err("chat message cannot set both content and parts".to_string());
            }
            (Some(content), None) => Some(Body::Content(content)),
            (None, Some(parts)) => Some(Body::Parts(parts)),
            (None, None) => None,
        };
        Ok(Self {
            role: value.role,
            body,
        })
    }
}

impl From<ChatMessage> for ChatMessageSerde {
    fn from(value: ChatMessage) -> Self {
        let (content, parts) = match value.body {
            Some(Body::Content(content)) => (Some(content), None),
            Some(Body::Parts(parts)) => (None, Some(parts)),
            None => (None, None),
        };
        Self {
            role: value.role,
            content,
            parts,
        }
    }
}

// Schemars reads these fields to project ChatMessage's public JSON shape.
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
pub(crate) struct ChatMessageSchema {
    role: i32,
    content: Option<MessageContent>,
    parts: Option<MessageParts>,
}
