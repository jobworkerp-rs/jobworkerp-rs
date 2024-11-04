use std::sync::Arc;

use self::{
    rdb::{RdbJobDispatcher, RdbJobDispatcherImpl},
    redis::{RedisJobDispatcher, RedisJobDispatcherImpl},
};
use super::{result_processor::ResultProcessorImpl, runner::map::RunnerFactoryWithPoolMap};
use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use async_trait::async_trait;
use chan::{ChanJobDispatcher, ChanJobDispatcherImpl};
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::{
    job::rdb::UseRdbChanJobRepository,
    module::{rdb::RdbChanRepositoryModule, redis::RedisRepositoryModule},
    runner::factory::RunnerFactory,
    IdGeneratorWrapper,
};
use proto::jobworkerp::data::StorageType;

pub mod chan;
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
                        rdb_chan_repositories.memory_job_status_repository.clone(),
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
