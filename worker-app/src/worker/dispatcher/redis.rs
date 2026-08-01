use crate::worker::dispatcher::redis_run_after::RedisRunAfterJobDispatcher;
use crate::worker::instance_session::{UseWorkerInstanceSession, WorkerInstanceSessionHandle};
use crate::worker::result_processor::{ResultProcessorImpl, UseResultProcessor};
use crate::worker::runner::JobRunner;
use crate::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::subscribe::UseSubscribeWorker;
use anyhow::Result;
use app::app::runner::{RunnerApp, UseRunnerApp};
use app::app::worker::{UseWorkerApp, WorkerApp};
use app::app::{UseWorkerConfig, WorkerConfig};
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use async_trait::async_trait;
use command_utils::trace::Tracing;
use command_utils::util::shutdown::ShutdownLock;
use futures::TryFutureExt;
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::queue::redis::RedisJobQueueRepository;
use infra::infra::job::rdb::{RdbChanJobRepositoryImpl, UseRdbChanJobRepositoryOptional};
use infra::infra::job::redis::RedisJobRepositoryImpl;
use infra::infra::job::redis::UseRedisJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::rdb::StatusIndexUpdate;
use infra::infra::job::status::{
    JobProcessingStatusRecord, StatusTransitionResult, UseJobProcessingStatusRepository,
};
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use infra_utils::infra::redis::{RedisPool, UseRedisBlockingPool, UseRedisPool};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobProcessingStatus, JobResult, Priority, QueueType, ResponseType, Worker,
};
use redis::{AsyncCommands, RedisError};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing;

use super::JobDispatcher;
use super::redis_run_after::{RedisRunAfterJobDispatcherImpl, UseRedisRunAfterJobDispatcher};

/// Result of releasing a claimed attempt before a Redis re-queue.
pub enum RequeueRestoreOutcome {
    Restored,
    Cancelled(Box<JobResult>),
    Skip,
}

// create worker threads by concurrency settings
// pop job from redis queue by blpop and execute by runner, and send result to redis
#[async_trait]
pub trait RedisJobDispatcher:
    UseRedisJobRepository
    + UseRedisRunAfterJobDispatcher
    + UseRdbChanJobRepositoryOptional
    + infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository
    + UseSubscribeWorker
    + UseRedisPool
    + UseRedisBlockingPool
    + JobRunner
    + UseRedisJobRepository
    + UseRunnerPoolMap
    + UseResultProcessor
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
    + UseIdGenerator
    + UseWorkerInstanceSession
    + JobDispatcher
{
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        // create a tokio thread to subscribe update worker event and update worker map
        tokio::spawn(
            self.subscribe_worker_changed()
                .map_err(|e| tracing::error!("subscribe worker changed error: {:?}", e)),
        );
        // for shutdown notification (spmc broadcast)
        let (send, recv) = tokio::sync::watch::channel(false);
        // send msg on shutdown signal (SIGINT/SIGTERM) for shutdown notification in parallel
        tokio::spawn(async move {
            command_utils::util::shutdown::shutdown_signal().await;
            tracing::debug!("got shutdown signal....");
            if let Err(e) = send.send(true) {
                tracing::debug!("failed to send shutdown notification: {:?}", e);
            }
        });

        for (ch, conc) in self.worker_config().channel_concurrency_pair() {
            tracing::info!(
                "create job dispatcher for channel {}, concurrency: {}",
                &ch,
                &conc
            );
            for _ in 0..conc {
                self.pop_and_execute(ch.clone(), lock.clone(), recv.clone());
            }
        }

        // run after job dispatcher (need when use only redis)
        if let Some(rad) = self.redis_run_after_job_dispatcher() {
            rad.execute(lock)?;
        } else {
            lock.unlock();
        }
        tracing::debug!("job dispatcher started");
        Ok(())
    }
    fn pop_and_execute(
        &'static self,
        channel_name: impl Into<String> + Send + 'static,
        lock: ShutdownLock,
        mut shutdown_recv: tokio::sync::watch::Receiver<bool>,
    ) -> JoinHandle<Result<()>>
    where
        Self: Send + Sync + 'static,
    {
        let cn: String = channel_name.into();
        tokio::spawn(async move {
            let cl = Self::queue_channel_name(cn.clone(), Some(Priority::Low as i32).as_ref());
            let cm = Self::queue_channel_name(cn.clone(), Some(Priority::Medium as i32).as_ref());
            let ch = Self::queue_channel_name(cn.clone(), Some(Priority::High as i32).as_ref());
            let c = vec![ch, cm, cl]; // priority
            tracing::debug!("redis pop_and_execute: start job loop for channel {}", &cn);
            'outer: loop {
                // Use blocking pool for BLPOP (no response timeout)
                let th_p = self.redis_blocking_pool().get().await;
                if let Ok(mut th) = th_p {
                    tracing::debug!("start loop of spawned job queue {}", &cn);
                    tokio::select! {
                        // break in shutdown for blpop wait
                        // cannot handle signal when running blocking job with plugin runner (external lib etc)
                        _ = shutdown_recv.changed() => {
                            tracing::debug!("got sigint signal.... channel {}", &cn);
                            break 'outer;
                        },
                        val = th.blpop::<'_, Vec<String>, Vec<Vec<u8>>>(c.clone(), 0f64) => {
                            tracing::trace!("got job.... channel {}", &cn);
                            match self.process_deque_job(
                                val
                            ).await {
                                Ok(r) => {
                                    tracing::trace!("job result: {:?}", &r);
                                },
                                Err(e) => {
                                    tracing::warn!("process job error: {:?}", e);
                                }
                            };
                            if should_exit_redis_dispatch_loop(self.worker_instance_session()) {
                                tracing::info!(
                                    "exit isolated Redis job loop after returning its current job: {}",
                                    &cn
                                );
                                break 'outer;
                            }
                        },
                    }
                } else {
                    tracing::warn!("cannot get connection from pool: {:?}", th_p.err());
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                // shutdown received (not selected case)
                if *shutdown_recv.borrow() {
                    break 'outer;
                }
            }
            tracing::info!("exit job loop for channel {}", cn);
            lock.unlock();
            Result::Ok(())
        })
    }
    #[inline]
    async fn process_deque_job(
        &'static self,
        val: Result<Vec<Vec<u8>>, RedisError>,
    ) -> Result<JobResult>
    where
        Self: Sync + Send + 'static,
    {
        match val {
            Ok(value) => match Self::deserialize_job_internal(&value[1]) {
                Ok((job, load_only)) => {
                    let job_id = job.id;
                    match self.process_job(job, load_only).await {
                        Ok(result) => Ok(result),
                        Err(e) => {
                            // Check if status should be deleted based on error type
                            if let Some(jid) = job_id
                                && super::should_cleanup_status_on_error(&e)
                            {
                                self.cleanup_failed_job_status(&jid, "redis").await;
                            }
                            Err(e)
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("job decode error: {:?}", e);
                    Err(e)
                }
            },
            Err(e) => {
                tracing::error!("pop job error: {:?}", e);
                Err(JobWorkerError::RedisError(e).into())
            }
        }
    }

    #[inline]
    async fn process_job(&'static self, job: Job, load_only: bool) -> Result<JobResult>
    where
        Self: Sync + Send + 'static,
    {
        tracing::debug!("process pop-ed job: {:?}", &job.id);
        let (jid, jdat, meta) = if let Job {
            id: Some(jid),
            data: Some(jdat),
            metadata,
        } = job
        {
            (jid, jdat, metadata)
        } else {
            // Status cleanup is handled by process_deque_job based on error type
            let mes = format!("job {:?} is incomplete data.", job.id);
            tracing::error!("{}", &mes);
            return Err(JobWorkerError::InvalidParameter(mes).into());
        };

        let (wid, wdat) = if let Some(Worker {
            id: Some(wid),
            data: Some(wdat),
        }) = self
            .worker_app()
            .find_by_opt(jdat.worker_id.as_ref())
            .await?
        {
            (wid, wdat)
        } else {
            // Status cleanup is handled by process_deque_job based on error type
            let mes = format!(
                "worker {:?} is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            return Err(JobWorkerError::NotFound(mes).into());
        };
        let sid = wdat.runner_id.ok_or(JobWorkerError::InvalidParameter(
            "worker runner_id is not found.".to_string(),
        ))?;
        let runner_data = if let Some(RunnerWithSchema {
            id: _,
            data: runner_data,
            ..
        }) = self.runner_app().find_runner(&sid).await?
        {
            runner_data.ok_or(JobWorkerError::NotFound(format!(
                "runner_data {:?} is not found.",
                sid
            )))
        } else {
            Err(JobWorkerError::NotFound(format!(
                "runner_data {:?} is not found.",
                sid
            )))
        }?;

        // Load-only (config-check / pre-load): run the runner's load() and return
        // the Direct result without entering the normal job lifecycle — no
        // cancellation monitoring, no status transitions/indexing, and no
        // retry/periodic/store/temp cleanup. These would be meaningless (and for
        // periodic workers, harmful) for a pre-load that never runs run().
        if load_only {
            let result = self
                .preload_runner(
                    &runner_data,
                    &wid,
                    &wdat,
                    Job {
                        id: Some(jid),
                        data: Some(jdat),
                        metadata: meta,
                    },
                )
                .await;
            let (result, completion_rx) = self
                .result_processor()
                .process_result_inner(result, None, wdat, true)
                .await?;
            if let Some(rx) = completion_rx
                && rx.await.is_err()
            {
                tracing::warn!("stream completion sender dropped for load job {:?}", &jid);
            }
            return Ok(result);
        }

        match super::resolve_dispatch_preflight(
            self,
            self.check_cancellation_status(&jid, &wid, &wdat, meta.clone(), &jdat)
                .await?,
            &wdat,
            &jid,
        )
        .await?
        {
            super::DispatchPreflight::Execute => {}
            super::DispatchPreflight::Skip => return Ok(JobResult::default()),
            super::DispatchPreflight::Completed(result) => return Ok(result),
        }

        let start_permit = self
            .worker_instance_session()
            .and_then(WorkerInstanceSessionHandle::acquire_start_permit);
        let worker_instance_id = start_permit.as_ref().map(|permit| permit.instance_id());

        if should_requeue_isolated_job(
            self.worker_instance_session().is_some(),
            start_permit.is_some(),
        ) {
            match self
                .restore_pending_attempt_for_requeue(&jid, &wid, &wdat, meta.clone(), &jdat)
                .await?
            {
                RequeueRestoreOutcome::Restored => {}
                RequeueRestoreOutcome::Cancelled(result) => {
                    return self
                        .process_cancelled_dispatch_result(*result, &wdat, &jid)
                        .await;
                }
                RequeueRestoreOutcome::Skip => return Ok(JobResult::default()),
            }
            let job = Job {
                id: Some(jid),
                data: Some(jdat),
                metadata: meta,
            };
            self.redis_job_repository()
                .requeue_job_with_load_only(wdat.channel.as_ref(), &job, load_only)
                .await
                .map_err(|error| {
                    tracing::error!(
                        job_id = jid.value,
                        %error,
                        "failed to requeue job after worker instance isolation"
                    );
                    error
                })?;
            return Err(JobWorkerError::RuntimeError(
                "worker instance is isolated; job was requeued".to_string(),
            )
            .into());
        }

        let resolved = app::app::job::resolve_job_params(&wdat, jdat.overrides.as_ref());
        let mut grab_lease_expires_at = None;
        let mut running_index_task = None;
        if resolved.response_type != ResponseType::Direct as i32
            && wdat.queue_type == QueueType::WithBackup as i32
        {
            if let Some(repo) = self.rdb_job_repository_opt() {
                // TODO(#311): Claim RUNNING only after the RDB lease is acquired.
                // Releasing this status blindly when grab fails can hide an active
                // RDB-dispatched owner and prevent its cancellation notification.
                // grab job in db (only for record as in progress)
                if let Some(grabbed_until) = repo
                    .grab_job_with_lease(
                        &jid,
                        Some(jdat.timeout),
                        jdat.grabbed_until_time.unwrap_or(0),
                    )
                    .await?
                {
                    grab_lease_expires_at = Some(grabbed_until);
                    // change status to running

                    // Index JobProcessingStatus in RDB (if enabled)
                    if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                        let job_id = jid;
                        let worker_id = wid;
                        let channel = wdat.channel.clone();
                        let priority = jdat.priority;
                        let enqueue_time = jdat.enqueue_time;
                        let is_streamable = jdat.streaming_type != 0;
                        let broadcast_results = resolved.broadcast_results;
                        // The recovery index is best-effort: awaiting it here would make
                        // RDB availability a prerequisite for starting the primary job.
                        running_index_task = Some(tokio::spawn(async move {
                            if let Err(e) = index_repo
                                .index_status_update(StatusIndexUpdate {
                                    job_id: &job_id,
                                    status: &JobProcessingStatus::Running,
                                    worker_id: &worker_id,
                                    channel: channel.as_deref(),
                                    priority,
                                    enqueue_time,
                                    is_streamable,
                                    broadcast_results,
                                    worker_instance_id,
                                })
                                .await
                            {
                                tracing::warn!(
                                    "Failed to index RUNNING status for job {}: {}",
                                    job_id.value,
                                    e
                                );
                            }
                        }));
                    }
                } else {
                    // already grabbed (strange! (not reset previous process in retry?), but continue processing job)
                    tracing::warn!("failed to grab job from db: {:?}, {:?}", &jid, &jdat);
                    return Err(JobWorkerError::AlreadyExists(format!(
                        "already grabbed: {:?}, {:?}",
                        jid, jdat
                    ))
                    .into());
                }
            }
        } else {
            tracing::debug!(
                "Job {} using Direct mode, updating status to Running",
                jid.value
            );
            // change status to running

            // Index JobProcessingStatus in RDB (if enabled)
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                let job_id = jid;
                let worker_id = wid;
                let channel = wdat.channel.clone();
                let priority = jdat.priority;
                let enqueue_time = jdat.enqueue_time;
                let is_streamable = jdat.streaming_type != 0;
                let broadcast_results = resolved.broadcast_results;
                // The recovery index is best-effort: awaiting it here would make
                // RDB availability a prerequisite for starting the primary job.
                running_index_task = Some(tokio::spawn(async move {
                    if let Err(e) = index_repo
                        .index_status_update(StatusIndexUpdate {
                            job_id: &job_id,
                            status: &JobProcessingStatus::Running,
                            worker_id: &worker_id,
                            channel: channel.as_deref(),
                            priority,
                            enqueue_time,
                            is_streamable,
                            broadcast_results,
                            worker_instance_id,
                        })
                        .await
                    {
                        tracing::warn!(
                            "Failed to index RUNNING status for job {}: {}",
                            job_id.value,
                            e
                        );
                    }
                }));
            }
        }

        // Copy metadata needed for RDB indexing before moving jdat
        let jdat_priority = jdat.priority;
        let jdat_enqueue_time = jdat.enqueue_time;
        let jdat_request_streaming = jdat.streaming_type != 0;
        let attempt_for_status = jdat.retried;

        // run job (load-only requests were handled and returned above)
        if let Some(permit) = &start_permit
            && !permit.confirm_start()
        {
            await_running_index_update(running_index_task.take(), jid.value).await;
            match self
                .restore_pending_attempt_for_requeue(&jid, &wid, &wdat, meta.clone(), &jdat)
                .await?
            {
                RequeueRestoreOutcome::Restored => {}
                RequeueRestoreOutcome::Cancelled(result) => {
                    return self
                        .process_cancelled_dispatch_result(*result, &wdat, &jid)
                        .await;
                }
                RequeueRestoreOutcome::Skip => return Ok(JobResult::default()),
            }
            if let Some(grabbed_until) = grab_lease_expires_at
                && let Some(repo) = self.rdb_job_repository_opt()
                && !repo
                    .reset_grabbed_until_time(&jid, grabbed_until, None)
                    .await?
            {
                anyhow::bail!(
                    "lost WithBackup RDB grab before isolated job requeue: {}",
                    jid.value
                );
            }
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository()
                && matches!(
                    index_repo
                        .reset_running_to_pending_by_owner(
                            &jid,
                            worker_instance_id
                                .expect("recovery-enabled session owns the RUNNING row"),
                        )
                        .await?,
                    infra::infra::job::status::rdb::ResetRunningOutcome::OwnedByOther
                )
            {
                anyhow::bail!(
                    "lost RUNNING status index ownership before isolated job requeue: {}",
                    jid.value
                );
            }
            let job = Job {
                id: Some(jid),
                data: Some(jdat),
                metadata: meta,
            };
            self.redis_job_repository()
                .requeue_job_with_load_only(wdat.channel.as_ref(), &job, load_only)
                .await
                .map_err(|error| {
                    tracing::error!(
                        job_id = jid.value,
                        %error,
                        "failed to requeue job after worker isolation before runner start"
                    );
                    error
                })?;
            return Err(JobWorkerError::RuntimeError(
                "worker instance was isolated before runner start; job was requeued".to_string(),
            )
            .into());
        }

        let mut r = self
            .run_job(
                &runner_data,
                &wid,
                &wdat,
                Job {
                    id: Some(jid),
                    data: Some(jdat),
                    metadata: meta,
                },
            )
            .await;
        let id = super::ensure_job_result_id(self.id_generator(), &mut r.0)?;
        // TODO execute and return result to result channel.
        tracing::trace!(
            "send result id: {:?}, data: {:?}, hasStream:{}, ",
            id,
            &r.0,
            &r.1.is_some()
        );
        await_running_index_update(running_index_task.take(), jid.value).await;
        // change status to wait handling result
        if resolved.response_type != ResponseType::Direct as i32 {
            let running = infra::infra::job::status::JobProcessingStatusRecord {
                status: JobProcessingStatus::Running,
                retried: attempt_for_status,
            };
            let wait_result = infra::infra::job::status::JobProcessingStatusRecord {
                status: JobProcessingStatus::WaitResult,
                retried: attempt_for_status,
            };
            let _ = self
                .redis_job_repository()
                .job_processing_status_repository()
                .compare_and_set_status(&jid, Some(running), Some(wait_result))
                .await?;

            // Index JobProcessingStatus in RDB (if enabled)
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                let job_id = jid;
                let worker_id = wid;
                let channel = wdat.channel.clone();
                let priority = jdat_priority;
                let enqueue_time = jdat_enqueue_time;
                let is_streamable = jdat_request_streaming;
                let broadcast_results = resolved.broadcast_results;
                if let Err(e) = index_repo
                    .index_status(
                        &job_id,
                        &JobProcessingStatus::WaitResult,
                        &worker_id,
                        channel.as_deref(),
                        priority,
                        enqueue_time,
                        is_streamable,
                        broadcast_results,
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to index WAIT_RESULT status for job {}: {}",
                        job_id.value,
                        e
                    );
                }
            }
        }
        let (result, completion_rx) = self
            .result_processor()
            .process_result(r.0, r.1, wdat)
            .await?;
        // Wait for background stream-publishing task to finish before allowing
        // this concurrency slot to pop the next job.
        if let Some(rx) = completion_rx
            && rx.await.is_err()
        {
            tracing::warn!("stream completion sender dropped for job {:?}", &jid);
        }
        Ok(result)
    }

    /// Restores a claimed attempt before placing it back on Redis.
    ///
    /// A cancellation that wins this race is finalized instead of re-queueing
    /// the job, so a stale delivery cannot revive a cancelled attempt.
    async fn restore_pending_attempt_for_requeue(
        &self,
        job_id: &proto::jobworkerp::data::JobId,
        worker_id: &proto::jobworkerp::data::WorkerId,
        worker_data: &proto::jobworkerp::data::WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<RequeueRestoreOutcome> {
        let running = JobProcessingStatusRecord {
            status: JobProcessingStatus::Running,
            retried: job_data.retried,
        };
        let pending = JobProcessingStatusRecord {
            status: JobProcessingStatus::Pending,
            retried: job_data.retried,
        };
        match self
            .job_processing_status_repository()
            .compare_and_set_status(job_id, Some(running), Some(pending))
            .await?
        {
            StatusTransitionResult::Applied => Ok(RequeueRestoreOutcome::Restored),
            StatusTransitionResult::Conflict(Some(record))
                if record.status == JobProcessingStatus::Cancelling
                    && record.retried == job_data.retried =>
            {
                match self
                    .check_cancellation_status(
                        job_id,
                        worker_id,
                        worker_data,
                        job_metadata,
                        job_data,
                    )
                    .await?
                {
                    super::DispatchEligibility::Cancelled(result) => {
                        Ok(RequeueRestoreOutcome::Cancelled(result))
                    }
                    super::DispatchEligibility::Skip => Ok(RequeueRestoreOutcome::Skip),
                    super::DispatchEligibility::Execute => {
                        Err(JobWorkerError::RuntimeError(format!(
                            "job {} unexpectedly became executable while cancelling",
                            job_id.value
                        ))
                        .into())
                    }
                }
            }
            StatusTransitionResult::Conflict(current) => {
                Err(JobWorkerError::RuntimeError(format!(
                    "lost RUNNING status ownership before isolated job requeue: {} (current: {:?})",
                    job_id.value, current
                ))
                .into())
            }
        }
    }
}

#[derive()]
pub struct RedisJobDispatcherImpl {
    pub id_generator: Arc<IdGeneratorWrapper>,
    pub pool: &'static RedisPool,
    /// Redis pool for blocking operations like BLPOP.
    /// This pool has response_timeout disabled to allow indefinite waiting.
    pub blocking_pool: &'static RedisPool,
    redis_client: redis::Client,
    pub redis_job_repository: Arc<RedisJobRepositoryImpl>,
    pub rdb_job_repository_opt: Option<Arc<RdbChanJobRepositoryImpl>>,
    pub app_module: Arc<AppModule>,
    pub run_after_dispatcher: Option<RedisRunAfterJobDispatcherImpl>,
    pub runner_factory: Arc<RunnerFactory>,
    pub runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
    worker_instance_session: Option<WorkerInstanceSessionHandle>,
}

impl RedisJobDispatcherImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        _config_module: Arc<AppConfigModule>,
        redis_client: redis::Client,
        redis_job_repository: Arc<RedisJobRepositoryImpl>,
        redis_blocking_pool: &'static RedisPool,
        rdb_job_repository_opt: Option<Arc<RdbChanJobRepositoryImpl>>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
    ) -> Self {
        // use redis only, use run after dispatcher for run after job
        let run_after_dispatcher = // TODO redis only storage
        //  if app_module.config_module.use_redis_only() {
        //     Some(RedisRunAfterJobDispatcherImpl::new(
        //         config_module.job_queue_config.clone(),
        //         app_module.clone(),
        //     ))
        // } else {
            None;
        // };

        Self {
            id_generator,
            pool: redis_job_repository.redis_pool,
            blocking_pool: redis_blocking_pool,
            redis_client,
            redis_job_repository,
            rdb_job_repository_opt,
            app_module,
            run_after_dispatcher,
            runner_factory,
            runner_pool_map,
            result_processor,
            worker_instance_session,
        }
    }
}

impl UseRedisPool for RedisJobDispatcherImpl {
    fn redis_pool(&self) -> &RedisPool {
        self.pool
    }
}

impl UseWorkerInstanceSession for RedisJobDispatcherImpl {
    fn worker_instance_session(&self) -> Option<&WorkerInstanceSessionHandle> {
        self.worker_instance_session.as_ref()
    }
}

impl UseRedisBlockingPool for RedisJobDispatcherImpl {
    fn redis_blocking_pool(&self) -> &RedisPool {
        self.blocking_pool
    }
}

impl UseRedisJobRepository for RedisJobDispatcherImpl {
    fn redis_job_repository(&self) -> &RedisJobRepositoryImpl {
        &self.redis_job_repository
    }
}

impl jobworkerp_base::codec::UseProstCodec for RedisJobDispatcherImpl {}
impl UseJobqueueAndCodec for RedisJobDispatcherImpl {}

impl UseWorkerApp for RedisJobDispatcherImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}
impl UseRunnerApp for RedisJobDispatcherImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.app_module.runner_app.clone()
    }
}

impl UseRunnerFactory for RedisJobDispatcherImpl {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}
impl UseRedisClient for RedisJobDispatcherImpl {
    fn redis_client(&self) -> &RedisClient {
        &self.redis_client
    }
}
impl UseSubscribeWorker for RedisJobDispatcherImpl {}
impl RunnerResultHandler for RedisJobDispatcherImpl {}
impl UseRunnerPoolMap for RedisJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
impl Tracing for RedisJobDispatcherImpl {}
impl JobRunner for RedisJobDispatcherImpl {
    fn register_feed_sender(
        &self,
        job_id: i64,
        sender: tokio::sync::mpsc::Sender<jobworkerp_runner::runner::FeedData>,
    ) {
        // Scalable mode: spawn Redis feed bridge to forward Redis List messages to the runner
        let job_id_proto = proto::jobworkerp::data::JobId { value: job_id };
        // JoinHandle intentionally not tracked: the bridge task self-terminates
        // when is_final is received or the feed_sender (receiver side) is dropped.
        // Note: if the spawned task panics, the panic is silently ignored.
        // This is acceptable because bridge_loop only uses fallible operations (no unwrap/expect).
        drop(crate::worker::runner::feed_bridge::spawn_redis_feed_bridge(
            &self.redis_client,
            &job_id_proto,
            sender,
        ));
    }

    // Scalable mode: Redis bridge self-terminates when feed_sender is dropped,
    // so no explicit cleanup is needed.
    fn unregister_feed_sender(&self, _job_id: i64) {}
}

impl UseIdGenerator for RedisJobDispatcherImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseWorkerConfig for RedisJobDispatcherImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_module.config_module.worker_config
    }
}

impl UseRedisRunAfterJobDispatcher for RedisJobDispatcherImpl {
    fn redis_run_after_job_dispatcher(&self) -> Option<&RedisRunAfterJobDispatcherImpl> {
        self.run_after_dispatcher.as_ref()
    }
}
impl UseRdbChanJobRepositoryOptional for RedisJobDispatcherImpl {
    fn rdb_job_repository_opt(&self) -> Option<&RdbChanJobRepositoryImpl> {
        self.rdb_job_repository_opt.as_deref()
    }
}

impl infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository
    for RedisJobDispatcherImpl
{
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>> {
        self.app_module
            .repositories
            .rdb_module
            .as_ref()
            .and_then(|m| m.rdb_job_processing_status_index_repository.clone())
    }
}
impl RedisJobDispatcher for RedisJobDispatcherImpl {}
impl UseJobQueueConfig for RedisJobDispatcherImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_module.config_module.job_queue_config
    }
}

fn should_requeue_isolated_job(has_session: bool, has_start_permit: bool) -> bool {
    has_session && !has_start_permit
}

fn should_exit_redis_dispatch_loop(session: Option<&WorkerInstanceSessionHandle>) -> bool {
    session.is_some_and(|session| !session.accepts_new_starts())
}

async fn await_running_index_update(task: Option<JoinHandle<()>>, job_id: i64) {
    if let Some(task) = task
        && let Err(error) = task.await
    {
        tracing::warn!(job_id, %error, "RUNNING status index task stopped before completion");
    }
}

impl UseResultProcessor for RedisJobDispatcherImpl {
    fn result_processor(&self) -> &ResultProcessorImpl {
        &self.result_processor
    }
}

impl UseJobProcessingStatusRepository for RedisJobDispatcherImpl {
    fn job_processing_status_repository(
        &self,
    ) -> Arc<dyn infra::infra::job::status::JobProcessingStatusRepository> {
        self.redis_job_repository()
            .job_processing_status_repository()
    }
}

#[async_trait]
impl JobDispatcher for RedisJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RedisJobDispatcher::dispatch_jobs(self, lock)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        await_running_index_update, should_exit_redis_dispatch_loop, should_requeue_isolated_job,
    };
    use crate::worker::instance_session::WorkerInstanceSessionHandle;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn requeues_only_when_a_configured_session_refuses_the_start() {
        assert!(should_requeue_isolated_job(true, false));
        assert!(!should_requeue_isolated_job(true, true));
        assert!(!should_requeue_isolated_job(false, false));
    }

    #[test]
    fn isolated_session_exits_redis_dispatch_loop_after_requeueing() {
        let session =
            WorkerInstanceSessionHandle::new(7, Duration::from_secs(10), Duration::from_secs(1));

        assert!(!should_exit_redis_dispatch_loop(Some(&session)));
        session.begin_isolation();
        assert!(should_exit_redis_dispatch_loop(Some(&session)));
        assert!(!should_exit_redis_dispatch_loop(None));
    }

    #[tokio::test]
    async fn completion_waits_for_the_running_index_update() {
        let running_index_completed = Arc::new(AtomicBool::new(false));
        let completed = running_index_completed.clone();
        let task = tokio::spawn(async move {
            completed.store(true, Ordering::SeqCst);
        });

        await_running_index_update(Some(task), 1).await;

        assert!(running_index_completed.load(Ordering::SeqCst));
    }
}

// create test for redis dispatcher
