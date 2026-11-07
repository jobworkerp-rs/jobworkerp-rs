//! Converts the workflow's explicit public output contract into AG-UI events.

use crate::{
    events::AgUiEvent,
    types::{Message, MessageContent, Role},
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug)]
pub struct WorkflowProjection {
    pub events: Vec<AgUiEvent>,
    pub result: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
#[error("workflow output does not match the public AG-UI output contract")]
pub struct ProjectionError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PublicOutput {
    messages: Vec<Message>,
    #[serde(default)]
    state: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
}

/// Validates workflow output before projecting only its public fields.
pub fn project_workflow_output(value: &Value) -> Result<WorkflowProjection, ProjectionError> {
    let output = PublicOutput::deserialize(value).map_err(|_| ProjectionError)?;
    let mut events = Vec::new();
    let mut known_server_calls = HashSet::new();

    for message in output.messages {
        if message.role == Role::Assistant {
            if let Some(content) = &message.content {
                let text = content.extract_text();
                if !text.is_empty() {
                    events.extend([
                        AgUiEvent::text_message_start(message.id.to_string(), message.role),
                        AgUiEvent::text_message_content(message.id.to_string(), text),
                        AgUiEvent::text_message_end(message.id.to_string()),
                    ]);
                }
            }
            for call in message.tool_calls.unwrap_or_default() {
                if !known_server_calls.insert(call.id.clone()) {
                    return Err(ProjectionError);
                }
                events.extend([
                    AgUiEvent::tool_call_start(
                        call.id.clone(),
                        call.function.name,
                        Some(message.id.to_string()),
                    ),
                    AgUiEvent::tool_call_args(call.id.clone(), call.function.arguments),
                    AgUiEvent::tool_call_end(call.id),
                ]);
            }
        } else if message.role == Role::Tool {
            let call_id = message.tool_call_id.ok_or(ProjectionError)?;
            if !known_server_calls.contains(&call_id) {
                return Err(ProjectionError);
            }
            let content = message
                .content
                .as_ref()
                .map(MessageContent::extract_text)
                .ok_or(ProjectionError)?;
            events.push(AgUiEvent::ToolCallResult {
                message_id: message.id.to_string(),
                tool_call_id: call_id,
                content: content.clone(),
                result: Value::String(content),
                timestamp: Some(AgUiEvent::now_timestamp()),
            });
        } else {
            return Err(ProjectionError);
        }
    }

    if let Some(state) = output.state {
        events.push(AgUiEvent::state_snapshot(state));
    }
    Ok(WorkflowProjection {
        events,
        result: output.result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_messages_server_tool_result_and_public_state() {
        let projection = project_workflow_output(&json!({
            "messages": [
                {"id":"a1","role":"assistant","content":"done","toolCalls":[
                    {"id":"call-1","type":"function","function":{"name":"lookup","arguments":"{}"}}
                ]},
                {"id":"t1","role":"tool","content":"{\"ok\":true}","toolCallId":"call-1"}
            ], "state":{"public":true}, "result":{"count":1}
        }))
        .unwrap();
        assert!(matches!(
            projection.events.first(),
            Some(AgUiEvent::TextMessageStart { .. })
        ));
        assert!(projection.events.iter().any(|e| matches!(e, AgUiEvent::ToolCallStart { tool_call_id, .. } if tool_call_id == "call-1")));
        assert!(projection.events.iter().any(|e| matches!(e, AgUiEvent::ToolCallResult { tool_call_id, message_id, .. } if tool_call_id == "call-1" && message_id == "t1")));
        assert!(
            matches!(projection.events.last(), Some(AgUiEvent::StateSnapshot { snapshot, .. }) if snapshot["public"] == true)
        );
        assert_eq!(projection.result, Some(json!({"count":1})));
    }

    #[test]
    fn rejects_internal_output_fields_without_emitting_events() {
        assert!(
            project_workflow_output(
                &json!({"messages":[],"contextVariables":{"secret":"private"}})
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_tool_result_without_a_preceding_matching_call() {
        assert!(project_workflow_output(&json!({"messages":[{"id":"t1","role":"tool","content":"ok","toolCallId":"missing"}]})).is_err());
    }
}
