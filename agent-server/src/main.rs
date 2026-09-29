//! Standalone HTTP/JSON/SSE process for the independent Agent Server.

use std::{error::Error, fmt, net::SocketAddr, sync::Arc};

use agent_server::{
    approval::{MemoryPendingApprovalStore, PendingApprovalStore, RedisPendingApprovalStore},
    backend::{AgentBackend, ChatOrchestratorFactory},
    chat::{
        ChatConfig, ChatOrchestrator, ChatToolRegistry, ModelInvocation, ModelInvoker,
        ModelJobObserver, ModelResponse, ToolRegistryAdapter, WorkerExecutor,
    },
    config::{AgentServerConfig, AgentServerConfigError, AgentServerStorage},
    grpc::{
        GrpcToolExecutor, JobworkerpRpc, ResultEnqueueResponse, StreamEnqueueResponse,
        ToolExecutor, proto,
    },
    http::{self, HttpBackend},
    model::{GrpcModelInvoker, ModelTextDeltaCallback},
    skills::SkillCatalog,
    tool_registry::{ToolRegistry, WorkerSchemaResolver},
    worker::GrpcWorkerExecutor,
};
use async_trait::async_trait;
use axum::Router;
use tonic::{
    Status,
    metadata::{Ascii, MetadataMap, MetadataValue},
    service::Interceptor,
    transport::{Channel, Endpoint},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Agent Server failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), StartupError> {
    let config = AgentServerConfig::from_env().map_err(StartupError::Configuration)?;
    // All service adapters share this single connected gRPC channel.
    let rpc = connect_jobworkerp_rpc(config.grpc_endpoint.clone(), config.grpc_auth_token.clone())
        .await?;
    let (address, app) = build_application(config, rpc).await?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|_| StartupError::BindListener)?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|_| StartupError::ServeHttp)
}

pub(crate) async fn build_application(
    config: AgentServerConfig,
    rpc: Arc<dyn JobworkerpRpc>,
) -> Result<(SocketAddr, Router), StartupError> {
    let http_config = config.http_config();
    let address = http_config.bind_addr;
    let approval_ttl = config.approval_ttl;

    // SkillCatalog publishes its first immutable snapshot in `new` and exposes administrator
    // reload through AgentBackend's /v1/skills/reload route.
    let skills = Arc::new(SkillCatalog::new(config.skills_roots));
    let initial_skill_report = skills.reload();
    for diagnostic in initial_skill_report.diagnostics {
        eprintln!(
            "Agent Server Skills startup diagnostic: {:?}",
            diagnostic.kind
        );
    }
    let schema_resolver: Arc<dyn WorkerSchemaResolver> =
        Arc::new(GrpcToolExecutor::with_rpc(rpc.clone()));
    let tools = Arc::new(
        ToolRegistry::open(&config.tool_registry_path, schema_resolver)
            .await
            .map_err(|_| StartupError::ToolRegistry)?,
    );
    let approvals: Arc<dyn PendingApprovalStore> = match &config.storage {
        AgentServerStorage::Standalone => Arc::new(MemoryPendingApprovalStore::new(approval_ttl)),
        AgentServerStorage::Scalable { .. } => Arc::new(RedisPendingApprovalStore::new(
            config
                .storage
                .redis_client()
                .map_err(|_| StartupError::RedisClient)?,
            approval_ttl,
        )),
    };

    let registry: Arc<dyn ChatToolRegistry> = Arc::new(ToolRegistryAdapter::new(tools.clone()));
    let factory: Arc<dyn ChatOrchestratorFactory> = Arc::new(ProductionChatFactory {
        rpc,
        skills: skills.clone(),
        registry,
        approvals,
        chat_config: ChatConfig {
            approval_ttl,
            ..ChatConfig::default()
        },
    });
    let backend: Arc<dyn HttpBackend> = Arc::new(AgentBackend::with_approval_ttl(
        factory,
        skills,
        tools,
        approval_ttl,
    ));
    let app = http::router(http_config, backend).map_err(|_| StartupError::HttpConfiguration)?;
    Ok((address, app))
}

struct ProductionChatFactory {
    rpc: Arc<dyn JobworkerpRpc>,
    skills: Arc<SkillCatalog>,
    registry: Arc<dyn ChatToolRegistry>,
    approvals: Arc<dyn PendingApprovalStore>,
    chat_config: ChatConfig,
}

#[async_trait]
impl ChatOrchestratorFactory for ProductionChatFactory {
    async fn create(
        &self,
        llm_worker_id: i64,
        _options: Option<http::ChatOptions>,
    ) -> Result<Arc<ChatOrchestrator>, String> {
        if llm_worker_id <= 0 {
            return Err("LLM Worker ID must be positive".to_owned());
        }

        Ok(self.build_orchestrator(Some(llm_worker_id)))
    }

    async fn create_for_resume(&self) -> Result<Arc<ChatOrchestrator>, String> {
        Ok(self.build_orchestrator(None))
    }
}

impl ProductionChatFactory {
    fn build_orchestrator(&self, selected_worker_id: Option<i64>) -> Arc<ChatOrchestrator> {
        let tool_executor: Arc<dyn ToolExecutor> =
            Arc::new(GrpcToolExecutor::with_rpc(self.rpc.clone()));
        let model: Arc<dyn ModelInvoker> = Arc::new(SelectedWorkerModelInvoker::new(
            tool_executor,
            selected_worker_id,
        ));
        let workers: Arc<dyn WorkerExecutor> =
            Arc::new(GrpcWorkerExecutor::with_rpc(self.rpc.clone()));

        Arc::new(ChatOrchestrator::new(
            model,
            self.skills.clone(),
            self.registry.clone(),
            workers,
            self.approvals.clone(),
            self.chat_config.clone(),
        ))
    }
}

pub(crate) struct SelectedWorkerModelInvoker {
    executor: Arc<dyn ToolExecutor>,
    selected_worker_id: Option<i64>,
}

impl SelectedWorkerModelInvoker {
    pub(crate) fn new(executor: Arc<dyn ToolExecutor>, selected_worker_id: Option<i64>) -> Self {
        Self {
            executor,
            selected_worker_id,
        }
    }

    fn validate_selection(&self, invocation: &ModelInvocation) -> Result<(), String> {
        if self
            .selected_worker_id
            .is_some_and(|selected| selected != invocation.llm_worker_id)
        {
            return Err(
                "model invocation does not match the chat's selected LLM Worker".to_owned(),
            );
        }
        Ok(())
    }
}

#[async_trait]
impl ModelInvoker for SelectedWorkerModelInvoker {
    async fn invoke(&self, invocation: ModelInvocation) -> Result<ModelResponse, String> {
        self.validate_selection(&invocation)?;
        GrpcModelInvoker::new(invocation.llm_worker_id, self.executor.clone())
            .invoke(invocation)
            .await
    }

    async fn invoke_with_observer(
        &self,
        invocation: ModelInvocation,
        observer: Arc<dyn ModelJobObserver>,
    ) -> Result<ModelResponse, String> {
        self.validate_selection(&invocation)?;
        GrpcModelInvoker::new(invocation.llm_worker_id, self.executor.clone())
            .invoke_with_observer(invocation, observer)
            .await
    }

    async fn invoke_with_observer_and_text_delta(
        &self,
        invocation: ModelInvocation,
        observer: Arc<dyn ModelJobObserver>,
        callback: ModelTextDeltaCallback,
    ) -> Result<ModelResponse, String> {
        self.validate_selection(&invocation)?;
        GrpcModelInvoker::new(invocation.llm_worker_id, self.executor.clone())
            .invoke_with_observer_and_text_delta(invocation, observer, callback)
            .await
    }

    async fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        self.executor.cancel_job(job_id).await
    }
}

pub(crate) async fn connect_jobworkerp_rpc(
    endpoint: String,
    auth_token: Option<String>,
) -> Result<Arc<dyn JobworkerpRpc>, StartupError> {
    let endpoint = Endpoint::from_shared(endpoint).map_err(|_| StartupError::GrpcConnection)?;
    let channel = endpoint
        .connect()
        .await
        .map_err(|_| StartupError::GrpcConnection)?;
    let mut auth_token = auth_token
        .map(|token| MetadataValue::<Ascii>::try_from(token.as_str()))
        .transpose()
        .map_err(|_| StartupError::GrpcAuthToken)?;
    if let Some(token) = auth_token.as_mut() {
        token.set_sensitive(true);
    }
    Ok(Arc::new(ConnectedJobworkerpRpc {
        channel,
        auth_token,
    }))
}

struct ConnectedJobworkerpRpc {
    channel: Channel,
    auth_token: Option<MetadataValue<Ascii>>,
}

impl ConnectedJobworkerpRpc {
    fn auth_interceptor(&self) -> JobworkerpAuthInterceptor {
        JobworkerpAuthInterceptor {
            auth_token: self.auth_token.clone(),
        }
    }
}

#[derive(Clone)]
struct JobworkerpAuthInterceptor {
    auth_token: Option<MetadataValue<Ascii>>,
}

impl Interceptor for JobworkerpAuthInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        if let Some(auth_token) = &self.auth_token {
            request
                .metadata_mut()
                .insert("jobworkerp-auth", auth_token.clone());
        }
        Ok(request)
    }
}

#[async_trait]
impl JobworkerpRpc for ConnectedJobworkerpRpc {
    async fn find_worker(
        &self,
        worker_id: i64,
    ) -> Result<Option<proto::jobworkerp::data::WorkerData>, Status> {
        use proto::jobworkerp::service::worker_service_client::WorkerServiceClient;

        let response =
            WorkerServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
                .find(proto::jobworkerp::data::WorkerId { value: worker_id })
                .await?
                .into_inner();
        let Some(worker) = response.data else {
            return Ok(None);
        };
        if worker.id.as_ref().map(|id| id.value) != Some(worker_id) {
            return Err(Status::data_loss(
                "WorkerService.Find returned a different worker ID",
            ));
        }
        Ok(worker.data)
    }

    async fn find_runner(
        &self,
        runner_id: i64,
    ) -> Result<Option<proto::jobworkerp::data::RunnerData>, Status> {
        use proto::jobworkerp::service::runner_service_client::RunnerServiceClient;

        let response =
            RunnerServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
                .find(proto::jobworkerp::data::RunnerId { value: runner_id })
                .await?
                .into_inner();
        let Some(runner) = response.data else {
            return Ok(None);
        };
        if runner.id.as_ref().map(|id| id.value) != Some(runner_id) {
            return Err(Status::data_loss(
                "RunnerService.Find returned a different runner ID",
            ));
        }
        Ok(runner.data)
    }

    async fn enqueue(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> Result<proto::jobworkerp::service::CreateJobResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        JobServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
            .enqueue(request)
            .await
            .map(tonic::Response::into_inner)
    }

    async fn enqueue_for_stream(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> Result<StreamEnqueueResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response =
            JobServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
                .enqueue_for_stream(request)
                .await?;
        let job_id_header = binary_job_id_header(response.metadata())?;
        Ok(StreamEnqueueResponse {
            job_id_header,
            items: Box::pin(response.into_inner()),
        })
    }

    async fn enqueue_for_result(
        &self,
        request: proto::jobworkerp::service::JobRequest,
    ) -> Result<ResultEnqueueResponse, Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response =
            JobServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
                .enqueue_for_result(request)
                .await?;
        let job_id_header = binary_job_id_header(response.metadata())?;
        Ok(ResultEnqueueResponse {
            job_id_header,
            results: Box::pin(response.into_inner()),
        })
    }

    async fn delete(&self, job_id: proto::jobworkerp::data::JobId) -> Result<(), Status> {
        use proto::jobworkerp::service::job_service_client::JobServiceClient;

        let response =
            JobServiceClient::with_interceptor(self.channel.clone(), self.auth_interceptor())
                .delete(job_id)
                .await?
                .into_inner();
        confirm_delete(response.is_success)
    }
}

fn confirm_delete(is_success: bool) -> Result<(), Status> {
    if is_success {
        Ok(())
    } else {
        Err(Status::failed_precondition(
            "jobworkerp did not cancel the job",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn delete_false_is_not_reported_as_cancellation_success() {
        assert!(super::confirm_delete(true).is_ok());
        assert_eq!(
            super::confirm_delete(false).unwrap_err().code(),
            tonic::Code::FailedPrecondition
        );
    }
}

fn binary_job_id_header(metadata: &MetadataMap) -> Result<Option<Vec<u8>>, Status> {
    metadata
        .get_bin("x-job-id-bin")
        .map(|value| {
            value
                .to_bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|_| Status::internal("invalid x-job-id-bin metadata"))
        })
        .transpose()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };

    #[cfg(unix)]
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    #[cfg(not(unix))]
    ctrl_c.await;
}

#[derive(Debug)]
pub(crate) enum StartupError {
    Configuration(AgentServerConfigError),
    GrpcConnection,
    GrpcAuthToken,
    RedisClient,
    ToolRegistry,
    HttpConfiguration,
    BindListener,
    ServeHttp,
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(formatter, "invalid configuration: {error}"),
            Self::GrpcConnection => formatter.write_str("could not connect to jobworkerp gRPC"),
            Self::GrpcAuthToken => formatter.write_str("invalid jobworkerp gRPC auth token"),
            Self::RedisClient => formatter.write_str("could not configure Redis approval storage"),
            Self::ToolRegistry => {
                formatter.write_str("could not load the Agent Server ToolRegistry")
            }
            Self::HttpConfiguration => formatter.write_str("invalid HTTP server configuration"),
            Self::BindListener => formatter.write_str("could not bind the HTTP listener"),
            Self::ServeHttp => formatter.write_str("the HTTP server stopped with an error"),
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Configuration(error) => Some(error),
            _ => None,
        }
    }
}
