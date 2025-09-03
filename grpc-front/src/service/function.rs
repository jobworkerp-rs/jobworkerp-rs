use anyhow::Result;
use futures::StreamExt;
use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::function::data::FunctionResult;
use crate::proto::jobworkerp::function::data::FunctionSpecs;
use crate::proto::jobworkerp::function::service::function_service_server::FunctionService;
use crate::proto::jobworkerp::function::service::{
    FindFunctionByIdRequest, FindFunctionByNameRequest, FindFunctionRequest,
    FindFunctionSetRequest, FunctionCallRequest, OptionalFunctionSpecsResponse,
};
use crate::service::error_handle::handle_error;
use app::app::function::{FunctionApp, FunctionAppImpl};
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use tonic::Response;

pub trait FunctionGrpc {
    fn function_app(&self) -> &Arc<FunctionAppImpl>;
}

pub trait FunctionRequestValidator {
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

    #[allow(clippy::result_large_err)]
    fn validate_find_by_id_request(
        &self,
        req: &FindFunctionByIdRequest,
    ) -> Result<(), tonic::Status> {
        match &req.id {
            Some(crate::proto::jobworkerp::function::service::find_function_by_id_request::Id::RunnerId(runner_id)) => {
                if runner_id.value <= 0 {
                    return Err(tonic::Status::invalid_argument("id must be greater than 0"));
                }
                Ok(())
            }
            Some(crate::proto::jobworkerp::function::service::find_function_by_id_request::Id::WorkerId(worker_id)) => {
                if worker_id.value <= 0 {
                    return Err(tonic::Status::invalid_argument("id must be greater than 0"));
                }
                Ok(())
            }
            None => Err(tonic::Status::invalid_argument("id must be specified"))
        }
    }

    #[allow(clippy::result_large_err)]
    fn validate_find_by_name_request(
        &self,
        req: &FindFunctionByNameRequest,
    ) -> Result<(), tonic::Status> {
        match &req.name {
            Some(crate::proto::jobworkerp::function::service::find_function_by_name_request::Name::RunnerName(runner_name)) => {
                if runner_name.is_empty() {
                    return Err(tonic::Status::invalid_argument("name cannot be empty"));
                }
                if runner_name.len() > 128 {
                    return Err(tonic::Status::invalid_argument("name too long (max 128 characters)"));
                }
                Ok(())
            }
            Some(crate::proto::jobworkerp::function::service::find_function_by_name_request::Name::WorkerName(worker_name)) => {
                if worker_name.is_empty() {
                    return Err(tonic::Status::invalid_argument("name cannot be empty"));
                }
                if worker_name.len() > 128 {
                    return Err(tonic::Status::invalid_argument("name too long (max 128 characters)"));
                }
                Ok(())
            }
            None => Err(tonic::Status::invalid_argument("name must be specified"))
        }
    }
}
const DEFAULT_TIMEOUT_SEC: u32 = 300; // Default timeout for function calls in seconds

#[tonic::async_trait]
impl<T: FunctionGrpc + FunctionRequestValidator + Tracing + Send + Debug + Sync + 'static>
    FunctionService for T
{
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
            .map_err(|e| tonic::Status::invalid_argument(format!("Invalid args_json: {e}")))?;
        let args_json = match args_json {
            serde_json::Value::String(json_str) => {
                // Re-parse as JSON string
                match serde_json::from_str::<serde_json::Value>(&json_str) {
                    Ok(parsed) => parsed,
                    Err(_) => serde_json::Value::String(json_str), // Use original string value when parsing fails
                }
            }
            other => other, // Keep non-String values as-is
        };

        let uniq_key = req.uniq_key;

        // Extract parameters from request and create the stream
        let stream = match req.name {
            Some(crate::proto::jobworkerp::function::service::function_call_request::Name::RunnerName(name)) => {
                let (runner_settings, worker_options) = if let Some(params) = req.runner_parameters{
                    (Some(serde_json::to_value(params.settings_json).map_err(
                        |e| tonic::Status::invalid_argument(format!("Invalid settings_json: {e}"))
                    )?),
                     params.worker_options)
                } else {
                   (None, None)
                };
                let runner_settings = match runner_settings {
                    Some(serde_json::Value::String(json_str)) => {
                        // Re-parse as JSON string
                        match serde_json::from_str::<serde_json::Value>(&json_str) {
                            Ok(parsed) => Some(parsed),
                            Err(_) => Some(serde_json::Value::String(json_str)), // Use original string value when parsing fails
                        }
                    },
                    other => other, // Keep non-String values as-is
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

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<FindFunctionByIdRequest>,
    ) -> Result<tonic::Response<OptionalFunctionSpecsResponse>, tonic::Status> {
        let _s = Self::trace_request("function", "find", &request);
        let req = request.into_inner();

        // Validate request
        self.validate_find_by_id_request(&req)?;

        match req.id {
            Some(crate::proto::jobworkerp::function::service::find_function_by_id_request::Id::RunnerId(runner_id)) => {
                match self.function_app().find_function_by_runner_id(&runner_id).await {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs)
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            Some(crate::proto::jobworkerp::function::service::find_function_by_id_request::Id::WorkerId(worker_id)) => {
                match self.function_app().find_function_by_worker_id(&worker_id).await {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs)
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            None => unreachable!("Validation should catch this case")
        }
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_by_name"))]
    async fn find_by_name(
        &self,
        request: tonic::Request<FindFunctionByNameRequest>,
    ) -> Result<tonic::Response<OptionalFunctionSpecsResponse>, tonic::Status> {
        let _s = Self::trace_request("function", "find_by_name", &request);
        let req = request.into_inner();

        // Validate request
        self.validate_find_by_name_request(&req)?;

        match req.name {
            Some(crate::proto::jobworkerp::function::service::find_function_by_name_request::Name::RunnerName(runner_name)) => {
                match self.function_app().find_function_by_runner_name(&runner_name).await {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs)
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            Some(crate::proto::jobworkerp::function::service::find_function_by_name_request::Name::WorkerName(worker_name)) => {
                match self.function_app().find_function_by_worker_name(&worker_name).await {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs)
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            None => unreachable!("Validation should catch this case")
        }
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

impl FunctionRequestValidator for FunctionGrpcImpl {}

// Implement tracing for FunctionGrpcImpl
impl Tracing for FunctionGrpcImpl {}
