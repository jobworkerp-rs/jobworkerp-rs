//! AG-UI 1.0 run input types.

use crate::types::{context::Context, message::Message, tool::Tool};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResumeEntry {
    pub interrupt_id: String,
    pub status: ResumeStatus,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub payload: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResumeStatus {
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunAgentInput {
    pub thread_id: String,
    pub run_id: String,
    pub messages: Vec<Message>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub protocol_version: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent_run_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub state: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub tools: Option<Vec<Tool>>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub context: Option<Vec<Context>>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub forwarded_props: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub resume: Option<Vec<ResumeEntry>>,
}

impl RunAgentInput {
    pub fn is_resume(&self) -> bool {
        self.resume.as_ref().is_some_and(|r| !r.is_empty())
    }
    pub fn get_resume(&self) -> Option<&[ResumeEntry]> {
        self.resume.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_run_input_requires_ids_messages_and_keeps_arbitrary_json_state() {
        let input: RunAgentInput = serde_json::from_str(
            r#"{"threadId":"t","runId":"r","messages":[],"state":{"x":1},"forwardedProps":[1]}"#,
        )
        .unwrap();
        assert_eq!(input.state, Some(serde_json::json!({"x":1})));
        assert_eq!(input.forwarded_props, Some(serde_json::json!([1])));
        assert!(serde_json::from_str::<RunAgentInput>(r#"{"runId":"r","messages":[]}"#).is_err());
        assert!(serde_json::from_str::<RunAgentInput>(r#"{"threadId":"t","runId":"r"}"#).is_err());
        assert!(
            serde_json::from_str::<RunAgentInput>(
                r#"{"threadId":"t","runId":"r","messages":[],"extra":1}"#
            )
            .is_err()
        );
    }

    #[test]
    fn run_input_is_checked_by_the_pinned_ag_ui_10_schema() {
        let mut schema: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ag-ui-1.0-schema.json"))
                .unwrap();
        schema["$ref"] = serde_json::json!("#/$defs/RunAgentInput");
        let validator = jsonschema::validator_for(&schema).unwrap();
        let valid = serde_json::json!({
            "threadId":"t",
            "runId":"r",
            "messages":[{"id":"m","role":"user","content":"hello"}],
            "state":[1,"public"],
            "tools":[{"name":"lookup","description":"Find a record","parameters":{"type":"object"},"metadata":{"owner":null}}],
            "context":[{"description":"page","value":"example"}],
            "forwardedProps":{"theme":"dark"},
            "resume":[{"interruptId":"i","status":"resolved","payload":{"ok":true},"metadata":{"source":null}}]
        });
        assert!(validator.is_valid(&valid));
        let parsed: RunAgentInput = serde_json::from_value(valid).unwrap();
        assert!(validator.is_valid(&serde_json::to_value(parsed).unwrap()));
        let null_state = serde_json::json!({"threadId":"t","runId":"r","messages":[],"state":null});
        assert!(!validator.is_valid(&null_state));
        assert!(serde_json::from_value::<RunAgentInput>(null_state).is_err());
        let extension =
            serde_json::json!({"threadId":"t","runId":"r","messages":[],"workflow_inline":{}});
        assert!(!validator.is_valid(&extension));
    }

    #[test]
    fn resume_is_an_array_with_standard_statuses() {
        let input: RunAgentInput = serde_json::from_str(r#"{"threadId":"t","runId":"r","messages":[],"resume":[{"interruptId":"i","status":"resolved","payload":{"ok":true}}]}"#).unwrap();
        assert_eq!(
            input.get_resume().unwrap()[0].status,
            ResumeStatus::Resolved
        );
        assert!(
            serde_json::from_str::<RunAgentInput>(
                r#"{"threadId":"t","runId":"r","messages":[],"resume":{"interruptId":"i"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn resume_rejects_non_standard_status() {
        assert!(
            serde_json::from_str::<RunAgentInput>(
                r#"{"threadId":"t","runId":"r","messages":[],"resume":[{"interruptId":"i","status":"approved"}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn optional_standard_fields_reject_null_instead_of_treating_it_as_absent() {
        for field in [
            "protocolVersion",
            "parentRunId",
            "tools",
            "context",
            "resume",
        ] {
            let input = format!(r#"{{"threadId":"t","runId":"r","messages":[],"{field}":null}}"#);
            assert!(
                serde_json::from_str::<RunAgentInput>(&input).is_err(),
                "accepted null field {field}"
            );
        }

        let entry = r#"{"threadId":"t","runId":"r","messages":[],"resume":[{"interruptId":"i","status":"resolved","metadata":null}]}"#;
        assert!(serde_json::from_str::<RunAgentInput>(entry).is_err());
        let entry = r#"{"threadId":"t","runId":"r","messages":[],"resume":[{"interruptId":"i","status":"resolved","payload":null}]}"#;
        assert!(serde_json::from_str::<RunAgentInput>(entry).is_err());
    }
}
