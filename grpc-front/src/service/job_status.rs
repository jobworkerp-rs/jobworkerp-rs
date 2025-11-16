use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::service::job_processing_status_service_server::JobProcessingStatusService;
use crate::proto::jobworkerp::service::{
    JobProcessingStatusResponse, OptionalJobProcessingStatusResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::JOB_STATUS_CONFIG;
use proto::jobworkerp::data::{Empty, JobId};
use tonic::Response;

pub trait JobProcessingStatusGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}

#[tonic::async_trait]
impl<T: JobProcessingStatusGrpc + Tracing + Send + Debug + Sync + 'static>
    JobProcessingStatusService for T
{
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<OptionalJobProcessingStatusResponse>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find", &request);
        let req = request.get_ref();
        match self.app().find_job_status(req).await {
            Ok(res) => Ok(Response::new(OptionalJobProcessingStatusResponse {
                status: res.map(|a| a as i32),
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindAllStream = BoxStream<'static, Result<JobProcessingStatusResponse, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_all"))]
    async fn find_all(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<Self::FindAllStream>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find_all", &request);
        match self.app().find_all_job_status().await {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for (i, s) in list {
                    yield Ok(JobProcessingStatusResponse { id: Some(i), status: s.into() })
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }

    // New method (Sprint 3) - Stub implementation
    type FindByConditionStream = BoxStream<
        'static,
        Result<crate::proto::jobworkerp::service::JobProcessingStatusDetailResponse, tonic::Status>,
    >;

    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_by_condition")
    )]
    async fn find_by_condition(
        &self,
        request: tonic::Request<crate::proto::jobworkerp::service::FindJobProcessingStatusRequest>,
    ) -> Result<tonic::Response<Self::FindByConditionStream>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find_by_condition", &request);
        if !JOB_STATUS_CONFIG.rdb_indexing_enabled {
            return Err(tonic::Status::unimplemented(concat!(
                "Job processing status RDB indexing is disabled. ",
                "Set JOB_STATUS_RDB_INDEXING=true to enable find_by_condition."
            )));
        }
        let req = request.get_ref();

        // Convert Option<i32> to Option<JobProcessingStatus>
        let status = req
            .status
            .and_then(|s| proto::jobworkerp::data::JobProcessingStatus::try_from(s).ok());

        match self
            .app()
            .find_by_condition(
                status,
                req.worker_id,
                req.channel.clone(),
                req.min_elapsed_time_ms,
                req.limit.unwrap_or(100),
                req.offset.unwrap_or(0),
                req.descending.unwrap_or(false),
            )
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for detail in list {
                    // Convert infra::JobProcessingStatusDetail to proto::JobProcessingStatusDetailResponse
                    let proto_response = crate::proto::jobworkerp::service::JobProcessingStatusDetailResponse {
                        id: Some(detail.job_id),
                        status: detail.status.into(),
                        worker_id: detail.worker_id,
                        channel: detail.channel,
                        priority: detail.priority,
                        enqueue_time: detail.enqueue_time,
                        start_time: detail.start_time,
                        pending_time: detail.pending_time,
                        is_streamable: Some(detail.is_streamable),
                        broadcast_results: Some(detail.broadcast_results),
                        updated_at: detail.updated_at,
                    };
                    yield Ok(proto_response)
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobProcessingStatusGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobProcessingStatusGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobProcessingStatusGrpcImpl { app_module }
    }
}
impl JobProcessingStatusGrpc for JobProcessingStatusGrpcImpl {
    fn app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}

// use tracing
impl Tracing for JobProcessingStatusGrpcImpl {}
