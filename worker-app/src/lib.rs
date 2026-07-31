use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use infra::infra::IdGeneratorWrapper;
use proto::jobworkerp::data::StorageType;
use std::sync::Arc;
use worker::{
    dispatcher::{JobDispatcher, JobDispatcherFactory},
    instance_session::WorkerInstanceSessionHandle,
    result_processor::ResultProcessorImpl,
    runner::map::RunnerFactoryWithPoolMap,
};

pub mod worker;

pub struct WorkerModules {
    pub job_dispatcher: Box<dyn JobDispatcher + 'static>,
}

impl WorkerModules {
    pub fn new(
        config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
    ) -> Self {
        Self::new_with_session(
            config_module,
            id_generator,
            app_module,
            runner_factory,
            None,
        )
    }

    pub fn new_with_session(
        config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
    ) -> Self {
        // XXX static?
        let runner_pool_map = Arc::new(RunnerFactoryWithPoolMap::new(
            runner_factory.clone(),
            config_module.worker_config.clone(),
        ));
        let result_processor = Arc::new(ResultProcessorImpl::new(
            config_module.clone(),
            app_module.clone(),
        ));
        match config_module.storage_type() {
            // StorageType::RedisOnly => {
            //     let job_dispatcher = JobDispatcherFactory::create(
            //         id_generator.clone(),
            //         config_module.clone(),
            //         app_module.clone(),
            //         None,
            //         app_module.repositories.redis_module.clone(),
            //         runner_factory,
            //         runner_pool_map,
            //         result_processor,
            //     );
            //     Self { job_dispatcher }
            // }
            StorageType::Standalone => {
                let job_dispatcher = JobDispatcherFactory::create(
                    id_generator.clone(),
                    config_module.clone(),
                    app_module.clone(),
                    app_module.repositories.rdb_module.clone(),
                    None,
                    runner_factory,
                    runner_pool_map,
                    result_processor,
                    worker_instance_session,
                );
                Self { job_dispatcher }
            }
            StorageType::Scalable => {
                let job_dispatcher = JobDispatcherFactory::create(
                    id_generator.clone(),
                    config_module.clone(),
                    app_module.clone(),
                    app_module.repositories.rdb_module.clone(),
                    app_module.repositories.redis_module.clone(),
                    runner_factory,
                    runner_pool_map,
                    result_processor,
                    worker_instance_session,
                );
                Self { job_dispatcher }
            }
        }
    }
}
