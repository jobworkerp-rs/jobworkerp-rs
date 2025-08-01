use super::JobDispatcher;
use crate::worker::result_processor::ResultProcessorImpl;
use crate::worker::result_processor::UseResultProcessor;
use crate::worker::runner::map::RunnerFactoryWithPoolMap;
use crate::worker::runner::map::UseRunnerPoolMap;
use crate::worker::runner::result::RunnerResultHandler;
use crate::worker::runner::JobRunner;
use anyhow::Result;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::runner::RunnerApp;
use app::app::runner::UseRunnerApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::module::AppConfigModule;
use app::module::AppModule;
use app_wrapper::runner::RunnerFactory;
use app_wrapper::runner::UseRunnerFactory;
use async_trait::async_trait;
use command_utils::util::datetime;
use command_utils::util::shutdown::ShutdownLock;
use futures::stream;
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::rdb::RdbChanJobRepositoryImpl;
use infra::infra::job::rdb::UseRdbChanJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::runner::rows::RunnerWithSchema;
use infra::infra::IdGeneratorWrapper;
use infra::infra::JobQueueConfig;
use infra::infra::UseIdGenerator;
use infra::infra::UseJobQueueConfig;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::Job;
use proto::jobworkerp::data::JobResult;
use proto::jobworkerp::data::Worker;
use proto::jobworkerp::data::WorkerId;
use std::sync::Arc;
use std::time::Duration;

// for rdb run_after, periodic job dispatching
#[async_trait]
pub trait RdbJobDispatcher:
    UseJobResultApp
    + UseIdGenerator
    + UseRdbChanJobRepository
    + UseResultProcessor
    + JobRunner
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
{
    // mergin time to re-execute if it does not disappear from queue (row) after timeout
    const GRAB_MERGIN_MILLISEC: i64 = infra::infra::job::queue::rdb::GRAB_MERGIN_MILLISEC;

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
                        tracing::trace!("execute pop and enqueue run_after job");
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
        use futures::StreamExt;
        tracing::trace!("run pop_and_execute: time:{}", datetime::now().to_rfc3339());
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
                        .inspect_err(|e| {
                            tracing::error!("failed to find worker_ids_by_channel: {:?}", e)
                        })
                        .unwrap_or(vec![]);
                    tracing::trace!("pop and execute: worker_ids:{}: {:?}", &ch, &worker_ids);
                    if worker_ids.is_empty() {
                        tracing::trace!("pop and execute: no worker_ids: {:?}", &ch);
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
                        .inspect_err(|e| tracing::error!("failed to fetch jobs: {:?}", e))
                        .unwrap_or(vec![]); // skip if failed to fetch jobs

                    if !jobs.is_empty() {
                        tracing::debug!("pop and execute: jobs: ch={}: jobs={:?}", &ch, &jobs);
                    }
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
        let wid = job.data.as_ref().and_then(|d| d.worker_id.as_ref());
        // get worker
        let (wid, w) = if let Some(Worker {
            id: Some(wid),
            data: Some(w),
        }) = self.worker_app().find_by_opt(wid).await?
        {
            (wid, w)
        } else {
            tracing::error!("failed to get worker: {:?}", &job);
            return Err(
                JobWorkerError::NotFound(format!("failed to get worker: {:?}", &job)).into(),
            );
        };
        let rid = if let Some(id) = w.runner_id.as_ref() {
            id
        } else {
            tracing::error!("failed to get runner_id: {:?}", &job);
            return Err(
                JobWorkerError::NotFound(format!("failed to get runner_id: {:?}", &job)).into(),
            );
        };
        let runner_data = if let Some(RunnerWithSchema {
            id: _,
            data: runner_data,
            ..
        }) = self.runner_app().find_runner(rid).await?
        {
            runner_data.ok_or(JobWorkerError::NotFound(format!(
                "runner data {:?} is not found.",
                &rid
            )))
        } else {
            tracing::error!("failed to get runner data for job: {:?}", &job);
            Err(JobWorkerError::NotFound(format!(
                "failed to get runner data for job: {:?}",
                &job
            )))
        }?;

        // time millis to re-execute if the job does not disappear from queue (row) after a while after timeout(GRAB_MERGIN_MILLISEC)
        match self
            .rdb_job_repository()
            .grab_job(
                job.id.as_ref().unwrap(), // checked is_some
                job.data.as_ref().map(|d| d.timeout),
                job.data
                    .as_ref()
                    .and_then(|d| d.grabbed_until_time)
                    .unwrap_or(0),
            )
            .await
        {
            Ok(grabbed) => {
                if grabbed {
                    let res = self.run_job(&runner_data, &wid, &w, job).await;
                    tracing::debug!("job completed. result: {:?}", &res.0);
                    // store result
                    self.result_processor()
                        .process_result(res.0, res.1, w)
                        .await
                        .inspect_err(|e| {
                            tracing::error!(
                                "failed to process result: worker_id={:?}, err={:?}",
                                &wid,
                                e
                            )
                        })
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
    }
}

pub struct RdbJobDispatcherImpl {
    id_generator: Arc<IdGeneratorWrapper>,
    job_queue_config: Arc<JobQueueConfig>,
    rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
    app_module: Arc<AppModule>,
    runner_factory: Arc<RunnerFactory>,
    runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    result_processor: Arc<ResultProcessorImpl>,
}

impl RdbJobDispatcherImpl {
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Self {
        Self {
            id_generator,
            job_queue_config: config_module.job_queue_config.clone(),
            rdb_job_repository,
            app_module,
            runner_factory,
            runner_pool_map,
            result_processor,
        }
    }
}

impl UseRdbChanJobRepository for RdbJobDispatcherImpl {
    fn rdb_job_repository(&self) -> &RdbChanJobRepositoryImpl {
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
impl UseRunnerApp for RdbJobDispatcherImpl {
    fn runner_app(&self) -> Arc<dyn RunnerApp> {
        self.app_module.runner_app.clone()
    }
}

impl UseJobqueueAndCodec for RdbJobDispatcherImpl {}
impl UseRunnerFactory for RdbJobDispatcherImpl {
    fn runner_factory(&self) -> &RunnerFactory {
        &self.runner_factory
    }
}

impl RunnerResultHandler for RdbJobDispatcherImpl {}

impl UseRunnerPoolMap for RdbJobDispatcherImpl {
    fn runner_pool_map(&self) -> &RunnerFactoryWithPoolMap {
        &self.runner_pool_map
    }
}
impl Tracing for RdbJobDispatcherImpl {}
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

impl infra::infra::job::status::UseJobProcessingStatusRepository for RdbJobDispatcherImpl {
    fn job_processing_status_repository(
        &self,
    ) -> Arc<dyn infra::infra::job::status::JobProcessingStatusRepository> {
        // RdbJobDispatcher typically doesn't use job processing status, hence dummy implementation
        // If actual status needed, retrieve appropriate repository from app_module
        Arc::new(infra::infra::job::status::memory::MemoryJobProcessingStatusRepository::new())
    }
}

impl RdbJobDispatcher for RdbJobDispatcherImpl {}

#[async_trait]
impl JobDispatcher for RdbJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RdbJobDispatcher::dispatch_jobs(self, lock)
    }
}
