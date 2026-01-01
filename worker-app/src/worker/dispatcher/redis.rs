use crate::worker::dispatcher::redis_run_after::RedisRunAfterJobDispatcher;
use crate::worker::result_processor::{ResultProcessorImpl, UseResultProcessor};
use crate::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::runner::JobRunner;
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
use infra::infra::job::rdb::{RdbChanJobRepositoryImpl, UseRdbChanJobRepositoryOptional};
use infra::infra::job::redis::RedisJobRepositoryImpl;
use infra::infra::job::redis::UseRedisJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::UseJobProcessingStatusRepository;
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use infra_utils::infra::redis::{RedisClient, UseRedisClient};
use infra_utils::infra::redis::{RedisPool, UseRedisPool};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobProcessingStatus, JobResult, JobResultId, Priority, QueueType, ResponseType, Worker,
};
use redis::{AsyncCommands, RedisError};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing;

use super::redis_run_after::{RedisRunAfterJobDispatcherImpl, UseRedisRunAfterJobDispatcher};
use super::JobDispatcher;

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
    + JobRunner
    + UseRedisJobRepository
    + UseRunnerPoolMap
    + UseResultProcessor
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
    + UseIdGenerator
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
        // send msg in ctrl_c signal with send for shutdown notification in parallel
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.map(|_| {
                tracing::debug!("got sigint signal....");
                send.send(true)
                    .inspect_err(|e| tracing::error!("mpmc send error: {:?}", e))
                    .unwrap();
            })
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
                let th_p = self
                    .redis_pool() // XXX use raw pool
                    .get()
                    .await;
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
            Ok(value) => match Self::deserialize_message::<Job>(&value[1]) {
                Ok(job) => {
                    let job_id = job.id;
                    match self.process_job(job).await {
                        Ok(result) => Ok(result),
                        Err(e) => {
                            // Check if status should be deleted based on error type
                            if let Some(jid) = job_id {
                                if super::should_cleanup_status_on_error(&e) {
                                    self.cleanup_failed_job_status(&jid, "redis").await;
                                }
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
    async fn process_job(&'static self, job: Job) -> Result<JobResult>
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
            let mes = format!("job {:?} is incomplete data.", &job.id);
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
                &sid
            )))
        } else {
            Err(JobWorkerError::NotFound(format!(
                "runner_data {:?} is not found.",
                &sid
            )))
        }?;
        if let Some(cancelled_result) = self
            .check_cancellation_status(&jid, &wid, &wdat, meta.clone(), &jdat)
            .await?
        {
            return self
                .result_processor()
                .process_result(cancelled_result, None, wdat)
                .await;
        }

        if wdat.response_type != ResponseType::Direct as i32
            && wdat.queue_type == QueueType::WithBackup as i32
        {
            if let Some(repo) = self.rdb_job_repository_opt() {
                // grab job in db (only for record as in progress)
                if repo
                    .grab_job(
                        &jid,
                        Some(jdat.timeout),
                        jdat.grabbed_until_time.unwrap_or(0),
                    )
                    .await?
                {
                    // change status to running
                    self.redis_job_repository()
                        .job_processing_status_repository()
                        .upsert_status(&jid, &JobProcessingStatus::Running)
                        .await?;

                    // Index JobProcessingStatus in RDB (if enabled)
                    if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                        let job_id = jid;
                        let worker_id = wid;
                        let channel = wdat.channel.clone().unwrap_or_default();
                        let priority = jdat.priority;
                        let enqueue_time = jdat.enqueue_time;
                        let is_streamable = jdat.streaming_type != 0;
                        let broadcast_results = wdat.broadcast_results;
                        tokio::spawn(async move {
                            if let Err(e) = index_repo
                                .index_status(
                                    &job_id,
                                    &JobProcessingStatus::Running,
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
                                    "Failed to index RUNNING status for job {}: {}",
                                    job_id.value,
                                    e
                                );
                            }
                        });
                    }
                } else {
                    // already grabbed (strange! (not reset previous process in retry?), but continue processing job)
                    tracing::warn!("failed to grab job from db: {:?}, {:?}", &jid, &jdat);
                    return Err(JobWorkerError::AlreadyExists(format!(
                        "already grabbed: {:?}, {:?}",
                        &jid, &jdat
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
            self.redis_job_repository()
                .job_processing_status_repository()
                .upsert_status(&jid, &JobProcessingStatus::Running)
                .await?;

            // Index JobProcessingStatus in RDB (if enabled)
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                let job_id = jid;
                let worker_id = wid;
                let channel = wdat.channel.clone().unwrap_or_default();
                let priority = jdat.priority;
                let enqueue_time = jdat.enqueue_time;
                let is_streamable = jdat.streaming_type != 0;
                let broadcast_results = wdat.broadcast_results;
                tokio::spawn(async move {
                    if let Err(e) = index_repo
                        .index_status(
                            &job_id,
                            &JobProcessingStatus::Running,
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
                            "Failed to index RUNNING status for job {}: {}",
                            job_id.value,
                            e
                        );
                    }
                });
            }
        }

        // Copy metadata needed for RDB indexing before moving jdat
        let jdat_priority = jdat.priority;
        let jdat_enqueue_time = jdat.enqueue_time;
        let jdat_request_streaming = jdat.streaming_type != 0;

        // run job
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
        let id = if let Some(id) = r.0.id {
            id
        } else {
            let id = JobResultId {
                value: self.id_generator().generate_id()?,
            };
            r.0.id = Some(id);
            id
        };
        // TODO execute and return result to result channel.
        tracing::trace!(
            "send result id: {:?}, data: {:?}, hasStream:{}, ",
            id,
            &r.0,
            &r.1.is_some()
        );
        // change status to wait handling result
        if wdat.response_type != ResponseType::Direct as i32 {
            self.redis_job_repository()
                .job_processing_status_repository()
                .upsert_status(&jid, &JobProcessingStatus::WaitResult)
                .await?;

            // Index JobProcessingStatus in RDB (if enabled)
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                let job_id = jid;
                let worker_id = wid;
                let channel = wdat.channel.clone().unwrap_or_default();
                let priority = jdat_priority;
                let enqueue_time = jdat_enqueue_time;
                let is_streamable = jdat_request_streaming;
                let broadcast_results = wdat.broadcast_results;
                tokio::spawn(async move {
                    if let Err(e) = index_repo
                        .index_status(
                            &job_id,
                            &JobProcessingStatus::WaitResult,
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
                            "Failed to index WAIT_RESULT status for job {}: {}",
                            job_id.value,
                            e
                        );
                    }
                });
            }
        }
        self.result_processor().process_result(r.0, r.1, wdat).await
    }
}

#[derive()]
pub struct RedisJobDispatcherImpl {
    pub id_generator: Arc<IdGeneratorWrapper>,
    pub pool: &'static RedisPool,
    redis_client: redis::Client,
    pub redis_job_repository: Arc<RedisJobRepositoryImpl>,
    pub rdb_job_repository_opt: Option<Arc<RdbChanJobRepositoryImpl>>,
    pub app_module: Arc<AppModule>,
    pub run_after_dispatcher: Option<RedisRunAfterJobDispatcherImpl>,
    pub runner_factory: Arc<RunnerFactory>,
    pub runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
}

impl RedisJobDispatcherImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        _config_module: Arc<AppConfigModule>,
        redis_client: redis::Client,
        redis_job_repository: Arc<RedisJobRepositoryImpl>,
        rdb_job_repository_opt: Option<Arc<RdbChanJobRepositoryImpl>>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
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
            redis_client,
            redis_job_repository,
            rdb_job_repository_opt,
            app_module,
            run_after_dispatcher,
            runner_factory,
            runner_pool_map,
            result_processor,
        }
    }
}

impl UseRedisPool for RedisJobDispatcherImpl {
    fn redis_pool(&self) -> &RedisPool {
        self.pool
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
impl JobRunner for RedisJobDispatcherImpl {}

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

// create test for redis dispatcher
