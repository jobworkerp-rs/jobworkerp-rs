use std::sync::Arc;

use crate::infra::job::queue::chan::ChanJobQueueRepositoryImpl;
use crate::infra::job::rdb::{RdbChanJobRepositoryImpl, UseRdbChanJobRepository};
use crate::infra::job::status::memory::MemoryJobStatusRepository;
use crate::infra::job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl;
use crate::infra::job_result::rdb::{RdbJobResultRepositoryImpl, UseRdbJobResultRepository};
use crate::infra::runner::factory::RunnerFactory;
use crate::infra::worker::rdb::{RdbWorkerRepositoryImpl, UseRdbWorkerRepository};
use crate::infra::worker_schema::rdb::RdbWorkerSchemaRepositoryImpl;
use crate::infra::{IdGeneratorWrapper, InfraConfigModule, JobQueueConfig};
use infra_utils::infra::chan::ChanBuffer;

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

#[derive(Clone, Debug)]
pub struct RdbChanRepositoryModule {
    pub worker_schema_repository: RdbWorkerSchemaRepositoryImpl,
    pub worker_repository: RdbWorkerRepositoryImpl,
    pub job_repository: RdbChanJobRepositoryImpl,
    pub job_result_repository: RdbJobResultRepositoryImpl,
    pub memory_job_status_repository: Arc<MemoryJobStatusRepository>,
    pub chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl,
    pub chan_job_queue_repository: ChanJobQueueRepositoryImpl,
}

impl RdbChanRepositoryModule {
    pub async fn new_by_env(
        job_queue_config: Arc<JobQueueConfig>,
        runner_factory: Arc<RunnerFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        let pool = super::super::resource::setup_rdb_by_env().await;
        RdbChanRepositoryModule {
            worker_schema_repository: RdbWorkerSchemaRepositoryImpl::new(
                pool,
                runner_factory,
                id_generator,
            ),
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbChanJobRepositoryImpl::new(job_queue_config.clone(), pool),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            memory_job_status_repository: Arc::new(MemoryJobStatusRepository::new()),
            chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                ChanBuffer::new(None, 100_000), // broadcast chan. TODO from config
                job_queue_config.clone(),
            ),
            chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                job_queue_config,
                ChanBuffer::new(None, 100_000), // mpmc chan. TODO from config
            ),
        }
    }
    pub async fn new(
        config_module: &InfraConfigModule,
        runner_factory: Arc<RunnerFactory>,
        id_generator: Arc<IdGeneratorWrapper>,
    ) -> Self {
        let pool =
            super::super::resource::setup_rdb(config_module.rdb_config.as_ref().unwrap()).await;
        RdbChanRepositoryModule {
            worker_schema_repository: RdbWorkerSchemaRepositoryImpl::new(
                pool,
                runner_factory,
                id_generator,
            ),
            worker_repository: RdbWorkerRepositoryImpl::new(pool),
            job_repository: RdbChanJobRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                pool,
            ),
            job_result_repository: RdbJobResultRepositoryImpl::new(pool),
            memory_job_status_repository: Arc::new(MemoryJobStatusRepository::new()),
            chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                ChanBuffer::new(None, 100_000), // TODO from config
                config_module.job_queue_config.clone(),
            ),
            chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                ChanBuffer::new(None, 100_000), // TODO from config
            ),
        }
    }
}

impl UseRdbChanRepositoryModule for RdbChanRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbChanRepositoryModule {
        self
    }
}

pub mod test {
    use super::RdbChanRepositoryModule;
    use crate::infra::job::queue::chan::ChanJobQueueRepositoryImpl;
    use crate::infra::runner::factory::RunnerFactory;
    use crate::infra::worker_schema::rdb::RdbWorkerSchemaRepositoryImpl;
    use crate::infra::IdGeneratorWrapper;
    use crate::infra::{
        job::rdb::RdbChanJobRepositoryImpl, job_result::rdb::RdbJobResultRepositoryImpl,
        worker::rdb::RdbWorkerRepositoryImpl,
    };
    use crate::infra::{
        job::status::memory::MemoryJobStatusRepository,
        job_result::pubsub::chan::ChanJobResultPubSubRepositoryImpl, JobQueueConfig,
    };
    use infra_utils::infra::test::{setup_test_rdb_from, TEST_RUNTIME};
    use sqlx::Executor;
    use std::sync::Arc;

    pub fn setup_test_rdb_module() -> RdbChanRepositoryModule {
        use infra_utils::infra::{chan::ChanBuffer, test::truncate_tables};

        TEST_RUNTIME.block_on(async {
            let dir = if cfg!(feature = "mysql") {
                "../infra/sql/mysql"
            } else {
                "../infra/sql/sqlite"
            };
            let pool = setup_test_rdb_from(dir).await;
            pool.execute("SELECT 1;").await.expect("test connection");
            // not worker_schema
            truncate_tables(pool, vec!["job", "worker", "job_result"]).await;
            pool.execute("DELETE FROM worker_schema WHERE id > 100;")
                .await
                .expect("test connection");
            let runner_factory = RunnerFactory::new();
            runner_factory.load_plugins().await;
            let id_generator = Arc::new(IdGeneratorWrapper::new());
            RdbChanRepositoryModule {
                worker_schema_repository: RdbWorkerSchemaRepositoryImpl::new(
                    pool,
                    Arc::new(runner_factory),
                    id_generator,
                ),
                worker_repository: RdbWorkerRepositoryImpl::new(pool),
                job_repository: RdbChanJobRepositoryImpl::new(
                    Arc::new(JobQueueConfig::default()),
                    pool,
                ),
                job_result_repository: RdbJobResultRepositoryImpl::new(pool),
                memory_job_status_repository: Arc::new(MemoryJobStatusRepository::new()),
                chan_job_result_pubsub_repository: ChanJobResultPubSubRepositoryImpl::new(
                    ChanBuffer::new(None, 10000),
                    Arc::new(JobQueueConfig::default()),
                ),
                chan_job_queue_repository: ChanJobQueueRepositoryImpl::new(
                    Arc::new(JobQueueConfig::default()),
                    ChanBuffer::new(None, 10000),
                ),
            }
        })
    }
}
