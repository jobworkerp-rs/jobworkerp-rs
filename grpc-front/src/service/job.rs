use crate::proto::jobworkerp::data::{Priority, ResultOutputItem};
use crate::proto::jobworkerp::service::client_stream_request;
use crate::proto::jobworkerp::service::job_request::Worker;
use crate::proto::jobworkerp::service::job_service_server::JobService;
use crate::proto::jobworkerp::service::{
    ClientStreamRequest, CountCondition, CountResponse, CreateJobResponse, FindListRequest,
    FindListWithProcessingStatusRequest, FindQueueListRequest, JobAndStatus, JobRequest,
    OptionalJobResponse, SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job::JobApp;
use app::module::AppModule;
use async_stream::stream;
use command_utils::trace::Tracing;
use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use prost::Message;
use proto::jobworkerp::data::result_output_item;
use proto::jobworkerp::data::{Job, JobId, JobProcessingStatus, StreamingType};
use std::fmt::Debug;
use std::sync::Arc;
use tonic::Response;
use tonic::metadata::MetadataValue;

/// Convert a ResultOutputStream into a gRPC-compatible stream that ends on End item.
fn wrap_result_output_stream(
    st: BoxStream<'static, ResultOutputItem>,
    method_name: &'static str,
) -> BoxStream<'static, Result<ResultOutputItem, tonic::Status>> {
    stream! {
        let mut st = st;
        loop {
            match st.next().await {
                Some(output_item) => {
                    match &output_item.item {
                        Some(result_output_item::Item::End(_)) => {
                            tracing::debug!(
                                "gRPC {} received End item (sending and ending stream)",
                                method_name
                            );
                            yield Ok(output_item);
                            break;
                        }
                        Some(_) => {
                            tracing::trace!(
                                "gRPC {} sending item: {:?}",
                                method_name,
                                output_item.item.as_ref().map(std::mem::discriminant)
                            );
                            yield Ok(output_item);
                        }
                        None => {
                            tracing::debug!(
                                "gRPC {} received None item (stream ending gracefully)",
                                method_name
                            );
                            break;
                        }
                    }
                }
                None => {
                    tracing::debug!("gRPC {}: underlying stream ended gracefully", method_name);
                    break;
                }
            }
        }
    }
    .boxed()
}

pub trait JobGrpc {
    fn app(&self) -> &Arc<dyn JobApp + 'static>;
    fn app_module(&self) -> &Arc<AppModule>;
}
pub trait RequestValidator {
    // almost no timeout (1 year after)
    const DEFAULT_TIMEOUT: u64 = 1000 * 60 * 60 * 24 * 365;

    /// Validate FindQueueListRequest for DoS protection
    #[allow(clippy::result_large_err)]
    fn validate_find_queue_list(&self, req: &FindQueueListRequest) -> Result<(), tonic::Status> {
        // Limit validation
        if let Some(limit) = req.limit {
            if limit > jobworkerp_base::limits::MAX_LIMIT {
                return Err(tonic::Status::invalid_argument(
                    "limit too large (max: 1000)",
                ));
            }
            if limit < 1 {
                return Err(tonic::Status::invalid_argument("limit must be positive"));
            }
        }

        // Channel name length validation
        if let Some(ref channel) = req.channel
            && channel.len() > jobworkerp_base::limits::MAX_CHANNEL_NAME_LENGTH
        {
            return Err(tonic::Status::invalid_argument(
                "channel name too long (max: 255)",
            ));
        }

        Ok(())
    }

    /// Validate FindListWithProcessingStatusRequest for DoS protection
    #[allow(clippy::result_large_err)]
    fn validate_find_list_with_processing_status(
        &self,
        req: &FindListWithProcessingStatusRequest,
    ) -> Result<(), tonic::Status> {
        // Limit validation
        if let Some(limit) = req.limit {
            if limit > jobworkerp_base::limits::MAX_LIMIT {
                return Err(tonic::Status::invalid_argument(
                    "limit too large (max: 1000)",
                ));
            }
            if limit < 1 {
                return Err(tonic::Status::invalid_argument("limit must be positive"));
            }
        }

        // Status validation
        if req.status == JobProcessingStatus::Unknown as i32 {
            return Err(tonic::Status::invalid_argument("invalid status: UNKNOWN"));
        }

        Ok(())
    }

    #[allow(clippy::result_large_err)]
    fn validate_create(&self, req: &JobRequest) -> Result<(), tonic::Status> {
        if req.worker.is_none() {
            return Err(tonic::Status::invalid_argument(format!(
                "worker_id or worker_name is required: {req:?}"
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
                "worker_name or worker_id is required: {req:?}"
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
        // Reject client-stream-only runners from non-client-stream enqueue
        {
            let (wid_opt, wname_opt) = match req.worker.as_ref() {
                Some(Worker::WorkerId(id)) => (Some(id), None),
                Some(Worker::WorkerName(name)) => (None, Some(name)),
                _ => (None, None),
            };
            let worker = self
                .app_module()
                .worker_app
                .find_by_id_or_name(wid_opt, wname_opt.map(|s| s as &String))
                .await
                .map_err(|e| handle_error(&e))?;
            let wid = worker
                .id
                .ok_or_else(|| tonic::Status::internal("worker has no id"))?;
            self.app_module()
                .worker_app
                .check_worker_streaming(&wid, false, Some(false), req.using.as_deref())
                .await
                .map_err(|e| handle_error(&e))?;
        }
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
                        StreamingType::None,
                        req.using,
                        req.overrides,
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
                        StreamingType::None,
                        req.using,
                        req.overrides,
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
        // Reject client-stream-only runners from non-client-stream enqueue
        {
            let (wid_opt, wname_opt) = match req.worker.as_ref() {
                Some(Worker::WorkerId(id)) => (Some(id), None),
                Some(Worker::WorkerName(name)) => (None, Some(name)),
                _ => (None, None),
            };
            let worker = self
                .app_module()
                .worker_app
                .find_by_id_or_name(wid_opt, wname_opt.map(|s| s as &String))
                .await
                .map_err(|e| handle_error(&e))?;
            let wid = worker
                .id
                .ok_or_else(|| tonic::Status::internal("worker has no id"))?;
            self.app_module()
                .worker_app
                .check_worker_streaming(&wid, true, Some(false), req.using.as_deref())
                .await
                .map_err(|e| handle_error(&e))?;
        }
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
                        StreamingType::Response,
                        req.using,
                        req.overrides,
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
                        StreamingType::Response,
                        req.using,
                        req.overrides,
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
            Ok((id, Some(res), Some(st))) => {
                tracing::debug!(
                    "enqueue_for_stream output = {:?}",
                    &res.data
                        .as_ref()
                        .map(|d| d.output.as_ref().map(|o| o.items.len()))
                );
                let res_header = res.encode_to_vec();
                let job_id_header = id.encode_to_vec();

                let stream: Self::EnqueueForStreamStream =
                    Box::pin(wrap_result_output_stream(st, "enqueue_for_stream"));
                let mut res = Response::new(stream);
                res.metadata_mut().insert_bin(
                    super::JOB_RESULT_HEADER_NAME,
                    MetadataValue::from_bytes(res_header.as_slice()),
                );
                res.metadata_mut().insert_bin(
                    super::JOB_ID_HEADER_NAME,
                    MetadataValue::from_bytes(job_id_header.as_slice()),
                );
                Ok(res)
            }
            Ok((id, res, _)) => {
                // For streaming requests where no actual stream is available (error cases),
                // return an error status with JobResult in trailers
                if let Some(job_result) = &res
                    && let Some(result_data) = &job_result.data
                {
                    use proto::jobworkerp::data::ResultStatus;
                    if result_data.status != ResultStatus::Success as i32 {
                        tracing::warn!(
                            "enqueue_for_stream: job {} failed with status {}, returning gRPC error with trailers",
                            id.value,
                            result_data.status
                        );

                        let status = JobGrpcImpl::create_job_error_status(&id, result_data);

                        return Err(status);
                    }
                }

                let res_header = res.map(|r| r.encode_to_vec());
                let job_id_header = id.encode_to_vec();

                // Using stream::iter with empty iterator to ensure proper gRPC stream termination
                let st = futures::stream::iter(std::iter::empty::<
                    Result<ResultOutputItem, tonic::Status>,
                >())
                .boxed() as Self::EnqueueForStreamStream;

                let mut res = Response::new(st);
                if let Some(header) = res_header {
                    res.metadata_mut().insert_bin(
                        super::JOB_RESULT_HEADER_NAME,
                        MetadataValue::from_bytes(header.as_slice()),
                    );
                }
                res.metadata_mut().insert_bin(
                    super::JOB_ID_HEADER_NAME,
                    MetadataValue::from_bytes(job_id_header.as_slice()),
                );
                Ok(res)
            }
            Err(e) => {
                // enqueue_job failed before creating job_id - no headers to include
                tracing::warn!("enqueue_for_stream failed during job creation: {:?}", e);
                Err(handle_error(&e))
            }
        }
    }

    type EnqueueWithClientStreamStream =
        BoxStream<'static, Result<ResultOutputItem, tonic::Status>>;
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "enqueue_with_client_stream")
    )]
    #[allow(clippy::result_large_err)]
    async fn enqueue_with_client_stream(
        &self,
        request: tonic::Request<tonic::Streaming<ClientStreamRequest>>,
    ) -> Result<tonic::Response<Self::EnqueueWithClientStreamStream>, tonic::Status> {
        let _span = Self::trace_request("job", "enqueue_with_client_stream", &request);
        let (metadata, _, mut client_stream) = request.into_parts();
        let metadata = Arc::new(super::process_metadata(metadata)?);

        // 1. First message MUST be job_request
        let first_msg = client_stream
            .next()
            .await
            .ok_or_else(|| tonic::Status::invalid_argument("stream ended without any message"))?
            .map_err(|e| tonic::Status::invalid_argument(format!("stream error: {e}")))?;

        let job_request = match first_msg.request {
            Some(client_stream_request::Request::JobRequest(req)) => req,
            Some(client_stream_request::Request::FeedData(_)) => {
                return Err(tonic::Status::invalid_argument(
                    "first message must be job_request, not feed_data",
                ));
            }
            None => {
                return Err(tonic::Status::invalid_argument(
                    "first message must contain job_request",
                ));
            }
        };

        // 2. Validate request
        self.validate_create(&job_request)?;

        // 3. Resolve worker
        let (worker_id, worker_name) = match job_request.worker.as_ref() {
            Some(Worker::WorkerId(id)) => (Some(*id), None),
            Some(Worker::WorkerName(name)) => (None, Some(name.clone())),
            None => {
                return Err(tonic::Status::invalid_argument(
                    "worker_id or worker_name is required",
                ));
            }
        };

        // 4. Check worker supports client streaming
        let using = job_request.using.clone();
        let worker = self
            .app_module()
            .worker_app
            .find_by_id_or_name(worker_id.as_ref(), worker_name.as_ref())
            .await
            .map_err(|e| handle_error(&e))?;
        let wid = worker
            .id
            .ok_or_else(|| tonic::Status::internal("worker has no id"))?;

        self.app_module()
            .worker_app
            .check_worker_streaming(&wid, true, Some(true), using.as_deref())
            .await
            .map_err(|e| handle_error(&e))?;

        // 5. Pre-generate job ID
        let reserved_job_id = self.app().generate_job_id().map_err(|e| handle_error(&e))?;
        let job_id_value = reserved_job_id.value;

        // 6. Run enqueue_job and feed_forwarder in parallel
        let app = self.app().clone();
        let app_module = self.app_module().clone();
        let meta_clone = metadata.clone();
        let job_request_clone = job_request.clone();
        let reserved_id_clone = reserved_job_id;

        let mut enqueue_handle = tokio::spawn(async move {
            let (wid, wname) = match job_request_clone.worker.as_ref() {
                Some(Worker::WorkerId(id)) => (Some(id), None),
                Some(Worker::WorkerName(name)) => (None, Some(name)),
                None => (None, None),
            };
            app.enqueue_job(
                meta_clone,
                wid,
                wname,
                job_request_clone.args,
                job_request_clone.uniq_key,
                job_request_clone.run_after_time.unwrap_or(0),
                job_request_clone
                    .priority
                    .unwrap_or(Priority::Medium as i32),
                job_request_clone.timeout.unwrap_or(Self::DEFAULT_TIMEOUT),
                Some(reserved_id_clone),
                // Currently hardcoded to Response; other StreamingTypes (e.g. Notify)
                // are not supported for client-streaming jobs yet.
                StreamingType::Response,
                job_request_clone.using,
                job_request_clone.overrides,
            )
            .await
        });

        let feed_publisher = app_module.feed_publisher.clone();
        let cleanup_feed_publisher = app_module.feed_publisher.clone();
        let feed_sender_store = app_module.feed_sender_store.clone();
        let feed_dispatch_timeout = app_module
            .config_module
            .job_queue_config
            .feed_dispatch_timeout_duration();

        let mut feed_handle = tokio::spawn(async move {
            // For Standalone mode, wait until the feed channel is ready
            if let Some(store) = feed_sender_store.as_ref() {
                let timeout = feed_dispatch_timeout;
                store
                    .wait_for_feed_ready(job_id_value, timeout)
                    .await
                    .map_err(|e| {
                        tonic::Status::deadline_exceeded(format!(
                            "feed channel not ready within timeout: {e}"
                        ))
                    })?;
            }
            // Forward feed data from client stream
            let mut sent_final = false;
            while let Some(msg_result) = client_stream.next().await {
                let msg = msg_result
                    .map_err(|e| tonic::Status::internal(format!("client stream error: {e}")))?;
                match msg.request {
                    Some(client_stream_request::Request::FeedData(feed_transport)) => {
                        let is_final = feed_transport.is_final;
                        feed_publisher
                            .publish_feed(
                                &JobId {
                                    value: job_id_value,
                                },
                                feed_transport.data,
                                is_final,
                            )
                            .await
                            .map_err(|e| {
                                tonic::Status::internal(format!("failed to publish feed: {e}"))
                            })?;
                        if is_final {
                            sent_final = true;
                            break;
                        }
                    }
                    Some(client_stream_request::Request::JobRequest(_)) => {
                        return Err(tonic::Status::invalid_argument(
                            "job_request is only allowed as the first message",
                        ));
                    }
                    None => {
                        tracing::trace!(
                            "enqueue_with_client_stream: received empty message (no request variant), skipping"
                        );
                    }
                }
            }
            // Ensure runner's feed channel is closed on client disconnect
            if !sent_final {
                let _ = feed_publisher
                    .publish_feed(
                        &JobId {
                            value: job_id_value,
                        },
                        Vec::new(),
                        true,
                    )
                    .await;
            }
            Ok::<(), tonic::Status>(())
        });

        // Wait for both; abort feed_handle early if enqueue fails.
        // IMPORTANT: enqueue_job must return before the runner starts consuming the
        // feed stream. If a future runner implementation consumes feed data inside
        // enqueue_job (before returning), this select! could deadlock.
        let (enqueue_result, feed_result) = tokio::select! {
            enqueue_res = &mut enqueue_handle => {
                let enqueue_res = enqueue_res
                    .map_err(|e| tonic::Status::internal(format!("enqueue task panicked: {e}")))?;
                if enqueue_res.is_err() {
                    feed_handle.abort();
                    (enqueue_res, Ok(()))
                } else {
                    let feed_res = feed_handle.await
                        .map_err(|e| tonic::Status::internal(format!("feed task panicked: {e}")))?;
                    (enqueue_res, feed_res)
                }
            }
            feed_res = &mut feed_handle => {
                let feed_res = feed_res
                    .map_err(|e| tonic::Status::internal(format!("feed task panicked: {e}")))?;
                let enqueue_res = enqueue_handle.await
                    .map_err(|e| tonic::Status::internal(format!("enqueue task panicked: {e}")))?;
                (enqueue_res, feed_res)
            }
        };

        // Check feed errors first
        if let Err(feed_err) = feed_result {
            tracing::warn!("feed forwarding failed: {:?}", feed_err);
            // Force-close the runner's feed channel so it does not hang waiting for data
            let _ = cleanup_feed_publisher
                .publish_feed(
                    &JobId {
                        value: job_id_value,
                    },
                    Vec::new(),
                    true,
                )
                .await;
            // Try to clean up the job if enqueue succeeded
            if let Ok((ref id, _, _)) = enqueue_result
                && let Err(e) = self.app().delete_job(id).await
            {
                tracing::warn!("failed to clean up orphan job {}: {e}", id.value);
            }
            return Err(feed_err);
        }

        // Process enqueue result (same pattern as enqueue_for_stream)
        match enqueue_result {
            Ok((id, Some(res), Some(st))) => {
                tracing::debug!(
                    "enqueue_with_client_stream output = {:?}",
                    &res.data
                        .as_ref()
                        .map(|d| d.output.as_ref().map(|o| o.items.len()))
                );
                let res_header = res.encode_to_vec();
                let job_id_header = id.encode_to_vec();

                let stream: Self::EnqueueWithClientStreamStream =
                    Box::pin(wrap_result_output_stream(st, "enqueue_with_client_stream"));
                let mut res = Response::new(stream);
                res.metadata_mut().insert_bin(
                    super::JOB_RESULT_HEADER_NAME,
                    MetadataValue::from_bytes(res_header.as_slice()),
                );
                res.metadata_mut().insert_bin(
                    super::JOB_ID_HEADER_NAME,
                    MetadataValue::from_bytes(job_id_header.as_slice()),
                );
                Ok(res)
            }
            Ok((id, res, _)) => {
                // NoResult mode or error case
                if let Some(job_result) = &res
                    && let Some(result_data) = &job_result.data
                {
                    use proto::jobworkerp::data::ResultStatus;
                    if result_data.status != ResultStatus::Success as i32 {
                        tracing::warn!(
                            "enqueue_with_client_stream: job {} failed with status {}",
                            id.value,
                            result_data.status
                        );
                        let status = JobGrpcImpl::create_job_error_status(&id, result_data);
                        return Err(status);
                    }
                }

                let res_header = res.map(|r| r.encode_to_vec());
                let job_id_header = id.encode_to_vec();

                let st = futures::stream::iter(std::iter::empty::<
                    Result<ResultOutputItem, tonic::Status>,
                >())
                .boxed() as Self::EnqueueWithClientStreamStream;

                let mut res = Response::new(st);
                if let Some(header) = res_header {
                    res.metadata_mut().insert_bin(
                        super::JOB_RESULT_HEADER_NAME,
                        MetadataValue::from_bytes(header.as_slice()),
                    );
                }
                res.metadata_mut().insert_bin(
                    super::JOB_ID_HEADER_NAME,
                    MetadataValue::from_bytes(job_id_header.as_slice()),
                );
                Ok(res)
            }
            Err(e) => {
                tracing::warn!(
                    "enqueue_with_client_stream failed during job creation: {:?}",
                    e
                );
                Err(handle_error(&e))
            }
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

        // Validation for DoS protection
        self.validate_find_queue_list(req)?;

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

    type FindListWithProcessingStatusStream =
        BoxStream<'static, Result<JobAndStatus, tonic::Status>>;
    #[allow(clippy::result_large_err)]
    #[tracing::instrument(
        level = "info",
        skip(self, request),
        fields(method = "find_list_with_processing_status")
    )]
    async fn find_list_with_processing_status(
        &self,
        request: tonic::Request<FindListWithProcessingStatusRequest>,
    ) -> Result<tonic::Response<Self::FindListWithProcessingStatusStream>, tonic::Status> {
        let _s = Self::trace_request("job", "find_list_with_processing_status", &request);
        let req = request.get_ref();

        // Validation for DoS protection
        self.validate_find_list_with_processing_status(req)?;

        let status = JobProcessingStatus::try_from(req.status)
            .map_err(|_| tonic::Status::invalid_argument("Invalid job status"))?;

        match self
            .app()
            .find_list_with_processing_status(status, req.limit.as_ref())
            .await
        {
            Ok(list) => Ok(Response::new(Box::pin(stream! {
                for (job, status) in list {
                    yield Ok(JobAndStatus {
                        job: Some(job),
                        status: Some(status as i32)
                    })
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
    fn app_module(&self) -> &Arc<AppModule> {
        &self.app_module
    }
}

// use tracing
impl Tracing for JobGrpcImpl {}

impl RequestValidator for JobGrpcImpl {}

impl JobGrpcImpl {
    /// Create appropriate gRPC error status from job result data with protobuf in trailers
    fn create_job_error_status(
        job_id: &proto::jobworkerp::data::JobId,
        result_data: &proto::jobworkerp::data::JobResultData,
    ) -> tonic::Status {
        use prost::Message;
        use proto::jobworkerp::data::ResultStatus;

        let job_result = proto::jobworkerp::data::JobResult {
            id: Some(proto::jobworkerp::data::JobResultId { value: 0 }), // dummy id for error response
            data: Some(result_data.clone()),
            metadata: std::collections::HashMap::new(),
        };

        let error_output = result_data
            .output
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.items))
            .unwrap_or_default();

        // Map ResultStatus to appropriate gRPC status codes and messages
        let (code, message) = match ResultStatus::try_from(result_data.status) {
            Ok(ResultStatus::Success) => (
                tonic::Code::Internal,
                "Unexpected success status in error handling".to_string(),
            ),
            Ok(ResultStatus::ErrorAndRetry) => (
                tonic::Code::Unavailable,
                format!("Job execution failed (retryable): {error_output}"),
            ),
            Ok(ResultStatus::MaxRetry) => (
                tonic::Code::Aborted,
                format!("Job execution failed (max retries exceeded): {error_output}"),
            ),
            Ok(ResultStatus::OtherError) => (
                tonic::Code::Internal,
                format!("Job execution error: {error_output}"),
            ),
            Ok(ResultStatus::FatalError) => (
                tonic::Code::FailedPrecondition,
                format!("Fatal job execution error: {error_output}"),
            ),
            Ok(ResultStatus::Abort) => (
                tonic::Code::Aborted,
                format!("Job execution aborted: {error_output}"),
            ),
            Ok(ResultStatus::Cancelled) => (
                tonic::Code::Cancelled,
                format!("Job execution cancelled: {error_output}"),
            ),
            Err(_) => (
                tonic::Code::Unknown,
                format!(
                    "Job execution failed with unknown status: {}",
                    result_data.status
                ),
            ),
        };

        let enhanced_message = format!("job_id={}, {message}", job_id.value);
        let mut status = tonic::Status::new(code, enhanced_message);

        let job_result_bytes = job_result.encode_to_vec();
        let metadata_value =
            tonic::metadata::MetadataValue::from_bytes(job_result_bytes.as_slice());
        status
            .metadata_mut()
            .insert_bin(super::JOB_RESULT_HEADER_NAME, metadata_value);

        // Also add job_id separately for convenience
        let job_id_bytes = job_id.encode_to_vec();
        let job_id_metadata = tonic::metadata::MetadataValue::from_bytes(job_id_bytes.as_slice());
        status
            .metadata_mut()
            .insert_bin(super::JOB_ID_HEADER_NAME, job_id_metadata);

        status
    }
}

// unit test for RequestValidator::validate_create method
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::jobworkerp::data::{Priority, WorkerId};
    use crate::proto::jobworkerp::service::JobRequest;
    use crate::proto::jobworkerp::service::job_request::Worker;
    use jobworkerp_base::codec::UseProstCodec;

    struct Validator;
    impl RequestValidator for Validator {}

    #[test]
    fn test_validate_create_ok() {
        let v = Validator {};
        let jargs =
            jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                args: vec!["fuga".to_string()],
            })
            .unwrap();
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
        let jargs =
            jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                args: vec!["fuga".to_string()],
            })
            .unwrap();
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

    #[test]
    fn test_client_stream_request_job_request_variant() {
        let req = ClientStreamRequest {
            request: Some(client_stream_request::Request::JobRequest(JobRequest {
                worker: Some(Worker::WorkerId(WorkerId { value: 1 })),
                args: vec![1, 2, 3],
                ..Default::default()
            })),
        };
        match req.request {
            Some(client_stream_request::Request::JobRequest(jr)) => {
                assert!(matches!(jr.worker, Some(Worker::WorkerId(id)) if id.value == 1));
            }
            _ => panic!("expected JobRequest variant"),
        }
    }

    #[test]
    fn test_client_stream_request_feed_data_variant() {
        use crate::proto::jobworkerp::data::FeedDataTransport;
        let req = ClientStreamRequest {
            request: Some(client_stream_request::Request::FeedData(
                FeedDataTransport {
                    data: vec![10, 20, 30],
                    is_final: true,
                },
            )),
        };
        match req.request {
            Some(client_stream_request::Request::FeedData(fd)) => {
                assert_eq!(fd.data, vec![10, 20, 30]);
                assert!(fd.is_final);
            }
            _ => panic!("expected FeedData variant"),
        }
    }

    #[test]
    fn test_client_stream_request_empty() {
        let req = ClientStreamRequest { request: None };
        assert!(req.request.is_none());
    }

    #[test]
    fn test_validate_create_rejects_first_message_without_worker() {
        let v = Validator {};
        let req = JobRequest {
            worker: None,
            args: vec![1, 2, 3],
            ..Default::default()
        };
        // validate_create is used for the first message in enqueue_with_client_stream
        assert!(v.validate_create(&req).is_err());
    }
}
