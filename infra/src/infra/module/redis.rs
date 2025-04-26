use std::sync::Arc;

use crate::infra::job::redis::RedisJobRepositoryImpl;
use crate::infra::job::redis::UseRedisJobRepository;
use crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
use crate::infra::job_result::redis::RedisJobResultRepositoryImpl;
use crate::infra::job_result::redis::UseRedisJobResultRepository;
use crate::infra::load_job_queue_config_from_env;
use crate::infra::runner::redis::RedisRunnerRepositoryImpl;
use crate::infra::runner::redis::UseRedisRunnerRepository;
use crate::infra::worker::redis::{RedisWorkerRepositoryImpl, UseRedisWorkerRepository};
use crate::infra::IdGeneratorWrapper;
use crate::infra::InfraConfigModule;
use debug_stub_derive::DebugStub;
use infra_utils::infra::redis::RedisClient;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;

pub trait UseRedisRepositoryModule {
    fn redis_repository_module(&self) -> &RedisRepositoryModule;
}
impl<T: UseRedisRepositoryModule> UseRedisRunnerRepository for T {
    fn redis_runner_repository(&self) -> &RedisRunnerRepositoryImpl {
        &self.redis_repository_module().redis_runner_repository
    }
}
impl<T: UseRedisRepositoryModule> UseRedisWorkerRepository for T {
    fn redis_worker_repository(&self) -> &RedisWorkerRepositoryImpl {
        &self.redis_repository_module().redis_worker_repository
    }
}
impl<T: UseRedisRepositoryModule> UseRedisJobRepository for T {
    fn redis_job_repository(&self) -> &RedisJobRepositoryImpl {
        &self.redis_repository_module().redis_job_repository
    }
}
impl<T: UseRedisRepositoryModule> UseRedisJobResultRepository for T {
    fn redis_job_result_repository(&self) -> &RedisJobResultRepositoryImpl {
        &self.redis_repository_module().redis_job_result_repository
    }
}

// redis and rdb module for DI
#[derive(Clone, DebugStub)]
pub struct RedisRepositoryModule {
    #[debug_stub = "&`static deadpool_redis::Pool"]
    pub redis_pool: &'static deadpool_redis::Pool,
    pub redis_client: RedisClient,
    pub redis_runner_repository: RedisRunnerRepositoryImpl,
    pub redis_worker_repository: RedisWorkerRepositoryImpl,
    pub redis_job_repository: RedisJobRepositoryImpl,
    pub redis_job_result_repository: RedisJobResultRepositoryImpl,
    pub redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl,
}

impl RedisRepositoryModule {
    pub async fn new_by_env(
        worker_expire_sec: Option<usize>,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_spec_factory: Arc<RunnerSpecFactory>,
    ) -> Self {
        let redis_pool = super::super::resource::setup_redis_pool_by_env().await;
        let redis_client = super::super::resource::setup_redis_client_by_env().await;
        let job_queue_config = Arc::new(load_job_queue_config_from_env().unwrap());
        RedisRepositoryModule {
            redis_pool,
            redis_client: redis_client.clone(),
            redis_runner_repository: RedisRunnerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                id_generator,
                runner_spec_factory,
            ),
            redis_worker_repository: RedisWorkerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                worker_expire_sec,
            ),
            redis_job_repository: RedisJobRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                job_queue_config,
            ),
        }
    }
    pub async fn new(
        config_module: &InfraConfigModule,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_spec_factory: Arc<RunnerSpecFactory>,
        worker_expire_sec: Option<usize>,
    ) -> Self {
        let conf = config_module.redis_config.clone().unwrap();
        let redis_pool = super::super::resource::setup_redis_pool(conf.clone()).await;
        let redis_client = super::super::resource::setup_redis_client(conf).await;
        RedisRepositoryModule {
            redis_pool,
            redis_client: redis_client.clone(),
            redis_runner_repository: RedisRunnerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                id_generator,
                runner_spec_factory,
            ),
            redis_worker_repository: RedisWorkerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                worker_expire_sec,
            ),
            redis_job_repository: RedisJobRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                redis_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                config_module.job_queue_config.clone(),
            ),
        }
    }
}
impl UseRedisRepositoryModule for RedisRepositoryModule {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        self
    }
}
#[cfg(any(test, feature = "test-utils"))]
pub mod test {
    use super::RedisRepositoryModule;
    use crate::infra::{
        job::redis::RedisJobRepositoryImpl,
        job_result::{
            pubsub::redis::RedisJobResultPubSubRepositoryImpl, redis::RedisJobResultRepositoryImpl,
        },
        module::test::TEST_PLUGIN_DIR,
        runner::redis::RedisRunnerRepositoryImpl,
        worker::redis::RedisWorkerRepositoryImpl,
        IdGeneratorWrapper,
    };
    use anyhow::Context;
    use infra_utils::infra::test::{setup_test_redis_client, setup_test_redis_pool};
    use jobworkerp_runner::runner::{
        factory::RunnerSpecFactory, mcp::client::McpServerFactory, plugins::Plugins,
    };
    use std::sync::Arc;

    // create RedsRepositoryModule for test
    pub async fn setup_test_redis_module() -> RedisRepositoryModule {
        // use normal redis
        let job_queue_config = Arc::new(crate::infra::JobQueueConfig::default());
        let redis_pool = setup_test_redis_pool().await;
        let redis_client = setup_test_redis_client().unwrap();

        // flush all
        let mut rcon = redis_pool
            .get()
            .await
            .context(format!(
                "failed to get redis connection: {:?}",
                redis_pool.clone()
            ))
            .unwrap();
        redis::cmd("FLUSHALL")
            .query_async::<()>(&mut rcon)
            .await
            .unwrap();

        let p = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        p.load_plugins_from(TEST_PLUGIN_DIR).await;
        RedisRepositoryModule {
            redis_pool,
            redis_client: redis_client.clone(),
            redis_runner_repository: RedisRunnerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                Arc::new(IdGeneratorWrapper::new()),
                Arc::new(p),
            ),
            redis_worker_repository: RedisWorkerRepositoryImpl::new(
                redis_pool,
                redis_client.clone(),
                None,
            ),
            redis_job_repository: RedisJobRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client,
                job_queue_config,
            ),
        }
    }
}
