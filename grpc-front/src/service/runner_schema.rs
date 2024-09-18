use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::{RunnerSchema, RunnerSchemaData, RunnerSchemaId};
use crate::proto::jobworkerp::service::runner_schema_service_server::RunnerSchemaService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, CreateRunnerSchemaResponse, FindListRequest,
    OptionalRunnerSchemaResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::runner_schema::RunnerSchemaApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::trace::Tracing;
use tonic::Response;

pub trait RunnerSchemaGrpc {
    fn app(&self) -> &Arc<dyn RunnerSchemaApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);
const LIST_TTL: Duration = Duration::from_secs(5);

#[tonic::async_trait]
impl<T: RunnerSchemaGrpc + Tracing + Send + Debug + Sync + 'static> RunnerSchemaService for T {
    #[tracing::instrument]
    async fn create(
        &self,
        request: tonic::Request<RunnerSchemaData>,
    ) -> Result<tonic::Response<CreateRunnerSchemaResponse>, tonic::Status> {
        let _span = Self::trace_request("runner_schema", "create", &request);
        let req = request.into_inner();
        match self.app().create_runner_schema(req).await {
            Ok(id) => Ok(Response::new(CreateRunnerSchemaResponse { id: Some(id) })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn update(
        &self,
        request: tonic::Request<RunnerSchema>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("runner_schema", "update", &request);
        let req = request.get_ref();
        if let Some(i) = &req.id {
            match self.app().update_runner_schema(i, &req.data).await {
                Ok(res) => Ok(Response::new(SuccessResponse { is_success: res })),
                Err(e) => Err(handle_error(&e)),
            }
        } else {
            tracing::warn!("id not found in updating: {:?}", req);
            Err(tonic::Status::not_found("id not found".to_string()))
        }
    }
    #[tracing::instrument]
    async fn delete(
        &self,
        request: tonic::Request<RunnerSchemaId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("runner_schema", "delete", &request);
        let req = request.get_ref();
        match self.app().delete_runner_schema(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn find(
        &self,
        request: tonic::Request<RunnerSchemaId>,
    ) -> Result<tonic::Response<OptionalRunnerSchemaResponse>, tonic::Status> {
        let _s = Self::trace_request("runner_schema", "find", &request);
        let req = request.get_ref();
        match self.app().find_runner_schema(req, Some(&DEFAULT_TTL)).await {
            Ok(res) => Ok(Response::new(OptionalRunnerSchemaResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<RunnerSchema, tonic::Status>>;
    #[tracing::instrument]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("runner_schema", "find_list", &request);
        let req = request.get_ref();
        let ttl = if req.limit.is_some() {
            LIST_TTL
        } else {
            DEFAULT_TTL
        };
        match self
            .app()
            .find_runner_schema_list(req.limit.as_ref(), req.offset.as_ref(), Some(&ttl))
            .await
        {
            Ok(list) => {
                // TODO streamingのより良いやり方がないか?
                Ok(Response::new(Box::pin(stream! {
                    for s in list {
                        yield Ok(s)
                    }
                })))
            }
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn count(
        &self,
        request: tonic::Request<CountCondition>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("runner_schema", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct RunnerSchemaGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl RunnerSchemaGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        RunnerSchemaGrpcImpl { app_module }
    }
}
impl RunnerSchemaGrpc for RunnerSchemaGrpcImpl {
    fn app(&self) -> &Arc<dyn RunnerSchemaApp + 'static> {
        &self.app_module.runner_schema_app
    }
}

// use tracing
impl Tracing for RunnerSchemaGrpcImpl {}
