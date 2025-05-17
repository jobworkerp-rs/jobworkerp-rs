use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::function::data::{FunctionSet, FunctionSetData, FunctionSetId};
use crate::proto::jobworkerp::function::service::function_set_service_server::FunctionSetService;
use crate::proto::jobworkerp::function::service::{
    CreateFunctionSetResponse, FindByNameRequest, OptionalFunctionSetResponse,
};
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, FindListRequest, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::trace::Tracing;
use tonic::Response;

pub trait FunctionSetGrpc {
    fn app(&self) -> &FunctionSetAppImpl;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);
const LIST_TTL: Duration = Duration::from_secs(5);

#[tonic::async_trait]
impl<T: FunctionSetGrpc + Tracing + Send + Debug + Sync + 'static> FunctionSetService for T {
    #[tracing::instrument(name = "function_set.create", skip(self, request))]
    async fn create(
        &self,
        request: tonic::Request<FunctionSetData>,
    ) -> Result<tonic::Response<CreateFunctionSetResponse>, tonic::Status> {
        let _span = Self::trace_request("function_set", "create", &request);
        let req = request.get_ref();
        match self.app().create_function_set(req).await {
            Ok(id) => Ok(Response::new(CreateFunctionSetResponse { id: Some(id) })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(name = "function_set.update", skip(self, request))]
    async fn update(
        &self,
        request: tonic::Request<FunctionSet>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("function_set", "update", &request);
        let req = request.get_ref();
        if let Some(i) = &req.id {
            match self.app().update_function_set(i, &req.data).await {
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
        request: tonic::Request<FunctionSetId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("function_set", "delete", &request);
        let req = request.get_ref();
        match self.app().delete_function_set(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(name = "function_set.find", skip(self, request))]
    async fn find(
        &self,
        request: tonic::Request<FunctionSetId>,
    ) -> Result<tonic::Response<OptionalFunctionSetResponse>, tonic::Status> {
        let _s = Self::trace_request("function_set", "find", &request);
        let req = request.get_ref();
        match self.app().find_function_set(req).await {
            Ok(res) => Ok(Response::new(OptionalFunctionSetResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(name = "function_set.find_by_name", skip(self, request))]
    async fn find_by_name(
        &self,
        request: tonic::Request<FindByNameRequest>,
    ) -> Result<tonic::Response<OptionalFunctionSetResponse>, tonic::Status> {
        let _s = Self::trace_request("function_set", "find", &request);
        let req = request.get_ref();
        match self.app().find_function_set_by_name(&req.name).await {
            Ok(res) => Ok(Response::new(OptionalFunctionSetResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<FunctionSet, tonic::Status>>;
    #[tracing::instrument(
        name = "function_set.find_list",
        skip(self, request),
        fields(method = "find_list")
    )]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("function_set", "find_list", &request);
        let req = request.get_ref();
        let ttl = if req.limit.is_some() {
            LIST_TTL
        } else {
            DEFAULT_TTL
        };
        match self
            .app()
            .find_function_set_list(req.limit.as_ref(), req.offset.as_ref(), Some(&ttl))
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
        let _s = Self::trace_request("function_set", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct FunctionSetGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl FunctionSetGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self { app_module }
    }
}
impl FunctionSetGrpc for FunctionSetGrpcImpl {
    fn app(&self) -> &FunctionSetAppImpl {
        &self.app_module.function_set_app
    }
}

// use tracing
impl Tracing for FunctionSetGrpcImpl {}
