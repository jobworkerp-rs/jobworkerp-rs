use anyhow::Result;
use futures::StreamExt;
use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::function::data::FunctionResult;
use crate::proto::jobworkerp::function::data::FunctionSpecs;
use crate::proto::jobworkerp::function::service::function_service_server::FunctionService;
use crate::proto::jobworkerp::function::service::{
    FindFunctionRequest, FindFunctionSetRequest, FunctionCallRequest,
};
use crate::service::error_handle::handle_error;
use app::app::function::{FunctionApp, FunctionAppImpl};
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::infra::trace::Tracing;
use tonic::Response;

pub trait FunctionGrpc {
    fn function_app(&self) -> &Arc<FunctionAppImpl>;

    #[allow(clippy::result_large_err)]
    fn validate_function_call_request(
        &self,
        req: &FunctionCallRequest,
    ) -> Result<(), tonic::Status> {
        // Validate that name is specified
        if req.name.is_none() {
            return Err(tonic::Status::invalid_argument("name must be specified"));
        }

        Ok(())
    }
}
const DEFAULT_TIMEOUT_SEC: u32 = 300; // Default timeout for function calls in seconds

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
    /// Server streaming response type for the Call method.
    type CallStream = BoxStream<'static, std::result::Result<FunctionResult, tonic::Status>>;

    async fn call(
        &self,
        request: tonic::Request<FunctionCallRequest>,
    ) -> std::result::Result<tonic::Response<Self::CallStream>, tonic::Status> {
        let _s = Self::trace_request("function", "call", &request);
        let req = request.into_inner();
        // Validate request
        self.validate_function_call_request(&req)?;

        let options = req.options;
        let timeout_sec = options
            .as_ref()
            .and_then(|o| (o.timeout_ms.map(|t| (t / 1000) as u32)))
            .unwrap_or(DEFAULT_TIMEOUT_SEC);
        let streaming = options.as_ref().and_then(|o| o.streaming).unwrap_or(false);
        let meta = Arc::new(options.map(|o| o.metadata).unwrap_or_default());

        // Clone the function app to avoid lifetime issues
        let function_app = self.function_app().clone();
        let args_json = serde_json::to_value(req.args_json)
            .map_err(|e| tonic::Status::invalid_argument(format!("Invalid args_json: {}", e)))?;
        let uniq_key = req.uniq_key;

        // Extract parameters from request and create the stream
        let stream = match req.name {
            Some(crate::proto::jobworkerp::function::service::function_call_request::Name::RunnerName(name)) => {
                let (runner_settings, worker_options) = if let Some(params) = req.runner_parameters{
                    (Some(serde_json::to_value(params.settings_json).map_err(
                        |e| tonic::Status::invalid_argument(format!("Invalid settings_json: {}", e))
                    )?),
                     params.worker_options)
                } else {
                   (None, None)
                };

                Box::pin(stream! {
                    let result_stream = function_app.handle_runner_for_front(
                        meta,
                        &name,
                        runner_settings,
                        worker_options,
                        args_json,
                        uniq_key,
                        timeout_sec,
                        streaming,
                    );

                    for await result in result_stream {
                        yield result;
                    }
                }) as BoxStream<'static, Result<FunctionResult, anyhow::Error>>
            }
            Some(crate::proto::jobworkerp::function::service::function_call_request::Name::WorkerName(name)) => {
                Box::pin(stream! {
                    let result_stream = function_app.handle_worker_call_for_front(
                        meta,
                        &name,
                        args_json,
                        uniq_key,
                        timeout_sec,
                        streaming,
                    );

                    for await result in result_stream {
                        yield result;
                    }
                }) as BoxStream<'static, Result<FunctionResult, anyhow::Error>>
            }

            None => return Err(tonic::Status::invalid_argument("name must be specified")),
        };

        Ok(Response::new(Box::pin(stream.map(|result| match result {
            Ok(res) => Ok(res),
            Err(e) => Err(handle_error(&e)),
        }))))
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
