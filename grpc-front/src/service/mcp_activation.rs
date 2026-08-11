use std::sync::Arc;

use app::app::function::function_set::{FunctionSetApp, FunctionSetAppImpl};
use tonic::{Request, Response, Status};

use crate::proto::jobworkerp::mcp::service::mcp_activation_service_server::McpActivationService;
use crate::proto::jobworkerp::mcp::service::{ActivateMcpRequest, ActivateMcpResponse};

/// Loopback-only control surface used by the Lookback launcher after it has
/// registered and verified the selected FunctionSet.
#[derive(Clone)]
pub struct McpActivationGrpcImpl {
    function_set_app: Arc<FunctionSetAppImpl>,
}

impl McpActivationGrpcImpl {
    pub fn new(function_set_app: Arc<FunctionSetAppImpl>) -> Self {
        Self { function_set_app }
    }
}

#[tonic::async_trait]
impl McpActivationService for McpActivationGrpcImpl {
    async fn activate(
        &self,
        request: Request<ActivateMcpRequest>,
    ) -> Result<Response<ActivateMcpResponse>, Status> {
        let request = request.into_inner();
        if request.function_set_name.trim().is_empty() {
            return Err(Status::invalid_argument("function_set_name must not be empty"));
        }
        let exists = self
            .function_set_app
            .find_function_set_by_name(&request.function_set_name)
            .await
            .map_err(|error| Status::internal(error.to_string()))?
            .is_some();
        if !exists {
            return Err(Status::not_found("FunctionSet does not exist"));
        }

        mcp_server::activate_deferred_mcp(&request.secret, request.function_set_name)
            .await
            .map(|mcp_addr| Response::new(ActivateMcpResponse { mcp_addr }))
            .map_err(|error| Status::failed_precondition(error.to_string()))
    }
}
