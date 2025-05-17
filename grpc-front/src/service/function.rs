use anyhow::Result;
use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::function::data::FunctionSpecs;
use crate::proto::jobworkerp::function::service::function_service_server::FunctionService;
use crate::proto::jobworkerp::function::service::{FindFunctionRequest, FindFunctionSetRequest};
use crate::service::error_handle::handle_error;
use app::app::function::{FunctionApp, FunctionAppImpl};
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::trace::Tracing;
use tonic::Response;

pub trait FunctionGrpc {
    fn function_app(&self) -> &Arc<FunctionAppImpl>;
}

#[tonic::async_trait]
impl<T: FunctionGrpc + Tracing + Send + Debug + Sync + 'static> FunctionService for T {
    type FindListStream = BoxStream<'static, Result<FunctionSpecs, tonic::Status>>;

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindFunctionRequest>,
    ) -> Result<tonic::Response<<T as FunctionService>::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("function", "find_list", &request);
        let req = request.into_inner();

        // Use the FunctionApp to get the list of functions
        let functions = match self
            .function_app()
            .find_functions(req.exclude_runner, req.exclude_worker)
            .await
        {
            Ok(funcs) => funcs,
            Err(e) => return Err(handle_error(&e)),
        };

        // Return stream of functions
        Ok(Response::new(Box::pin(stream! {
            for function in functions {
                yield Ok(function);
            }
        })))
    }
    type FindListBySetStream = BoxStream<'static, Result<FunctionSpecs, tonic::Status>>;
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_list_by_set")
    )]
    async fn find_list_by_set(
        &self,
        request: tonic::Request<FindFunctionSetRequest>,
    ) -> Result<tonic::Response<<T as FunctionService>::FindListBySetStream>, tonic::Status> {
        let _s = Self::trace_request("function", "find_list_by_set", &request);
        let req = request.into_inner();

        // Use the FunctionApp to get the list of functions by set
        let functions = match self.function_app().find_functions_by_set(&req.name).await {
            Ok(funcs) => funcs,
            Err(e) => return Err(handle_error(&e)),
        };

        // Return stream of functions
        Ok(Response::new(Box::pin(stream! {
            for function in functions {
                yield Ok(function);
            }
        })))
    }
}

#[derive(DebugStub)]
pub(crate) struct FunctionGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl FunctionGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        FunctionGrpcImpl { app_module }
    }
}

impl FunctionGrpc for FunctionGrpcImpl {
    fn function_app(&self) -> &Arc<FunctionAppImpl> {
        &self.app_module.function_app
    }
}

// Implement tracing for FunctionGrpcImpl
impl Tracing for FunctionGrpcImpl {}
