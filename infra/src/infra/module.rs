pub mod rdb;
pub mod redis;

use std::sync::Arc;

use self::rdb::{RdbRepositoryModule, UseRdbRepositoryModule};
use self::redis::{RedisRepositoryModule, UseRedisRepositoryModule};
use super::InfraConfigModule;

// redis and rdb module for DI
#[derive(Clone, Debug)]
pub struct HybridRepositoryModule {
    pub redis_module: RedisRepositoryModule,
    pub rdb_module: RdbRepositoryModule,
}

impl HybridRepositoryModule {
    // TODO to config?
    const DEFAULT_WORKER_REDIS_EXPIRE_SEC: Option<usize> = Some(60 * 60);
    pub async fn new(infra_config_module: &InfraConfigModule) -> Self {
        let redis_module =
            RedisRepositoryModule::new(infra_config_module, Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC)
                .await;
        let rdb_module = RdbRepositoryModule::new(infra_config_module).await;
        HybridRepositoryModule {
            redis_module,
            rdb_module,
        }
    }
    pub async fn new_by_env() -> Self {
        let redis_module =
            RedisRepositoryModule::new_by_env(Self::DEFAULT_WORKER_REDIS_EXPIRE_SEC).await;
        let rdb_module = RdbRepositoryModule::new_by_env().await;
        HybridRepositoryModule {
            redis_module,
            rdb_module,
        }
    }
}
impl UseRedisRepositoryModule for HybridRepositoryModule {
    fn redis_repository_module(&self) -> &RedisRepositoryModule {
        &self.redis_module
    }
}

impl UseRdbRepositoryModule for HybridRepositoryModule {
    fn rdb_repository_module(&self) -> &RdbRepositoryModule {
        &self.rdb_module
    }
}

// for app module container
#[derive(Clone, Debug)]
pub struct RedisRdbOptionalRepositoryModule {
    pub redis_module: Option<Arc<RedisRepositoryModule>>,
    pub rdb_module: Option<Arc<RdbRepositoryModule>>,
}
impl From<Arc<RedisRepositoryModule>> for RedisRdbOptionalRepositoryModule {
    fn from(redis_module: Arc<RedisRepositoryModule>) -> Self {
        RedisRdbOptionalRepositoryModule {
            redis_module: Some(redis_module),
            rdb_module: None,
        }
    }
}
impl From<Arc<RdbRepositoryModule>> for RedisRdbOptionalRepositoryModule {
    fn from(rdb_module: Arc<RdbRepositoryModule>) -> Self {
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
            rdb_module: Some(Arc::new(hybrid_module.rdb_module.clone())),
        }
    }
}
