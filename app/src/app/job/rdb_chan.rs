use crate::app::{UseWorkerConfig, WorkerConfig};
use crate::module::AppConfigModule;

use super::super::worker::{UseWorkerApp, WorkerApp};
use super::super::JobBuilder;
use super::{JobApp, JobCacheKeys};
// use super::constants::cancellation::{CANCEL_REASON_USER_REQUEST, CANCEL_REASON_BEFORE_EXECUTION};
use anyhow::Result;
use async_trait::async_trait;

use command_utils::util::datetime;
use futures::stream::BoxStream;
use infra::infra::job::queue::{
    JobQueueCancellationRepository, JobQueueCancellationRepositoryDispatcher, UseJobQueueCancellationRepositoryDispatcher,
};
use infra::infra::job::queue::chan::{
    ChanJobQueueRepository, ChanJobQueueRepositoryImpl, UseChanJobQueueRepository,
};
use infra::infra::job::rdb::{RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use infra::infra::job_result::pubsub::chan::{
    ChanJobResultPubSubRepositoryImpl, UseChanJobResultPubSubRepository,
};
use infra::infra::job_result::pubsub::{JobResultPublisher, JobResultSubscriber};
use infra::infra::module::rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::cache::{MokaCacheImpl, UseMokaCache};
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobData, JobId, JobResult, JobResultData, JobResultId, JobProcessingStatus, QueueType, ResponseType,
    ResultOutputItem, Worker, WorkerData, WorkerId,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct RdbChanJobAppImpl {
    app_config_module: Arc<AppConfigModule>,
    id_generator: Arc<IdGeneratorWrapper>,
    repositories: Arc<RdbChanRepositoryModule>,
    worker_app: Arc<dyn WorkerApp + 'static>,
    memory_cache: MokaCacheImpl<Arc<String>, Job>,
    job_queue_cancellation_repository_dispatcher: JobQueueCancellationRepositoryDispatcher,
}

impl RdbChanJobAppImpl {
    pub fn new(
        app_config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        repositories: Arc<RdbChanRepositoryModule>,
        worker_app: Arc<dyn WorkerApp + 'static>,
        memory_cache: MokaCacheImpl<Arc<String>, Job>,
        job_queue_cancellation_repository_dispatcher: JobQueueCancellationRepositoryDispatcher,
    ) -> Self {
        Self {
            app_config_module,
            id_generator,
            repositories,
            worker_app,
            memory_cache,
            job_queue_cancellation_repository_dispatcher,
        }
    }

    /// Internal implementation: Proper cancellation processing + cleanup
    /// Similar to HybridJobAppImpl implementation for Standalone mode
    async fn cancel_job_with_cleanup(&self, id: &JobId) -> Result<bool> {
        // 1. Check current job status
        let current_status = self.job_processing_status_repository()
            .find_status(id)
            .await?;
        
        let cancellation_result = match current_status {
            Some(JobProcessingStatus::Running) => {
                // Running → Cancelling state change
                self.job_processing_status_repository()
                    .upsert_status(id, &JobProcessingStatus::Cancelling)
                    .await?;
                
                // 2. Active cancellation of running jobs (broadcast)
                self.broadcast_job_cancellation(id).await?;
                
                tracing::info!("Job {} marked as cancelling, broadcasting to workers", id.value);
                true
            }
            Some(JobProcessingStatus::Pending) => {
                // Pending → Cancelling state change (Worker will detect when fetching)
                self.job_processing_status_repository()
                    .upsert_status(id, &JobProcessingStatus::Cancelling)
                    .await?;
                
                tracing::info!("Pending job {} marked as cancelling", id.value);
                true // Will be processed by Worker's ResultProcessor
            }
            Some(JobProcessingStatus::Cancelling) => {
                tracing::info!("Job {} is already being cancelled", id.value);
                true // Already being cancelled but considered success
            }
            Some(JobProcessingStatus::WaitResult) => {
                // Processing completed, waiting for result - cannot cancel
                tracing::info!("Job {} is waiting for result processing, cancellation not possible", id.value);
                false // Cancellation failed
            }
            Some(JobProcessingStatus::Unknown) => {
                tracing::warn!("Job {} has unknown status, attempting cancellation", id.value);
                false // Cancellation failed
            }
            None => {
                // Status doesn't exist (already completed or doesn't exist)
                tracing::info!("Job {} status not found, may be already completed", id.value);
                false // Cancellation failed
            }
        };
        
        // 3. Cancellation state setting completed (result processing will be done by Worker's ResultProcessor)
        
        // 4. DB/cache deletion (if necessary)
        let db_deletion_result = match self.rdb_job_repository().delete(id).await {
            Ok(r) => {
                let _ = self.memory_cache.delete_cache(&Arc::new(Self::find_cache_key(id))).await;
                Ok(r)
            }
            Err(e) => Err(e),
        };
        
        // Cancellation success or DB deletion success
        let db_result = db_deletion_result.unwrap_or(false);
        let final_result = cancellation_result || db_result;
        Ok(final_result)
    }

    /// Active cancellation of running jobs (Memory environment notification)
    async fn broadcast_job_cancellation(&self, job_id: &JobId) -> Result<()> {
        tracing::info!("Job cancellation broadcast requested for job {} (Memory environment)", job_id.value);
        
        // Use JobQueueCancellationRepositoryDispatcher to broadcast cancellation
        self.job_queue_cancellation_repository_dispatcher
            .broadcast_job_cancellation(job_id)
            .await?;
        
        tracing::info!("Job cancellation broadcast completed for job {}", job_id.value);
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
        request_streaming: bool,
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
            self.worker_app()
                .check_worker_streaming(wid, request_streaming)
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
                request_streaming,
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
            let data = if (w.periodic_interval > 0 || w.queue_type == QueueType::ForcedRdb as i32)
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
                self.enqueue_job_sync(&job, w).await
            } else if w.periodic_interval > 0 || self.is_run_after_job_data(&data) {
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                    metadata: (*metadata).clone(),
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
                // normal job
                let job = Job {
                    id: Some(jid),
                    data: Some(data),
                    metadata: (*metadata).clone(),
                };
                if w.queue_type == QueueType::WithBackup as i32 {
                    // instant job (store rdb for backup, and enqueue to chan)
                    match self.rdb_job_repository().create(&job).await {
                        Ok(_id) => self.enqueue_job_sync(&job, w).await,
                        Err(e) => Err(e),
                    }
                } else if w.queue_type == QueueType::ForcedRdb as i32 {
                    // use only rdb queue
                    let created = self.rdb_job_repository().create(&job).await?;
                    if created {
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
                    self.enqueue_job_sync(&job, w).await
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
        request_streaming: bool,
        with_random_name: bool,
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
            request_streaming,
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
        request_streaming: bool,
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
                request_streaming,
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
                    || w.queue_type == QueueType::ForcedRdb as i32
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
                        let _ = self
                            .memory_cache
                            .delete_cache(&Arc::new(Self::find_cache_key(jid)))
                            .await; // ignore error
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

    async fn complete_job(
        &self,
        id: &JobResultId,
        data: &JobResultData,
        stream: Option<BoxStream<'static, ResultOutputItem>>,
    ) -> Result<bool> {
        tracing::debug!("complete_job: res_id={}", &id.value);
        if let Some(jid) = data.job_id.as_ref() {
            self.job_processing_status_repository().delete_status(jid).await?;
            match ResponseType::try_from(data.response_type) {
                Ok(ResponseType::Direct) => {
                    let res = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, true) // XXX to_listen = worker.broadcast_result (if possible)
                        .await;
                    // stream data
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        tracing::debug!(
                            "complete_job(direct): publish stream data: {}",
                            &jid.value
                        );
                        pubsub_repo.publish_result_stream_data(*jid, stream).await?;
                        tracing::debug!(
                            "complete_job(direct): stream data published: {}",
                            &jid.value
                        );
                    }
                    res
                }
                Ok(_res) => {
                    // publish for listening result client
                    let r = self
                        .job_result_pubsub_repository()
                        .publish_result(id, data, true) // XXX to_listen = worker.broadcast_result (if possible)
                        .await;
                    // broadcast stream data if exists
                    if let Some(stream) = stream {
                        let pubsub_repo = self.job_result_pubsub_repository().clone();
                        pubsub_repo.publish_result_stream_data(*jid, stream).await?;
                        tracing::debug!("complete_job: stream data published: {}", &jid.value);
                    }
                    self.delete_job(jid).await?;
                    r
                }
                _ => {
                    tracing::warn!("complete_job: invalid response_type: {:?}", &data);
                    // abnormal response type, no publish
                    self.delete_job(jid).await?;
                    Ok(false)
                }
            }
        } else {
            // something wrong
            tracing::error!("no job found from result: {:?}", data);
            Ok(false)
        }
    }

    async fn delete_job(&self, id: &JobId) -> Result<bool> {
        // Phase 4.5: Updated to use proper cancellation instead of simple deletion
        self.cancel_job_with_cleanup(id).await
    }

    // cannot get job of queue type REDIS (redis is used for queue and job cache)
    async fn find_job(&self, id: &JobId) -> Result<Option<Job>>
    where
        Self: Send + 'static,
    {
        let k = Arc::new(Self::find_cache_key(id));
        self.memory_cache
            .with_cache_if_some(&k, || async { self.rdb_job_repository().find(id).await })
            .await
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
            if let Some(jid) = j.id.as_ref() {
                if let Ok(status) = self.job_processing_status_repository().find_status(jid).await {
                    res.push((j, status));
                    if limit.is_some() && res.len() >= *limit.unwrap() as usize {
                        break;
                    }
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
        let all_statuses = self.job_processing_status_repository().find_status_all().await?;

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
        self.job_processing_status_repository().find_status(id).await
    }

    async fn find_all_job_status(&self) -> Result<Vec<(JobId, JobProcessingStatus)>>
    where
        Self: Send + 'static,
    {
        self.job_processing_status_repository().find_status_all().await
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
impl UseJobqueueAndCodec for RdbChanJobAppImpl {}
impl UseJobProcessingStatusRepository for RdbChanJobAppImpl {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.repositories.memory_job_processing_status_repository.clone()
    }
}
impl UseChanJobResultPubSubRepository for RdbChanJobAppImpl {
    fn job_result_pubsub_repository(&self) -> &ChanJobResultPubSubRepositoryImpl {
        &self.repositories.chan_job_result_pubsub_repository
    }
}

impl UseJobQueueCancellationRepositoryDispatcher for RdbChanJobAppImpl {
    fn job_queue_cancellation_repository_dispatcher(&self) -> &JobQueueCancellationRepositoryDispatcher {
        &self.job_queue_cancellation_repository_dispatcher
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
                // update status (not use direct response)
                self.job_processing_status_repository()
                    .upsert_status(&job_id, &JobProcessingStatus::Pending)
                    .await?;
                // wait for result if direct response type
                if worker.response_type == ResponseType::Direct as i32 {
                    // XXX keep chan connection until response
                    self._wait_job_for_direct_response(
                        &job_id,
                        job.data.as_ref().and_then(|d| {
                            if d.timeout == 0 {
                                None
                            } else {
                                Some(d.timeout)
                            }
                        }),
                        job.data.as_ref().is_some_and(|j| j.request_streaming),
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
                (Ok(r), Ok(st)) => Ok((r, Some(st))),
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
    use crate::app::runner::rdb::RdbRunnerAppImpl;
    use crate::app::runner::RunnerApp;
    use crate::app::worker::rdb::RdbWorkerAppImpl;
    use crate::app::{StorageConfig, StorageType};
    use crate::module::test::TEST_PLUGIN_DIR;
    use anyhow::Result;
    use command_utils::util::datetime;
    use infra::infra::job::rows::JobqueueAndCodec;
    // use command_utils::util::tracing::tracing_init_test;
    use infra::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl;
    use infra::infra::job_result::pubsub::JobResultSubscriber;
    use infra::infra::module::rdb::test::setup_test_rdb_module;
    use infra::infra::IdGeneratorWrapper;
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
        let rdb_module = setup_test_rdb_module().await;
        let repositories = Arc::new(rdb_module);
        // mock id generator (generate 1 until called set method)
        let id_generator = if use_mock_id {
            Arc::new(IdGeneratorWrapper::new_mock())
        } else {
            Arc::new(IdGeneratorWrapper::new())
        };
        let moka_config = infra_utils::infra::cache::MokaCacheConfig {
            num_counters: 10000,
            ttl: Some(Duration::from_millis(100)),
        };
        let job_memory_cache = infra_utils::infra::cache::MokaCacheImpl::new(&moka_config);
        let storage_config = Arc::new(StorageConfig {
            r#type: StorageType::Standalone,
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
            Arc::new(infra_utils::infra::cache::MokaCacheImpl::new(&moka_config));
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
        
        // Create JobQueueCancellationRepositoryDispatcher for test
        let job_queue_cancellation_repository_dispatcher = JobQueueCancellationRepositoryDispatcher::Chan(
            repositories.chan_job_queue_repository.clone()
        );
        
        Ok((
            RdbChanJobAppImpl::new(
                config_module,
                id_generator,
                repositories,
                Arc::new(worker_app),
                job_memory_cache,
                job_queue_cancellation_repository_dispatcher,
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
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
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
                args: vec!["/".to_string()],
            });
            // move
            let worker_id1 = worker_id;
            let jargs1 = jargs.clone();
            let app1 = app.clone();
            // need waiting for direct response
            let metadata = Arc::new(HashMap::new());
            let jh = tokio::spawn(async move {
                let res = app1
                    .enqueue_job(
                        metadata.clone(),
                        Some(&worker_id1),
                        None,
                        jargs1,
                        None,
                        0,
                        0,
                        0,
                        None,
                        false,
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
                    .unwrap();
                assert!(job.is_none());
                assert_eq!(
                    app1.job_processing_status_repository()
                        .find_status(&jid)
                        .await
                        .unwrap(),
                    None
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
                    request_streaming: false,
                    enqueue_time: datetime::now_millis(),
                    run_after_time: 0,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::Direct as i32,
                    store_success: false,
                    store_failure: false,
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
                app.job_processing_status_repository().find_status(&jid).await.unwrap(),
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
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
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
            });
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
                    false,
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
                    request_streaming: false,
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
                    request_streaming: false,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: true,
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
        });
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
            });
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
                    false,
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
                    request_streaming: false,
                    enqueue_time: job.data.as_ref().unwrap().enqueue_time,
                    run_after_time: job.data.as_ref().unwrap().run_after_time,
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    response_type: ResponseType::NoResult as i32,
                    store_success: true,
                    store_failure: false,
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
            let runner_settings = JobqueueAndCodec::serialize_message(&proto::TestRunnerSettings {
                name: "ls".to_string(),
            });
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
            });
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
                    false,
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
                    false,
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
}
