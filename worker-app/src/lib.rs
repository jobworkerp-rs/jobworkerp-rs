use app::{
    app::StorageType,
    module::{AppConfigModule, AppModule},
};
use infra::infra::{job::rdb::UseRdbJobRepository, IdGeneratorWrapper};
use plugins::Plugins;
use std::sync::Arc;
use worker::{
    dispatcher::{JobDispatcher, JobDispatcherFactory},
    result_processor::ResultProcessorImpl,
    runner::map::RunnerFactoryWithPoolMap,
};

pub mod plugins;
pub mod worker;

pub struct WorkerModules {
    pub job_dispatcher: Box<dyn JobDispatcher + 'static>,
}

impl WorkerModules {
    pub fn new(
        config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        app_module: Arc<AppModule>,
        plugins: Arc<Plugins>,
    ) -> Self {
        // XXX static?
        let runner_pool_map = Arc::new(RunnerFactoryWithPoolMap::new(
            plugins.clone(),
            config_module.worker_config.clone(),
        ));
        let result_processor = Arc::new(ResultProcessorImpl::new(
            config_module.clone(),
            app_module.clone(),
        ));
        match config_module.storage_type() {
            StorageType::Redis => {
                let job_dispatcher = JobDispatcherFactory::create(
                    id_generator.clone(),
                    config_module.clone(),
                    app_module.clone(),
                    None,
                    Some(Arc::new(
                        app_module
                            .repositories
                            .redis_module
                            .as_ref()
                            .unwrap()
                            .redis_job_repository
                            .clone(),
                    )),
                    plugins,
                    runner_pool_map,
                    result_processor,
                );
                Self { job_dispatcher }
            }
            StorageType::RDB => {
                let job_dispatcher = JobDispatcherFactory::create(
                    id_generator.clone(),
                    config_module.clone(),
                    app_module.clone(),
                    Some(Arc::new(
                        app_module
                            .repositories
                            .rdb_module
                            .as_ref()
                            .unwrap()
                            .rdb_job_repository()
                            .clone(),
                    )),
                    None,
                    plugins,
                    runner_pool_map,
                    result_processor,
                );
                Self { job_dispatcher }
            }
            StorageType::Hybrid => {
                let job_dispatcher = JobDispatcherFactory::create(
                    id_generator.clone(),
                    config_module.clone(),
                    app_module.clone(),
                    Some(Arc::new(
                        app_module
                            .repositories
                            .rdb_module
                            .as_ref()
                            .unwrap()
                            .rdb_job_repository()
                            .clone(),
                    )),
                    Some(Arc::new(
                        app_module
                            .repositories
                            .redis_module
                            .as_ref()
                            .unwrap()
                            .redis_job_repository
                            .clone(),
                    )),
                    plugins,
                    runner_pool_map,
                    result_processor,
                );
                Self { job_dispatcher }
            }
        }
    }
}
