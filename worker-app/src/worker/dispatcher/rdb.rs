use crate::plugins::runner::UsePluginRunner;
use crate::plugins::Plugins;
use crate::worker::result_processor::ResultProcessorImpl;
use crate::worker::result_processor::UseResultProcessor;
use crate::worker::runner::map::RunnerFactoryWithPoolMap;
use crate::worker::runner::map::UseRunnerPoolMap;
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::runner::JobRunner;
use anyhow::Result;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::module::AppConfigModule;
use app::module::AppModule;
use async_trait::async_trait;
use common::util::datetime;
use common::util::option::FlatMap;
use common::util::result::TapErr;
use common::util::shutdown::ShutdownLock;
use futures::{stream, StreamExt};
use infra::error::JobWorkerError;
use infra::infra::job::rdb::queue::RdbJobQueueRepository;
use infra::infra::job::rdb::RdbJobRepositoryImpl;
use infra::infra::job::rdb::UseRdbJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::IdGeneratorWrapper;
use infra::infra::JobQueueConfig;
use infra::infra::UseIdGenerator;
use infra::infra::UseJobQueueConfig;
use libloading::Library;
use proto::jobworkerp::data::Job;
use proto::jobworkerp::data::JobResult;
use proto::jobworkerp::data::JobResultId;
use proto::jobworkerp::data::Worker;
use proto::jobworkerp::data::WorkerId;
use std::sync::Arc;
use std::time::Duration;

use super::JobDispatcher;

// for rdb run_after, periodic job dispatching
#[async_trait]
pub trait RdbJobDispatcher:
    UseJobResultApp
    + UseIdGenerator
    + UseRdbJobRepository
    + UseResultProcessor
    + JobRunner
    + UseWorkerConfig
    + UseWorkerApp
    + UseJobQueueConfig
{
    // mergin time to re-execute if it does not disappear from queue (row) after timeout
    const GRAB_MERGIN_MILLISEC: i64 = infra::infra::job::rdb::queue::GRAB_MERGIN_MILLISEC;

    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        let pairs = self.worker_config().channel_concurrency_pair();
        tracing::debug!("start dispatch jobs by rdb. workers and conc: {:?}", &pairs);
        // TODO
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(
                self.job_queue_config().fetch_interval as u64,
            ));
            loop {
                // using tokio::select and tokio::signal::ctrl_c, break loop by ctrl-c
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::debug!("execute pop and enqueue run_after job");
                        let _ = self.pop_and_execute(pairs.clone()).await.map_err(|e| {
                            tracing::error!("failed to pop and enqueue: {:?}", e);
                            e
                        });
                    }
                    _ = tokio::signal::ctrl_c() => {
                        tracing::debug!("break execute pop and enqueue run_after job");
                        lock.unlock();
                        break;
                    }
                }
            }
        });
        tracing::debug!("end execute pop and enqueue run_after job");
        Ok(())
    }

    // pop jobs using pop_run_after_jobs_to_run(), and enqueue them to redis for execute
    async fn pop_and_execute(&'static self, pairs: Vec<(String, u32)>) -> Result<()> {
        tracing::debug!("run pop_and_execute: time:{}", datetime::now().to_rfc3339());
        // thread to return to continue fetching
        let pairs_len = pairs.len();
        stream::iter(pairs)
            .map(|(ch, conc)| {
                // threads per channel (from config)
                tokio::spawn(async move {
                    let worker_ids: Vec<WorkerId> = self // cache worker_ids of channel?
                        .worker_app()
                        .find_worker_ids_by_channel(&ch)
                        .await
                        .tap_err(|e| {
                            tracing::error!("failed to find worker_ids_by_channel: {:?}", e)
                        })
                        .unwrap_or(vec![]);
                    tracing::debug!("pop and execute: worker_ids:{}: {:?}", &ch, &worker_ids);
                    if worker_ids.is_empty() {
                        tracing::debug!("pop and execute: no worker_ids: {:?}", &ch);
                        return;
                    }
                    let jobs = self
                        .rdb_job_repository()
                        .fetch_jobs_to_process(
                            0,
                            conc,
                            worker_ids,
                            self.job_queue_config().fetch_interval,
                            true,
                        )
                        .await
                        .tap_err(|e| tracing::error!("failed to fetch jobs: {:?}", e))
                        .unwrap_or(vec![]); // skip if failed to fetch jobs
                    tracing::debug!("pop and execute: fetched jobs:{}: {:?}", &ch, jobs);
                    // cunc threads for each channel
                    stream::iter(jobs)
                        .map(|job| {
                            // spawn async task for each job
                            tokio::spawn(async move { self._process_job(job).await })
                        })
                        .buffered(conc as usize)
                        .collect::<Vec<_>>()
                        .await;
                })
            })
            .buffered(pairs_len) // concurrent per channel ((additional channel + default channel) x concurrency)
            .collect::<Vec<_>>()
            .await;
        Ok(())
    }
    async fn _process_job(&'static self, job: Job) -> Result<Option<JobResult>> {
        if job.id.is_none() || job.data.is_none() {
            return Err(JobWorkerError::InvalidParameter(format!(
                "job data is strange: {:?}",
                &job
            ))
            .into());
        }
        let wid = job.data.as_ref().flat_map(|d| d.worker_id.as_ref());
        // get worker
        if let Some(Worker {
            id: Some(wid),
            data: Some(w),
        }) = self.worker_app().find_by_opt(wid).await?
        {
            // time millis to re-execute if the job does not disappear from queue (row) after a while after timeout(GRAB_MERGIN_MILLISEC)
            match self
                .rdb_job_repository()
                .grab_job(
                    job.id.as_ref().unwrap(), // cheched is_some
                    job.data.as_ref().map(|d| d.timeout),
                    job.data
                        .as_ref()
                        .flat_map(|d| d.grabbed_until_time)
                        .unwrap_or(0),
                )
                .await
            {
                Ok(grabbed) => {
                    if grabbed {
                        let res = self.run_job(&wid, &w, job).await;
                        let id = JobResultId {
                            value: self.id_generator().generate_id()?,
                        };
                        tracing::debug!("job completed. result: {:?}", &res);
                        // store result
                        self.result_processor()
                            .process_result(id, res, w)
                            .await
                            .map(Some)
                    } else {
                        tracing::debug!("failed to grab job: {:?}", job.data);
                        Ok(None)
                    }
                }
                Err(e) => {
                    tracing::error!("error in grab job: {:?}", e);
                    Err(e)
                }
            }
        } else {
            tracing::error!("failed to get worker: {:?}", &job);
            Err(JobWorkerError::NotFound(format!("failed to get worker: {:?}", &job)).into())
        }
    }
}

pub struct RdbJobDispatcherImpl {
    id_generator: Arc<IdGeneratorWrapper>,
    job_queue_config: Arc<JobQueueConfig>,
    rdb_job_repository: Arc<RdbJobRepositoryImpl>,
    app_module: Arc<AppModule>,
    plugins: Arc<Plugins>,
    runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
}

impl RdbJobDispatcherImpl {
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        rdb_job_repository: Arc<RdbJobRepositoryImpl>,
        app_module: Arc<AppModule>,
        plugins: Arc<Plugins>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Self {
        Self {
            id_generator,
            job_queue_config: config_module.job_queue_config.clone(),
            rdb_job_repository,
            app_module,
            plugins,
            runner_pool_map,
            result_processor,
        }
    }
}

impl UseRdbJobRepository for RdbJobDispatcherImpl {
    fn rdb_job_repository(&self) -> &RdbJobRepositoryImpl {
        &self.rdb_job_repository
    }
}
impl UseJobResultApp for RdbJobDispatcherImpl {
    fn job_result_app(&self) -> &Arc<dyn JobResultApp + 'static> {
        &self.app_module.job_result_app
    }
}
impl UseWorkerApp for RdbJobDispatcherImpl {
    fn worker_app(&self) -> &Arc<dyn WorkerApp + 'static> {
        &self.app_module.worker_app
    }
}

impl UseJobqueueAndCodec for RdbJobDispatcherImpl {}
impl UsePluginRunner for RdbJobDispatcherImpl {
    fn runner_plugins(&self) -> &Vec<(String, Library)> {
        self.plugins.runner_plugins()
    }
}

impl RunnerResultHandler for RdbJobDispatcherImpl {}

impl UseRunnerPoolMap for RdbJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
impl JobRunner for RdbJobDispatcherImpl {}

impl UseWorkerConfig for RdbJobDispatcherImpl {
    fn worker_config(&self) -> &WorkerConfig {
        &self.app_module.config_module.worker_config
    }
}

impl UseJobQueueConfig for RdbJobDispatcherImpl {
    fn job_queue_config(&self) -> &JobQueueConfig {
        &self.job_queue_config
    }
}
impl UseIdGenerator for RdbJobDispatcherImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        &self.id_generator
    }
}
impl UseResultProcessor for RdbJobDispatcherImpl {
    fn result_processor(&self) -> &ResultProcessorImpl {
        &self.result_processor
    }
}

impl RdbJobDispatcher for RdbJobDispatcherImpl {}

impl JobDispatcher for RdbJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RdbJobDispatcher::dispatch_jobs(self, lock)
    }
}
