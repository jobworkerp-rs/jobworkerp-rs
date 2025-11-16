use std::sync::Arc;

use crate::infra::function_set::rdb::FunctionSetRepositoryImpl;
use crate::infra::job::queue::chan::ChanJobQueueRepositoryImpl;
use crate::infra::job::queue::{JobQueueCancellationRepository, UseJobQueueCancellationRepository};
use crate::infra::job::rdb::{RdbChanJobRepositoryImpl, UseRdbChanJobRepository};
use crate::infra::job::status::memory::MemoryJobProcessingStatusRepository;
use crate::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository;
use crate::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl;
use crate::infra::job_result::rdb::{RdbJobResultRepositoryImpl, UseRdbJobResultRepository};
use crate::infra::runner::rdb::RdbRunnerRepositoryImpl;
use crate::infra::worker::rdb::{RdbWorkerRepositoryImpl, UseRdbWorkerRepository};
use crate::infra::{IdGeneratorWrapper, InfraConfigModule, JobQueueConfig};
use jobworkerp_base::job_status_config::JobStatusConfig;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;
use memory_utils::chan::broadcast::BroadcastChan;
use memory_utils::chan::ChanBuffer;

pub trait UseRdbChanRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule;
}
impl<T: UseRdbChanRepositoryModule> UseRdbWorkerRepository for T {
    fn rdb_worker_repository(&self) -> &RdbWorkerRepositoryImpl {
        &self.rdb_repository_module().worker_repository
    }
}
impl<T: UseRdbChanRepositoryModule> UseRdbChanJobRepository for T {
    fn rdb_job_repository(&self) -> &RdbChanJobRepositoryImpl {
        &self.rdb_repository_module().job_repository
    }
}
impl<T: UseRdbChanRepositoryModule> UseRdbJobResultRepository for T {
    fn rdb_job_result_repository(&self) -> &RdbJobResultRepositoryImpl {
        &self.rdb_repository_module().job_result_repository
    }
}
impl UseJobQueueCancellationRepository for RdbChanRepositoryModule {
    fn job_queue_cancellation_repository(&self) -> Arc<dyn JobQueueCancellationRepository> {
        Arc::new(self.chan_job_queue_repository.clone())
    }
}

#[derive(Clone, Debug)]
pub struct RdbChanRepositoryModule {
    pub runner_repository: RdbRunnerRepositoryImpl,
    pub worker_repository: RdbWorkerRepositoryImpl,
    pub job_repository: RdbChanJobRepositoryImpl,
    pub job_result_repository: RdbJobResultRepositoryImpl,
    pub memory_job_processing_status_repository: Arc<MemoryJobProcessingStatusRepository>,
    pub rdb_job_processing_status_index_repository:
        Option<Arc<RdbJobProcessingStatusIndexRepository>>,
    pub chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl,
    pub chan_job_queue_repository: ChanJobQueueRepositoryImpl,
    pub function_set_repository: Arc<FunctionSetRepositoryImpl>,
}

impl RdbChanRepositoryModule {
    pub async fn new_by_env(
        job_queue_config: Arc<JobQueueConfig>,
        runner_factory: Arc<RunnerSpecFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        let pool = super::super::resource::setup_rdb_by_env().await;
        let job_status_config = JobStatusConfig::from_env();

        // Initialize RDB index repository only if enabled
        let rdb_job_processing_status_index_repository = if job_status_config.rdb_indexing_enabled {
            Some(Arc::new(RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                job_status_config.clone(),
            )))
        } else {
            None
        };

        RdbChanRepositoryModule {
            runner_repository: RdbRunnerRepositoryImpl::new(
                pool,
                runner_factory,
                id_generator.clone(),
            ),
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbChanJobRepositoryImpl::new(job_queue_config.clone(), pool),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            memory_job_processing_status_repository: Arc::new(
                MemoryJobProcessingStatusRepository::new(),
            ),
            rdb_job_processing_status_index_repository,
            chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                ChanBuffer::new(None, 100_000), // broadcast chan. TODO from config
                job_queue_config.clone(),
            ),
            chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                job_queue_config,
                ChanBuffer::new(None, 100_000), // mpmc chan. TODO from config
                BroadcastChan::new(1000),       // broadcast chan for cancellation. TODO from config
            ),
            function_set_repository: Arc::new(FunctionSetRepositoryImpl::new(id_generator, pool)),
        }
    }
    pub async fn new(
        config_module: &InfraConfigModule,
        runner_factory: Arc<RunnerSpecFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        let pool =
            super::super::resource::setup_rdb(config_module.rdb_config.as_ref().unwrap()).await;
        let job_status_config = JobStatusConfig::from_env();

        // Initialize RDB index repository only if enabled
        let rdb_job_processing_status_index_repository = if job_status_config.rdb_indexing_enabled {
            Some(Arc::new(RdbJobProcessingStatusIndexRepository::new(
                Arc::new(pool.clone()),
                job_status_config.clone(),
            )))
        } else {
            None
        };

        RdbChanRepositoryModule {
            runner_repository: RdbRunnerRepositoryImpl::new(
                pool,
                runner_factory,
                id_generator.clone(),
            ),
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbChanJobRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                pool,
            ),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            memory_job_processing_status_repository: Arc::new(
                MemoryJobProcessingStatusRepository::new(),
            ),
            rdb_job_processing_status_index_repository,
            chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                ChanBuffer::new(None, 100_000), // TODO from config
                config_module.job_queue_config.clone(),
            ),
            chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                ChanBuffer::new(None, 100_000), // TODO from config
                BroadcastChan::new(1000),       // broadcast chan for cancellation. TODO from config
            ),
            function_set_repository: Arc::new(FunctionSetRepositoryImpl::new(id_generator, pool)),
        }
    }
}

impl UseRdbChanRepositoryModule for RdbChanRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        self
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    use super::RdbChanRepositoryModule;
    use crate::infra::function_set::rdb::FunctionSetRepositoryImpl;
    use crate::infra::job::queue::chan::ChanJobQueueRepositoryImpl;
    use crate::infra::module::test::TEST_PLUGIN_DIR;
    use crate::infra::runner::rdb::RdbRunnerRepositoryImpl;
    use crate::infra::IdGeneratorWrapper;
    use crate::infra::{
        job::rdb::RdbChanJobRepositoryImpl, job_result::rdb::RdbJobResultRepositoryImpl,
        worker::rdb::RdbWorkerRepositoryImpl,
    };
    use crate::infra::{
        job::status::memory::MemoryJobProcessingStatusRepository,
        job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl, JobQueueConfig,
    };
    use infra_utils::infra::test::setup_test_rdb_from;
    use memory_utils::chan::broadcast::BroadcastChan;

    use jobworkerp_runner::runner::factory::RunnerSpecFactory;
    use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
    use jobworkerp_runner::runner::plugins::Plugins;
    use sqlx::Executor;
    use std::sync::Arc;

    /// Create test RDB repository module with optional RDB indexing
    ///
    /// # Arguments
    /// * `enable_rdb_indexing` - If true, enables JobProcessingStatus RDB indexing for tests
    pub async fn setup_test_rdb_module_with_indexing(
        enable_rdb_indexing: bool,
    ) -> RdbChanRepositoryModule {
        use infra_utils::infra::test::truncate_tables;
        use jobworkerp_base::job_status_config::JobStatusConfig;
        use memory_utils::chan::ChanBuffer;

        let dir = if cfg!(feature = "mysql") {
            "../infra/sql/mysql"
        } else {
            "../infra/sql/sqlite"
        };
        let pool = setup_test_rdb_from(dir).await;
        pool.execute("SELECT 1;").await.expect("test connection");
        // not runner
        truncate_tables(
            pool,
            vec!["job", "worker", "job_result", "job_processing_status"],
        )
        .await;
        // for test
        pool.execute("DELETE FROM runner WHERE id > 1000000;")
            .await
            .expect("delete test runners");
        let runner_factory = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        runner_factory.load_plugins_from(TEST_PLUGIN_DIR).await;
        let id_generator = Arc::new(IdGeneratorWrapper::new());

        // Create RDB indexing repository if enabled
        let rdb_job_processing_status_index_repository = if enable_rdb_indexing {
            Some(Arc::new(
                crate::infra::job::status::rdb::RdbJobProcessingStatusIndexRepository::new(
                    Arc::new(pool.clone()),
                    JobStatusConfig {
                        rdb_indexing_enabled: true,
                        cleanup_interval_hours: 1,
                        retention_hours: 24,
                    },
                ),
            ))
        } else {
            None
        };

        RdbChanRepositoryModule {
            runner_repository: RdbRunnerRepositoryImpl::new(
                pool,
                Arc::new(runner_factory),
                id_generator.clone(),
            ),
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbChanJobRepositoryImpl::new(
                Arc::new(JobQueueConfig::default()),
                pool,
            ),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            memory_job_processing_status_repository: Arc::new(
                MemoryJobProcessingStatusRepository::new(),
            ),
            rdb_job_processing_status_index_repository,
            chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                ChanBuffer::new(None, 10000),
                Arc::new(JobQueueConfig::default()),
            ),
            chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                Arc::new(JobQueueConfig::default()),
                ChanBuffer::new(None, 10000),
                BroadcastChan::new(1000), // broadcast chan for cancellation (test)
            ),
            function_set_repository: Arc::new(FunctionSetRepositoryImpl::new(id_generator, pool)),
        }
    }

    /// Create test RDB repository module (default: RDB indexing disabled)
    pub async fn setup_test_rdb_module() -> RdbChanRepositoryModule {
        setup_test_rdb_module_with_indexing(false).await
    }
}
