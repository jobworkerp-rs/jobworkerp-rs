pub mod memory;
pub mod redis;

use std::sync::Arc;

use anyhow::Result;
use proto::jobworkerp::data::{JobId, JobStatus};
use tonic::async_trait;

#[async_trait]
pub trait JobStatusRepository: Send + Sync + 'static {
    async fn upsert_status(&self, id: &JobId, status: &JobStatus) -> Result<bool>;
    async fn delete_status(&self, id: &JobId) -> Result<bool>;
    async fn find_status_all(&self) -> Result<Vec<(JobId, JobStatus)>>;
    async fn find_status(&self, id: &JobId) -> Result<Option<JobStatus>>;
}

pub trait UseJobStatusRepository {
    fn job_status_repository(&self) -> Arc<dyn JobStatusRepository>;
}
