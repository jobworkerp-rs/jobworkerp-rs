pub mod cleanup;
pub mod memory;
pub mod rdb;
pub mod redis;

use std::sync::Arc;

use anyhow::Result;
use proto::jobworkerp::data::{JobId, JobProcessingStatus};
use tonic::async_trait;

/// Source of truth for live job processing state.
///
/// Note: this trait is authoritative. The RDB-backed
/// [`crate::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository`]
/// is an eventually-consistent secondary index of the same data — see its
/// docs for the inverted SoT relationship versus the rest of the codebase.
#[async_trait]
pub trait JobProcessingStatusRepository: Send + Sync + std::fmt::Debug + 'static {
    async fn upsert_status(&self, id: &JobId, status: &JobProcessingStatus) -> Result<bool>;
    async fn delete_status(&self, id: &JobId) -> Result<bool>;
    async fn find_status_all(&self) -> Result<Vec<(JobId, JobProcessingStatus)>>;
    async fn find_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>>;
}

pub trait UseJobProcessingStatusRepository {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository>;
}
