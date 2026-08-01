use std::sync::Arc;

use self::{
    rdb::{RdbJobDispatcher, RdbJobDispatcherImpl},
    redis::{RedisJobDispatcher, RedisJobDispatcherImpl},
};
use super::instance_session::WorkerInstanceSessionHandle;
use super::result_processor::UseResultProcessor;
use super::{result_processor::ResultProcessorImpl, runner::map::RunnerFactoryWithPoolMap};
use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use async_trait::async_trait;
use chan::{ChanJobDispatcher, ChanJobDispatcherImpl};
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::{
    IdGeneratorWrapper, UseIdGenerator,
    job::rdb::UseRdbChanJobRepository,
    job::status::rdb::UseRdbJobProcessingStatusIndexRepository,
    job::status::{
        JobProcessingStatusRecord, StatusTransitionResult, UseJobProcessingStatusRepository,
    },
    module::{rdb::RdbChanRepositoryModule, redis::RedisRepositoryModule},
};
use jobworkerp_base::error::JobWorkerError;
use proto::jobworkerp::data::{
    JobId, JobProcessingStatus, JobResult, JobResultData, JobResultId, ResultOutput, ResultStatus,
    StorageType, WorkerData, WorkerId,
};

pub mod chan;
pub mod rdb;
pub mod redis;
pub mod redis_run_after;

pub enum DispatchEligibility {
    Execute,
    Cancelled(Box<JobResult>),
    Skip,
}

pub(crate) enum DispatchPreflight {
    Execute,
    Skip,
    Completed(JobResult),
}

pub(crate) async fn resolve_dispatch_preflight<D>(
    dispatcher: &D,
    eligibility: DispatchEligibility,
    worker_data: &WorkerData,
    job_id: &JobId,
) -> Result<DispatchPreflight>
where
    D: JobDispatcher + ?Sized,
{
    match eligibility {
        DispatchEligibility::Execute => Ok(DispatchPreflight::Execute),
        DispatchEligibility::Skip => Ok(DispatchPreflight::Skip),
        DispatchEligibility::Cancelled(result) => dispatcher
            .process_cancelled_dispatch_result(*result, worker_data, job_id)
            .await
            .map(DispatchPreflight::Completed),
    }
}

/// Ensures every result that reaches the result processor has an identifier.
pub fn ensure_job_result_id(
    id_generator: &IdGeneratorWrapper,
    result: &mut JobResult,
) -> Result<JobResultId> {
    if let Some(id) = result.id {
        Ok(id)
    } else {
        let id = JobResultId {
            value: id_generator.generate_id()?,
        };
        result.id = Some(id);
        Ok(id)
    }
}

/// Determine if job status should be cleaned up based on error type
pub fn should_cleanup_status_on_error(err: &anyhow::Error) -> bool {
    if let Some(job_err) = err.downcast_ref::<JobWorkerError>() {
        job_err.should_delete_job_status()
    } else {
        // Unknown error types: don't delete (safer default)
        false
    }
}

#[async_trait]
pub trait JobDispatcher:
    Send
    + Sync
    + 'static
    + UseJobProcessingStatusRepository
    + UseRdbJobProcessingStatusIndexRepository
    + UseIdGenerator
    + UseResultProcessor
{
    /// Clean up job processing status for permanent errors
    ///
    /// Deletes status from both primary storage and RDB indexing (if enabled).
    /// Logs appropriate messages based on cleanup success/failure.
    ///
    /// # Arguments
    /// * `job_id` - The job ID to clean up
    /// * `storage_label` - Label for logging (e.g., "redis", "memory")
    async fn cleanup_failed_job_status(&self, job_id: &JobId, storage_label: &str) {
        let mut storage_deleted = false;
        let mut rdb_deleted = false;

        // Delete from primary storage (Redis or Memory)
        match self
            .job_processing_status_repository()
            .delete_status(job_id)
            .await
        {
            Ok(_) => storage_deleted = true,
            Err(e) => {
                tracing::warn!("Failed to cleanup status for job {}: {:?}", job_id.value, e);
            }
        }

        // Delete from RDB index (if enabled)
        if let Some(index_repo) = self.rdb_job_processing_status_index_repository() {
            match index_repo.mark_deleted_by_job_id(job_id).await {
                Ok(_) => rdb_deleted = true,
                Err(e) => {
                    tracing::warn!(
                        "Failed to cleanup RDB index for job {}: {:?}",
                        job_id.value,
                        e
                    );
                }
            }
        } else {
            // No RDB index configured, consider it as "not applicable" rather than failed
            rdb_deleted = true;
        }

        if storage_deleted || rdb_deleted {
            tracing::info!(
                "Job {} status cleaned up due to permanent error ({}: {}, rdb: {})",
                job_id.value,
                storage_label,
                storage_deleted,
                rdb_deleted
            );
        } else {
            tracing::warn!("Job {} status cleanup failed for all stores", job_id.value);
        }
    }

    /// Finalize a cancellation result consistently across queue backends.
    async fn process_cancelled_dispatch_result(
        &self,
        cancelled_result: JobResult,
        worker_data: &WorkerData,
        job_id: &JobId,
    ) -> Result<JobResult> {
        let (result, completion_rx) = self
            .result_processor()
            .process_result(cancelled_result, None, worker_data.clone())
            .await?;
        if let Some(rx) = completion_rx
            && rx.await.is_err()
        {
            tracing::warn!(
                "stream completion sender dropped for cancelled job {:?}",
                job_id
            );
        }
        Ok(result)
    }

    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static;

    /// Claims a pending queue attempt or makes a stale/cancelled delivery inert.
    async fn check_cancellation_status(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<DispatchEligibility> {
        self.check_cancellation_status_with_missing_status(
            job_id,
            worker_id,
            worker_data,
            job_metadata,
            job_data,
            false,
        )
        .await
    }

    async fn check_cancellation_status_with_missing_status(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
        execute_when_status_missing: bool,
    ) -> Result<DispatchEligibility> {
        let record = self
            .job_processing_status_repository()
            .find_status_record(job_id)
            .await?;

        tracing::debug!(
            "check_cancellation_status: job {} has status {:?}",
            job_id.value,
            record
        );

        let record = match record {
            Some(record) if record.retried == job_data.retried => record,
            Some(record) => {
                tracing::info!(
                    "Skipping stale job {} attempt {} because live attempt is {}",
                    job_id.value,
                    job_data.retried,
                    record.retried
                );
                return Ok(DispatchEligibility::Skip);
            }
            None if execute_when_status_missing => return Ok(DispatchEligibility::Execute),
            None => return Ok(DispatchEligibility::Skip),
        };

        match record.status {
            JobProcessingStatus::Pending => match self
                .job_processing_status_repository()
                .compare_and_set_status(
                    job_id,
                    Some(record),
                    Some(JobProcessingStatusRecord {
                        status: JobProcessingStatus::Running,
                        retried: job_data.retried,
                    }),
                )
                .await?
            {
                StatusTransitionResult::Applied => Ok(DispatchEligibility::Execute),
                StatusTransitionResult::Conflict(Some(conflict))
                    if conflict.status == JobProcessingStatus::Cancelling
                        && conflict.retried == job_data.retried =>
                {
                    self.cancelled_dispatch_eligibility(
                        job_id,
                        worker_id,
                        worker_data,
                        job_metadata,
                        job_data,
                    )
                    .await
                }
                StatusTransitionResult::Conflict(_) => Ok(DispatchEligibility::Skip),
            },
            JobProcessingStatus::Cancelling => {
                self.cancelled_dispatch_eligibility(
                    job_id,
                    worker_id,
                    worker_data,
                    job_metadata,
                    job_data,
                )
                .await
            }
            JobProcessingStatus::Running
            | JobProcessingStatus::WaitResult
            | JobProcessingStatus::Unknown => Ok(DispatchEligibility::Skip),
        }
    }

    /// RDB-dispatched jobs created before live status publication are allowed
    /// to execute, while jobs with a status use the regular CAS claim path.
    async fn check_rdb_cancellation_status(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<DispatchEligibility> {
        self.check_cancellation_status_with_missing_status(
            job_id,
            worker_id,
            worker_data,
            job_metadata,
            job_data,
            true,
        )
        .await
    }

    async fn cancelled_dispatch_eligibility(
        &self,
        job_id: &JobId,
        worker_id: &WorkerId,
        worker_data: &WorkerData,
        job_metadata: std::collections::HashMap<String, String>,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> Result<DispatchEligibility> {
        // Cancellation requested: skip execution and create cancellation result
        tracing::info!(
            "Job {} marked for cancellation, skipping execution",
            job_id.value
        );

        // Resolve job params (merge worker defaults with per-job overrides)
        use command_utils::util::datetime;
        let resolved = app::app::job::resolve_job_params(worker_data, job_data.overrides.as_ref());
        #[allow(deprecated)]
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
            streaming_type: job_data.streaming_type,
            enqueue_time: job_data.enqueue_time,
            run_after_time: job_data.run_after_time,
            response_type: resolved.response_type,
            // Cancellation is a failure-path; store_success is effectively unused
            // but we use the resolved value for consistency with other fields.
            store_success: resolved.store_success,
            store_failure: resolved.store_failure,
            worker_name: worker_data.name.clone(),
            using: job_data.using.clone(),
            broadcast_results: resolved.broadcast_results,
            // Cancellation is handled by status (Cancelled); build_retry_job()
            // returns None for non-ErrorAndRetry status regardless of policy.
            resolved_retry_policy: resolved.retry_policy,
        };

        let cancelled_result = JobResult {
            id: Some(proto::jobworkerp::data::JobResultId {
                value: self.id_generator().generate_id()?,
            }),
            data: Some(job_result_data),
            metadata: job_metadata,
        };

        Ok(DispatchEligibility::Cancelled(Box::new(cancelled_result)))
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
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
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
                // Use the shared ChanFeedSenderStore from AppModule so that
                // gRPC handler (publish) and runner (register) operate on the same instance.
                let feed_store = app_module.feed_sender_store.clone().unwrap_or_else(|| {
                    Arc::new(infra::infra::feed::chan::ChanFeedSenderStore::new())
                });
                Box::new(RdbChanJobDispatcherImpl {
                    rdb_job_dispatcher: RdbJobDispatcherImpl::new(
                        id_generator.clone(),
                        config_module,
                        rdb_job_repository.clone(),
                        app_module.clone(),
                        runner_factory.clone(),
                        runner_pool_map.clone(),
                        result_processor.clone(),
                        feed_store.clone(),
                        None,
                    ),
                    chan_job_dispatcher: ChanJobDispatcherImpl::new(
                        id_generator,
                        Arc::new(rdb_chan_repositories.chan_job_queue_repository.clone()),
                        rdb_job_repository,
                        rdb_chan_repositories
                            .memory_job_processing_status_repository
                            .clone(),
                        rdb_chan_repositories
                            .rdb_job_processing_status_index_repository
                            .clone(),
                        rdb_chan_repositories.chan_worker_pubsub_repository.clone(),
                        app_module,
                        runner_factory,
                        runner_pool_map,
                        result_processor,
                        feed_store,
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
                        // TODO: In Scalable mode, ChanFeedSenderStore is in-process only and
                        // won't receive feed data from gRPC handlers using RedisFeedPublisher.
                        // If a periodic/RDB-dispatched job uses feed, the ChanFeedSenderStore
                        // will have no registered senders from the gRPC side, causing publish_feed
                        // to return "No feed channel registered" errors. To fix this, RDB dispatcher
                        // should use RedisJobDispatcherImpl's Redis-based feed bridge instead.
                        Arc::new(infra::infra::feed::chan::ChanFeedSenderStore::new()),
                        worker_instance_session.clone(),
                    ),
                    redis_job_dispatcher: RedisJobDispatcherImpl::new(
                        id_generator,
                        config_module,
                        redis_repositories.redis_client.clone(),
                        Arc::new(redis_repositories.redis_job_repository.clone()),
                        redis_repositories.redis_blocking_pool,
                        Some(Arc::new(rdb_chan_repositories.rdb_job_repository().clone())),
                        app_module,
                        runner_factory,
                        runner_pool_map,
                        result_processor,
                        worker_instance_session,
                    ),
                })
            }
            (t, db, rd) => panic!(
                "illegal storage type and repository: {:?}, {:?}, {:?}",
                t, db, rd
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

impl UseRdbJobProcessingStatusIndexRepository for HybridJobDispatcherImpl {
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>> {
        self.redis_job_dispatcher
            .rdb_job_processing_status_index_repository()
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

impl UseRdbJobProcessingStatusIndexRepository for RdbChanJobDispatcherImpl {
    fn rdb_job_processing_status_index_repository(
        &self,
    ) -> Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>> {
        self.chan_job_dispatcher
            .rdb_job_processing_status_index_repository()
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

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use infra::infra::IdGeneratorWrapper;
    use infra::infra::job::status::memory::MemoryJobProcessingStatusRepository;
    use infra::infra::job::status::rdb::UseRdbJobProcessingStatusIndexRepository;
    use infra::infra::job::status::{
        JobProcessingStatusRepository, UseJobProcessingStatusRepository,
    };
    use proto::jobworkerp::data::{JobProcessingStatus, JobResult, JobResultId, WorkerData};

    struct TestDispatcher {
        id_generator: IdGeneratorWrapper,
        status_repository: Arc<MemoryJobProcessingStatusRepository>,
    }

    impl TestDispatcher {
        fn new() -> Self {
            Self {
                id_generator: IdGeneratorWrapper::new_mock(),
                status_repository: Arc::new(MemoryJobProcessingStatusRepository::new()),
            }
        }
    }

    impl UseJobProcessingStatusRepository for TestDispatcher {
        fn job_processing_status_repository(
            &self,
        ) -> Arc<dyn infra::infra::job::status::JobProcessingStatusRepository> {
            self.status_repository.clone()
        }
    }

    impl UseRdbJobProcessingStatusIndexRepository for TestDispatcher {
        fn rdb_job_processing_status_index_repository(
            &self,
        ) -> Option<Arc<infra::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository>>
        {
            None
        }
    }

    impl UseIdGenerator for TestDispatcher {
        fn id_generator(&self) -> &IdGeneratorWrapper {
            &self.id_generator
        }
    }

    impl UseResultProcessor for TestDispatcher {
        fn result_processor(&self) -> &super::super::result_processor::ResultProcessorImpl {
            panic!("status claim tests do not process results")
        }
    }

    #[async_trait]
    impl JobDispatcher for TestDispatcher {
        fn dispatch_jobs(&'static self, _lock: ShutdownLock) -> Result<()> {
            Ok(())
        }
    }

    fn worker_data() -> WorkerData {
        WorkerData {
            name: "rdb-claim-test".to_string(),
            ..Default::default()
        }
    }

    fn job_data() -> proto::jobworkerp::data::JobData {
        proto::jobworkerp::data::JobData {
            retried: 0,
            ..Default::default()
        }
    }

    #[test]
    fn ensure_job_result_id_generates_only_when_missing() {
        let generator = IdGeneratorWrapper::new_mock();
        let mut missing = JobResult::default();
        let generated = ensure_job_result_id(&generator, &mut missing).unwrap();
        assert_eq!(missing.id, Some(generated));

        let existing = JobResultId { value: 42 };
        let mut result = JobResult {
            id: Some(existing),
            ..Default::default()
        };
        assert_eq!(
            ensure_job_result_id(&generator, &mut result).unwrap(),
            existing
        );
        assert_eq!(result.id, Some(existing));
    }

    #[tokio::test]
    async fn rdb_claim_allows_statusless_legacy_jobs() {
        let dispatcher = TestDispatcher::new();
        let job_id = JobId { value: 1 };
        let worker_id = WorkerId { value: 1 };
        assert!(matches!(
            dispatcher
                .check_rdb_cancellation_status(
                    &job_id,
                    &worker_id,
                    &worker_data(),
                    Default::default(),
                    &job_data(),
                )
                .await
                .unwrap(),
            DispatchEligibility::Execute
        ));
    }

    #[tokio::test]
    async fn rdb_claim_transitions_pending_and_returns_cancelling_jobs() {
        let dispatcher = TestDispatcher::new();
        let job_id = JobId { value: 2 };
        let worker_id = WorkerId { value: 1 };
        dispatcher
            .status_repository
            .upsert_status(&job_id, &JobProcessingStatus::Pending)
            .await
            .unwrap();
        assert!(matches!(
            dispatcher
                .check_rdb_cancellation_status(
                    &job_id,
                    &worker_id,
                    &worker_data(),
                    Default::default(),
                    &job_data(),
                )
                .await
                .unwrap(),
            DispatchEligibility::Execute
        ));
        assert_eq!(
            dispatcher
                .status_repository
                .find_status(&job_id)
                .await
                .unwrap(),
            Some(JobProcessingStatus::Running)
        );

        dispatcher
            .status_repository
            .upsert_status(&job_id, &JobProcessingStatus::Cancelling)
            .await
            .unwrap();
        assert!(matches!(
            dispatcher
                .check_rdb_cancellation_status(
                    &job_id,
                    &worker_id,
                    &worker_data(),
                    Default::default(),
                    &job_data(),
                )
                .await
                .unwrap(),
            DispatchEligibility::Cancelled(_)
        ));
    }
}
