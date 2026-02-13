use super::JobDispatcher;
use crate::worker::result_processor::{ResultProcessorImpl, UseResultProcessor};
use crate::worker::runner::JobRunner;
use crate::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use crate::worker::runner::result::RunnerResultHandler;
use anyhow::Result;
use app::app::runner::{RunnerApp, UseRunnerApp};
use app::app::worker::{UseWorkerApp, WorkerApp};
use app::app::{UseWorkerConfig, WorkerConfig};
use app::module::AppModule;
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use async_trait::async_trait;
use command_utils::trace::Tracing;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::job::queue::chan::{
    ChanJobQueueRepository, ChanJobQueueRepositoryImpl, UseChanJobQueueRepository,
};
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::rdb::{RdbChanJobRepositoryImpl, UseRdbChanJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::memory::MemoryJobProcessingStatusRepository;
use infra::infra::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::worker::pubsub::ChanWorkerPubSubRepositoryImpl;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobProcessingStatus, JobResult, Priority, QueueType, ResponseType, Worker,
};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing;

// create worker threads by concurrency settings
// pop job from chan queue by blpop and execute by runner, and send result to chan
#[async_trait]
pub trait ChanJobDispatcher:
    UseChanJobQueueRepository
    + UseRdbChanJobRepository
    // + UseSubscribeWorker
    + JobRunner
    + UseJobProcessingStatusRepository
    + UseRunnerPoolMap
    + UseResultProcessor
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
    + UseIdGenerator
    + infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository
    + JobDispatcher
{
    fn chan_worker_pubsub_repository(&self) -> &ChanWorkerPubSubRepositoryImpl;

    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        // subscribe worker change events for runner pool release in standalone mode
        tokio::spawn(async move {
            if let Err(e) = self.subscribe_worker_changed_chan().await {
                tracing::error!("subscribe worker changed (chan) error: {:?}", e);
            }
        });
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
                "create chan job dispatcher for channel {}, concurrency: {}",
                &ch,
                &conc
            );
            for _ in 0..conc {
                self.pop_and_execute(ch.clone(), lock.clone(), recv.clone());
            }
        }
        lock.unlock();
        tracing::debug!("channel job dispatcher started");
        Ok(())
    }

    /// Subscribe to worker change events via BroadcastChan and release runner pools accordingly.
    /// Mirrors the Redis-based `subscribe_worker_changed()` in `UseSubscribeWorker`.
    async fn subscribe_worker_changed_chan(&'static self) -> Result<bool>
    where
        Self: Send + Sync + 'static,
    {
        let mut receiver = self.chan_worker_pubsub_repository().subscribe().await;

        let (send, mut recv) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            command_utils::util::shutdown::shutdown_signal().await;
            tracing::debug!("got shutdown signal (worker pubsub chan)");
            if let Err(e) = send.send(true) {
                tracing::debug!("failed to send shutdown notification: {:?}", e);
            }
        });

        loop {
            tokio::select! {
                _ = recv.changed() => {
                    tracing::debug!("got shutdown signal in subscribe_worker_changed_chan");
                    break;
                },
                val = receiver.recv() => {
                    match val {
                        Ok(payload) => {
                            match ProstMessageCodec::deserialize_message::<Worker>(&payload) {
                                Ok(worker) => {
                                    tracing::info!("subscribe_worker_changed_chan: worker changed: {:?}", worker);
                                    if let Some(wid) = worker.id.as_ref() {
                                        self.runner_pool_map().delete_runner(wid).await;
                                    } else if worker.data.is_none() {
                                        // id=None, data=None means delete all
                                        self.runner_pool_map().clear().await;
                                    }
                                    let _ = self.worker_app()
                                        .clear_cache_by(worker.id.as_ref(), worker.data.as_ref().map(|d| &d.name))
                                        .await
                                        .inspect_err(|e| tracing::error!("cache clear error: {:?}", e));
                                }
                                Err(e) => {
                                    tracing::error!("deserialize worker in chan subscribe: {:?}", e);
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            // Lost messages may include worker deletions, so clear all pools
                            // to prevent runner pool leaks from missed notifications.
                            // Pools are lazily re-created by get_or_create_static_runner().
                            tracing::warn!("worker pubsub chan lagged by {} messages, clearing all runner pools", n);
                            self.runner_pool_map().clear().await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::info!("worker pubsub chan closed");
                            break;
                        }
                    }
                }
            }
            if *recv.borrow() {
                break;
            }
        }
        tracing::info!("subscribe_worker_changed_chan end");
        Ok(true)
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
            tracing::debug!("chan pop_and_execute: start job loop for channel {}", &cn);
            'outer: loop {
                tracing::debug!("start loop of spawned job queue {}", &cn);
                tokio::select! {
                    // break in shutdown for blpop wait
                    // cannot handle signal when running blocking job with plugin runner (external lib etc)
                    _ = shutdown_recv.changed() => {
                        tracing::debug!("got sigint signal.... channel {}", &cn);
                        break 'outer;
                    },
                    val = self.chan_job_queue_repository().receive_job_from_channels(c.clone()) => {
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
                // shutdown received (not selected case)
                if *shutdown_recv.borrow() {
                    break 'outer;
                }
            }
            tracing::info!("exit chan job loop for channel {}", cn);
            lock.unlock();
            Result::Ok(())
        })
    }
    #[inline]
    async fn process_deque_job(&'static self, val: Result<Job>) -> Result<JobResult>
    where
        Self: Sync + Send + 'static,
    {
        match val {
            Ok(job) => {
                let job_id = job.id;
                match self.process_job(job).await {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        // Check if status should be deleted based on error type
                        if let Some(jid) = job_id
                            && super::should_cleanup_status_on_error(&e) {
                                self.cleanup_failed_job_status(&jid, "memory").await;
                            }
                        Err(e)
                    }
                }
            }
            Err(e) => {
                tracing::error!("pop job error: {:?}", e);
                Err(JobWorkerError::ChanError(e).into())
            }
        }
    }

    #[inline]
    async fn process_job(&'static self, job: Job) -> Result<JobResult>
    where
        Self: Sync + Send + 'static,
    {
        tracing::debug!("process pop-ed job: {:?}", job);
        let (jid, jdat,metadata) = if let Job {
            id: Some(jid),
            data: Some(jdat),
            metadata
        } = job {
           (jid, jdat, metadata)
        } else {
            // Status cleanup is handled by process_deque_job based on error type
            let mes = format!("job {:?} is incomplete data.", &job);
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
        let sid = if let Some(id) = wdat.runner_id.as_ref() {
            id
        } else {
            // Status cleanup is handled by process_deque_job based on error type
            let mes = format!(
                "worker {:?} runner_id is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            return Err(JobWorkerError::NotFound(mes).into());
        };
        let runner_data = if let Some(RunnerWithSchema{id:_, data: runner_data,..}) =
             self.runner_app().find_runner(sid).await?
        {
                runner_data.ok_or(JobWorkerError::NotFound(format!("runner data {:?} is not found.", &sid)))
        } else {
            // Status cleanup is handled by process_deque_job based on error type
            let mes = format!(
                "runner data {:?} is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            Err(JobWorkerError::NotFound(mes))
        }?;

        if let Some(cancelled_result) = self
            .check_cancellation_status(&jid, &wid, &wdat, metadata.clone(), &jdat)
            .await?
        {
            return self.result_processor().process_result(cancelled_result, None, wdat).await;
        }

        if wdat.response_type != ResponseType::Direct as i32
            && wdat.queue_type == QueueType::WithBackup as i32
        {
            // grab job in db (only for record as in progress)
            if self.rdb_job_repository()
                .grab_job(
                    &jid,
                    Some(jdat.timeout),
                    jdat.grabbed_until_time.unwrap_or(0),
                )
                .await?
            {
                // change status to running
                self.job_processing_status_repository()
                    .upsert_status(&jid, &JobProcessingStatus::Running)
                    .await?;

                // Index RUNNING status in RDB (if enabled) with full metadata including start_time
                if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                    let channel = wdat.channel.clone().unwrap_or_default();
                    let priority = jdat.priority;
                    let enqueue_time = jdat.enqueue_time;
                    let is_streamable = jdat.streaming_type != 0;
                    let broadcast_results = wdat.broadcast_results;

                    if let Err(e) = index_repo
                        .index_status(
                            &jid,
                            &JobProcessingStatus::Running,
                            &wid,
                            &channel,
                            priority,
                            enqueue_time,
                            is_streamable,
                            broadcast_results,
                        )
                        .await
                    {
                        tracing::warn!(
                            "Failed to index RUNNING status in RDB for job {}: {:?}",
                            jid.value,
                            e
                        );
                    }
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
        } else {
            // change status to running
            self.job_processing_status_repository()
                .upsert_status(&jid, &JobProcessingStatus::Running)
                .await?;

            // Index RUNNING status in RDB (if enabled) with full metadata including start_time
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
                let channel = wdat.channel.clone().unwrap_or_default();
                let priority = jdat.priority;
                let enqueue_time = jdat.enqueue_time;
                let is_streamable = jdat.streaming_type != 0;
                let broadcast_results = wdat.broadcast_results;

                if let Err(e) = index_repo
                    .index_status(
                        &jid,
                        &JobProcessingStatus::Running,
                        &wid,
                        &channel,
                        priority,
                        enqueue_time,
                        is_streamable,
                        broadcast_results,
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to index RUNNING status in RDB for job {}: {:?}",
                        jid.value,
                        e
                    );
                }
            }
        }

        // Save job metadata before run_job() consumes jdat
        let channel_for_indexing = wdat.channel.clone().unwrap_or_default();
        let priority_for_indexing = jdat.priority;
        let enqueue_time_for_indexing = jdat.enqueue_time;
        let is_streamable_for_indexing = jdat.streaming_type != 0;
        let broadcast_results_for_indexing = wdat.broadcast_results;

        // run job
        let r = self
                .run_job(
                    &runner_data,
                    &wid,
                    &wdat,
                    Job {
                        id: Some(jid),
                        data: Some(jdat),
                        metadata
                    },
                )
                .await;
            // TODO execute and return result to result channel.
            tracing::trace!("send result id: {:?}, data: {:?}", &r.0.id, &r.0.data);
            // change status to wait handling result
            self.job_processing_status_repository()
                .upsert_status(&jid, &JobProcessingStatus::WaitResult)
                .await?;

            // Index WAIT_RESULT status in RDB (if enabled) with full metadata
            if let Some(index_repo) = self.rdb_job_processing_status_index_repository()
                && let Err(e) = index_repo
                    .index_status(
                        &jid,
                        &JobProcessingStatus::WaitResult,
                        &wid,
                        &channel_for_indexing,
                        priority_for_indexing,
                        enqueue_time_for_indexing,
                        is_streamable_for_indexing,
                        broadcast_results_for_indexing,
                    )
                    .await
                {
                    tracing::warn!(
                        "Failed to index WAIT_RESULT status in RDB for job {}: {:?}",
                        jid.value,
                        e
                    );
                }

            self.result_processor().process_result(r.0, r.1, wdat).await
    }
}

#[derive()]
pub struct ChanJobDispatcherImpl {
    id_generator: Arc<IdGeneratorWrapper>,
    job_queue_repository: Arc<ChanJobQueueRepositoryImpl>,
    rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
    job_processing_status_repository: Arc<MemoryJobProcessingStatusRepository>,
    rdb_job_processing_status_index_repository:
        Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>>,
    chan_worker_pubsub_repository: ChanWorkerPubSubRepositoryImpl,
    app_module: Arc<AppModule>,
    runner_factory: Arc<RunnerFactory>,
    runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
}

impl ChanJobDispatcherImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        chan_job_queue_repository: Arc<ChanJobQueueRepositoryImpl>,
        rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
        job_processing_status_repository: Arc<MemoryJobProcessingStatusRepository>,
        rdb_job_processing_status_index_repository: Option<
            Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>,
        >,
        chan_worker_pubsub_repository: ChanWorkerPubSubRepositoryImpl,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Self {
        Self {
            id_generator,
            job_queue_repository: chan_job_queue_repository,
            rdb_job_repository,
            job_processing_status_repository,
            rdb_job_processing_status_index_repository,
            chan_worker_pubsub_repository,
            app_module,
            runner_factory,
            runner_pool_map,
            result_processor,
        }
    }
}

impl UseChanJobQueueRepository for ChanJobDispatcherImpl {
    fn chan_job_queue_repository(&self) -> &ChanJobQueueRepositoryImpl {
        &self.job_queue_repository
    }
}
impl UseJobProcessingStatusRepository for ChanJobDispatcherImpl {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.job_processing_status_repository.clone()
    }
}

impl jobworkerp_base::codec::UseProstCodec for ChanJobDispatcherImpl {}
impl UseJobqueueAndCodec for ChanJobDispatcherImpl {}

impl UseWorkerApp for ChanJobDispatcherImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

impl RunnerResultHandler for ChanJobDispatcherImpl {}
impl UseRunnerPoolMap for ChanJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
impl Tracing for ChanJobDispatcherImpl {}
impl JobRunner for ChanJobDispatcherImpl {}

impl UseIdGenerator for ChanJobDispatcherImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}

impl UseWorkerConfig for ChanJobDispatcherImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_module.config_module.worker_config
    }
}

impl UseRdbChanJobRepository for ChanJobDispatcherImpl {
    fn rdb_job_repository(&self) -> &RdbChanJobRepositoryImpl {
        &self.rdb_job_repository
    }
}
impl UseRunnerApp for ChanJobDispatcherImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.app_module.runner_app.clone()
    }
}
impl ChanJobDispatcher for ChanJobDispatcherImpl {
    fn chan_worker_pubsub_repository(&self) -> &ChanWorkerPubSubRepositoryImpl {
        &self.chan_worker_pubsub_repository
    }
}
impl UseRunnerFactory for ChanJobDispatcherImpl {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}
impl UseJobQueueConfig for ChanJobDispatcherImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.app_module.config_module.job_queue_config
    }
}

impl UseResultProcessor for ChanJobDispatcherImpl {
    fn result_processor(&self) -> &ResultProcessorImpl {
        &self.result_processor
    }
}

impl infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository
    for ChanJobDispatcherImpl
{
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<std::sync::Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>>
    {
        self.rdb_job_processing_status_index_repository.clone()
    }
}

#[async_trait]
impl JobDispatcher for ChanJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        ChanJobDispatcher::dispatch_jobs(self, lock)
    }
}

// create test for chan dispatcher
