//! Shared response types and error handling for AG-UI HTTP handlers.

use crate::error::AgUiError;
use crate::session::manager::Session;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoResponse {
    pub session_id: String,
    pub run_id: String,
    pub thread_id: String,
    pub state: String,
    pub last_event_id: u64,
    pub created_at: DateTime<Utc>,
    pub hitl_info: Option<HitlInfoResponse>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HitlInfoResponse {
    pub interrupt_id: String,
    pub tool_call_id: String,
    pub pending_tool_calls: Vec<PendingToolCallResponse>,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingToolCallResponse {
    pub call_id: String,
    pub fn_name: String,
    pub fn_arguments: Option<String>,
}

/// Build a `SessionInfoResponse` from an optional `Session`.
/// Returns `None` when no active session exists (normal state).
pub fn build_session_info(session: Option<Session>) -> Option<SessionInfoResponse> {
    session.map(|s| {
        let hitl_info = s.hitl_waiting_info.as_ref().map(|h| HitlInfoResponse {
            interrupt_id: h.interrupt_id.clone(),
            tool_call_id: h.tool_call_id.clone(),
            pending_tool_calls: h
                .pending_tool_calls
                .iter()
                .map(|tc| PendingToolCallResponse {
                    call_id: tc.call_id.clone(),
                    fn_name: tc.fn_name.clone(),
                    fn_arguments: if tc.fn_arguments.is_empty() {
                        None
                    } else {
                        Some(tc.fn_arguments.clone())
                    },
                })
                .collect(),
        });

        SessionInfoResponse {
            session_id: s.session_id,
            run_id: s.run_id.to_string(),
            thread_id: s.thread_id.to_string(),
            state: s.state.to_string(),
            last_event_id: s.last_event_id,
            created_at: s.created_at,
            hitl_info,
        }
    })
}

/// Application error type for axum handlers.
#[derive(Debug)]
pub struct AppError(pub AgUiError);

impl From<AgUiError> for AppError {
    fn from(err: AgUiError) -> Self {
        AppError(err)
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError(AgUiError::Serialization(err))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match &self.0 {
            AgUiError::SessionNotFound(id) => (
                StatusCode::NOT_FOUND,
                "SESSION_NOT_FOUND",
                format!("Session not found: {}", id),
            ),
            AgUiError::SessionExpired(id) => (
                StatusCode::GONE,
                "SESSION_EXPIRED",
                format!("Session expired: {}", id),
            ),
            AgUiError::InvalidInput(msg) => (StatusCode::BAD_REQUEST, "INVALID_INPUT", msg.clone()),
            AgUiError::WorkflowInitFailed(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "WORKFLOW_INIT_FAILED",
                msg.clone(),
            ),
            AgUiError::Cancelled => (
                StatusCode::OK,
                "CANCELLED",
                "Workflow cancelled".to_string(),
            ),
            AgUiError::Timeout { timeout_sec } => (
                StatusCode::GATEWAY_TIMEOUT,
                "TIMEOUT",
                format!("Timeout after {} seconds", timeout_sec),
            ),
            AgUiError::SessionNotPaused { current_state } => (
                StatusCode::CONFLICT,
                "INVALID_SESSION_STATE",
                format!("Session not paused: current state is {}", current_state),
            ),
            AgUiError::InvalidToolCallId { expected, actual } => (
                StatusCode::BAD_REQUEST,
                "INVALID_TOOL_CALL_ID",
                format!(
                    "Invalid tool_call_id: expected {}, got {}",
                    expected, actual
                ),
            ),
            AgUiError::CheckpointNotFound {
                workflow_name,
                position,
            } => (
                StatusCode::NOT_FOUND,
                "CHECKPOINT_NOT_FOUND",
                format!("Checkpoint not found: {} at {}", workflow_name, position),
            ),
            AgUiError::HitlInfoNotFound { session_id } => (
                StatusCode::NOT_FOUND,
                "HITL_INFO_NOT_FOUND",
                format!("HITL waiting info not found for session: {}", session_id),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                format!("{}", self.0),
            ),
        };

        let body = serde_json::json!({
            "error": {
                "code": error_code,
                "message": message
            }
        });

        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_error_response() {
        let err = AppError(AgUiError::SessionNotFound("test-session".to_string()));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_app_error_invalid_input() {
        let err = AppError(AgUiError::InvalidInput("bad request".to_string()));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
