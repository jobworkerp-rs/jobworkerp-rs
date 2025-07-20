use std::sync::Arc;

use self::{
    rdb::{RdbJobDispatcher, RdbJobDispatcherImpl},
    redis::{RedisJobDispatcher, RedisJobDispatcherImpl},
};
use super::result_processor::UseResultProcessor;
use super::{result_processor::ResultProcessorImpl, runner::map::RunnerFactoryWithPoolMap};
use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use async_trait::async_trait;
use chan::{ChanJobDispatcher, ChanJobDispatcherImpl};
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::{
    job::rdb::UseRdbChanJobRepository,
    job::status::UseJobProcessingStatusRepository,
    module::{rdb::RdbChanRepositoryModule, redis::RedisRepositoryModule},
    IdGeneratorWrapper, UseIdGenerator,
};
use proto::jobworkerp::data::{
    JobId, JobProcessingStatus, JobResult, JobResultData, ResultOutput, ResultStatus, StorageType,
    WorkerData, WorkerId,
};

pub mod chan;
pub mod rdb;
pub mod redis;
pub mod redis_run_after;

#[async_trait]
pub trait JobDispatcher:
    Send + Sync + 'static + UseJobProcessingStatusRepository + UseIdGenerator + UseResultProcessor
{
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static;

    /// Check cancellation status and return cancellation result if job is marked for cancellation
    /// This method provides common cancellation detection logic for all dispatchers
    async fn check_cancellation_status(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<Option<JobResult>> {
        let status = self
            .job_processing_status_repository()
            .find_status(job_id)
            .await?;

        tracing::debug!(
            "check_cancellation_status: job {} has status {:?}",
            job_id.value,
            status
        );

        match status {
            Some(JobProcessingStatus::Pending) => {
                // Normal state: continue normal execution
                tracing::debug!(
                    "Job {} is in expected Pending state, proceeding with execution",
                    job_id.value
                );
                Ok(None)
            }
            Some(JobProcessingStatus::Cancelling) => {
                // Cancellation requested: skip execution and create cancellation result
                tracing::info!(
                    "Job {} marked for cancellation, skipping execution",
                    job_id.value
                );

                // Directly create cancellation result
                use command_utils::util::datetime;
                let job_result_data = JobResultData {
                    job_id: Some(*job_id),
                    status: ResultStatus::Cancelled as i32,
                    output: Some(ResultOutput {
                        items: b"Job was cancelled before execution".to_vec(),
                    }),
                    start_time: datetime::now_millis(),
                    end_time: datetime::now_millis(),
                    worker_id: Some(*worker_id),
                    args: job_data.args.clone(),
                    uniq_key: job_data.uniq_key.clone(),
                    retried: job_data.retried,
                    max_retry: 0, // No retry on cancellation
                    priority: job_data.priority,
                    timeout: job_data.timeout,
                    request_streaming: job_data.request_streaming,
                    enqueue_time: job_data.enqueue_time,
                    run_after_time: job_data.run_after_time,
                    response_type: worker_data.response_type,
                    store_success: false,
                    store_failure: true,
                    worker_name: worker_data.name.clone(),
                };

                let cancelled_result = JobResult {
                    id: Some(proto::jobworkerp::data::JobResultId {
                        value: self.id_generator().generate_id()?,
                    }),
                    data: Some(job_result_data),
                    metadata: job_metadata,
                };

                Ok(Some(cancelled_result))
            }
            Some(JobProcessingStatus::Running) => {
                // Abnormal state: already running on another worker, prevent duplicate execution
                tracing::error!(
                    "Job {} is already in Running state, preventing duplicate execution",
                    job_id.value
                );
                Err(
                    jobworkerp_base::error::JobWorkerError::RuntimeError(format!(
                        "Job {} is already running",
                        job_id.value
                    ))
                    .into(),
                )
            }
            Some(JobProcessingStatus::WaitResult) => {
                // Abnormal state: already waiting for result
                tracing::error!(
                    "Job {} is already in WaitResult state, preventing duplicate execution",
                    job_id.value
                );
                Err(
                    jobworkerp_base::error::JobWorkerError::RuntimeError(format!(
                        "Job {} is already waiting for result",
                        job_id.value
                    ))
                    .into(),
                )
            }
            Some(JobProcessingStatus::Unknown) => {
                tracing::warn!(
                    "Job {} has unknown status, proceeding with execution",
                    job_id.value
                );
                Ok(None)
            }
            None => {
                tracing::warn!(
                    "Job {} has no status record, proceeding with execution",
                    job_id.value
                );
                Ok(None)
            }
        }
    }
}
// TODO divide into three traits (redis, rdb and redis+rdb)
pub struct JobDispatcherFactory {}
pub struct HybridJobDispatcherImpl {
    pub rdb_job_dispatcher: RdbJobDispatcherImpl,
    pub redis_job_dispatcher: RedisJobDispatcherImpl,
}

pub struct RdbChanJobDispatcherImpl {
    pub rdb_job_dispatcher: RdbJobDispatcherImpl,
    pub chan_job_dispatcher: ChanJobDispatcherImpl,
}

impl JobDispatcherFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        app_module: Arc<AppModule>,
        rdb_chan_repositories_opt: Option<Arc<RdbChanRepositoryModule>>,
        redis_repositories_opt: Option<Arc<RedisRepositoryModule>>,
        runner_factory: Arc<RunnerFactory>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Box<dyn JobDispatcher + 'static> {
        match (
            app_module.config_module.storage_type(),
            rdb_chan_repositories_opt.clone(),
            redis_repositories_opt,
        ) {
            // (StorageType::Redis, _, Some(redis_repositories)) => {
            //     Box::new(RedisJobDispatcherImpl::new(
            //         id_generator,
            //         config_module,
            //         redis_repositories.redis_client.clone(),
            //         Arc::new(redis_repositories.redis_job_repository.clone()),
            //         None,
            //         app_module,
            //         runner_factory,
            //         runner_pool_map,
            //         result_processor,
            //     ))
            // }
            (StorageType::Standalone, Some(rdb_chan_repositories), _) => {
                let rdb_job_repository = Arc::new(rdb_chan_repositories.job_repository.clone());
                Box::new(RdbChanJobDispatcherImpl {
                    rdb_job_dispatcher: RdbJobDispatcherImpl::new(
                        id_generator.clone(),
                        config_module,
                        rdb_job_repository.clone(),
                        app_module.clone(),
                        runner_factory.clone(),
                        runner_pool_map.clone(),
                        result_processor.clone(),
                    ),
                    chan_job_dispatcher: ChanJobDispatcherImpl::new(
                        id_generator,
                        Arc::new(rdb_chan_repositories.chan_job_queue_repository.clone()),
                        rdb_job_repository,
                        rdb_chan_repositories
                            .memory_job_processing_status_repository
                            .clone(),
                        app_module,
                        runner_factory,
                        runner_pool_map,
                        result_processor,
                    ),
                })
            }
            (StorageType::Scalable, Some(rdb_chan_repositories), Some(redis_repositories)) => {
                Box::new(HybridJobDispatcherImpl {
                    rdb_job_dispatcher: RdbJobDispatcherImpl::new(
                        id_generator.clone(),
                        config_module.clone(),
                        Arc::new(rdb_chan_repositories.rdb_job_repository().clone()),
                        app_module.clone(),
                        runner_factory.clone(),
                        runner_pool_map.clone(),
                        result_processor.clone(),
                    ),
                    redis_job_dispatcher: RedisJobDispatcherImpl::new(
                        id_generator,
                        config_module,
                        redis_repositories.redis_client.clone(),
                        Arc::new(redis_repositories.redis_job_repository.clone()),
                        Some(Arc::new(rdb_chan_repositories.rdb_job_repository().clone())),
                        app_module,
                        runner_factory,
                        runner_pool_map,
                        result_processor,
                    ),
                })
            }
            (t, db, rd) => panic!(
                "illegal storage type and repository: {:?}, {:?}, {:?}",
                t, &db, &rd
            ),
        }
    }
}

impl UseJobProcessingStatusRepository for HybridJobDispatcherImpl {
    fn job_processing_status_repository(
        &self,
    ) -> Arc<dyn infra::infra::job::status::JobProcessingStatusRepository> {
        self.redis_job_dispatcher.job_processing_status_repository()
    }
}

impl UseIdGenerator for HybridJobDispatcherImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        self.redis_job_dispatcher.id_generator()
    }
}

impl UseResultProcessor for HybridJobDispatcherImpl {
    fn result_processor(&self) -> &ResultProcessorImpl {
        self.redis_job_dispatcher.result_processor()
    }
}

#[async_trait]
impl JobDispatcher for HybridJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RdbJobDispatcher::dispatch_jobs(&self.rdb_job_dispatcher, lock.clone())?;
        RedisJobDispatcher::dispatch_jobs(&self.redis_job_dispatcher, lock)
    }
}
impl UseJobProcessingStatusRepository for RdbChanJobDispatcherImpl {
    fn job_processing_status_repository(
        &self,
    ) -> Arc<dyn infra::infra::job::status::JobProcessingStatusRepository> {
        self.chan_job_dispatcher.job_processing_status_repository()
    }
}

impl UseIdGenerator for RdbChanJobDispatcherImpl {
    fn id_generator(&self) -> &IdGeneratorWrapper {
        self.chan_job_dispatcher.id_generator()
    }
}

impl UseResultProcessor for RdbChanJobDispatcherImpl {
    fn result_processor(&self) -> &ResultProcessorImpl {
        self.chan_job_dispatcher.result_processor()
    }
}

#[async_trait]
impl JobDispatcher for RdbChanJobDispatcherImpl {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static,
    {
        RdbJobDispatcher::dispatch_jobs(&self.rdb_job_dispatcher, lock.clone())?;
        ChanJobDispatcher::dispatch_jobs(&self.chan_job_dispatcher, lock)
    }
}
