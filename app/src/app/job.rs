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
pub mod rdb_chan_cancellation_test;

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
use proto::jobworkerp::data::{
    Job, JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId, QueueType,
    ResponseType, ResultOutputItem, WorkerData, WorkerId,
};
use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

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
        request_streaming: bool,
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
        request_streaming: bool,
        with_random_name: bool,
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

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>>;

    async fn restore_jobs_from_rdb(&self, include_grabbed: bool, limit: Option<&i32>)
        -> Result<()>;

    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>>;
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
    async fn enqueue_job_to_redis_with_wait_if_needed(
        &self,
        job: &Job,
        worker: &WorkerData,
        request_streaming: bool,
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
                // TTL prevents job orphaning when worker fails unexpectedly
                if worker.queue_type == QueueType::Normal as i32 {
                    if let Some(job_data) = &job.data {
                        // Safety margin prevents premature cleanup during normal completion
                        let ttl = Duration::from_millis(job_data.timeout + 300_000);
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
                if worker.response_type == ResponseType::Direct as i32 {
                    // Connection kept open to maintain real-time response capability
                    self._wait_job_for_direct_response(
                        &job_id,
                        job.data.as_ref().map(|d| d.timeout),
                        request_streaming,
                    )
                    .await
                    .map(|(r, stream)| (job_id, Some(r), stream))
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
