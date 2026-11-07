use crate::events::{AgUiEvent, RunOutcome};
use anyhow::{Context, Result};
use app::module::AppModule;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::WorkflowRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
    OllamaRunnerSettings, Settings,
};
use jobworkerp_runner::jobworkerp::runner::workflow_runner_settings::WorkflowSource;
use prost::Message;
use proto::jobworkerp::data::{ResponseType, RunnerId, WorkerData, WorkerId};
use std::future::Future;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tower::ServiceExt;

pub async fn create_test_app() -> Result<Arc<AppModule>> {
    Ok(Arc::new(app::module::test::create_hybrid_test_app().await?))
}

/// Test app whose normal Redis pool goes through `proxy_url`, with a dedicated worker
/// channel. Returns the name of its blocking Redis clients, which
/// `start_proxied_test_worker` releases on shutdown.
pub async fn create_proxied_test_app(
    proxy_url: &str,
    worker_channel: &str,
) -> Result<(Arc<AppModule>, String)> {
    let client_name_prefix = format!("ag-ui-test-{}", uuid::Uuid::new_v4());
    let app_module = app::module::test::create_hybrid_test_app_with_redis_proxy_and_channel(
        &client_name_prefix,
        proxy_url,
        worker_channel,
    )
    .await?;
    let blocking_client_name =
        infra::infra::module::redis::test::proxied_blocking_client_name(&client_name_prefix);
    Ok((Arc::new(app_module), blocking_client_name))
}

pub fn run_server_test(future: impl Future<Output = ()>) {
    infra_utils::infra::test::TEST_RUNTIME.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(60), future)
            .await
            .expect("server fixture test timed out");
    });
}

/// Worker for a proxied test app. Its BLPOP connections belong to the app's own
/// blocking pool, so shutdown also releases them by name; otherwise they would pop
/// jobs that a test enqueues after stopping the worker.
pub struct ProxiedWorkerHandle {
    worker: tests_with_worker::TestWorkerHandle,
    blocking_client_name: String,
}

impl ProxiedWorkerHandle {
    pub async fn shutdown(self) -> Result<()> {
        self.worker.shutdown().await;
        let redis_client = infra_utils::infra::test::setup_test_redis_client()?;
        infra_utils::infra::redis::kill_blocked_clients(&redis_client, &self.blocking_client_name)
            .await?;
        Ok(())
    }
}

pub async fn start_proxied_test_worker(
    app_module: Arc<AppModule>,
    blocking_client_name: String,
) -> Result<ProxiedWorkerHandle> {
    Ok(ProxiedWorkerHandle {
        worker: tests_with_worker::start_test_worker(app_module).await?,
        blocking_client_name,
    })
}

pub(super) async fn test_redis_connection() -> Result<redis::aio::MultiplexedConnection> {
    Ok(infra_utils::infra::test::setup_test_redis_client()?
        .get_multiplexed_async_connection()
        .await?)
}

pub struct MockOllama {
    pub base_url: String,
    task: Option<JoinHandle<Result<Vec<String>>>>,
}

impl MockOllama {
    pub async fn finish(mut self) -> Result<Vec<String>> {
        let task = self
            .task
            .take()
            .context("mock Ollama task already consumed")?;
        task.await?
    }
}

impl Drop for MockOllama {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start_mock_ollama(responses: Vec<serde_json::Value>) -> Result<MockOllama> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        let mut requests = Vec::with_capacity(responses.len());
        for response_body in responses {
            let (mut socket, _) = listener.accept().await?;
            requests.push(read_request(&mut socket).await?);
            let body = format!("{}\n", response_body);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await?;
        }
        Ok(requests)
    });
    Ok(MockOllama {
        base_url,
        task: Some(task),
    })
}

pub struct ClientToolRoundTrip {
    pub first_status: StatusCode,
    pub first_reconnect_status: StatusCode,
    pub first_sse: String,
    pub second_status: StatusCode,
    pub second_sse: String,
}

pub async fn run_client_tool_roundtrip(app: Router, path: &str) -> Result<ClientToolRoundTrip> {
    let tools = serde_json::json!([{
        "name": "computer_click",
        "description": "Click at a screen coordinate",
        "parameters": {
            "type": "object",
            "properties": {"x": {"type": "number"}},
            "required": ["x"]
        }
    }]);
    let first = serde_json::json!({
        "threadId": "thread-client-tool-roundtrip",
        "runId": "run-client-tool-1",
        "messages": [{"id": "user-1", "role": "user", "content": "Click at x=12"}],
        "tools": tools
    });
    let response = app
        .clone()
        .oneshot(client_tool_request(path, first)?)
        .await
        .context("first client tool profile request failed")?;
    let first_status = response.status();
    let mut post_stream = response.into_body().into_data_stream();
    let first_chunk = tokio::time::timeout(std::time::Duration::from_secs(5), post_stream.next())
        .await
        .context("timed out waiting for the initial POST SSE event")?
        .context("initial POST SSE stream closed before sending an event")??;
    let first_chunk = String::from_utf8_lossy(&first_chunk).into_owned();
    let last_event_id = first_chunk
        .lines()
        .filter_map(|line| line.strip_prefix("id: "))
        .filter_map(|value| value.parse::<u64>().ok())
        .max()
        .context("initial POST SSE event did not include an event ID")?;
    drop(post_stream);

    let mut reconnect_request = Request::builder()
        .method(Method::GET)
        .uri("/ag-ui/stream/run-client-tool-1")
        .header("last-event-id", last_event_id.to_string())
        .body(Body::empty())?;
    add_auth_if_enabled(&mut reconnect_request)?;
    let reconnect_response = app
        .clone()
        .oneshot(reconnect_request)
        .await
        .context("client tool SSE reconnect request failed")?;
    let first_reconnect_status = reconnect_response.status();
    let resumed_events = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        to_bytes(reconnect_response.into_body(), 1_000_000),
    )
    .await
    .context("timed out waiting for the reconnected run to finish")??;
    let first_sse = format!("{first_chunk}{}", String::from_utf8_lossy(&resumed_events));

    let second = serde_json::json!({
        "threadId": "thread-client-tool-roundtrip",
        "runId": "run-client-tool-2",
        "messages": [
            {"id": "user-1", "role": "user", "content": "Click at x=12"},
            {
                "id": "assistant-tool-call",
                "role": "assistant",
                "toolCalls": [{
                    "id": "call-computer-click",
                    "type": "function",
                    "function": {
                        "name": "computer_click",
                        "arguments": "{\"x\":12}"
                    }
                }]
            },
            {
                "id": "tool-result",
                "role": "tool",
                "toolCallId": "call-computer-click",
                "content": "clicked at x=12"
            }
        ],
        "tools": tools
    });
    let response = app
        .oneshot(client_tool_request(path, second)?)
        .await
        .context("second client tool profile request failed")?;
    let second_status = response.status();
    let second_sse = String::from_utf8_lossy(
        &to_bytes(response.into_body(), 1_000_000)
            .await
            .context("failed reading second client tool SSE response")?,
    )
    .into_owned();

    Ok(ClientToolRoundTrip {
        first_status,
        first_reconnect_status,
        first_sse,
        second_status,
        second_sse,
    })
}

fn client_tool_request(path: &str, body: serde_json::Value) -> Result<Request<Body>> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body)?))?;
    add_auth_if_enabled(&mut request)?;
    Ok(request)
}

pub(super) fn add_auth_if_enabled(request: &mut Request<Body>) -> Result<()> {
    if std::env::var("AG_UI_AUTH_ENABLED")
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(false)
    {
        let token = std::env::var("AG_UI_AUTH_TOKENS")
            .ok()
            .and_then(|value| value.split(',').next().map(str::trim).map(str::to_string))
            .unwrap_or_else(|| "demo-token".to_string());
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, format!("Bearer {token}").parse()?);
    }
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = socket.read(&mut chunk).await?;
        anyhow::ensure!(read > 0, "client closed before sending request");
        request.extend_from_slice(&chunk[..read]);
        if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if request.len() >= header_end + 4 + content_length {
                return Ok(String::from_utf8_lossy(&request).into_owned());
            }
        }
        anyhow::ensure!(
            request.len() <= 1_048_576,
            "mock request exceeded size limit"
        );
    }
}

pub async fn create_public_output_worker(
    app_module: &Arc<AppModule>,
    worker_name: &str,
) -> Result<WorkerId> {
    create_set_workflow_worker(
        app_module,
        worker_name,
        serde_json::json!([
            {"guardrail_candidate": {"set": {"candidate": "PRIVATE_GUARDRAIL_CANDIDATE"}}},
            {"publish": {"set": {"messages": [
                {"id": "assistant-message", "role": "assistant", "content": "I cannot comply with this request."}
            ]}}}
        ]),
    )
    .await
}

/// Register a WORKFLOW worker running the given `do` task list.
pub async fn create_set_workflow_worker(
    app_module: &Arc<AppModule>,
    worker_name: &str,
    tasks: serde_json::Value,
) -> Result<WorkerId> {
    let runner = app_module
        .runner_app
        .find_runner_by_name("WORKFLOW")
        .await?
        .context("WORKFLOW runner is unavailable in test app")?;
    let runner_id = runner.id.context("WORKFLOW runner has no ID")?;
    let workflow = serde_json::json!({
        "document": {
            "dsl": "1.0.0",
            "namespace": "ag-ui-test",
            "name": worker_name,
            "version": "1.0.0"
        },
        "do": tasks
    });
    let settings = WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(workflow.to_string())),
        workflow_context: None,
    };
    let worker = WorkerData {
        name: worker_name.to_string(),
        description: "AG-UI public output integration test".to_string(),
        runner_id: Some(RunnerId {
            value: runner_id.value,
        }),
        runner_settings: settings.encode_to_vec(),
        response_type: ResponseType::Direct as i32,
        broadcast_results: true,
        ..Default::default()
    };
    app_module
        .worker_app
        .create(&worker)
        .await
        .context("Failed to create test workflow worker")
}

pub async fn create_two_wait_worker(
    app_module: &Arc<AppModule>,
    worker_name: &str,
) -> Result<WorkerId> {
    create_two_wait_worker_with_channel(app_module, worker_name, None).await
}

pub async fn create_two_wait_worker_with_channel(
    app_module: &Arc<AppModule>,
    worker_name: &str,
    channel: Option<String>,
) -> Result<WorkerId> {
    let runner = app_module
        .runner_app
        .find_runner_by_name("WORKFLOW")
        .await?
        .context("WORKFLOW runner is unavailable in test app")?;
    let runner_id = runner.id.context("WORKFLOW runner has no ID")?;
    let workflow = serde_json::json!({
        "document": {
            "dsl": "1.0.0",
            "namespace": "ag-ui-test",
            "name": "guardrail-workflow",
            "version": "1.0.0"
        },
        "input": {
            "schema": {
                "document": {
                    "type": "object",
                    "properties": {}
                }
            }
        },
        "checkpointing": {
            "enabled": true,
            "storage": "redis"
        },
        "do": [
            {"first_wait": {"set": {"first_waited": true}, "then": "wait"}},
            {"second_wait": {"set": {"second_waited": true}, "then": "wait"}},
            {"publish": {"set": {"messages": [
                {
                    "id": "assistant-final",
                    "role": "assistant",
                    "content": "${\"all approvals complete for \" + $workflow.input.threadId + \"; \" + $trusted_marker}"
                }
            ]}}}
        ]
    });
    let settings = WorkflowRunnerSettings {
        workflow_source: Some(WorkflowSource::WorkflowData(workflow.to_string())),
        workflow_context: Some(r#"{"trusted_marker":"server-config"}"#.to_string()),
    };
    let worker = WorkerData {
        name: worker_name.to_string(),
        description: "AG-UI multi-stage HITL integration test".to_string(),
        runner_id: Some(RunnerId {
            value: runner_id.value,
        }),
        runner_settings: settings.encode_to_vec(),
        channel,
        response_type: ResponseType::Direct as i32,
        broadcast_results: true,
        ..Default::default()
    };
    app_module
        .worker_app
        .create(&worker)
        .await
        .context("Failed to create multi-stage HITL workflow worker")
}

pub async fn create_streaming_llm_worker(
    app_module: &Arc<AppModule>,
    worker_name: &str,
    base_url: &str,
) -> Result<WorkerId> {
    create_streaming_llm_worker_on_channel(app_module, worker_name, base_url, None).await
}

/// A streaming LLM worker on a channel no test worker serves, so its jobs stay queued.
pub async fn create_queued_streaming_llm_worker(
    app_module: &Arc<AppModule>,
    worker_name: &str,
) -> Result<WorkerId> {
    create_streaming_llm_worker_on_channel(
        app_module,
        worker_name,
        "http://127.0.0.1:9",
        // Short enough for the MySQL `worker.channel` column (VARCHAR(32)).
        Some(format!(
            "unserved-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..16]
        )),
    )
    .await
}

async fn create_streaming_llm_worker_on_channel(
    app_module: &Arc<AppModule>,
    worker_name: &str,
    base_url: &str,
    channel: Option<String>,
) -> Result<WorkerId> {
    let runner = app_module
        .runner_app
        .find_runner_by_name("LLM")
        .await?
        .context("LLM runner is unavailable in test app")?;
    let runner_id = runner.id.context("LLM runner has no ID")?;
    let settings = LlmRunnerSettings {
        settings: Some(Settings::Ollama(OllamaRunnerSettings {
            model: "ag-ui-http-test".to_string(),
            base_url: Some(base_url.to_string()),
            system_prompt: None,
            pull_model: Some(false),
        })),
        embedding_chunking: None,
    };
    let worker = WorkerData {
        name: worker_name.to_string(),
        description: "AG-UI streaming HTTP integration test".to_string(),
        runner_id: Some(RunnerId {
            value: runner_id.value,
        }),
        runner_settings: settings.encode_to_vec(),
        response_type: ResponseType::Direct as i32,
        broadcast_results: true,
        channel,
        ..Default::default()
    };
    app_module
        .worker_app
        .create(&worker)
        .await
        .context("Failed to create test streaming LLM worker")
}

fn direct_input(
    thread_id: &str,
    run_id: &str,
    content: serde_json::Value,
) -> crate::types::RunAgentInput {
    serde_json::from_value(serde_json::json!({
        "threadId": thread_id,
        "runId": run_id,
        "messages": [{"id": format!("{run_id}-user"), "role": "user", "content": content}],
    }))
    .expect("valid direct input")
}

/// A direct run whose job stays queued because no worker serves its channel.
pub(super) struct QueuedDirectRun {
    pub worker_name: String,
    worker_id: WorkerId,
    pub input: crate::types::RunAgentInput,
}

impl QueuedDirectRun {
    pub async fn create(app_module: &Arc<AppModule>, run_id: &str) -> Result<Self> {
        let worker_name = format!("queued-direct-{}", uuid::Uuid::new_v4());
        let worker_id = create_queued_streaming_llm_worker(app_module, &worker_name).await?;
        Ok(Self {
            worker_name,
            worker_id,
            input: direct_input("queued-direct-thread", run_id, "hi".into()),
        })
    }

    /// Check that the POST stream, a reconnected stream and the session all ended as
    /// cancelled, then remove the worker.
    pub async fn assert_cancelled(
        self,
        app_module: &AppModule,
        live: &[(u64, AgUiEvent)],
        reconnect: &[(u64, AgUiEvent)],
        session_manager: &crate::session::InMemorySessionManager,
        session_id: &str,
    ) -> Result<()> {
        use crate::session::{SessionManager, SessionState};

        crate::direct::assert_cancelled_once(live);
        crate::direct::assert_cancelled_once(reconnect);
        let session = session_manager
            .get_session(session_id)
            .await
            .context("cancelled session")?;
        assert_eq!(session.state, SessionState::Cancelled);
        app_module.worker_app.delete(&self.worker_id).await?;
        Ok(())
    }
}

/// Collect a run's SSE events, failing instead of hanging when the run never terminates.
pub(super) async fn collect_run_events(
    stream: impl futures::Stream<Item = (u64, AgUiEvent)>,
) -> Vec<(u64, AgUiEvent)> {
    tokio::time::timeout(std::time::Duration::from_secs(10), stream.collect())
        .await
        .expect("the run stream must close after its terminal event")
}

/// Direct-profile inputs that must fail before any job runs: one rejected by message
/// conversion and one that only fails when the worker lookup misses.
pub(super) fn rejected_direct_inputs(thread_id: &str) -> Vec<crate::types::RunAgentInput> {
    let image = serde_json::json!([{
        "type": "image",
        "source": {"type": "data", "value": "aGVsbG8=", "mimeType": "image/png"}
    }]);
    [
        ("direct-rejected-image", image),
        ("direct-missing-worker", "hi".into()),
    ]
    .into_iter()
    .map(|(run_id, content)| direct_input(thread_id, run_id, content))
    .collect()
}

pub(super) async fn assert_no_session_left(
    session_manager: &crate::session::InMemorySessionManager,
    run_id: &str,
    thread_id: &str,
) {
    use crate::session::SessionManager;
    assert!(
        session_manager
            .get_session_by_run_id(&crate::types::RunId::new(run_id))
            .await
            .is_none(),
        "rejected direct run {run_id} must not leave a session"
    );
    assert!(
        session_manager
            .get_active_session_by_thread_id(&crate::types::ThreadId::new(thread_id))
            .await
            .is_none(),
        "rejected direct run {run_id} must not report the thread as running"
    );
}

pub(super) fn interrupt_id(events: &[(u64, AgUiEvent)]) -> String {
    events
        .iter()
        .find_map(|(_, event)| match event {
            AgUiEvent::RunFinished {
                outcome: Some(RunOutcome::Interrupt { interrupts }),
                ..
            } => interrupts.first().map(|interrupt| interrupt.id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("run should finish with an interrupt, got {events:#?}"))
}

pub(super) fn assert_no_internal_tool_lifecycle(events: &[(u64, AgUiEvent)]) {
    assert!(events.iter().all(|(_, event)| !matches!(
        event,
        AgUiEvent::ToolCallStart { .. }
            | AgUiEvent::ToolCallArgs { .. }
            | AgUiEvent::ToolCallEnd { .. }
            | AgUiEvent::ToolCallResult { .. }
    )));
}
