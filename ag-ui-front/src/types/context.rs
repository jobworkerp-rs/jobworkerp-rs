//! Context types for AG-UI protocol.
//!
//! These types define context information passed to workflow execution.

use serde::{Deserialize, Serialize};

/// Context information for workflow execution.
/// Supports AG-UI standard contexts and jobworkerp-rs extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum Context {
    /// Workflow definition information (by name reference)
    #[serde(rename = "workflow_definition")]
    WorkflowDefinition {
        /// Name of the workflow to execute
        workflow_name: String,
        /// Optional version identifier
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        /// Optional namespace
        #[serde(skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
    },

    /// Inline workflow definition (full schema)
    #[serde(rename = "workflow_inline")]
    WorkflowInline {
        /// Full workflow schema as JSON
        workflow: serde_json::Value,
    },

    /// Resume from a checkpoint
    #[serde(rename = "checkpoint_resume")]
    CheckpointResume {
        /// Execution ID to resume from (must match runId)
        execution_id: String,
        /// Position in workflow (JSON Pointer format)
        position: String,
        /// Optional checkpoint data (for restoring state)
        #[serde(skip_serializing_if = "Option::is_none")]
        checkpoint_data: Option<serde_json::Value>,
    },

    /// Custom context for extensions
    #[serde(rename = "custom")]
    Custom {
        /// Context name
        name: String,
        /// Context data
        data: serde_json::Value,
    },
}

impl Context {
    /// Create a workflow definition context (by name reference)
    pub fn workflow(name: impl Into<String>) -> Self {
        Self::WorkflowDefinition {
            workflow_name: name.into(),
            version: None,
            namespace: None,
        }
    }

    /// Create an inline workflow context (full schema)
    pub fn workflow_inline(workflow: serde_json::Value) -> Self {
        Self::WorkflowInline { workflow }
    }

    /// Create a checkpoint resume context
    pub fn checkpoint_resume(execution_id: impl Into<String>, position: impl Into<String>) -> Self {
        Self::CheckpointResume {
            execution_id: execution_id.into(),
            position: position.into(),
            checkpoint_data: None,
        }
    }

    /// Create a checkpoint resume context with data
    pub fn checkpoint_resume_with_data(
        execution_id: impl Into<String>,
        position: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self::CheckpointResume {
            execution_id: execution_id.into(),
            position: position.into(),
            checkpoint_data: Some(data),
        }
    }

    /// Create a custom context
    pub fn custom(name: impl Into<String>, data: serde_json::Value) -> Self {
        Self::Custom {
            name: name.into(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_definition_context() {
        let ctx = Context::workflow("my_workflow");
        let json = serde_json::to_value(&ctx).unwrap();

        assert_eq!(json["type"], "workflow_definition");
        assert_eq!(json["data"]["workflow_name"], "my_workflow");
    }

    #[test]
    fn test_workflow_inline_context() {
        let workflow_schema = serde_json::json!({
            "name": "my_workflow",
            "tasks": [{ "name": "task1", "type": "run" }]
        });
        let ctx = Context::workflow_inline(workflow_schema.clone());
        let json = serde_json::to_value(&ctx).unwrap();

        assert_eq!(json["type"], "workflow_inline");
        assert_eq!(json["data"]["workflow"]["name"], "my_workflow");
    }

    #[test]
    fn test_checkpoint_resume_context() {
        let ctx = Context::checkpoint_resume("exec-123", "/tasks/task1");
        let json = serde_json::to_value(&ctx).unwrap();

        assert_eq!(json["type"], "checkpoint_resume");
        assert_eq!(json["data"]["execution_id"], "exec-123");
        assert_eq!(json["data"]["position"], "/tasks/task1");
    }

    #[test]
    fn test_checkpoint_resume_with_data() {
        let checkpoint_data = serde_json::json!({ "state": "paused", "variables": {} });
        let ctx = Context::checkpoint_resume_with_data("exec-123", "/tasks/task1", checkpoint_data);
        let json = serde_json::to_value(&ctx).unwrap();

        assert_eq!(json["type"], "checkpoint_resume");
        assert_eq!(json["data"]["execution_id"], "exec-123");
        assert!(json["data"]["checkpoint_data"].is_object());
    }

    #[test]
    fn test_custom_context() {
        let ctx = Context::custom(
            "user_preferences",
            serde_json::json!({ "theme": "dark", "language": "ja" }),
        );
        let json = serde_json::to_value(&ctx).unwrap();

        assert_eq!(json["type"], "custom");
        assert_eq!(json["data"]["name"], "user_preferences");
        assert_eq!(json["data"]["data"]["theme"], "dark");
    }

    #[test]
    fn test_context_deserialization() {
        let json = r#"{
            "type": "workflow_definition",
            "data": {
                "workflow_name": "test_workflow",
                "version": "1.0.0"
            }
        }"#;

        let ctx: Context = serde_json::from_str(json).unwrap();
        match ctx {
            Context::WorkflowDefinition {
                workflow_name,
                version,
                ..
            } => {
                assert_eq!(workflow_name, "test_workflow");
                assert_eq!(version, Some("1.0.0".to_string()));
            }
            _ => panic!("Expected WorkflowDefinition"),
        }
    }
}
