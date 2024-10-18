use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::{WorkerSchema, WorkerSchemaId};
use crate::proto::jobworkerp::service::worker_schema_service_server::WorkerSchemaService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, FindListRequest, OptionalWorkerSchemaResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::worker_schema::WorkerSchemaApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::trace::Tracing;
use tonic::Response;

pub trait WorkerSchemaGrpc {
    fn app(&self) -> &Arc<dyn WorkerSchemaApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);
const LIST_TTL: Duration = Duration::from_secs(5);

#[tonic::async_trait]
impl<T: WorkerSchemaGrpc + Tracing + Send + Debug + Sync + 'static> WorkerSchemaService for T {
    #[tracing::instrument]
    async fn delete(
        &self,
        request: tonic::Request<WorkerSchemaId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("worker_schema", "delete", &request);
        let req = request.get_ref();
        match self.app().delete_worker_schema(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn find(
        &self,
        request: tonic::Request<WorkerSchemaId>,
    ) -> Result<tonic::Response<OptionalWorkerSchemaResponse>, tonic::Status> {
        let _s = Self::trace_request("worker_schema", "find", &request);
        let req = request.get_ref();
        match self.app().find_worker_schema(req, Some(&DEFAULT_TTL)).await {
            Ok(res) => Ok(Response::new(OptionalWorkerSchemaResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<WorkerSchema, tonic::Status>>;
    #[tracing::instrument]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("worker_schema", "find_list", &request);
        let req = request.get_ref();
        let ttl = if req.limit.is_some() {
            LIST_TTL
        } else {
            DEFAULT_TTL
        };
        match self
            .app()
            .find_worker_schema_list(req.limit.as_ref(), req.offset.as_ref(), Some(&ttl))
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
        let _s = Self::trace_request("worker_schema", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct WorkerSchemaGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl WorkerSchemaGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        WorkerSchemaGrpcImpl { app_module }
    }
}
impl WorkerSchemaGrpc for WorkerSchemaGrpcImpl {
    fn app(&self) -> &Arc<dyn WorkerSchemaApp + 'static> {
        &self.app_module.worker_schema_app
    }
}

// use tracing
impl Tracing for WorkerSchemaGrpcImpl {}
