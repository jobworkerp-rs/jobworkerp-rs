use crate::app::job::hybrid::HybridJobAppImpl;
use crate::app::job::rdb_chan::RdbChanJobAppImpl;
use crate::app::job::JobApp;
use crate::app::job_result::hybrid::HybridJobResultAppImpl;
use crate::app::job_result::rdb::RdbJobResultAppImpl;
use crate::app::job_result::JobResultApp;
use crate::app::runner::hybrid::HybridRunnerAppImpl;
use crate::app::runner::rdb::RdbRunnerAppImpl;
use crate::app::runner::RunnerApp;
use crate::app::worker::hybrid::HybridWorkerAppImpl;
use crate::app::worker::rdb::RdbWorkerAppImpl;
use crate::app::worker::WorkerApp;
use crate::app::{StorageConfig, WorkerConfig};
use anyhow::Result;
use infra::infra::module::rdb::RdbChanRepositoryModule;
use infra::infra::module::{HybridRepositoryModule, RedisRdbOptionalRepositoryModule};
use infra::infra::runner::factory::RunnerFactory;
use infra::infra::{IdGeneratorWrapper, JobQueueConfig};
use infra_utils::infra::memory::MemoryCacheImpl;
use proto::jobworkerp::data::StorageType;
use std::sync::Arc;
use std::time::Duration;

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
    pub runner_factory: Arc<RunnerFactory>,
}
impl AppConfigModule {
    pub fn new_by_env(runner_factory: Arc<RunnerFactory>) -> Self {
        Self {
            storage_config: Arc::new(load_storage_config()),
            worker_config: Arc::new(load_worker_config()),
            // XXX check dependency(infra config?)
            job_queue_config: Arc::new(
                infra::infra::load_job_queue_config_from_env().unwrap_or_default(),
            ),
            runner_factory,
        }
    }
    // shortcut method
    pub fn storage_type(&self) -> StorageType {
        self.storage_config.r#type
    }
    // shortcut method
    // pub fn use_redis_only(&self) -> bool {
    //     self.storage_config.r#type == StorageType::Redis
    // }
}

#[derive(Clone, Debug)]
pub struct AppModule {
    pub config_module: Arc<AppConfigModule>,
    pub repositories: Arc<RedisRdbOptionalRepositoryModule>,
    pub worker_app: Arc<dyn WorkerApp + 'static>,
    pub job_app: Arc<dyn JobApp + 'static>,
    pub job_result_app: Arc<dyn JobResultApp + 'static>,
    pub runner_app: Arc<dyn RunnerApp + 'static>,
}

impl AppModule {
    pub async fn new_by_env(config_module: Arc<AppConfigModule>) -> Result<Self> {
        // TODO from env
        //TODO recover redis records from rdb if option is enabled
        // TODO memory cache をinfraでも利用する場合はinfra層でモジュール化しておく
        let mc_config = envy::prefixed("MEMORY_CACHE_")
            .from_env::<infra_utils::infra::memory::MemoryCacheConfig>()
            .unwrap_or_default();
        let job_queue_config = config_module.job_queue_config.clone();
        let id_generator = Arc::new(IdGeneratorWrapper::new());
        let descriptor_cache = Arc::new(MemoryCacheImpl::new(&mc_config, None));
        match config_module.storage_type() {
            StorageType::Standalone => {
                let repositories = Arc::new(
                    RdbChanRepositoryModule::new_by_env(
                        job_queue_config.clone(),
                        config_module.runner_factory.clone(),
                        id_generator.clone(),
                    )
                    .await,
                );
                let runner_app = Arc::new(RdbRunnerAppImpl::new(
                    config_module.storage_config.clone(),
                    &mc_config,
                    repositories.clone(),
                    descriptor_cache.clone(),
                ));
                let worker_app = Arc::new(RdbWorkerAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    infra_utils::infra::memory::MemoryCacheImpl::new(
                        &mc_config,
                        Some(Duration::from_secs(5 * 60)),
                    ),
                    repositories.clone(),
                    descriptor_cache.clone(),
                    runner_app.clone(),
                ));
                let job_result_app = Arc::new(RdbJobResultAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                ));
                let job_app = Arc::new(RdbChanJobAppImpl::new(
                    config_module.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                    infra_utils::infra::memory::MemoryCacheImpl::new(
                        &mc_config,
                        Some(Duration::from_secs(5)),
                    ),
                ));
                Ok(AppModule {
                    config_module,
                    repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
                    worker_app,
                    job_app,
                    job_result_app,
                    runner_app,
                })
            }
            // TODO not used now (remove or implement later)
            // StorageType::RedisOnly => {
            //     let repositories = Arc::new(
            //         RedisRepositoryModule::new_by_env(
            //             None,
            //             id_generator.clone(),
            //             config_module.runner_factory.clone(),
            //         )
            //         .await,
            //     );
            //     let runner_app = Arc::new(RedisRunnerAppImpl::new(
            //         config_module.storage_config.clone(),
            //         id_generator.clone(),
            //         &mc_config,
            //         repositories.clone(),
            //         descriptor_cache.clone(),
            //     ));
            //     let worker_app = Arc::new(RedisWorkerAppImpl::new(
            //         config_module.storage_config.clone(),
            //         id_generator.clone(),
            //         infra_utils::infra::memory::MemoryCacheImpl::new(
            //             &mc_config,
            //             Some(Duration::from_secs(60 * 60)),
            //         ),
            //         repositories.clone(),
            //         descriptor_cache.clone(),
            //         runner_app.clone(),
            //     ));
            //     let job_result_app = Arc::new(RedisJobResultAppImpl::new(
            //         config_module.storage_config.clone(),
            //         repositories.clone(),
            //         worker_app.clone(),
            //     ));
            //     let job_app = Arc::new(RedisJobAppImpl::new(
            //         job_queue_config.clone(),
            //         id_generator,
            //         repositories.clone(),
            //         worker_app.clone(),
            //         job_result_app.clone(),
            //     ));
            //     Ok(AppModule {
            //         config_module,
            //         repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
            //         worker_app,
            //         job_app,
            //         job_result_app,
            //         runner_app,
            //     })
            // }
            StorageType::Scalable => {
                let repositories = Arc::new(
                    HybridRepositoryModule::new_by_env(
                        job_queue_config,
                        id_generator.clone(),
                        config_module.runner_factory.clone(),
                    )
                    .await,
                );
                // TODO imprement and use hybrid runner app
                let runner_app = Arc::new(HybridRunnerAppImpl::new(
                    config_module.storage_config.clone(),
                    &mc_config,
                    repositories.clone(),
                    descriptor_cache.clone(),
                ));
                let worker_app = Arc::new(HybridWorkerAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    infra_utils::infra::memory::MemoryCacheImpl::new(
                        &mc_config,
                        Some(Duration::from_secs(5 * 60)),
                    ),
                    repositories.clone(),
                    descriptor_cache,
                    runner_app.clone(),
                ));
                let job_app = Arc::new(HybridJobAppImpl::new(
                    config_module.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                    infra_utils::infra::memory::MemoryCacheImpl::new(
                        &mc_config,
                        Some(Duration::from_secs(60)),
                    ),
                ));
                let job_result_app = Arc::new(HybridJobResultAppImpl::new(
                    config_module.storage_config.clone(),
                    id_generator.clone(),
                    repositories.clone(),
                    worker_app.clone(),
                ));
                Ok(AppModule {
                    config_module,
                    repositories: Arc::new(RedisRdbOptionalRepositoryModule::from(repositories)),
                    worker_app,
                    job_app,
                    job_result_app,
                    runner_app,
                })
            }
        }
    }
    async fn reload_jobs_from_rdb_with_config(&self) -> Result<()> {
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
    // call on start all-in-one binary main
    pub async fn on_start_all_in_one(&self) -> Result<()> {
        self.load_runner().await?;
        self.reload_jobs_from_rdb_with_config().await?;
        Ok(())
    }
    // call on start worker binary main
    pub async fn on_start_worker(&self) -> Result<()> {
        self.load_runner().await?;
        self.reload_jobs_from_rdb_with_config().await?;
        Ok(())
    }
    // call on start grpc-front binary main
    pub async fn on_start_front(&self) -> Result<()> {
        self.load_runner().await?;
        Ok(())
    }
    async fn load_runner(&self) -> Result<()> {
        self.runner_app.load_runner().await?;
        Ok(())
    }
}
