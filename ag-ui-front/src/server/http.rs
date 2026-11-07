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
use axum::extract::{OriginalUri, Path, State};
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
    config.ag_ui.validate()?;
    let bind_addr = config.bind_addr.clone();
    let token_store = Arc::new(TokenStore::from_env());

    let app_state = Arc::new(AppState {
        handler: Arc::new(handler),
        token_store: token_store.clone(),
        config,
    });

    let app = build_app_router(app_state, token_store);

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

fn build_app_router<SM, ES>(
    app_state: Arc<AppState<SM, ES>>,
    token_store: Arc<TokenStore>,
) -> Router
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::ACCEPT])
        .expose_headers([
            header::HeaderName::from_static("x-ag-ui-run-id"),
            header::HeaderName::from_static("x-ag-ui-session-id"),
        ]);

    let mut ag_ui_routes = Router::new()
        .route("/stream/{run_id}", get(stream_handler::<SM, ES>))
        .route("/run/{run_id}", delete(cancel_handler::<SM, ES>))
        .route("/state/{run_id}", get(state_handler::<SM, ES>))
        .route(
            "/sessions/by-thread/{thread_id}",
            get(session_by_thread_handler::<SM, ES>),
        );
    for profile in &app_state.config.ag_ui.profiles {
        let local_path = profile.local_route_path().expect("validated profile path");
        ag_ui_routes = ag_ui_routes.route(local_path, post(run_profile_handler::<SM, ES>));
    }
    let ag_ui_routes = ag_ui_routes.layer(axum::middleware::from_fn_with_state(
        token_store,
        auth_middleware,
    ));

    Router::new()
        .route("/", get(index_handler))
        .nest("/api", Router::new().route("/health", get(health_handler)))
        .nest("/ag-ui", ag_ui_routes)
        .layer(cors)
        .with_state(app_state)
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

async fn run_profile_handler<SM, ES>(
    State(state): State<Arc<AppState<SM, ES>>>,
    OriginalUri(uri): OriginalUri,
    Json(input): Json<RunAgentInput>,
) -> Result<impl IntoResponse, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let profile = crate::config::profile_for_path(&state.config.ag_ui.profiles, uri.path())
        .ok_or_else(|| AppError(AgUiError::InvalidInput("unknown AG-UI profile".into())))?;
    let (session, event_stream) = match profile.mode {
        crate::config::AgUiProfileMode::Workflow => {
            state
                .handler
                .run_workflow_for_worker(input, Some(&profile.worker_name), Some(&profile.path))
                .await
        }
        crate::config::AgUiProfileMode::Streaming => {
            state
                .handler
                .run_direct_profile(input, &profile.worker_name, &profile.path)
                .await
        }
    }?;
    let sse_stream = event_stream.map(|(event_id, event)| {
        Ok::<_, Infallible>(
            Event::default()
                .event(event.event_type())
                .data(serde_json::to_string(&event).unwrap_or_else(|_| "{}".into()))
                .id(event_id.to_string()),
        )
    });
    let mut response = Sse::new(sse_stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("x-accel-buffering"),
        header::HeaderValue::from_static("no"),
    );
    if let Ok(value) = header::HeaderValue::from_str(&session.run_id.to_string()) {
        response
            .headers_mut()
            .insert(header::HeaderName::from_static("x-ag-ui-run-id"), value);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgUiProfile, AgUiProfileMode, profile_for_path};
    use crate::events::{AgUiEvent, RunOutcome};
    use crate::server::test_support::{
        self, add_auth_if_enabled, assert_no_internal_tool_lifecycle, interrupt_id,
    };
    use crate::session::{
        InMemoryEventStore, InMemorySessionManager, InterruptInfo, SessionManager,
    };
    use crate::types::RunAgentInput;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use futures::StreamExt;
    use tower::ServiceExt;

    fn standard_input(run_id: &str, resume: Option<serde_json::Value>) -> RunAgentInput {
        let mut body = serde_json::json!({
            "threadId": "two-stage-thread",
            "runId": run_id,
            "messages": [],
            "state": {"trusted_marker": "client-state"},
        });
        if let Some(resume) = resume {
            body["resume"] = resume;
        }
        serde_json::from_value(body).unwrap()
    }

    fn assert_run_started(events: &[(u64, AgUiEvent)], expected_run_id: &str) {
        assert!(events.iter().any(|(_, event)| matches!(
            event,
            AgUiEvent::RunStarted { run_id, .. } if run_id == expected_run_id
        )));
    }

    fn assert_run_event_order(events: &[(u64, AgUiEvent)], expected_run_id: &str) {
        assert!(matches!(
            events.first().map(|(_, event)| event),
            Some(AgUiEvent::RunStarted { run_id, .. }) if run_id == expected_run_id
        ));
        assert!(matches!(
            events.last().map(|(_, event)| event),
            Some(AgUiEvent::RunFinished { .. }) | Some(AgUiEvent::RunError { .. })
        ));
    }

    async fn profile_probe(
        State(profiles): State<Arc<Vec<AgUiProfile>>>,
        OriginalUri(uri): OriginalUri,
    ) -> (StatusCode, String) {
        profile_for_path(&profiles, uri.path())
            .map(|profile| (StatusCode::OK, profile.worker_name.clone()))
            .unwrap_or((StatusCode::NOT_FOUND, String::new()))
    }

    #[tokio::test]
    async fn configured_literal_profile_route_matches_original_uri() {
        let profiles = Arc::new(vec![AgUiProfile {
            path: "/ag-ui/agents/live/run".to_string(),
            worker_name: "llm-worker".to_string(),
            mode: AgUiProfileMode::Streaming,
        }]);
        let local_path = profiles[0].local_route_path().unwrap();
        let ag_ui = Router::new()
            .route(local_path, post(profile_probe))
            .with_state(profiles);
        let app = Router::new().nest("/ag-ui", ag_ui);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ag-ui/agents/live/run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "llm-worker"
        );

        let missing = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/ag-ui/agents/unknown/run")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn invalid_public_output_releases_the_executor() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_name = format!("ag-ui-private-output-{}", uuid::Uuid::new_v4());
            test_support::create_set_workflow_worker(
                &app_module,
                &worker_name,
                serde_json::json!([{"leak": {"set": {"internal": "not a public output"}}}]),
            )
            .await
            .unwrap();
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None)),
                app_module,
                Arc::new(crate::session::InMemorySessionManager::new(60)),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                Default::default(),
            );
            let run_id = "invalid-public-output-run";
            let (_session, stream) = handler
                .run_workflow_for_worker(
                    standard_input(run_id, None),
                    Some(&worker_name),
                    Some("/ag-ui/agents/private/run"),
                )
                .await
                .unwrap();
            assert!(handler.has_registered_executor(run_id).await);

            let events: Vec<_> = stream.collect().await;

            assert!(events.iter().any(|(_, event)| matches!(
                event,
                AgUiEvent::RunError { code: Some(code), .. } if code == "INVALID_PUBLIC_OUTPUT"
            )));
            assert!(
                !handler.has_registered_executor(run_id).await,
                "a run that failed its public output contract must release its executor"
            );
        });
    }

    #[test]
    fn rejected_direct_run_leaves_no_active_session() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(60));
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None)),
                app_module,
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                Default::default(),
            );
            let thread_id = "direct-rejected-thread";
            for input in test_support::rejected_direct_inputs(thread_id) {
                let run_id = input.run_id.clone();
                assert!(
                    handler
                        .run_direct_profile(input, "missing-direct-worker", "/ag-ui/direct/run")
                        .await
                        .is_err()
                );
                test_support::assert_no_session_left(&session_manager, &run_id, thread_id).await;
            }
        });
    }

    #[test]
    fn cancelling_a_queued_direct_run_settles_its_terminal_event() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(60));
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None)),
                app_module.clone(),
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                Default::default(),
            );
            let run_id = "queued-direct-run";
            let queued = test_support::QueuedDirectRun::create(&app_module, run_id)
                .await
                .unwrap();
            let (session, stream) = handler
                .run_direct_profile(
                    queued.input.clone(),
                    &queued.worker_name,
                    "/ag-ui/direct/run",
                )
                .await
                .unwrap();
            let live = tokio::spawn(test_support::collect_run_events(stream));

            let (first, concurrent) = tokio::join!(
                handler.cancel_workflow(run_id),
                handler.cancel_workflow(run_id)
            );
            first.unwrap();
            concurrent.unwrap();
            // A DELETE after the run settled must not append another terminal event.
            handler.cancel_workflow(run_id).await.unwrap();

            let live = live.await.unwrap();
            let reconnect = test_support::collect_run_events(
                handler.resume_stream(run_id, None).await.unwrap(),
            )
            .await;
            queued
                .assert_cancelled(
                    &app_module,
                    &live,
                    &reconnect,
                    &session_manager,
                    &session.session_id,
                )
                .await
                .unwrap();
        });
    }

    #[test]
    fn registered_profile_reaches_sync_handler_with_standard_input() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let wrapper_module = Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None));
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(60));
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(100, 60));
            let profiles = vec![AgUiProfile {
                path: "/ag-ui/agents/missing/run".to_string(),
                worker_name: "missing-worker".to_string(),
                mode: AgUiProfileMode::Workflow,
            }];
            let config = ServerConfig {
                bind_addr: "127.0.0.1:0".to_string(),
                ag_ui: crate::config::AgUiServerConfig {
                    profiles,
                    ..Default::default()
                },
                auth: crate::config::AuthConfig::default(),
            };
            let handler = AgUiHandler::new(
                wrapper_module.clone(),
                app_module.clone(),
                session_manager.clone(),
                event_store.clone(),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AppState {
                handler: Arc::new(handler),
                token_store: Arc::new(TokenStore::from_env()),
                config: config.clone(),
            });
            let app = build_app_router(state, Arc::new(TokenStore::from_env()));
            let mut request = Request::builder()
                .method(Method::POST)
                .uri("/ag-ui/agents/missing/run")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"threadId":"thread-1","runId":"run-1","messages":[]}"#,
                ))
                .unwrap();
            add_auth_if_enabled(&mut request).unwrap();
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

            let invalid_config = ServerConfig {
                ag_ui: crate::config::AgUiServerConfig {
                    profile_config_error: Some("invalid AG_UI_PROFILES JSON".to_string()),
                    ..Default::default()
                },
                ..config
            };
            let invalid_handler = AgUiHandler::new(
                wrapper_module,
                app_module,
                session_manager,
                event_store,
                invalid_config.ag_ui.clone(),
            );
            let (lock, _wait) = command_utils::util::shutdown::create_lock_and_wait();
            assert!(
                boot_ag_ui_server(invalid_handler, invalid_config, lock, None)
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn workflow_profile_sse_exposes_public_messages_without_guardrail_candidate() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_name = format!("ag-ui-public-output-{}", uuid::Uuid::new_v4());
            let worker_id = test_support::create_public_output_worker(&app_module, &worker_name)
                .await
                .unwrap();
            let path = format!("/ag-ui/agents/{worker_name}/run");
            let config = ServerConfig {
                bind_addr: "127.0.0.1:0".to_string(),
                ag_ui: crate::config::AgUiServerConfig {
                    profiles: vec![AgUiProfile {
                        path: path.clone(),
                        worker_name: worker_name.clone(),
                        mode: AgUiProfileMode::Workflow,
                    }],
                    ..Default::default()
                },
                auth: crate::config::AuthConfig::default(),
            };
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(100, 60));
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None)),
                app_module.clone(),
                Arc::new(crate::session::InMemorySessionManager::new(60)),
                event_store.clone(),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AppState {
                handler: Arc::new(handler),
                token_store: Arc::new(TokenStore::from_env()),
                config,
            });
            let app = build_app_router(state, Arc::new(TokenStore::from_env()));
            let mut request = Request::builder()
                .method(Method::POST)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"threadId":"thread-guardrail","runId":"run-guardrail","messages":[]}"#,
                ))
                .unwrap();
            add_auth_if_enabled(&mut request).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            if response.status() != StatusCode::OK {
                let status = response.status();
                let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
                panic!(
                    "profile request failed with {status}: {}",
                    String::from_utf8_lossy(&body)
                );
            }
            let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
            let sse = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(sse.contains("RUN_STARTED"), "{sse}");
            assert!(sse.contains("TEXT_MESSAGE_CONTENT"), "{sse}");
            assert!(sse.contains("I cannot comply with this request."), "{sse}");
            assert!(sse.contains("RUN_FINISHED"), "{sse}");
            assert!(!sse.contains("PRIVATE_GUARDRAIL_CANDIDATE"), "{sse}");
            assert!(!sse.contains("STEP_STARTED"), "{sse}");
            assert!(!sse.contains("candidate"), "{sse}");

            let stored_events = event_store.get_all_events("run-guardrail").await;
            let stored_json = serde_json::to_string(&stored_events).unwrap();
            assert!(!stored_json.contains("PRIVATE_GUARDRAIL_CANDIDATE"));
            assert!(!stored_json.contains("candidate"));
            assert!(!stored_json.contains("STATE_SNAPSHOT"));
            let mut state_request = Request::builder()
                .method(Method::GET)
                .uri("/ag-ui/state/run-guardrail")
                .body(Body::empty())
                .unwrap();
            add_auth_if_enabled(&mut state_request).unwrap();
            let state_response = app.oneshot(state_request).await.unwrap();
            assert_eq!(state_response.status(), StatusCode::OK);
            assert_eq!(
                to_bytes(state_response.into_body(), 1_000_000)
                    .await
                    .unwrap(),
                "null"
            );

            app_module.worker_app.delete(&worker_id).await.unwrap();
        });
    }

    #[test]
    fn streaming_profile_returns_client_tool_calls_and_accepts_results_on_next_run() {
        test_support::run_server_test(async {
            let tool_response = serde_json::json!({
                "model": "ag-ui-http-test",
                "created_at": "2026-01-01T00:00:00Z",
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "function": {
                            "name": "computer_click",
                            "arguments": {"x": 12}
                        }
                    }]
                },
                "done": true,
                "total_duration": 0,
                "load_duration": 0,
                "prompt_eval_count": 1,
                "prompt_eval_duration": 0,
                "eval_count": 1,
                "eval_duration": 0
            });
            let final_response = serde_json::json!({
                "model": "ag-ui-http-test",
                "created_at": "2026-01-01T00:00:00Z",
                "message": {"role": "assistant", "content": "Done."},
                "done": true,
                "total_duration": 0,
                "load_duration": 0,
                "prompt_eval_count": 1,
                "prompt_eval_duration": 0,
                "eval_count": 1,
                "eval_duration": 0
            });
            let mock = test_support::start_mock_ollama(vec![tool_response, final_response])
                .await
                .unwrap();
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_handle = tests_with_worker::start_test_worker(app_module.clone())
                .await
                .unwrap();
            let worker_name = format!("ag-ui-stream-{}", uuid::Uuid::new_v4());
            let worker_id = test_support::create_streaming_llm_worker(
                &app_module,
                &worker_name,
                &mock.base_url,
            )
            .await
            .unwrap();
            let path = format!("/ag-ui/agents/{worker_name}/run");
            let config = ServerConfig {
                bind_addr: "127.0.0.1:0".to_string(),
                ag_ui: crate::config::AgUiServerConfig {
                    profiles: vec![AgUiProfile {
                        path: path.clone(),
                        worker_name: worker_name.clone(),
                        mode: AgUiProfileMode::Streaming,
                    }],
                    ..Default::default()
                },
                auth: crate::config::AuthConfig::default(),
            };
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(None)),
                app_module.clone(),
                Arc::new(crate::session::InMemorySessionManager::new(60)),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AppState {
                handler: Arc::new(handler),
                token_store: Arc::new(TokenStore::from_env()),
                config,
            });
            let app = build_app_router(state, Arc::new(TokenStore::from_env()));

            let roundtrip = test_support::run_client_tool_roundtrip(app, &path)
                .await
                .unwrap();
            let requests_result =
                tokio::time::timeout(std::time::Duration::from_secs(10), mock.finish()).await;

            app_module.worker_app.delete(&worker_id).await.unwrap();
            worker_handle.shutdown().await;

            assert_eq!(
                roundtrip.first_status,
                StatusCode::OK,
                "{}",
                roundtrip.first_sse
            );
            assert_eq!(
                roundtrip.first_reconnect_status,
                StatusCode::OK,
                "{}",
                roundtrip.first_sse
            );
            assert!(
                roundtrip.first_sse.contains("TOOL_CALL_START"),
                "{}",
                roundtrip.first_sse
            );
            assert!(
                roundtrip.first_sse.contains("TOOL_CALL_ARGS"),
                "{}",
                roundtrip.first_sse
            );
            assert!(
                roundtrip.first_sse.contains("TOOL_CALL_END"),
                "{}",
                roundtrip.first_sse
            );
            assert!(
                roundtrip.first_sse.contains("RUN_FINISHED"),
                "{}",
                roundtrip.first_sse
            );
            assert!(
                !roundtrip.first_sse.contains("TOOL_CALL_RESULT"),
                "{}",
                roundtrip.first_sse
            );
            assert!(
                !roundtrip.first_sse.contains("clicked"),
                "{}",
                roundtrip.first_sse
            );
            assert_eq!(
                roundtrip.second_status,
                StatusCode::OK,
                "{}",
                roundtrip.second_sse
            );
            assert!(
                roundtrip.second_sse.contains("TEXT_MESSAGE_CONTENT"),
                "{}",
                roundtrip.second_sse
            );
            assert!(
                roundtrip.second_sse.contains("Done."),
                "{}",
                roundtrip.second_sse
            );
            assert!(
                roundtrip.second_sse.contains("RUN_FINISHED"),
                "{}",
                roundtrip.second_sse
            );

            let requests = requests_result.unwrap().unwrap();
            assert_eq!(requests.len(), 2);
            assert!(requests[0].contains("computer_click"));
            assert!(
                requests[1].contains("clicked at x=12"),
                "second LLM request did not contain the client tool result: {}",
                requests[1]
            );
        });
    }

    #[test]
    fn registered_handler_resumes_partial_and_multiple_waits_on_one_checkpoint_owner() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_name = format!("chat-guarded-{}", uuid::Uuid::new_v4());
            let worker_id = test_support::create_two_wait_worker(&app_module, &worker_name)
                .await
                .unwrap();
            let profile_path = format!("/ag-ui/agents/{worker_name}/run");
            let session_manager = Arc::new(InMemorySessionManager::new(3600));
            let event_store = Arc::new(InMemoryEventStore::new(1000, 3600));
            let handler = AgUiHandler::new(
                Arc::new(app_wrapper::modules::test::create_test_app_wrapper_module(
                    app_module.clone(),
                )),
                app_module.clone(),
                session_manager.clone(),
                event_store,
                crate::config::AgUiServerConfig::default(),
            );

            let initial_run_id = format!("initial-{}", uuid::Uuid::new_v4());
            let (initial_session, initial_stream) = handler
                .run_workflow_for_worker(
                    standard_input(&initial_run_id, None),
                    Some(&worker_name),
                    Some(&profile_path),
                )
                .await
                .unwrap();
            let initial_events: Vec<_> = initial_stream.collect().await;
            let first_interrupt_id = interrupt_id(&initial_events);
            assert_run_event_order(&initial_events, &initial_run_id);
            assert_no_internal_tool_lifecycle(&initial_events);

            let canonical = session_manager
                .get_session_by_interrupt_id(&first_interrupt_id)
                .await
                .unwrap();
            assert_eq!(canonical.session_id, initial_session.session_id);
            assert_eq!(canonical.run_id.as_str(), initial_run_id);
            let mut hitl_info = canonical.hitl_waiting_info.unwrap();
            assert_eq!(hitl_info.workflow_name, "guardrail-workflow");
            assert_eq!(
                hitl_info.registered_worker_name.as_deref(),
                Some(worker_name.as_str())
            );
            assert_eq!(
                hitl_info.execution_id.as_deref(),
                Some(initial_run_id.as_str())
            );

            let original_hitl_info = hitl_info.clone();
            let failed_worker_run_id = format!("failed-worker-{}", uuid::Uuid::new_v4());
            let failed_worker_interrupt_id = format!("failed-worker-int-{}", uuid::Uuid::new_v4());
            let mut missing_worker_info = original_hitl_info.clone();
            missing_worker_info.registered_worker_name = Some("missing-worker".into());
            missing_worker_info.interrupt_id = failed_worker_interrupt_id.clone();
            for interrupt in &mut missing_worker_info.interrupts {
                interrupt.interrupt_id = failed_worker_interrupt_id.clone();
            }
            let failed_worker_session = session_manager
                .create_session(
                    crate::types::RunId::new(format!("worker-alias-{}", uuid::Uuid::new_v4())),
                    canonical.thread_id.clone(),
                )
                .await;
            assert!(
                session_manager
                    .set_profile_path(&failed_worker_session.session_id, &profile_path)
                    .await
            );
            assert!(
                session_manager
                    .set_paused_with_hitl_info(
                        &failed_worker_session.session_id,
                        missing_worker_info,
                    )
                    .await
            );
            assert!(
                handler
                    .run_workflow_for_worker(
                        standard_input(
                            &failed_worker_run_id,
                            Some(serde_json::json!([{
                                "interruptId": failed_worker_interrupt_id,
                                "status": "resolved",
                                "payload": {"answer": "must-not-consume"}
                            }])),
                        ),
                        Some(&worker_name),
                        Some(&profile_path),
                    )
                    .await
                    .is_err()
            );
            let after_missing_worker = session_manager
                .get_session(&failed_worker_session.session_id)
                .await
                .unwrap();
            assert_eq!(
                after_missing_worker.state,
                crate::session::SessionState::Paused
            );
            assert!(
                after_missing_worker
                    .hitl_waiting_info
                    .unwrap()
                    .interrupts
                    .iter()
                    .all(|entry| entry.status.is_none())
            );
            assert!(
                session_manager
                    .get_session_by_run_id(&crate::types::RunId::new(&failed_worker_run_id))
                    .await
                    .is_none()
            );

            let failed_checkpoint_run_id = format!("failed-checkpoint-{}", uuid::Uuid::new_v4());
            let failed_checkpoint_interrupt_id =
                format!("failed-checkpoint-int-{}", uuid::Uuid::new_v4());
            let mut missing_checkpoint_info = original_hitl_info.clone();
            missing_checkpoint_info.checkpoint_position = "/tasks/missing".into();
            missing_checkpoint_info.interrupt_id = failed_checkpoint_interrupt_id.clone();
            for interrupt in &mut missing_checkpoint_info.interrupts {
                interrupt.interrupt_id = failed_checkpoint_interrupt_id.clone();
            }
            let failed_checkpoint_session = session_manager
                .create_session(
                    crate::types::RunId::new(format!("checkpoint-alias-{}", uuid::Uuid::new_v4())),
                    canonical.thread_id.clone(),
                )
                .await;
            assert!(
                session_manager
                    .set_profile_path(&failed_checkpoint_session.session_id, &profile_path)
                    .await
            );
            assert!(
                session_manager
                    .set_paused_with_hitl_info(
                        &failed_checkpoint_session.session_id,
                        missing_checkpoint_info,
                    )
                    .await
            );
            assert!(
                handler
                    .run_workflow_for_worker(
                        standard_input(
                            &failed_checkpoint_run_id,
                            Some(serde_json::json!([{
                                "interruptId": failed_checkpoint_interrupt_id,
                                "status": "resolved",
                                "payload": {"answer": "must-not-consume"}
                            }])),
                        ),
                        Some(&worker_name),
                        Some(&profile_path),
                    )
                    .await
                    .is_err()
            );
            let after_missing_checkpoint = session_manager
                .get_session(&failed_checkpoint_session.session_id)
                .await
                .unwrap();
            assert_eq!(
                after_missing_checkpoint.state,
                crate::session::SessionState::Paused
            );
            assert!(
                after_missing_checkpoint
                    .hitl_waiting_info
                    .unwrap()
                    .interrupts
                    .iter()
                    .all(|entry| entry.status.is_none())
            );
            assert!(
                session_manager
                    .get_session_by_run_id(&crate::types::RunId::new(&failed_checkpoint_run_id))
                    .await
                    .is_none()
            );

            hitl_info = original_hitl_info;
            let first_batch_interrupt_id = format!("first-batch-{}", uuid::Uuid::new_v4());
            let partial_interrupt_id = format!("partial-{}", uuid::Uuid::new_v4());
            hitl_info.interrupt_id = first_batch_interrupt_id.clone();
            hitl_info.tool_call_id = format!("batch-call-{}", uuid::Uuid::new_v4());
            hitl_info.interrupts = vec![
                InterruptInfo {
                    interrupt_id: first_batch_interrupt_id.clone(),
                    call_id: hitl_info.tool_call_id.clone(),
                    status: None,
                    payload: None,
                },
                InterruptInfo {
                    interrupt_id: partial_interrupt_id.clone(),
                    call_id: "additional-wait-call".into(),
                    status: None,
                    payload: None,
                },
            ];
            let checkpoint_owner_run_id = format!("checkpoint-owner-{}", uuid::Uuid::new_v4());
            let checkpoint_owner = session_manager
                .create_session(
                    crate::types::RunId::new(&checkpoint_owner_run_id),
                    canonical.thread_id.clone(),
                )
                .await;
            assert!(
                session_manager
                    .set_profile_path(&checkpoint_owner.session_id, &profile_path)
                    .await
            );
            assert!(
                session_manager
                    .set_paused_with_hitl_info(&checkpoint_owner.session_id, hitl_info)
                    .await
            );

            let partial_run_id = format!("partial-run-{}", uuid::Uuid::new_v4());
            let (partial_session, partial_stream) = handler
                .run_workflow_for_worker(
                    standard_input(
                        &partial_run_id,
                        Some(serde_json::json!([{
                            "interruptId": first_batch_interrupt_id,
                            "status": "resolved",
                            "payload": {"answer": "first"}
                        }])),
                    ),
                    Some(&worker_name),
                    Some(&profile_path),
                )
                .await
                .unwrap();
            let partial_events: Vec<_> = partial_stream.collect().await;
            assert_run_started(&partial_events, &partial_run_id);
            assert_run_event_order(&partial_events, &partial_run_id);
            assert_eq!(interrupt_id(&partial_events), partial_interrupt_id);
            assert_no_internal_tool_lifecycle(&partial_events);
            assert_eq!(
                session_manager
                    .get_session_by_interrupt_id(&partial_interrupt_id)
                    .await
                    .unwrap()
                    .session_id,
                checkpoint_owner.session_id
            );
            assert_eq!(
                session_manager
                    .get_session(&checkpoint_owner.session_id)
                    .await
                    .unwrap()
                    .state,
                crate::session::SessionState::Paused
            );
            let partial_info = session_manager
                .get_session(&checkpoint_owner.session_id)
                .await
                .unwrap()
                .hitl_waiting_info
                .unwrap();
            assert!(partial_info.interrupts.iter().any(|entry| {
                entry.interrupt_id == first_batch_interrupt_id
                    && entry.status == Some(crate::types::ResumeStatus::Resolved)
                    && entry.payload == Some(serde_json::json!({"answer":"first"}))
            }));
            assert!(partial_info.interrupts.iter().any(|entry| {
                entry.interrupt_id == partial_interrupt_id && entry.status.is_none()
            }));
            assert_eq!(
                session_manager
                    .get_session(&partial_session.session_id)
                    .await
                    .unwrap()
                    .state,
                crate::session::SessionState::Completed
            );

            let second_run_id = format!("second-{}", uuid::Uuid::new_v4());
            let (second_session, second_stream) = handler
                .run_workflow_for_worker(
                    standard_input(
                        &second_run_id,
                        Some(serde_json::json!([{
                            "interruptId": partial_interrupt_id,
                            "status": "resolved",
                            "payload": {"answer": "second"}
                        }])),
                    ),
                    Some(&worker_name),
                    Some(&profile_path),
                )
                .await
                .unwrap();
            let second_events: Vec<_> = second_stream.collect().await;
            assert_run_started(&second_events, &second_run_id);
            assert_run_event_order(&second_events, &second_run_id);
            let final_interrupt_id = interrupt_id(&second_events);
            assert_no_internal_tool_lifecycle(&second_events);
            let second_wait = session_manager
                .get_session_by_interrupt_id(&final_interrupt_id)
                .await
                .unwrap();
            assert_eq!(second_wait.session_id, checkpoint_owner.session_id);
            assert_eq!(second_wait.run_id.as_str(), checkpoint_owner_run_id);
            let second_hitl = second_wait.hitl_waiting_info.unwrap();
            assert_eq!(
                second_hitl.execution_id.as_deref(),
                Some(initial_run_id.as_str())
            );
            assert_eq!(
                session_manager
                    .get_session(&second_session.session_id)
                    .await
                    .unwrap()
                    .state,
                crate::session::SessionState::Completed
            );

            let final_run_id = format!("final-{}", uuid::Uuid::new_v4());
            let (_, final_stream) = handler
                .run_workflow_for_worker(
                    standard_input(
                        &final_run_id,
                        Some(serde_json::json!([{
                            "interruptId": final_interrupt_id,
                            "status": "resolved",
                            "payload": {"answer": "final"}
                        }])),
                    ),
                    Some(&worker_name),
                    Some(&profile_path),
                )
                .await
                .unwrap();
            let final_events: Vec<_> = final_stream.collect().await;
            assert_run_started(&final_events, &final_run_id);
            assert_run_event_order(&final_events, &final_run_id);
            assert!(
                final_events.iter().any(|(_, event)| matches!(
                    event,
                    AgUiEvent::RunFinished {
                        outcome: Some(RunOutcome::Success),
                        ..
                    }
                )),
                "final resume did not complete: {final_events:#?}"
            );
            assert!(
                final_events.iter().any(|(_, event)| matches!(
                    event,
                    AgUiEvent::TextMessageContent { delta, .. }
                        if delta == "all approvals complete for two-stage-thread; server-config"
                )),
                "final output did not preserve the input and server context: {final_events:#?}"
            );
            assert_no_internal_tool_lifecycle(&final_events);
            let completed = session_manager
                .get_session(&checkpoint_owner.session_id)
                .await
                .unwrap();
            assert_eq!(completed.state, crate::session::SessionState::Completed);
            assert!(completed.hitl_waiting_info.is_none());

            app_module.worker_app.delete(&worker_id).await.unwrap();
        });
    }
}
