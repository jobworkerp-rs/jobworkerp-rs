use std::{
    collections::VecDeque,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_server::{
    approval::MemoryPendingApprovalStore,
    backend::{AgentBackend, ChatOrchestratorFactory},
    chat::{
        ChatConfig, ChatOrchestrator, ChatToolRegistry, ModelInvocation, ModelInvoker,
        ModelOptions, ModelResponse, RegisteredTool, Role, ToolCall, ToolRegistryAdapter,
        WorkerError, WorkerExecutor, WorkerJob, WorkerWaitError,
    },
    http::{HttpConfig, router},
    model::ModelTextDeltaCallback,
    skills::SkillCatalog,
    tool_registry::{
        ResolvedInputSchema, SchemaResolutionError, ToolRegistration, ToolRegistry,
        WorkerSchemaResolver,
    },
};
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Notify;
use tokio::sync::oneshot;
use tower::ServiceExt;

#[derive(Default)]
struct FakeModel {
    outputs: Mutex<VecDeque<Result<ModelResponse, String>>>,
    invocations: Mutex<Vec<ModelInvocation>>,
    gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    delta_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    text_deltas: Mutex<VecDeque<Vec<String>>>,
    finished_stream_invocations: AtomicUsize,
}

impl FakeModel {
    fn new(outputs: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into_iter().map(Ok).collect()),
            ..Self::default()
        }
    }
}

#[async_trait]
impl ModelInvoker for FakeModel {
    async fn invoke(&self, invocation: ModelInvocation) -> Result<ModelResponse, String> {
        self.invocations
            .lock()
            .expect("invocation lock")
            .push(invocation);
        let gate = self.gate.lock().expect("gate lock").take();
        if let Some((started, release)) = gate {
            let _ = started.send(());
            release
                .await
                .map_err(|_| "test gate was dropped".to_owned())?;
        }
        self.outputs
            .lock()
            .expect("output lock")
            .pop_front()
            .unwrap_or_else(|| Err("no scripted model response".to_owned()))
    }

    async fn invoke_with_observer_and_text_delta(
        &self,
        invocation: ModelInvocation,
        _observer: Arc<dyn agent_server::chat::ModelJobObserver>,
        callback: ModelTextDeltaCallback,
    ) -> Result<ModelResponse, String> {
        let deltas = self
            .text_deltas
            .lock()
            .expect("text delta lock")
            .pop_front()
            .unwrap_or_default();
        for (index, text) in deltas.iter().enumerate() {
            callback(text)?;
            if index == 0 {
                let gate = self.delta_gate.lock().expect("delta gate lock").take();
                if let Some((started, release)) = gate {
                    let _ = started.send(());
                    release
                        .await
                        .map_err(|_| "test delta gate was dropped".to_owned())?;
                }
            }
        }
        let result = self.invoke(invocation).await;
        self.finished_stream_invocations
            .fetch_add(1, Ordering::SeqCst);
        result
    }
}

#[derive(Default)]
struct FakeWorkers {
    started: Mutex<Vec<(RegisteredTool, Value)>>,
    cancelled: Mutex<Vec<String>>,
    block_next_wait: AtomicBool,
    fail_blocked_wait: AtomicBool,
    fail_next_cancel: AtomicBool,
    uncertain_next_wait: AtomicBool,
    wait_started: Notify,
    wait_released: Notify,
    cancel_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}

#[async_trait]
impl WorkerExecutor for FakeWorkers {
    async fn start(
        &self,
        tool: &RegisteredTool,
        arguments: Value,
    ) -> Result<WorkerJob, WorkerError> {
        self.started
            .lock()
            .expect("worker starts lock")
            .push((tool.clone(), arguments));
        Ok(WorkerJob {
            job_id: "owned-job-1".to_owned(),
        })
    }

    async fn wait(&self, job_id: &str) -> Result<Value, WorkerError> {
        assert_eq!(job_id, "owned-job-1");
        if self.block_next_wait.swap(false, Ordering::SeqCst) {
            self.wait_started.notify_one();
            self.wait_released.notified().await;
            if self.fail_blocked_wait.swap(false, Ordering::SeqCst) {
                return Err(WorkerError("the job was cancelled".to_owned()));
            }
        }
        Ok(json!({"published": true}))
    }

    async fn wait_with_ownership(&self, job_id: &str) -> Result<Value, WorkerWaitError> {
        if self.uncertain_next_wait.swap(false, Ordering::SeqCst) {
            return Err(WorkerWaitError::Uncertain(WorkerError(format!(
                "result stream lost for {job_id}"
            ))));
        }
        self.wait(job_id).await.map_err(WorkerWaitError::Terminal)
    }

    async fn cancel(&self, job_id: &str) -> Result<(), WorkerError> {
        self.cancelled
            .lock()
            .expect("worker cancellations lock")
            .push(job_id.to_owned());
        if self.fail_next_cancel.swap(false, Ordering::SeqCst) {
            return Err(WorkerError("simulated Delete failure".to_owned()));
        }
        let gate = { self.cancel_gate.lock().unwrap().take() };
        if let Some((started, release)) = gate {
            let _ = started.send(());
            release
                .await
                .map_err(|_| WorkerError("test cancellation gate was dropped".to_owned()))?;
        }
        self.wait_released.notify_one();
        Ok(())
    }
}

impl FakeWorkers {
    fn block_next_wait(&self) {
        self.fail_blocked_wait.store(true, Ordering::SeqCst);
        self.block_next_wait.store(true, Ordering::SeqCst);
    }

    fn block_next_wait_successfully(&self) {
        self.fail_blocked_wait.store(false, Ordering::SeqCst);
        self.block_next_wait.store(true, Ordering::SeqCst);
    }

    fn release_wait_successfully(&self) {
        self.wait_released.notify_one();
    }

    fn fail_next_cancel(&self) {
        self.fail_next_cancel.store(true, Ordering::SeqCst);
    }

    fn uncertain_next_wait(&self) {
        self.uncertain_next_wait.store(true, Ordering::SeqCst);
    }

    fn block_next_cancel(&self, started: oneshot::Sender<()>, release: oneshot::Receiver<()>) {
        *self.cancel_gate.lock().unwrap() = Some((started, release));
    }
}

struct FakeResolver;

#[async_trait]
impl WorkerSchemaResolver for FakeResolver {
    async fn resolve_input_schema(
        &self,
        worker_id: i64,
        using: &str,
    ) -> Result<ResolvedInputSchema, SchemaResolutionError> {
        Ok(ResolvedInputSchema {
            schema: json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"],
                "additionalProperties": false
            }),
            revision: format!("{worker_id}:{using}:1"),
        })
    }
}

struct TestFactory {
    model: Arc<FakeModel>,
    skills: Arc<SkillCatalog>,
    registry: Arc<dyn ChatToolRegistry>,
    workers: Arc<FakeWorkers>,
    approvals: Arc<MemoryPendingApprovalStore>,
    chat_config: ChatConfig,
    requests: Mutex<Vec<(i64, Option<agent_server::http::ChatOptions>)>>,
    resume_creations: AtomicBool,
}

impl TestFactory {
    fn new(
        model: Arc<FakeModel>,
        skills: Arc<SkillCatalog>,
        registry: Arc<dyn ChatToolRegistry>,
        workers: Arc<FakeWorkers>,
    ) -> Self {
        Self {
            model,
            skills,
            registry,
            workers,
            approvals: Arc::new(MemoryPendingApprovalStore::new(Duration::from_secs(600))),
            chat_config: ChatConfig::default(),
            requests: Mutex::new(Vec::new()),
            resume_creations: AtomicBool::new(false),
        }
    }

    fn build_orchestrator(&self) -> Arc<ChatOrchestrator> {
        Arc::new(ChatOrchestrator::new(
            self.model.clone(),
            self.skills.clone(),
            self.registry.clone(),
            self.workers.clone(),
            self.approvals.clone(),
            self.chat_config.clone(),
        ))
    }
}

#[async_trait]
impl ChatOrchestratorFactory for TestFactory {
    async fn create(
        &self,
        llm_worker_id: i64,
        options: Option<agent_server::http::ChatOptions>,
    ) -> Result<Arc<ChatOrchestrator>, String> {
        self.requests
            .lock()
            .expect("factory request lock")
            .push((llm_worker_id, options));
        Ok(self.build_orchestrator())
    }

    async fn create_for_resume(&self) -> Result<Arc<ChatOrchestrator>, String> {
        self.resume_creations.store(true, Ordering::SeqCst);
        Ok(self.build_orchestrator())
    }
}

struct TestApp {
    app: axum::Router,
    factory: Arc<TestFactory>,
    skills: Arc<SkillCatalog>,
    registry: Arc<ToolRegistry>,
    workers: Arc<FakeWorkers>,
}

async fn test_app(model: Arc<FakeModel>, skills_root: &Path) -> TestApp {
    test_app_with_config(model, skills_root, ChatConfig::default()).await
}

async fn test_app_with_config(
    model: Arc<FakeModel>,
    skills_root: &Path,
    chat_config: ChatConfig,
) -> TestApp {
    let registry = Arc::new(
        ToolRegistry::open(skills_root.join("tools.json"), Arc::new(FakeResolver))
            .await
            .expect("registry opens"),
    );
    let skills = Arc::new(SkillCatalog::new(vec![skills_root.to_path_buf()]));
    let workers = Arc::new(FakeWorkers::default());
    let chat_registry: Arc<dyn ChatToolRegistry> =
        Arc::new(ToolRegistryAdapter::new(registry.clone()));
    let mut factory = TestFactory::new(model, skills.clone(), chat_registry, workers.clone());
    factory.chat_config = chat_config;
    let factory = Arc::new(factory);
    let backend = Arc::new(AgentBackend::new(
        factory.clone(),
        skills.clone(),
        registry.clone(),
    ));
    let config = HttpConfig::local_no_token(
        "127.0.0.1:9000"
            .parse::<SocketAddr>()
            .expect("valid test socket"),
    );
    TestApp {
        app: router(config, backend.clone()).expect("router validates"),
        factory,
        skills,
        registry,
        workers,
    }
}

fn request(method: &str, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:9000")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request builds")
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body reads");
    serde_json::from_slice(&body).expect("response is JSON")
}

async fn read_sse_body(response: axum::response::Response) -> String {
    let mut events = response.into_body().into_data_stream();
    let mut output = String::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(chunk) = events.next().await {
            output.push_str(std::str::from_utf8(&chunk.expect("SSE data")).expect("SSE text"));
        }
    })
    .await
    .expect("SSE stream finishes");
    output
}

fn sse_event_count(output: &str, name: &str) -> usize {
    let expected = format!("event: {name}");
    output.lines().filter(|line| *line == expected).count()
}

fn parse_sse_frame(frame: &[u8]) -> (String, Value) {
    let frame = std::str::from_utf8(frame).expect("SSE frame is UTF-8");
    let event = frame
        .lines()
        .find_map(|line| line.strip_prefix("event: "))
        .expect("SSE event name")
        .to_owned();
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("SSE event data");
    (
        event,
        serde_json::from_str(data).expect("SSE envelope parses"),
    )
}

fn text_delta_from_frame(frame: &[u8]) -> String {
    let (event, envelope) = parse_sse_frame(frame);
    assert_eq!(event, "text_delta");
    envelope["event"]["text"]
        .as_str()
        .expect("text delta payload")
        .to_owned()
}

fn model_response(content: Value, tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        content,
        tool_calls,
    }
}

#[tokio::test]
async fn admin_routes_project_skills_and_worker_resolved_tool_schemas() {
    let directory = tempfile::tempdir().expect("temporary root");
    std::fs::create_dir_all(directory.path().join("alpha-skill")).expect("skill directory");
    std::fs::write(
        directory.path().join("alpha-skill/SKILL.md"),
        "---\nname: alpha-skill\ndescription: A test skill\n---\nSkill body.\n",
    )
    .expect("skill file");
    let test = test_app(Arc::new(FakeModel::default()), directory.path()).await;

    let skills = test
        .app
        .clone()
        .oneshot(request("GET", "/v1/skills", json!({})))
        .await
        .expect("skills response");
    assert_eq!(skills.status(), StatusCode::OK);
    assert_eq!(
        response_json(skills).await["skills"][0]["name"],
        "alpha-skill"
    );

    let detail = test
        .app
        .clone()
        .oneshot(request("GET", "/v1/skills/alpha-skill", json!({})))
        .await
        .expect("skill detail response");
    assert_eq!(detail.status(), StatusCode::OK);
    assert_eq!(response_json(detail).await["body"], "Skill body.\n");

    let reloaded = test
        .app
        .clone()
        .oneshot(request("POST", "/v1/skills/reload", json!({})))
        .await
        .expect("reload response");
    assert_eq!(reloaded.status(), StatusCode::OK);
    assert_eq!(response_json(reloaded).await["loadedCount"], 1);

    let definition = json!({
        "description": "Publish a message",
        "workerId": 75,
        "method": "publish",
        "requiresApproval": true
    });
    let upsert = test
        .app
        .clone()
        .oneshot(request("PUT", "/v1/tools/publish", definition.clone()))
        .await
        .expect("upsert response");
    assert_eq!(upsert.status(), StatusCode::OK);
    assert_eq!(
        response_json(upsert).await["inputSchema"]["properties"]["message"]["type"],
        "string"
    );

    let mut update = definition;
    update["description"] = json!("Updated description");
    update["requiresApproval"] = json!(false);
    let updated = test
        .app
        .clone()
        .oneshot(request("PUT", "/v1/tools/publish", update))
        .await
        .expect("update response");
    assert_eq!(updated.status(), StatusCode::OK);
    assert_eq!(
        response_json(updated).await["description"],
        "Updated description"
    );

    let listed = test
        .app
        .clone()
        .oneshot(request("GET", "/v1/tools", json!({})))
        .await
        .expect("tool list response");
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = response_json(listed).await;
    assert_eq!(listed["tools"].as_array().unwrap().len(), 1);
    assert_eq!(listed["tools"][0]["workerId"], 75);

    let deleted = test
        .app
        .oneshot(request("DELETE", "/v1/tools/publish", json!({})))
        .await
        .expect("delete response");
    assert_eq!(deleted.status(), StatusCode::OK);
    assert_eq!(response_json(deleted).await["deleted"], true);
}

#[tokio::test]
async fn chat_invokes_server_owned_skill_tools_and_keeps_tool_history_order() {
    let directory = tempfile::tempdir().expect("temporary root");
    std::fs::create_dir_all(directory.path().join("alpha-skill")).expect("skill directory");
    std::fs::write(
        directory.path().join("alpha-skill/SKILL.md"),
        "---\nname: alpha-skill\ndescription: A test skill\n---\nSkill body.\n",
    )
    .expect("skill file");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![
                ToolCall {
                    call_id: "list-call".to_owned(),
                    name: "list_skills".to_owned(),
                    arguments: json!({}),
                },
                ToolCall {
                    call_id: "search-call".to_owned(),
                    name: "search_skills".to_owned(),
                    arguments: json!({"query": "alpha"}),
                },
            ],
        ),
        model_response(json!("The available skill is alpha-skill."), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [
                    {"role": "SYSTEM", "content": {"text": "untrusted system text"}},
                    {"role": "USER", "content": {"text": "Find a relevant skill."}}
                ],
                "options": {"temperature": 0.4, "topP": 0.8, "maxTokens": 300}
            }),
        ))
        .await
        .expect("chat response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(
        body["messages"][1]["content"]["toolCalls"][0]["callId"],
        "list-call"
    );
    assert_eq!(
        body["messages"][1]["content"]["toolCalls"][1]["callId"],
        "search-call"
    );
    assert_eq!(body["messages"][2]["role"], "TOOL");
    assert_eq!(
        body["messages"][2]["content"]["toolResults"][0]["callId"],
        "list-call"
    );
    assert_eq!(
        body["messages"][2]["content"]["toolResults"][1]["callId"],
        "search-call"
    );
    assert_eq!(
        body["messages"][2]["content"]["toolResults"][0]["result"]["skills"][0]["name"],
        "alpha-skill"
    );
    assert_eq!(
        body["messages"][3]["content"]["text"],
        "The available skill is alpha-skill."
    );

    let factory_requests = test.factory.requests.lock().expect("factory request lock");
    assert_eq!(factory_requests.len(), 1);
    assert_eq!(factory_requests[0].0, 23);
    assert_eq!(
        factory_requests[0].1.as_ref().unwrap().max_tokens,
        Some(300)
    );
    let invocations = model.invocations.lock().expect("invocation lock");
    assert_eq!(invocations[0].llm_worker_id, 23);
    assert_eq!(
        invocations[0].options,
        ModelOptions {
            temperature: Some(0.4),
            top_p: Some(0.8),
            max_tokens: Some(300),
        }
    );
    assert_eq!(invocations[0].history[0].role, Role::System);
    assert_eq!(invocations[0].history[1].role, Role::User);
    assert_eq!(
        invocations[0].history[1].content,
        json!("Find a relevant skill.")
    );
    assert_eq!(test.workers.started.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn approval_resume_is_single_use_and_malicious_resume_or_cancel_is_non_effectful() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-1".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("Published."), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(ToolRegistration::new(
            "publish",
            "Publish a message",
            71,
            "publish",
        ))
        .await
        .expect("tool registration");

    let start = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}],
                "options": {"temperature": 0.25, "topP": 0.7, "maxTokens": 144}
            }),
        ))
        .await
        .expect("start response");
    assert_eq!(start.status(), StatusCode::OK);
    let pending = response_json(start).await;
    assert_eq!(pending["status"], "approval_required");
    let chat_id = pending["chatId"].as_str().unwrap().to_owned();
    let cancel_capability = pending["cancelCapability"].as_str().unwrap().to_owned();
    let resume_capability = pending["resumeCapability"].as_str().unwrap().to_owned();
    assert_eq!(pending["pendingCalls"][0]["callId"], "publish-1");

    let denied_cancel = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "attacker-controlled"}),
        ))
        .await
        .expect("cancel response");
    assert_eq!(denied_cancel.status(), StatusCode::OK);
    assert_eq!(response_json(denied_cancel).await["cancelled"], false);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());

    let denied_resume = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats/guessed-chat/resume",
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId": "publish-1", "decision": "approve"}]
            }),
        ))
        .await
        .expect("resume response");
    assert_eq!(denied_resume.status(), StatusCode::BAD_GATEWAY);
    assert!(test.workers.started.lock().unwrap().is_empty());

    let wrong_call = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId": "other-call", "decision": "approve"}]
            }),
        ))
        .await
        .expect("wrong call response");
    assert_eq!(wrong_call.status(), StatusCode::BAD_GATEWAY);
    assert!(test.workers.started.lock().unwrap().is_empty());

    let restarted_backend = Arc::new(AgentBackend::new(
        test.factory.clone(),
        test.skills.clone(),
        test.registry.clone(),
    ));
    let restarted = router(
        HttpConfig::local_no_token("127.0.0.1:9000".parse().unwrap()),
        restarted_backend,
    )
    .expect("restarted router validates");
    let resumed = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId": "publish-1", "decision": "approve"}]
            }),
        ))
        .await
        .expect("valid resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let completed = response_json(resumed).await;
    assert_eq!(completed["status"], "completed");
    {
        let started = test.workers.started.lock().expect("worker starts lock");
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].0.worker_id, 71);
        assert_eq!(started[0].1, json!({"message": "release"}));
        let invocations = model.invocations.lock().expect("invocation lock");
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[1].llm_worker_id, 23);
        assert_eq!(
            invocations[1].options,
            ModelOptions {
                temperature: Some(0.25),
                top_p: Some(0.7),
                max_tokens: Some(144),
            }
        );
    }
    assert!(test.factory.resume_creations.load(Ordering::SeqCst));

    let replay = restarted
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId": "publish-1", "decision": "approve"}]
            }),
        ))
        .await
        .expect("replay response");
    assert_eq!(replay.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(test.workers.started.lock().unwrap().len(), 1);
    assert_eq!(test.factory.requests.lock().unwrap().len(), 1);
    assert_ne!(cancel_capability, resume_capability);
}

#[tokio::test]
async fn sse_started_and_cancel_capability_are_available_before_model_completion() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        json!("streamed answer"),
        vec![],
    )]));
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    *model.gate.lock().expect("gate lock") = Some((started_tx, release_rx));
    let test = test_app(model.clone(), directory.path()).await;

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "hello"}}],
                "options": {"temperature": 0.6, "topP": 0.75, "maxTokens": 90}
            }),
        ))
        .await
        .expect("SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut events = response.into_body().into_data_stream();
    let started_frame = events
        .next()
        .await
        .expect("started frame")
        .expect("SSE data");
    let started_frame = String::from_utf8(started_frame.to_vec()).expect("SSE text");
    assert!(started_frame.contains("event: started"));
    assert!(started_frame.contains("cancelCapability"));

    tokio::time::timeout(Duration::from_secs(2), started_rx)
        .await
        .expect("model invocation starts")
        .expect("model start signal");
    {
        let invocations = model.invocations.lock().unwrap();
        assert_eq!(invocations.len(), 1);
        let invocation = &invocations[0];
        assert_eq!(invocation.llm_worker_id, 23);
        assert_eq!(
            invocation.options,
            ModelOptions {
                temperature: Some(0.6),
                top_p: Some(0.75),
                max_tokens: Some(90),
            }
        );
    }
    release_tx.send(()).expect("release model gate");
    let output = tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = String::new();
        while let Some(chunk) = events.next().await {
            output.push_str(std::str::from_utf8(&chunk.expect("SSE data")).unwrap());
        }
        output
    })
    .await
    .expect("SSE stream finishes");
    assert!(output.contains("event: text_delta"));
    assert!(output.contains("streamed answer"));
    assert!(output.contains("event: completed"));
}

#[tokio::test]
async fn sse_delivers_text_before_model_completion_without_replaying_the_full_response() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        json!("early answer"),
        vec![],
    )]));
    *model.text_deltas.lock().expect("text delta lock") =
        VecDeque::from([vec!["early ".to_owned(), "answer".to_owned()]]);
    let (delta_started_tx, delta_started_rx) = oneshot::channel();
    let (release_delta_tx, release_delta_rx) = oneshot::channel();
    *model.delta_gate.lock().expect("delta gate lock") = Some((delta_started_tx, release_delta_rx));
    let test = test_app(model.clone(), directory.path()).await;

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "hello"}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let started = events
        .next()
        .await
        .expect("started frame")
        .expect("SSE data");
    assert_eq!(parse_sse_frame(&started).0, "started");

    tokio::time::timeout(Duration::from_secs(2), delta_started_rx)
        .await
        .expect("first text delta is emitted")
        .expect("delta gate signal");
    let first_delta = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("text delta arrives while model is gated")
        .expect("text delta frame")
        .expect("SSE data");
    assert_eq!(text_delta_from_frame(&first_delta), "early ");
    assert_eq!(model.finished_stream_invocations.load(Ordering::SeqCst), 0);

    release_delta_tx.send(()).expect("release model delta gate");
    let mut deltas = vec![text_delta_from_frame(&first_delta)];
    let mut terminal_events = Vec::new();
    while let Some(frame) = events.next().await {
        let frame = frame.expect("SSE data");
        let (event, envelope) = parse_sse_frame(&frame);
        match event.as_str() {
            "text_delta" => deltas.push(
                envelope["event"]["text"]
                    .as_str()
                    .expect("text delta")
                    .to_owned(),
            ),
            "completed" | "approval_required" | "error" => terminal_events.push(event),
            _ => {}
        }
    }

    assert_eq!(deltas, ["early ", "answer"]);
    assert_eq!(deltas.concat(), "early answer");
    assert_eq!(terminal_events, ["completed"]);
}

#[tokio::test]
async fn sse_backpressure_replays_only_the_unsent_text_suffix_in_order() {
    let directory = tempfile::tempdir().expect("temporary root");
    let chunks: Vec<String> = (0..24).map(|index| format!("<{index:02}>")).collect();
    let final_text = chunks.concat();
    let model = Arc::new(FakeModel::new([model_response(
        json!(final_text.clone()),
        vec![],
    )]));
    *model.text_deltas.lock().expect("text delta lock") = VecDeque::from([chunks.clone()]);
    let test = test_app(model.clone(), directory.path()).await;

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "hello"}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let started = events
        .next()
        .await
        .expect("started frame")
        .expect("SSE data");
    assert_eq!(parse_sse_frame(&started).0, "started");

    tokio::time::timeout(Duration::from_secs(2), async {
        while model.finished_stream_invocations.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the model finishes despite a full SSE queue");

    let mut deltas = Vec::new();
    let mut terminal_events = Vec::new();
    while let Some(frame) = events.next().await {
        let frame = frame.expect("SSE data");
        let (event, envelope) = parse_sse_frame(&frame);
        match event.as_str() {
            "text_delta" => deltas.push(
                envelope["event"]["text"]
                    .as_str()
                    .expect("text delta")
                    .to_owned(),
            ),
            "completed" | "approval_required" | "error" => terminal_events.push(event),
            _ => {}
        }
    }

    assert_eq!(deltas.len(), 17, "sixteen chunks plus one resumed suffix");
    assert_eq!(&deltas[..16], &chunks[..16]);
    assert_eq!(deltas[16], chunks[16..].concat());
    assert_eq!(deltas.concat(), final_text);
    assert_eq!(terminal_events, ["completed"]);
}

#[tokio::test]
async fn sse_matches_live_text_to_the_generated_turn_after_tool_progress() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-before-text".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("second turn"), vec![]),
    ]));
    *model.text_deltas.lock().expect("text delta lock") =
        VecDeque::from([vec![], vec!["second ".to_owned(), "turn".to_owned()]]);
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let output = read_sse_body(response).await;
    let event_positions = ["tool_call", "tool_result", "text_delta", "completed"].map(|event| {
        output
            .find(&format!("event: {event}"))
            .expect("event exists")
    });
    assert!(
        event_positions
            .windows(2)
            .all(|positions| positions[0] < positions[1])
    );
    assert_eq!(sse_event_count(&output, "text_delta"), 2, "{output}");
    assert!(output.contains("second "), "{output}");
    assert!(output.contains("\"text\":\"turn\""), "{output}");
    assert_eq!(sse_event_count(&output, "completed"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "error"), 0, "{output}");
}

#[tokio::test]
async fn sse_text_that_disagrees_with_the_final_turn_ends_in_error() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        json!("authoritative final text"),
        vec![],
    )]));
    *model.text_deltas.lock().expect("text delta lock") =
        VecDeque::from([vec!["incorrect prefix".to_owned()]]);
    let test = test_app(model, directory.path()).await;

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "hello"}}]
            }),
        ))
        .await
        .expect("SSE response");
    let output = read_sse_body(response).await;
    assert_eq!(sse_event_count(&output, "text_delta"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "error"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "completed"), 0, "{output}");
}

#[tokio::test]
async fn cancel_capability_cancels_a_pending_worker_owned_by_the_chat() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-pending".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("done"), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish a message", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.block_next_wait();

    let response = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut events = response.into_body().into_data_stream();
    let start = events
        .next()
        .await
        .expect("start frame")
        .expect("start SSE data");
    let start = String::from_utf8(start.to_vec()).expect("SSE text");
    let start_data = start
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("start event JSON");
    let start_data: Value = serde_json::from_str(start_data).expect("start event parses");
    let chat_id = start_data["chatId"].as_str().expect("chat ID");
    let cancel_capability = start_data["event"]["cancelCapability"]
        .as_str()
        .expect("cancel capability");

    tokio::time::timeout(Duration::from_secs(2), test.workers.wait_started.notified())
        .await
        .expect("worker wait starts");
    assert_eq!(test.workers.started.lock().unwrap().len(), 1);

    let denied = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "not-the-capability"}),
        ))
        .await
        .expect("unauthorized cancel response");
    assert_eq!(denied.status(), StatusCode::OK);
    assert_eq!(response_json(denied).await["cancelled"], false);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());

    let (cancel_started_tx, cancel_started_rx) = oneshot::channel();
    let (cancel_release_tx, cancel_release_rx) = oneshot::channel();
    test.workers
        .block_next_cancel(cancel_started_tx, cancel_release_rx);
    let cancel_app = test.app.clone();
    let cancel_uri = format!("/v1/chats/{chat_id}/cancel");
    let cancel_capability = cancel_capability.to_owned();
    let cancel_task = tokio::spawn(async move {
        cancel_app
            .oneshot(request(
                "POST",
                &cancel_uri,
                json!({"cancelCapability": cancel_capability}),
            ))
            .await
            .expect("authorized cancel response")
    });
    tokio::time::timeout(Duration::from_secs(2), cancel_started_rx)
        .await
        .expect("worker cancellation starts")
        .expect("worker cancellation signal");

    let while_cancelling = tokio::time::timeout(
        Duration::from_secs(2),
        test.app.clone().oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "not-the-capability"}),
        )),
    )
    .await
    .expect("session lock is released during worker cancellation")
    .expect("concurrent unauthorized cancel response");
    assert_eq!(while_cancelling.status(), StatusCode::OK);
    assert_eq!(response_json(while_cancelling).await["cancelled"], false);

    cancel_release_tx
        .send(())
        .expect("release cancellation gate");
    let cancelled = cancel_task.await.expect("cancel task finishes");
    assert_eq!(cancelled.status(), StatusCode::OK);
    assert_eq!(response_json(cancelled).await["cancelled"], true);
    assert_eq!(
        test.workers.cancelled.lock().unwrap().as_slice(),
        ["owned-job-1"]
    );
}

#[tokio::test]
async fn sse_emits_tool_call_before_the_worker_finishes() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "pending-publication".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("done"), vec![]),
    ]));
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.block_next_wait();

    let response = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let start = events.next().await.expect("start frame").expect("SSE data");
    let start = String::from_utf8(start.to_vec()).expect("start UTF-8");
    let start_data = start
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .expect("start event JSON");
    let start_data: Value = serde_json::from_str(start_data).expect("start event parses");
    let chat_id = start_data["chatId"].as_str().expect("chat ID");
    let capability = start_data["event"]["cancelCapability"]
        .as_str()
        .expect("cancel capability");
    tokio::time::timeout(Duration::from_secs(2), test.workers.wait_started.notified())
        .await
        .expect("tool wait starts");

    let progress = tokio::time::timeout(Duration::from_millis(300), events.next())
        .await
        .expect("tool call must arrive while Worker is waiting")
        .expect("tool call frame")
        .expect("SSE data");
    let progress = String::from_utf8(progress.to_vec()).expect("progress UTF-8");
    assert!(progress.contains("event: tool_call"), "{progress}");
    assert!(progress.contains("pending-publication"));

    let cancelled = test
        .app
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("cancel response");
    assert_eq!(response_json(cancelled).await["cancelled"], true);
}

#[tokio::test]
async fn sse_emits_one_live_tool_result_before_one_completed_terminal() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-result".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("done"), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.block_next_wait_successfully();

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let start = events.next().await.expect("start frame").expect("SSE data");
    let start = String::from_utf8(start.to_vec()).expect("start UTF-8");
    assert!(start.contains("event: started"));
    tokio::time::timeout(Duration::from_secs(2), test.workers.wait_started.notified())
        .await
        .expect("tool wait starts");

    let tool_call = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("tool call is delivered while the Worker waits")
        .expect("tool call frame")
        .expect("SSE data");
    let tool_call = String::from_utf8(tool_call.to_vec()).expect("tool call UTF-8");
    assert!(tool_call.contains("event: tool_call"), "{tool_call}");
    assert!(tool_call.contains("publish-result"));

    let (next_model_started_tx, next_model_started_rx) = oneshot::channel();
    let (release_next_model_tx, release_next_model_rx) = oneshot::channel();
    *model.gate.lock().expect("gate lock") = Some((next_model_started_tx, release_next_model_rx));
    test.workers.release_wait_successfully();
    tokio::time::timeout(Duration::from_secs(2), next_model_started_rx)
        .await
        .expect("next model invocation starts")
        .expect("next model start signal");
    let tool_result = tokio::time::timeout(Duration::from_secs(2), events.next())
        .await
        .expect("tool result arrives while the next model invocation is blocked")
        .expect("tool result frame")
        .expect("SSE data");
    let tool_result = String::from_utf8(tool_result.to_vec()).expect("tool result UTF-8");
    assert!(tool_result.contains("event: tool_result"), "{tool_result}");
    assert!(tool_result.contains("publish-result"));

    release_next_model_tx
        .send(())
        .expect("release next model gate");
    let tail = tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = String::new();
        while let Some(chunk) = events.next().await {
            output.push_str(std::str::from_utf8(&chunk.expect("SSE data")).unwrap());
        }
        output
    })
    .await
    .expect("SSE stream finishes");
    let output = format!("{tool_call}{tool_result}{tail}");
    assert_eq!(sse_event_count(&output, "tool_call"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "tool_result"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "completed"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "error"), 0, "{output}");
    assert!(
        output.find("event: tool_call").unwrap() < output.find("event: tool_result").unwrap()
            && output.find("event: tool_result").unwrap()
                < output.find("event: completed").unwrap(),
        "tool call, result, and terminal order: {output}"
    );
}

#[tokio::test]
async fn sse_backpressure_does_not_block_tool_execution_or_cancel_workers() {
    let directory = tempfile::tempdir().expect("temporary root");
    let calls = (0..9)
        .map(|index| ToolCall {
            call_id: format!("publish-{index}"),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        })
        .collect();
    let model = Arc::new(FakeModel::new([
        model_response(Value::Null, calls),
        model_response(json!("done"), vec![]),
    ]));
    *model.text_deltas.lock().expect("text delta lock") =
        VecDeque::from([vec![], vec!["done".to_owned()]]);
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish these."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let start = events.next().await.expect("start frame").expect("SSE data");
    assert!(
        String::from_utf8(start.to_vec())
            .expect("start UTF-8")
            .contains("event: started")
    );

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if test
                .workers
                .started
                .lock()
                .expect("worker starts lock")
                .len()
                == 9
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all tools proceed despite more progress events than the bounded queue holds");
    assert!(test.workers.cancelled.lock().unwrap().is_empty());

    // Dropping an unread stream must not turn progress backpressure into a worker cancellation.
    drop(events);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if model.invocations.lock().expect("invocation lock").len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("orchestration continues after the SSE receiver disconnects");
    tokio::time::timeout(Duration::from_secs(2), async {
        while model.finished_stream_invocations.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("text progress after disconnect does not block model completion");
    assert!(test.workers.cancelled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sse_emits_approval_required_as_its_only_terminal_event() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        Value::Null,
        vec![ToolCall {
            call_id: "publish-approval".to_owned(),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        }],
    )]));
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(ToolRegistration::new("publish", "Publish", 71, "publish"))
        .await
        .expect("tool registration");

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let output = read_sse_body(response).await;
    assert_eq!(sse_event_count(&output, "tool_call"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "approval_required"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "completed"), 0, "{output}");
    assert_eq!(sse_event_count(&output, "error"), 0, "{output}");
}

#[tokio::test]
async fn sse_text_before_tool_call_still_exposes_approval_capability() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        Value::Null,
        vec![ToolCall {
            call_id: "publish-after-commentary".to_owned(),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        }],
    )]));
    *model.text_deltas.lock().unwrap() = VecDeque::from([vec!["Checking the tool…".to_owned()]]);
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(ToolRegistration::new("publish", "Publish", 71, "publish"))
        .await
        .expect("tool registration");

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let output = read_sse_body(response).await;
    assert_eq!(sse_event_count(&output, "text_delta"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "tool_call"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "approval_required"), 1, "{output}");
    assert_eq!(sse_event_count(&output, "error"), 0, "{output}");
    assert!(output.contains("resume_capability"), "{output}");
}

#[tokio::test]
async fn sse_disconnect_does_not_cancel_a_worker_waiting_for_its_result() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-disconnected".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("done"), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.block_next_wait_successfully();

    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let start = events.next().await.expect("start frame").expect("SSE data");
    let start = String::from_utf8(start.to_vec()).expect("start UTF-8");
    assert!(start.contains("event: started"));
    tokio::time::timeout(Duration::from_secs(2), test.workers.wait_started.notified())
        .await
        .expect("worker wait starts");

    drop(events);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());
    test.workers.release_wait_successfully();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if model.invocations.lock().expect("invocation lock").len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker result is processed after disconnect");
    assert!(test.workers.cancelled.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_timeout_cleanup_keeps_http_cancel_capability_retryable() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        Value::Null,
        vec![ToolCall {
            call_id: "publish-timeout".to_owned(),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        }],
    )]));
    let test = test_app_with_config(
        model,
        directory.path(),
        ChatConfig {
            max_elapsed: Duration::from_millis(40),
            ..ChatConfig::default()
        },
    )
    .await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.block_next_wait();
    test.workers.fail_next_cancel();
    let response = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    let mut events = response.into_body().into_data_stream();
    let start = events.next().await.expect("start frame").expect("SSE data");
    let start = String::from_utf8(start.to_vec()).expect("start UTF-8");
    let start_data: Value = serde_json::from_str(
        start
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("start event JSON"),
    )
    .expect("start event parses");
    let chat_id = start_data["chatId"].as_str().expect("chat ID");
    let capability = start_data["event"]["cancelCapability"]
        .as_str()
        .expect("cancel capability");

    // Drain until the terminal error to ensure backend session cleanup has completed.
    let terminal = tokio::time::timeout(Duration::from_secs(2), async {
        let mut frames = String::new();
        while let Some(frame) = events.next().await {
            frames.push_str(std::str::from_utf8(&frame.expect("SSE data")).unwrap());
        }
        frames
    })
    .await
    .expect("timeout error event arrives");
    assert!(terminal.contains("event: error"), "{terminal}");
    assert_eq!(sse_event_count(&terminal, "error"), 1, "{terminal}");
    assert_eq!(test.workers.cancelled.lock().unwrap().len(), 1);

    let denied = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "wrong-capability"}),
        ))
        .await
        .expect("denied cancel response");
    assert_eq!(response_json(denied).await["cancelled"], false);
    let retry = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("authorized retry response");
    assert_eq!(response_json(retry).await["cancelled"], true);
    assert_eq!(test.workers.cancelled.lock().unwrap().len(), 2);
    let replay = test
        .app
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("replay response");
    assert_eq!(response_json(replay).await["cancelled"], false);
}

#[tokio::test]
async fn restored_resume_failure_returns_a_new_cancellation_capability_only_after_claim() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        Value::Null,
        vec![ToolCall {
            call_id: "publish-restored".to_owned(),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        }],
    )]));
    let test = test_app_with_config(
        model,
        directory.path(),
        ChatConfig {
            max_elapsed: Duration::from_millis(40),
            ..ChatConfig::default()
        },
    )
    .await;
    test.registry
        .register(ToolRegistration::new("publish", "Publish", 71, "publish"))
        .await
        .expect("tool registration");
    let initial = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("chat response");
    let initial = response_json(initial).await;
    assert_eq!(initial["status"], "approval_required");
    let chat_id = initial["chatId"].as_str().expect("chat ID");
    let resume_capability = initial["resumeCapability"]
        .as_str()
        .expect("resume capability");
    let old_cancel_capability = initial["cancelCapability"]
        .as_str()
        .expect("original cancel capability");

    let resumed_backend = Arc::new(AgentBackend::new(
        test.factory.clone(),
        test.skills.clone(),
        test.registry.clone(),
    ));
    let restarted = router(
        HttpConfig::local_no_token("127.0.0.1:9000".parse().unwrap()),
        resumed_backend,
    )
    .expect("restarted router");
    let invalid = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": "wrong-capability",
                "decisions": [{"callId":"publish-restored", "decision":"approve"}]
            }),
        ))
        .await
        .expect("invalid resume response");
    assert_eq!(invalid.status(), StatusCode::BAD_GATEWAY);
    assert!(
        response_json(invalid)
            .await
            .get("cancelCapability")
            .is_none()
    );

    test.workers.block_next_wait();
    test.workers.fail_next_cancel();
    let failed = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId":"publish-restored", "decision":"approve"}]
            }),
        ))
        .await
        .expect("resume failure response");
    assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(failed.headers().get("cache-control").unwrap(), "no-store");
    let failed = response_json(failed).await;
    let recovered_capability = failed["cancelCapability"]
        .as_str()
        .expect("recoverable cancellation capability");
    assert_ne!(recovered_capability, old_cancel_capability);
    assert_eq!(failed["error"]["code"], "backend_error");

    let old_cancel = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": old_cancel_capability}),
        ))
        .await
        .expect("old cancel response");
    assert_eq!(response_json(old_cancel).await["cancelled"], false);
    let recovered = restarted
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": recovered_capability}),
        ))
        .await
        .expect("recovered cancel response");
    assert_eq!(response_json(recovered).await["cancelled"], true);
}

#[tokio::test]
async fn completed_chat_retains_uncertain_worker_for_capability_bound_http_cancel() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "uncertain-result".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("completed despite uncertain tool result"), vec![]),
    ]));
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.uncertain_next_wait();
    let completed = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("chat response");
    assert_eq!(completed.status(), StatusCode::OK);
    let completed = response_json(completed).await;
    assert_eq!(completed["status"], "completed");
    let chat_id = completed["chatId"].as_str().expect("chat ID");
    let capability = completed["cancelCapability"].as_str().expect("capability");

    let denied = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "not-the-capability"}),
        ))
        .await
        .expect("denied cancellation");
    assert_eq!(response_json(denied).await["cancelled"], false);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());
    let cancelled = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("authorized cancellation");
    assert_eq!(response_json(cancelled).await["cancelled"], true);
    assert_eq!(
        test.workers.cancelled.lock().unwrap().as_slice(),
        ["owned-job-1"]
    );
    let replay = test
        .app
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("replay cancellation");
    assert_eq!(response_json(replay).await["cancelled"], false);
}

#[tokio::test]
async fn nonstream_error_after_uncertain_worker_exposes_only_owned_cancel_context() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        Value::Null,
        vec![ToolCall {
            call_id: "uncertain-before-model-failure".to_owned(),
            name: "publish".to_owned(),
            arguments: json!({"message": "release"}),
        }],
    )]));
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.uncertain_next_wait();
    let response = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("chat error response");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let failed = response_json(response).await;
    let chat_id = failed["chatId"].as_str().expect("owned chat ID");
    let capability = failed["cancelCapability"]
        .as_str()
        .expect("owned cancellation capability");
    assert_eq!(failed["error"]["code"], "backend_error");
    assert!(!failed.to_string().contains("no scripted model response"));
    let denied = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "wrong-capability"}),
        ))
        .await
        .expect("denied cancellation");
    assert_eq!(response_json(denied).await["cancelled"], false);
    let cancelled = test
        .app
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("owned cancellation");
    assert_eq!(response_json(cancelled).await["cancelled"], true);
    assert_eq!(
        test.workers.cancelled.lock().unwrap().as_slice(),
        ["owned-job-1"]
    );

    // A failure before enqueue must not mint a spurious capability or chat identifier.
    let directory = tempfile::tempdir().unwrap();
    let no_job = test_app(Arc::new(FakeModel::new([])), directory.path()).await;
    let failure = no_job
        .app
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Hello"}}]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(failure.status(), StatusCode::BAD_GATEWAY);
    let failure = response_json(failure).await;
    assert!(failure.get("chatId").is_none());
    assert!(failure.get("cancelCapability").is_none());
}

#[tokio::test]
async fn sse_completed_chat_retains_uncertain_worker_for_started_cancel_capability() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "uncertain-stream-result".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("completed despite uncertain tool result"), vec![]),
    ]));
    let test = test_app(model, directory.path()).await;
    test.registry
        .register(
            ToolRegistration::new("publish", "Publish", 71, "publish")
                .with_requires_approval(false),
        )
        .await
        .expect("tool registration");
    test.workers.uncertain_next_wait();

    let response = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats/stream",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    let mut events = response.into_body().into_data_stream();
    let started = events
        .next()
        .await
        .expect("started frame")
        .expect("started SSE data");
    let started = String::from_utf8(started.to_vec()).expect("started SSE text");
    let started_data: Value = serde_json::from_str(
        started
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("started event JSON"),
    )
    .expect("started event parses");
    let chat_id = started_data["chatId"].as_str().expect("chat ID");
    let capability = started_data["event"]["cancelCapability"]
        .as_str()
        .expect("started cancellation capability");

    let terminal = tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = String::new();
        while let Some(frame) = events.next().await {
            output.push_str(std::str::from_utf8(&frame.expect("SSE data")).unwrap());
        }
        output
    })
    .await
    .expect("SSE stream finishes");
    assert_eq!(sse_event_count(&terminal, "completed"), 1, "{terminal}");
    assert_eq!(sse_event_count(&terminal, "error"), 0, "{terminal}");

    let denied = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": "not-the-capability"}),
        ))
        .await
        .expect("denied cancellation");
    assert_eq!(response_json(denied).await["cancelled"], false);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());

    let cancelled = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": capability}),
        ))
        .await
        .expect("authorized cancellation");
    assert_eq!(response_json(cancelled).await["cancelled"], true);
    assert_eq!(
        test.workers.cancelled.lock().unwrap().as_slice(),
        ["owned-job-1"]
    );
}

#[tokio::test]
async fn restored_resume_after_uncertain_wait_rotates_cancel_capability() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "publish-restored-uncertain".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("completed after restored uncertain wait"), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(ToolRegistration::new("publish", "Publish", 71, "publish"))
        .await
        .expect("tool registration");

    let initial = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({
                "llmWorkerId": 23,
                "messages": [{"role": "USER", "content": {"text": "Publish this."}}]
            }),
        ))
        .await
        .expect("initial chat response");
    let initial = response_json(initial).await;
    assert_eq!(initial["status"], "approval_required");
    let chat_id = initial["chatId"].as_str().expect("chat ID");
    let resume_capability = initial["resumeCapability"]
        .as_str()
        .expect("resume capability");
    let old_cancel_capability = initial["cancelCapability"]
        .as_str()
        .expect("original cancellation capability");

    // Resume state survives only because the restarted backend uses the factory's shared approval store.
    let resumed_backend = Arc::new(AgentBackend::new(
        test.factory.clone(),
        test.skills.clone(),
        test.registry.clone(),
    ));
    let restarted = router(
        HttpConfig::local_no_token("127.0.0.1:9000".parse().unwrap()),
        resumed_backend,
    )
    .expect("restarted router");
    let invalid = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": "wrong-capability",
                "decisions": [{"callId": "publish-restored-uncertain", "decision": "approve"}]
            }),
        ))
        .await
        .expect("invalid resume response");
    assert_eq!(invalid.status(), StatusCode::BAD_GATEWAY);
    let invalid = response_json(invalid).await;
    assert!(invalid.get("cancelCapability").is_none());
    assert!(test.workers.started.lock().unwrap().is_empty());

    test.workers.uncertain_next_wait();
    let resumed = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": resume_capability,
                "decisions": [{"callId": "publish-restored-uncertain", "decision": "approve"}]
            }),
        ))
        .await
        .expect("valid resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed = response_json(resumed).await;
    assert_eq!(resumed["status"], "completed");
    let new_cancel_capability = resumed["cancelCapability"]
        .as_str()
        .expect("rotated cancellation capability");
    assert_ne!(new_cancel_capability, old_cancel_capability);
    assert_eq!(test.workers.started.lock().unwrap().len(), 1);
    assert_eq!(model.invocations.lock().unwrap().len(), 2);

    let old_cancel = restarted
        .clone()
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": old_cancel_capability}),
        ))
        .await
        .expect("old cancellation response");
    assert_eq!(response_json(old_cancel).await["cancelled"], false);
    assert!(test.workers.cancelled.lock().unwrap().is_empty());

    let new_cancel = restarted
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/cancel"),
            json!({"cancelCapability": new_cancel_capability}),
        ))
        .await
        .expect("new cancellation response");
    assert_eq!(response_json(new_cancel).await["cancelled"], true);
    assert_eq!(
        test.workers.cancelled.lock().unwrap().as_slice(),
        ["owned-job-1"]
    );
}

#[tokio::test]
async fn image_base64_survives_http_model_and_response_history() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([model_response(
        json!("base64 image received"),
        vec![],
    )]));
    let test = test_app(model.clone(), directory.path()).await;
    let content = json!({"image": {"contentType": "image/png", "source": {"base64": "aGVsbG8="}}});
    let response = test
        .app
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({"llmWorkerId": 23, "messages": [{"role": "USER", "content": content}]}),
        ))
        .await
        .expect("chat response");
    assert_eq!(response.status(), StatusCode::OK);
    let response = response_json(response).await;
    assert_eq!(response["status"], "completed");
    assert_eq!(response["messages"][0]["content"], content);
    assert_eq!(
        model.invocations.lock().unwrap()[0].history[1].content,
        content
    );
}

#[tokio::test]
async fn approved_image_chat_preserves_image_in_continuation_and_resume_response() {
    let directory = tempfile::tempdir().expect("temporary root");
    let model = Arc::new(FakeModel::new([
        model_response(
            Value::Null,
            vec![ToolCall {
                call_id: "image-publish".to_owned(),
                name: "publish".to_owned(),
                arguments: json!({"message": "release"}),
            }],
        ),
        model_response(json!("done"), vec![]),
    ]));
    let test = test_app(model.clone(), directory.path()).await;
    test.registry
        .register(ToolRegistration::new("publish", "Publish", 71, "publish"))
        .await
        .expect("tool registration");
    let image = json!({"image": {"contentType": "image/jpeg", "source": {"base64": "aGVsbG8="}}});
    let proposal = test
        .app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/chats",
            json!({"llmWorkerId": 23, "messages": [{"role": "USER", "content": image}]}),
        ))
        .await
        .expect("approval response");
    assert_eq!(proposal.status(), StatusCode::OK);
    let proposal = response_json(proposal).await;
    assert_eq!(proposal["status"], "approval_required");
    assert_eq!(proposal["messages"][0]["content"], image);
    let chat_id = proposal["chatId"].as_str().unwrap();
    let capability = proposal["resumeCapability"].as_str().unwrap();
    let resumed = test
        .app
        .oneshot(request(
            "POST",
            &format!("/v1/chats/{chat_id}/resume"),
            json!({
                "resumeCapability": capability,
                "decisions": [{"callId": "image-publish", "decision": "approve"}]
            }),
        ))
        .await
        .expect("resume response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed = response_json(resumed).await;
    assert_eq!(resumed["status"], "completed");
    assert_eq!(resumed["messages"][0]["content"], image);
    assert_eq!(
        model.invocations.lock().unwrap()[1].history[1].content,
        image
    );
}
