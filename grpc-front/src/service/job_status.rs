use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::service::job_status_service_server::JobStatusService;
use crate::proto::jobworkerp::service::{JobStatusResponse, OptionalJobStatusResponse};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra_utils::trace::Tracing;
use proto::jobworkerp::data::{Empty, JobId};
use tonic::Response;

pub trait JobStatusGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}

#[tonic::async_trait]
impl<T: JobStatusGrpc + Tracing + Send + Debug + Sync + 'static> JobStatusService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<OptionalJobStatusResponse>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find", &request);
        let req = request.get_ref();
        match self.app().find_job_status(req).await {
            Ok(res) => Ok(Response::new(OptionalJobStatusResponse {
                status: res.map(|a| a as i32),
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindAllStream = BoxStream<'static, Result<JobStatusResponse, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_all"))]
    async fn find_all(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<tonic::Response<Self::FindAllStream>, tonic::Status> {
        let _s = Self::trace_request("job_status", "find_all", &request);
        match self.app().find_all_job_status().await {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for (i, s) in list {
                    yield Ok(JobStatusResponse { id: Some(i), status: s.into() })
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobStatusGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobStatusGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobStatusGrpcImpl { app_module }
    }
}
impl JobStatusGrpc for JobStatusGrpcImpl {
    fn app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}

// use tracing
impl Tracing for JobStatusGrpcImpl {}
