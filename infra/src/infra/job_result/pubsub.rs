pub mod chan;
pub mod redis;

use anyhow::Result;
use proto::jobworkerp::data::{JobId, JobResult, JobResultData, JobResultId};
use tonic::async_trait;

#[async_trait]
pub trait JobResultPublisher {
    async fn publish_result(&self, id: &JobResultId, data: &JobResultData) -> Result<bool>;
}
#[async_trait]
pub trait JobResultSubscriber {
    async fn subscribe_result(&self, job_id: &JobId, timeout: Option<u64>) -> Result<JobResult>;
}
