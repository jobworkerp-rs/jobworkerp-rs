//! HTTP server implementation for AG-UI (async/decoupled architecture).
//!
//! Provides axum-based HTTP SSE server with AG-UI protocol endpoints.
//! Uses job queue for workflow execution instead of direct execution.

use crate::config::ServerConfig;
use crate::error::AgUiError;
use crate::handler_async::AsyncAgUiHandler;
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

/// Application state for axum handlers (async version).
pub struct AsyncAppState<SM, ES>
where
    SM: SessionManager + 'static,
    ES: EventStore + 'static,
{
    pub handler: Arc<AsyncAgUiHandler<SM, ES>>,
    pub token_store: Arc<TokenStore>,
    pub config: ServerConfig,
}

impl<SM, ES> Clone for AsyncAppState<SM, ES>
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

/// Boot the AG-UI HTTP server (async/decoupled version).
pub async fn boot_ag_ui_async_server<SM, ES>(
    handler: AsyncAgUiHandler<SM, ES>,
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

    let app_state = Arc::new(AsyncAppState {
        handler: Arc::new(handler),
        token_store: token_store.clone(),
        config,
    });

    let app = build_app_router(app_state, token_store);

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("AG-UI HTTP Async Server started on {}", bind_addr);

    // Graceful shutdown setup
    let shutdown_future: Pin<Box<dyn Future<Output = ()> + Send>> = match shutdown_signal {
        Some(signal) => signal,
        None => {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            tokio::spawn(async move {
                command_utils::util::shutdown::shutdown_signal().await;
                tracing::info!("Shutting down AG-UI async server...");
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
    app_state: Arc<AsyncAppState<SM, ES>>,
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
    "AG-UI Front Server (Async) - jobworkerp-rs workflow execution via AG-UI protocol (decoupled architecture)"
}

/// Health check endpoint.
async fn health_handler() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "ag-ui-front-async",
        "architecture": "decoupled"
    }))
}

/// POST a configured AG-UI profile path and return its SSE stream.
async fn run_profile_handler<SM, ES>(
    State(state): State<Arc<AsyncAppState<SM, ES>>>,
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
                .clone()
                .run_workflow(input, &profile.worker_name, &profile.path)
                .await
        }
        crate::config::AgUiProfileMode::Streaming => {
            state
                .handler
                .clone()
                .run_direct_profile(input, &profile.worker_name, &profile.path)
                .await
        }
    }?;

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

    let mut response = response;
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
async fn stream_handler<SM, ES>(
    State(state): State<Arc<AsyncAppState<SM, ES>>>,
    Path(run_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, AppError>
where
    SM: SessionManager + Clone + Send + Sync + 'static,
    ES: EventStore + Clone + Send + Sync + 'static,
{
    let last_event_id = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let event_stream = state
        .handler
        .clone()
        .resume_stream(&run_id, last_event_id)
        .await?;

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

    Ok(Sse::new(sse_stream).keep_alive(KeepAlive::default()))
}

/// DELETE /ag-ui/run/{run_id} - Cancel a running workflow.
async fn cancel_handler<SM, ES>(
    State(state): State<Arc<AsyncAppState<SM, ES>>>,
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
    State(state): State<Arc<AsyncAppState<SM, ES>>>,
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
    State(state): State<Arc<AsyncAppState<SM, ES>>>,
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
    use crate::config::{AgUiProfile, AgUiProfileMode, AuthConfig, ServerConfig};
    use crate::events::{AgUiEvent, RunOutcome};
    use crate::server::test_support::{
        self, add_auth_if_enabled, assert_no_internal_tool_lifecycle, interrupt_id,
        test_redis_connection,
    };
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use futures::StreamExt;
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};
    use infra::infra::test_redis_proxy::RedisResponseDropProxy;
    use prost::Message;
    use redis::AsyncCommands;
    use tower::ServiceExt;

    fn standard_run(run_id: &str, resume: Option<serde_json::Value>) -> RunAgentInput {
        let mut body = serde_json::json!({
            "threadId": "async-two-stage-thread",
            "runId": run_id,
            "messages": [],
            "state": {"trusted_marker": "client-state"},
        });
        if let Some(resume) = resume {
            body["resume"] = resume;
        }
        serde_json::from_value(body).unwrap()
    }

    #[test]
    fn rejected_direct_run_leaves_no_active_session() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(60));
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module,
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                Default::default(),
            ));
            let thread_id = "direct-rejected-thread";
            for input in test_support::rejected_direct_inputs(thread_id) {
                let run_id = input.run_id.clone();
                assert!(
                    handler
                        .clone()
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
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                Default::default(),
            ));
            let run_id = "queued-direct-run";
            let queued = test_support::QueuedDirectRun::create(&app_module, run_id)
                .await
                .unwrap();
            let (session, stream) = handler
                .clone()
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
                handler.clone().resume_stream(run_id, None).await.unwrap(),
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
    fn registered_profile_reaches_async_handler_with_standard_input() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(60));
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(100, 60));
            let config = ServerConfig {
                bind_addr: "127.0.0.1:0".to_string(),
                ag_ui: crate::config::AgUiServerConfig {
                    profiles: vec![AgUiProfile {
                        path: "/ag-ui/agents/missing/run".to_string(),
                        worker_name: "missing-worker".to_string(),
                        mode: AgUiProfileMode::Workflow,
                    }],
                    ..Default::default()
                },
                auth: AuthConfig::default(),
            };
            let handler = AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                event_store.clone(),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AsyncAppState {
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
            assert_eq!(
                response.status(),
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            );

            let invalid_config = ServerConfig {
                ag_ui: crate::config::AgUiServerConfig {
                    profile_config_error: Some("invalid AG_UI_PROFILES JSON".to_string()),
                    ..Default::default()
                },
                ..config
            };
            let invalid_handler = AsyncAgUiHandler::new(
                app_module,
                session_manager,
                event_store,
                invalid_config.ag_ui.clone(),
            );
            let (lock, _wait) = command_utils::util::shutdown::create_lock_and_wait();
            assert!(
                boot_ag_ui_async_server(invalid_handler, invalid_config, lock, None)
                    .await
                    .is_err()
            );
        });
    }

    #[test]
    fn async_workflow_profile_publishes_refusal_without_internal_candidate() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_handle = tests_with_worker::start_test_worker(app_module.clone())
                .await
                .unwrap();
            let worker_name = format!("ag-ui-async-public-output-{}", uuid::Uuid::new_v4());
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
                auth: AuthConfig::default(),
            };
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(100, 60));
            let handler = AsyncAgUiHandler::new(
                app_module.clone(),
                Arc::new(crate::session::InMemorySessionManager::new(60)),
                event_store.clone(),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AsyncAppState {
                handler: Arc::new(handler),
                token_store: Arc::new(TokenStore::from_env()),
                config,
            });
            let app = build_app_router(state, Arc::new(TokenStore::from_env()));
            let mut request = Request::builder()
                .method(Method::POST)
                .uri(&path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"threadId":"thread-async-guardrail","runId":"run-async-guardrail","messages":[]}"#,
                ))
                .unwrap();
            add_auth_if_enabled(&mut request).unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let status = response.status();
            let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
            let sse = String::from_utf8_lossy(&body).into_owned();

            let stored_events = event_store.get_all_events("run-async-guardrail").await;
            let stored_json = serde_json::to_string(&stored_events).unwrap();
            let state_response = app
                .oneshot({
                    let mut request = Request::builder()
                        .method(Method::GET)
                        .uri("/ag-ui/state/run-async-guardrail")
                        .body(Body::empty())
                        .unwrap();
                    add_auth_if_enabled(&mut request).unwrap();
                    request
                })
                .await
                .unwrap();
            let state_status = state_response.status();
            let state = to_bytes(state_response.into_body(), 1_000_000)
                .await
                .unwrap();
            let state = String::from_utf8_lossy(&state).into_owned();

            app_module.worker_app.delete(&worker_id).await.unwrap();
            worker_handle.shutdown().await;

            assert_eq!(status, StatusCode::OK, "{sse}");
            assert!(sse.contains("RUN_STARTED"), "{sse}");
            assert!(sse.contains("TEXT_MESSAGE_CONTENT"), "{sse}");
            assert!(sse.contains("I cannot comply with this request."), "{sse}");
            assert!(sse.contains("RUN_FINISHED"), "{sse}");
            assert!(!sse.contains("PRIVATE_GUARDRAIL_CANDIDATE"), "{sse}");
            assert!(!sse.contains("candidate"), "{sse}");
            assert!(!sse.contains("STEP_STARTED"), "{sse}");
            assert!(!stored_json.contains("PRIVATE_GUARDRAIL_CANDIDATE"));
            assert!(!stored_json.contains("candidate"));
            assert!(!stored_json.contains("STATE_SNAPSHOT"));
            assert_eq!(state_status, StatusCode::OK);
            assert_eq!(state, "null");
        });
    }

    #[test]
    fn async_resume_to_a_worker_without_live_response_keeps_the_interrupt_pending() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_handle = tests_with_worker::start_test_worker(app_module.clone())
                .await
                .unwrap();
            let worker_name = format!("ag-ui-async-no-stream-{}", uuid::Uuid::new_v4());
            let worker_id = test_support::create_two_wait_worker(&app_module, &worker_name)
                .await
                .unwrap();
            let profile_path = format!("/ag-ui/agents/{worker_name}/run");
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(3600));
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(1000, 3600)),
                crate::config::AgUiServerConfig::default(),
            ));
            let initial_run_id = format!("async-no-stream-initial-{}", uuid::Uuid::new_v4());
            let (_, initial_stream) = handler
                .clone()
                .run_workflow(
                    standard_run(&initial_run_id, None),
                    &worker_name,
                    &profile_path,
                )
                .await
                .unwrap();
            let initial_events: Vec<_> = initial_stream.collect().await;
            let waiting_interrupt_id = interrupt_id(&initial_events);
            let resume = |run_id: &str| {
                standard_run(
                    run_id,
                    Some(serde_json::json!([{
                        "interruptId": waiting_interrupt_id,
                        "status": "resolved",
                        "payload": {"answer": "first"}
                    }])),
                )
            };

            let mut worker_data = app_module
                .worker_app
                .find(&worker_id)
                .await
                .unwrap()
                .and_then(|worker| worker.data)
                .unwrap();
            let direct_response_type = worker_data.response_type;
            worker_data.response_type = proto::jobworkerp::data::ResponseType::NoResult as i32;
            app_module
                .worker_app
                .update(&worker_id, &Some(worker_data.clone()))
                .await
                .unwrap();

            let rejected_run_id = format!("async-no-stream-rejected-{}", uuid::Uuid::new_v4());
            assert!(
                handler
                    .clone()
                    .run_workflow(resume(&rejected_run_id), &worker_name, &profile_path)
                    .await
                    .is_err()
            );
            assert!(
                session_manager
                    .get_session_by_interrupt_id(&waiting_interrupt_id)
                    .await
                    .is_some(),
                "a resume rejected before acceptance must leave the interrupt pending"
            );
            assert!(
                session_manager
                    .get_session_by_run_id(&crate::types::RunId::new(rejected_run_id.as_str()))
                    .await
                    .is_none()
            );

            worker_data.response_type = direct_response_type;
            app_module
                .worker_app
                .update(&worker_id, &Some(worker_data))
                .await
                .unwrap();
            let retry_run_id = format!("async-no-stream-retry-{}", uuid::Uuid::new_v4());
            let (_, retry_stream) = handler
                .clone()
                .run_workflow(resume(&retry_run_id), &worker_name, &profile_path)
                .await
                .unwrap();
            let retry_events: Vec<_> = retry_stream.collect().await;
            assert!(matches!(
                retry_events.first().map(|(_, event)| event),
                Some(AgUiEvent::RunStarted { run_id, .. }) if run_id == &retry_run_id
            ));
            assert!(
                retry_events
                    .iter()
                    .all(|(_, event)| !matches!(event, AgUiEvent::RunError { .. }))
            );

            app_module.worker_app.delete(&worker_id).await.unwrap();
            worker_handle.shutdown().await;
        });
    }

    #[test]
    #[ignore = "requires a dedicated test Redis configured with TEST_REDIS_URL"]
    fn async_resume_rejected_by_redis_reopens_the_interrupt_for_retry() {
        test_support::run_server_test(async {
            let test_redis_url = std::env::var("TEST_REDIS_URL")
                .expect("TEST_REDIS_URL must point to a dedicated test Redis instance");
            let worker_name = format!("ag-ui-async-rejected-enqueue-{}", uuid::Uuid::new_v4());
            let queue_channel = format!("ag-ui-async-rejected-enqueue-{}", uuid::Uuid::new_v4());
            let (app_module, blocking_client_name) =
                test_support::create_proxied_test_app(&test_redis_url, &queue_channel)
                    .await
                    .unwrap();
            let queue_key = JobqueueAndCodec::queue_channel_name(queue_channel.as_str(), None);
            let worker_handle =
                test_support::start_proxied_test_worker(app_module.clone(), blocking_client_name)
                    .await
                    .unwrap();
            let worker_id = test_support::create_two_wait_worker_with_channel(
                &app_module,
                &worker_name,
                Some(queue_channel),
            )
            .await
            .unwrap();
            let profile_path = format!("/ag-ui/agents/{worker_name}/run");
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(3600));
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                Arc::new(crate::session::InMemoryEventStore::new(1000, 3600)),
                crate::config::AgUiServerConfig::default(),
            ));
            let initial_run_id = format!("async-rejected-initial-{}", uuid::Uuid::new_v4());
            let (_, initial_stream) = bounded_test_stage(
                "enqueue initial interrupting workflow",
                handler.clone().run_workflow(
                    standard_run(&initial_run_id, None),
                    &worker_name,
                    &profile_path,
                ),
            )
            .await
            .unwrap();
            let initial_events: Vec<_> =
                bounded_test_stage("wait for initial interrupt", initial_stream.collect()).await;
            let waiting_interrupt_id = interrupt_id(&initial_events);
            // Stop the worker so it does not poll the queue key while it holds a string.
            bounded_test_stage("stop worker", worker_handle.shutdown())
                .await
                .unwrap();
            let resume = |run_id: &str| {
                standard_run(
                    run_id,
                    Some(serde_json::json!([{
                        "interruptId": waiting_interrupt_id,
                        "status": "resolved",
                        "payload": {"answer": "first"}
                    }])),
                )
            };

            let mut redis = test_redis_connection().await.unwrap();
            redis
                .set::<_, _, ()>(&queue_key, "not a job queue")
                .await
                .unwrap();
            let rejected_run_id = format!("async-rejected-resume-{}", uuid::Uuid::new_v4());
            let rejected = bounded_test_stage(
                "resume against a WRONGTYPE queue",
                handler
                    .clone()
                    .run_workflow(resume(&rejected_run_id), &worker_name, &profile_path),
            )
            .await;
            let pending_after_rejection = session_manager
                .get_session_by_interrupt_id(&waiting_interrupt_id)
                .await;
            let rejected_session = session_manager
                .get_session_by_run_id(&crate::types::RunId::new(rejected_run_id.as_str()))
                .await;
            redis.del::<_, ()>(&queue_key).await.unwrap();

            let retry_run_id = format!("async-rejected-retry-{}", uuid::Uuid::new_v4());
            let retried = bounded_test_stage(
                "retry the same answer after repairing the queue",
                handler
                    .clone()
                    .run_workflow(resume(&retry_run_id), &worker_name, &profile_path),
            )
            .await;
            let queued_after_retry = queued_job_ids(&queue_key).await;
            // Dropping the accepted stream releases its result subscription before cleanup.
            let retry_error = retried.err();
            redis.del::<_, ()>(&queue_key).await.unwrap();
            app_module.worker_app.delete(&worker_id).await.unwrap();

            let error = rejected
                .err()
                .expect("an explicitly rejected queue write must fail the resume");
            assert!(format!("{error:?}").contains("WRONGTYPE"), "{error:?}");
            assert!(
                pending_after_rejection.is_some(),
                "a write Redis refused must leave the interrupt resumable"
            );
            assert!(rejected_session.is_none());
            assert!(
                retry_error.is_none(),
                "retry must be accepted: {retry_error:?}"
            );
            assert_eq!(queued_after_retry.len(), 1);
        });
    }

    #[test]
    #[ignore = "requires a dedicated test Redis configured with TEST_REDIS_URL"]
    fn async_resume_after_redis_accepts_job_but_drops_reply_is_not_retried() {
        test_support::run_server_test(async {
            let test_redis_url = std::env::var("TEST_REDIS_URL")
                .expect("TEST_REDIS_URL must point to a dedicated test Redis instance");
            let proxy = bounded_test_stage(
                "start Redis response-drop proxy",
                RedisResponseDropProxy::start(&test_redis_url),
            )
            .await
            .unwrap();
            let worker_name = format!("ag-ui-async-unknown-enqueue-{}", uuid::Uuid::new_v4());
            let queue_channel = format!("ag-ui-async-unknown-enqueue-{}", uuid::Uuid::new_v4());
            let (app_module, blocking_client_name) = bounded_test_stage(
                "create app with proxied normal Redis pool and dedicated channel",
                test_support::create_proxied_test_app(proxy.url(), &queue_channel),
            )
            .await
            .unwrap();

            let queue_key = JobqueueAndCodec::queue_channel_name(queue_channel.as_str(), None);
            proxy.observe_queue(&queue_key);
            let worker_handle = bounded_test_stage(
                "start proxied worker",
                test_support::start_proxied_test_worker(app_module.clone(), blocking_client_name),
            )
            .await
            .unwrap();
            let worker_id = bounded_test_stage(
                "register two-stage workflow worker",
                test_support::create_two_wait_worker_with_channel(
                    &app_module,
                    &worker_name,
                    Some(queue_channel),
                ),
            )
            .await
            .unwrap();
            let profile_path = format!("/ag-ui/agents/{worker_name}/run");
            let initial_run_id = format!("async-unknown-initial-{}", uuid::Uuid::new_v4());
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(3600));
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(1000, 3600));
            let config = ServerConfig {
                bind_addr: "127.0.0.1:0".to_string(),
                ag_ui: crate::config::AgUiServerConfig {
                    profiles: vec![AgUiProfile {
                        path: profile_path.clone(),
                        worker_name: worker_name.clone(),
                        mode: AgUiProfileMode::Workflow,
                    }],
                    ..Default::default()
                },
                auth: AuthConfig::default(),
            };
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                event_store.clone(),
                config.ag_ui.clone(),
            ));
            let state = Arc::new(AsyncAppState {
                handler: handler.clone(),
                token_store: Arc::new(TokenStore::from_env()),
                config,
            });
            let app = build_app_router(state, Arc::new(TokenStore::from_env()));

            let (initial_session, initial_stream) = bounded_test_stage(
                "enqueue initial interrupting workflow",
                handler.clone().run_workflow(
                    standard_run(&initial_run_id, None),
                    &worker_name,
                    &profile_path,
                ),
            )
            .await
            .unwrap();
            let initial_events: Vec<_> = bounded_test_stage(
                "wait for initial workflow interrupt",
                initial_stream.collect(),
            )
            .await;
            let waiting_interrupt_id = interrupt_id(&initial_events);
            bounded_test_stage(
                "stop worker before resume enqueue",
                worker_handle.shutdown(),
            )
            .await
            .unwrap();

            let queued_before = queued_job_ids(&queue_key).await;
            proxy.drop_next_queue_append_response();

            let unknown_run_id = format!("async-unknown-resume-{}", uuid::Uuid::new_v4());
            let first_response = bounded_test_stage(
                "return first resume POST after response loss",
                app.clone().oneshot(resume_request(
                    &profile_path,
                    &unknown_run_id,
                    &waiting_interrupt_id,
                )),
            )
            .await
            .unwrap();
            let first_status = first_response.status();
            let first_body = to_bytes(first_response.into_body(), 1_000_000)
                .await
                .unwrap();
            let first_sse = String::from_utf8_lossy(&first_body).into_owned();
            let queued_after_unknown = queued_job_ids(&queue_key).await;
            let unknown_job_id = queued_after_unknown
                .first()
                .expect("the resume job whose reply was lost stays queued")
                .to_string();

            let canonical_session = session_manager
                .get_session_by_run_id(&initial_session.run_id)
                .await;
            let active_thread_session = session_manager
                .get_active_session_by_thread_id(&initial_session.thread_id)
                .await;
            let pending_session = session_manager
                .get_session_by_interrupt_id(&waiting_interrupt_id)
                .await;
            let response_session = session_manager
                .get_session_by_run_id(&crate::types::RunId::new(&unknown_run_id))
                .await;
            let stored_events = event_store.get_all_events(&unknown_run_id).await;
            let reconnect_response = app
                .clone()
                .oneshot({
                    let mut request = Request::builder()
                        .method(Method::GET)
                        .uri(format!("/ag-ui/stream/{unknown_run_id}"))
                        .body(Body::empty())
                        .unwrap();
                    add_auth_if_enabled(&mut request).unwrap();
                    request
                })
                .await
                .unwrap();
            let reconnect_status = reconnect_response.status();
            let reconnect_body = to_bytes(reconnect_response.into_body(), 1_000_000)
                .await
                .unwrap();
            let reconnect_sse = String::from_utf8_lossy(&reconnect_body).into_owned();

            let retry_run_id = format!("async-unknown-retry-{}", uuid::Uuid::new_v4());
            let retry_response = bounded_test_stage(
                "reject retry for consumed interrupt",
                app.oneshot(resume_request(
                    &profile_path,
                    &retry_run_id,
                    &waiting_interrupt_id,
                )),
            )
            .await
            .unwrap();
            let retry_status = retry_response.status();
            let mut retry_stream = retry_response.into_body().into_data_stream();
            let retry_first_chunk =
                tokio::time::timeout(std::time::Duration::from_secs(5), retry_stream.next())
                    .await
                    .expect("retry response did not produce its first body chunk")
                    .and_then(Result::ok)
                    .map(|chunk| String::from_utf8_lossy(&chunk).into_owned())
                    .unwrap_or_default();
            drop(retry_stream);
            let retry_run_session = session_manager
                .get_session_by_run_id(&crate::types::RunId::new(&retry_run_id))
                .await;
            let queued_after_retry = queued_job_ids(&queue_key).await;

            test_redis_connection()
                .await
                .unwrap()
                .del::<_, ()>(&queue_key)
                .await
                .unwrap();
            app_module.worker_app.delete(&worker_id).await.unwrap();

            let stored_started_then_error = matches!(
                stored_events.as_slice(),
                [
                    (_, AgUiEvent::RunStarted { run_id, .. }),
                    (_, AgUiEvent::RunError {
                        code: Some(code),
                        ..
                    })
                ] if run_id == &unknown_run_id && code == "ENQUEUE_OUTCOME_UNKNOWN"
            );

            assert!(queued_before.is_empty());
            assert_eq!(queued_after_unknown.len(), 1);
            assert_eq!(
                queued_after_retry, queued_after_unknown,
                "retry must not append another job"
            );
            assert_eq!(first_status, StatusCode::OK, "{first_sse}");
            assert!(first_sse.contains("RUN_STARTED"), "{first_sse}");
            assert!(first_sse.contains("RUN_ERROR"), "{first_sse}");
            assert!(first_sse.contains("ENQUEUE_OUTCOME_UNKNOWN"), "{first_sse}");
            assert!(!first_sse.contains(&unknown_job_id), "{first_sse}");
            assert!(
                canonical_session.is_some_and(|session| {
                    session.session_id == initial_session.session_id
                        && session.state == crate::session::SessionState::Error
                        && session.hitl_waiting_info.is_none()
                }),
                "the canonical checkpoint session must remain stored in error state"
            );
            assert!(active_thread_session.is_none());
            assert!(
                pending_session.is_none(),
                "the accepted resume consumes its interrupt"
            );
            assert!(
                response_session.is_some_and(|session| {
                    session.state == crate::session::SessionState::Error
                })
            );
            assert!(stored_started_then_error);
            assert_eq!(reconnect_status, StatusCode::OK, "{reconnect_sse}");
            assert!(reconnect_sse.contains("RUN_STARTED"), "{reconnect_sse}");
            assert!(reconnect_sse.contains("RUN_ERROR"), "{reconnect_sse}");
            assert!(
                reconnect_sse.contains("ENQUEUE_OUTCOME_UNKNOWN"),
                "{reconnect_sse}"
            );
            assert!(!retry_status.is_success(), "{retry_first_chunk}");
            assert!(
                !retry_first_chunk.contains("RUN_STARTED"),
                "{retry_first_chunk}"
            );
            assert!(retry_run_session.is_none());
        });
    }

    fn resume_request(profile_path: &str, run_id: &str, interrupt_id: &str) -> Request<Body> {
        let input = standard_run(
            run_id,
            Some(serde_json::json!([{
                "interruptId": interrupt_id,
                "status": "resolved",
                "payload": {"answer": "confirmed"}
            }])),
        );
        let mut request = Request::builder()
            .method(Method::POST)
            .uri(profile_path)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&input).unwrap()))
            .unwrap();
        add_auth_if_enabled(&mut request).unwrap();
        request
    }

    /// IDs of the jobs waiting in `queue_key`, read directly from Redis.
    async fn queued_job_ids(queue_key: &str) -> Vec<i64> {
        let payloads: Vec<Vec<u8>> = test_redis_connection()
            .await
            .unwrap()
            .lrange(queue_key, 0, -1)
            .await
            .unwrap();
        payloads
            .iter()
            .map(|payload| {
                proto::jobworkerp::data::JobInternal::decode(payload.as_slice())
                    .unwrap()
                    .job
                    .and_then(|job| job.id)
                    .expect("queued job has an ID")
                    .value
            })
            .collect()
    }

    async fn bounded_test_stage<T>(
        stage: &'static str,
        future: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(15), future)
            .await
            .unwrap_or_else(|_| panic!("timed out during test stage: {stage}"))
    }

    #[test]
    fn async_resume_preserves_checkpoint_input_context_and_internal_execution_id() {
        test_support::run_server_test(async {
            let app_module = test_support::create_test_app().await.unwrap();
            let worker_handle = tests_with_worker::start_test_worker(app_module.clone())
                .await
                .unwrap();
            let worker_name = format!("ag-ui-async-two-wait-{}", uuid::Uuid::new_v4());
            let worker_id = test_support::create_two_wait_worker(&app_module, &worker_name)
                .await
                .unwrap();
            let profile_path = format!("/ag-ui/agents/{worker_name}/run");
            let session_manager = Arc::new(crate::session::InMemorySessionManager::new(3600));
            let event_store = Arc::new(crate::session::InMemoryEventStore::new(1000, 3600));
            let handler = Arc::new(AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager.clone(),
                event_store,
                crate::config::AgUiServerConfig::default(),
            ));

            let initial_run_id = format!("async-initial-{}", uuid::Uuid::new_v4());
            let (initial_session, initial_stream) = handler
                .clone()
                .run_workflow(
                    standard_run(&initial_run_id, None),
                    &worker_name,
                    &profile_path,
                )
                .await
                .unwrap();
            let initial_events: Vec<_> = initial_stream.collect().await;
            let first_interrupt_id = interrupt_id(&initial_events);
            assert!(matches!(
                initial_events.first().map(|(_, event)| event),
                Some(AgUiEvent::RunStarted { run_id, .. }) if run_id == &initial_run_id
            ));
            assert_no_internal_tool_lifecycle(&initial_events);
            let initial_finished = initial_events
                .iter()
                .filter(|(_, event)| matches!(event, AgUiEvent::RunFinished { .. }))
                .count();
            assert_eq!(initial_finished, 1);

            let canonical = session_manager
                .get_session_by_interrupt_id(&first_interrupt_id)
                .await
                .unwrap();
            assert_eq!(canonical.session_id, initial_session.session_id);
            let initial_hitl = canonical.hitl_waiting_info.unwrap();
            assert_eq!(
                initial_hitl.execution_id.as_deref(),
                Some(initial_run_id.as_str())
            );
            assert_eq!(initial_hitl.workflow_name, "guardrail-workflow");
            assert_eq!(initial_hitl.checkpoint_position, "/ROOT/do/1/second_wait");

            let second_run_id = format!("async-second-{}", uuid::Uuid::new_v4());
            let (second_session, second_stream) = handler
                .clone()
                .run_workflow(
                    standard_run(
                        &second_run_id,
                        Some(serde_json::json!([{
                            "interruptId": first_interrupt_id,
                            "status": "resolved",
                            "payload": {"answer": "first"}
                        }])),
                    ),
                    &worker_name,
                    &profile_path,
                )
                .await
                .unwrap();
            let second_events: Vec<_> = second_stream.collect().await;
            let second_interrupt_id = interrupt_id(&second_events);
            assert!(matches!(
                second_events.first().map(|(_, event)| event),
                Some(AgUiEvent::RunStarted { run_id, .. }) if run_id == &second_run_id
            ));
            assert_no_internal_tool_lifecycle(&second_events);
            assert_eq!(
                second_events
                    .iter()
                    .filter(|(_, event)| matches!(event, AgUiEvent::RunFinished { .. }))
                    .count(),
                1
            );
            assert_eq!(
                session_manager
                    .get_session(&second_session.session_id)
                    .await
                    .unwrap()
                    .state,
                crate::session::SessionState::Completed
            );
            let second_wait = session_manager
                .get_session_by_interrupt_id(&second_interrupt_id)
                .await
                .unwrap();
            assert_eq!(second_wait.session_id, canonical.session_id);
            assert_eq!(second_wait.run_id.as_str(), initial_run_id);
            let second_hitl = second_wait.hitl_waiting_info.unwrap();
            assert_eq!(
                second_hitl.execution_id.as_deref(),
                Some(initial_run_id.as_str())
            );
            assert_eq!(second_hitl.checkpoint_position, "/ROOT/do/2/publish");

            let final_run_id = format!("async-final-{}", uuid::Uuid::new_v4());
            let (_, final_stream) = handler
                .clone()
                .run_workflow(
                    standard_run(
                        &final_run_id,
                        Some(serde_json::json!([{
                            "interruptId": second_interrupt_id,
                            "status": "resolved",
                            "payload": {"answer": "second"}
                        }])),
                    ),
                    &worker_name,
                    &profile_path,
                )
                .await
                .unwrap();
            let final_events: Vec<_> = final_stream.collect().await;
            assert!(matches!(
                final_events.first().map(|(_, event)| event),
                Some(AgUiEvent::RunStarted { run_id, .. }) if run_id == &final_run_id
            ));
            assert!(final_events.iter().any(|(_, event)| matches!(
                event,
                AgUiEvent::TextMessageContent { delta, .. }
                    if delta == "all approvals complete for async-two-stage-thread; server-config"
            )));
            assert_no_internal_tool_lifecycle(&final_events);
            assert!(matches!(
                final_events.last().map(|(_, event)| event),
                Some(AgUiEvent::RunFinished {
                    outcome: Some(RunOutcome::Success),
                    ..
                })
            ));
            let completed = session_manager
                .get_session(&canonical.session_id)
                .await
                .unwrap();
            assert_eq!(completed.state, crate::session::SessionState::Completed);
            assert!(completed.hitl_waiting_info.is_none());

            app_module.worker_app.delete(&worker_id).await.unwrap();
            worker_handle.shutdown().await;
        });
    }

    #[test]
    fn async_streaming_profile_returns_client_tool_calls_and_accepts_next_run_results() {
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
            let worker_name = format!("ag-ui-async-stream-{}", uuid::Uuid::new_v4());
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
                        worker_name,
                        mode: AgUiProfileMode::Streaming,
                    }],
                    ..Default::default()
                },
                auth: AuthConfig::default(),
            };
            let handler = AsyncAgUiHandler::new(
                app_module.clone(),
                Arc::new(crate::session::InMemorySessionManager::new(60)),
                Arc::new(crate::session::InMemoryEventStore::new(100, 60)),
                config.ag_ui.clone(),
            );
            let state = Arc::new(AsyncAppState {
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
}
