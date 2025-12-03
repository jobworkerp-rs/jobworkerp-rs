use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::data::{Runner, RunnerId};
use crate::proto::jobworkerp::service::runner_service_server::RunnerService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, CountRunnerRequest, CreateRunnerRequest, CreateRunnerResponse,
    FindListRequest, FindRunnerListRequest, OptionalRunnerResponse, RunnerNameRequest,
    SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::runner::RunnerApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use tonic::Response;

pub trait RunnerGrpc {
    fn app(&self) -> &Arc<dyn RunnerApp + 'static>;
}

const MAX_RESERVED_RUNNER_ID: i64 = 65535;

#[tonic::async_trait]
impl<T: RunnerGrpc + Tracing + Send + Debug + Sync + 'static> RunnerService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "create"))]
    async fn create(
        &self,
        request: tonic::Request<CreateRunnerRequest>,
    ) -> std::result::Result<tonic::Response<CreateRunnerResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "create", &request);
        let req = request.get_ref();
        match self
            .app()
            .create_runner(
                req.name.as_str(),
                req.description.as_str(),
                req.runner_type,
                req.definition.as_str(),
            )
            .await
        {
            Ok(r) => Ok(Response::new(CreateRunnerResponse { id: Some(r) })),
            Err(e) => Err(handle_error(&e)),
        }
    }
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
        match self.app().find_runner(req).await {
            Ok(Some(res)) => Ok(Response::new(OptionalRunnerResponse {
                data: Some(res.into_proto()),
            })),
            Ok(None) => Ok(Response::new(OptionalRunnerResponse { data: None })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_by_name"))]
    async fn find_by_name(
        &self,
        request: tonic::Request<RunnerNameRequest>,
    ) -> std::result::Result<tonic::Response<OptionalRunnerResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "find", &request);
        let req = request.get_ref();
        match self.app().find_runner_by_name(req.name.as_str()).await {
            Ok(Some(res)) => Ok(Response::new(OptionalRunnerResponse {
                data: Some(res.into_proto()),
            })),
            Ok(None) => Ok(Response::new(OptionalRunnerResponse { data: None })),
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
        tracing::warn!(
            "FindList method is deprecated, use FindListBy instead. \
             This method will be removed in version 2.0.0"
        );
        match self
            .app()
            .find_runner_list(false, req.limit.as_ref(), req.offset.as_ref())
            .await
        {
            Ok(list) => {
                // TODO streamingのより良いやり方がないか?
                Ok(Response::new(Box::pin(stream! {
                    for s in list {
                        yield Ok(s.into_proto());
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
        tracing::warn!(
            "Count method is deprecated, use CountBy instead. \
             This method will be removed in version 2.0.0"
        );
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListByStream = BoxStream<'static, Result<Runner, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list_by"))]
    async fn find_list_by(
        &self,
        request: tonic::Request<FindRunnerListRequest>,
    ) -> Result<tonic::Response<Self::FindListByStream>, tonic::Status> {
        let _s = Self::trace_request("runner", "find_list_by", &request);
        let req = request.into_inner();

        super::validation::validate_limit(req.limit)?;
        super::validation::validate_offset(req.offset)?;
        super::validation::validate_name_filter(req.name_filter.as_ref())?;
        super::validation::validate_filter_enums(&req.runner_types, "runner_types")?;

        let runner_types: Vec<i32> = req.runner_types.clone();

        let name_filter = req.name_filter.clone();
        let limit = req.limit;
        let offset = req.offset;
        let sort_by = req
            .sort_by
            .and_then(|val| proto::jobworkerp::data::RunnerSortField::try_from(val).ok());
        let ascending = req.ascending;

        match self
            .app()
            .find_runner_list_by(runner_types, name_filter, limit, offset, sort_by, ascending)
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for s in list {
                    yield Ok(s.into_proto());
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count_by"))]
    async fn count_by(
        &self,
        request: tonic::Request<CountRunnerRequest>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("runner", "count_by", &request);
        let req = request.into_inner();

        super::validation::validate_name_filter(req.name_filter.as_ref())?;
        super::validation::validate_filter_enums(&req.runner_types, "runner_types")?;

        let runner_types: Vec<i32> = req.runner_types.clone();
        let name_filter = req.name_filter.clone();

        match self.app().count_by(runner_types, name_filter).await {
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
