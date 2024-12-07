use std::fmt::Debug;
use std::sync::Arc;

use crate::proto::jobworkerp::service::job_result_service_server::JobResultService;
use crate::proto::jobworkerp::service::listen_request::Worker;
use crate::proto::jobworkerp::service::{
    listen_stream_by_worker_request, CountCondition, CountResponse, FindListByJobIdRequest,
    FindListRequest, ListenRequest, ListenStreamByWorkerRequest, OptionalJobResultResponse,
    SuccessResponse,
};
use crate::service::error_handle::handle_error;
use app::app::job_result::JobResultApp;
use app::module::AppModule;
use async_stream::stream;
use futures::stream::BoxStream;
use infra::error::JobWorkerError;
use infra_utils::trace::Tracing;
use proto::jobworkerp::data::{JobResult, JobResultId};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Response;

pub trait JobResultGrpc {
    fn app(&self) -> &Arc<dyn JobResultApp + 'static>;
}
// 1 day (same as expire_job_result_seconds in JobQueueConfig)
const DEFAULT_TIMEOUT: u64 = 1000 * 60 * 60 * 24;

#[tonic::async_trait]
impl<T: JobResultGrpc + Tracing + Send + Debug + Sync + 'static> JobResultService for T {
    #[tracing::instrument]
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
    #[tracing::instrument]
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
    #[tracing::instrument]
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
    #[tracing::instrument]
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
    #[tracing::instrument]
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
    #[tracing::instrument]
    async fn listen(
        &self,
        request: tonic::Request<ListenRequest>,
    ) -> Result<tonic::Response<JobResult>, tonic::Status> {
        let _s = Self::trace_request("job_result", "listen", &request);
        let req = request.get_ref();
        let res = match (req.job_id.as_ref(), req.worker.as_ref()) {
            (Some(job_id), Some(Worker::WorkerId(worker_id))) => {
                self.app()
                    .listen_result(
                        job_id,
                        Some(worker_id),
                        None,
                        req.timeout.unwrap_or(DEFAULT_TIMEOUT),
                    )
                    .await
            }
            (Some(job_id), Some(Worker::WorkerName(name))) => {
                self.app()
                    .listen_result(
                        job_id,
                        None,
                        Some(name),
                        req.timeout.unwrap_or(DEFAULT_TIMEOUT),
                    )
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter(
                "job_id and worker_id are required".to_string(),
            )
            .into()),
        };
        match res {
            Ok(res) => Ok(Response::new(res)),
            Err(e) => Err(handle_error(&e)),
        }
    }

    type ListenStreamByWorkerStream = BoxStream<'static, Result<JobResult, tonic::Status>>;
    #[tracing::instrument]
    async fn listen_stream_by_worker(
        &self,
        request: tonic::Request<ListenStreamByWorkerRequest>,
    ) -> std::result::Result<tonic::Response<Self::ListenStreamByWorkerStream>, tonic::Status> {
        use tokio_stream::StreamExt;

        let _s = Self::trace_request("job_result", "listen_by_worker", &request);
        let req = request.into_inner().worker;
        let res = match req.as_ref() {
            Some(listen_stream_by_worker_request::Worker::WorkerId(worker_id)) => {
                self.app()
                    .listen_result_stream_by_worker(Some(worker_id), None)
                    .await
            }
            Some(listen_stream_by_worker_request::Worker::WorkerName(name)) => {
                self.app()
                    .listen_result_stream_by_worker(None, Some(name))
                    .await
            }
            _ => Err(JobWorkerError::InvalidParameter("worker is required".to_string()).into()),
        };
        let req = req.clone();
        if let Ok(mut stream) = res {
            // spawn and channel are required if you want handle "disconnect" functionality
            // the `out_stream` will not be polled after client disconnect
            let (tx, rx) = mpsc::channel(128);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {
                            // Ctrl-C が押されたらループを抜ける
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
            // tokio::spawn(async move {
            //     while let Some(item) = stream.next().await {
            //         tracing::debug!("\treceive result item: worker = {:?}, item = {:?}", &req, &item);
            //         let res = match item {
            //             Ok(item) => Ok(item),
            //             Err(e) => Err(handle_error(&e)),
            //         };
            //         match tx.send(res).await {
            //             Ok(_) => {
            //                 // item (server response) was queued to be send to client
            //             }
            //             Err(_item) => {
            //                 // output_stream was build from rx and both are dropped
            //                 break;
            //             }
            //         }
            //     }
            //     tracing::info!("\tclient disconnected");
            // });

            let output_stream = ReceiverStream::new(rx);
            Ok(Response::new(
                Box::pin(output_stream) as Self::ListenStreamByWorkerStream
            ))
        } else {
            Err(handle_error(&res.err().unwrap()))
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
