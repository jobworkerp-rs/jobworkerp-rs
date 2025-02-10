use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::{Runner, RunnerId};
use crate::proto::jobworkerp::service::runner_service_server::RunnerService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, FindListRequest, OptionalRunnerResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::runner::RunnerApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra::error::JobWorkerError;
use infra_utils::trace::Tracing;
use tonic::Response;

pub trait RunnerGrpc {
    fn app(&self) -> &Arc<dyn RunnerApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);
const LIST_TTL: Duration = Duration::from_secs(5);
const MAX_RESERVED_RUNNER_ID: i64 = 1024;

#[tonic::async_trait]
impl<T: RunnerGrpc + Tracing + Send + Debug + Sync + 'static> RunnerService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "delete"))]
    async fn delete(
        &self,
        request: tonic::Request<RunnerId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "delete", &request);
        let req = request.get_ref();
        // cannot delete reserved runner
        if req.value <= MAX_RESERVED_RUNNER_ID {
            return Err(handle_error(
                &JobWorkerError::InvalidParameter(format!(
                    "cannot delete reserved runner: {}",
                    req.value
                ))
                .into(),
            ));
        }
        match self.app().delete_runner(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<RunnerId>,
    ) -> Result<tonic::Response<OptionalRunnerResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "find", &request);
        let req = request.get_ref();
        match self.app().find_runner(req, Some(&DEFAULT_TTL)).await {
            Ok(res) => Ok(Response::new(OptionalRunnerResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<Runner, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("runner", "find_list", &request);
        let req = request.get_ref();
        let ttl = if req.limit.is_some() {
            LIST_TTL
        } else {
            DEFAULT_TTL
        };
        match self
            .app()
            .find_runner_list(req.limit.as_ref(), req.offset.as_ref(), Some(&ttl))
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
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count"))]
    async fn count(
        &self,
        request: tonic::Request<CountCondition>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct RunnerGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl RunnerGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        RunnerGrpcImpl { app_module }
    }
}
impl RunnerGrpc for RunnerGrpcImpl {
    fn app(&self) -> &Arc<dyn RunnerApp + 'static> {
        &self.app_module.runner_app
    }
}

// use tracing
impl Tracing for RunnerGrpcImpl {}
