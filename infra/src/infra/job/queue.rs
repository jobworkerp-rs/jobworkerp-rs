pub mod chan;
pub mod rdb;
pub mod redis;

// Re-export implementations
pub use chan::ChanJobQueueRepositoryImpl;
pub use redis::RedisJobQueueRepositoryImpl;

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::future::BoxFuture;
use proto::jobworkerp::data::{Job, JobId, JobResult, JobResultData, JobResultId, Priority};

pub trait JobQueueRepository: Sync + 'static {
    // for front (send job to worker)
    // return: jobqueue size
    fn enqueue_job(
        &self,
        channel_name: Option<&String>,
        job: &Job,
    ) -> impl std::future::Future<Output = Result<i64>> + Send
    where
        Self: Send + Sync;

    // send job result from worker to front directly
    fn enqueue_result_direct(
        &self,
        id: &JobResultId,
        res: &JobResultData,
    ) -> impl std::future::Future<Output = Result<bool>> + Send;

    // wait response from worker for direct response job
    // TODO shutdown lock until receive result ? (but not recorded...)
    fn wait_for_result_queue_for_response(
        &self,
        job_id: &JobId,
        timeout: Option<&u64>,
    ) -> impl std::future::Future<Output = Result<JobResult>> + Send;

    fn find_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        id: &JobId,
    ) -> impl std::future::Future<Output = Result<Option<Job>>> + Send;

    fn find_multi_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        ids: &HashSet<i64>,
    ) -> impl std::future::Future<Output = Result<Vec<Job>>> + Send;

    fn delete_from_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
        job: &Job,
    ) -> impl std::future::Future<Output = Result<i32>> + Send;

    fn count_queue(
        &self,
        channel: Option<&String>,
        priority: Priority,
    ) -> impl std::future::Future<Output = Result<i64>> + Send;
}

/// DI trait for accessing JobQueueCancellationRepository
pub trait UseJobQueueCancellationRepository {
    fn job_queue_cancellation_repository(&self) -> Arc<dyn JobQueueCancellationRepository>;
}

/// Extended job queue repository interface for cancellation functionality
///
/// This trait provides job cancellation broadcast capabilities for distributed workers.
#[async_trait]
pub trait JobQueueCancellationRepository: Send + Sync + 'static + std::fmt::Debug {
    /// Check if a job is cancelled
    async fn is_cancelled(&self, job_id: &JobId) -> Result<bool>;

    /// Broadcast job cancellation notification to all workers
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()>;

    /// Subscribe to job cancellation notifications
    async fn subscribe_job_cancellation(
        &self,
        callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static>,
    ) -> Result<()>;

    /// Subscribe to job cancellation notifications with timeout and cleanup support
    ///
    /// **Leak prevention**: Job timeout + margin for automatic disconnection
    /// **Simple design**: No complex control needed, leverages redis-rs standard functionality
    async fn subscribe_job_cancellation_with_timeout(
        &self,
        callback: Box<dyn Fn(JobId) -> BoxFuture<'static, Result<()>> + Send + Sync + 'static>,
        job_timeout_ms: u64, // Job timeout time (milliseconds)
        cleanup_receiver: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()>;
}
