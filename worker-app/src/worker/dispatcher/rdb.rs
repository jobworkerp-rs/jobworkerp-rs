use super::JobDispatcher;
use crate::worker::instance_session::{UseWorkerInstanceSession, WorkerInstanceSessionHandle};
use crate::worker::result_processor::ResultProcessorImpl;
use crate::worker::result_processor::UseResultProcessor;
use crate::worker::runner::JobRunner;
use crate::worker::runner::map::RunnerFactoryWithPoolMap;
use crate::worker::runner::map::UseRunnerPoolMap;
use crate::worker::runner::result::RunnerResultHandler;
use anyhow::Result;
use app::app::UseWorkerConfig;
use app::app::WorkerConfig;
use app::app::job_result::JobResultApp;
use app::app::job_result::UseJobResultApp;
use app::app::runner::RunnerApp;
use app::app::runner::UseRunnerApp;
use app::app::worker::UseWorkerApp;
use app::app::worker::WorkerApp;
use app::module::AppConfigModule;
use app::module::AppModule;
use app_wrapper::runner::RunnerFactory;
use app_wrapper::runner::UseRunnerFactory;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use command_utils::util::datetime;
use command_utils::util::shutdown::ShutdownLock;
use futures::stream;
use infra::infra::IdGeneratorWrapper;
use infra::infra::JobQueueConfig;
use infra::infra::UseIdGenerator;
use infra::infra::UseJobQueueConfig;
use infra::infra::job::queue::rdb::RdbJobQueueRepository;
use infra::infra::job::rdb::RdbChanJobRepositoryImpl;
use infra::infra::job::rdb::UseRdbChanJobRepository;
use infra::infra::job::rows::UseJobqueueAndCodec;
use infra::infra::job::status::execution::{RdbDispatchStart, RdbJobStatusExecutionRepository};
use infra::infra::job::status::{
    JobProcessingStatusRecord, JobProcessingStatusRepository, StatusTransitionResult,
};
use infra::infra::runner::rows::RunnerWithSchema;
use infra_utils::infra::rdb::UseRdbPool;
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::Job;
use proto::jobworkerp::data::JobId;
use proto::jobworkerp::data::JobProcessingStatus;
use proto::jobworkerp::data::JobResult;
use proto::jobworkerp::data::Worker;
use proto::jobworkerp::data::WorkerId;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnstartedRdbDispatchRestoreOutcome {
    Restored,
    CancellationWon,
    OwnershipLost,
}

async fn restore_pending_after_unstarted_rdb_dispatch(
    repository: Arc<dyn JobProcessingStatusRepository>,
    job_id: &JobId,
    retried: u32,
) -> Result<UnstartedRdbDispatchRestoreOutcome> {
    let running = JobProcessingStatusRecord {
        status: JobProcessingStatus::Running,
        retried,
    };
    let pending = JobProcessingStatusRecord {
        status: JobProcessingStatus::Pending,
        retried,
    };
    match repository
        .compare_and_set_status(job_id, Some(running), Some(pending))
        .await?
    {
        StatusTransitionResult::Applied => Ok(UnstartedRdbDispatchRestoreOutcome::Restored),
        StatusTransitionResult::Conflict(Some(record))
            if record.status == JobProcessingStatus::Cancelling && record.retried == retried =>
        {
            Ok(UnstartedRdbDispatchRestoreOutcome::CancellationWon)
        }
        StatusTransitionResult::Conflict(_) => {
            Ok(UnstartedRdbDispatchRestoreOutcome::OwnershipLost)
        }
    }
}

// for rdb run_after, periodic job dispatching
#[async_trait]
pub trait RdbJobDispatcher:
    JobDispatcher
    + UseJobResultApp
    + UseIdGenerator
    + UseRdbChanJobRepository
    + UseResultProcessor
    + JobRunner
    + UseWorkerConfig
    + UseWorkerApp
    + UseRunnerApp
    + UseJobQueueConfig
    + UseWorkerInstanceSession
{
    // mergin time to re-execute if it does not disappear from queue (row) after timeout
    const GRAB_MERGIN_MILLISEC: i64 = infra::infra::job::queue::rdb::GRAB_MERGIN_MILLISEC;

    /// Releases the live claim when this worker cannot begin a runner.
    async fn restore_or_finalize_unstarted_rdb_dispatch(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &proto::jobworkerp::data::WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<JobResult>> {
        match restore_pending_after_unstarted_rdb_dispatch(
            self.job_processing_status_repository(),
            job_id,
            job_data.retried,
        )
        .await?
        {
            UnstartedRdbDispatchRestoreOutcome::Restored => Ok(None),
            UnstartedRdbDispatchRestoreOutcome::CancellationWon => {
                match self
                    .check_rdb_cancellation_status(
                        job_id,
                        worker_id,
                        worker_data,
                        job_metadata,
                        job_data,
                    )
                    .await?
                {
                    super::DispatchEligibility::Cancelled(result) => self
                        .process_cancelled_dispatch_result(*result, worker_data, job_id)
                        .await
                        .map(Some),
                    super::DispatchEligibility::Skip => Ok(None),
                    super::DispatchEligibility::Execute => Err(JobWorkerError::RuntimeError(
                        format!(
                            "job {} unexpectedly became executable while restoring an unstarted RDB dispatch",
                            job_id.value
                        ),
                    )
                    .into()),
                }
            }
            UnstartedRdbDispatchRestoreOutcome::OwnershipLost => {
                tracing::warn!(
                    job_id = job_id.value,
                    retried = job_data.retried,
                    "lost live status ownership while restoring an unstarted RDB dispatch"
                );
                Ok(None)
            }
        }
    }

    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        let pairs = self.worker_config().channel_concurrency_pair();
        tracing::debug!("start dispatch jobs by rdb. workers and conc: {:?}", &pairs);
        if pairs.is_empty() {
            tracing::info!("RDB job dispatcher is not started because no channels are enabled");
            return Ok(());
        }
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(
                self.job_queue_config().fetch_interval as u64,
            ));
            loop {
                // using tokio::select and shutdown_signal, break loop on SIGINT/SIGTERM
                tokio::select! {
                    _ = interval.tick() => {
                        tracing::trace!("execute pop and enqueue run_after job");
                        let _ = self.pop_and_execute(pairs.clone()).await.map_err(|e| {
                            tracing::error!("failed to pop and enqueue: {:?}", e);
                            e
                        });
                    }
                    _ = command_utils::util::shutdown::shutdown_signal() => {
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
                job
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
                JobWorkerError::NotFound(format!("failed to get worker: {:?}", job)).into(),
            );
        };
        let rid = if let Some(id) = w.runner_id.as_ref() {
            id
        } else {
            tracing::error!("failed to get runner_id: {:?}", &job);
            return Err(
                JobWorkerError::NotFound(format!("failed to get runner_id: {:?}", job)).into(),
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
                rid
            )))
        } else {
            tracing::error!(
                "failed to get runner data for job: {}",
                proto::log_ext::JobSummary(&job)
            );
            Err(JobWorkerError::NotFound(format!(
                "failed to get runner data for job: {}",
                proto::log_ext::JobSummary(&job)
            )))
        }?;

        let job_id = job.id.as_ref().expect("validated job ID");
        let job_data = job.data.as_ref().expect("validated job data");
        match super::resolve_dispatch_preflight(
            self,
            self.check_rdb_cancellation_status(job_id, &wid, &w, job.metadata.clone(), job_data)
                .await?,
            &w,
            job_id,
        )
        .await?
        {
            super::DispatchPreflight::Execute => {}
            super::DispatchPreflight::Skip => return Ok(None),
            super::DispatchPreflight::Completed(result) => return Ok(Some(*result)),
        }
        let start_permit = if let Some(session) = self.worker_instance_session() {
            if let Some(permit) = session.acquire_start_permit() {
                Some(permit)
            } else {
                if let Some(result) = self
                    .restore_or_finalize_unstarted_rdb_dispatch(
                        job_id,
                        &wid,
                        &w,
                        job.metadata.clone(),
                        job_data,
                    )
                    .await?
                {
                    return Ok(Some(result));
                }
                return Err(JobWorkerError::RuntimeError(
                    "worker instance is isolated; RDB job remains pending".to_string(),
                )
                .into());
            }
        } else {
            None
        };
        let (grabbed, rdb_execution) = if let Some(permit) = &start_permit {
            // The live status is claimed before this durable execution claim.
            // #311 remains responsible for making both claims one transaction.
            let resolved = app::app::job::resolve_job_params(&w, job_data.overrides.as_ref());
            let repository = RdbJobStatusExecutionRepository::new(Arc::new(
                self.rdb_job_repository().db_pool().clone(),
            ));
            let execution = repository
                .grab_and_mark_running(RdbDispatchStart {
                    job_id,
                    worker_id: &wid,
                    worker_instance_id: permit.instance_id(),
                    channel: w.channel.as_deref(),
                    priority: job_data.priority,
                    enqueue_time: job_data.enqueue_time,
                    is_streamable: job_data.streaming_type != 0,
                    broadcast_results: resolved.broadcast_results,
                    timeout: Some(job_data.timeout),
                    original_grabbed_until_time: job_data.grabbed_until_time.unwrap_or(0),
                })
                .await?;
            (execution.is_some(), execution)
        } else {
            let grabbed = self
                .rdb_job_repository()
                .grab_job(
                    job_id,
                    Some(job_data.timeout),
                    job_data.grabbed_until_time.unwrap_or(0),
                )
                .await?;
            (grabbed, None)
        };
        match Ok(grabbed) {
            Ok(grabbed) => {
                if grabbed {
                    if let Some(permit) = &start_permit
                        && !permit.confirm_start()
                    {
                        let execution = rdb_execution
                            .as_ref()
                            .expect("RDB execution exists when a start permit grabbed the job");
                        let repository = RdbJobStatusExecutionRepository::new(Arc::new(
                            self.rdb_job_repository().db_pool().clone(),
                        ));
                        if repository.release_unstarted_dispatch(execution).await?
                            != infra::infra::job::status::execution::ClaimOutcome::Claimed
                        {
                            anyhow::bail!(
                                "lost RDB execution before releasing isolated job: {}",
                                job_id.value
                            );
                        }
                        if let Some(result) = self
                            .restore_or_finalize_unstarted_rdb_dispatch(
                                job_id,
                                &wid,
                                &w,
                                job.metadata.clone(),
                                job_data,
                            )
                            .await?
                        {
                            return Ok(Some(result));
                        }
                        return Err(JobWorkerError::RuntimeError(
                            "worker instance was isolated before RDB runner start; RDB job was requeued"
                                .to_string(),
                        )
                        .into());
                    }
                    let mut res = self.run_job(&runner_data, &wid, &w, job).await;
                    super::ensure_job_result_id(self.id_generator(), &mut res.0)?;
                    tracing::debug!(
                        "job completed. result: {}",
                        proto::log_ext::JobResultSummary(&res.0)
                    );
                    // store result
                    let (result, completion_rx) = self
                        .result_processor()
                        .process_result(res.0, res.1, w)
                        .await
                        .inspect_err(|e| {
                            tracing::error!(
                                "failed to process result: worker_id={:?}, err={:?}",
                                &wid,
                                e
                            )
                        })?;
                    if let Some(rx) = completion_rx
                        && rx.await.is_err()
                    {
                        tracing::warn!("stream completion sender dropped for rdb job {:?}", &wid);
                    }
                    Ok(Some(result))
                } else {
                    tracing::debug!("failed to grab job: {}", proto::log_ext::JobSummary(&job));
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
    feed_sender_store: Arc<infra::infra::feed::chan::ChanFeedSenderStore>,
    worker_instance_session: Option<WorkerInstanceSessionHandle>,
}

impl RdbJobDispatcherImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        rdb_job_repository: Arc<RdbChanJobRepositoryImpl>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
        feed_sender_store: Arc<infra::infra::feed::chan::ChanFeedSenderStore>,
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
    ) -> Self {
        Self {
            id_generator,
            job_queue_config: config_module.job_queue_config.clone(),
            rdb_job_repository,
            app_module,
            runner_factory,
            runner_pool_map,
            result_processor,
            feed_sender_store,
            worker_instance_session,
        }
    }
}

impl UseRdbChanJobRepository for RdbJobDispatcherImpl {
    fn rdb_job_repository(&self) -> &RdbChanJobRepositoryImpl {
        &self.rdb_job_repository
    }
}
impl UseWorkerInstanceSession for RdbJobDispatcherImpl {
    fn worker_instance_session(&self) -> Option<&WorkerInstanceSessionHandle> {
        self.worker_instance_session.as_ref()
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

impl jobworkerp_base::codec::UseProstCodec for RdbJobDispatcherImpl {}
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
impl JobRunner for RdbJobDispatcherImpl {
    fn register_feed_sender(
        &self,
        job_id: i64,
        sender: tokio::sync::mpsc::Sender<jobworkerp_runner::runner::FeedData>,
    ) {
        // Standalone RDB mode: register sender directly in feed store
        self.feed_sender_store.register(job_id, sender);
    }

    fn unregister_feed_sender(&self, job_id: i64) {
        self.feed_sender_store.remove(job_id);
    }
}

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
        self.app_module.job_processing_status_repository()
    }
}

impl infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository
    for RdbJobDispatcherImpl
{
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>> {
        self.app_module
            .repositories
            .rdb_module
            .as_ref()
            .and_then(|module| module.rdb_job_processing_status_index_repository.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use infra::infra::job::status::memory::MemoryJobProcessingStatusRepository;

    #[tokio::test]
    async fn restores_only_the_claimed_running_attempt_after_an_unstarted_dispatch() {
        let repository = Arc::new(MemoryJobProcessingStatusRepository::new());
        let job_id = JobId { value: 1 };
        repository
            .upsert_status(&job_id, &JobProcessingStatus::Running)
            .await
            .unwrap();

        assert_eq!(
            restore_pending_after_unstarted_rdb_dispatch(repository.clone(), &job_id, 0)
                .await
                .unwrap(),
            UnstartedRdbDispatchRestoreOutcome::Restored
        );
        assert_eq!(
            repository.find_status(&job_id).await.unwrap(),
            Some(JobProcessingStatus::Pending)
        );
    }

    #[tokio::test]
    async fn does_not_restore_when_cancellation_wins_the_unstarted_dispatch_race() {
        let repository = Arc::new(MemoryJobProcessingStatusRepository::new());
        let job_id = JobId { value: 2 };
        repository
            .upsert_status(&job_id, &JobProcessingStatus::Cancelling)
            .await
            .unwrap();

        assert_eq!(
            restore_pending_after_unstarted_rdb_dispatch(repository.clone(), &job_id, 0)
                .await
                .unwrap(),
            UnstartedRdbDispatchRestoreOutcome::CancellationWon
        );
        assert_eq!(
            repository.find_status(&job_id).await.unwrap(),
            Some(JobProcessingStatus::Cancelling)
        );
    }
}
