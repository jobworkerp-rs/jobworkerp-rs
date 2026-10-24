use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_server::{
    approval::{
        MemoryPendingApprovalStore, PendingApproval, PendingApprovalStore,
        PendingApprovalStoreError,
    },
    backend::{AgentBackend, ChatOrchestratorFactory},
    chat::{
        ChatConfig, ChatOrchestrator, ChatToolRegistry, ModelInvocation, ModelInvoker,
        ModelResponse, RegisteredTool, RegistryError, ToolCall, WorkerError, WorkerExecutor,
        WorkerJob, WorkerWaitError,
    },
    http::{
        ApprovalDecision, CancelRequest, ChatContent, ChatMessage, ChatRequest, ChatStatus,
        ChatStreamEvent, HttpBackend, MessageRole, ResumeRequest, TextContent, ToolDecision,
    },
    skills::SkillCatalog,
    tool_registry::{
        ResolvedInputSchema, SchemaResolutionError, ToolRegistry, WorkerSchemaResolver,
    },
};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Notify;

const APPROVAL_TTL: Duration = Duration::from_millis(200);

struct GatedApprovalStore {
    inner: MemoryPendingApprovalStore,
    block_next_claim: AtomicBool,
    claim_started: Notify,
    release_claim: Notify,
}

impl GatedApprovalStore {
    fn new(ttl: Duration) -> Self {
        Self {
            inner: MemoryPendingApprovalStore::new(ttl),
            block_next_claim: AtomicBool::new(false),
            claim_started: Notify::new(),
            release_claim: Notify::new(),
        }
    }
}

#[async_trait]
impl PendingApprovalStore for GatedApprovalStore {
    async fn save(&self, approval: PendingApproval) -> Result<(), PendingApprovalStoreError> {
        self.inner.save(approval).await
    }

    async fn claim(
        &self,
        chat_id: &str,
        call_id: &str,
    ) -> Result<Option<PendingApproval>, PendingApprovalStoreError> {
        if self.block_next_claim.swap(false, Ordering::SeqCst) {
            self.claim_started.notify_one();
            self.release_claim.notified().await;
        }
        self.inner.claim(chat_id, call_id).await
    }
}

struct ScriptedModel {
    responses: Mutex<VecDeque<ModelResponse>>,
    invocations: AtomicUsize,
    block_next_invoke: AtomicBool,
    invoke_started: Notify,
    release_invoke: Notify,
}

impl ScriptedModel {
    fn new(responses: impl IntoIterator<Item = ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            invocations: AtomicUsize::new(0),
            block_next_invoke: AtomicBool::new(false),
            invoke_started: Notify::new(),
            release_invoke: Notify::new(),
        }
    }
}

#[async_trait]
impl ModelInvoker for ScriptedModel {
    async fn invoke(&self, _: ModelInvocation) -> Result<ModelResponse, String> {
        self.invocations.fetch_add(1, Ordering::SeqCst);
        if self.block_next_invoke.swap(false, Ordering::SeqCst) {
            self.invoke_started.notify_one();
            self.release_invoke.notified().await;
        }
        self.responses
            .lock()
            .expect("model response lock")
            .pop_front()
            .ok_or_else(|| "no scripted model response".to_owned())
    }
}

struct FixedToolRegistry {
    tools: Vec<RegisteredTool>,
}

impl FixedToolRegistry {
    fn with_approval(requires_approval: bool) -> Self {
        let mut tools = vec![registered_tool("publish"), registered_tool("review")];
        for tool in &mut tools {
            tool.requires_approval = requires_approval;
        }
        Self { tools }
    }
}

#[async_trait]
impl ChatToolRegistry for FixedToolRegistry {
    async fn list_tools(&self) -> Result<Vec<RegisteredTool>, RegistryError> {
        Ok(self.tools.clone())
    }

    async fn current_tool(&self, name: &str) -> Result<Option<RegisteredTool>, RegistryError> {
        Ok(self.tools.iter().find(|tool| tool.name == name).cloned())
    }
}

fn registered_tool(name: &str) -> RegisteredTool {
    RegisteredTool {
        name: name.to_owned(),
        description: format!("Run {name}"),
        worker_id: 7,
        method: name.to_owned(),
        requires_approval: true,
        schema_revision: "1".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
            "additionalProperties": false
        }),
    }
}

#[derive(Default)]
struct TestWorkers {
    starts: AtomicUsize,
    cancellations: AtomicUsize,
    block_next_wait: AtomicBool,
    uncertain_next_wait: AtomicBool,
    wait_started: Notify,
    release_wait: Notify,
}

#[async_trait]
impl WorkerExecutor for TestWorkers {
    async fn start(&self, _: &RegisteredTool, _: Value) -> Result<WorkerJob, WorkerError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(WorkerJob {
            job_id: "owned-job".to_owned(),
        })
    }

    async fn wait(&self, job_id: &str) -> Result<Value, WorkerError> {
        assert_eq!(job_id, "owned-job");
        if self.block_next_wait.swap(false, Ordering::SeqCst) {
            self.wait_started.notify_one();
            self.release_wait.notified().await;
        }
        Ok(json!({"published": true}))
    }

    async fn wait_with_ownership(&self, job_id: &str) -> Result<Value, WorkerWaitError> {
        if self.uncertain_next_wait.swap(false, Ordering::SeqCst) {
            return Err(WorkerWaitError::Uncertain(WorkerError(
                "result stream was interrupted".to_owned(),
            )));
        }
        self.wait(job_id).await.map_err(WorkerWaitError::Terminal)
    }

    async fn cancel(&self, job_id: &str) -> Result<(), WorkerError> {
        assert_eq!(job_id, "owned-job");
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        self.release_wait.notify_one();
        Ok(())
    }
}

struct TestFactory {
    model: Arc<ScriptedModel>,
    registry: Arc<dyn ChatToolRegistry>,
    workers: Arc<TestWorkers>,
    approvals: Arc<GatedApprovalStore>,
    approval_ttl: Duration,
    orchestrators: Mutex<Vec<Weak<ChatOrchestrator>>>,
    resume_creations: AtomicUsize,
}

impl TestFactory {
    fn build_orchestrator(&self) -> Arc<ChatOrchestrator> {
        let orchestrator = Arc::new(ChatOrchestrator::new(
            self.model.clone(),
            Arc::new(SkillCatalog::new(Vec::new())),
            self.registry.clone(),
            self.workers.clone(),
            self.approvals.clone(),
            ChatConfig {
                approval_ttl: self.approval_ttl,
                ..ChatConfig::default()
            },
        ));
        self.orchestrators
            .lock()
            .expect("orchestrator weak-reference lock")
            .push(Arc::downgrade(&orchestrator));
        orchestrator
    }

    fn first_orchestrator(&self) -> Weak<ChatOrchestrator> {
        self.orchestrators
            .lock()
            .expect("orchestrator weak-reference lock")
            .first()
            .expect("chat orchestrator was created")
            .clone()
    }

    fn latest_orchestrator(&self) -> Weak<ChatOrchestrator> {
        self.orchestrators
            .lock()
            .expect("orchestrator weak-reference lock")
            .last()
            .expect("chat orchestrator was created")
            .clone()
    }
}

#[async_trait]
impl ChatOrchestratorFactory for TestFactory {
    async fn create(
        &self,
        _: i64,
        _: Option<agent_server::http::ChatOptions>,
    ) -> Result<Arc<ChatOrchestrator>, String> {
        Ok(self.build_orchestrator())
    }

    async fn create_for_resume(&self) -> Result<Arc<ChatOrchestrator>, String> {
        self.resume_creations.fetch_add(1, Ordering::SeqCst);
        Ok(self.build_orchestrator())
    }
}

struct EmptySchemaResolver;

#[async_trait]
impl WorkerSchemaResolver for EmptySchemaResolver {
    async fn resolve_input_schema(
        &self,
        _: i64,
        _: &str,
    ) -> Result<ResolvedInputSchema, SchemaResolutionError> {
        Ok(ResolvedInputSchema {
            schema: json!({"type": "object"}),
            revision: "1".to_owned(),
        })
    }
}

struct TestSetup {
    backend: Arc<AgentBackend>,
    factory: Arc<TestFactory>,
    workers: Arc<TestWorkers>,
    _directory: tempfile::TempDir,
}

async fn setup(ttl: Duration, responses: Vec<ModelResponse>) -> TestSetup {
    setup_with_ttls(ttl, ttl, responses).await
}

async fn setup_with_ttls(
    session_ttl: Duration,
    store_ttl: Duration,
    responses: Vec<ModelResponse>,
) -> TestSetup {
    setup_with_ttls_and_approval(session_ttl, store_ttl, responses, true).await
}

async fn setup_with_ttls_and_approval(
    session_ttl: Duration,
    store_ttl: Duration,
    responses: Vec<ModelResponse>,
    requires_approval: bool,
) -> TestSetup {
    let directory = tempfile::tempdir().expect("test directory");
    let tools = Arc::new(
        ToolRegistry::open(
            directory.path().join("tools.json"),
            Arc::new(EmptySchemaResolver),
        )
        .await
        .expect("empty tool registry opens"),
    );
    let workers = Arc::new(TestWorkers::default());
    let factory = Arc::new(TestFactory {
        model: Arc::new(ScriptedModel::new(responses)),
        registry: Arc::new(FixedToolRegistry::with_approval(requires_approval)),
        workers: workers.clone(),
        approvals: Arc::new(GatedApprovalStore::new(store_ttl)),
        approval_ttl: store_ttl,
        orchestrators: Mutex::new(Vec::new()),
        resume_creations: AtomicUsize::new(0),
    });
    let backend = Arc::new(AgentBackend::with_approval_ttl(
        factory.clone(),
        Arc::new(SkillCatalog::new(Vec::new())),
        tools,
        session_ttl,
    ));
    TestSetup {
        backend,
        factory,
        workers,
        _directory: directory,
    }
}

fn tool_call(call_id: &str, name: &str) -> ToolCall {
    ToolCall {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        arguments: json!({"message": "hello"}),
    }
}

fn model_response(tool_calls: Vec<ToolCall>) -> ModelResponse {
    ModelResponse {
        content: Value::Null,
        tool_calls,
    }
}

fn text_response(text: &str) -> ModelResponse {
    ModelResponse {
        content: json!(text),
        tool_calls: Vec::new(),
    }
}

fn chat_request() -> ChatRequest {
    ChatRequest {
        llm_worker_id: 1,
        messages: vec![ChatMessage {
            role: MessageRole::User,
            content: ChatContent::Text(TextContent {
                text: "hello".to_owned(),
            }),
            tool_call_id: None,
        }],
        options: None,
        stream: None,
    }
}

fn approval_resume(response: &agent_server::http::ChatResponse) -> ResumeRequest {
    ResumeRequest {
        resume_capability: response
            .resume_capability
            .clone()
            .expect("approval response has a resume capability"),
        decisions: vec![ToolDecision {
            call_id: response
                .pending_calls
                .first()
                .expect("approval response has a pending call")
                .call_id
                .clone(),
            decision: ApprovalDecision::Approve,
        }],
    }
}

async fn wait_until_released(orchestrator: &Weak<ChatOrchestrator>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if orchestrator.upgrade().is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("idle approval orchestrator is periodically released");
}

async fn wait_for_cancellations(workers: &TestWorkers, expected: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while workers.cancellations.load(Ordering::SeqCst) < expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("owned Worker job is cancelled after request cancellation");
}

#[tokio::test]
async fn idle_approval_session_is_swept_without_another_request() {
    let setup = setup(
        APPROVAL_TTL,
        vec![model_response(vec![tool_call("call-1", "publish")])],
    )
    .await;
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    assert_eq!(response.status, ChatStatus::ApprovalRequired);
    let orchestrator = setup.factory.first_orchestrator();

    wait_until_released(&orchestrator).await;
}

#[tokio::test]
async fn approval_can_be_resumed_before_expiry() {
    let setup = setup(
        Duration::from_secs(2),
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
    )
    .await;
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");

    let resumed = setup
        .backend
        .resume_chat(response.chat_id.clone(), approval_resume(&response))
        .await
        .expect("pending approval resumes before expiry");

    assert_eq!(resumed.status, ChatStatus::Completed);
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 1);
    assert_eq!(setup.factory.resume_creations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn expired_approval_is_removed_and_cannot_execute() {
    let setup = setup(
        APPROVAL_TTL,
        vec![model_response(vec![tool_call("call-1", "publish")])],
    )
    .await;
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let orchestrator = setup.factory.first_orchestrator();
    wait_until_released(&orchestrator).await;

    let error = setup
        .backend
        .resume_chat(response.chat_id.clone(), approval_resume(&response))
        .await
        .expect_err("expired persisted approval is not executable");

    assert!(!error.to_string().is_empty());
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 0);
    assert_eq!(setup.factory.model.invocations.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn expired_in_memory_session_restores_a_still_valid_persisted_approval() {
    let setup = setup_with_ttls(
        Duration::from_millis(80),
        Duration::from_secs(1),
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
    )
    .await;
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let orchestrator = setup.factory.first_orchestrator();
    wait_until_released(&orchestrator).await;

    let resumed = setup
        .backend
        .resume_chat(response.chat_id.clone(), approval_resume(&response))
        .await
        .expect("expired cache entry restores unexpired persisted state");

    assert_eq!(resumed.status, ChatStatus::Completed);
    assert_eq!(setup.factory.resume_creations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn streamed_approval_expires_from_memory_and_restores_on_resume() {
    let setup = setup_with_ttls(
        Duration::from_millis(80),
        Duration::from_secs(3),
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
    )
    .await;
    let mut stream = setup
        .backend
        .stream_chat(chat_request())
        .await
        .expect("stream starts");
    let chat_id = stream.chat_id.clone();
    let mut terminal_count = 0;
    let mut resume_request = None;
    while let Some(event) = stream.events.next().await {
        match event.expect("stream event") {
            ChatStreamEvent::ApprovalRequired {
                pending_calls,
                resume_capability,
            } => {
                terminal_count += 1;
                resume_request = Some(ResumeRequest {
                    resume_capability,
                    decisions: vec![ToolDecision {
                        call_id: pending_calls
                            .first()
                            .expect("approval has a pending call")
                            .call_id
                            .clone(),
                        decision: ApprovalDecision::Approve,
                    }],
                });
            }
            ChatStreamEvent::Completed { .. } | ChatStreamEvent::Error { .. } => {
                panic!("stream ends with an approval-required event");
            }
            ChatStreamEvent::Started { .. }
            | ChatStreamEvent::TextDelta { .. }
            | ChatStreamEvent::ToolCall { .. }
            | ChatStreamEvent::ToolResult { .. } => {}
        }
    }
    assert_eq!(terminal_count, 1, "stream has one terminal event");
    let orchestrator = setup.factory.first_orchestrator();

    wait_until_released(&orchestrator).await;
    let resumed = setup
        .backend
        .resume_chat(
            chat_id,
            resume_request.expect("stream exposes an approval to resume"),
        )
        .await
        .expect("persisted approval remains resumable after cache expiry");

    assert_eq!(resumed.status, ChatStatus::Completed);
    assert_eq!(setup.factory.resume_creations.load(Ordering::SeqCst), 1);
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn resumed_approval_refreshes_the_session_ttl() {
    let ttl = Duration::from_millis(600);
    let setup = setup(
        ttl,
        vec![
            model_response(vec![
                tool_call("call-1", "publish"),
                tool_call("call-2", "review"),
            ]),
            text_response("done"),
        ],
    )
    .await;
    let first = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("first approval response");
    let orchestrator = setup.factory.first_orchestrator();
    tokio::time::sleep(Duration::from_millis(120)).await;

    let second = setup
        .backend
        .resume_chat(first.chat_id.clone(), approval_resume(&first))
        .await
        .expect("first approval resumes to a second approval");
    assert_eq!(second.status, ChatStatus::ApprovalRequired);

    tokio::time::sleep(Duration::from_millis(520)).await;
    assert!(
        orchestrator.upgrade().is_some(),
        "the second approval refreshes the in-memory session deadline"
    );
    let completed = setup
        .backend
        .resume_chat(second.chat_id.clone(), approval_resume(&second))
        .await
        .expect("refreshed approval remains resumable");

    assert_eq!(completed.status, ChatStatus::Completed);
    assert_eq!(setup.factory.resume_creations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn active_worker_ownership_is_not_swept_after_approval_ttl() {
    let setup = setup(
        APPROVAL_TTL,
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
    )
    .await;
    setup.workers.block_next_wait.store(true, Ordering::SeqCst);
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let orchestrator = setup.factory.first_orchestrator();
    let wait_started = setup.workers.wait_started.notified();
    let chat_id = response.chat_id.clone();
    let cancellation_chat_id = chat_id.clone();
    let cancel_capability = response
        .cancel_capability
        .clone()
        .expect("approval response has a cancel capability");
    let backend = setup.backend.clone();
    let resume = tokio::spawn(async move {
        backend
            .resume_chat(chat_id, approval_resume(&response))
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), wait_started)
        .await
        .expect("resumed worker enters its wait");

    tokio::time::sleep(APPROVAL_TTL + Duration::from_millis(100)).await;
    assert!(orchestrator.upgrade().is_some());
    let cancelled = setup
        .backend
        .cancel_chat(cancellation_chat_id, CancelRequest { cancel_capability })
        .await
        .expect("worker job remains cancelable");
    assert!(cancelled.cancelled);
    let _ = resume.await;
}

#[tokio::test]
async fn uncertain_worker_remains_cancelable_after_approval_ttl() {
    let setup = setup(
        APPROVAL_TTL,
        vec![model_response(vec![tool_call("call-1", "publish")])],
    )
    .await;
    setup
        .workers
        .uncertain_next_wait
        .store(true, Ordering::SeqCst);
    let response = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let orchestrator = setup.factory.first_orchestrator();
    let error = setup
        .backend
        .resume_chat(response.chat_id.clone(), approval_resume(&response))
        .await
        .expect_err("an uncertain result leaves an owned job to cancel");
    assert!(!error.to_string().is_empty());

    tokio::time::sleep(APPROVAL_TTL + Duration::from_millis(100)).await;
    assert!(orchestrator.upgrade().is_some());
    let cancelled = setup
        .backend
        .cancel_chat(
            response.chat_id,
            CancelRequest {
                cancel_capability: response
                    .cancel_capability
                    .expect("approval response has a cancel capability"),
            },
        )
        .await
        .expect("uncertain worker remains cancelable");

    assert!(cancelled.cancelled);
}

#[tokio::test]
async fn dropping_backend_does_not_leave_the_sweeper_holding_sessions() {
    let setup = setup(
        APPROVAL_TTL,
        vec![model_response(vec![tool_call("call-1", "publish")])],
    )
    .await;
    setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let orchestrator = setup.factory.first_orchestrator();
    assert!(orchestrator.upgrade().is_some());

    drop(setup.backend);

    wait_until_released(&orchestrator).await;
}

#[tokio::test]
async fn aborting_nonstream_chat_cancels_started_worker_and_releases_session() {
    let setup = setup_with_ttls_and_approval(
        APPROVAL_TTL,
        APPROVAL_TTL,
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
        false,
    )
    .await;
    setup.workers.block_next_wait.store(true, Ordering::SeqCst);
    let wait_started = setup.workers.wait_started.notified();
    let backend = setup.backend.clone();
    let chat = tokio::spawn(async move { backend.start_chat(chat_request()).await });

    tokio::time::timeout(Duration::from_secs(1), wait_started)
        .await
        .expect("nonstream chat starts and waits for its owned Worker job");
    let orchestrator = setup.factory.first_orchestrator();
    chat.abort();
    assert!(
        chat.await
            .expect_err("chat task was aborted")
            .is_cancelled()
    );

    wait_for_cancellations(&setup.workers, 1).await;
    wait_until_released(&orchestrator).await;
}

#[tokio::test]
async fn aborting_chat_before_any_job_does_not_leave_a_prepared_session() {
    let setup = setup(APPROVAL_TTL, vec![text_response("never returned")]).await;
    setup
        .factory
        .model
        .block_next_invoke
        .store(true, Ordering::SeqCst);
    let invoke_started = setup.factory.model.invoke_started.notified();
    let backend = setup.backend.clone();
    let chat = tokio::spawn(async move { backend.start_chat(chat_request()).await });

    tokio::time::timeout(Duration::from_secs(1), invoke_started)
        .await
        .expect("model invocation started before any job ID");
    let orchestrator = setup.factory.first_orchestrator();
    chat.abort();
    assert!(chat.await.expect_err("chat aborted").is_cancelled());
    wait_until_released(&orchestrator).await;
    assert_eq!(setup.workers.cancellations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn concurrent_resume_cannot_claim_approval_until_aborted_predecessor_releases_lease() {
    let setup = setup_with_ttls(
        Duration::from_secs(3),
        Duration::from_secs(3),
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("continued"),
        ],
    )
    .await;
    let approval = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("approval issued");
    let original = setup.factory.first_orchestrator();
    setup
        .factory
        .approvals
        .block_next_claim
        .store(true, Ordering::SeqCst);
    let claim_started = setup.factory.approvals.claim_started.notified();
    let backend = setup.backend.clone();
    let chat_id = approval.chat_id.clone();
    let resume_request = approval_resume(&approval);
    let interrupted =
        tokio::spawn(async move { backend.resume_chat(chat_id, resume_request).await });
    tokio::time::timeout(Duration::from_secs(1), claim_started)
        .await
        .expect("first resume holds the session before claiming approval");

    // The second request cannot begin using the same session while the first is in flight.
    assert!(
        setup
            .backend
            .resume_chat(approval.chat_id.clone(), approval_resume(&approval))
            .await
            .is_err()
    );
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 0);
    interrupted.abort();
    assert!(
        interrupted
            .await
            .expect_err("first resume aborted")
            .is_cancelled()
    );

    // Aborting before the claim cannot invalidate a later, valid use of the persisted approval.
    let resumed = setup
        .backend
        .resume_chat(approval.chat_id.clone(), approval_resume(&approval))
        .await
        .expect("second resume claims original approval");
    assert_eq!(resumed.status, ChatStatus::Completed);
    assert_eq!(setup.workers.starts.load(Ordering::SeqCst), 1);
    assert_eq!(setup.workers.cancellations.load(Ordering::SeqCst), 0);
    wait_until_released(&original).await;
}

#[tokio::test]
async fn aborting_restored_resume_cancels_started_worker_and_releases_session() {
    let setup = setup_with_ttls(
        Duration::from_millis(80),
        Duration::from_secs(3),
        vec![
            model_response(vec![tool_call("call-1", "publish")]),
            text_response("done"),
        ],
    )
    .await;
    let approval = setup
        .backend
        .start_chat(chat_request())
        .await
        .expect("chat returns approval required");
    let original_orchestrator = setup.factory.first_orchestrator();
    wait_until_released(&original_orchestrator).await;

    setup.workers.block_next_wait.store(true, Ordering::SeqCst);
    let wait_started = setup.workers.wait_started.notified();
    let backend = setup.backend.clone();
    let chat_id = approval.chat_id.clone();
    let resume = approval_resume(&approval);
    let resumed = tokio::spawn(async move { backend.resume_chat(chat_id, resume).await });

    tokio::time::timeout(Duration::from_secs(1), wait_started)
        .await
        .expect("restored resume starts and waits for its owned Worker job");
    let restored_orchestrator = setup.factory.latest_orchestrator();
    resumed.abort();
    assert!(
        resumed
            .await
            .expect_err("resume task was aborted")
            .is_cancelled()
    );

    wait_for_cancellations(&setup.workers, 1).await;
    wait_until_released(&restored_orchestrator).await;
}
