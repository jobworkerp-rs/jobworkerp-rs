use std::{collections::HashMap, path::Path, sync::Arc};

use agent_server::chat::ModelJobObserver;
use agent_server::{
    approval::MemoryPendingApprovalStore,
    chat::{
        ChatConfig, ChatMessage, ChatOrchestrator, ChatRequest, ChatToolRegistry, ModelInvocation,
        ModelInvoker, ModelOptions, RegisteredTool, RegistryError, Role, WorkerError,
        WorkerExecutor, WorkerJob,
    },
    config::AgentServerConfig,
    grpc::{
        JobworkerpRpc, OutputChunk, OutputKind, ResultEnqueueResponse, StreamEnqueueResponse,
        ToolExecutionResult, ToolExecutor, ToolInvocation,
        proto::jobworkerp::{
            data::{JobId, MethodProtoMap, MethodSchema, RunnerData, RunnerId, WorkerData},
            service::{CreateJobResponse, JobRequest},
        },
    },
    model::{GrpcModelInvoker, ModelTextDeltaCallback},
    skills::SkillCatalog,
};
use async_trait::async_trait;
use axum::{
    body::{Body as AxumBody, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Mutex;
use tonic::Status;
use tonic::{
    body::Body as GrpcBody,
    codegen::{Service, http as tonic_http},
    transport::Server,
};
use tower::ServiceExt;

#[path = "../src/main.rs"]
#[allow(dead_code)]
mod executable;

struct StartupRpc;

#[async_trait]
impl JobworkerpRpc for StartupRpc {
    async fn find_worker(&self, _: i64) -> Result<Option<WorkerData>, Status> {
        Ok(Some(WorkerData {
            runner_id: Some(RunnerId { value: 2 }),
            ..Default::default()
        }))
    }

    async fn find_runner(&self, _: i64) -> Result<Option<RunnerData>, Status> {
        Ok(Some(RunnerData {
            method_proto_map: Some(MethodProtoMap {
                schemas: HashMap::from([(
                    "publish".to_owned(),
                    MethodSchema {
                        args_proto: "syntax = \"proto3\"; message Args { string text = 1; }"
                            .to_owned(),
                        result_proto: "syntax = \"proto3\"; message Reply { string answer = 1; }"
                            .to_owned(),
                        ..Default::default()
                    },
                )]),
            }),
            ..Default::default()
        }))
    }

    async fn enqueue(&self, _: JobRequest) -> Result<CreateJobResponse, Status> {
        Err(Status::unimplemented("not used by this startup test"))
    }

    async fn enqueue_for_stream(&self, _: JobRequest) -> Result<StreamEnqueueResponse, Status> {
        Err(Status::unimplemented("not used by this startup test"))
    }

    async fn enqueue_for_result(&self, _: JobRequest) -> Result<ResultEnqueueResponse, Status> {
        Err(Status::unimplemented("not used by this startup test"))
    }

    async fn delete(&self, _: JobId) -> Result<(), Status> {
        Err(Status::unimplemented("not used by this startup test"))
    }
}

#[derive(Default)]
struct RecordingExecutor {
    worker_ids: Mutex<Vec<i64>>,
}

#[derive(Clone)]
struct ProtectedGrpcState {
    expected_token: String,
    calls: Arc<Mutex<Vec<RecordedGrpcCall>>>,
}

type RecordedGrpcCall = (String, Option<String>, &'static str);

fn protected_response(
    request: tonic_http::Request<GrpcBody>,
    state: &ProtectedGrpcState,
) -> tonic_http::Response<GrpcBody> {
    let path = request.uri().path().to_owned();
    let token = request
        .headers()
        .get("jobworkerp-auth")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let authenticated = token.as_deref() == Some(state.expected_token.as_str());
    let grpc_status = if authenticated { "12" } else { "16" };
    state.calls.lock().unwrap().push((path, token, grpc_status));

    tonic_http::Response::builder()
        .status(200)
        .header("content-type", "application/grpc")
        .header("grpc-status", grpc_status)
        .body(GrpcBody::empty())
        .expect("protected fake gRPC response builds")
}

macro_rules! protected_grpc_service {
    ($service:ident, $name:literal) => {
        #[derive(Clone)]
        struct $service(ProtectedGrpcState);

        impl tonic::server::NamedService for $service {
            const NAME: &'static str = $name;
        }

        impl Service<tonic_http::Request<GrpcBody>> for $service {
            type Response = tonic_http::Response<GrpcBody>;
            type Error = std::convert::Infallible;
            type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

            fn poll_ready(
                &mut self,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }

            fn call(&mut self, request: tonic_http::Request<GrpcBody>) -> Self::Future {
                std::future::ready(Ok(protected_response(request, &self.0)))
            }
        }
    };
}

protected_grpc_service!(ProtectedWorkerService, "jobworkerp.service.WorkerService");
protected_grpc_service!(ProtectedRunnerService, "jobworkerp.service.RunnerService");
protected_grpc_service!(ProtectedJobService, "jobworkerp.service.JobService");

#[async_trait]
impl ToolExecutor for RecordingExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> anyhow::Result<ToolExecutionResult> {
        self.worker_ids.lock().unwrap().push(invocation.worker_id);
        Ok(ToolExecutionResult {
            worker_id: invocation.worker_id,
            using: "run".to_owned(),
            job_id: 1,
            result: Some(json!({"content": {"text": "model answer"}, "done": true})),
            raw_result: None,
            chunks: Vec::new(),
        })
    }
}

#[derive(Default)]
struct RecordingModelObserver {
    started: Mutex<Vec<i64>>,
    finished: Mutex<Vec<i64>>,
}

#[async_trait]
impl ModelJobObserver for RecordingModelObserver {
    async fn job_started(&self, job_id: i64) -> Result<(), String> {
        self.started.lock().unwrap().push(job_id);
        Ok(())
    }

    fn job_finished(&self, job_id: i64) {
        self.finished.lock().unwrap().push(job_id);
    }
}

#[derive(Default)]
struct BlockingObserverExecutor {
    invocations: Mutex<Vec<ToolInvocation>>,
    cancelled_job_ids: Mutex<Vec<String>>,
    started: tokio::sync::Notify,
    cancelled: tokio::sync::Notify,
}

struct EmptyChatRegistry;

#[async_trait]
impl ChatToolRegistry for EmptyChatRegistry {
    async fn list_tools(&self) -> Result<Vec<RegisteredTool>, RegistryError> {
        Ok(Vec::new())
    }

    async fn current_tool(&self, _name: &str) -> Result<Option<RegisteredTool>, RegistryError> {
        Ok(None)
    }
}

struct UnusedWorkerExecutor;

#[async_trait]
impl WorkerExecutor for UnusedWorkerExecutor {
    async fn start(
        &self,
        _tool: &RegisteredTool,
        _arguments: Value,
    ) -> Result<WorkerJob, WorkerError> {
        Err(WorkerError(
            "no tools are available in this test".to_owned(),
        ))
    }

    async fn wait(&self, _job_id: &str) -> Result<Value, WorkerError> {
        Err(WorkerError(
            "no tool job is expected in this test".to_owned(),
        ))
    }

    async fn cancel(&self, _job_id: &str) -> Result<(), WorkerError> {
        Ok(())
    }
}

#[async_trait]
impl ToolExecutor for BlockingObserverExecutor {
    async fn execute(&self, invocation: ToolInvocation) -> anyhow::Result<ToolExecutionResult> {
        self.invocations.lock().unwrap().push(invocation.clone());
        Ok(ToolExecutionResult {
            worker_id: invocation.worker_id,
            using: "run".to_owned(),
            job_id: 91,
            result: None,
            raw_result: None,
            chunks: vec![
                OutputChunk {
                    kind: OutputKind::Data,
                    bytes: Vec::new(),
                    json: Some(json!({
                        "content": {"text": "streamed answer"},
                        "done": false,
                    })),
                },
                OutputChunk {
                    kind: OutputKind::FinalCollected,
                    bytes: Vec::new(),
                    json: Some(json!({
                        "content": {"text": "streamed answer"},
                        "done": true,
                    })),
                },
            ],
        })
    }

    async fn execute_with_observers(
        &self,
        invocation: ToolInvocation,
        job_observer: Option<Arc<dyn ModelJobObserver>>,
        chunk_observer: Option<agent_server::grpc::StreamChunkObserver>,
    ) -> anyhow::Result<ToolExecutionResult> {
        let execution = self.execute(invocation).await?;
        if let Some(observer) = job_observer.as_ref() {
            observer
                .job_started(execution.job_id)
                .await
                .map_err(anyhow::Error::msg)?;
        }
        if let Some(observer) = chunk_observer {
            for chunk in &execution.chunks {
                if chunk.kind == OutputKind::Data {
                    observer(chunk).map_err(anyhow::Error::msg)?;
                }
            }
        }
        self.started.notify_one();
        self.cancelled.notified().await;
        Err(anyhow::anyhow!("fake model job was cancelled"))
    }

    async fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        self.cancelled_job_ids
            .lock()
            .unwrap()
            .push(job_id.to_owned());
        self.cancelled.notify_one();
        Ok(())
    }
}

fn environment(skills_root: &Path, registry_path: &Path) -> HashMap<String, String> {
    HashMap::from([
        ("AGENT_SERVER_ADDR".to_owned(), "127.0.0.1:0".to_owned()),
        (
            "AGENT_SERVER_GRPC_ENDPOINT".to_owned(),
            "http://127.0.0.1:9000".to_owned(),
        ),
        (
            "AGENT_SERVER_AUTH_MODE".to_owned(),
            "local-shared-token".to_owned(),
        ),
        (
            "AGENT_SERVER_TOKEN".to_owned(),
            "startup-test-secret".to_owned(),
        ),
        (
            "AGENT_SERVER_SKILLS_ROOTS".to_owned(),
            skills_root.display().to_string(),
        ),
        (
            "AGENT_SERVER_TOOL_REGISTRY_PATH".to_owned(),
            registry_path.display().to_string(),
        ),
    ])
}

fn request(method: &str, uri: &str, token: Option<&str>) -> Request<AxumBody> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:0")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(AxumBody::from("{}")).expect("request builds")
}

async fn response_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body reads");
    serde_json::from_slice(&bytes).expect("response is JSON")
}

#[tokio::test]
async fn startup_loads_skills_and_admin_reload_publishes_updates_behind_auth() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let skills_root = directory.path().join("skills");
    let skill_dir = skills_root.join("release-notes");
    std::fs::create_dir_all(&skill_dir).expect("skill directory");
    let skill_file = skill_dir.join("SKILL.md");
    std::fs::write(
        &skill_file,
        "---\nname: release-notes\ndescription: Draft release notes\n---\nInitial guidance.\n",
    )
    .expect("initial skill content");
    let registry_path = directory.path().join("tools.json");
    std::fs::write(
        &registry_path,
        r#"{"version":1,"tools":[{"name":"publish","description":"Publish a message","workerId":7,"using":"publish","requiresApproval":true}]}"#,
    )
    .expect("persisted tool registration");
    let config = AgentServerConfig::parse(&environment(&skills_root, &registry_path))
        .expect("valid bootstrap config");

    let (_, app) = executable::build_application(config, Arc::new(StartupRpc))
        .await
        .expect("application bootstraps");

    let unauthorized = app
        .clone()
        .oneshot(request("GET", "/v1/skills", None))
        .await
        .expect("unauthorized skills response");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let loaded = app
        .clone()
        .oneshot(request(
            "GET",
            "/v1/skills/release-notes",
            Some("startup-test-secret"),
        ))
        .await
        .expect("initial skill response");
    assert_eq!(loaded.status(), StatusCode::OK);
    assert_eq!(response_json(loaded).await["body"], "Initial guidance.\n");

    let tools = app
        .clone()
        .oneshot(request("GET", "/v1/tools", Some("startup-test-secret")))
        .await
        .expect("persisted tool registry response");
    assert_eq!(tools.status(), StatusCode::OK);
    assert_eq!(response_json(tools).await["tools"][0]["name"], "publish");

    std::fs::write(
        &skill_file,
        "---\nname: release-notes\ndescription: Draft release notes\n---\nReloaded guidance.\n",
    )
    .expect("updated skill content");
    let reloaded = app
        .clone()
        .oneshot(request(
            "POST",
            "/v1/skills/reload",
            Some("startup-test-secret"),
        ))
        .await
        .expect("reload response");
    assert_eq!(reloaded.status(), StatusCode::OK);
    assert_eq!(response_json(reloaded).await["loadedCount"], 1);

    let updated = app
        .oneshot(request(
            "GET",
            "/v1/skills/release-notes",
            Some("startup-test-secret"),
        ))
        .await
        .expect("reloaded skill response");
    assert_eq!(response_json(updated).await["body"], "Reloaded guidance.\n");
}

#[tokio::test]
async fn startup_reports_registry_load_failures_without_echoing_file_contents() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let skills_root = directory.path().join("skills");
    std::fs::create_dir_all(&skills_root).expect("skills root");
    let registry_path = directory.path().join("tools.json");
    std::fs::write(&registry_path, "private-registry-payload").expect("invalid registry file");
    let config = AgentServerConfig::parse(&environment(&skills_root, &registry_path))
        .expect("valid bootstrap config");

    let error = match executable::build_application(config, Arc::new(StartupRpc)).await {
        Err(error) => error,
        Ok(_) => panic!("an invalid registry must prevent startup"),
    };
    assert!(!error.to_string().contains("private-registry-payload"));
}

#[tokio::test]
async fn unavailable_skill_root_diagnostics_do_not_block_startup() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let missing_skills_root = directory.path().join("missing-skills");
    let config = AgentServerConfig::parse(&environment(
        &missing_skills_root,
        &directory.path().join("tools.json"),
    ))
    .expect("valid bootstrap config");

    let (_, app) = executable::build_application(config, Arc::new(StartupRpc))
        .await
        .expect("a missing Skills root is reported but does not block startup");
    let skills = app
        .oneshot(request("GET", "/v1/skills", Some("startup-test-secret")))
        .await
        .expect("skills response");
    assert_eq!(skills.status(), StatusCode::OK);
    assert_eq!(
        response_json(skills).await["skills"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn scalable_startup_constructs_redis_approval_storage() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let skills_root = directory.path().join("skills");
    std::fs::create_dir_all(&skills_root).expect("skills root");
    let mut environment = environment(&skills_root, &directory.path().join("tools.json"));
    environment.insert("STORAGE_TYPE".to_owned(), "Scalable".to_owned());
    environment.insert("REDIS_URL".to_owned(), "redis://127.0.0.1:6379".to_owned());
    let config = AgentServerConfig::parse(&environment).expect("valid scalable config");

    let (_, app) = executable::build_application(config, Arc::new(StartupRpc))
        .await
        .expect("Redis-backed approval storage is configured at startup");
    let skills = app
        .oneshot(request("GET", "/v1/skills", Some("startup-test-secret")))
        .await
        .expect("skills response");
    assert_eq!(skills.status(), StatusCode::OK);
}

fn model_invocation(llm_worker_id: i64) -> ModelInvocation {
    ModelInvocation {
        llm_worker_id,
        options: ModelOptions::default(),
        history: vec![ChatMessage::new(Role::User, json!("hello"))],
        client_tools_json: "[]".to_owned(),
        is_auto_calling: false,
        function_set_name: None,
    }
}

#[tokio::test]
async fn chat_model_invoker_uses_selected_worker_and_resume_uses_persisted_selection() {
    let executor = Arc::new(RecordingExecutor::default());
    let model = executable::SelectedWorkerModelInvoker::new(executor.clone(), Some(41));

    let response = model
        .invoke(model_invocation(41))
        .await
        .expect("selected Worker model invocation");
    assert_eq!(response.content, json!("model answer"));
    assert_eq!(*executor.worker_ids.lock().unwrap(), [41]);

    let observer = Arc::new(RecordingModelObserver::default());
    model
        .invoke_with_observer(model_invocation(41), observer.clone())
        .await
        .expect("selected Worker model invocation preserves job observation");
    assert_eq!(*observer.started.lock().unwrap(), [1]);
    assert_eq!(*observer.finished.lock().unwrap(), [1]);

    assert!(model.invoke(model_invocation(73)).await.is_err());
    assert_eq!(*executor.worker_ids.lock().unwrap(), [41, 41]);

    let restored = executable::SelectedWorkerModelInvoker::new(executor.clone(), None);
    restored
        .invoke(model_invocation(73))
        .await
        .expect("restored approval uses its persisted Worker selection");
    assert_eq!(*executor.worker_ids.lock().unwrap(), [41, 41, 73]);
}

#[tokio::test]
async fn selected_model_forwards_live_deltas_and_cancels_the_observed_job() {
    let executor = Arc::new(BlockingObserverExecutor::default());
    let model = Arc::new(executable::SelectedWorkerModelInvoker::new(
        executor.clone(),
        Some(41),
    ));
    let observer = Arc::new(RecordingModelObserver::default());
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        delta_tx
            .send(text.to_owned())
            .map_err(|_| "test delta receiver closed".to_owned())
    });

    let invocation_task = {
        let model = model.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            model
                .invoke_with_observer_and_text_delta(model_invocation(41), observer, callback)
                .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        executor.started.notified(),
    )
    .await
    .expect("executor blocks after forwarding the live delta");

    assert_eq!(delta_rx.try_recv().unwrap(), "streamed answer");
    assert_eq!(*observer.started.lock().unwrap(), [91]);
    assert!(observer.finished.lock().unwrap().is_empty());
    assert_eq!(
        executor.invocations.lock().unwrap()[0].worker_id,
        41,
        "the selected Worker ID, not a callback-provided target, reaches the executor"
    );

    model
        .cancel_job("91")
        .await
        .expect("cancel delegates to the executor for the observed job");
    assert_eq!(*executor.cancelled_job_ids.lock().unwrap(), ["91"]);
    assert!(invocation_task.await.unwrap().is_err());

    let rejected_observer = Arc::new(RecordingModelObserver::default());
    let callback_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let callback_count = callback_calls.clone();
    let rejected_callback: ModelTextDeltaCallback = Arc::new(move |_| {
        callback_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    });
    assert!(
        model
            .invoke_with_observer_and_text_delta(
                model_invocation(73),
                rejected_observer.clone(),
                rejected_callback,
            )
            .await
            .is_err()
    );
    assert_eq!(executor.invocations.lock().unwrap().len(), 1);
    assert!(rejected_observer.started.lock().unwrap().is_empty());
    assert_eq!(callback_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[tokio::test]
async fn grpc_model_invoker_trait_forwards_observer_and_text_delta_together() {
    let executor = Arc::new(BlockingObserverExecutor::default());
    let model: Arc<dyn ModelInvoker> = Arc::new(GrpcModelInvoker::new(41, executor.clone()));
    let observer = Arc::new(RecordingModelObserver::default());
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let callback: ModelTextDeltaCallback = Arc::new(move |text| {
        delta_tx
            .send(text.to_owned())
            .map_err(|_| "test delta receiver closed".to_owned())
    });

    let invocation_task = {
        let model = model.clone();
        let observer = observer.clone();
        tokio::spawn(async move {
            model
                .invoke_with_observer_and_text_delta(model_invocation(41), observer, callback)
                .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        executor.started.notified(),
    )
    .await
    .expect("executor blocks after forwarding the live delta");

    assert_eq!(delta_rx.try_recv().unwrap(), "streamed answer");
    assert_eq!(*observer.started.lock().unwrap(), [91]);
    assert!(observer.finished.lock().unwrap().is_empty());
    model
        .cancel_job("91")
        .await
        .expect("trait cancellation delegates to the ToolExecutor");
    assert_eq!(*executor.cancelled_job_ids.lock().unwrap(), ["91"]);
    assert!(invocation_task.await.unwrap().is_err());
}

#[tokio::test]
async fn chat_cancellation_targets_the_selected_model_job_registered_by_its_observer() {
    let executor = Arc::new(BlockingObserverExecutor::default());
    let model: Arc<dyn ModelInvoker> = Arc::new(executable::SelectedWorkerModelInvoker::new(
        executor.clone(),
        Some(41),
    ));
    let orchestrator = Arc::new(ChatOrchestrator::new(
        model,
        Arc::new(SkillCatalog::new(Vec::new())),
        Arc::new(EmptyChatRegistry),
        Arc::new(UnusedWorkerExecutor),
        Arc::new(MemoryPendingApprovalStore::new(
            std::time::Duration::from_secs(60),
        )),
        ChatConfig::default(),
    ));
    let chat_id = "chat-selected-model-cancel";
    let capability = orchestrator
        .prepare_chat_execution(chat_id)
        .expect("chat cancellation capability is prepared");
    let chat = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move {
            orchestrator
                .chat(ChatRequest {
                    chat_id: chat_id.to_owned(),
                    llm_worker_id: 41,
                    options: ModelOptions::default(),
                    history: vec![ChatMessage::new(Role::User, json!("hello"))],
                })
                .await
        })
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        executor.started.notified(),
    )
    .await
    .expect("the selected model job is registered before its result is awaited");

    assert!(
        orchestrator
            .cancel(chat_id, &capability, "91")
            .await
            .expect("cancel succeeds for the tracked model job")
    );
    assert_eq!(*executor.cancelled_job_ids.lock().unwrap(), ["91"]);
    assert!(chat.await.unwrap().is_err());
}

#[tokio::test]
async fn grpc_auth_metadata_protects_every_worker_runner_and_job_rpc() {
    const AUTH_TOKEN: &str = "protected-jobworkerp-token";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake gRPC listener binds");
    let address = listener.local_addr().expect("fake gRPC address");
    let state = ProtectedGrpcState {
        expected_token: AUTH_TOKEN.to_owned(),
        calls: Arc::new(Mutex::new(Vec::new())),
    };
    let observed_calls = state.calls.clone();
    let incoming = futures_util::stream::unfold(listener, |listener| async move {
        match listener.accept().await {
            Ok((stream, _)) => Some((Ok::<_, std::io::Error>(stream), listener)),
            Err(error) => Some((Err(error), listener)),
        }
    });
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        Server::builder()
            .add_service(ProtectedWorkerService(server_state.clone()))
            .add_service(ProtectedRunnerService(server_state.clone()))
            .add_service(ProtectedJobService(server_state))
            .serve_with_incoming(incoming)
            .await
            .expect("fake gRPC server serves requests");
    });

    let endpoint = format!("http://{address}");
    let unauthenticated = executable::connect_jobworkerp_rpc(endpoint.clone(), None)
        .await
        .expect("unauthenticated test client connects");
    assert_eq!(
        rpc_status(unauthenticated.find_worker(17).await).code(),
        tonic::Code::Unauthenticated
    );

    let authenticated = executable::connect_jobworkerp_rpc(endpoint, Some(AUTH_TOKEN.to_owned()))
        .await
        .expect("authenticated test client connects");
    assert_eq!(
        rpc_status(authenticated.find_worker(17).await).code(),
        tonic::Code::Unimplemented
    );
    assert_eq!(
        rpc_status(authenticated.find_runner(23).await).code(),
        tonic::Code::Unimplemented
    );
    assert_eq!(
        rpc_status(authenticated.enqueue(JobRequest::default()).await).code(),
        tonic::Code::Unimplemented
    );
    assert_eq!(
        rpc_status(
            authenticated
                .enqueue_for_stream(JobRequest::default())
                .await
        )
        .code(),
        tonic::Code::Unimplemented
    );
    assert_eq!(
        rpc_status(
            authenticated
                .enqueue_for_result(JobRequest::default())
                .await
        )
        .code(),
        tonic::Code::Unimplemented
    );
    assert_eq!(
        rpc_status(authenticated.delete(JobId { value: 31 }).await).code(),
        tonic::Code::Unimplemented
    );

    let calls = observed_calls.lock().unwrap();
    assert_eq!(calls.len(), 7);
    assert_eq!(calls[0].0, "/jobworkerp.service.WorkerService/Find");
    assert_eq!(calls[0].1, None);
    assert_eq!(calls[0].2, "16");
    let expected_paths = [
        "/jobworkerp.service.WorkerService/Find",
        "/jobworkerp.service.RunnerService/Find",
        "/jobworkerp.service.JobService/Enqueue",
        "/jobworkerp.service.JobService/EnqueueForStream",
        "/jobworkerp.service.JobService/EnqueueForResult",
        "/jobworkerp.service.JobService/Delete",
    ];
    for (call, expected_path) in calls[1..].iter().zip(expected_paths) {
        assert_eq!(call.0, expected_path);
        assert_eq!(call.1.as_deref(), Some(AUTH_TOKEN));
        assert_eq!(call.2, "12");
    }
    server.abort();
}

#[tokio::test]
async fn grpc_startup_errors_do_not_echo_the_endpoint_or_auth_token() {
    let endpoint = "not-a-grpc-endpoint-with-private-data";
    let token = "never-log-this-jobworkerp-secret";
    let error =
        match executable::connect_jobworkerp_rpc(endpoint.to_owned(), Some(token.to_owned())).await
        {
            Err(error) => error,
            Ok(_) => panic!("invalid endpoint unexpectedly connected"),
        };
    assert!(!error.to_string().contains(endpoint));
    assert!(!error.to_string().contains(token));
}

fn rpc_status<T>(result: Result<T, Status>) -> Status {
    match result {
        Ok(_) => panic!("protected fake server unexpectedly returned a successful RPC"),
        Err(status) => status,
    }
}
