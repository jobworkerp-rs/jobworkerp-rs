use crate::plugins::runner::UsePluginRunner;
use crate::plugins::Plugins;
use crate::worker::dispatcher::redis_run_after::RedisRunAfterJobDispatcher;
use crate::worker::result_processor::{ResultProcessorImpl, UseResultProcessor};
use crate::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::runner::JobRunner;
use crate::worker::subscribe::UseSubscribeWorker;
use anyhow::Result;
use app::app::worker::{UseWorkerApp, WorkerApp};
use app::app::{UseWorkerConfig, WorkerConfig};
use app::module::{AppConfigModule, AppModule};
use async_trait::async_trait;
use common::infra::redis::UseRedisClient;
use common::infra::redis::{RedisPool, UseRedisPool};
use common::util::option::FlatMap;
use common::util::result::{Tap, TapErr};
use common::util::shutdown::ShutdownLock;
use futures::TryFutureExt;
use infra::error::JobWorkerError;
use infra::infra::job::rdb::queue::RdbJobQueueRepository;
use infra::infra::job::rdb::{RdbJobRepository, RdbJobRepositoryImpl, UseRdbJobRepositoryOptional};
use infra::infra::job::redis::job_status::JobStatusRepository;
use infra::infra::job::redis::queue::RedisJobQueueRepository;
use infra::infra::job::redis::RedisJobRepositoryImpl;
use infra::infra::job::redis::UseRedisJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use libloading::Library;
use proto::jobworkerp::data::{
    Job, JobResult, JobResultId, JobStatus, Priority, QueueType, ResponseType, Worker,
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
    + UseRdbJobRepositoryOptional
    + UseSubscribeWorker
    + UseRedisPool
    + JobRunner
    + UseRedisJobRepository
    + UseRunnerPoolMap
    + UseResultProcessor
    + UseWorkerConfig
    + UseWorkerApp
    + UseJobQueueConfig
    + UseIdGenerator
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
                    .tap_err(|e| tracing::error!("mpmc send error: {:?}", e))
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
        if self.rdb_job_repository_opt().is_some()
            && !self.job_queue_config().without_recovery_hybrid
        {
            // recovery process thread for timeouts
            let _ = self.recover_timeouts_loop_from_backup(recv.clone());
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
                        val = th.blpop::<Vec<String>, Vec<Vec<u8>>>(c.clone(), 0f64) => {
                            tracing::trace!("got job.... channel {}", &cn);
                            self.process_deque_job(
                                val
                            ).await?;
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
            Result::<()>::Ok(())
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
            Ok(value) => match Self::deserialize_job(&value[1]) {
                Ok(job) => self.process_job(job).await,
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
        tracing::debug!("process pop-ed job: {:?}", job);
        if let Job {
            id: Some(jid),
            data: Some(jdat),
        } = job
        {
            if let Some(Worker {
                id: Some(wid),
                data: Some(wdat),
            }) = self
                .worker_app()
                .find_by_opt(jdat.worker_id.as_ref())
                .await?
            {
                if wdat.response_type != ResponseType::Direct as i32
                    && wdat.queue_type == QueueType::Hybrid as i32
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
                                .upsert_status(&jid, &JobStatus::Running)
                                .await?;
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
                }
                // run job
                let r = self
                    .run_job(
                        &wid,
                        &wdat,
                        Job {
                            id: Some(jid.clone()),
                            data: Some(jdat),
                        },
                    )
                    .await;
                let id = JobResultId {
                    value: self.id_generator().generate_id()?,
                };
                // TODO execute and return result to result channel.
                tracing::trace!("send result id: {:?}, data: {:?}", id, r);
                // change status to wait handling result
                if wdat.response_type != ResponseType::Direct as i32 {
                    self.redis_job_repository()
                        .upsert_status(&jid, &JobStatus::WaitResult)
                        .await?;
                }
                self.result_processor().process_result(id, r, wdat).await
            } else {
                // TODO cannot return result in this case. send result as error?
                let mes = format!(
                    "worker {:?} is not found.",
                    jdat.worker_id.as_ref().unwrap()
                );
                tracing::error!("{}", &mes);
                self.redis_job_repository().delete_status(&jid).await?;
                if let Some(repo) = self.rdb_job_repository_opt() {
                    repo.delete(&jid).await?;
                }
                Err(JobWorkerError::NotFound(mes).into())
            }
        } else {
            // TODO cannot return result in this case. send result as error?
            let mes = format!("job {:?} is incomplete data.", &job);
            tracing::error!("{}", &mes);
            if let Some(id) = job.id.as_ref() {
                self.redis_job_repository().delete_status(id).await?;
                if let Some(repo) = self.rdb_job_repository_opt() {
                    repo.delete(id).await?;
                }
            }
            Err(JobWorkerError::OtherError(mes).into())
        }
    }

    // TODO under construction
    fn recover_timeouts_loop_from_backup(
        &'static self,
        mut shutdown_recv: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        if let Some(rdb_repository) = self.rdb_job_repository_opt() {
            tokio::spawn(async move {
                // same as fetching job interval
                let mut interval = tokio::time::interval(Duration::from_millis(
                    self.job_queue_config().fetch_interval as u64,
                ));
                loop {
                    // using tokio::select and tokio::signal::ctrl_c, break loop by ctrl-c
                    tokio::select! {
                        _ = interval.tick() => {
                            tracing::debug!("execute recovery of timeouts jobs from backup");
                            self._recover_timeouts(rdb_repository).await;
                        }
                        _ = shutdown_recv.changed() => {
                            tracing::debug!("break recovery of timeouts");
                            break;
                        }
                    }
                    // shutdown received (not selected case)
                    if *shutdown_recv.borrow() {
                        break;
                    }
                }
            });
            tracing::debug!("end execute recovery of timeouts");
        } else {
            tracing::debug!("rdb_repository not found, not start recovery process")
        }
        Ok(())
    }
    #[inline]
    async fn _recover_timeouts(&self, rdb_repository: &RdbJobRepositoryImpl) {
        // XXX limit fixed to 500
        let limit = 500;
        match rdb_repository
            .fetch_timeouted_backup_jobs(limit, 0)
            .await
            .map_err(|e| {
                tracing::error!("failed to recovery timeouts: {:?}", e);
                e
            }) {
            Ok(jobs) => {
                // TODO bulk insert? (pipelined)
                for j in jobs {
                    match self
                        .worker_app()
                        .find_data_by_opt(j.data.as_ref().flat_map(|d| d.worker_id.as_ref()))
                        .await
                    {
                        Ok(Some(wd)) => {
                            // cannot recover direct job(not store to rdb)
                            // XXX shouldn't recover not retryable job?
                            if wd.queue_type != QueueType::Hybrid as i32 {
                                continue;
                            }
                            let _ = self
                                .redis_job_repository()
                                .enqueue_job(wd.channel.as_ref(), &j)
                                .await
                                .tap_err(|e| {
                                    tracing::error!("failed to enqueue job for recovery: {:?}", e)
                                })
                                .tap(|_| {
                                    // maybe set timeout value too short?(warn)
                                    tracing::warn!(
                                        "timeout job has be recovered from rdb: job_id={}",
                                        j.id.as_ref().map(|d| d.value).unwrap_or_default()
                                    )
                                });
                        }
                        Ok(None) => tracing::error!("worker not found for job: {:?}", &j),
                        Err(e) => tracing::error!("failed to enqueue job: {:?}", e),
                    }
                }
            }
            Err(e) => {
                tracing::error!("error in finding recovery timeouts: {:?}", e);
            }
        }
    }
}

#[derive()]
pub struct RedisJobDispatcherImpl {
    pub id_generator: Arc<IdGeneratorWrapper>,
    pub pool: &'static RedisPool,
    pub redis_client: redis::Client,
    pub redis_job_repository: Arc<RedisJobRepositoryImpl>,
    pub rdb_job_repository_opt: Option<Arc<RdbJobRepositoryImpl>>,
    pub app_module: Arc<AppModule>,
    pub run_after_dispatcher: Option<RedisRunAfterJobDispatcherImpl>,
    pub plugins: Arc<Plugins>,
    pub runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
}

impl RedisJobDispatcherImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        redis_job_repository: Arc<RedisJobRepositoryImpl>,
        rdb_job_repository_opt: Option<Arc<RdbJobRepositoryImpl>>,
        app_module: Arc<AppModule>,
        plugins: Arc<Plugins>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Self {
        // use redis only, use run after dispatcher for run after job
        let run_after_dispatcher = if app_module.config_module.use_redis_only() {
            Some(RedisRunAfterJobDispatcherImpl::new(
                config_module.job_queue_config.clone(),
                app_module.clone(),
            ))
        } else {
            None
        };
        Self {
            id_generator,
            pool: redis_job_repository.redis_pool,
            redis_client: redis_job_repository.redis_client.clone(),
            redis_job_repository,
            rdb_job_repository_opt,
            app_module,
            run_after_dispatcher,
            plugins,
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

impl UseJobqueueAndCodec for RedisJobDispatcherImpl {}

impl UseRedisClient for RedisJobDispatcherImpl {
    fn redis_client(&self) -> &redis::Client {
        &self.redis_client
    }
}
impl UseWorkerApp for RedisJobDispatcherImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

impl UsePluginRunner for RedisJobDispatcherImpl {
    fn runner_plugins(&self) -> &Vec<(String, Library)> {
        self.plugins.runner_plugins()
    }
}
impl UseSubscribeWorker for RedisJobDispatcherImpl {}
impl RunnerResultHandler for RedisJobDispatcherImpl {}
impl UseRunnerPoolMap for RedisJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
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
impl UseRdbJobRepositoryOptional for RedisJobDispatcherImpl {
    fn rdb_job_repository_opt(&self) -> Option<&RdbJobRepositoryImpl> {
        self.rdb_job_repository_opt.as_deref()
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

impl JobDispatcher for RedisJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RedisJobDispatcher::dispatch_jobs(self, lock)
    }
}

// create test for redis dispatcher
