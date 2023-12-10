use crate::app::job::hybrid::HybridJobAppImpl;
use crate::app::job::rdb::RdbJobAppImpl;
use crate::app::job::redis::RedisJobAppImpl;
use crate::app::job::JobApp;
use crate::app::job_result::hybrid::HybridJobResultAppImpl;
use crate::app::job_result::rdb::RdbJobResultAppImpl;
use crate::app::job_result::redis::RedisJobResultAppImpl;
use crate::app::job_result::JobResultApp;
use crate::app::worker::hybrid::HybridWorkerAppImpl;
use crate::app::worker::rdb::RdbWorkerAppImpl;
use crate::app::worker::redis::RedisWorkerAppImpl;
use crate::app::worker::WorkerApp;
use crate::app::{StorageConfig, StorageType, WorkerConfig};
use anyhow::Result;
use infra::infra::module::rdb::RdbRepositoryModule;
use infra::infra::module::redis::RedisRepositoryModule;
use infra::infra::module::{HybridRepositoryModule, RedisRdbOptionalRepositoryModule};
use infra::infra::{IdGeneratorWrapper, JobQueueConfig};
use proto::jobworkerp::data::{Job, Worker};
use std::sync::Arc;

pub fn load_storage_config() -> StorageConfig {
    envy::prefixed("STORAGE_")
        .from_env::<StorageConfig>()
        .unwrap_or_default()
}
pub fn load_worker_config() -> WorkerConfig {
    envy::prefixed("WORKER_")
        .from_env::<WorkerConfig>()
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct AppConfigModule {
    pub storage_config: Arc<StorageConfig>,
    pub worker_config: Arc<WorkerConfig>,
    pub job_queue_config: Arc<JobQueueConfig>,
}
impl AppConfigModule {
    pub fn new_by_env() -> Self {
        Self {
            storage_config: Arc::new(load_storage_config()),
            worker_config: Arc::new(load_worker_config()),
            // XXX check dependency(infra config?)
            job_queue_config: Arc::new(
                infra::infra::load_job_queue_config_from_env().unwrap_or_default(),
            ),
        }
    }
    // shortcut method
    pub fn storage_type(&self) -> StorageType {
        self.storage_config.r#type
    }
    // shortcut method
    pub fn use_rdb(&self) -> bool {
        self.storage_config.r#type != StorageType::Redis
    }
    // shortcut method
    pub fn use_redis(&self) -> bool {
        self.storage_config.r#type != StorageType::RDB
    }
    // shortcut method
    pub fn use_redis_only(&self) -> bool {
        self.storage_config.r#type == StorageType::Redis
    }
}

#[derive(Clone)]
pub struct AppModule {
    pub config_module: Arc<AppConfigModule>,
    pub repositories: Arc<RedisRdbOptionalRepositoryModule>,
    pub worker_app: Arc<dyn WorkerApp + 'static>,
    pub job_app: Arc<dyn JobApp + 'static>,
    pub job_result_app: Arc<dyn JobResultApp + 'static>,
}

impl AppModule {
    pub async fn new_by_env(config_module: Arc<AppConfigModule>) -> Result<Self> {
        // TODO from env
        //TODO recover redis records from rdb if option is enabled
        // TODO memory cache をinfraでも利用する場合はinfra層でモジュール化しておく
        let mc_config = envy::prefixed("MEMORY_CACHE_")
            .from_env::<common::infra::memory::MemoryCacheConfig>()
            .unwrap_or_default();
        let job_queue_config = config_module.job_queue_config.clone();
        let id_generator = Arc::new(IdGeneratorWrapper::new());
        match config_module.storage_type() {
            StorageType::RDB => {
                let repositories = Arc::new(RdbRepositoryModule::new_by_env().await);
                let worker_app = Arc::new(RdbWorkerAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Worker>>(&mc_config),
                    repositories.clone(),
                ));
                let job_result_app = Arc::new(RdbJobResultAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                ));
                let job_app = Arc::new(RdbJobAppImpl::new(
                    job_queue_config,
                    config_module.storage_config.clone(),
                    id_generator,
                    repositories.clone(),
                    worker_app.clone(),
                    job_result_app.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Job>>(&mc_config),
                ));
                Ok(AppModule {
                    config_module,
                    repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
                    worker_app,
                    job_app,
                    job_result_app,
                })
            }
            StorageType::Redis => {
                let repositories = Arc::new(RedisRepositoryModule::new_by_env(None).await);
                let worker_app = Arc::new(RedisWorkerAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Worker>>(&mc_config),
                    repositories.clone(),
                ));
                let job_result_app = Arc::new(RedisJobResultAppImpl::new(
                    config_module.storage_config.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                ));
                let job_app = Arc::new(RedisJobAppImpl::new(
                    job_queue_config.clone(),
                    id_generator,
                    repositories.clone(),
                    worker_app.clone(),
                    job_result_app.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Job>>(&mc_config),
                ));
                Ok(AppModule {
                    config_module,
                    repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
                    worker_app,
                    job_app,
                    job_result_app,
                })
            }
            StorageType::Hybrid => {
                let repositories = Arc::new(HybridRepositoryModule::new_by_env().await);
                let worker_app = Arc::new(HybridWorkerAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Worker>>(&mc_config),
                    repositories.clone(),
                ));
                let job_app = Arc::new(HybridJobAppImpl::new(
                    config_module.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                    common::infra::memory::new_memory_cache::<Arc<String>, Vec<Job>>(&mc_config),
                ));
                let job_result_app = Arc::new(HybridJobResultAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator,
                    repositories.clone(),
                    worker_app.clone(),
                ));
                Ok(AppModule {
                    config_module,
                    repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
                    worker_app,
                    job_app,
                    job_result_app,
                })
            }
        }
    }
    pub async fn reload_jobs_from_rdb_with_config(&self) -> Result<()> {
        if self
            .config_module
            .storage_config
            .should_restore_at_startup()
        {
            // reload jobs from rdb to redis (for recovery)
            self.job_app.restore_jobs_from_rdb(false, None).await?;
        }
        Ok(())
    }
}
