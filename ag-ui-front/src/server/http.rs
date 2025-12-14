//! HTTP server implementation for AG-UI.
//!
//! Provides axum-based HTTP SSE server with AG-UI protocol endpoints.

use crate::config::ServerConfig;
use crate::error::AgUiError;
use crate::handler::AgUiHandler;
use crate::server::auth::{auth_middleware, TokenStore};
use crate::session::{EventStore, SessionManager};
use crate::types::RunAgentInput;
use axum::extract::{Path, State};
use axum::http::{header, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use command_utils::util::shutdown::ShutdownLock;
use futures::stream::StreamExt;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Application state for axum handlers.
pub struct AppState<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    pub handler: Arc<AgUiHandler<SM, ES>>,
    pub token_store: Arc<TokenStore>,
    pub config: ServerConfig,
}

impl<SM, ES> Clone for AppState<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    fn clone(&self) -> Self {
        Self {
            handler: self.handler.clone(),
            token_store: self.token_store.clone(),
            config: self.config.clone(),
        }
    }
}

/// Boot the AG-UI HTTP server.
pub async fn boot_ag_ui_server<SM, ES>(
    handler: AgUiHandler<SM, ES>,
    config: ServerConfig,
    lock: ShutdownLock,
    shutdown_signal: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
) -> anyhow::Result<()>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let bind_addr = config.bind_addr.clone();
    let token_store = Arc::new(TokenStore::from_env());

    let app_state = Arc::new(AppState {
        handler: Arc::new(handler),
        token_store: token_store.clone(),
        config,
    });

    // CORS configuration
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        .expose_headers([
            header::HeaderName::from_static("x-ag-ui-run-id"),
            header::HeaderName::from_static("x-ag-ui-session-id"),
        ]);

    // AG-UI routes with authentication
    let ag_ui_routes = Router::new()
        .route("/run", post(run_workflow_handler::<SM, ES>))
        .route("/stream/{run_id}", get(stream_handler::<SM, ES>))
        .route("/message", post(message_handler::<SM, ES>))
        .route("/run/{run_id}", delete(cancel_handler::<SM, ES>))
        .route("/state/{run_id}", get(state_handler::<SM, ES>))
        .layer(axum::middleware::from_fn_with_state(
            token_store,
            auth_middleware,
        ));

    // Main app router
    let app = Router::new()
        .route("/", get(index_handler))
        .nest("/api", Router::new().route("/health", get(health_handler)))
        .nest("/ag-ui", ag_ui_routes)
        .layer(cors)
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("AG-UI HTTP Server started on {}", bind_addr);

    // Graceful shutdown setup
    let shutdown_future: Pin<Box<dyn Future<Output = ()> + Send>> = match shutdown_signal {
        Some(signal) => signal,
        None => {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                match tokio::signal::ctrl_c().await {
                    Ok(()) => {
                        tracing::info!("Shutting down AG-UI server...");
                        let _ = tx.send(());
                    }
                    Err(e) => tracing::error!("Failed to listen for ctrl_c: {:?}", e),
                }
            });
            Box::pin(async move {
                rx.await.ok();
            })
        }
    };

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_future)
        .await;

    // Always unlock regardless of success or error
    lock.unlock();

    result.map_err(Into::into)
}

/// Index handler - basic info page.
async fn index_handler() -> &'static str {
    "AG-UI Front Server - jobworkerp-rs workflow execution via AG-UI protocol"
}

/// Health check endpoint.
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "ag-ui-front"
    }))
}

/// POST /ag-ui/run - Start a new workflow run and return SSE stream.
async fn run_workflow_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Json(input): Json<RunAgentInput>,
) -> Result<impl IntoResponse, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let (session, event_stream) = state.handler.run_workflow(input).await?;

    // Use the stored event IDs from the handler to ensure SSE IDs match persisted IDs
    let sse_stream = event_stream.map(move |(event_id, event)| {
        let event_type = event.event_type();
        let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(
            Event::default()
                .event(event_type)
                .data(data)
                .id(event_id.to_string()),
        )
    });

    let response = Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response();

    // Add custom headers
    let mut response = response;

    // Disable buffering for real-time streaming
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    // Disable nginx buffering
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        header::HeaderValue::from_static("no"),
    );

    if let Ok(run_id_value) = header::HeaderValue::from_str(&session.run_id.to_string()) {
        response.headers_mut().insert(
            header::HeaderName::from_static("x-ag-ui-run-id"),
            run_id_value,
        );
    }
    if let Ok(session_id_value) = header::HeaderValue::from_str(&session.session_id) {
        response.headers_mut().insert(
            header::HeaderName::from_static("x-ag-ui-session-id"),
            session_id_value,
        );
    }

    Ok(response)
}

/// GET /ag-ui/stream/{run_id} - Subscribe to existing run.
///
/// Supports SSE reconnection via Last-Event-ID header. Returns stored events
/// since the given ID, then continues streaming live events if the workflow
/// is still running.
async fn stream_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Path(run_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    // Extract Last-Event-ID header for reconnection support
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let event_stream = state.handler.resume_stream(&run_id, last_event_id).await?;

    // Use the stored event IDs to ensure SSE IDs match persisted IDs for proper reconnection
    let sse_stream = event_stream.map(move |(event_id, event)| {
        let event_type = event.event_type();
        let data = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(
            Event::default()
                .event(event_type)
                .data(data)
                .id(event_id.to_string()),
        )
    });

    let mut response = Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response();

    // Disable buffering for real-time streaming
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        header::HeaderValue::from_static("no"),
    );

    Ok(response)
}

/// HITL message request body
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HitlMessageRequest {
    /// Run ID of the paused workflow
    run_id: String,
    /// Tool call results containing user input
    tool_call_results: Vec<ToolCallResultInput>,
}

/// Single tool call result from client
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolCallResultInput {
    /// Tool call ID (must match the one issued in TOOL_CALL_START)
    tool_call_id: String,
    /// User input result
    result: serde_json::Value,
}

/// POST /ag-ui/message - Send message to running workflow (Human-in-the-Loop).
///
/// Resumes a paused HITL workflow with user input.
/// Returns an SSE stream with resumed workflow events.
async fn message_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Json(body): Json<HitlMessageRequest>,
) -> Result<impl IntoResponse, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let HitlMessageRequest {
        run_id,
        mut tool_call_results,
    } = body;

    // Validate input: exactly one tool_call_result required
    if tool_call_results.len() != 1 {
        return Err(AppError(AgUiError::InvalidInput(format!(
            "tool_call_results must contain exactly 1 item, got {}",
            tool_call_results.len()
        ))));
    }

    // Extract the single tool call result
    let tool_call_result = tool_call_results.remove(0);

    // Resume the workflow with user input
    let event_stream = state
        .handler
        .resume_workflow(
            &run_id,
            &tool_call_result.tool_call_id,
            tool_call_result.result,
        )
        .await?;

    // Convert to SSE stream
    let sse_stream = event_stream.map(move |(event_id, event)| {
        let event_type = event.event_type();
        let data = match serde_json::to_string(&event) {
            Ok(serialized) => serialized,
            Err(e) => {
                tracing::warn!(error = ?e, "Failed to serialize event for SSE");
                "{}".to_string()
            }
        };
        Ok::<_, Infallible>(
            Event::default()
                .event(event_type)
                .data(data)
                .id(event_id.to_string()),
        )
    });

    let mut response = Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response();

    // Disable buffering for real-time streaming
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        header::HeaderValue::from_static("no"),
    );

    Ok(response)
}

/// DELETE /ag-ui/run/{run_id} - Cancel a running workflow.
async fn cancel_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    state.handler.cancel_workflow(&run_id).await?;
    Ok(Json(serde_json::json!({
        "status": "cancelled",
        "runId": run_id
    })))
}

/// GET /ag-ui/state/{run_id} - Get current workflow state.
async fn state_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Path(run_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let workflow_state = state.handler.get_state(&run_id).await?;
    Ok(Json(serde_json::to_value(workflow_state)?))
}

/// Application error type for axum handlers.
#[derive(Debug)]
struct AppError(AgUiError);

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
            // HITL-specific errors
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
