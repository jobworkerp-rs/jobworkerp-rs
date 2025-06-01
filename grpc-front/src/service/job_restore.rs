use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::service::JobRestoreRequest;
use crate::proto::jobworkerp::service::{
    job_restore_service_server::JobRestoreService, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::infra::trace::Tracing;
use proto::jobworkerp::data::Job;
use tonic::Response;

pub trait JobRestoreGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}

#[tonic::async_trait]
impl<T: JobRestoreGrpc + Tracing + Send + Debug + Sync + 'static> JobRestoreService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "restore"))]
    async fn restore(
        &self,
        request: tonic::Request<JobRestoreRequest>,
    ) -> std::result::Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("job", "restore", &request);
        let req = request.get_ref();
        match self
            .app()
            .restore_jobs_from_rdb(req.include_grabbed.unwrap_or(false), req.limit.as_ref())
            .await
        {
            Ok(_) => Ok(Response::new(SuccessResponse { is_success: true })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    type FindAllStream = BoxStream<'static, Result<Job, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_all"))]
    async fn find_all(
        &self,
        request: tonic::Request<JobRestoreRequest>,
    ) -> Result<tonic::Response<Self::FindAllStream>, tonic::Status> {
        let _s = Self::trace_request("job", "restore", &request);
        let req: &JobRestoreRequest = request.get_ref();
        // TODO streaming
        match self
            .app()
            .find_restore_jobs_from_rdb(req.include_grabbed.unwrap_or(false), req.limit.as_ref())
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for s in list {
                    yield Ok(s)
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobRestoreGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobRestoreGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobRestoreGrpcImpl { app_module }
    }
}
impl JobRestoreGrpc for JobRestoreGrpcImpl {
    fn app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}

// use tracing
impl Tracing for JobRestoreGrpcImpl {}
