//! Standard AG-UI tool definitions and calls.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parameters: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

impl Tool {
    pub fn new(name: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            parameters: Some(parameters),
            metadata: None,
        }
    }
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: ToolCallType,
    pub function: ToolCallFunction,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCallType {
    #[serde(rename = "function")]
    Function,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: ToolCallType::Function,
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.to_string(),
            },
            encrypted_value: None,
            metadata: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolCallResult {
    pub tool_call_id: String,
    pub result: serde_json::Value,
    #[serde(
        default,
        deserialize_with = "crate::types::deserialize_non_null_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub error: Option<String>,
}

impl ToolCallResult {
    pub fn success(tool_call_id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            result,
            error: None,
        }
    }
    pub fn error(tool_call_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            result: serde_json::Value::Null,
            error: Some(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_tool_allows_missing_parameters_but_requires_description() {
        let tool: Tool =
            serde_json::from_str(r#"{"name":"lookup","description":"Find a record"}"#).unwrap();
        assert!(tool.parameters.is_none());
        assert!(serde_json::from_str::<Tool>(r#"{"name":"lookup","parameters":{}}"#).is_err());
        assert!(
            serde_json::from_str::<Tool>(
                r#"{"name":"lookup","description":"x","parameters":null}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Tool>(r#"{"name":"lookup","description":"x","extra":1}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<Tool>(r#"{"name":"lookup","description":"x","metadata":null}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<ToolCall>(r#"{"id":"c","type":"function","function":{"name":"lookup","arguments":"{}"},"metadata":null}"#).is_err());
    }
}
