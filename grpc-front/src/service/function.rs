use anyhow::Result;
use futures::StreamExt;
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};

// Pre-compiled regex pattern for worker name validation (performance optimization)
static WORKER_NAME_PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-zA-Z][a-zA-Z0-9_-]*$")
        .expect("Failed to compile worker name validation regex")
});

use crate::proto::jobworkerp::function::data::{FunctionId, FunctionResult, FunctionSpecs};
use crate::proto::jobworkerp::function::service::function_service_server::FunctionService;
use crate::proto::jobworkerp::function::service::{
    CreateWorkerRequest, CreateWorkerResponse, CreateWorkflowRequest, CreateWorkflowResponse,
    FindFunctionByNameRequest, FindFunctionRequest, FindFunctionSetRequest, FunctionCallRequest,
    OptionalFunctionSpecsResponse,
};
use crate::service::error_handle::handle_error;
use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use app::app::function::{FunctionApp, FunctionAppImpl};
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use tonic::Response;

pub trait FunctionGrpc {
    fn function_app(&self) -> &Arc<FunctionAppImpl>;
    fn function_set_app(&self) -> &Arc<FunctionSetAppImpl>;
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
    fn validate_function_id(&self, function_id: &FunctionId) -> Result<(), tonic::Status> {
        use crate::proto::jobworkerp::function::data::function_id;

        match &function_id.id {
            Some(function_id::Id::RunnerId(runner_id)) => {
                if runner_id.value <= 0 {
                    return Err(tonic::Status::invalid_argument(
                        "runner_id must be greater than 0",
                    ));
                }
                Ok(())
            }
            Some(function_id::Id::WorkerId(worker_id)) => {
                if worker_id.value <= 0 {
                    return Err(tonic::Status::invalid_argument(
                        "worker_id must be greater than 0",
                    ));
                }
                Ok(())
            }
            None => Err(tonic::Status::invalid_argument("id must be specified")),
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

    #[allow(clippy::result_large_err)]
    fn validate_create_worker_request(
        &self,
        req: &CreateWorkerRequest,
    ) -> Result<(), tonic::Status> {
        use crate::proto::jobworkerp::function::service::create_worker_request;

        // Validate that runner is specified
        if req.runner.is_none() {
            return Err(tonic::Status::invalid_argument(
                "either runner_name or runner_id must be specified",
            ));
        }

        // Validate runner_id if specified
        if let Some(create_worker_request::Runner::RunnerId(runner_id)) = &req.runner {
            if runner_id.value <= 0 {
                return Err(tonic::Status::invalid_argument(
                    "runner_id must be greater than 0",
                ));
            }
        }

        // Validate runner_name if specified
        if let Some(create_worker_request::Runner::RunnerName(runner_name)) = &req.runner {
            if runner_name.is_empty() {
                return Err(tonic::Status::invalid_argument(
                    "runner_name cannot be empty",
                ));
            }
            if runner_name.len() > 128 {
                return Err(tonic::Status::invalid_argument(
                    "runner_name too long (max 128 characters)",
                ));
            }
        }

        // Validate worker name
        if req.name.is_empty() {
            return Err(tonic::Status::invalid_argument("name cannot be empty"));
        }
        if req.name.len() > 255 {
            return Err(tonic::Status::invalid_argument(
                "name too long (max 255 characters)",
            ));
        }
        // Naming convention check (must start with letter, contain only alphanumeric, underscore, hyphen)
        if !WORKER_NAME_PATTERN.is_match(&req.name) {
            return Err(tonic::Status::invalid_argument(
                format!("Worker name '{}' is invalid. Must start with a letter (a-z, A-Z) and contain only alphanumeric characters, underscores, and hyphens", req.name)
            ));
        }

        Ok(())
    }

    #[allow(clippy::result_large_err)]
    fn validate_create_workflow_request(
        &self,
        req: &CreateWorkflowRequest,
    ) -> Result<(), tonic::Status> {
        use crate::proto::jobworkerp::function::service::create_workflow_request;

        // Validate that workflow_source is specified
        if req.workflow_source.is_none() {
            return Err(tonic::Status::invalid_argument(
                "either workflow_data or workflow_url must be specified",
            ));
        }

        // Validate workflow_data if specified
        if let Some(create_workflow_request::WorkflowSource::WorkflowData(data)) =
            &req.workflow_source
        {
            if data.is_empty() {
                return Err(tonic::Status::invalid_argument(
                    "workflow_data cannot be empty",
                ));
            }
        }

        // Validate workflow_url if specified
        if let Some(create_workflow_request::WorkflowSource::WorkflowUrl(url)) =
            &req.workflow_source
        {
            if url.is_empty() {
                return Err(tonic::Status::invalid_argument(
                    "workflow_url cannot be empty",
                ));
            }
        }

        // Validate worker name if specified
        if let Some(name) = &req.name {
            if name.is_empty() {
                return Err(tonic::Status::invalid_argument("name cannot be empty"));
            }
            if name.len() > 255 {
                return Err(tonic::Status::invalid_argument(
                    "name too long (max 255 characters)",
                ));
            }
            // Naming convention check (must start with letter, contain only alphanumeric, underscore, hyphen)
            if !WORKER_NAME_PATTERN.is_match(name) {
                return Err(tonic::Status::invalid_argument(
                    format!("Worker name '{}' is invalid. Must start with a letter (a-z, A-Z) and contain only alphanumeric characters, underscores, and hyphens", name)
                ));
            }
        }

        Ok(())
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

        // Use FunctionSetApp to get the list of functions by set
        let functions = match self
            .function_set_app()
            .find_functions_by_set(&req.name)
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
            .and_then(|o| o.timeout_ms.map(|t| (t / 1000) as u32))
            .unwrap_or(DEFAULT_TIMEOUT_SEC);
        let streaming = options.as_ref().and_then(|o| o.streaming).unwrap_or(false);
        let meta = Arc::new(options.map(|o| o.metadata).unwrap_or_default());

        // Clone the function app to avoid lifetime issues
        let function_app = self.function_app().clone();

        // Note: args_json may be double-encoded JSON string from some clients (e.g., LLM clients)
        // First serde_json::to_value converts protobuf string to serde_json::Value::String
        // Then we attempt to parse the inner JSON if it's a valid JSON string
        // This handles cases where clients send: "'{\"key\":\"value\"}'" instead of: {\"key\":\"value\"}
        let args_json = serde_json::to_value(req.args_json)
            .map_err(|e| tonic::Status::invalid_argument(format!("Invalid args_json: {e}")))?;
        let args_json = match args_json {
            serde_json::Value::String(json_str) => {
                // Re-parse as JSON string to handle double-encoded JSON
                match serde_json::from_str::<serde_json::Value>(&json_str) {
                    Ok(parsed) => parsed,
                    Err(_) => serde_json::Value::String(json_str), // Use original string value when parsing fails (treat as plain string)
                }
            }
            other => other, // Keep non-String values as-is (already proper JSON structures)
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
        request: tonic::Request<FunctionId>,
    ) -> Result<tonic::Response<OptionalFunctionSpecsResponse>, tonic::Status> {
        use crate::proto::jobworkerp::function::data::function_id;

        let _s = Self::trace_request("function", "find", &request);
        let function_id = request.into_inner();

        // Validate request
        self.validate_function_id(&function_id)?;

        match function_id.id {
            Some(function_id::Id::RunnerId(runner_id)) => {
                match self
                    .function_app()
                    .find_function_by_runner_id(&runner_id)
                    .await
                {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs),
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            Some(function_id::Id::WorkerId(worker_id)) => {
                match self
                    .function_app()
                    .find_function_by_worker_id(&worker_id)
                    .await
                {
                    Ok(Some(function_specs)) => Ok(Response::new(OptionalFunctionSpecsResponse {
                        data: Some(function_specs),
                    })),
                    Ok(None) => Ok(Response::new(OptionalFunctionSpecsResponse { data: None })),
                    Err(e) => Err(handle_error(&e)),
                }
            }
            None => unreachable!("Validation should catch this case"),
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

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "create_worker"))]
    async fn create_worker(
        &self,
        request: tonic::Request<CreateWorkerRequest>,
    ) -> Result<tonic::Response<CreateWorkerResponse>, tonic::Status> {
        let _s = Self::trace_request("function", "create_worker", &request);
        let req = request.into_inner();

        // Validate request
        self.validate_create_worker_request(&req)?;

        // Extract runner information
        use crate::proto::jobworkerp::function::service::create_worker_request;
        let (runner_name, runner_id) = match req.runner {
            Some(create_worker_request::Runner::RunnerName(name)) => (Some(name), None),
            Some(create_worker_request::Runner::RunnerId(id)) => (None, Some(id)),
            None => unreachable!("Validation should catch this case"),
        };

        // Call FunctionApp::create_worker_from_runner
        match self
            .function_app()
            .create_worker_from_runner(
                runner_name,
                runner_id,
                req.name.clone(),
                req.description,
                req.settings_json,
                req.worker_options,
            )
            .await
        {
            Ok((worker_id, worker_name)) => Ok(Response::new(CreateWorkerResponse {
                worker_id: Some(worker_id),
                worker_name,
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "create_workflow")
    )]
    async fn create_workflow(
        &self,
        request: tonic::Request<CreateWorkflowRequest>,
    ) -> Result<tonic::Response<CreateWorkflowResponse>, tonic::Status> {
        let _s = Self::trace_request("function", "create_workflow", &request);
        let req = request.into_inner();

        // Validate request
        self.validate_create_workflow_request(&req)?;

        // Extract workflow source
        use crate::proto::jobworkerp::function::service::create_workflow_request;
        let (workflow_data, workflow_url) = match req.workflow_source {
            Some(create_workflow_request::WorkflowSource::WorkflowData(data)) => (Some(data), None),
            Some(create_workflow_request::WorkflowSource::WorkflowUrl(url)) => (None, Some(url)),
            None => unreachable!("Validation should catch this case"),
        };

        // Call FunctionApp::create_workflow_from_definition
        match self
            .function_app()
            .create_workflow_from_definition(
                workflow_data,
                workflow_url,
                req.name,
                req.worker_options,
            )
            .await
        {
            Ok((worker_id, worker_name, workflow_name)) => {
                Ok(Response::new(CreateWorkflowResponse {
                    worker_id: Some(worker_id),
                    worker_name,
                    workflow_name,
                }))
            }
            Err(e) => Err(handle_error(&e)),
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

    fn function_set_app(&self) -> &Arc<FunctionSetAppImpl> {
        &self.app_module.function_set_app
    }
}

impl FunctionRequestValidator for FunctionGrpcImpl {}

// Implement tracing for FunctionGrpcImpl
impl Tracing for FunctionGrpcImpl {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::jobworkerp::function::service::create_worker_request;
    use crate::proto::jobworkerp::function::service::create_workflow_request;

    struct TestValidator;
    impl FunctionRequestValidator for TestValidator {}

    #[test]
    fn test_validate_create_worker_request_with_invalid_names() {
        let validator = TestValidator;

        // Test various invalid names
        let too_long = "a".repeat(256);
        let invalid_names = vec![
            "",             // Empty
            "123invalid",   // Starts with number
            "invalid name", // Contains space
            "invalid@name", // Contains special char
            "invalid.name", // Contains dot
            &too_long,      // Too long (>255 chars)
        ];

        for invalid_name in invalid_names {
            let req = CreateWorkerRequest {
                runner: Some(create_worker_request::Runner::RunnerName(
                    "COMMAND".to_string(),
                )),
                name: invalid_name.to_string(),
                description: None,
                settings_json: None,
                worker_options: None,
            };

            let result = validator.validate_create_worker_request(&req);
            assert!(
                result.is_err(),
                "Should fail with invalid name: {}",
                invalid_name
            );
        }
    }

    #[test]
    fn test_validate_create_worker_request_with_valid_names() {
        let validator = TestValidator;

        // Test various valid names
        let max_length = "a".repeat(255);
        let valid_names = vec![
            "validName",
            "valid_name",
            "valid-name",
            "ValidName123",
            "a",         // Single char
            &max_length, // Max length (255 chars)
        ];

        for valid_name in valid_names {
            let req = CreateWorkerRequest {
                runner: Some(create_worker_request::Runner::RunnerName(
                    "COMMAND".to_string(),
                )),
                name: valid_name.to_string(),
                description: None,
                settings_json: None,
                worker_options: None,
            };

            let result = validator.validate_create_worker_request(&req);
            assert!(
                result.is_ok(),
                "Should succeed with valid name: {}, error: {:?}",
                valid_name,
                result
            );
        }
    }

    #[test]
    fn test_validate_create_workflow_request_with_invalid_names() {
        let validator = TestValidator;

        let too_long = "a".repeat(256);
        let invalid_names = vec![
            "",             // Empty
            "123invalid",   // Starts with number
            "invalid name", // Contains space
            "invalid@name", // Contains special char
            "invalid.name", // Contains dot
            &too_long,      // Too long (>255 chars)
        ];

        for invalid_name in invalid_names {
            let req = CreateWorkflowRequest {
                workflow_source: Some(create_workflow_request::WorkflowSource::WorkflowData(
                    r#"{"document":{"dsl":"1.0.0","namespace":"test","name":"test"},"do":[]}"#
                        .to_string(),
                )),
                name: Some(invalid_name.to_string()),
                worker_options: None,
            };

            let result = validator.validate_create_workflow_request(&req);
            assert!(
                result.is_err(),
                "Should fail with invalid name: {}",
                invalid_name
            );
        }
    }

    #[test]
    fn test_validate_create_workflow_request_with_valid_names() {
        let validator = TestValidator;

        let max_length = "a".repeat(255);
        let valid_names = vec![
            "validName",
            "valid_name",
            "valid-name",
            "ValidName123",
            "a",         // Single char
            &max_length, // Max length (255 chars)
        ];

        for valid_name in valid_names {
            let req = CreateWorkflowRequest {
                workflow_source: Some(create_workflow_request::WorkflowSource::WorkflowData(
                    r#"{"document":{"dsl":"1.0.0","namespace":"test","name":"test"},"do":[]}"#
                        .to_string(),
                )),
                name: Some(valid_name.to_string()),
                worker_options: None,
            };

            let result = validator.validate_create_workflow_request(&req);
            assert!(
                result.is_ok(),
                "Should succeed with valid name: {}, error: {:?}",
                valid_name,
                result
            );
        }
    }
}
