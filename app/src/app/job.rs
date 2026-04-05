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
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::{
    UseJobQueueConfig,
    job::{
        queue::redis::RedisJobQueueRepository,
        redis::{RedisJobRepository, UseRedisJobRepository, schedule::RedisJobScheduleRepository},
        status::UseJobProcessingStatusRepository,
    },
};
use proto::calculate_direct_response_timeout_ms;
use proto::jobworkerp::data::{
    Job, JobExecutionOverrides, JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId,
    QueueType, ResponseType, ResultOutputItem, ResultStatus, RetryPolicy, StreamingType, Trailer,
    WorkerData, WorkerId, result_output_item,
};
use std::{collections::HashMap, fmt, sync::Arc};

/// Receiver that the dispatcher awaits to know when a spawned stream-publishing
/// task has finished. `None` means there is no background task to wait for.
pub type StreamCompletionReceiver = Option<tokio::sync::oneshot::Receiver<()>>;

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

    // NOTE: Both rdb_chan.rs and hybrid.rs impl accept `worker: &Worker` (by reference).
    // Keep signatures consistent when modifying.
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
    ///
    /// # Limitations (documented for callers)
    /// - In Standalone mode, QueueType::NORMAL jobs are not persisted to RDB,
    ///   so `find_job()` cannot detect them. However, running/pending jobs will
    ///   have an in-memory processing status. After worker restart, both job and
    ///   status are lost, so they are correctly identified as orphans.
    /// - Set `stale_threshold_hours` appropriately to avoid purging jobs that are
    ///   still legitimately running.
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

/// Shared orphaned-only purge logic for hybrid.rs and rdb_chan.rs.
///
/// Checks each stale candidate against both the job store and the status repository.
/// Only marks records as deleted when neither source has the job (true orphan).
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
pub trait RedisJobAppHelper:
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
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>
    where
        Self: Send + 'static,
    {
        let job_id = job.id.unwrap();

        // Wait before processing to handle scheduled jobs
        let res = match if self.is_run_after_job(job) {
            self.redis_job_repository()
                .add_run_after_job(job.clone())
                .await
                .map(|_| 1i64) // dummy
        } else {
            self.redis_job_repository()
                .enqueue_job(worker.channel.as_ref(), job)
                .await
        } {
            Ok(_) => {
                self.job_processing_status_repository()
                    .upsert_status(&job_id, &JobProcessingStatus::Pending)
                    .await?;

                // Call hook after PENDING status is set
                self.after_enqueue_to_redis_hook(job_id, job, worker, streaming_type);

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
                        // Calculate total timeout including retries (None means unlimited)
                        let job_timeout = job.data.as_ref().map(|d| d.timeout).unwrap_or(0);
                        let total_timeout = calculate_direct_response_timeout_ms(
                            job_timeout,
                            resolved.retry_policy.as_ref(),
                        );
                        self._wait_job_for_direct_response(
                            &job_id,
                            total_timeout,
                            request_streaming,
                        )
                        .await
                        .map(|(r, stream)| (job_id, Some(r), stream))
                    }
                } else {
                    Ok((job_id, None, None))
                }
            }
            Err(e) => Err(e),
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
}
