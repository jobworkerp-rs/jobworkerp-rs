use super::super::JobBuilder;
use super::super::worker::{UseWorkerApp, WorkerApp};
use super::{JobApp, JobCacheKeys};
use crate::app::{UseWorkerConfig, WorkerConfig};
use crate::module::AppConfigModule;
use anyhow::Result;
use async_trait::async_trait;
use command_utils::util::datetime;
use futures::stream::BoxStream;
use infra::infra::job::queue::JobQueueCancellationRepository;
use infra::infra::job::queue::chan::{
    ChanJobQueueRepository, ChanJobQueueRepositoryImpl, UseChanJobQueueRepository,
};
use infra::infra::job::rdb::{RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository;
use infra::infra::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use infra::infra::job_result::pubsub::chan::{
    ChanJobResultPubSubRepositoryImpl, UseChanJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::{JobResultPublisher, JobResultSubscriber};
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use memory_utils::cache::stretto::{self as memory, MemoryCacheConfig, UseMemoryCache};
use memory_utils::lock::RwLockWithKey;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId, QueueType,
    ResponseType, ResultOutputItem, ResultStatus, StreamingType, Worker, WorkerData, WorkerId,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use stretto::AsyncCache;

#[derive(Clone)]
pub struct RdbChanJobAppImpl {
    app_config_module: Arc<AppConfigModule>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<RdbChanRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    // Previous: memory_cache: MokaCacheImpl<Arc<String>, Job>,
    // UseMemoryCache implementation (Stretto-based)
    memory_cache: AsyncCache<Arc<String>, Job>,
    key_lock: Arc<RwLockWithKey<Arc<String>>>,
    job_cache_ttl: Duration,
    job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
    // RDBインデックス専用Repository（独立、Option型でデフォルト無効）
    job_status_index_repository: Option<Arc<RdbJobProcessingStatusIndexRepository>>,
}

impl std::fmt::Debug for RdbChanJobAppImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RdbChanJobAppImpl")
            .field("app_config_module", &self.app_config_module)
            .field("id_generator", &self.id_generator)
            .field("repositories", &self.repositories)
            .field("worker_app", &"Arc<dyn WorkerApp>")
            .field("memory_cache", &"AsyncCache<Arc<String>, Job>")
            .field("key_lock", &"Arc<RwLockWithKey<Arc<String>>>")
            .field("job_cache_ttl", &self.job_cache_ttl)
            .field(
                "job_queue_cancellation_repository",
                &"Arc<dyn JobQueueCancellationRepository>",
            )
            .field(
                "job_status_index_repository",
                &self
                    .job_status_index_repository
                    .as_ref()
                    .map(|_| "Some(Arc<RdbJobProcessingStatusIndexRepository>)")
                    .unwrap_or("None"),
            )
            .finish()
    }
}

impl RdbChanJobAppImpl {
    // Configuration constants for UseMemoryCache
    const MEMORY_CACHE_CONFIG: MemoryCacheConfig = MemoryCacheConfig {
        num_counters: 12960, // Number of cacheable entries
        max_cost: 12960,     // Maximum cost (number of items)
        use_metrics: false,  // Metrics disabled for performance optimization
    };
    const JOB_DEFAULT_TTL: Duration = Duration::from_secs(3600); // Default 1 hour TTL

    pub fn new(
        app_config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<RdbChanRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository>,
        job_status_index_repository: Option<Arc<RdbJobProcessingStatusIndexRepository>>,
    ) -> Self {
        Self {
            app_config_module,
            id_generator,
            repositories,
            worker_app,

            // Initialize UseMemoryCache
            memory_cache: memory::new_memory_cache(&Self::MEMORY_CACHE_CONFIG),
            key_lock: Arc::new(RwLockWithKey::new(
                Self::MEMORY_CACHE_CONFIG.max_cost as usize,
            )),
            job_cache_ttl: Self::JOB_DEFAULT_TTL,
            job_queue_cancellation_repository,
            job_status_index_repository,
        }
    }

    /// Asynchronously index job status to RDB (non-blocking, fire-and-forget)
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

    /// Active cancellation of running jobs (Memory environment notification)
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()> {
        tracing::info!(
            "Job cancellation broadcast requested for job {} (Memory environment)",
            job_id.value
        );

        self.job_queue_cancellation_repository
            .broadcast_job_cancellation(job_id)
            .await?;

        tracing::info!(
            "Job cancellation broadcast completed for job {}",
            job_id.value
        );
        Ok(())
    }

    /// Internal: Job cancellation logic (respects job state)
    ///
    /// # Purpose
    /// This method handles user-initiated job cancellation requests.
    /// It transitions jobs to CANCELLING state when appropriate and triggers cleanup.
    ///
    /// # State-based Behavior
    /// - **PENDING**: Transition to CANCELLING → cleanup → return true
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
                // Pending → Cancelling state change (Worker will detect when fetching)
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

                tracing::info!("Pending job {} marked as cancelling", id.value);

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
    /// 2. Delete job cache from memory
    /// 3. Delete job processing status
    ///
    /// # Error Handling
    /// - RDB deletion failure: Logged as warning, processing continues
    /// - Cache deletion failure: Logged as warning, processing continues
    /// - Status deletion failure: Returns error (critical failure)
    ///
    /// # Returns
    /// - `Ok(())`: Cleanup succeeded (or non-critical failures)
    /// - `Err(e)`: Critical failure (status deletion failed)
    pub(crate) async fn cleanup_job(&self, id: &JobId) -> Result<()> {
        // 1. Mark as logically deleted in RDB index BEFORE deleting memory status
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
        }

        // 3. Delete job cache from memory
        let cache_key = Arc::new(Self::find_cache_key(id));
        let _ = self.delete_cache(&cache_key).await.inspect_err(|e| {
            tracing::warn!("Failed to delete job cache for {}: {:?}", id.value, e)
        });

        // 4. Delete job processing status from memory/Redis (critical operation)
        self.job_processing_status_repository()
            .delete_status(id)
            .await?;

        tracing::debug!("Job {} cleanup completed", id.value);
        Ok(())
    }

    // find not queueing  jobs from argument 'jobs' in channels
    // TODO
    async fn find_restore_jobs_by(
        &self,
        _job_ids: &HashSet<i64>,
        _channels: &[String],
    ) -> Result<Vec<Job>> {
        Ok(vec![])
    }

    // use find_restore_jobs_by and enqueue them to redis queue
    // TODO
    async fn restore_jobs_by(&self, _job_ids: &HashSet<i64>, _channels: &[String]) -> Result<()> {
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn enqueue_job_with_worker(
        &self,
        metadata: Arc<HashMap<String, String>>,
        worker: Worker,
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
        } = &worker
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
            // TODO validate argument types
            // self.validate_worker_and_job_args(w, job_data.args.as_ref())?;
            // cannot wait for direct response
            if run_after_time > 0 && w.response_type == ResponseType::Direct as i32 {
                return Err(JobWorkerError::InvalidParameter(format!(
                    "run_after_time must be 0 for worker response_type=Direct: {:?}",
                    &job_data
                ))
                .into());
            }
            let jid = reserved_job_id.unwrap_or(JobId {
                value: self.id_generator().generate_id()?,
            });
            // job fetched by rdb (periodic job) should be positive run_after_time
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
                // use chan only for direct response (not restore)
                // TODO create backup for queue_type == RdbChan ?
                let job = Job {
                    id: Some(jid),
                    data: Some(data.to_owned()),
                    metadata: (*metadata).clone(),
                };

                let result = self.enqueue_job_sync(&job, w).await?;

                // Async RDB indexing for Direct response jobs
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

                Ok(result)
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

                    // Async RDB indexing for periodic/scheduled jobs
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
                // normal job
                let job = Job {
                    id: Some(jid),
                    data: Some(data.clone()),
                    metadata: (*metadata).clone(),
                };
                if w.queue_type == QueueType::WithBackup as i32 {
                    // instant job (store rdb for backup, and enqueue to chan)
                    match self.rdb_job_repository().create(&job).await {
                        Ok(_id) => {
                            let result = self.enqueue_job_sync(&job, w).await?;

                            // Async RDB indexing for WithBackup jobs
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

                            Ok(result)
                        }
                        Err(e) => Err(e),
                    }
                } else if w.queue_type == QueueType::DbOnly as i32 {
                    // use only rdb queue
                    let created = self.rdb_job_repository().create(&job).await?;
                    if created {
                        // Async RDB indexing for DbOnly jobs
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
                    // instant job (enqueue to chan only)
                    let result = self.enqueue_job_sync(&job, w).await?;

                    // Async RDB indexing for Normal (chan-only) jobs
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

                    Ok(result)
                }
            }
        } else {
            Err(JobWorkerError::WorkerNotFound(format!(
                "illegal worker structure with empty: {:?}",
                &worker
            ))
            .into())
        }
    }
}

#[async_trait]
impl JobApp for RdbChanJobAppImpl {
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
        let wid = self
            .worker_app()
            .create_temp(worker_data.clone(), with_random_name)
            .await?;
        let worker = Worker {
            id: Some(wid),
            data: Some(worker_data),
        };
        self.enqueue_job_with_worker(
            meta,
            worker,
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
        metadata: Arc<HashMap<String, String>>,
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
        if let Some(w) = worker_res {
            self.enqueue_job_with_worker(
                metadata,
                w,
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
        } else {
            Err(JobWorkerError::WorkerNotFound(format!("name: {:?}", &worker_name)).into())
        }
    }

    // update (re-enqueue) job with id (chan: retry, rdb: update)
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
                // TODO validate argument types
                // self.validate_worker_and_job_args(&w, data.args.as_ref())?;

                // use db queue (run after, periodic, queue_type=DB worker)
                let res_db = if is_run_after_job_data
                    || w.periodic_interval > 0
                    || w.queue_type == QueueType::DbOnly as i32
                    || w.queue_type == QueueType::WithBackup as i32
                {
                    // XXX should compare grabbed_until_time and update if not changed or not (now not compared)
                    // TODO store metadata
                    self.rdb_job_repository().upsert(jid, data).await
                } else {
                    Ok(false)
                };
                // update job status of redis(memory)
                self.job_processing_status_repository()
                    .upsert_status(jid, &JobProcessingStatus::Pending)
                    .await?;
                let res_chan = if !is_run_after_job_data
                    && w.periodic_interval == 0
                    && (w.queue_type == QueueType::Normal as i32
                        || w.queue_type == QueueType::WithBackup as i32)
                {
                    // enqueue to chan for instant job
                    self.enqueue_job_sync(job, &w).await.map(|_| true)
                } else {
                    Ok(false)
                };
                let res = res_chan.or(res_db);
                match res {
                    Ok(_updated) => {
                        let cache_key = Arc::new(Self::find_cache_key(jid));
                        let _ = self.delete_cache(&cache_key).await.inspect_err(|e| {
                            tracing::warn!("Failed to delete job cache for {}: {:?}", jid.value, e)
                        });
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            } else {
                Err(JobWorkerError::WorkerNotFound(format!("in re-enqueue job: {:?}", &job)).into())
            }
        } else {
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
            // For streaming jobs, don't delete status immediately as the process may still be running
            let res = match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    let res = self
                        .job_result_pubsub_repository()
                        // Direct: always publish because the client blocks waiting for the result
                        .publish_result(id, data, true)
                        .await;
                    // Start stream publishing as background task (non-blocking)
                    // This enables realtime streaming instead of batch delivery
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
                    } else {
                        super::spawn_end_marker_if_needed(
                            data,
                            jid,
                            self.job_result_pubsub_repository(),
                        );
                    }
                    tracing::debug!(
                        "Deleted memory cache for Direct Response job: {}",
                        jid.value
                    );
                    res
                }
                Ok(_res) => {
                    // Publish result first so subscribe_result completes immediately.
                    // Client uses tokio::join! for subscribe_result and subscribe_result_stream,
                    // so both subscriptions start in parallel. Publishing result first allows
                    // the client to receive the stream response without waiting for stream completion.
                    let r = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, data.broadcast_results)
                        .await;
                    tracing::debug!(
                        "complete_job(other): result published, starting stream: {}",
                        &jid.value
                    );
                    // Start stream publishing as background task (non-blocking)
                    // This enables realtime streaming instead of batch delivery
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        let job_id_for_stream = *jid;
                        tracing::debug!(
                            "complete_job(other): starting stream publish task: {}",
                            &jid.value
                        );
                        tokio::spawn(async move {
                            if let Err(e) = pubsub_repo
                                .publish_result_stream_data(job_id_for_stream, stream)
                                .await
                            {
                                tracing::error!(
                                    "complete_job(other): stream publish error for job {}: {:?}",
                                    job_id_for_stream.value,
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    "complete_job(other): stream data published: {}",
                                    job_id_for_stream.value
                                );
                            }
                        });
                    } else {
                        super::spawn_end_marker_if_needed(
                            data,
                            jid,
                            self.job_result_pubsub_repository(),
                        );
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
        let k = Arc::new(Self::find_cache_key(id));

        match self.find_cache(&k).await {
            Some(job) => {
                tracing::debug!("Found job {} in cache", id.value);
                return Ok(Some(job));
            }
            None => {
                tracing::debug!("Job {} not found in cache, fetching from RDB", id.value);
                // Cache job with appropriate TTL based on job characteristics
                match self.rdb_job_repository().find(id).await? {
                    Some(job) => {
                        // Cache job from RDB with individual TTL to prevent premature eviction
                        // For timeout=0 (unlimited), uses expire_job_result_seconds from config
                        if let Some(data) = &job.data {
                            let job_ttl = self.calculate_job_ttl(data.timeout);
                            tracing::debug!(
                                "Found job {} from RDB, caching with TTL {:?}",
                                id.value,
                                job_ttl
                            );
                            // Store in cache separately with TTL
                            let _ = self
                                .set_cache(k.clone(), job.clone(), job_ttl.as_ref())
                                .await;
                        }
                        Ok(Some(job))
                    }
                    None => {
                        tracing::debug!("Job {} not found in RDB", id.value);
                        Ok(None)
                    }
                }
            }
        }
    }

    async fn find_job_list(&self, limit: Option<&i32>, offset: Option<&i64>) -> Result<Vec<Job>>
    where
        Self: Send + 'static,
    {
        // TODO cache?
        // let k = Arc::new(Self::find_list_cache_key(limit, offset.unwrap_or(&0i64)));
        // self.memory_cache
        //     .with_cache(&k, ttl, || async {
        // from rdb with limit offset
        let v = self.rdb_job_repository().find_list(limit, offset).await?;
        Ok(v)
        // })
        // .await
    }
    async fn find_job_queue_list(
        &self,
        limit: Option<&i32>,
        channel: Option<&str>,
    ) -> Result<Vec<(Job, Option<JobProcessingStatus>)>>
    where
        Self: Send + 'static,
    {
        let v = self
            .chan_job_queue_repository()
            .find_multi_from_queue(channel, None)
            .await?;
        let mut res = vec![];
        for j in v {
            if let Some(jid) = j.id.as_ref()
                && let Ok(status) = self
                    .job_processing_status_repository()
                    .find_status(jid)
                    .await
            {
                res.push((j, status));
                if limit.is_some() && res.len() >= *limit.unwrap() as usize {
                    break;
                }
            }
        }
        Ok(res)
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
impl UseRdbChanRepositoryModule for RdbChanJobAppImpl {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.repositories
    }
}
impl UseIdGenerator for RdbChanJobAppImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseWorkerApp for RdbChanJobAppImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.worker_app
    }
}
impl JobCacheKeys for RdbChanJobAppImpl {}

impl JobBuilder for RdbChanJobAppImpl {}

impl UseMemoryCache<Arc<String>, Job> for RdbChanJobAppImpl {
    fn cache(&self) -> &AsyncCache<Arc<String>, Job> {
        &self.memory_cache
    }

    fn default_ttl(&self) -> Option<&Duration> {
        Some(&self.job_cache_ttl)
    }

    fn key_lock(&self) -> &RwLockWithKey<Arc<String>> {
        &self.key_lock
    }
}

impl UseJobQueueConfig for RdbChanJobAppImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_config_module.job_queue_config
    }
}
impl UseWorkerConfig for RdbChanJobAppImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_config_module.worker_config
    }
}
impl UseChanJobQueueRepository for RdbChanJobAppImpl {
    fn chan_job_queue_repository(&self) -> &ChanJobQueueRepositoryImpl {
        &self.repositories.chan_job_queue_repository
    }
}
impl RdbChanJobAppHelper for RdbChanJobAppImpl {}
impl jobworkerp_base::codec::UseProstCodec for RdbChanJobAppImpl {}
impl UseJobqueueAndCodec for RdbChanJobAppImpl {}
impl UseJobProcessingStatusRepository for RdbChanJobAppImpl {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.repositories
            .memory_job_processing_status_repository
            .clone()
    }
}
impl UseChanJobResultPubSubRepository for RdbChanJobAppImpl {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl {
        &self.repositories.chan_job_result_pubsub_repository
    }
}

// for rdb chan
#[async_trait]
pub trait RdbChanJobAppHelper:
    UseRdbChanJobRepository
    + UseChanJobQueueRepository
    + UseJobProcessingStatusRepository
    + JobBuilder
    + UseJobQueueConfig
    + UseChanJobResultPubSubRepository
    + UseMemoryCache<Arc<String>, Job>
    + JobCacheKeys
where
    Self: Sized + 'static,
{
    // for chanbuffer
    async fn enqueue_job_sync(
        &self,
        job: &Job,
        worker: &WorkerData,
    ) -> Result<(
        JobId,
        Option<JobResult>,
        Option<BoxStream<'static, ResultOutputItem>>,
    )> {
        let job_id = job.id.unwrap();
        if self.is_run_after_job(job) {
            return Err(JobWorkerError::InvalidParameter(
                "run_after_time is not supported in rdb_chan mode".to_string(),
            )
            .into());
        }
        // use channel to enqueue job immediately
        let res = match self
            .chan_job_queue_repository()
            .enqueue_job(worker.channel.as_ref(), job)
            .await
        {
            Ok(_) => {
                if let Job {
                    id: Some(id),
                    data: Some(job_data),
                    ..
                } = &job
                {
                    let cache_key = Arc::new(Self::find_cache_key(id));
                    // For timeout=0 (unlimited), uses expire_job_result_seconds from config
                    let job_ttl = self.calculate_job_ttl(job_data.timeout);

                    // Direct jobs exist only in cache (not in RDB), so wait for
                    // stretto admission to complete before returning.
                    self.set_and_wait_cache(cache_key, job.clone(), job_ttl.as_ref())
                        .await;
                    tracing::debug!(
                        "Cached Direct Response job {} with TTL {:?} for running job visibility",
                        id.value,
                        job_ttl
                    );
                }
                // update status (not use direct response)
                self.job_processing_status_repository()
                    .upsert_status(&job_id, &JobProcessingStatus::Pending)
                    .await?;
                // wait for result if direct response type
                if worker.response_type == ResponseType::Direct as i32 {
                    // XXX keep chan connection until response
                    // Calculate total timeout including retries (None means unlimited)
                    let job_timeout = job.data.as_ref().map(|d| d.timeout).unwrap_or(0);
                    let total_timeout = proto::calculate_direct_response_timeout_ms(
                        job_timeout,
                        worker.retry_policy.as_ref(),
                    );
                    self._wait_job_for_direct_response(
                        &job_id,
                        total_timeout,
                        job.data.as_ref().is_some_and(|j| j.streaming_type != 0),
                    )
                    .await
                    .map(|(r, st)| (job_id, Some(r), st))
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
        // wait for and return result (with channel)
        // self.chan_job_queue_repository()
        //     .wait_for_result_queue_for_response(job_id, timeout, output_as_stream) // no timeout
        //     .await
        let fut = self
            .job_result_pubsub_repository()
            .subscribe_result(job_id, timeout);
        if request_streaming {
            let fut2 = self
                .job_result_pubsub_repository()
                .subscribe_result_stream(job_id, timeout);
            let (res, stream) = futures::join!(fut, fut2);
            tracing::debug!("wait_job_for_direct_response: job={:?}", job_id);
            match (res, stream) {
                (Ok(r), Ok(st)) => {
                    // When status is not Success, stream was not created (error before stream generation).
                    // Stream data will never arrive, so discard to prevent hang.
                    // When status IS Success, stream contains intermediate results + error data + End marker.
                    let should_disable_stream = r
                        .data
                        .as_ref()
                        .is_some_and(|d| d.status != ResultStatus::Success as i32);
                    if should_disable_stream {
                        tracing::debug!(
                            "wait_job_for_direct_response: disabling stream for error result, job_id={:?}",
                            job_id,
                        );
                        Ok((r, None))
                    } else {
                        Ok((r, Some(st)))
                    }
                }
                (Err(e), _) => Err(e),
                (_, Err(e)) => Err(e),
            }
        } else {
            fut.await.map(|r| (r, None))
        }
    }
}

//TODO
// create test
#[cfg(test)]
mod tests {
    use super::RdbChanJobAppImpl;
    use super::*;
    use crate::app::runner::RunnerApp;
    use crate::app::runner::rdb::RdbRunnerAppImpl;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use command_utils::util::datetime;
    use infra::infra::job::rows::JobqueueAndCodec;
    use jobworkerp_base::codec::UseProstCodec;
    // use command_utils::util::tracing::tracing_init_test;
    use infra::infra::IdGeneratorWrapper;
    use infra::infra::job_result::pubsub::JobResultSubscriber;
    use infra::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra_utils::infra::test::TEST_RUNTIME;
    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use proto::jobworkerp::data::{
        JobResult, JobResultId, Priority, QueueType, ResponseType, ResultOutput, ResultStatus,
        RunnerId, WorkerData,
    };
    use std::sync::Arc;
    use std::time::Duration;

    async fn create_test_app(
        use_mock_id: bool,
    ) -> Result<(RdbChanJobAppImpl, ChanJobResultPubSubRepositoryImpl)> {
        let rdb_module = setup_test_rdb_module(false).await;
        let repositories = Arc::new(rdb_module);
        // mock id generator (generate 1 until called set method)
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };
        // UseMemoryCache is auto-initialized in RdbChanJobAppImpl::new(), explicit creation unnecessary here
        // MokaCacheConfig used by other applications (RunnerApp, WorkerApp)
        let moka_config = memory_utils::cache::moka::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_millis(100)),
        };
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
            restore_at_startup: Some(false),
        });
        let job_queue_config = Arc::new(JobQueueConfig {
            expire_job_result_seconds: 10,
            fetch_interval: 1000,
            channel_capacity: 10_000,
            pubsub_channel_capacity: 128,
            max_channels: 10_000,
            cancel_channel_capacity: 1_000,
        });
        let worker_config = Arc::new(WorkerConfig {
            default_concurrency: 4,
            channels: vec!["test".to_string()],
            channel_concurrencies: vec![2],
        });
        let descriptor_cache =
            Arc::new(memory_utils::cache::moka::MokaCacheImpl::new(&moka_config));
        let runner_app = Arc::new(RdbRunnerAppImpl::new(
            TEST_PLUGIN_DIR.to_string(),
            storage_config.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache.clone(),
        ));
        let worker_app = RdbWorkerAppImpl::new(
            storage_config.clone(),
            id_generator.clone(),
            &moka_config,
            repositories.clone(),
            descriptor_cache,
            runner_app.clone(),
        );
        let _ = runner_app
            .create_test_runner(&RunnerId { value: 1 }, "Test")
            .await?;

        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
        let config_module = Arc::new(AppConfigModule {
            storage_config,
            worker_config,
            job_queue_config: job_queue_config.clone(),
            runner_factory: Arc::new(runner_factory),
        });
        let subscrber = repositories.chan_job_result_pubsub_repository.clone();

        let job_queue_cancellation_repository: Arc<dyn JobQueueCancellationRepository> =
            Arc::new(repositories.chan_job_queue_repository.clone());

        Ok((
            RdbChanJobAppImpl::new(
                config_module,
                id_generator,
                repositories,
                Arc::new(worker_app),
                job_queue_cancellation_repository,
                None, // RDB indexing disabled for test
            ),
            subscrber,
        ))
    }
    #[test]
    fn test_create_direct_job_complete() -> Result<()> {
        // tracing_init_test(tracing::Level::DEBUG);
        // enqueue, find, complete, find, delete, find
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let runner_settings =
                JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                    name: "ls".to_string(),
                })?;
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
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
            let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["ls".to_string(), "/".to_string()],
            })?;
            // move
            let worker_id1 = worker_id;
            let jargs1 = jargs.clone();
            let app1 = app.clone();
            // need waiting for direct response
            let metadata = Arc::new(HashMap::new());
            let wd1 = wd.clone();
            let jh = tokio::spawn(async move {
                let res = app1
                    .enqueue_job(
                        metadata.clone(),
                        Some(&worker_id1),
                        None,
                        jargs1.clone(),
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
                // can find job in direct response type until completed
                let job = app1
                    .find_job(
                        &job_res
                            .clone()
                            .and_then(|j| j.data.and_then(|d| d.job_id))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(job.is_some());
                let job = job.unwrap();
                assert_eq!(job.id.as_ref().unwrap().value, jid.value);
                assert_eq!(job.data.as_ref().unwrap().worker_id, Some(worker_id1));
                assert_eq!(job.data.as_ref().unwrap().args, jargs1);

                let job_res = job_res.unwrap();
                assert_eq!(job_res.id.as_ref().unwrap().value, jid.value);
                assert_eq!(
                    job_res.data.as_ref().unwrap().job_id,
                    Some(JobId { value: jid.value })
                );
                assert_eq!(job_res.data.as_ref().unwrap().worker_id, Some(worker_id1));
                assert_eq!(job_res.data.as_ref().unwrap().worker_name, wd1.name);
                assert_eq!(job_res.data.as_ref().unwrap().args, jargs1);
                assert_eq!(
                    job_res.data.as_ref().unwrap().status,
                    ResultStatus::Success as i32
                );
                assert_eq!(
                    job_res.data.as_ref().unwrap().output,
                    Some(ResultOutput {
                        items: { "test".as_bytes().to_vec() },
                    })
                );
                assert_eq!(
                    job_res.data.as_ref().unwrap().status,
                    ResultStatus::Success as i32
                );
                // check job processing status
                // not running worker, only by complete_job()
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
                    args: jargs,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                    broadcast_results: false,
                }),
                ..Default::default()
            };
            assert!(
                app.complete_job(&rid, result.data.as_ref().unwrap(), None)
                    .await?
            );
            let (jid, job_res) = jh.await?;
            tokio::time::sleep(Duration::from_millis(100)).await;

            assert_eq!(&job_res, &result);
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
    fn test_create_broadcast_result_job_complete() -> Result<()> {
        // tracing_init_test(tracing::Level::DEBUG);
        // enqueue, find, complete, find, delete, find
        TEST_RUNTIME.block_on(async {
            let (app, subscriber) = create_test_app(true).await?;
            let runner_settings =
                JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                    name: "ls".to_string(),
                })?;
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: None,
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::WithBackup as i32,
                store_failure: true,
                store_success: true,
                use_static: false,
                broadcast_results: true,
            };
            let worker_id = app.worker_app().create(&wd).await?;
            let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            })?;
            let metadata = Arc::new(HashMap::new());

            // wait for direct response
            let job_id = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jargs.clone(),
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
                    args: jargs.clone(),
                    uniq_key: None,
                    enqueue_time: datetime::now_millis(),
                    grabbed_until_time: None,
                    run_after_time: 0,
                    retried: 0,
                    priority: 0,
                    timeout: 0,
                    streaming_type: 0,
                    using: None,
                }),
                metadata: (*metadata).clone(),
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
                    args: jargs,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                    store_failure: true,
                    using: None,
                    broadcast_results: true,
                }),
                metadata: (*metadata).clone(),
            };
            let jid = job_id;
            let res = result.clone();
            // waiting for receiving job result at first (as same as job result listening process)
            let jh = tokio::task::spawn(async move {
                let job_result = subscriber.subscribe_result(&jid, Some(6000)).await.unwrap();
                assert!(job_result.id.unwrap().value > 0);
                assert_eq!(&job_result.data, &res.data);
            });
            // pseudo process time
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
        let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
            name: "ls".to_string(),
        })?;
        let wd = WorkerData {
            name: "testworker".to_string(),
            description: "desc1".to_string(),
            runner_id: Some(RunnerId { value: 1 }),
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
        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(true).await?;
            let worker_id = app.worker_app().create(&wd).await?;
            let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            })?;
            let metadata = Arc::new(HashMap::new());

            // wait for direct response
            let (job_id, res, _) = app
                .enqueue_job(
                    metadata.clone(),
                    Some(&worker_id),
                    None,
                    jargs.clone(),
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
                    args: jargs,
                    uniq_key: None,
                    status: ResultStatus::Success as i32,
                    output: Some(ResultOutput {
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
                    broadcast_results: false,
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

    // TODO not implemented for rdb chan
    #[test]
    fn test_restore_jobs_from_rdb() -> Result<()> {
        // tracing_init_test(tracing::Level::DEBUG);
        let priority = Priority::Medium;
        let channel: Option<&String> = None;

        TEST_RUNTIME.block_on(async {
            let (app, _) = create_test_app(false).await?;
            // create command worker with backup queue
            let runner_settings =
                JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                    name: "ls".to_string(),
                })?;
            let wd = WorkerData {
                name: "testworker".to_string(),
                description: "desc1".to_string(),
                runner_id: Some(RunnerId { value: 1 }),
                runner_settings,
                channel: channel.cloned(),
                response_type: ResponseType::NoResult as i32,
                periodic_interval: 0,
                retry_policy: None,
                queue_type: QueueType::WithBackup as i32, // with chan queue
                store_success: true,
                store_failure: false,
                use_static: false,
                broadcast_results: false,
            };
            let worker_id = app.worker_app().create(&wd).await?;

            // enqueue job
            let jargs = JobqueueAndCodec::serialize_message(&proto::TestArgs {
                args: vec!["/".to_string()],
            })?;
            assert_eq!(
                app.chan_job_queue_repository()
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
                    jargs.clone(),
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
                    metadata,
                    Some(&worker_id),
                    None,
                    jargs.clone(),
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
            assert_eq!(
                app.chan_job_queue_repository()
                    .count_queue(channel, priority)
                    .await?,
                2
            );
            // println!(
            //     "==== statuses: {:?}",
            //     app.job_processing_status_repository().find_status_all().await.unwrap()
            // );

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

            // // no jobs to restore  (exists in both redis and rdb)
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );
            // app.restore_jobs_from_rdb(false, None).await?;
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );

            // // find job for delete (grabbed_until_time is SOme(0) in queue)
            // let job_d = app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id)
            //     .await?
            //     .unwrap();
            // // lost only from redis (delete)
            // assert!(
            //     app.rdb_job_repository()
            //         .delete_from_queue(channel, priority, &job_d)
            //         .await?
            //         > 0
            // );
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     1
            // );
            //
            // // restore 1 lost jobs
            // app.restore_jobs_from_rdb(false, None).await?;
            // assert!(app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id)
            //     .await?
            //     .is_some());
            // assert!(app
            //     .rdb_job_repository()
            //     .find_from_queue(channel, priority, &job_id2)
            //     .await?
            //     .is_some());
            // assert_eq!(
            //     app.rdb_job_repository()
            //         .count_queue(channel, priority)
            //         .await?,
            //     2
            // );
            Ok(())
        })
    }

    #[test]
    fn test_cleanup_job_processing_status_disabled() {
        TEST_RUNTIME.block_on(async {
            // Setup: RDB indexing disabled
            // SAFETY: called in test setup before spawning threads
            unsafe { std::env::set_var("JOB_STATUS_RDB_INDEXING", "false") };

            let (app, _) = create_test_app(false).await.unwrap();

            // Execute cleanup - should fail with repository not available
            let result = app.cleanup_job_processing_status(None).await;

            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("index repository not available")
                    || err_msg.contains("JOB_STATUS_RDB_INDEXING")
            );

            // Cleanup
            // SAFETY: called in test cleanup
            unsafe { std::env::remove_var("JOB_STATUS_RDB_INDEXING") };
        })
    }

    // #[test]
    // #[ignore] // Requires real RDB with job_processing_status table
    // fn test_cleanup_job_processing_status_success() {
    //     TEST_RUNTIME.block_on(async {
    //         // Setup: RDB indexing enabled
    //         std::env::set_var("JOB_STATUS_RDB_INDEXING", "true");
    //         std::env::set_var("JOB_STATUS_RETENTION_HOURS", "24");

    //         // Create app with index repository
    //         let (app, _) = create_test_app(false).await.unwrap();

    //         // Execute cleanup with 1 hour retention override
    //         let result = app.cleanup_job_processing_status(Some(1)).await;

    //         // Should succeed (even if no records deleted)
    //         assert!(result.is_ok());
    //         let (_deleted_count, cutoff_time) = result.unwrap();
    //         assert!(cutoff_time > 0);
    //         // Note: deleted_count can be 0 if no old records exist

    //         // Cleanup
    //         std::env::remove_var("JOB_STATUS_RDB_INDEXING");
    //         std::env::remove_var("JOB_STATUS_RETENTION_HOURS");
    //     })
    // }

    /// Verify that when a streaming job completes with error status and no stream (Layer 2),
    /// complete_job publishes an End marker so subscribe_result_stream does not hang forever.
    #[test]
    fn test_streaming_error_complete_job_publishes_end_marker() -> Result<()> {
        use infra::infra::job_result::pubsub::JobResultPublisher;
        TEST_RUNTIME.block_on(async {
            let (_app, subscriber) = create_test_app(true).await?;
            let job_id = JobId { value: 99001 };

            // Subscribe to both result and stream before publishing (simulates client behavior)
            let jid = job_id;
            let sub = subscriber.clone();
            let jh = tokio::task::spawn(async move {
                let (result, stream) = futures::join!(
                    sub.subscribe_result(&jid, Some(5000)),
                    sub.subscribe_result_stream(&jid, Some(5000)),
                );
                let result = result.unwrap();
                assert_ne!(
                    result.data.as_ref().unwrap().status,
                    ResultStatus::Success as i32,
                    "Status should not be Success for error case"
                );
                // Stream should have been unblocked by End marker
                assert!(stream.is_ok(), "Stream subscription should succeed");
            });

            tokio::time::sleep(Duration::from_millis(100)).await;

            // Simulate: publish result first, then End marker via spawn_end_marker_if_needed
            let result_data = JobResultData {
                job_id: Some(job_id),
                worker_id: Some(WorkerId { value: 1 }),
                worker_name: "test".to_string(),
                args: vec![],
                uniq_key: None,
                status: ResultStatus::OtherError as i32,
                output: None,
                retried: 0,
                max_retry: 0,
                priority: 0,
                timeout: 0,
                streaming_type: StreamingType::Response as i32,
                enqueue_time: datetime::now_millis(),
                run_after_time: 0,
                start_time: datetime::now_millis(),
                end_time: datetime::now_millis(),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                using: None,
                broadcast_results: false,
            };
            let rid = JobResultId { value: 99001 };
            subscriber.publish_result(&rid, &result_data, true).await?;
            // Layer 2: publish End marker for streaming error
            super::super::spawn_end_marker_if_needed(&result_data, &job_id, &subscriber);
            // Allow spawned task to complete
            tokio::time::sleep(Duration::from_millis(100)).await;

            let join_result = tokio::time::timeout(Duration::from_secs(5), jh).await;
            assert!(
                join_result.is_ok(),
                "subscribe_result_stream should not hang on error status"
            );
            join_result.unwrap()?;

            Ok(())
        })
    }

    /// Verify that _wait_job_for_direct_response (Layer 1) discards stream
    /// when result status is not Success, preventing client hang.
    #[test]
    fn test_wait_job_direct_response_discards_stream_on_error() -> Result<()> {
        use infra::infra::job_result::pubsub::JobResultPublisher;
        TEST_RUNTIME.block_on(async {
            let (app, subscriber) = create_test_app(true).await?;
            let job_id = JobId { value: 99002 };

            let app_clone = app.clone();
            let jid = job_id;
            // Start _wait_job_for_direct_response with streaming=true
            let jh = tokio::task::spawn(async move {
                let result = app_clone
                    ._wait_job_for_direct_response(&jid, Some(5000), true)
                    .await;
                assert!(result.is_ok(), "Should succeed even with error status");
                let (job_result, stream) = result.unwrap();
                assert_ne!(
                    job_result.data.as_ref().unwrap().status,
                    ResultStatus::Success as i32,
                );
                // Layer 1: stream should be discarded (None) for non-Success status
                assert!(
                    stream.is_none(),
                    "Stream should be None when result status is not Success"
                );
            });

            tokio::time::sleep(Duration::from_millis(100)).await;

            // Publish error result + End marker (simulating what complete_job does)
            let result_data = JobResultData {
                job_id: Some(job_id),
                worker_id: Some(WorkerId { value: 1 }),
                worker_name: "test".to_string(),
                args: vec![],
                uniq_key: None,
                status: ResultStatus::OtherError as i32,
                output: None,
                retried: 0,
                max_retry: 0,
                priority: 0,
                timeout: 0,
                streaming_type: StreamingType::Response as i32,
                enqueue_time: datetime::now_millis(),
                run_after_time: 0,
                start_time: datetime::now_millis(),
                end_time: datetime::now_millis(),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                using: None,
                broadcast_results: false,
            };
            let rid = JobResultId { value: 99002 };
            subscriber.publish_result(&rid, &result_data, true).await?;
            super::super::spawn_end_marker_if_needed(&result_data, &job_id, &subscriber);
            tokio::time::sleep(Duration::from_millis(100)).await;

            let join_result = tokio::time::timeout(Duration::from_secs(5), jh).await;
            assert!(
                join_result.is_ok(),
                "_wait_job_for_direct_response should not hang on error status"
            );
            join_result.unwrap()?;

            Ok(())
        })
    }

    /// Verify that non-streaming jobs are not affected by the streaming error fix.
    #[test]
    fn test_non_streaming_job_unaffected() -> Result<()> {
        use infra::infra::job_result::pubsub::JobResultPublisher;
        TEST_RUNTIME.block_on(async {
            let (app, subscriber) = create_test_app(true).await?;
            let job_id = JobId { value: 99003 };

            let app_clone = app.clone();
            let jid = job_id;
            // Start _wait_job_for_direct_response with streaming=false
            let jh = tokio::task::spawn(async move {
                let result = app_clone
                    ._wait_job_for_direct_response(&jid, Some(5000), false)
                    .await;
                assert!(result.is_ok());
                let (job_result, stream) = result.unwrap();
                assert_eq!(
                    job_result.data.as_ref().unwrap().status,
                    ResultStatus::Success as i32,
                );
                // Non-streaming: stream should always be None
                assert!(
                    stream.is_none(),
                    "Stream should be None for non-streaming jobs"
                );
            });

            tokio::time::sleep(Duration::from_millis(100)).await;

            let result_data = JobResultData {
                job_id: Some(job_id),
                worker_id: Some(WorkerId { value: 1 }),
                worker_name: "test".to_string(),
                args: vec![],
                uniq_key: None,
                status: ResultStatus::Success as i32,
                output: None,
                retried: 0,
                max_retry: 0,
                priority: 0,
                timeout: 0,
                streaming_type: StreamingType::None as i32,
                enqueue_time: datetime::now_millis(),
                run_after_time: 0,
                start_time: datetime::now_millis(),
                end_time: datetime::now_millis(),
                response_type: ResponseType::Direct as i32,
                store_success: false,
                store_failure: false,
                using: None,
                broadcast_results: false,
            };
            let rid = JobResultId { value: 99003 };
            subscriber.publish_result(&rid, &result_data, true).await?;

            let join_result = tokio::time::timeout(Duration::from_secs(5), jh).await;
            assert!(
                join_result.is_ok(),
                "Non-streaming job should complete normally"
            );
            join_result.unwrap()?;

            Ok(())
        })
    }
}
