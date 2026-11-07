use std::sync::Arc;

use crate::infra::IdGeneratorWrapper;
use crate::infra::InfraConfigModule;
use crate::infra::job::queue::redis::{RedisJobQueueRepositoryImpl, UseRedisJobQueueRepository};
use crate::infra::job::queue::{JobQueueCancellationRepository, UseJobQueueCancellationRepository};
use crate::infra::job::redis::RedisJobRepositoryImpl;
use crate::infra::job::redis::UseRedisJobRepository;
use crate::infra::job::status::redis::RedisJobProcessingStatusRepository;
use crate::infra::job::status::{JobProcessingStatusRepository, UseJobProcessingStatusRepository};
use crate::infra::job_result::pubsub::redis::RedisJobResultPubSubRepositoryImpl;
use crate::infra::job_result::redis::RedisJobResultRepositoryImpl;
use crate::infra::job_result::redis::UseRedisJobResultRepository;
use crate::infra::load_job_queue_config_from_env;
use crate::infra::runner::redis::RedisRunnerRepositoryImpl;
use crate::infra::runner::redis::UseRedisRunnerRepository;
use crate::infra::worker::redis::{RedisWorkerRepositoryImpl, UseRedisWorkerRepository};
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
impl<T: UseRedisRepositoryModule> UseRedisJobQueueRepository for T {
    fn redis_job_queue_repository(&self) -> &RedisJobQueueRepositoryImpl {
        &self.redis_repository_module().redis_job_queue_repository
    }
}
impl UseJobQueueCancellationRepository for RedisRepositoryModule {
    fn job_queue_cancellation_repository(&self) -> Arc<dyn JobQueueCancellationRepository> {
        Arc::new(self.redis_job_queue_repository.clone())
    }
}

impl UseJobProcessingStatusRepository for RedisRepositoryModule {
    fn job_processing_status_repository(&self) -> Arc<dyn JobProcessingStatusRepository> {
        self.redis_job_processing_status_repository.clone()
    }
}

// redis and rdb module for DI
#[derive(Clone, DebugStub)]
pub struct RedisRepositoryModule {
    #[debug_stub = "&`static RedisPool"]
    pub redis_pool: &'static infra_utils::infra::redis::RedisPool,
    /// Redis pool for blocking operations like BLPOP.
    /// This pool has response_timeout disabled to allow indefinite waiting.
    #[debug_stub = "&`static RedisPool"]
    pub redis_blocking_pool: &'static infra_utils::infra::redis::RedisPool,
    pub redis_client: RedisClient,
    pub redis_runner_repository: RedisRunnerRepositoryImpl,
    pub redis_worker_repository: RedisWorkerRepositoryImpl,
    pub redis_job_repository: RedisJobRepositoryImpl,
    pub redis_job_result_repository: RedisJobResultRepositoryImpl,
    pub redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl,
    pub redis_job_queue_repository: RedisJobQueueRepositoryImpl,
    pub redis_job_processing_status_repository: Arc<RedisJobProcessingStatusRepository>,
}

impl RedisRepositoryModule {
    pub async fn new_by_env(
        worker_expire_sec: Option<usize>,
        id_generator: Arc<IdGeneratorWrapper>,
        runner_spec_factory: Arc<RunnerSpecFactory>,
    ) -> Self {
        let redis_pool = super::super::resource::setup_redis_pool_by_env().await;
        let redis_blocking_pool = super::super::resource::setup_redis_blocking_pool_by_env().await;
        let redis_client = super::super::resource::setup_redis_client_by_env().await;
        let job_queue_config = Arc::new(load_job_queue_config_from_env().unwrap());
        RedisRepositoryModule {
            redis_pool,
            redis_job_processing_status_repository: Arc::new(
                RedisJobProcessingStatusRepository::new(redis_pool),
            ),
            redis_blocking_pool,
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
                redis_blocking_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client.clone(),
                job_queue_config.clone(),
            ),
            redis_job_queue_repository: RedisJobQueueRepositoryImpl::new(
                job_queue_config,
                redis_pool,
                redis_blocking_pool,
                redis_client.clone(),
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
        let redis_blocking_pool =
            super::super::resource::setup_redis_blocking_pool(conf.clone()).await;
        let redis_client = super::super::resource::setup_redis_client(conf).await;
        RedisRepositoryModule {
            redis_pool,
            redis_job_processing_status_repository: Arc::new(
                RedisJobProcessingStatusRepository::new(redis_pool),
            ),
            redis_blocking_pool,
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
                redis_blocking_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client.clone(),
                config_module.job_queue_config.clone(),
            ),
            redis_job_queue_repository: RedisJobQueueRepositoryImpl::new(
                config_module.job_queue_config.clone(),
                redis_pool,
                redis_blocking_pool,
                redis_client,
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
    use crate::infra::job::status::redis::RedisJobProcessingStatusRepository;
    use crate::infra::{
        IdGeneratorWrapper,
        job::queue::redis::RedisJobQueueRepositoryImpl,
        job::redis::RedisJobRepositoryImpl,
        job_result::{
            pubsub::redis::RedisJobResultPubSubRepositoryImpl, redis::RedisJobResultRepositoryImpl,
        },
        module::test::TEST_PLUGIN_DIR,
        runner::redis::RedisRunnerRepositoryImpl,
        worker::redis::RedisWorkerRepositoryImpl,
    };
    use anyhow::Context;
    use infra_utils::infra::redis::{RedisPool, new_named_redis_pool};
    use infra_utils::infra::test::{
        REDIS_CONFIG, setup_test_redis_blocking_pool, setup_test_redis_client,
        setup_test_redis_pool,
    };
    use jobworkerp_runner::runner::{
        factory::RunnerSpecFactory, mcp::proxy::McpServerFactory, plugins::Plugins,
    };
    use std::sync::Arc;

    // create RedsRepositoryModule for test
    pub async fn setup_test_redis_module() -> RedisRepositoryModule {
        let redis_pool = setup_test_redis_pool().await;
        let redis_blocking_pool = setup_test_redis_blocking_pool().await;
        setup_test_redis_module_with_pools(redis_pool, redis_blocking_pool, true).await
    }

    /// Client name of the blocking pool built by `setup_test_redis_module_through_proxy`,
    /// for releasing the BLPOP connections a stopped test worker leaves behind.
    pub fn proxied_blocking_client_name(client_name_prefix: &str) -> String {
        format!("{client_name_prefix}-blocking")
    }

    /// Create a test Redis module with isolated named pools and no global database flush,
    /// whose normal pool connects through `proxy_url`. Blocking operations and pub/sub
    /// keep using the test Redis URL.
    pub async fn setup_test_redis_module_through_proxy(
        client_name_prefix: &str,
        proxy_url: &str,
    ) -> RedisRepositoryModule {
        async fn leaked_pool(
            blocking: bool,
            url: Option<&str>,
            name: String,
        ) -> &'static RedisPool {
            let mut config = REDIS_CONFIG.clone();
            config.blocking = blocking;
            if let Some(url) = url {
                config.url = url.to_string();
            }
            Box::leak(Box::new(new_named_redis_pool(config, &name).await.unwrap()))
        }
        let redis_pool = leaked_pool(
            false,
            Some(proxy_url),
            format!("{client_name_prefix}-normal"),
        )
        .await;
        let redis_blocking_pool =
            leaked_pool(true, None, proxied_blocking_client_name(client_name_prefix)).await;
        setup_test_redis_module_with_pools(redis_pool, redis_blocking_pool, false).await
    }

    async fn setup_test_redis_module_with_pools(
        redis_pool: &'static RedisPool,
        redis_blocking_pool: &'static RedisPool,
        flush_database: bool,
    ) -> RedisRepositoryModule {
        // use normal redis
        let job_queue_config = Arc::new(crate::infra::JobQueueConfig::default());
        let redis_client = setup_test_redis_client().unwrap();

        if flush_database {
            let mut rcon = redis_pool
                .get()
                .await
                .context(format!(
                    "failed to get redis connection: {:?}",
                    redis_pool.clone()
                ))
                .unwrap();
            redis::cmd("FLUSHALL")
                .query_async::<()>(&mut *rcon)
                .await
                .unwrap();
        }

        let p = RunnerSpecFactory::new(
            Arc::new(Plugins::new()),
            Arc::new(McpServerFactory::default()),
        );
        p.load_plugins_from(TEST_PLUGIN_DIR).await;
        RedisRepositoryModule {
            redis_pool,
            redis_job_processing_status_repository: Arc::new(
                RedisJobProcessingStatusRepository::new(redis_pool),
            ),
            redis_blocking_pool,
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
                redis_blocking_pool,
                redis_client.clone(),
            ),
            redis_job_result_repository: RedisJobResultRepositoryImpl::new(
                job_queue_config.clone(),
                redis_pool,
            ),
            redis_job_result_pubsub_repository: RedisJobResultPubSubRepositoryImpl::new(
                redis_client.clone(),
                job_queue_config.clone(),
            ),
            redis_job_queue_repository: RedisJobQueueRepositoryImpl::new(
                job_queue_config,
                redis_pool,
                redis_blocking_pool,
                redis_client.clone(),
            ),
        }
    }
}

#[cfg(test)]
mod proxied_pool_tests {
    use super::test::{proxied_blocking_client_name, setup_test_redis_module_through_proxy};
    use infra_utils::infra::test::{REDIS_CONFIG, TEST_RUNTIME};

    #[test]
    fn proxied_blocking_pool_uses_the_published_client_name() -> anyhow::Result<()> {
        TEST_RUNTIME.block_on(async {
            let prefix = format!(
                "proxied-name-test-{}",
                command_utils::util::datetime::now_millis()
            );
            let module = setup_test_redis_module_through_proxy(&prefix, &REDIS_CONFIG.url).await;
            let mut connection = module.redis_blocking_pool.get().await?;
            let name: Option<String> = redis::cmd("CLIENT")
                .arg("GETNAME")
                .query_async(&mut *connection)
                .await?;
            assert_eq!(name, Some(proxied_blocking_client_name(&prefix)));
            Ok(())
        })
    }
}
