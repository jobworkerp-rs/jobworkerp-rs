pub mod constants;
pub mod execute;
pub mod hybrid;
pub mod rdb_chan;

// Test modules for job cancellation functionality
#[cfg(test)]
pub mod cancellation_test;
#[cfg(test)]
pub mod find_list_with_processing_status_test;
#[cfg(test)]
pub mod hybrid_indexing_integration_test;
#[cfg(test)]
pub mod process_deque_job_cleanup_test;
#[cfg(test)]
pub mod purge_stale_status_test;
#[cfg(test)]
pub mod rdb_chan_cancellation_test;
#[cfg(test)]
pub mod rdb_chan_indexing_integration_test;
use super::JobBuilder;
use super::worker::WorkerApp;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::{
    UseJobQueueConfig,
    job::{
        queue::redis::RedisJobQueueRepository,
        rdb::{RdbChanJobRepositoryImpl, RdbJobRepository},
        redis::{RedisJobRepository, UseRedisJobRepository, schedule::RedisJobScheduleRepository},
        status::{
            JobProcessingStatusRecord, StatusTransitionResult, UseJobProcessingStatusRepository,
        },
    },
};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobData, JobExecutionOverrides, JobId, JobProcessingStatus, JobResult, JobResultData,
    JobResultId, QueueType, ResponseType, ResultOutputItem, ResultStatus, RetryPolicy,
    StreamingType, Trailer, Worker, WorkerData, WorkerId, result_output_item,
};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

/// Receiver that the dispatcher awaits to know when a spawned stream-publishing
/// task has finished. `None` means there is no background task to wait for.
pub type StreamCompletionReceiver = Option<tokio::sync::oneshot::Receiver<()>>;

/// Deferred result of an `enqueue_job_with_channel` call: the job's optional
/// `JobResult` plus an optional output stream, resolved once the (Direct
/// response) job completes. `'static` so it can be returned past the borrow of
/// `self` and awaited by the caller after recording the `JobId`.
pub struct ChannelJobResultFuture {
    result: BoxFuture<'static, Result<Option<JobResult>>>,
    stream: Option<BoxStream<'static, ResultOutputItem>>,
}

impl ChannelJobResultFuture {
    pub fn new(
        result: BoxFuture<'static, Result<Option<JobResult>>>,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Self {
        Self { result, stream }
    }

    /// Split the already-subscribed output stream from the final-result wait.
    /// This lets transports forward output while the job is still running.
    pub fn into_parts(
        self,
    ) -> (
        BoxFuture<'static, Result<Option<JobResult>>>,
        Option<BoxStream<'static, ResultOutputItem>>,
    ) {
        (self.result, self.stream)
    }
}

impl Future for ChannelJobResultFuture {
    type Output = Result<(
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.result.as_mut().poll(cx) {
            Poll::Ready(result) => Poll::Ready(result.map(|result| (result, self.stream.take()))),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Guard that sends `()` on the oneshot when dropped.
/// Prevents the dispatcher from hanging if the spawned task panics or returns early.
pub struct OneshotCompletionGuard {
    sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl OneshotCompletionGuard {
    pub fn new(sender: tokio::sync::oneshot::Sender<()>) -> Self {
        Self {
            sender: Some(sender),
        }
    }
}

impl Drop for OneshotCompletionGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.sender.take() {
            let _ = tx.send(());
        }
    }
}

/// Resolved execution settings after merging worker defaults with per-job overrides.
#[derive(Debug, Clone)]
pub struct ResolvedJobParams {
    pub response_type: i32,
    pub store_success: bool,
    pub store_failure: bool,
    pub broadcast_results: bool,
    pub retry_policy: Option<RetryPolicy>,
}

/// Describes who owns publishing a job's initial PENDING status.
///
/// A retry has already atomically claimed PENDING before it reaches a queue.
/// Re-recording it with an upsert could resurrect a concurrent cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingStatusPublication {
    Create,
    AlreadyClaimedForRetry,
}

impl PendingStatusPublication {
    fn creates_status(self) -> bool {
        matches!(self, Self::Create)
    }
}

/// Default wait for a load-only job when the caller does not specify one.
/// Generous because an LLM model download can take minutes.
pub(crate) const DEFAULT_LOAD_TIMEOUT_MS: u64 = 600_000;

/// Build the minimal Job used to drive a load-only (config-check / pre-load)
/// request for `worker_id`: no args, and Direct response forced so the caller
/// can await the load outcome even when the worker's default is NoResult.
pub(crate) fn build_load_job(job_id: JobId, worker_id: &WorkerId, timeout_ms: Option<u64>) -> Job {
    #[allow(deprecated)]
    Job {
        id: Some(job_id),
        data: Some(JobData {
            worker_id: Some(*worker_id),
            args: Vec::new(),
            uniq_key: None,
            enqueue_time: command_utils::util::datetime::now_millis(),
            grabbed_until_time: None,
            run_after_time: 0,
            retried: 0,
            priority: 0,
            timeout: timeout_ms.unwrap_or(DEFAULT_LOAD_TIMEOUT_MS),
            streaming_type: StreamingType::None as i32,
            using: None,
            overrides: Some(JobExecutionOverrides {
                response_type: Some(ResponseType::Direct as i32),
                store_success: Some(false),
                store_failure: Some(false),
                broadcast_results: Some(false),
                retry_policy: None,
            }),
        }),
        metadata: HashMap::new(),
    }
}

/// Interpret a load-only job's Direct result: success returns Ok(true), any
/// non-success status returns an error carrying the runner's load() failure
/// message (e.g. a missing LLM model), and a missing result is treated as a
/// timeout. Shared by the rdb_chan and hybrid JobApp::load_worker impls.
pub(crate) fn load_result_to_outcome(
    worker_id: &WorkerId,
    result: Option<JobResult>,
) -> Result<bool> {
    let Some(data) = result.and_then(|r| r.data) else {
        return Err(JobWorkerError::TimeoutError(format!(
            "load timed out or returned no result for worker {}",
            worker_id.value
        ))
        .into());
    };
    if data.status == ResultStatus::Success as i32 {
        Ok(true)
    } else {
        let message = data
            .output
            .as_ref()
            .map(|o| String::from_utf8_lossy(&o.items).to_string())
            .unwrap_or_default();
        Err(JobWorkerError::RuntimeError(format!(
            "load failed for worker {}: {}",
            worker_id.value, message
        ))
        .into())
    }
}

/// Merge worker-level settings with optional per-job overrides.
/// Each override field, when present, replaces the worker default.
pub fn resolve_job_params(
    worker: &WorkerData,
    overrides: Option<&JobExecutionOverrides>,
) -> ResolvedJobParams {
    match overrides {
        None => ResolvedJobParams {
            response_type: worker.response_type,
            store_success: worker.store_success,
            store_failure: worker.store_failure,
            broadcast_results: worker.broadcast_results,
            retry_policy: worker.retry_policy,
        },
        Some(o) => ResolvedJobParams {
            response_type: o.response_type.unwrap_or(worker.response_type),
            store_success: o.store_success.unwrap_or(worker.store_success),
            store_failure: o.store_failure.unwrap_or(worker.store_failure),
            broadcast_results: o.broadcast_results.unwrap_or(worker.broadcast_results),
            retry_policy: if o.retry_policy.is_some() {
                o.retry_policy
            } else {
                worker.retry_policy
            },
        },
    }
}

/// Reject response and queue combinations whose semantics cannot be honored.
pub fn validate_response_queue_type(queue_type: i32, response_type: i32) -> Result<()> {
    if response_type == ResponseType::Direct as i32 && queue_type == QueueType::DbOnly as i32 {
        return Err(JobWorkerError::InvalidParameter(
            "response_type=Direct is not supported for queue_type=DbOnly".to_string(),
        )
        .into());
    }
    Ok(())
}

/// Resolve worker settings and reject unsupported response/queue combinations.
pub fn resolve_and_validate_job_params(
    worker: &WorkerData,
    overrides: Option<&JobExecutionOverrides>,
) -> Result<ResolvedJobParams> {
    let resolved = resolve_job_params(worker, overrides);
    validate_response_queue_type(worker.queue_type, resolved.response_type)?;
    Ok(resolved)
}

pub trait JobCacheKeys {
    fn find_cache_key(id: &JobId) -> String {
        ["j:eid:", &id.value.to_string()].join("")
    }

    fn find_list_cache_key(limit: Option<&i32>, offset: &i64) -> String {
        [
            "j:list:",
            limit
                .as_ref()
                .map(|l| l.to_string())
                .unwrap_or_else(|| "none".to_string())
                .as_str(),
            ":",
            offset.to_string().as_str(),
        ]
        .join("")
    }
}
#[async_trait]
pub trait JobApp: fmt::Debug + Send + Sync {
    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        worker_id: Option<&'a WorkerId>,
        worker_name: Option<&'a String>,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
        streaming_type: StreamingType,
        using: Option<String>,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>;

    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job_with_worker(
        &self,
        meta: Arc<HashMap<String, String>>,
        worker: Worker,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
        streaming_type: StreamingType,
        using: Option<String>,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>;

    /// Enqueue a job and return its `JobId` eagerly, deferring the result wait
    /// to the returned future.
    ///
    /// `enqueue_job` blocks until the (Direct-response) job completes before it
    /// returns the `JobId`, so a caller cannot learn the id while the job is
    /// still running. This variant performs only the enqueue side-effects (queue
    /// write, cache admission, Pending status) before returning, then hands back
    /// a `'static` future that resolves with the result. Callers that must act
    /// on the in-flight job — e.g. a workflow registering child jobs so it can
    /// cancel them on abort — register the id, then await the future.
    ///
    /// For non-Direct workers the future resolves immediately to `(None, None)`,
    /// mirroring `enqueue_job`'s behavior.
    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job_with_channel<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        worker: Worker,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
        streaming_type: StreamingType,
        using: Option<String>,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<(JobId, ChannelJobResultFuture)>;

    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job_with_temp_worker<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        worker_data: WorkerData,
        arg: Vec<u8>,
        uniq_key: Option<String>,
        run_after_time: i64,
        priority: i32,
        timeout: u64,
        reserved_job_id: Option<JobId>,
        streaming_type: StreamingType,
        with_random_name: bool,
        using: Option<String>,
        overrides: Option<JobExecutionOverrides>,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>;

    /// Run the worker's Runner load() for config validation / pre-loading.
    ///
    /// Enqueues a load-only job that the worker executes by running the runner's
    /// `load()` (with the worker's `runner_settings`) exactly as a job would
    /// before execution, then skipping `run()`. For `use_static=true` this warms
    /// up the runner pool; for `use_static=false` it instantiates and loads a
    /// runner to verify the settings, then drops it. The load runs on the worker
    /// side and its outcome is awaited via a Direct response, so failures (e.g. a
    /// missing LLM model) surface here as an error.
    ///
    /// Returns `Ok(true)` when the load succeeded.
    async fn load_worker(&self, worker_id: &WorkerId, timeout_ms: Option<u64>) -> Result<bool>
    where
        Self: Send + 'static;

    async fn update_job(&self, job: &Job) -> Result<()>;

    /// Complete job if the job finished
    ///
    /// # Arguments
    ///
    /// * `result` - JobResult
    /// * `worker` - WorkerData
    ///
    /// # Returns
    ///
    /// * `Result<bool>` - Result of runner_settings (true if changed data)
    ///
    async fn complete_job(
        &self,
        id: &JobResultId,
        result: &JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Result<(bool, StreamCompletionReceiver)>;
    async fn delete_job(&self, id: &JobId) -> Result<bool>;
    async fn find_job(&self, id: &JobId) -> Result<Option<Job>>
    where
        Self: Send + 'static;

    async fn find_job_list(&self, limit: Option<&i32>, offset: Option<&i64>) -> Result<Vec<Job>>
    where
        Self: Send + 'static;

    async fn find_job_queue_list(
        &self,
        limit: Option<&i32>,
        channel: Option<&str>,
    ) -> Result<Vec<(Job, Option<JobProcessingStatus>)>>
    where
        Self: Send + 'static;

    async fn find_list_with_processing_status(
        &self,
        status: JobProcessingStatus,
        limit: Option<&i32>,
    ) -> Result<Vec<(Job, JobProcessingStatus)>>
    where
        Self: Send + 'static;

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static;

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>>
    where
        Self: Send + 'static;

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobProcessingStatus)>>
    where
        Self: Send + 'static;

    /// Advanced search using RDB index (Sprint 3)
    ///
    /// Returns UNIMPLEMENTED error if JOB_STATUS_RDB_INDEXING=false
    #[allow(clippy::too_many_arguments)]
    async fn find_by_condition(
        &self,
        status: Option<JobProcessingStatus>,
        worker_id: Option<i64>,
        channel: Option<String>,
        min_elapsed_time_ms: Option<i64>,
        limit: i32,
        offset: i32,
        descending: bool,
    ) -> Result<Vec<infra::infra::job::status::rdb::JobProcessingStatusDetail>>
    where
        Self: Send + 'static;

    /// Count active job statuses using the same predicates as FindByCondition.
    ///
    /// Returns UNIMPLEMENTED error if JOB_STATUS_RDB_INDEXING=false
    async fn count_by_condition(
        &self,
        status: Option<JobProcessingStatus>,
        worker_id: Option<i64>,
        channel: Option<String>,
        min_elapsed_time_ms: Option<i64>,
        mode: infra::infra::job::status::rdb::JobProcessingStatusCountMode,
    ) -> Result<infra::infra::job::status::rdb::JobProcessingStatusCountResult>
    where
        Self: Send + 'static;

    /// Cleanup logically deleted job_processing_status records
    ///
    /// This method delegates to RdbJobProcessingStatusIndexRepository.cleanup_deleted_records()
    ///
    /// # Arguments
    /// * `retention_hours_override` - Override default retention hours (for testing)
    ///
    /// # Returns
    /// * `Ok((deleted_count, cutoff_time))` - Number of deleted records and cutoff timestamp
    /// * `Err` - If RDB indexing is disabled or database error occurs
    async fn cleanup_job_processing_status(
        &self,
        retention_hours_override: Option<u64>,
    ) -> Result<(u64, i64)>;

    /// Purge stale job_processing_status records (mark as logically deleted)
    ///
    /// # Modes
    /// - `orphaned_only=false`: Bulk mark all stale records as deleted.
    ///   Records with a corresponding job that has future run_after_time are excluded.
    /// - `orphaned_only=true`: Only mark records where the job no longer exists
    ///   in both the job store (`find_job()`) AND the processing status repository
    ///   (`find_status()`). Each candidate is checked individually (N+1 queries),
    ///   so this mode may be slow if many stale records exist. Use an appropriate
    ///   `stale_threshold_hours` to keep the candidate set small.
    ///   The gRPC request layer does not validate this threshold in orphaned-only
    ///   mode, but this value is still used to filter purge candidates.
    ///
    /// # Limitations (documented for callers)
    /// - In Standalone mode, QueueType::NORMAL jobs are not persisted to RDB,
    ///   so `find_job()` cannot detect them. However, running/pending jobs will
    ///   have an in-memory processing status. After worker restart, both job and
    ///   status are lost, so they are correctly identified as orphans.
    /// - Set `stale_threshold_hours` appropriately in bulk mode to avoid purging
    ///   jobs that are still legitimately running.
    async fn purge_stale_job_processing_status(
        &self,
        stale_threshold_hours: u64,
        orphaned_only: bool,
    ) -> Result<(u64, i64)>;

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>>;

    async fn restore_jobs_from_rdb(&self, include_grabbed: bool, limit: Option<&i32>)
    -> Result<()>;

    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>>;

    /// Generate a new unique job ID.
    /// Used by EnqueueWithClientStream to pre-allocate job ID before enqueue.
    fn generate_job_id(&self) -> Result<JobId>;

    /// Downcast to concrete type for testing internal methods
    fn as_any(&self) -> &dyn std::any::Any;
}

pub trait UseJobApp {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static>;
}

/// Synchronously reset the RDB search index back to PENDING for the retry path.
///
/// Unlike most index updates this runs awaited (not via `tokio::spawn`):
/// the row may carry `deleted_at` from a prior WAIT_RESULT/CANCELLING and
/// `index_status(Running)` refuses to resurrect a deleted row, so a worker
/// that grabs the job before the index is restored would silently lose the
/// RUNNING update. Callers must invoke this before any queue path makes
/// the job grabbable. RDB errors are logged but do not abort the retry.
pub(crate) async fn reset_index_to_pending_for_retry(
    index_repo: Option<&Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>>,
    job_id: &JobId,
) {
    let Some(repo) = index_repo else {
        return;
    };
    if let Err(e) = repo.reset_to_pending_by_job_id(job_id).await {
        tracing::warn!(
            error = ?e,
            job_id = job_id.value,
            "Failed to reset RDB index to PENDING on retry (non-critical)"
        );
    }
}

/// Backend hooks for the shared user-initiated cancellation lifecycle.
///
/// Status ownership is identical for the channel and Redis-backed apps; only
/// cleanup, cancellation notification, and optional index persistence differ.
const MAX_CANCELLATION_STATUS_TRANSITION_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingCancellationDisposition {
    AwaitQueuedDelivery,
    FinalizedWithoutDelivery,
}

pub(crate) async fn is_rdb_only_pending_job(
    job: &Job,
    worker_app: &Arc<dyn WorkerApp + 'static>,
    is_run_after: bool,
) -> Result<bool> {
    if is_run_after {
        return Ok(true);
    }

    let Some(worker_id) = job.data.as_ref().and_then(|data| data.worker_id.as_ref()) else {
        return Ok(false);
    };

    // RDB-only workers do not have a queue delivery that can finalize CANCELLING.
    Ok(worker_app
        .find(worker_id)
        .await?
        .and_then(|worker| worker.data)
        .is_some_and(|data| {
            data.periodic_interval > 0 || data.queue_type == QueueType::DbOnly as i32
        }))
}

pub(crate) async fn remove_pending_rdb_job<F>(
    repository: &RdbChanJobRepositoryImpl,
    worker_app: &Arc<dyn WorkerApp + 'static>,
    id: &JobId,
    is_run_after: F,
) -> Result<PendingCancellationDisposition>
where
    F: FnOnce(&Job) -> bool,
{
    let rdb_only = match repository.find(id).await? {
        Some(job) => is_rdb_only_pending_job(&job, worker_app, is_run_after(&job)).await?,
        None => false,
    };

    match repository.delete(id).await {
        Ok(true) if rdb_only => Ok(PendingCancellationDisposition::FinalizedWithoutDelivery),
        Ok(_) => Ok(PendingCancellationDisposition::AwaitQueuedDelivery),
        Err(error) => {
            tracing::warn!(
                job_id = id.value,
                ?error,
                "Failed to remove pending RDB queue entry during cancellation"
            );
            Ok(PendingCancellationDisposition::AwaitQueuedDelivery)
        }
    }
}

pub(crate) async fn remove_unpublished_rdb_only_job<F>(
    repository: &RdbChanJobRepositoryImpl,
    worker_app: &Arc<dyn WorkerApp + 'static>,
    id: &JobId,
    is_run_after: F,
) -> Result<bool>
where
    F: FnOnce(&Job) -> bool,
{
    let Some(job) = repository.find(id).await? else {
        return Ok(false);
    };
    if !is_rdb_only_pending_job(&job, worker_app, is_run_after(&job)).await? {
        return Ok(false);
    }

    repository.delete(id).await
}

#[async_trait]
pub(crate) trait JobCancellationLifecycle: UseJobProcessingStatusRepository {
    async fn broadcast_cancelled_job(&self, id: &JobId) -> Result<()>;

    async fn record_cancelling_index(&self, _id: &JobId) {}

    async fn remove_pending_rdb_queue_entry(
        &self,
        _id: &JobId,
    ) -> Result<PendingCancellationDisposition> {
        Ok(PendingCancellationDisposition::AwaitQueuedDelivery)
    }

    /// Removes an RDB-only job that is visible before its PENDING status is published.
    async fn cancel_unpublished_rdb_job(&self, _id: &JobId) -> Result<bool> {
        Ok(false)
    }

    async fn record_pending_cancellation_completion(&self, _id: &JobId) {}

    async fn cancel_job_lifecycle(&self, id: &JobId) -> Result<bool> {
        let Some(mut record) = self
            .job_processing_status_repository()
            .find_status_record(id)
            .await?
        else {
            return self.cancel_unpublished_rdb_job(id).await;
        };
        if record.status == JobProcessingStatus::Unknown {
            return Ok(false);
        }
        for _ in 0..MAX_CANCELLATION_STATUS_TRANSITION_ATTEMPTS {
            if record.status == JobProcessingStatus::Unknown {
                return Ok(false);
            }
            if record.status == JobProcessingStatus::Cancelling {
                return Ok(true);
            }

            let cancelling = JobProcessingStatusRecord {
                status: JobProcessingStatus::Cancelling,
                retried: record.retried,
            };
            match self
                .job_processing_status_repository()
                .compare_and_set_status(id, Some(record), Some(cancelling))
                .await?
            {
                StatusTransitionResult::Applied => {
                    self.record_cancelling_index(id).await;
                    if record.status == JobProcessingStatus::Pending {
                        if self.remove_pending_rdb_queue_entry(id).await?
                            == PendingCancellationDisposition::FinalizedWithoutDelivery
                        {
                            self.record_pending_cancellation_completion(id).await;
                            self.job_processing_status_repository()
                                .delete_status(id)
                                .await?;
                        }
                    } else {
                        self.broadcast_cancelled_job(id).await?;
                    }
                    // A queued delivery observes CANCELLING and publishes a
                    // terminal Cancelled result before complete_job cleans up.
                    return Ok(true);
                }
                StatusTransitionResult::Conflict(Some(current)) => {
                    // Retry against the authoritative state: a dispatcher may
                    // have claimed PENDING as RUNNING while Delete was in flight.
                    record = current;
                }
                StatusTransitionResult::Conflict(None) => return Ok(false),
            }
        }

        Ok(matches!(
            self.job_processing_status_repository()
                .find_status_record(id)
                .await?,
            Some(JobProcessingStatusRecord {
                status: JobProcessingStatus::Cancelling,
                ..
            })
        ))
    }
}

/// Shared orphaned-only purge logic for hybrid.rs and rdb_chan.rs.
///
/// Walks the candidates produced by `find_stale_job_ids` and asks the caller-
/// supplied `is_orphaned` predicate whether each one should be marked as
/// deleted in the RDB index.
///
/// # Orphan determination
///
/// The predicate must check both:
/// 1. the live `JobProcessingStatusRepository` (Redis/Memory SoT for normal
///    response/queue paths), and
/// 2. the `job` table (SoT for queue types that don't populate live status —
///    `QueueType::DbOnly`, periodic workers, and future `run_after_time`
///    jobs — and also a guard against transient cleanup races where the live
///    status has just been cleared but the job row hasn't yet).
///
/// A row is orphan only when neither source acknowledges the job.
pub(crate) async fn purge_orphaned_stale_records<F, Fut>(
    index_repo: &infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository,
    stale_threshold_hours: u64,
    is_orphaned: F,
) -> Result<(u64, i64)>
where
    F: Fn(JobId) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let (stale_job_ids, cutoff_time) = index_repo.find_stale_job_ids(stale_threshold_hours).await?;

    let mut marked_count = 0u64;
    for job_id_value in stale_job_ids {
        let job_id = JobId {
            value: job_id_value,
        };
        match is_orphaned(job_id).await {
            Ok(true) => {
                index_repo
                    .mark_deleted_by_job_id(&job_id)
                    .await
                    .inspect_err(|_| {
                        tracing::warn!(
                            job_id = job_id.value,
                            marked_count,
                            "purge_orphaned interrupted during mark_deleted"
                        );
                    })?;
                marked_count += 1;
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(
                    job_id = job_id.value,
                    marked_count,
                    "purge_orphaned interrupted during is_orphaned check"
                );
                return Err(e);
            }
        }
    }

    Ok((marked_count, cutoff_time))
}

/// Spawn a background task to publish an End marker stream when a streaming job
/// failed before creating a stream (stream=None, status!=Success).
/// This unblocks subscribers waiting on `subscribe_result_stream`.
pub(crate) fn spawn_end_marker_if_needed<P: JobResultPublisher + Clone + Send + 'static>(
    data: &JobResultData,
    jid: &JobId,
    pubsub_repo: &P,
) -> StreamCompletionReceiver {
    if data.streaming_type != StreamingType::None as i32
        && data.status != ResultStatus::Success as i32
    {
        let end_item = ResultOutputItem {
            item: Some(result_output_item::Item::End(Trailer {
                metadata: Default::default(),
            })),
        };
        let end_stream = futures::stream::once(async move { end_item }).boxed();
        let pubsub_repo = pubsub_repo.clone();
        let job_id_for_stream = *jid;
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _guard = OneshotCompletionGuard::new(tx);
            if let Err(e) = pubsub_repo
                .publish_result_stream_data(job_id_for_stream, end_stream)
                .await
            {
                tracing::warn!(
                    "complete_job: end marker publish error for job {}: {:?}",
                    job_id_for_stream.value,
                    e
                );
            }
        });
        Some(rx)
    } else {
        None
    }
}

#[async_trait]
pub(crate) trait RedisJobAppHelper:
    UseRedisJobRepository + JobBuilder + UseJobQueueConfig + UseJobProcessingStatusRepository
where
    Self: Sized + 'static,
{
    /// Hook called after successfully enqueueing a job to Redis with PENDING status set
    ///
    /// Default implementation does nothing. Override to add custom behavior like RDB indexing.
    ///
    /// # Arguments
    /// * `job_id` - The enqueued job ID
    /// * `job` - The job data
    /// * `worker` - The worker configuration
    /// * `streaming_type` - The streaming type for this job
    #[allow(unused_variables)]
    fn after_enqueue_to_redis_hook(
        &self,
        job_id: JobId,
        job: &Job,
        worker: &WorkerData,
        streaming_type: StreamingType,
    ) {
        // Default: no-op
    }
    /// TODO move to job/hybrid.rs
    async fn enqueue_job_to_redis_with_wait_if_needed(
        &self,
        job: &Job,
        worker: &WorkerData,
        streaming_type: StreamingType,
        load_only: bool,
        pending_status_publication: PendingStatusPublication,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>
    where
        Self: Send + 'static,
    {
        let job_id = job.id.unwrap();
        let should_record_pending = !load_only && pending_status_publication.creates_status();

        if should_record_pending {
            self.job_processing_status_repository()
                .upsert_status(&job_id, &JobProcessingStatus::Pending)
                .await?;
        }

        // Wait before processing to handle scheduled jobs
        let res = match if self.is_run_after_job(job) {
            self.redis_job_repository()
                .add_run_after_job(job.clone())
                .await
                .map(|_| 1i64) // dummy
        } else {
            self.redis_job_repository()
                .enqueue_job_with_load_only(worker.channel.as_ref(), job, load_only)
                .await
        } {
            Ok(_) => {
                // Load-only (config-check / pre-load) requests are not real jobs:
                // no Pending status, no enqueue hook, and no running-job RDB
                // record — they have no lifecycle to observe and the worker side
                // likewise skips all status management for them. Only the Direct
                // result is awaited below.
                if !load_only {
                    // Retry publication already owns a conditional PENDING
                    // transition, so it must not overwrite a later cancellation.
                    if pending_status_publication.creates_status() {
                        self.after_enqueue_to_redis_hook(job_id, job, worker, streaming_type);
                    }

                    // TTL prevents job orphaning when worker fails unexpectedly
                    if worker.queue_type == QueueType::Normal as i32
                        && let Some(job_data) = &job.data
                    {
                        // For timeout=0 (unlimited), uses expire_job_result_seconds from config
                        let ttl = self.calculate_job_ttl(job_data.timeout);
                        self.redis_job_repository()
                            .create_with_expire(&job_id, job_data, ttl)
                            .await?;
                        tracing::debug!(
                            "Created job {} with TTL {:?} for running job visibility",
                            job_id.value,
                            ttl
                        );
                    }
                }
                // Direct response requires blocking until job completion
                // EXCEPT for STREAMING_TYPE_INTERNAL - these jobs should return immediately
                // so the caller can subscribe to the stream before data is published.
                //
                // StreamingType::Internal means the job uses streaming internally (run_stream())
                // but the final result is collected via collect_stream() and returned as a single
                // chunk. This is typically used by workflow steps that need the final result but
                // want to leverage streaming-capable runners for better resource management.
                // The caller subscribes to the stream, collects chunks, and receives FinalCollected.
                let resolved = resolve_job_params(
                    worker,
                    job.data.as_ref().and_then(|d| d.overrides.as_ref()),
                );
                if resolved.response_type == ResponseType::Direct as i32 {
                    if streaming_type == StreamingType::Internal {
                        // For Internal streaming jobs with Direct response_type:
                        // Return immediately without waiting for job completion.
                        // The caller is responsible for subscribing to the stream
                        // and collecting results via RunnerSpec::collect_stream().
                        // This allows the caller to subscribe before worker publishes
                        // stream data (avoiding race condition where data is published
                        // before subscriber is ready).
                        tracing::debug!(
                            "Internal streaming job with Direct response_type: returning immediately (job_id={})",
                            job_id.value
                        );
                        Ok((job_id, None, None))
                    } else {
                        // Non-Internal streaming or no streaming: wait for job completion
                        let request_streaming = streaming_type == StreamingType::Response;
                        self._wait_job_for_direct_response(&job_id, None, request_streaming)
                            .await
                            .map(|(r, stream)| (job_id, Some(r), stream))
                    }
                } else {
                    Ok((job_id, None, None))
                }
            }
            Err(error) => {
                if should_record_pending
                    && let Err(cleanup_error) = self
                        .job_processing_status_repository()
                        .delete_status(&job_id)
                        .await
                {
                    tracing::warn!(
                        job_id = job_id.value,
                        %cleanup_error,
                        "failed to remove PENDING status after Redis enqueue failure"
                    );
                }
                Err(error)
            }
        }?;
        Ok(res)
    }

    #[inline]
    async fn _wait_job_for_direct_response(
        &self,
        job_id: &JobId,
        timeout: Option<u64>,
        request_streaming: bool,
    ) -> Result<(JobResult, Option<BoxStream<'static, ResultOutputItem>>)> {
        self.redis_job_repository()
            .wait_for_result_queue_for_response(job_id, timeout, request_streaming)
            .await
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use proto::jobworkerp::data::{RetryPolicy, RetryType, RunnerId};

    fn test_worker() -> WorkerData {
        WorkerData {
            name: "test".to_string(),
            description: String::new(),
            runner_id: Some(RunnerId { value: 1 }),
            runner_settings: vec![],
            channel: None,
            response_type: ResponseType::NoResult as i32,
            periodic_interval: 0,
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Constant as i32,
                interval: 1000,
                max_interval: 0,
                max_retry: 3,
                basis: 0.0,
            }),
            queue_type: 0,
            store_success: false,
            store_failure: true,
            use_static: false,
            broadcast_results: false,
        }
    }

    #[test]
    fn test_resolve_none_returns_worker_defaults() {
        let w = test_worker();
        let eff = resolve_job_params(&w, None);
        assert_eq!(eff.response_type, ResponseType::NoResult as i32);
        assert!(!eff.store_success);
        assert!(eff.store_failure);
        assert!(!eff.broadcast_results);
        assert_eq!(eff.retry_policy.as_ref().unwrap().max_retry, 3);
    }

    #[test]
    fn direct_db_only_is_rejected() {
        let mut worker = test_worker();
        worker.response_type = ResponseType::Direct as i32;
        worker.queue_type = QueueType::DbOnly as i32;

        let error = resolve_and_validate_job_params(&worker, None).unwrap_err();
        assert!(error.to_string().contains("queue_type=DbOnly"));
    }

    #[test]
    fn test_resolve_full_override() {
        let w = test_worker();
        let o = JobExecutionOverrides {
            response_type: Some(ResponseType::Direct as i32),
            store_success: Some(true),
            store_failure: Some(false),
            broadcast_results: Some(true),
            retry_policy: Some(RetryPolicy {
                r#type: RetryType::Exponential as i32,
                interval: 2000,
                max_interval: 60000,
                max_retry: 10,
                basis: 2.0,
            }),
        };
        let eff = resolve_job_params(&w, Some(&o));
        assert_eq!(eff.response_type, ResponseType::Direct as i32);
        assert!(eff.store_success);
        assert!(!eff.store_failure);
        assert!(eff.broadcast_results);
        let rp = eff.retry_policy.unwrap();
        assert_eq!(rp.r#type, RetryType::Exponential as i32);
        assert_eq!(rp.max_retry, 10);
    }

    #[test]
    fn test_resolve_partial_override() {
        let w = test_worker();
        let o = JobExecutionOverrides {
            response_type: Some(ResponseType::Direct as i32),
            store_success: None,
            store_failure: None,
            broadcast_results: Some(true),
            retry_policy: None,
        };
        let eff = resolve_job_params(&w, Some(&o));
        assert_eq!(eff.response_type, ResponseType::Direct as i32);
        // Not overridden: use worker defaults
        assert!(!eff.store_success);
        assert!(eff.store_failure);
        assert!(eff.broadcast_results);
        // retry_policy not overridden: worker's policy
        assert_eq!(eff.retry_policy.as_ref().unwrap().max_retry, 3);
    }

    #[test]
    fn test_resolve_empty_override() {
        let w = test_worker();
        let o = JobExecutionOverrides {
            response_type: None,
            store_success: None,
            store_failure: None,
            broadcast_results: None,
            retry_policy: None,
        };
        let eff = resolve_job_params(&w, Some(&o));
        // All None: same as worker defaults
        assert_eq!(eff.response_type, ResponseType::NoResult as i32);
        assert!(!eff.store_success);
        assert!(eff.store_failure);
        assert!(!eff.broadcast_results);
        assert_eq!(eff.retry_policy.as_ref().unwrap().max_retry, 3);
    }

    /// Verify the streaming overrides pattern used by worker-path streaming.
    #[test]
    fn test_resolve_streaming_overrides_on_direct_worker() {
        // Worker configured as Direct response_type (typical for streaming)
        let w = WorkerData {
            response_type: ResponseType::Direct as i32,
            store_success: false,
            store_failure: false,
            broadcast_results: false,
            ..test_worker()
        };
        // Overrides set NoResult + broadcast (streaming pattern)
        let o = JobExecutionOverrides {
            response_type: Some(ResponseType::NoResult as i32),
            store_success: Some(true),
            store_failure: Some(true),
            broadcast_results: Some(true),
            retry_policy: None,
        };
        let eff = resolve_job_params(&w, Some(&o));
        // response_type overridden to NoResult
        assert_eq!(eff.response_type, ResponseType::NoResult as i32);
        assert!(eff.store_success);
        assert!(eff.store_failure);
        assert!(eff.broadcast_results);
        // retry_policy not overridden: worker's policy
        assert_eq!(eff.retry_policy.as_ref().unwrap().max_retry, 3);
    }

    #[test]
    fn test_oneshot_completion_guard_sends_on_drop() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        {
            let _guard = OneshotCompletionGuard::new(tx);
            // guard dropped here
        }
        // receiver should get the signal
        assert!(rx.blocking_recv().is_ok());
    }

    #[test]
    fn test_oneshot_completion_guard_sends_on_panic() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = OneshotCompletionGuard::new(tx);
            panic!("test panic");
        }));
        assert!(result.is_err());
        // receiver should still get the signal despite panic
        assert!(rx.blocking_recv().is_ok());
    }

    #[test]
    fn test_stream_completion_receiver_type() {
        // Verify StreamCompletionReceiver is None by default
        let rx: StreamCompletionReceiver = None;
        assert!(rx.is_none());

        // Verify Some(rx) works with oneshot
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let completion: StreamCompletionReceiver = Some(rx);
        assert!(completion.is_some());
        let _ = tx.send(());
    }

    #[test]
    fn build_load_job_forces_direct_and_carries_worker() {
        let wid = WorkerId { value: 7 };
        let job = build_load_job(JobId { value: 99 }, &wid, Some(12345));
        let data = job.data.unwrap();
        assert_eq!(data.worker_id, Some(wid));
        assert!(data.args.is_empty());
        assert_eq!(data.run_after_time, 0);
        assert_eq!(data.timeout, 12345);
        let ov = data.overrides.unwrap();
        assert_eq!(ov.response_type, Some(ResponseType::Direct as i32));
        assert_eq!(ov.store_success, Some(false));
        assert_eq!(ov.store_failure, Some(false));
    }

    #[test]
    fn build_load_job_uses_default_timeout_when_unset() {
        let wid = WorkerId { value: 7 };
        let job = build_load_job(JobId { value: 99 }, &wid, None);
        assert_eq!(job.data.unwrap().timeout, DEFAULT_LOAD_TIMEOUT_MS);
    }

    #[test]
    fn load_result_to_outcome_maps_status() {
        use proto::jobworkerp::data::{ResultOutput, ResultStatus};
        let wid = WorkerId { value: 1 };

        // success
        let ok = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::Success as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(load_result_to_outcome(&wid, Some(ok)).unwrap());

        // failure carries the runner's error message
        let fail = JobResult {
            data: Some(JobResultData {
                status: ResultStatus::FatalError as i32,
                output: Some(ResultOutput {
                    items: b"model not found".to_vec(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = load_result_to_outcome(&wid, Some(fail)).unwrap_err();
        assert!(err.to_string().contains("model not found"));

        // no result is treated as a timeout
        assert!(load_result_to_outcome(&wid, None).is_err());
    }
}

#[cfg(test)]
mod cancellation_lifecycle_tests {
    use super::*;
    use infra::infra::job::status::JobProcessingStatusRepository;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex;

    #[derive(Debug)]
    struct PendingToRunningConflictRepository {
        record: Mutex<JobProcessingStatusRecord>,
        compare_count: AtomicUsize,
        transition_to_running_on_first_compare: bool,
    }

    impl PendingToRunningConflictRepository {
        fn new(transition_to_running_on_first_compare: bool) -> Self {
            Self {
                record: Mutex::new(JobProcessingStatusRecord {
                    status: JobProcessingStatus::Pending,
                    retried: 0,
                }),
                compare_count: AtomicUsize::new(0),
                transition_to_running_on_first_compare,
            }
        }
    }

    #[async_trait]
    impl JobProcessingStatusRepository for PendingToRunningConflictRepository {
        async fn upsert_status(&self, _id: &JobId, _status: &JobProcessingStatus) -> Result<bool> {
            unreachable!("the cancellation lifecycle only uses compare_and_set_status")
        }

        async fn delete_status(&self, _id: &JobId) -> Result<bool> {
            unreachable!("the cancellation lifecycle only uses compare_and_set_status")
        }

        async fn find_status_all(&self) -> Result<Vec<(JobId, JobProcessingStatus)>> {
            unreachable!("the cancellation lifecycle only reads one status record")
        }

        async fn find_status(&self, _id: &JobId) -> Result<Option<JobProcessingStatus>> {
            unreachable!("the cancellation lifecycle only reads one status record")
        }

        async fn find_status_record(
            &self,
            _id: &JobId,
        ) -> Result<Option<JobProcessingStatusRecord>> {
            Ok(Some(*self.record.lock().await))
        }

        async fn compare_and_set_status(
            &self,
            _id: &JobId,
            expected: Option<JobProcessingStatusRecord>,
            next: Option<JobProcessingStatusRecord>,
        ) -> Result<StatusTransitionResult> {
            let mut record = self.record.lock().await;
            if self.transition_to_running_on_first_compare
                && self.compare_count.fetch_add(1, Ordering::SeqCst) == 0
            {
                *record = JobProcessingStatusRecord {
                    status: JobProcessingStatus::Running,
                    retried: 0,
                };
                return Ok(StatusTransitionResult::Conflict(Some(*record)));
            }
            assert_eq!(expected, Some(*record));
            *record = next.expect("cancellation must retain a status record");
            Ok(StatusTransitionResult::Applied)
        }
    }

    #[derive(Debug)]
    struct TestCancellationLifecycle {
        repository: Arc<PendingToRunningConflictRepository>,
        broadcast_count: AtomicUsize,
        pending_rdb_removal_count: AtomicUsize,
    }

    impl UseJobProcessingStatusRepository for TestCancellationLifecycle {
        fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
            self.repository.clone()
        }
    }

    #[async_trait]
    impl JobCancellationLifecycle for TestCancellationLifecycle {
        async fn broadcast_cancelled_job(&self, _id: &JobId) -> Result<()> {
            self.broadcast_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn remove_pending_rdb_queue_entry(
            &self,
            _id: &JobId,
        ) -> Result<PendingCancellationDisposition> {
            self.pending_rdb_removal_count
                .fetch_add(1, Ordering::SeqCst);
            Ok(PendingCancellationDisposition::AwaitQueuedDelivery)
        }
    }

    #[tokio::test]
    async fn cancellation_retries_after_pending_to_running_conflict() {
        let repository = Arc::new(PendingToRunningConflictRepository::new(true));
        let lifecycle = TestCancellationLifecycle {
            repository: repository.clone(),
            broadcast_count: AtomicUsize::new(0),
            pending_rdb_removal_count: AtomicUsize::new(0),
        };

        assert!(
            lifecycle
                .cancel_job_lifecycle(&JobId { value: 1 })
                .await
                .unwrap()
        );
        assert_eq!(
            lifecycle.pending_rdb_removal_count.load(Ordering::SeqCst),
            0
        );
        assert_eq!(lifecycle.broadcast_count.load(Ordering::SeqCst), 1);
        assert_eq!(
            repository
                .find_status_record(&JobId { value: 1 })
                .await
                .unwrap(),
            Some(JobProcessingStatusRecord {
                status: JobProcessingStatus::Cancelling,
                retried: 0,
            })
        );
    }

    #[tokio::test]
    async fn pending_cancellation_is_retained_for_dispatch_finalization() {
        let repository = Arc::new(PendingToRunningConflictRepository::new(false));
        let lifecycle = TestCancellationLifecycle {
            repository: repository.clone(),
            broadcast_count: AtomicUsize::new(0),
            pending_rdb_removal_count: AtomicUsize::new(0),
        };

        assert!(
            lifecycle
                .cancel_job_lifecycle(&JobId { value: 2 })
                .await
                .unwrap()
        );
        assert_eq!(
            lifecycle.pending_rdb_removal_count.load(Ordering::SeqCst),
            1
        );
        assert_eq!(lifecycle.broadcast_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            repository
                .find_status_record(&JobId { value: 2 })
                .await
                .unwrap(),
            Some(JobProcessingStatusRecord {
                status: JobProcessingStatus::Cancelling,
                retried: 0,
            })
        );
    }
}
