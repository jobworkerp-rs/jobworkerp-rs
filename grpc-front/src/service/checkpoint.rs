use crate::proto::jobworkerp::data::{
    CheckPoint, TaskCheckPointContext, WorkflowCheckPointContext,
};
use crate::proto::jobworkerp::service::checkpoint_service_server::CheckpointService;
use crate::proto::jobworkerp::service::{
    GetCheckpointRequest, OptionalCheckpointResponse, SuccessResponse, UpdateCheckpointRequest,
};
use crate::service::error_handle::handle_error;
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::workflow::execute::checkpoint::{
    CheckPointContext as InternalCheckPointContext,
    TaskCheckPointContext as InternalTaskCheckPointContext,
    WorkflowCheckPointContext as InternalWorkflowCheckPointContext,
};
use app_wrapper::workflow::execute::context::WorkflowPosition;
use app_wrapper::workflow::execute::task::ExecutionId;
use command_utils::trace::Tracing;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub struct CheckpointGrpcImpl {
    app_wrapper_module: Arc<AppWrapperModule>,
}

impl CheckpointGrpcImpl {
    pub fn new(app_wrapper_module: Arc<AppWrapperModule>) -> Self {
        Self { app_wrapper_module }
    }

    fn to_proto_checkpoint(
        ctx: InternalCheckPointContext,
    ) -> CheckPoint {
        CheckPoint {
            workflow: Some(WorkflowCheckPointContext {
                name: ctx.workflow.name,
                input: ctx.workflow.input.to_string(),
                context_variables: serde_json::to_string(&ctx.workflow.context_variables).unwrap_or_default(),
            }),
            task: Some(TaskCheckPointContext {
                input: ctx.task.input.to_string(),
                output: ctx.task.output.to_string(),
                context_variables: serde_json::to_string(&ctx.task.context_variables).unwrap_or_default(),
                flow_directive: ctx.task.flow_directive,
            }),
            position: ctx.position.as_json_pointer(),
        }
    }

    fn from_proto_checkpoint(
        proto: CheckPoint,
    ) -> Result<InternalCheckPointContext, anyhow::Error> {
        let workflow = proto.workflow.ok_or(anyhow::anyhow!("Missing workflow context"))?;
        let task = proto.task.ok_or(anyhow::anyhow!("Missing task context"))?;
        
        let position = WorkflowPosition::parse(&proto.position)?;

        Ok(InternalCheckPointContext {
            workflow: InternalWorkflowCheckPointContext {
                name: workflow.name,
                input: Arc::new(serde_json::from_str(&workflow.input)?),
                context_variables: Arc::new(serde_json::from_str(&workflow.context_variables)?),
            },
            task: InternalTaskCheckPointContext {
                input: Arc::new(serde_json::from_str(&task.input)?),
                output: Arc::new(serde_json::from_str(&task.output)?),
                context_variables: Arc::new(serde_json::from_str(&task.context_variables)?),
                flow_directive: task.flow_directive,
            },
            position,
        })
    }
}

// use tracing
impl Tracing for CheckpointGrpcImpl {}

#[tonic::async_trait]
impl CheckpointService for CheckpointGrpcImpl {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "get"))]
    /// Get checkpoint by execution_id, workflow_name, and position.
    async fn get(
        &self,
        request: Request<GetCheckpointRequest>,
    ) -> Result<Response<OptionalCheckpointResponse>, Status> {
        let _s = Self::trace_request("checkpoint", "get", &request);
        let req = request.into_inner();
        let execution_id = ExecutionId { value: req.execution_id };
        let workflow_name = req.workflow_name;
        let position = req.position;

        // Try Redis first, then Memory
        let repo: &dyn app_wrapper::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId = 
        if let Some(redis_repo) = &self.app_wrapper_module.repositories.redis_checkpoint_repository {
            redis_repo.as_ref()
        } else {
             self.app_wrapper_module.repositories.memory_checkpoint_repository.as_ref()
        };

        match repo.get_checkpoint_with_id(&execution_id, &workflow_name, &position).await {
            Ok(Some(ctx)) => Ok(Response::new(OptionalCheckpointResponse {
                checkpoint: Some(Self::to_proto_checkpoint(ctx)),
            })),
            Ok(None) => Ok(Response::new(OptionalCheckpointResponse { checkpoint: None })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "update"))]
    /// Update checkpoint. This allows modifying the context variables of the checkpoint.
    async fn update(
        &self,
        request: Request<UpdateCheckpointRequest>,
    ) -> Result<Response<SuccessResponse>, Status> {
        let _s = Self::trace_request("checkpoint", "update", &request);
        let req = request.into_inner();
        let execution_id = ExecutionId { value: req.execution_id };
        let workflow_name = req.workflow_name;
        let checkpoint_proto = req.checkpoint.ok_or(Status::invalid_argument("Missing checkpoint"))?;
        
        let checkpoint = Self::from_proto_checkpoint(checkpoint_proto)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;

        let repo: &dyn app_wrapper::workflow::execute::checkpoint::repository::CheckPointRepositoryWithId = 
        if let Some(redis_repo) = &self.app_wrapper_module.repositories.redis_checkpoint_repository {
             redis_repo.as_ref()
        } else {
             self.app_wrapper_module.repositories.memory_checkpoint_repository.as_ref()
        };

        match repo.save_checkpoint_with_id(&execution_id, &workflow_name, &checkpoint).await {
             Ok(_) => Ok(Response::new(SuccessResponse { is_success: true })),
             Err(e) => Err(handle_error(&e)),
        }
    }
}
