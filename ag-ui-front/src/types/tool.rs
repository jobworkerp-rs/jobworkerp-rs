//! Tool types for AG-UI protocol.
//!
//! References AG-UI Rust SDK (ag-ui-core) tool.rs

use serde::{Deserialize, Serialize};

/// Tool definition for frontend-defined tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    /// Tool name (unique identifier)
    pub name: String,

    /// Human-readable description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// JSON Schema for tool parameters
    pub parameters: serde_json::Value,
}

impl Tool {
    /// Create a new tool definition
    pub fn new(name: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            description: None,
            parameters,
        }
    }

    /// Set tool description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// A tool call request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// Unique ID for this tool call
    pub id: String,

    /// Name of the tool to call
    pub name: String,

    /// Arguments to pass to the tool
    pub arguments: serde_json::Value,
}

impl ToolCall {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// Result from a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// ID of the tool call this result is for
    pub tool_call_id: String,

    /// Result value from the tool
    pub result: serde_json::Value,

    /// Optional error message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolCallResult {
    /// Create a successful tool result
    pub fn success(tool_call_id: impl Into<String>, result: serde_json::Value) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            result,
            error: None,
        }
    }

    /// Create an error tool result
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
    fn test_tool_definition() {
        let tool = Tool::new(
            "http_request",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "method": { "type": "string" }
                }
            }),
        )
        .with_description("Make an HTTP request");

        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "http_request");
        assert_eq!(json["description"], "Make an HTTP request");
    }

    #[test]
    fn test_tool_call() {
        let call = ToolCall::new(
            "call_123",
            "http_request",
            serde_json::json!({
                "url": "https://api.example.com",
                "method": "GET"
            }),
        );

        let json = serde_json::to_value(&call).unwrap();
        assert_eq!(json["id"], "call_123");
        assert_eq!(json["name"], "http_request");
    }

    #[test]
    fn test_tool_result_success() {
        let result = ToolCallResult::success(
            "call_123",
            serde_json::json!({ "status": 200, "body": "OK" }),
        );

        assert_eq!(result.tool_call_id, "call_123");
        assert!(result.error.is_none());
    }

    #[test]
    fn test_tool_result_error() {
        let result = ToolCallResult::error("call_123", "Connection timeout");

        assert_eq!(result.tool_call_id, "call_123");
        assert_eq!(result.error, Some("Connection timeout".to_string()));
    }
}
