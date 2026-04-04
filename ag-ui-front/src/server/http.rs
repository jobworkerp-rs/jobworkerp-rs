//! HTTP server implementation for AG-UI.
//!
//! Provides axum-based HTTP SSE server with AG-UI protocol endpoints.

use crate::config::ServerConfig;
use crate::error::AgUiError;
use crate::handler::AgUiHandler;
use crate::server::auth::{TokenStore, auth_middleware};
use crate::server::types::AppError;
use crate::session::{EventStore, SessionManager};
use crate::types::RunAgentInput;
use crate::types::ids::ThreadId;
use axum::extract::{Path, State};
use axum::http::{Method, header};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
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
        .route(
            "/sessions/by-thread/{thread_id}",
            get(session_by_thread_handler::<SM, ES>),
        )
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
                command_utils::util::shutdown::shutdown_signal().await;
                tracing::info!("Shutting down AG-UI server...");
                let _ = tx.send(());
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

/// POST /ag-ui/message - Send tool call result for LLM HITL.
///
/// Accepts tool call results from the client and returns TOOL_CALL_RESULT/TOOL_CALL_END events.
/// After receiving these events, the client should send a new /ag-ui/run request
/// with the tool results included in the message history to continue the conversation.
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
        tool_call_results,
    } = body;

    // Validate input: at least one tool_call_result required
    if tool_call_results.is_empty() {
        return Err(AppError(AgUiError::InvalidInput(
            "tool_call_results must contain at least 1 item".to_string(),
        )));
    }

    // Process tool call results and emit events
    let event_stream = state
        .handler
        .handle_tool_call_results(
            &run_id,
            tool_call_results
                .into_iter()
                .map(|r| (r.tool_call_id, r.result))
                .collect(),
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

/// GET /ag-ui/sessions/by-thread/{thread_id} - Find active/paused session for a thread.
async fn session_by_thread_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    Path(thread_id): Path<String>,
) -> Result<Json<Option<super::types::SessionInfoResponse>>, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let tid = ThreadId::validated(&thread_id)
        .map_err(|e| AppError(AgUiError::InvalidInput(e.to_string())))?;
    let session = state
        .handler
        .session_manager()
        .get_active_session_by_thread_id(&tid)
        .await;

    let info = super::types::build_session_info(session);
    Ok(Json(info))
}
