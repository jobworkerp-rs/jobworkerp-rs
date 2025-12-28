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
pub mod rdb_chan_cancellation_test;
#[cfg(test)]
pub mod rdb_chan_indexing_integration_test;

use super::JobBuilder;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use infra::infra::{
    job::{
        queue::redis::RedisJobQueueRepository,
        redis::{schedule::RedisJobScheduleRepository, RedisJobRepository, UseRedisJobRepository},
        status::UseJobProcessingStatusRepository,
    },
    UseJobQueueConfig,
};
use proto::calculate_direct_response_timeout_ms;
use proto::jobworkerp::data::{
    Job, JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId, QueueType,
    ResponseType, ResultOutputItem, StreamingType, WorkerData, WorkerId,
};
use std::{collections::HashMap, fmt, sync::Arc};

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
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )>;

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
    ) -> Result<bool>;
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

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>>;

    async fn restore_jobs_from_rdb(&self, include_grabbed: bool, limit: Option<&i32>)
        -> Result<()>;

    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>>;

    /// Downcast to concrete type for testing internal methods
    fn as_any(&self) -> &dyn std::any::Any;
}

pub trait UseJobApp {
    fn job_app(&self) -> &Arc<dyn JobApp + 'static>;
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
                if worker.queue_type == QueueType::Normal as i32 {
                    if let Some(job_data) = &job.data {
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
                if worker.response_type == ResponseType::Direct as i32 {
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
                            worker.retry_policy.as_ref(),
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
