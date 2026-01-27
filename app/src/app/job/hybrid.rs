use crate::app::{UseWorkerConfig, WorkerConfig};
use crate::module::AppConfigModule;

use super::super::JobBuilder;
use super::super::worker::{UseWorkerApp, WorkerApp};
use super::{JobApp, JobCacheKeys, RedisJobAppHelper};
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use futures::stream::BoxStream;
use infra::infra::job::queue::JobQueueCancellationRepository;
use infra::infra::job::queue::redis::RedisJobQueueRepository;
use infra::infra::job::rdb::{RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::redis::{RedisJobRepository, UseRedisJobRepository};
use infra::infra::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use infra::infra::job_result::pubsub::JobResultPublisher;
use infra::infra::job_result::pubsub::redis::{
    RedisJobResultPubSubRepositoryImpl, UseRedisJobResultPubSubRepository,
};
use infra::infra::module::HybridRepositoryModule;
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::module::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::moka::{MokaCacheImpl, UseMokaCache};
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId, Priority,
    QueueType, ResponseType, ResultOutputItem, StreamingType, Worker, WorkerData, WorkerId,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct HybridJobAppImpl {
    app_config_module: Arc<AppConfigModule>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<HybridRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    memory_cache: MokaCacheImpl<Arc<String>, Vec<Job>>,
    job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
    // RDB index repository for JobProcessingStatus (controlled by JOB_STATUS_RDB_INDEXING env var)
    job_status_index_repository:
        Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>>,
}

impl HybridJobAppImpl {
    pub fn new(
        app_config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<HybridRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        memory_cache: MokaCacheImpl<Arc<String>, Vec<Job>>,
        job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
        job_status_index_repository: Option<
            Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>,
        >,
    ) -> Self {
        Self {
            app_config_module,
            id_generator,
            repositories,
            worker_app,
            memory_cache,
            job_queue_cancellation_repository,
            job_status_index_repository,
        }
    }

    /// Asynchronously indexes JobProcessingStatus to RDB for monitoring and analytics
    ///
    /// # Design Principles
    /// - **Enqueue throughput first**: RDB indexing runs asynchronously without blocking
    /// - **Inconsistency tolerance**: Data may be delayed by seconds to tens of seconds
    /// - **Non-critical errors**: Logs warning on failure, but processing continues
    #[allow(clippy::too_many_arguments)]
    fn index_job_status_async(
        &self,
        job_id: JobId,
        status: JobProcessingStatus,
        worker_id: WorkerId,
        channel: String,
        priority: i32,
        enqueue_time: i64,
        is_streamable: bool,
        broadcast_results: bool,
    ) {
        if let Some(index_repo) = &self.job_status_index_repository {
            let repo = Arc::clone(index_repo);
            tokio::spawn(async move {
                if let Err(e) = repo
                    .index_status(
                        &job_id,
                        &status,
                        &worker_id,
                        &channel,
                        priority,
                        enqueue_time,
                        is_streamable,
                        broadcast_results,
                    )
                    .await
                {
                    tracing::warn!(
                        error = ?e,
                        job_id = job_id.value,
                        status = ?status,
                        "Failed to index job status to RDB (non-critical)"
                    );
                }
            });
        }
    }

    // find not queueing  jobs from argument 'jobs' in channels
    async fn find_restore_jobs_by(
        &self,
        job_ids: &HashSet<i64>,
        channels: &[String],
    ) -> Result<Vec<Job>> {
        let all_priority = [Priority::High, Priority::Medium, Priority::Low];
        // get current jids by iterate all channels and all_priority
        let mut current_jids = HashSet::new();
        for channel in channels {
            for priority in all_priority.iter() {
                let jids = self
                    .redis_job_repository()
                    .find_multi_from_queue(
                        // decode all jobs and check id in queue (heavy runner_settings when many jobs in queue)
                        // TODO change to resolve only ids for less memory
                        Some(channel.as_str()),
                        *priority,
                        Some(job_ids),
                    )
                    .await?
                    .iter()
                    .flat_map(|c| c.id.as_ref().map(|i| i.value))
                    .collect::<HashSet<i64>>();
                current_jids.extend(jids);
            }
        }
        // db record ids that not exists in job queue
        // restore jobs to redis queue (store jobs which not include current_jids)
        let diff_ids = job_ids.difference(&current_jids).collect::<Vec<&i64>>();
        let mut ret = vec![];
        // db only jobs which not is_run_after_job_data
        for jids in diff_ids.chunks(1000) {
            ret.extend(
                self.rdb_job_repository()
                    .find_list_in(jids)
                    .await?
                    .into_iter()
                    .filter(|j| {
                        j.data
                            .as_ref()
                            .is_some_and(|d| !self.is_run_after_job_data(d))
                    }),
            );
        }
        Ok(ret)
    }

    // use find_restore_jobs_by and enqueue them to redis queue
    async fn restore_jobs_by(&self, job_ids: &HashSet<i64>, channels: &[String]) -> Result<()> {
        let restores = self.find_restore_jobs_by(job_ids, channels).await?;
        // restore jobs to redis queue (store jobs which not include current jids)
        for job in restores.iter() {
            // unwrap
            if let Ok(Some(w)) = self
                .worker_app
                .find_data_by_opt(job.data.as_ref().and_then(|d| d.worker_id.as_ref()))
                .await
            {
                // not restore use rdb jobs (periodic worker or run after jobs)(should not exists in 'restores')
                if w.periodic_interval > 0 {
                    tracing::debug!("not restore use rdb job to redis: {:?}", &job);
                } else if w.response_type == ResponseType::Direct as i32 {
                    // not need to store to redis (should not reach here because direct response job shouldnot stored to rdb)
                    tracing::warn!(
                        "restore jobs from db: not restore direct response job: {:?}",
                        &job
                    );
                } else {
                    // need to store to redis
                    self.enqueue_job_to_redis_with_wait_if_needed(job, &w, StreamingType::None)
                        .await?;
                }
            } else {
                tracing::warn!("restore jobs from db: worker not found for job: {:?}", &job);
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job_with_worker(
        &self,
        metadata: Arc<HashMap<String, String>>,
        worker: &Worker,
        args: Vec<u8>,
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
    )> {
        if let Worker {
            id: Some(wid),
            data: Some(w),
        } = worker
        {
            // check if worker supports streaming mode
            let request_streaming = streaming_type != StreamingType::None;
            self.worker_app()
                .check_worker_streaming(wid, request_streaming, using.as_deref())
                .await?;

            let job_data = JobData {
                worker_id: Some(*wid),
                args,
                uniq_key,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time,
                retried: 0u32,
                priority,
                timeout,
                streaming_type: streaming_type as i32,
                using,
            };

            // TODO validate argument types (using Runner)
            // self.validate_worker_and_job_arg(w, job_data.arg.as_ref())?;
            // cannot wait for direct response
            if run_after_time > 0 && w.response_type == ResponseType::Direct as i32 {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "run_after_time must be 0 for worker response_type=Direct. job: {:?}",
                    &job_data
                ))
                .into());
            }
            let jid = reserved_job_id.unwrap_or(JobId {
                value: self.id_generator().generate_id()?,
            });
            // job fetched by rdb (periodic job) should have positive run_after_time
            let data = if (w.periodic_interval > 0 || w.queue_type == QueueType::DbOnly as i32)
                && job_data.run_after_time == 0
            {
                // make job_data.run_after_time datetime::now_millis() and create job by db
                JobData {
                    run_after_time: datetime::now_millis(), // set now millis
                    ..job_data
                }
            } else {
                job_data
            };
            if w.response_type == ResponseType::Direct as i32 {
                // use redis only for direct response (not restore)
                // TODO create backup for queue_type == Hybrid ?
                let job = Job {
                    id: Some(jid),
                    data: Some(data.to_owned()),
                    metadata: (*metadata).clone(),
                };
                self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                    .await
            } else if w.periodic_interval > 0 || self.is_run_after_job_data(&data) {
                let job = Job {
                    id: Some(jid),
                    data: Some(data.clone()),
                    metadata: (*metadata).clone(),
                };
                // enqueue rdb only
                if self.rdb_job_repository().create(&job).await? {
                    self.job_processing_status_repository()
                        .upsert_status(&jid, &JobProcessingStatus::Pending)
                        .await?;
                    // Index PENDING status to RDB asynchronously
                    self.index_job_status_async(
                        jid,
                        JobProcessingStatus::Pending,
                        *wid,
                        w.channel.clone().unwrap_or_default(),
                        priority,
                        data.enqueue_time,
                        request_streaming,
                        w.broadcast_results,
                    );
                    Ok((jid, None, None))
                } else {
                    Err(
                        JobWorkerError::RuntimeError(format!("cannot create record: {:?}", &job))
                            .into(),
                    )
                }
            } else {
                // normal instant job
                let job = Job {
                    id: Some(jid),
                    data: Some(data.clone()),
                    metadata: (*metadata).clone(),
                };
                if w.queue_type == QueueType::WithBackup as i32 {
                    // instant job (store rdb for failback, and enqueue to redis)
                    // TODO store async to rdb (not necessary to wait)
                    match self.rdb_job_repository().create(&job).await {
                        Ok(_id) => {
                            self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                                .await
                        }
                        Err(e) => Err(e),
                    }
                } else if w.queue_type == QueueType::DbOnly as i32 {
                    // use only rdb queue (not recommended for hybrid storage)
                    let created = self.rdb_job_repository().create(&job).await?;
                    if created {
                        self.job_processing_status_repository()
                            .upsert_status(&jid, &JobProcessingStatus::Pending)
                            .await?;
                        // Index PENDING status to RDB asynchronously
                        self.index_job_status_async(
                            jid,
                            JobProcessingStatus::Pending,
                            *wid,
                            w.channel.clone().unwrap_or_default(),
                            priority,
                            data.enqueue_time,
                            request_streaming,
                            w.broadcast_results,
                        );
                        Ok((job.id.unwrap(), None, None))
                    } else {
                        // storage error?
                        Err(JobWorkerError::RuntimeError(format!(
                            "cannot create record: {:?}",
                            &job
                        ))
                        .into())
                    }
                } else {
                    // instant job (enqueue to redis only)
                    self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                        .await
                }
            }
        } else {
            Err(
                JobWorkerError::WorkerNotFound(format!("illegal structure with empty: {worker:?}"))
                    .into(),
            )
        }
    }

    /// Internal: Job cancellation logic (respects job state)
    ///
    /// # Purpose
    /// This method handles user-initiated job cancellation requests.
    /// It transitions jobs to CANCELLING state when appropriate and triggers cleanup.
    ///
    /// # State-based Behavior
    /// - **PENDING**: Transition to CANCELLING → broadcast → cleanup → return true
    /// - **RUNNING**: Transition to CANCELLING → broadcast cancellation → cleanup → return true
    /// - **CANCELLING**: Already cancelling → cleanup → return true
    /// - **WAIT_RESULT**: Cannot cancel (preserve status) → return false
    /// - **Unknown/None**: Job not found or invalid state → return false
    ///
    /// # Returns
    /// - `Ok(true)`: Cancellation succeeded
    /// - `Ok(false)`: Cancellation failed (job in non-cancellable state)
    pub(crate) async fn cancel_job(&self, id: &JobId) -> Result<bool> {
        let current_status = self
            .job_processing_status_repository()
            .find_status(id)
            .await?;

        match current_status {
            Some(JobProcessingStatus::Running) => {
                // Running → Cancelling state change
                self.job_processing_status_repository()
                    .upsert_status(id, &JobProcessingStatus::Cancelling)
                    .await?;

                // Update RDB index status to CANCELLING (if enabled)
                if let Some(index_repo) = self.job_status_index_repository.as_ref()
                    && let Err(e) = index_repo
                        .update_status_by_job_id(id, &JobProcessingStatus::Cancelling)
                        .await
                {
                    tracing::warn!(
                        "Failed to update status to CANCELLING in RDB index for job {}: {:?}",
                        id.value,
                        e
                    );
                }
                // Note: RDB index deleted_at will be set by cleanup_job()

                // Active cancellation of running jobs (broadcast)
                self.broadcast_job_cancellation(id).await?;

                tracing::info!(
                    "Job {} marked as cancelling, broadcasting to workers",
                    id.value
                );

                // Cleanup job resources
                self.cleanup_job(id).await?;
                Ok(true)
            }
            Some(JobProcessingStatus::Pending) => {
                // Pending → Cancelling state change
                self.job_processing_status_repository()
                    .upsert_status(id, &JobProcessingStatus::Cancelling)
                    .await?;

                // Update RDB index status to CANCELLING (if enabled)
                if let Some(index_repo) = self.job_status_index_repository.as_ref()
                    && let Err(e) = index_repo
                        .update_status_by_job_id(id, &JobProcessingStatus::Cancelling)
                        .await
                {
                    tracing::warn!(
                        "Failed to update status to CANCELLING in RDB index for job {}: {:?}",
                        id.value,
                        e
                    );
                }
                // Note: RDB index deleted_at will be set by cleanup_job()

                // Broadcast cancellation (handles race conditions)
                self.broadcast_job_cancellation(id).await?;

                tracing::info!(
                    "Pending job {} marked as cancelling with broadcast",
                    id.value
                );

                // Cleanup job resources
                self.cleanup_job(id).await?;
                Ok(true)
            }
            Some(JobProcessingStatus::Cancelling) => {
                tracing::info!("Job {} is already being cancelled", id.value);
                // Already being cancelled, cleanup anyway
                self.cleanup_job(id).await?;
                Ok(true)
            }
            Some(JobProcessingStatus::WaitResult) => {
                // Cannot cancel: preserve status, no changes
                tracing::info!(
                    "Job {} is waiting for result processing, cancellation not possible",
                    id.value
                );
                Ok(false)
            }
            Some(JobProcessingStatus::Unknown) => {
                tracing::warn!(
                    "Job {} has unknown status, cancellation not possible",
                    id.value
                );
                Ok(false)
            }
            None => {
                // Status doesn't exist in Redis (already completed or doesn't exist)
                // Still cleanup RDB index and other resources (orphaned records)
                tracing::info!(
                    "Job {} status not found in Redis, cleaning up RDB index if exists",
                    id.value
                );
                self.cleanup_job(id).await?;
                Ok(false)
            }
        }
    }

    /// Internal: Unconditional job cleanup (always deletes resources)
    ///
    /// # Purpose
    /// This method performs unconditional cleanup of job resources.
    /// It should be called when the job is definitely finished (completed or cancelled).
    ///
    /// # Cleanup Operations
    /// 1. Delete job record from RDB
    /// 2. Delete job record from Redis
    /// 3. Delete job cache from memory
    /// 4. Delete job processing status
    ///
    /// # Error Handling
    /// - RDB deletion failure: Logged as warning, processing continues
    /// - Redis deletion failure: Logged as warning, processing continues
    /// - Cache deletion failure: Logged as warning, processing continues
    /// - Status deletion failure: Returns error (critical failure)
    ///
    /// # Returns
    /// - `Ok(())`: Cleanup succeeded (or non-critical failures)
    /// - `Err(e)`: Critical failure (status deletion failed)
    pub(crate) async fn cleanup_job(&self, id: &JobId) -> Result<()> {
        // 1. Mark as logically deleted in RDB index BEFORE deleting Redis status
        //    (deleted_at must be set while we still know this job is being cleaned up)
        if let Some(index_repo) = self.job_status_index_repository.as_ref()
            && let Err(e) = index_repo.mark_deleted_by_job_id(id).await
        {
            tracing::warn!(
                "Failed to mark job {} as deleted in RDB index: {:?}",
                id.value,
                e
            );
        }

        // 2. Delete job record from RDB
        let db_deletion_result = self.rdb_job_repository().delete(id).await;
        if let Err(e) = &db_deletion_result {
            tracing::warn!("Failed to delete job {} from RDB: {:?}", id.value, e);
        } else {
            let _ = self
                .memory_cache
                .delete_cache(&Arc::new(Self::find_cache_key(id)))
                .await;
        }

        // 3. Delete job record from Redis
        let redis_deletion_result = self.redis_job_repository().delete(id).await;
        if let Err(e) = &redis_deletion_result {
            tracing::warn!("Failed to delete job {} from Redis: {:?}", id.value, e);
        }

        // 4. Delete job processing status from Redis (critical operation)
        self.job_processing_status_repository()
            .delete_status(id)
            .await?;

        tracing::debug!("Job {} cleanup completed", id.value);
        Ok(())
    }

    /// Active cancellation of running jobs (distributed Worker notification)
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()> {
        // implementation: Use storage-specific broadcast
        tracing::debug!(
            "Job cancellation broadcast requested for job {}",
            job_id.value
        );

        self.job_queue_cancellation_repository
            .broadcast_job_cancellation(job_id)
            .await?;

        tracing::debug!(
            "Job cancellation broadcast completed for job {}",
            job_id.value
        );
        Ok(())
    }
}

#[async_trait]
impl JobApp for HybridJobAppImpl {
    async fn enqueue_job_with_temp_worker<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        worker_data: WorkerData,
        args: Vec<u8>,
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
    )> {
        let id = self
            .worker_app()
            .create_temp(worker_data.clone(), with_random_name)
            .await?;
        let worker = Worker {
            id: Some(id),
            data: Some(worker_data),
        };
        self.enqueue_job_with_worker(
            meta,
            &worker,
            args,
            uniq_key,
            run_after_time,
            priority,
            timeout,
            reserved_job_id,
            streaming_type,
            using,
        )
        .await
    }
    async fn enqueue_job<'a>(
        &'a self,
        meta: Arc<HashMap<String, String>>,
        worker_id: Option<&'a WorkerId>,
        worker_name: Option<&'a String>,
        args: Vec<u8>,
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
    )> {
        let worker_res = if let Some(id) = worker_id {
            self.worker_app().find(id).await?
        } else if let Some(name) = worker_name {
            self.worker_app().find_by_name(name).await?
        } else {
            return Err(JobWorkerError::WorkerNotFound(
                "worker_id or worker_name is required".to_string(),
            )
            .into());
        };
        if let Some(Worker {
            id: Some(wid),
            data: Some(w),
        }) = worker_res.as_ref()
        {
            // check if worker supports streaming mode
            let request_streaming = streaming_type != StreamingType::None;
            let _ = self
                .worker_app()
                .check_worker_streaming(wid, request_streaming, using.as_deref())
                .await?;

            let job_data = JobData {
                worker_id: Some(*wid),
                args,
                uniq_key,
                enqueue_time: datetime::now_millis(),
                grabbed_until_time: None,
                run_after_time,
                retried: 0u32,
                priority,
                timeout,
                streaming_type: streaming_type as i32,
                using,
            };

            // TODO validate argument types (using Runner)
            // self.validate_worker_and_job_arg(w, job_data.arg.as_ref())?;
            // cannot wait for direct response
            if run_after_time > 0 && w.response_type == ResponseType::Direct as i32 {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "run_after_time must be 0 for worker response_type=Direct. job: {:?}",
                    &job_data
                ))
                .into());
            }
            let jid = reserved_job_id.unwrap_or(JobId {
                value: self.id_generator().generate_id()?,
            });
            // job fetched by rdb (periodic job) should have positive run_after_time
            let data = if (w.periodic_interval > 0 || w.queue_type == QueueType::DbOnly as i32)
                && job_data.run_after_time == 0
            {
                // make job_data.run_after_time datetime::now_millis() and create job by db
                JobData {
                    run_after_time: datetime::now_millis(), // set now millis
                    ..job_data
                }
            } else {
                job_data
            };
            if w.response_type == ResponseType::Direct as i32 {
                // use redis only for direct response (not restore)
                // TODO create backup for queue_type == Hybrid ?
                let job = Job {
                    id: Some(jid),
                    data: Some(data.to_owned()),
                    metadata: (*meta).clone(),
                };
                self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                    .await
            } else if w.periodic_interval > 0 || self.is_run_after_job_data(&data) {
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                    metadata: (*meta).clone(),
                };
                // enqueue rdb only
                if self.rdb_job_repository().create(&job).await? {
                    self.job_processing_status_repository()
                        .upsert_status(&jid, &JobProcessingStatus::Pending)
                        .await?;
                    Ok((jid, None, None))
                } else {
                    Err(
                        JobWorkerError::RuntimeError(format!("cannot create record: {:?}", &job))
                            .into(),
                    )
                }
            } else {
                // normal instant job
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                    metadata: (*meta).clone(),
                };
                if w.queue_type == QueueType::WithBackup as i32 {
                    // instant job (store rdb for failback, and enqueue to redis)
                    // TODO store async to rdb (not necessary to wait)
                    match self.rdb_job_repository().create(&job).await {
                        Ok(_id) => {
                            self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                                .await
                        }
                        Err(e) => Err(e),
                    }
                } else if w.queue_type == QueueType::DbOnly as i32 {
                    // use only rdb queue (not recommended for hybrid storage)
                    let created = self.rdb_job_repository().create(&job).await?;
                    if created {
                        self.job_processing_status_repository()
                            .upsert_status(&jid, &JobProcessingStatus::Pending)
                            .await?;
                        Ok((job.id.unwrap(), None, None))
                    } else {
                        // storage error?
                        Err(JobWorkerError::RuntimeError(format!(
                            "cannot create record: {:?}",
                            &job
                        ))
                        .into())
                    }
                } else {
                    // instant job (enqueue to redis only)
                    self.enqueue_job_to_redis_with_wait_if_needed(&job, w, streaming_type)
                        .await
                }
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update (re-enqueue) job with id (redis: upsert, rdb: update)
    async fn update_job(&self, job: &Job) -> Result<()> {
        if let Job {
            id: Some(jid),
            data: Some(data),
            metadata: _,
        } = job
        {
            let is_run_after_job_data = self.is_run_after_job_data(data);
            if let Ok(Some(w)) = self
                .worker_app()
                .find_data_by_opt(data.worker_id.as_ref())
                .await
            {
                // TODO validate argument types (using Runner)
                // self.validate_worker_and_job_arg(&w, data.arg.as_ref())?;

                // use db queue (run after, periodic, queue_type=DB worker)
                let res_db = if is_run_after_job_data
                    || w.periodic_interval > 0
                    || w.queue_type == QueueType::DbOnly as i32
                    || w.queue_type == QueueType::WithBackup as i32
                {
                    tracing::debug!(
                        "re-enqueue job to rdb(upsert): {:?}, worker: {:?}",
                        &job,
                        &w.name
                    );
                    // XXX should compare grabbed_until_time and update if not changed or not (now not compared)
                    // TODO store metadata to rdb
                    self.rdb_job_repository().upsert(jid, data).await
                } else {
                    Ok(false)
                };
                // update job status of redis(memory)
                self.job_processing_status_repository()
                    .upsert_status(jid, &JobProcessingStatus::Pending)
                    .await?;
                let res_redis = if !is_run_after_job_data
                    && w.periodic_interval == 0
                    && (w.queue_type == QueueType::Normal as i32
                        || w.queue_type == QueueType::WithBackup as i32)
                {
                    tracing::debug!("re-enqueue job to redis: {:?}, worker: {:?}", &job, &w.name);
                    // enqueue to redis for instant job
                    let streaming_type =
                        StreamingType::try_from(data.streaming_type).unwrap_or(StreamingType::None);
                    self.enqueue_job_to_redis_with_wait_if_needed(job, &w, streaming_type)
                        .await
                        .map(|_| true)
                } else {
                    Ok(false)
                };
                let res = res_redis.or(res_db);
                match res {
                    Ok(_updated) => {
                        let _ = self
                            .memory_cache
                            .delete_cache(&Arc::new(Self::find_cache_key(jid)))
                            .await;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                tracing::error!(
                    "re-enqueue job: worker not found: {:?}, job: {:?}",
                    &data.worker_id,
                    &job,
                );
                Err(JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job)).into())
            }
        } else {
            tracing::error!("re-enqueue job: invalid job: {:?}", &job,);
            Err(JobWorkerError::NotFound(format!("illegal re-enqueue job: {:?}", &job)).into())
        }
    }

    /// Complete job and perform cleanup
    ///
    /// # Purpose
    /// This method is called after job execution completes (success/failure/cancelled).
    /// It publishes the result and performs cleanup by calling cleanup_job() directly.
    ///
    /// # Implementation
    /// - Publishes JobResult to Pub/Sub for listeners
    /// - Publishes streaming data if available
    /// - Calls `cleanup_job()` for unconditional resource cleanup
    async fn complete_job(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Result<bool> {
        tracing::debug!("complete_job: res_id={}", &id.value);
        if let Some(jid) = data.job_id.as_ref() {
            let res = match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    // Start stream publishing as background task (non-blocking).
                    // PR #126 ensures client subscribes before job execution via tokio::join!,
                    // so stream data won't be missed even if published after enqueue_result_direct.
                    // This enables realtime streaming instead of batch delivery.
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        let job_id_for_stream = *jid;
                        tracing::debug!(
                            "complete_job(direct): starting stream publish task: {}",
                            &jid.value
                        );
                        tokio::spawn(async move {
                            if let Err(e) = pubsub_repo
                                .publish_result_stream_data(job_id_for_stream, stream)
                                .await
                            {
                                tracing::error!(
                                    "complete_job(direct): stream publish error for job {}: {:?}",
                                    job_id_for_stream.value,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "complete_job(direct): stream data published: {}",
                                    job_id_for_stream.value
                                );
                            }
                        });
                    }

                    // send result immediately (don't wait for stream to complete)
                    let res = self
                        .redis_job_repository()
                        .enqueue_result_direct(id, data)
                        .await;
                    // publish for listening result client
                    // (XXX can receive response by listen_after, listen_by_worker for DIRECT response)
                    let _ = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, true) // XXX to_listen must be set worker.broadcast_results
                        .await
                        .inspect_err(|e| {
                            tracing::warn!("complete_job: pubsub publish error: {:?}", e)
                        });
                    res
                }
                Ok(ResponseType::NoResult) => {
                    // Publish result first so subscribe_result completes immediately.
                    // Client uses tokio::join! for subscribe_result and subscribe_result_stream,
                    // so both subscriptions start in parallel. Publishing result first allows
                    // the client to receive the stream response without waiting for stream completion.
                    let r = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, true) // XXX to_listen must be set worker.broadcast_results
                        .await;
                    tracing::debug!(
                        "complete_job(no_result): result published, starting stream: {}",
                        &jid.value
                    );
                    // Start stream publishing as background task (non-blocking)
                    // This enables realtime streaming instead of batch delivery
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        let job_id_for_stream = *jid;
                        tracing::debug!(
                            "complete_job(no_result): starting stream publish task: {}",
                            &jid.value
                        );
                        tokio::spawn(async move {
                            if let Err(e) = pubsub_repo
                                .publish_result_stream_data(job_id_for_stream, stream)
                                .await
                            {
                                tracing::error!(
                                    "complete_job(no_result): stream publish error for job {}: {:?}",
                                    job_id_for_stream.value,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "complete_job(no_result): stream data published: {}",
                                    job_id_for_stream.value
                                );
                            }
                        });
                    }
                    r
                }
                _ => {
                    tracing::warn!("complete_job: invalid response_type: {:?}", &data);
                    // abnormal response type, no publish
                    Ok(false)
                }
            };
            // Unconditional cleanup (no state checks needed)
            self.cleanup_job(jid).await?;

            res
        } else {
            // something wrong
            tracing::error!("no job found from result: {:?}", data);
            Ok(false)
        }
    }

    /// Delete job (Public API for job cancellation)
    ///
    /// # Purpose
    /// This is the public API for user-initiated job cancellation.
    /// It delegates to `cancel_job()` which handles state-aware cancellation logic.
    ///
    /// # Returns
    /// - `Ok(true)`: Cancellation succeeded
    /// - `Ok(false)`: Cancellation failed (job in non-cancellable state like WAIT_RESULT)
    async fn delete_job(&self, id: &JobId) -> Result<bool> {
        self.cancel_job(id).await
    }

    // cannot get job of queue type REDIS (redis is used for queue and job cache)
    async fn find_job(&self, id: &JobId) -> Result<Option<Job>>
    where
        Self: Send + 'static,
    {
        // Search order optimization: Redis individual key -> RDB

        // 1. Check Redis individual TTL key first (for running jobs)
        if let Ok(Some(job)) = self.redis_job_repository().find(id).await {
            tracing::debug!("Found job {} from Redis individual TTL key", id.value);
            return Ok(Some(job));
        }

        // 2. Check RDB and cache result if found
        if let Ok(Some(job)) = self.rdb_job_repository().find(id).await {
            tracing::debug!("Found job {} from RDB", id.value);

            // Cache in Redis with appropriate TTL for future lookups
            // For timeout=0 (unlimited), uses expire_job_result_seconds from config
            if let Some(job_data) = &job.data {
                let ttl = self.calculate_job_ttl(job_data.timeout);
                if let Err(e) = self
                    .redis_job_repository()
                    .create_with_expire(id, job_data, ttl)
                    .await
                {
                    tracing::warn!("Failed to cache job {} in Redis: {:?}", id.value, e);
                }
            }
            return Ok(Some(job));
        }

        // 3. Not found anywhere
        tracing::debug!("Job {} not found in any storage", id.value);
        Ok(None)
    }

    async fn find_job_list(&self, limit: Option<&i32>, offset: Option<&i64>) -> Result<Vec<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_list_cache_key(limit, offset.unwrap_or(&0i64)));
        self.memory_cache
            .with_cache(&k, || async {
                // from rdb with limit offset
                // XXX use redis as cache ?
                let v = self.rdb_job_repository().find_list(limit, offset).await?;
                Ok(v)
            })
            .await
    }
    async fn find_job_queue_list(
        &self,
        limit: Option<&i32>,
        channel: Option<&str>,
    ) -> Result<Vec<(Job, Option<JobProcessingStatus>)>>
    where
        Self: Send + 'static,
    {
        // let k = Arc::new(Self::find_queue_cache_key(channel, limit));
        // self.memory_cache
        //     .with_cache(&k, ttl, || async {
        // from redis queue with limit channel
        let priorities = [Priority::High, Priority::Medium, Priority::Low];
        let mut ret = vec![];
        for priority in priorities.iter() {
            let v = self
                .redis_job_repository()
                .find_multi_from_queue(channel, *priority, None)
                .await?;
            ret.extend(v);
            if ret.len() >= *limit.unwrap_or(&100) as usize {
                ret.truncate(*limit.unwrap_or(&100) as usize);
                break;
            }
        }
        let mut job_and_status = vec![];
        for j in ret.iter() {
            if let Some(jid) = j.id.as_ref()
                && let Some(j) = self.find_job(jid).await?
            {
                let s = self
                    .job_processing_status_repository()
                    .find_status(jid)
                    .await?;
                job_and_status.push((j, s));
            }
        }
        Ok(job_and_status)
        // })
        // .await
    }

    async fn find_list_with_processing_status(
        &self,
        status: JobProcessingStatus,
        limit: Option<&i32>,
    ) -> Result<Vec<(Job, JobProcessingStatus)>>
    where
        Self: Send + 'static,
    {
        // 1. Get all job statuses
        let all_statuses = self
            .job_processing_status_repository()
            .find_status_all()
            .await?;

        // 2. Filter by specified status and apply limit
        let target_job_ids: Vec<JobId> = all_statuses
            .into_iter()
            .filter(|(_, job_status)| *job_status == status)
            .map(|(id, _)| id)
            .take(*limit.unwrap_or(&100) as usize)
            .collect();

        tracing::debug!(
            "find_list_with_processing_status: found {} job IDs for status {:?}",
            target_job_ids.len(),
            status
        );
        // 3. Get corresponding job data
        let mut target_jobs = Vec::new();
        for job_id in target_job_ids {
            if let Some(job) = self.find_job(&job_id).await? {
                target_jobs.push((job, status));
            }
        }

        Ok(target_jobs)
    }

    async fn find_job_status(&self, id: &JobId) -> Result<Option<JobProcessingStatus>>
    where
        Self: Send + 'static,
    {
        self.job_processing_status_repository()
            .find_status(id)
            .await
    }

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobProcessingStatus)>>
    where
        Self: Send + 'static,
    {
        self.job_processing_status_repository()
            .find_status_all()
            .await
    }

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
        Self: Send + 'static,
    {
        if let Some(index_repo) = &self.job_status_index_repository {
            index_repo
                .find_by_condition(
                    status,
                    worker_id,
                    channel,
                    min_elapsed_time_ms,
                    limit,
                    offset,
                    descending,
                )
                .await
        } else {
            Err(anyhow::anyhow!(
                "Advanced job status search is disabled. \
                 Enable JOB_STATUS_RDB_INDEXING=true to use this feature."
            ))
        }
    }

    async fn cleanup_job_processing_status(
        &self,
        retention_hours_override: Option<u64>,
    ) -> Result<(u64, i64)> {
        let index_repo = self.job_status_index_repository.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "RDB JobProcessingStatus index repository not available. \
                     Ensure JOB_STATUS_RDB_INDEXING=true"
            )
        })?;

        // Execute cleanup with override (Infra layer calculates cutoff_time)
        index_repo
            .cleanup_deleted_records(retention_hours_override)
            .await
    }

    async fn count(&self) -> Result<i64>
    where
        Self: Send + 'static,
    {
        // TODO cache
        self.rdb_job_repository()
            .count_list_tx(self.rdb_job_repository().db_pool())
            .await
    }

    async fn pop_run_after_jobs_to_run(&self) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    /// restore jobs from rdb to redis
    /// (TODO offset, limit iteration)
    async fn restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<()> {
        let channels = self.worker_config().get_channels();
        if let Some(l) = limit {
            let job_ids = self
                .rdb_job_repository()
                .find_id_set_in_instant(include_grabbed, Some(l), None)
                .await?;
            if !job_ids.is_empty() {
                self.restore_jobs_by(&job_ids, channels.as_slice()).await?
            }
        } else {
            // fetch all with limit, offset (XXX heavy process: iterate and decode all job queue element multiple times (if larger than limit))
            let limit = 2000; // XXX depends on memory size and job size
            let mut offset = 0;
            loop {
                let job_id_set = self
                    .rdb_job_repository()
                    .find_id_set_in_instant(include_grabbed, Some(&limit), Some(&offset))
                    .await?;
                if job_id_set.is_empty() {
                    break;
                }
                self.restore_jobs_by(&job_id_set, channels.as_slice())
                    .await?;
                offset += limit as i64;
            }
        }
        Ok(())
    }
    // TODO return with streaming
    async fn find_restore_jobs_from_rdb(
        &self,
        include_grabbed: bool,
        limit: Option<&i32>,
    ) -> Result<Vec<Job>> {
        let channels = self.worker_config().get_channels();
        let job_id_set = self
            .rdb_job_repository()
            .find_id_set_in_instant(include_grabbed, limit, None)
            .await?;
        if job_id_set.is_empty() {
            return Ok(vec![]);
        } else {
            self.find_restore_jobs_by(&job_id_set, channels.as_slice())
                .await
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
impl UseRdbChanRepositoryModule for HybridJobAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories.rdb_chan_module
    }
}
impl UseRedisRepositoryModule for HybridJobAppImpl {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.repositories.redis_module
    }
}
impl UseJobProcessingStatusRepository for HybridJobAppImpl {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.repositories.job_processing_status_repository()
    }
}
impl UseIdGenerator for HybridJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for HybridJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobCacheKeys for HybridJobAppImpl {}

impl JobBuilder for HybridJobAppImpl {}

impl UseRedisJobResultPubSubRepository for HybridJobAppImpl {
    fn job_result_pubsub_repository(&self) -> &RedisJobResultPubSubRepositoryImpl {
        &self
            .repositories
            .redis_module
            .redis_job_result_pubsub_repository
    }
}

impl UseJobQueueConfig for HybridJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_config_module.job_queue_config
    }
}
impl UseWorkerConfig for HybridJobAppImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_config_module.worker_config
    }
}
impl RedisJobAppHelper for HybridJobAppImpl {
    fn after_enqueue_to_redis_hook(
        &self,
        job_id: JobId,
        job: &Job,
        worker: &WorkerData,
        streaming_type: StreamingType,
    ) {
        // Index PENDING status to RDB asynchronously
        if let Some(worker_id) = job.data.as_ref().and_then(|d| d.worker_id)
            && let Some(job_data) = &job.data
        {
            let request_streaming = streaming_type == StreamingType::Response;
            self.index_job_status_async(
                job_id,
                JobProcessingStatus::Pending,
                worker_id,
                worker.channel.clone().unwrap_or_default(),
                job_data.priority,
                job_data.enqueue_time,
                request_streaming,
                worker.broadcast_results,
            );
        }
    }
}

//TODO
// create test
#[cfg(any(test, feature = "test-utils"))]
pub mod tests {
    use super::*;
    use crate::app::runner::RunnerApp;
    use crate::app::runner::hybrid::HybridRunnerAppImpl;
    use crate::app::worker::hybrid::HybridWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use infra::infra::IdGeneratorWrapper;
    use infra::infra::job::queue::redis::UseRedisJobQueueRepository;
    use infra::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
    use infra::infra::module::HybridRepositoryModule;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::module::redis::test::setup_test_redis_module;
    #[allow(unused_imports)]
    use jobworkerp_base::codec::UseProstCodec;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::RunnerId;
    use std::sync::Arc;
    use std::time::Duration;

    const TEST_RUNNER_ID: RunnerId = RunnerId { value: 100000000 };

    pub async fn create_test_app(
        use_mock_id: bool,
    ) -> Result<(HybridJobAppImpl, RedisJobResultPubSubRepositoryImpl)> {
        let rdb_module = setup_test_rdb_module(false).await;
        let redis_module = setup_test_redis_module().await;
        let repositories = Arc::new(HybridRepositoryModule {
            redis_module: redis_module.clone(),
            rdb_chan_module: rdb_module,
        });
        // mock id generator (generate 1 until called set method)
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };
        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 1000000,
            ttl: Some(Duration::from_millis(100)),
        };
        let job_memory_cache = memory_utils::cache::moka::MokaCacheImpl::new(&moka_config);
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Scalable,
            restore_at_startup: Some(false),
        });
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
        });
        let worker_config = Arc::new(WorkerConfig {
            default_concurrency: 4,
            channels: vec!["test".to_string()],
            channel_concurrencies: vec![2],
        });
        let descriptor_cache =
            Arc::new(memory_utils::cache::moka::MokaCacheImpl::new(&moka_config));
        let runner_app = Arc::new(HybridRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache.clone(),
            id_generator.clone(),
        ));
        runner_app.load_runner().await?;
        let _ = runner_app
            .create_test_runner(&TEST_RUNNER_ID, "Test")
            .await
            .unwrap();
        let worker_app = HybridWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache,
            runner_app,
        );
        let subscrber = RedisJobResultPubSubRepositoryImpl::new(
            redis_module.redis_client,
            job_queue_config.clone(),
        );
        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory
            .load_plugins_from("./target/debug,../target/debug,./target/release,../target/release")
            .await;
        let config_module = Arc::new(AppConfigModule {
            storage_config,
            worker_config,
            job_queue_config,
            runner_factory: Arc::new(runner_factory),
        });
        let job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository> =
            Arc::new(repositories.redis_job_queue_repository().clone());

        // JobProcessingStatus RDB indexing for test fixtures (controlled by JOB_STATUS_RDB_INDEXING env var)
        let job_status_index_repository = repositories
            .rdb_chan_module
            .rdb_job_processing_status_index_repository
            .clone();

        Ok((
            HybridJobAppImpl::new(
                config_module,
                id_generator,
                repositories,
                Arc::new(worker_app),
                job_memory_cache,
                job_queue_cancellation_repository,
                job_status_index_repository,
            ),
            subscrber,
        ))
    }

    #[test]
    fn test_create_direct_job_complete() -> Result<()> {
        // enqueue, find, complete, find, delete, find
        // tracing_subscriber::fmt()
        //     .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        //     .with_max_level(tracing::Level::TRACE)
        //     .compact()
        //     .init();

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
                runner_settings,
                channel: None,
                response_type: ResponseType::Direct as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32,
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;
            let metadata = Arc::new(HashMap::new());
            // move
            let worker_id1 = worker_id;
            let jarg1 = jarg.clone();
            let app1 = app.clone();
            // need waiting for direct response
            let jh = tokio::spawn(async move {
                let res = app1
                    .enqueue_job(
                        metadata.clone(),
                        Some(&worker_id1),
                        None,
                        jarg1.clone(),
                        None,
                        0,
                        0,
                        0,
                        None,
                        StreamingType::None,
                        None, // using
                    )
                    .await;
                let (jid, job_res, _) = res.unwrap();
                assert!(jid.value > 0);
                assert!(job_res.is_some());
                // cannot find job in direct response type
                let job = app1
                    .find_job(
                        &job_res
                            .clone()
                            .and_then(|j| j.data.and_then(|d| d.job_id))
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(job.id, Some(jid));
                assert_eq!(job.data.as_ref().unwrap().worker_id, Some(worker_id1));
                assert_eq!(job.data.as_ref().unwrap().args, jarg1);
                assert_eq!(job.data.as_ref().unwrap().uniq_key, None);
                assert!(job.data.as_ref().unwrap().enqueue_time > 0);
                assert_eq!(job.data.as_ref().unwrap().grabbed_until_time, None);
                assert_eq!(job.data.as_ref().unwrap().run_after_time, 0);
                assert_eq!(job.data.as_ref().unwrap().retried, 0);
                assert_eq!(job.data.as_ref().unwrap().priority, 0);
                assert_eq!(job.data.as_ref().unwrap().timeout, 0);
                assert_eq!(job.data.as_ref().unwrap().streaming_type, 0);
                assert_eq!(
                    app1.job_processing_status_repository()
                        .find_status(&jid)
                        .await
                        .unwrap(),
                    Some(JobProcessingStatus::Pending)
                );

                (jid, job_res)
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            let rid = JobResultId {
                value: app.id_generator().generate_id().unwrap(),
            };
            let result = JobResult {
                id: Some(rid),
                data: Some(JobResultData {
                    job_id: Some(JobId { value: 1 }), // generated by mock generator
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { "test".as_bytes().to_vec() },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    streaming_type: 0,
                    enqueue_time: datetime::now_millis(),
                    run_after_time: 0,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: false,
                    using: None,
                }),
                ..Default::default()
            };
            assert!(
                app.complete_job(&rid, result.data.as_ref().unwrap(), None)
                    .await?
            );
            let (jid, job_res) = jh.await?;
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(&job_res.unwrap(), &result);
            let job0 = app.find_job(&jid).await?;
            assert!(job0.is_none());
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&jid)
                    .await
                    .unwrap(),
                None
            );
            Ok(())
        })
    }

    #[test]
    fn test_create_job_not_streaming_error() -> Result<()> {
        // enqueue, find, complete, find, delete, find
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _subscriber) = create_test_app(true).await?;
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
                runner_settings,
                channel: None,
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32, // store to rdb for failback (can find job from rdb but not from redis)
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: true,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;
            let metadata = Arc::new(HashMap::new());

            // wait for direct response
            let res = app
                .enqueue_job(
                    metadata,
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    0,
                    0,
                    None,
                    StreamingType::Response, // STREAMING NOT SUPPORTED by runner -> error
                    None,                    // using
                )
                .await;
            assert!(res.is_err());
            Ok(())
        })
    }
    #[test]
    fn test_create_listen_after_job_complete() -> Result<()> {
        use infra::infra::job_result::pubsub::JobResultSubscriber;

        // enqueue, find, complete, find, delete, find
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, subscriber) = create_test_app(true).await?;
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
                runner_settings,
                channel: None,
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::Normal as i32, // store to rdb for failback (can find job from rdb but not from redis)
                store_failure: false,
                store_success: false,
                use_static: false,
                broadcast_results: true, // need for listen after
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;
            let metadata = Arc::new(HashMap::new());

            // wait for direct response
            let job_id = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    0,
                    0,
                    None,
                    StreamingType::None,
                    None, // using
                )
                .await?
                .0;
            let job = Job {
                id: Some(job_id),
                data: Some(JobData {
                    worker_id: Some(worker_id),
                    args: jarg.clone(),
                    uniq_key: None,
                    enqueue_time: datetime::now_millis(),
                    grabbed_until_time: None,
                    run_after_time: 0,
                    retried: 0,
                    priority: 0,
                    timeout: 0,
                    streaming_type: 1,
                    using: None,
                }),
                ..Default::default()
            };
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            let result = JobResult {
                id: Some(JobResultId { value: 15555 }),
                data: Some(JobResultData {
                    job_id: Some(job_id),
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { "test".as_bytes().to_vec() },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    streaming_type: 1,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::NoResult as i32,
                    store_success: false,
                    store_failure: false,
                    using: None,
                }),
                metadata: (*metadata).clone(),
            };
            let jid = job_id;
            let res = result.clone();
            let jh = tokio::task::spawn(async move {
                // assert_eq!(job_result.data.as_ref().unwrap().retried, 0);
                let job_result = subscriber.subscribe_result(&jid, None).await.unwrap();
                assert!(job_result.id.unwrap().value > 0);
                assert_eq!(&job_result.data, &res.data);
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                // send mock result as completed job result
                app.complete_job(
                    result.id.as_ref().unwrap(),
                    result.data.as_ref().unwrap(),
                    None
                )
                .await?
            );
            jh.await?;

            let job0 = app.find_job(&job_id).await.unwrap();
            assert!(job0.is_none());
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                None
            );
            Ok(())
        })
    }
    #[test]
    fn test_create_normal_job_complete() -> Result<()> {
        // enqueue, find, complete, find, delete, find
        let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
            &proto::TestRunnerSettings {
                name: "ls".to_string(),
            },
        )?;
        let wd = proto::jobworkerp::data::WorkerData {
            name: "testworker".to_string(),
            description: "desc1".to_string(),
            runner_id: Some(TEST_RUNNER_ID),
            runner_settings,
            channel: None,
            response_type: ResponseType::NoResult as i32,
            periodic_interval: 0,
            retry_policy: None,
            queue_type: QueueType::WithBackup as i32,
            store_success: true,
            store_failure: false,
            use_static: false,
            broadcast_results: false,
        };
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let worker_id = app.worker_app().create(&wd).await?;
            let jarg =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;
            let metadata = Arc::new(HashMap::new());

            // wait for direct response
            let (job_id, res, _) = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    0,
                    0,
                    None,
                    StreamingType::None,
                    None, // using
                )
                .await?;
            assert!(job_id.value > 0);
            assert!(res.is_none());
            let job = app.find_job(&job_id).await?.unwrap();
            assert_eq!(job.id.as_ref().unwrap(), &job_id);
            assert_eq!(
                job.data.as_ref().unwrap().worker_id.as_ref(),
                Some(&worker_id)
            );
            assert_eq!(job.data.as_ref().unwrap().retried, 0);
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            let result = JobResult {
                id: Some(JobResultId {
                    value: app.id_generator().generate_id().unwrap(),
                }),
                data: Some(JobResultData {
                    job_id: Some(job_id),
                    worker_id: Some(worker_id),
                    worker_name: wd.name.clone(),
                    args: jarg,
                    uniq_key: None,
                    status: proto::jobworkerp::data::ResultStatus::Success as i32,
                    output: Some(proto::jobworkerp::data::ResultOutput {
                        items: { "test".as_bytes().to_vec() },
                    }),
                    retried: 0,
                    max_retry: 0,
                    priority: 0,
                    timeout: 0,
                    streaming_type: 0,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: false,
                    using: None,
                }),
                metadata: (*metadata).clone(),
            };
            assert!(
                !app.complete_job(
                    result.id.as_ref().unwrap(),
                    result.data.as_ref().unwrap(),
                    None
                )
                .await?
            );
            // not fetched job (because of not use job_dispatcher)
            assert!(app.find_job(&job_id).await?.is_none());
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                None
            );
            Ok(())
        })
    }

    #[test]
    fn test_restore_jobs_from_rdb() -> Result<()> {
        let priority = Priority::Medium;
        let channel: Option<&String> = None;

        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(false).await?;
            // create command worker with hybrid queue
            let runner_settings = jobworkerp_base::codec::ProstMessageCodec::serialize_message(
                &proto::TestRunnerSettings {
                    name: "ls".to_string(),
                },
            )?;
            let wd = proto::jobworkerp::data::WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(TEST_RUNNER_ID),
                runner_settings,
                channel: channel.cloned(),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::WithBackup as i32,
                store_success: true,
                store_failure: false,
                use_static: false,
                broadcast_results: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;

            // enqueue job
            let jarg =
                jobworkerp_base::codec::ProstMessageCodec::serialize_message(&proto::TestArgs {
                    args: vec!["/".to_string()],
                })?;
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                0
            );
            let metadata = Arc::new(HashMap::new());

            let (job_id, res, _) = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
                    None,
                    StreamingType::None,
                    None, // using
                )
                .await?;
            assert!(job_id.value > 0);
            assert!(res.is_none());
            // job2
            let (job_id2, res2, _) = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jarg.clone(),
                    None,
                    0,
                    priority as i32,
                    0,
                    None,
                    StreamingType::None,
                    None, // using
                )
                .await?;
            assert!(job_id2.value > 0);
            assert!(res2.is_none());

            // get job from redis
            let job = app.find_job(&job_id).await?.unwrap();
            assert_eq!(job.id.as_ref().unwrap(), &job_id);
            assert_eq!(
                job.data.as_ref().unwrap().worker_id.as_ref(),
                Some(&worker_id)
            );

            // check job status
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );
            assert_eq!(
                app.job_processing_status_repository()
                    .find_status(&job_id2)
                    .await
                    .unwrap(),
                Some(JobProcessingStatus::Pending)
            );

            // no jobs to restore  (exists in both redis and rdb)
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            app.restore_jobs_from_rdb(false, None).await?;
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );

            // find job for delete (grabbed_until_time is SOme(0) in queue)
            let job_d = app
                .redis_job_repository()
                .find_from_queue(channel, priority, &job_id)
                .await?
                .unwrap();
            // lost only from redis (delete)
            assert!(
                app.redis_job_repository()
                    .delete_from_queue(channel, priority, &job_d)
                    .await?
                    > 0
            );
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                1
            );

            // restore 1 lost jobs
            app.restore_jobs_from_rdb(false, None).await?;
            assert!(
                app.redis_job_repository()
                    .find_from_queue(channel, priority, &job_id)
                    .await?
                    .is_some()
            );
            assert!(
                app.redis_job_repository()
                    .find_from_queue(channel, priority, &job_id2)
                    .await?
                    .is_some()
            );
            assert_eq!(
                app.redis_job_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            Ok(())
        })
    }
}
