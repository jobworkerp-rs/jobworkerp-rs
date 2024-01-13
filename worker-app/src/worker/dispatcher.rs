use std::sync::Arc;

use crate::plugins::Plugins;
use anyhow::Result;
use app::{
    app::StorageType,
    module::{AppConfigModule, AppModule},
};
use async_trait::async_trait;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::{
    job::{rdb::RdbJobRepositoryImpl, redis::RedisJobRepositoryImpl},
    IdGeneratorWrapper,
};

use self::{
    rdb::{RdbJobDispatcher, RdbJobDispatcherImpl},
    redis::{RedisJobDispatcher, RedisJobDispatcherImpl},
};

use super::{result_processor::ResultProcessorImpl, runner::map::RunnerFactoryWithPoolMap};

pub mod rdb;
pub mod redis;
pub mod redis_run_after;

#[async_trait]
pub trait JobDispatcher: Send + Sync + 'static {
    fn dispatch_jobs(&'static self, lock: ShutdownLock) -> Result<()>
    where
        Self: Send + Sync + 'static;
}
// TODO divide into three traits (redis, rdb and redis+rdb)
pub struct JobDispatcherFactory {}
pub struct HybridJobDispatcherImpl {
    pub rdb_job_dispatcher: RdbJobDispatcherImpl,
    pub redis_job_dispatcher: RedisJobDispatcherImpl,
}

impl JobDispatcherFactory {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        id_generator: Arc<IdGeneratorWrapper>,
        config_module: Arc<AppConfigModule>,
        app_module: Arc<AppModule>,
        rdb_job_repository_opt: Option<Arc<RdbJobRepositoryImpl>>,
        redis_job_repository_opt: Option<Arc<RedisJobRepositoryImpl>>,
        plugins: Arc<Plugins>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
        result_processor: Arc<ResultProcessorImpl>,
    ) -> Box<dyn JobDispatcher + 'static> {
        match (
            app_module.config_module.storage_type(),
            rdb_job_repository_opt.clone(),
            redis_job_repository_opt,
        ) {
            (StorageType::Redis, _, Some(redis_job_repository)) => {
                Box::new(RedisJobDispatcherImpl::new(
                    id_generator,
                    config_module,
                    redis_job_repository,
                    None,
                    app_module,
                    plugins,
                    runner_pool_map,
                    result_processor,
                ))
            }
            (StorageType::RDB, Some(rdb_job_repository), _) => Box::new(RdbJobDispatcherImpl::new(
                id_generator,
                config_module,
                rdb_job_repository,
                app_module,
                plugins,
                runner_pool_map,
                result_processor,
            )),
            (StorageType::Hybrid, Some(rdb_job_repository), Some(redis_job_repository)) => {
                Box::new(HybridJobDispatcherImpl {
                    rdb_job_dispatcher: RdbJobDispatcherImpl::new(
                        id_generator.clone(),
                        config_module.clone(),
                        rdb_job_repository.clone(),
                        app_module.clone(),
                        plugins.clone(),
                        runner_pool_map.clone(),
                        result_processor.clone(),
                    ),
                    redis_job_dispatcher: RedisJobDispatcherImpl::new(
                        id_generator,
                        config_module,
                        redis_job_repository,
                        Some(rdb_job_repository),
                        app_module,
                        plugins,
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
