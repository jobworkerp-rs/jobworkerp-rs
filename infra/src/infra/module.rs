pub mod rdb;
pub mod redis;

use self::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use super::{plugins::Plugins, IdGeneratorWrapper, InfraConfigModule, JobQueueConfig};
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
        plugins: Arc<Plugins>,
    ) -> Self {
        let redis_module = RedisRepositoryModule::new(
            infra_config_module,
            id_generator,
            plugins.clone(),
            Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC,
        )
        .await;
        let rdb_module = RdbChanRepositoryModule::new(infra_config_module, plugins).await;
        HybridRepositoryModule {
            redis_module,
            rdb_chan_module: rdb_module,
        }
    }
    pub async fn new_by_env(
        job_queue_config: Arc<JobQueueConfig>,
        id_generator: Arc<IdGeneratorWrapper>,
        plugins: Arc<Plugins>,
    ) -> Self {
        let redis_module = RedisRepositoryModule::new_by_env(
            Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC,
            id_generator,
            plugins.clone(),
        )
        .await;
        let rdb_module = RdbChanRepositoryModule::new_by_env(job_queue_config, plugins).await;
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
