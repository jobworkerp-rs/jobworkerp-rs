use crate::proto::jobworkerp::data::{Priority, ResultOutputItem};
use crate::proto::jobworkerp::service::job_request::Worker;
use crate::proto::jobworkerp::service::job_service_server::JobService;
use crate::proto::jobworkerp::service::{
    CountCondition, CountResponse, CreateJobResponse, FindListRequest, FindQueueListRequest,
    JobAndStatus, JobRequest, OptionalJobResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{Job, JobId};
use std::fmt::Debug;
use std::sync::Arc;
use tonic::metadata::MetadataValue;
use tonic::Response;

pub trait JobGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
}
pub trait RequestValidator {
    // almost no timeout (1 year after)
    const DEFAULT_TIMEOUT: u64 = 1000 * 60 * 60 * 24 * 365;
    #[allow(clippy::result_large_err)]
    fn validate_create(&self, req: &JobRequest) -> Result<(), tonic::Status> {
        if req.worker.is_none() {
            return Err(tonic::Status::invalid_argument(format!(
                "worker_id or worker_name is required: {:?}",
                req
            )));
        }
        // run_after_time should be positive or none
        if req.run_after_time.is_some_and(|t| t < 0) {
            return Err(tonic::Status::invalid_argument(
                "run_after_time should be positive",
            ));
        }
        match req.worker.as_ref() {
            Some(Worker::WorkerName(n)) if n.is_empty() => Err(tonic::Status::invalid_argument(
                "worker_name should not be empty",
            )),
            None => Err(tonic::Status::invalid_argument(format!(
                "worker_name or worker_id is required: {:?}",
                req
            ))),
            _ => Ok(()),
        }?;
        Ok(())
    }
}

#[tonic::async_trait]
impl<T: JobGrpc + RequestValidator + Tracing + Send + Debug + Sync + 'static> JobService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "enqueue"))]
    #[allow(clippy::result_large_err)]
    async fn enqueue(
        &self,
        request: tonic::Request<JobRequest>,
    ) -> Result<tonic::Response<CreateJobResponse>, tonic::Status> {
        let _span = Self::trace_request("job", "create", &request);
        let (metadata, _extensions, req) = request.into_parts();
        let metadata = Arc::new(super::process_metadata(metadata)?);
        self.validate_create(&req)?;
        let res = match req.worker.as_ref() {
            Some(Worker::WorkerId(id)) => {
                self.app()
                    .enqueue_job(
                        metadata,
                        Some(id),
                        None,
                        req.args,
                        req.uniq_key,
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                        None,
                        false,
                    )
                    .await
            }
            Some(Worker::WorkerName(name)) => {
                self.app()
                    .enqueue_job(
                        metadata,
                        None,
                        Some(name),
                        req.args,
                        req.uniq_key,
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                        None,
                        false,
                    )
                    .await
            }
            None => Err(JobWorkerError::InvalidParameter(
                "Invalid worker type: both worker_id and worker_name are None".to_string(),
            )
            .into()),
        };
        match res {
            Ok((id, res, st)) => {
                // if st is some, collect it and return as result
                if let Some(res) = res {
                    if res.data.as_ref().is_some_and(|d| d.output.is_none()) {
                        // if stream is some, collect it and return as result
                        if let Some(_stream) = st {
                            Err(handle_error(
                                &JobWorkerError::InvalidParameter(
                                    "Result stream is not supported for enqueue".to_string(),
                                )
                                .into(),
                            ))?;
                        };
                        // res.data = data;
                        Ok(Response::new(CreateJobResponse {
                            id: Some(id),
                            result: Some(res),
                        }))
                    } else {
                        Ok(Response::new(CreateJobResponse {
                            id: Some(id),
                            result: Some(res),
                        }))
                    }
                } else {
                    Ok(Response::new(CreateJobResponse {
                        id: Some(id),
                        result: res,
                    }))
                }
            }
            Err(e) => Err(handle_error(&e)),
        }
    }
    type EnqueueForStreamStream = BoxStream<'static, Result<ResultOutputItem, tonic::Status>>;
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "enqueue_for_stream")
    )]
    #[allow(clippy::result_large_err)]
    async fn enqueue_for_stream(
        &self,
        request: tonic::Request<JobRequest>,
    ) -> Result<tonic::Response<Self::EnqueueForStreamStream>, tonic::Status> {
        let _span = Self::trace_request("job", "create", &request);
        let (metadata, _, req) = request.into_parts();
        let metadata = Arc::new(super::process_metadata(metadata)?);
        self.validate_create(&req)?;
        let res = match req.worker.as_ref() {
            Some(Worker::WorkerId(id)) => {
                self.app()
                    .enqueue_job(
                        metadata,
                        Some(id),
                        None,
                        req.args,
                        req.uniq_key,
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                        None,
                        true,
                    )
                    .await
            }
            Some(Worker::WorkerName(name)) => {
                self.app()
                    .enqueue_job(
                        metadata,
                        None,
                        Some(name),
                        req.args,
                        req.uniq_key,
                        req.run_after_time.unwrap_or(0),
                        req.priority.unwrap_or(Priority::Medium as i32),
                        req.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                        None,
                        true,
                    )
                    .await
            }
            None => Err(JobWorkerError::InvalidParameter(
                "Invalid worker type: both worker_id and worker_name are None".to_string(),
            )
            .into()),
        };
        tracing::debug!(
            "enqueue_for_stream result = {:?}",
            &res.as_ref().map(|r| r.0)
        );
        match res {
            Ok((_id, Some(res), Some(st))) => {
                tracing::debug!(
                    "enqueue_for_stream output = {:?}",
                    &res.data
                        .as_ref()
                        .map(|d| d.output.as_ref().map(|o| o.items.len()))
                );
                let res_header = res.encode_to_vec();
                let stream = st.map(Ok);
                let stream: Self::EnqueueForStreamStream = Box::pin(stream);
                let mut res = Response::new(stream);
                res.metadata_mut().insert_bin(
                    super::JOB_RESULT_HEADER_NAME,
                    MetadataValue::from_bytes(res_header.as_slice()),
                );
                Ok(res)
            }
            Ok((_id, res, _)) => {
                let res_header = res.map(|r| r.encode_to_vec());
                // empty stream
                let st = stream::empty().boxed() as Self::EnqueueForStreamStream;
                let mut res = Response::new(st);
                if let Some(header) = res_header {
                    res.metadata_mut().insert_bin(
                        super::JOB_RESULT_HEADER_NAME,
                        MetadataValue::from_bytes(header.as_slice()),
                    );
                }
                Ok(res)
            }
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "delete"))]
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
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<JobId>,
    ) -> Result<tonic::Response<OptionalJobResponse>, tonic::Status> {
        let _s = Self::trace_request("job", "find", &request);
        let req = request.get_ref();
        match self.app().find_job(req).await {
            Ok(res) => Ok(Response::new(OptionalJobResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<Job, tonic::Status>>;
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("job", "find_list", &request);
        let req = request.get_ref();
        // TODO streaming?
        match self
            .app()
            .find_job_list(req.limit.as_ref(), req.offset.as_ref())
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
    type FindQueueListStream = BoxStream<'static, Result<JobAndStatus, tonic::Status>>;
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_queue_list")
    )]
    async fn find_queue_list(
        &self,
        request: tonic::Request<FindQueueListRequest>,
    ) -> Result<tonic::Response<Self::FindQueueListStream>, tonic::Status> {
        let _s = Self::trace_request("job", "find_queue_list", &request);
        let req = request.get_ref();
        // TODO streaming?
        match self
            .app()
            .find_job_queue_list(req.limit.as_ref(), req.channel.as_deref())
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for (j,s) in list {
                    yield Ok(JobAndStatus { job: Some(j), status: s.map(|s| s as i32) })
                }
            }))),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count"))]
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

#[derive(Debug)]
pub(crate) struct JobGrpcImpl {
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
    use infra::infra::job::rows::{JobqueueAndCodec, UseJobqueueAndCodec};

    use super::*;
    use crate::proto::jobworkerp::data::{Priority, WorkerId};
    use crate::proto::jobworkerp::service::job_request::Worker;
    use crate::proto::jobworkerp::service::JobRequest;

    struct Validator;
    impl RequestValidator for Validator {}

    #[test]
    fn test_validate_create_ok() {
        let v = Validator {};
        let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
            args: vec!["fuga".to_string()],
        });
        let mut req = JobRequest {
            worker: Some(Worker::WorkerId(WorkerId { value: 1 })),
            args: jargs,
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
        let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
            args: vec!["fuga".to_string()],
        });
        let reqr = JobRequest {
            worker: Some(Worker::WorkerId(WorkerId { value: 1 })),
            args: jargs,
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
        req.args = Vec::new();
        assert!(v.validate_create(&req).is_ok());
    }
}
