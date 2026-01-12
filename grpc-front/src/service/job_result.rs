use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::data::{JobResultSortField, ResultOutputItem};
use crate::proto::jobworkerp::service::job_result_service_server::JobResultService;
use crate::proto::jobworkerp::service::listen_request::Worker;
use crate::proto::jobworkerp::service::{
    listen_by_worker_request, CountCondition, CountJobResultRequest, CountResponse,
    DeleteJobResultBulkRequest, DeleteJobResultBulkResponse, FindJobResultListRequest,
    FindListByJobIdRequest, FindListRequest, ListenByWorkerRequest, ListenRequest,
    OptionalJobResultResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use crate::service::validation::{
    validate_bulk_delete_safety, validate_filter_enums, validate_filter_ids, validate_limit,
    validate_offset, validate_time_range,
};
use app::app::job_result::JobResultApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::{JobResult, JobResultId};
use proto::DEFAULT_METHOD_NAME;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::Response;

pub trait JobResultGrpc {
    fn app(&self) -> &Arc<dyn JobResultApp + 'static>;
}
// 1 day (same as expire_job_result_seconds in JobQueueConfig)
// const DEFAULT_TIMEOUT: u64 = 1000 * 60 * 60 * 24;

#[tonic::async_trait]
impl<T: JobResultGrpc + Tracing + Send + Debug + Sync + 'static> JobResultService for T {
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "delete"))]
    async fn delete(
        &self,
        request: tonic::Request<JobResultId>,
    ) -> Result<tonic::Response<SuccessResponse>, tonic::Status> {
        let _s = Self::trace_request("job_result", "delete", &request);
        let req = request.get_ref();
        match self.app().delete_job_result(req).await {
            Ok(r) => Ok(Response::new(SuccessResponse { is_success: r })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find"))]
    async fn find(
        &self,
        request: tonic::Request<JobResultId>,
    ) -> Result<tonic::Response<OptionalJobResultResponse>, tonic::Status> {
        let _s = Self::trace_request("job_result", "find", &request);
        let req = request.get_ref();
        match self.app().find_job_result_from_db(req).await {
            Ok(res) => Ok(Response::new(OptionalJobResultResponse { data: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type FindListStream = BoxStream<'static, Result<JobResult, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list"))]
    async fn find_list(
        &self,
        request: tonic::Request<FindListRequest>,
    ) -> Result<tonic::Response<Self::FindListStream>, tonic::Status> {
        let _s = Self::trace_request("job_result", "find_list", &request);
        let req = request.get_ref();
        // TODO streaming?
        match self
            .app()
            .find_job_result_list(req.limit.as_ref(), req.offset.as_ref())
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
    type FindListByJobIdStream = BoxStream<'static, Result<JobResult, tonic::Status>>;
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_list_by_job_id")
    )]
    async fn find_list_by_job_id(
        &self,
        request: tonic::Request<FindListByJobIdRequest>,
    ) -> Result<tonic::Response<Self::FindListByJobIdStream>, tonic::Status> {
        let _s = Self::trace_request("job_result", "find_list", &request);
        let req = request.get_ref();
        if let Some(job_id) = req.job_id.as_ref() {
            match self.app().find_job_result_list_by_job_id(job_id).await {
                Ok(list) => Ok(Response::new(Box::pin(stream! {
                    for s in list {
                        yield Ok(s)
                    }
                }))),
                Err(e) => Err(handle_error(&e)),
            }
        } else {
            Err(tonic::Status::invalid_argument(
                "job_id is required".to_string(),
            ))
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count"))]
    async fn count(
        &self,
        request: tonic::Request<CountCondition>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("job_result", "count", &request);
        match self.app().count().await {
            Ok(res) => Ok(Response::new(CountResponse { total: res })),
            Err(e) => Err(handle_error(&e)),
        }
    }
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "listen"))]
    async fn listen(
        &self,
        request: tonic::Request<ListenRequest>,
    ) -> Result<tonic::Response<JobResult>, tonic::Status> {
        let _s = Self::trace_request("job_result", "listen", &request);
        let req = request.get_ref();
        let using = req.using.as_deref().unwrap_or(DEFAULT_METHOD_NAME);
        let res = match (req.job_id.as_ref(), req.worker.as_ref()) {
            (Some(job_id), Some(Worker::WorkerId(worker_id))) => {
                self.app()
                    .listen_result(job_id, Some(worker_id), None, req.timeout, false, using)
                    .await
            }
            (Some(job_id), Some(Worker::WorkerName(name))) => {
                self.app()
                    .listen_result(job_id, None, Some(name), req.timeout, false, using)
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter(
                "job_id and worker_id are required".to_string(),
            )
            .into()),
        };
        match res {
            Ok(res) => {
                let data_stream_opt = res.1;
                // not streaming mode. collect stream data and modify res.0.data
                if let Some(_data_stream) = data_stream_opt {
                    Err(handle_error(
                        &JobWorkerError::InvalidParameter(
                            "streaming mode is not supported in this method".to_string(),
                        )
                        .into(),
                    ))
                } else {
                    Ok(Response::new(res.0))
                }
            }
            Err(e) => Err(handle_error(&e)),
        }
    }
    type ListenStreamStream = BoxStream<'static, Result<ResultOutputItem, tonic::Status>>;
    #[tracing::instrument(level = "info", skip(self, request), fields(method = "listen_stream"))]
    async fn listen_stream(
        &self,
        request: tonic::Request<ListenRequest>,
    ) -> std::result::Result<tonic::Response<Self::ListenStreamStream>, tonic::Status> {
        use tokio_stream::StreamExt;
        let _s = Self::trace_request("job_result", "listen_stream", &request);
        let req = request.into_inner();
        let using = req.using.as_deref().unwrap_or(DEFAULT_METHOD_NAME);
        let timeout = req.timeout;
        let res = match (req.job_id, req.worker) {
            (Some(job_id), Some(Worker::WorkerId(worker_id))) => {
                self.app()
                    .listen_result(&job_id, Some(&worker_id), None, timeout, true, using)
                    .await
            }
            (Some(job_id), Some(Worker::WorkerName(name))) => {
                self.app()
                    .listen_result(&job_id, None, Some(&name), timeout, true, using)
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter(
                "job_id and worker_id are required".to_string(),
            )
            .into()),
        };
        match res {
            Ok((result, Some(stream))) => {
                let res_header = result.encode_to_vec();
                let stream = stream.map(Ok);
                let stream: Self::ListenStreamStream = Box::pin(stream);
                let mut res = Response::new(stream);
                res.metadata_mut().insert_bin(
                    super::JOB_RESULT_HEADER_NAME,
                    MetadataValue::from_bytes(res_header.as_slice()),
                );
                Ok(res)
            }
            Ok((result, None)) => {
                tracing::debug!("no stream available for job result");
                let res_header = result.encode_to_vec();
                // empty stream
                let mut res = Response::new(Box::pin(stream! {
                    yield Ok(ResultOutputItem {
                        item: None,
                    });
                }) as Self::ListenStreamStream);
                res.metadata_mut().insert_bin(
                    super::JOB_RESULT_HEADER_NAME,
                    MetadataValue::from_bytes(res_header.as_slice()),
                );
                Ok(res)
            }
            Err(e) => Err(handle_error(&e)),
        }
    }

    type ListenByWorkerStream = BoxStream<'static, Result<JobResult, tonic::Status>>;
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "listen_by_worker")
    )]
    async fn listen_by_worker(
        &self,
        request: tonic::Request<ListenByWorkerRequest>,
    ) -> std::result::Result<tonic::Response<Self::ListenByWorkerStream>, tonic::Status> {
        use tokio_stream::StreamExt;

        let _s = Self::trace_request("job_result", "listen_by_worker", &request);
        let req = request.into_inner().worker;
        let res = match req.as_ref() {
            Some(listen_by_worker_request::Worker::WorkerId(worker_id)) => {
                self.app()
                    .listen_result_stream_by_worker(Some(worker_id), None)
                    .await
            }
            Some(listen_by_worker_request::Worker::WorkerName(name)) => {
                self.app()
                    .listen_result_stream_by_worker(None, Some(name))
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter("worker is required".to_string()).into()),
        };
        let req = req.clone();
        if let Ok(mut stream) = res {
            let (tx, rx) = mpsc::channel(128);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = command_utils::util::shutdown::shutdown_signal() => {
                            break;
                        }
                        item = stream.next() => {
                    tracing::debug!("\treceive result item: worker = {:?}, item = {:?}", &req, &item);
                            match item {
                                Some(Ok(item)) => {
                                    if tx.send(Ok(item)).await.is_err() {
                                        break;
                                    }
                                }
                                Some(Err(e)) => {
                                    if tx.send(Err(handle_error(&e))).await.is_err() {
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                    }
                }
                tracing::info!("\tclient disconnected");
            });
            let output_stream = ReceiverStream::new(rx);
            Ok(Response::new(
                Box::pin(output_stream) as Self::ListenByWorkerStream
            ))
        } else {
            Err(handle_error(&res.err().unwrap()))
        }
    }

    // New methods (Sprint 4)
    type FindListByStream = BoxStream<'static, Result<JobResult, tonic::Status>>;

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "find_list_by"))]
    async fn find_list_by(
        &self,
        request: tonic::Request<FindJobResultListRequest>,
    ) -> Result<tonic::Response<Self::FindListByStream>, tonic::Status> {
        let _s = Self::trace_request("job_result", "find_list_by", &request);
        let req = request.into_inner();

        // Validation
        validate_limit(req.limit)?;
        validate_offset(req.offset)?;
        validate_time_range(req.start_time_from, req.start_time_to, "start_time")?;
        validate_time_range(req.end_time_from, req.end_time_to, "end_time")?;
        validate_filter_ids(&req.worker_ids, "worker_ids")?;
        validate_filter_enums(&req.statuses, "statuses")?;
        validate_filter_enums(&req.priorities, "priorities")?;

        let worker_ids: Vec<i64> = req.worker_ids.iter().map(|w| w.value).collect();

        let statuses: Vec<i32> = req.statuses.to_vec();

        let priorities: Vec<i32> = req.priorities.to_vec();

        let sort_by = req
            .sort_by
            .and_then(|s| JobResultSortField::try_from(s).ok());

        // Call App layer
        match self
            .app()
            .find_list_by(
                worker_ids,
                statuses,
                req.start_time_from,
                req.start_time_to,
                req.end_time_from,
                req.end_time_to,
                priorities,
                req.uniq_key,
                req.limit,
                req.offset,
                sort_by,
                req.ascending,
            )
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

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "count_by"))]
    async fn count_by(
        &self,
        request: tonic::Request<CountJobResultRequest>,
    ) -> Result<tonic::Response<CountResponse>, tonic::Status> {
        let _s = Self::trace_request("job_result", "count_by", &request);
        let req = request.into_inner();

        // Validation (same as FindListBy)
        validate_time_range(req.start_time_from, req.start_time_to, "start_time")?;
        validate_time_range(req.end_time_from, req.end_time_to, "end_time")?;
        validate_filter_ids(&req.worker_ids, "worker_ids")?;
        validate_filter_enums(&req.statuses, "statuses")?;
        validate_filter_enums(&req.priorities, "priorities")?;

        let worker_ids: Vec<i64> = req.worker_ids.iter().map(|w| w.value).collect();

        let statuses: Vec<i32> = req.statuses.to_vec();

        let priorities: Vec<i32> = req.priorities.to_vec();

        // Call App layer
        match self
            .app()
            .count_by(
                worker_ids,
                statuses,
                req.start_time_from,
                req.start_time_to,
                req.end_time_from,
                req.end_time_to,
                priorities,
                req.uniq_key,
            )
            .await
        {
            Ok(count) => Ok(Response::new(CountResponse { total: count })),
            Err(e) => Err(handle_error(&e)),
        }
    }

    #[tracing::instrument(level = "info", skip(self, request), fields(method = "delete_bulk"))]
    async fn delete_bulk(
        &self,
        request: tonic::Request<DeleteJobResultBulkRequest>,
    ) -> Result<tonic::Response<DeleteJobResultBulkResponse>, tonic::Status> {
        let _s = Self::trace_request("job_result", "delete_bulk", &request);
        let req = request.into_inner();

        // Safety feature 1: Required filter condition check (gRPC layer)
        if req.end_time_before.is_none() && req.statuses.is_empty() && req.worker_ids.is_empty() {
            return Err(tonic::Status::invalid_argument(
                "At least one filter condition is required (end_time_before, statuses, or worker_ids). \
                 Unconditional deletion of all records is not permitted for safety reasons."
            ));
        }

        // Safety feature 2: Recent data protection (gRPC layer - Defense in Depth)
        // Note: 24-hour protection is ALWAYS enforced at RDB layer, even if end_time_before is not specified.
        // This validation provides early error feedback if user explicitly requests deletion within 24 hours.
        validate_bulk_delete_safety(req.end_time_before)?;

        // Safety feature 3: Filter array size validation (prevent unbounded SQL IN clauses)
        validate_filter_enums(&req.statuses, "statuses")?;
        validate_filter_ids(&req.worker_ids, "worker_ids")?;

        let statuses: Vec<i32> = req.statuses.to_vec();

        let worker_ids: Vec<i64> = req.worker_ids.iter().map(|w| w.value).collect();

        // Call App layer
        match self
            .app()
            .delete_bulk(req.end_time_before, statuses, worker_ids)
            .await
        {
            Ok(deleted_count) => {
                tracing::info!(
                    deleted_count = deleted_count,
                    end_time_before = ?req.end_time_before,
                    statuses_count = req.statuses.len(),
                    worker_ids_count = req.worker_ids.len(),
                    "Bulk delete completed successfully"
                );
                Ok(Response::new(DeleteJobResultBulkResponse { deleted_count }))
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    end_time_before = ?req.end_time_before,
                    statuses_count = req.statuses.len(),
                    worker_ids_count = req.worker_ids.len(),
                    "Bulk delete failed"
                );
                Err(handle_error(&e))
            }
        }
    }
}

#[derive(DebugStub)]
pub(crate) struct JobResultGrpcImpl {
    #[debug_stub = "AppModule"]
    app_module: Arc<AppModule>,
}

impl JobResultGrpcImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        JobResultGrpcImpl { app_module }
    }
}
impl JobResultGrpc for JobResultGrpcImpl {
    fn app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.app_module.job_result_app
    }
}

// use tracing
impl Tracing for JobResultGrpcImpl {}
