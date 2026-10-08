pub mod rdb;
pub mod redis;

use self::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use super::job::queue::{JobQueueCancellationRepository, UseJobQueueCancellationRepository};
use super::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use super::worker_instance::memory::MemoryWorkerInstanceRepository;
use super::worker_instance::redis::RedisWorkerInstanceRepository;
use super::worker_instance::{UseWorkerInstanceRepository, WorkerInstanceRepository};
use super::{IdGeneratorWrapper, InfraConfigModule, JobQueueConfig};
use jobworkerp_base::job_status_config::JobStatusConfig;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;
use rdb::{RdbChanRepositoryModule, UseRdbChanRepositoryModule};
use std::sync::Arc;

// redis and rdb module for DI
#[derive(Clone, Debug)]
pub struct HybridRepositoryModule {
    pub redis_module: RedisRepositoryModule,
    pub rdb_chan_module: RdbChanRepositoryModule,
}

impl HybridRepositoryModule {
    // TODO to config?
    const DEFAULT_WORKER_REDIS_EXPIRE_SEC: Option<usize> = Some(60 * 60);
    pub async fn new(
        infra_config_module: &InfraConfigModule,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_factory: Arc<RunnerSpecFactory>,
    ) -> Self {
        let redis_module = RedisRepositoryModule::new(
            infra_config_module,
            id_generator.clone(),
            runner_factory.clone(),
            Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC,
        )
        .await;
        let rdb_module =
            RdbChanRepositoryModule::new(infra_config_module, runner_factory, id_generator).await;
        HybridRepositoryModule {
            redis_module,
            rdb_chan_module: rdb_module,
        }
    }
    pub async fn new_by_env(
        job_queue_config: Arc<JobQueueConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_factory: Arc<RunnerSpecFactory>,
    ) -> Self {
        let redis_module = RedisRepositoryModule::new_by_env(
            Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC,
            id_generator.clone(),
            runner_factory.clone(),
        )
        .await;
        let job_status_config = Arc::new(JobStatusConfig::from_env());

        let rdb_module = RdbChanRepositoryModule::new_by_env(
            job_queue_config,
            job_status_config,
            runner_factory,
            id_generator,
        )
        .await;
        HybridRepositoryModule {
            redis_module,
            rdb_chan_module: rdb_module,
        }
    }
}
impl UseRedisRepositoryModule for HybridRepositoryModule {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.redis_module
    }
}

impl UseRdbChanRepositoryModule for HybridRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        &self.rdb_chan_module
    }
}
impl UseJobQueueCancellationRepository for HybridRepositoryModule {
    fn job_queue_cancellation_repository(&self) -> Arc<dyn JobQueueCancellationRepository> {
        // In Hybrid mode, Redis is used preferentially
        Arc::new(self.redis_module.redis_job_queue_repository.clone())
    }
}

impl UseJobProcessingStatusRepository for HybridRepositoryModule {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        // In Hybrid mode, Redis is used preferentially
        self.redis_module.job_processing_status_repository()
    }
}

impl UseWorkerInstanceRepository for HybridRepositoryModule {
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository> {
        // In Hybrid (Scalable) mode, Redis is used for cross-instance coordination
        Arc::new(RedisWorkerInstanceRepository::new(
            self.redis_module.redis_pool,
        ))
    }
}

// for app module container
#[derive(Clone, Debug)]
pub struct RedisRdbOptionalRepositoryModule {
    pub redis_module: Option<Arc<RedisRepositoryModule>>,
    pub rdb_module: Option<Arc<RdbChanRepositoryModule>>,
}
impl From<Arc<RedisRepositoryModule>> for RedisRdbOptionalRepositoryModule {
    fn from(redis_module: Arc<RedisRepositoryModule>) -> Self {
        RedisRdbOptionalRepositoryModule {
            redis_module: Some(redis_module),
            rdb_module: None,
        }
    }
}
impl From<Arc<RdbChanRepositoryModule>> for RedisRdbOptionalRepositoryModule {
    fn from(rdb_module: Arc<RdbChanRepositoryModule>) -> Self {
        RedisRdbOptionalRepositoryModule {
            redis_module: None,
            rdb_module: Some(rdb_module),
        }
    }
}
impl From<Arc<HybridRepositoryModule>> for RedisRdbOptionalRepositoryModule {
    fn from(hybrid_module: Arc<HybridRepositoryModule>) -> Self {
        RedisRdbOptionalRepositoryModule {
            redis_module: Some(Arc::new(hybrid_module.redis_module.clone())),
            rdb_module: Some(Arc::new(hybrid_module.rdb_chan_module.clone())),
        }
    }
}
impl UseJobQueueCancellationRepository for RedisRdbOptionalRepositoryModule {
    fn job_queue_cancellation_repository(&self) -> Arc<dyn JobQueueCancellationRepository> {
        match (&self.redis_module, &self.rdb_module) {
            (Some(redis), _) => Arc::new(redis.redis_job_queue_repository.clone()),
            (None, Some(rdb)) => Arc::new(rdb.chan_job_queue_repository.clone()),
            (None, None) => panic!("No repository module available"),
        }
    }
}

impl UseJobProcessingStatusRepository for RedisRdbOptionalRepositoryModule {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        match (&self.redis_module, &self.rdb_module) {
            (Some(redis), _) => redis.job_processing_status_repository(),
            (None, Some(rdb)) => rdb.memory_job_processing_status_repository.clone(),
            (None, None) => panic!("No repository module available"),
        }
    }
}

impl UseWorkerInstanceRepository for RedisRdbOptionalRepositoryModule {
    fn worker_instance_repository(&self) -> Arc<dyn WorkerInstanceRepository> {
        match (&self.redis_module, &self.rdb_module) {
            // Scalable mode: use Redis for cross-instance coordination
            (Some(redis), _) => Arc::new(RedisWorkerInstanceRepository::new(redis.redis_pool)),
            // Standalone mode (RDB only): use Memory (single process, no persistence needed)
            (None, Some(_rdb)) => Arc::new(MemoryWorkerInstanceRepository::new()),
            // Fallback: use Memory with warning
            (None, None) => {
                tracing::warn!(
                    "WorkerInstanceRepository: No storage module available, using memory fallback"
                );
                Arc::new(MemoryWorkerInstanceRepository::new())
            }
        }
    }
}
#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    #[cfg(debug_assertions)]
    pub const TEST_PLUGIN_DIR: &str = "./target/debug,../target/debug";
    #[cfg(not(debug_assertions))]
    pub const TEST_PLUGIN_DIR: &str = "./target/release,../target/release";
    // jobworkerp_runner::runner::factory::test::TEST_PLUGIN_DIR;

    #[test]
    fn test_plugin_directory_matches_build_profile() {
        let expected_profile = if cfg!(debug_assertions) {
            "target/debug"
        } else {
            "target/release"
        };

        assert!(
            TEST_PLUGIN_DIR
                .split(',')
                .all(|path| path.ends_with(expected_profile)),
            "test plugins must use only the current build profile: {TEST_PLUGIN_DIR}"
        );
    }
}

#[cfg(test)]
mod status_sharing_tests {
    use super::*;
    use crate::infra::job::status::{JobProcessingStatusRecord, StatusTransitionResult};
    use proto::jobworkerp::data::{JobId, JobProcessingStatus};

    #[tokio::test]
    async fn standalone_module_shares_status_and_preserves_cancellation() {
        let rdb = Arc::new(rdb::test::setup_test_rdb_module(false).await);
        let module = RedisRdbOptionalRepositoryModule::from(rdb.clone());
        let job_id = JobId { value: 70001 };
        let running = JobProcessingStatusRecord {
            status: JobProcessingStatus::Running,
            retried: 1,
        };
        let cancelling = JobProcessingStatusRecord {
            status: JobProcessingStatus::Cancelling,
            retried: 1,
        };
        let pending = JobProcessingStatusRecord {
            status: JobProcessingStatus::Pending,
            retried: 2,
        };
        let dispatcher = &rdb.memory_job_processing_status_repository;
        assert_eq!(
            dispatcher
                .compare_and_set_status(&job_id, None, Some(running))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        let processor = module.job_processing_status_repository();
        assert_eq!(
            processor.find_status_record(&job_id).await.unwrap(),
            Some(running)
        );
        assert_eq!(
            processor
                .compare_and_set_status(&job_id, Some(running), Some(cancelling))
                .await
                .unwrap(),
            StatusTransitionResult::Applied
        );
        let reopened = module.clone().job_processing_status_repository();
        assert_eq!(
            reopened.find_status_record(&job_id).await.unwrap(),
            Some(cancelling)
        );
        assert_eq!(
            dispatcher
                .compare_and_set_status(&job_id, Some(running), Some(pending))
                .await
                .unwrap(),
            StatusTransitionResult::Conflict(Some(cancelling))
        );
        assert_eq!(
            reopened.find_status_record(&job_id).await.unwrap(),
            Some(cancelling)
        );
        processor.delete_status(&job_id).await.unwrap();
        assert_eq!(dispatcher.find_status_record(&job_id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn standalone_status_does_not_leak_between_independent_modules() {
        let first = RedisRdbOptionalRepositoryModule::from(Arc::new(
            rdb::test::setup_test_rdb_module(false).await,
        ));
        let second = RedisRdbOptionalRepositoryModule::from(Arc::new(
            rdb::test::setup_test_rdb_module(false).await,
        ));
        let job_id = JobId { value: 70002 };
        first
            .job_processing_status_repository()
            .upsert_status(&job_id, &JobProcessingStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            first
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap(),
            Some(JobProcessingStatus::Pending)
        );
        assert_eq!(
            second
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn redis_module_reuses_its_owned_status_repository_across_wrappers() {
        let redis_url = std::env::var("TEST_REDIS_URL").expect(
            "TEST_REDIS_URL must identify an isolated Redis instance for this destructive fixture",
        );
        assert!(!redis_url.trim().is_empty());
        let redis = redis::test::setup_test_redis_module().await;
        let optional = RedisRdbOptionalRepositoryModule::from(Arc::new(redis.clone()));
        let hybrid = HybridRepositoryModule {
            redis_module: redis,
            rdb_chan_module: rdb::test::setup_test_rdb_module(false).await,
        };
        let first = optional.job_processing_status_repository();
        let second = hybrid.job_processing_status_repository();
        assert!(Arc::ptr_eq(&first, &second));
        let job_id = JobId { value: 70003 };
        first
            .upsert_status(&job_id, &JobProcessingStatus::Cancelling)
            .await
            .unwrap();
        assert_eq!(
            second.find_status(&job_id).await.unwrap(),
            Some(JobProcessingStatus::Cancelling)
        );
        second.delete_status(&job_id).await.unwrap();
        assert_eq!(
            optional
                .job_processing_status_repository()
                .find_status(&job_id)
                .await
                .unwrap(),
            None
        );
    }
}
