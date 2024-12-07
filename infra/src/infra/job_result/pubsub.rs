pub mod chan;
pub mod redis;

use std::pin::Pin;

use anyhow::Result;
use futures::Stream;
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId, WorkerId};
use tonic::async_trait;

#[async_trait]
pub trait JobResultPublisher {
    /// publish job result to all listeners
    /// return true if there is at least one listener
    /// return false if there is no listener
    /// return error if failed to publish
    /// if to_listen is false, do not publish to response_type: listen_after
    async fn publish_result(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        to_listen: bool,
    ) -> Result<bool>;
}
#[async_trait]
pub trait JobResultSubscriber {
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<u64>) -> Result<JobResult>;
    async fn subscribe_result_stream_by_worker(
        &self,
        worker_id: WorkerId,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<JobResult>> + Send>>>;
}
