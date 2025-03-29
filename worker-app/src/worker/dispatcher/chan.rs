use super::JobDispatcher;
use crate::worker::result_processor::{ResultProcessorImpl, UseResultProcessor};
use crate::worker::runner::map::{RunnerFactoryWithPoolMap, UseRunnerPoolMap};
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::runner::JobRunner;
use anyhow::Result;
use app::app::runner::{RunnerApp, UseRunnerApp};
use app::app::worker::{UseWorkerApp, WorkerApp};
use app::app::{UseWorkerConfig, WorkerConfig};
use app::module::AppModule;
use app_wrapper::runner::{RunnerFactory, UseRunnerFactory};
use async_trait::async_trait;
use command_utils::util::option::ToResult;
use command_utils::util::result::TapErr;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::job::queue::chan::{
    ChanJobQueueRepository, ChanJobQueueRepositoryImpl, UseChanJobQueueRepository,
};
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::rdb::{RdbChanJobRepositoryImpl, RdbJobRepository, UseRdbChanJobRepository};
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::memory::MemoryJobStatusRepository;
use infra::infra::job::status::{JobStatusRepository, UseJobStatusRepository};
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig, UseIdGenerator, UseJobQueueConfig};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    Job, JobResult, JobResultId, JobStatus, Priority, QueueType, ResponseType, Worker,
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
    + UseJobStatusRepository
    + UseRunnerPoolMap
    + UseResultProcessor
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
    + UseIdGenerator
{
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        // TODO
        // // create a tokio thread to subscribe update worker event and update worker map
        // tokio::spawn(
        //     self.subscribe_worker_changed()
        //         .map_err(|e| tracing::error!("subscribe worker changed error: {:?}", e)),
        // );
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
            Result::<()>::Ok(())
        })
    }
    #[inline]
    async fn process_deque_job(&'static self, val: Result<Job>) -> Result<JobResult>
    where
        Self: Sync + Send + 'static,
    {
        match val {
            Ok(job) => self.process_job(job).await,
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
        let (jid, jdat) = if let Job {
            id: Some(jid),
            data: Some(jdat),
        } = job {
           (jid, jdat)
        } else {
            // TODO cannot return result in this case. send result as error?
            let mes = format!("job {:?} is incomplete data.", &job);
            tracing::error!("{}", &mes);
            if let Some(id) = job.id.as_ref() {
                self.job_status_repository().delete_status(id).await?;
                self.rdb_job_repository().delete(id).await?;
            }
            return Err(JobWorkerError::OtherError(mes).into());
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
            // TODO cannot return result in this case. send result as error?
            let mes = format!(
                "worker {:?} is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            self.job_status_repository().delete_status(&jid).await?;
            self.rdb_job_repository().delete(&jid).await?;
            return Err(JobWorkerError::NotFound(mes).into());
        };
        let sid = if let Some(id) = wdat.runner_id.as_ref() {
            id
        } else {
            // TODO cannot return result in this case. send result as error?
            let mes = format!(
                "worker {:?} runner_id is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            self.job_status_repository().delete_status(&jid).await?;
            self.rdb_job_repository().delete(&jid).await?;
            return Err(JobWorkerError::NotFound(mes).into());
        };
        let runner_data = if let Some(RunnerWithSchema{id:_, data: runner_data,..}) =
             self.runner_app().find_runner(sid, None).await?
        {
                runner_data.to_result(||JobWorkerError::NotFound(format!("runner data {:?} is not found.", &sid)))
        } else {
            // TODO cannot return result in this case. send result as error?
            let mes = format!(
                "runner data {:?} is not found.",
                jdat.worker_id.as_ref().unwrap()
            );
            tracing::error!("{}", &mes);
            self.job_status_repository().delete_status(&jid).await?;
            self.rdb_job_repository().delete(&jid).await?;
            Err(JobWorkerError::NotFound(mes))
        }?;
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
                        self.job_status_repository()
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
            } else {
                // change status to running
                self.job_status_repository()
                    .upsert_status(&jid, &JobStatus::Running)
                    .await?;
            }
            // run job
            let r = self
                .run_job(
                    &runner_data,
                    &wid,
                    &wdat,
                    Job {
                        id: Some(jid),
                        data: Some(jdat),
                    },
                )
                .await;
            let id = JobResultId {
                value: self.id_generator().generate_id()?,
            };
            // TODO execute and return result to result channel.
            tracing::trace!("send result id: {:?}, data: {:?}", id, &r.0);
            // change status to wait handling result
            if wdat.response_type != ResponseType::Direct as i32 {
                self.job_status_repository()
                    .upsert_status(&jid, &JobStatus::WaitResult)
                    .await?;
            }
            self.result_processor().process_result(id, r, wdat).await
    }
}

#[derive()]
pub struct ChanJobDispatcherImpl {
    id_generator: Arc<IdGeneratorWrapper>,
    job_queue_repository: Arc<ChanJobQueueRepositoryImpl>,
    rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
    job_status_repository: Arc<MemoryJobStatusRepository>,
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
        job_status_repository: Arc<MemoryJobStatusRepository>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Self {
        Self {
            id_generator,
            job_queue_repository: chan_job_queue_repository,
            rdb_job_repository,
            job_status_repository,
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
impl UseJobStatusRepository for ChanJobDispatcherImpl {
    fn job_status_repository(&self) -> Arc<dyn JobStatusRepository> {
        self.job_status_repository.clone()
    }
}

impl UseJobqueueAndCodec for ChanJobDispatcherImpl {}

impl UseWorkerApp for ChanJobDispatcherImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

// impl UseSubscribeWorker for ChanJobDispatcherImpl {}
impl RunnerResultHandler for ChanJobDispatcherImpl {}
impl UseRunnerPoolMap for ChanJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
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
impl ChanJobDispatcher for ChanJobDispatcherImpl {}
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

impl JobDispatcher for ChanJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        ChanJobDispatcher::dispatch_jobs(self, lock)
    }
}

// create test for chan dispatcher
