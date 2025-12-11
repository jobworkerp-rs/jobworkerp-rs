//! Error types for AG-UI Front.

use thiserror::Error;

/// AG-UI error types
#[derive(Error, Debug)]
pub enum AgUiError {
    /// General error
    #[error("AG-UI Error: {message}")]
    General { message: String },

    /// Workflow not found (by run_id)
    #[error("Workflow not found: {run_id}")]
    WorkflowNotFound { run_id: String },

    /// Workflow definition not found (by name)
    #[error("Workflow definition not found: {name}")]
    WorkflowDefinitionNotFound { name: String },

    /// Session not found
    #[error("Session not found: {0}")]
    SessionNotFound(String),

    /// Session expired
    #[error("Session expired: {0}")]
    SessionExpired(String),

    /// Invalid input
    #[error("Invalid input: {0}")]
    InvalidInput(String),

    /// Workflow initialization failed
    #[error("Workflow init failed: {0}")]
    WorkflowInitFailed(String),

    /// Task execution failed
    #[error("Task failed: {task_name} - {message}")]
    TaskFailed { task_name: String, message: String },

    /// Operation timeout
    #[error("Timeout after {timeout_sec} seconds")]
    Timeout { timeout_sec: u32 },

    /// User cancelled the operation
    #[error("Cancelled by user")]
    Cancelled,

    /// Workflow execution error (from workflow::Error)
    #[error("Workflow error [{code}]: {message}")]
    WorkflowError {
        code: String,
        message: String,
        details: Option<String>,
        position: Option<String>,
    },

    /// Internal error
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

impl AgUiError {
    /// Get the error code for AG-UI protocol
    pub fn error_code(&self) -> &'static str {
        match self {
            AgUiError::General { .. } => "GENERAL_ERROR",
            AgUiError::WorkflowNotFound { .. } => "WORKFLOW_NOT_FOUND",
            AgUiError::WorkflowDefinitionNotFound { .. } => "WORKFLOW_NOT_FOUND",
            AgUiError::SessionNotFound(_) => "SESSION_NOT_FOUND",
            AgUiError::SessionExpired(_) => "SESSION_EXPIRED",
            AgUiError::InvalidInput(_) => "INVALID_INPUT",
            AgUiError::WorkflowInitFailed(_) => "WORKFLOW_INIT_FAILED",
            AgUiError::TaskFailed { .. } => "TASK_FAILED",
            AgUiError::Timeout { .. } => "TIMEOUT",
            AgUiError::Cancelled => "CANCELLED",
            AgUiError::WorkflowError { code, .. } => {
                // Return static str based on common codes
                match code.as_str() {
                    "INVALID_INPUT" => "INVALID_INPUT",
                    "UNAUTHORIZED" => "UNAUTHORIZED",
                    "PERMISSION_DENIED" => "PERMISSION_DENIED",
                    "WORKFLOW_NOT_FOUND" => "WORKFLOW_NOT_FOUND",
                    "TIMEOUT" => "TIMEOUT",
                    "SERVICE_UNAVAILABLE" => "SERVICE_UNAVAILABLE",
                    "NOT_IMPLEMENTED" => "NOT_IMPLEMENTED",
                    "TASK_FAILED" => "TASK_FAILED",
                    "CONFLICT" => "CONFLICT",
                    "RATE_LIMITED" => "RATE_LIMITED",
                    "GATEWAY_TIMEOUT" => "GATEWAY_TIMEOUT",
                    _ => "INTERNAL_ERROR",
                }
            }
            AgUiError::Internal(_) => "INTERNAL_ERROR",
            AgUiError::Serialization(_) => "SERIALIZATION_ERROR",
        }
    }

    /// Create error details for RUN_ERROR event
    pub fn to_error_details(&self) -> Option<serde_json::Value> {
        match self {
            AgUiError::WorkflowError {
                details, position, ..
            } => {
                let mut map = serde_json::Map::new();
                if let Some(d) = details {
                    map.insert("detail".to_string(), serde_json::Value::String(d.clone()));
                }
                if let Some(p) = position {
                    map.insert("position".to_string(), serde_json::Value::String(p.clone()));
                }
                if map.is_empty() {
                    None
                } else {
                    Some(serde_json::Value::Object(map))
                }
            }
            AgUiError::TaskFailed { task_name, .. } => Some(serde_json::json!({
                "taskName": task_name
            })),
            AgUiError::Timeout { timeout_sec } => Some(serde_json::json!({
                "timeoutSec": timeout_sec
            })),
            _ => None,
        }
    }

    /// Create a workflow error from workflow::Error components
    pub fn from_workflow_error(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<String>,
        position: Option<String>,
    ) -> Self {
        AgUiError::WorkflowError {
            code: code.into(),
            message: message.into(),
            details,
            position,
        }
    }
}

/// Result type alias for AG-UI operations
pub type Result<T> = std::result::Result<T, AgUiError>;

/// Error code mappings from workflow::ErrorCode to AG-UI error codes
pub mod error_codes {
    /// Map workflow error status code to AG-UI error code
    pub fn from_http_status(status: i64) -> &'static str {
        match status {
            400 => "INVALID_INPUT",
            401 => "UNAUTHORIZED",
            403 => "PERMISSION_DENIED",
            404 => "WORKFLOW_NOT_FOUND",
            405 => "METHOD_NOT_ALLOWED",
            406 => "NOT_ACCEPTABLE",
            408 => "TIMEOUT",
            409 => "CONFLICT",
            410 => "RESOURCE_GONE",
            412 => "PRECONDITION_FAILED",
            415 => "UNSUPPORTED_MEDIA_TYPE",
            422 => "UNPROCESSABLE_ENTITY",
            423 => "TASK_FAILED", // Locked (used for RaiseTask)
            429 => "RATE_LIMITED",
            451 => "UNAVAILABLE_FOR_LEGAL_REASONS",
            500 => "INTERNAL_ERROR",
            501 => "NOT_IMPLEMENTED",
            502 => "BAD_GATEWAY",
            503 => "SERVICE_UNAVAILABLE",
            504 => "GATEWAY_TIMEOUT",
            _ => "INTERNAL_ERROR",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_codes() {
        assert_eq!(
            AgUiError::WorkflowNotFound {
                run_id: "123".to_string()
            }
            .error_code(),
            "WORKFLOW_NOT_FOUND"
        );
        assert_eq!(
            AgUiError::SessionExpired("abc".to_string()).error_code(),
            "SESSION_EXPIRED"
        );
        assert_eq!(AgUiError::Cancelled.error_code(), "CANCELLED");
    }

    #[test]
    fn test_workflow_error() {
        let err = AgUiError::from_workflow_error(
            "TASK_FAILED",
            "HTTP request failed",
            Some("Connection timeout".to_string()),
            Some("/tasks/http_request".to_string()),
        );

        assert_eq!(err.error_code(), "TASK_FAILED");

        let details = err.to_error_details().unwrap();
        assert_eq!(details["detail"], "Connection timeout");
        assert_eq!(details["position"], "/tasks/http_request");
    }

    #[test]
    fn test_task_failed_details() {
        let err = AgUiError::TaskFailed {
            task_name: "http_request".to_string(),
            message: "Connection failed".to_string(),
        };

        let details = err.to_error_details().unwrap();
        assert_eq!(details["taskName"], "http_request");
    }

    #[test]
    fn test_http_status_mapping() {
        assert_eq!(error_codes::from_http_status(400), "INVALID_INPUT");
        assert_eq!(error_codes::from_http_status(404), "WORKFLOW_NOT_FOUND");
        assert_eq!(error_codes::from_http_status(500), "INTERNAL_ERROR");
        assert_eq!(error_codes::from_http_status(503), "SERVICE_UNAVAILABLE");
        assert_eq!(error_codes::from_http_status(999), "INTERNAL_ERROR");
    }

    #[test]
    fn test_error_display() {
        let err = AgUiError::InvalidInput("Missing workflow name".to_string());
        assert_eq!(err.to_string(), "Invalid input: Missing workflow name");
    }
}
