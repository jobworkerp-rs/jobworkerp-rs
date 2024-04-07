use std::sync::Arc;
use std::{fmt::Debug, time::Duration};

use crate::proto::jobworkerp::data::Priority;
use crate::proto::jobworkerp::service::job_request::Worker;
use crate::proto::jobworkerp::service::job_service_server::JobService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, CreateJobResponse, FindListRequest, JobRequest,
    OptionalJobResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::util::option::Exists;
use futures::stream::BoxStream;
use infra::error::JobWorkerError;
use infra_utils::trace::Tracing;
use proto::jobworkerp::data::{Job, JobId};
use tonic::Response;

pub trait JobGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}

const DEFAULT_TTL: Duration = Duration::from_secs(30);
const LIST_TTL: Duration = Duration::from_secs(5);

pub trait RequestValidator {
    // almost no timeout (1 year after)
    const DEFAULT_TIMEOUT: u64 = 1000 * 60 * 60 * 24 * 365;
    fn validate_create(&self, req: &JobRequest) -> Result<(), tonic::Status> {
        if req.worker.is_none() {
            return Err(tonic::Status::invalid_argument(
                "worker_id or worker_name is required",
            ));
        }
        // run_after_time should be positive or none
        if req.run_after_time.exists(|t| t < 0) {
            return Err(tonic::Status::invalid_argument(
                "run_after_time should be positive",
            ));
        }
        match req.worker.as_ref() {
            Some(Worker::WorkerName(n)) if n.is_empty() => Err(tonic::Status::invalid_argument(
                "worker_name should not be empty",
            )),
            None => Err(tonic::Status::invalid_argument(
                "worker_id or worker_name is required",
            )),
            _ => Ok(()),
        }?;
        match req.arg.as_ref() {
            Some(_) => Ok(()), // XXX validation each runner args?
            _ => Err(tonic::Status::invalid_argument(
                "worker_id or worker_name is required",
            )),
        }?;
        Ok(())
    }
}

#[tonic::async_trait]
impl<T: JobGrpc + RequestValidator + Tracing + Send + Debug + Sync + 'static> JobService for T {
    #[tracing::instrument]
    async fn enqueue(
        &self,
        request: tonic::Request<JobRequest>,
    ) -> Result<tonic::Response<CreateJobResponse>, tonic::Status> {
        let _span = Self::trace_request("job", "create", &request);
        let req = request.get_ref();
        self.validate_create(req)?;
        let res = match req.worker.as_ref() {
            Some(Worker::WorkerId(id)) => {
                self.app()
                    .enqueue_job(
                        Some(id),
                        None,
                        req.arg.clone(),
                        req.uniq_key.clone(),
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                    )
                    .await
            }
            Some(Worker::WorkerName(name)) => {
                self.app()
                    .enqueue_job(
                        None,
                        Some(name),
                        req.arg.clone(),
                        req.uniq_key.clone(),
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                    )
                    .await
            }
            None => {
                Err(JobWorkerError::InvalidParameter("should not reach hear!!".to_string()).into())
            }
        };
        match res {
            Ok((id, res)) => Ok(Response::new(CreateJobResponse {
                id: Some(id),
                result: res,
            })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn delete(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("job", "delete", &request);
        let req = request.get_ref();
        match self.app().delete_job(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument]
    async fn find(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<OptionalJobResponse>, tonic::Status> {
        let _s = Self::trace_request("job", "find", &request);
        let req = request.get_ref();
        match self.app().find_job(req, Some(DEFAULT_TTL)).await {
            Ok(res) => Ok(Response::new(OptionalJobResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<Job, tonic::Status>>;
    #[tracing::instrument]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("job", "find_list", &request);
        let req = request.get_ref();
        let ttl = if req.limit.is_some() {
            LIST_TTL
        } else {
            DEFAULT_TTL
        };
        // TODO streaming?
        match self
            .app()
            .find_job_list(req.limit.as_ref(), req.offset.as_ref(), Some(ttl))
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
    #[tracing::instrument]
    async fn count(
        &self,
        request: tonic::Request<CountCondition>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("job", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobGrpcImpl { app_module }
    }
}
impl JobGrpc for JobGrpcImpl {
    fn app(&self) -> &Arc<dyn JobApp + 'static> {
        &self.app_module.job_app
    }
}

// use tracing
impl Tracing for JobGrpcImpl {}

impl RequestValidator for JobGrpcImpl {}

// unit test for RequestValidator::validate_create method
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::jobworkerp::data::{Priority, RunnerArg, WorkerId};
    use crate::proto::jobworkerp::service::job_request::Worker;
    use crate::proto::jobworkerp::service::JobRequest;

    struct Validator;
    impl RequestValidator for Validator {}

    #[test]
    fn test_validate_create_ok() {
        let v = Validator {};
        let jarg = RunnerArg {
            data: Some(proto::jobworkerp::data::runner_arg::Data::Command(
                proto::jobworkerp::data::CommandArg {
                    args: vec!["fuga".to_string()],
                },
            )),
        };
        let mut req = JobRequest {
            worker: Some(Worker::WorkerId(WorkerId { value: 1 })),
            arg: Some(jarg),
            ..Default::default()
        };
        assert!(v.validate_create(&req).is_ok());
        req.worker = Some(Worker::WorkerName("a".to_string()));
        assert!(v.validate_create(&req).is_ok());
        req.timeout = Some(1);
        assert!(v.validate_create(&req).is_ok());
        req.run_after_time = Some(1);
        assert!(v.validate_create(&req).is_ok());
        req.priority = Some(Priority::High as i32);
        assert!(v.validate_create(&req).is_ok());
    }
    #[test]
    fn test_validate_create_ng() {
        let v = Validator {};
        let jarg = RunnerArg {
            data: Some(proto::jobworkerp::data::runner_arg::Data::Command(
                proto::jobworkerp::data::CommandArg {
                    args: vec!["fuga".to_string()],
                },
            )),
        };
        let reqr = JobRequest {
            worker: Some(Worker::WorkerId(WorkerId { value: 1 })),
            arg: Some(jarg),
            ..Default::default()
        };
        assert!(v.validate_create(&reqr).is_ok());
        let mut req = reqr.clone();
        req.worker = Some(Worker::WorkerName("".to_string()));
        assert!(v.validate_create(&req).is_err());
        let mut req = reqr.clone();
        req.worker = None;
        assert!(v.validate_create(&req).is_err());
        let mut req = reqr.clone();
        req.timeout = Some(0);
        assert!(v.validate_create(&req).is_ok());
        let mut req = reqr.clone();
        req.run_after_time = Some(-1);
        assert!(v.validate_create(&req).is_err());
        let mut req = reqr.clone();
        req.run_after_time = Some(0);
        assert!(v.validate_create(&req).is_ok());
        let mut req = reqr.clone();
        req.priority = Some(Priority::High as i32);
        assert!(v.validate_create(&req).is_ok());
        req.arg = None;
        assert!(v.validate_create(&req).is_err());
    }
}
