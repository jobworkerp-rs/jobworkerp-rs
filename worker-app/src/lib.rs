use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::IdGeneratorWrapper;
use jobworkerp_runner::runner::sandbox::SandboxCleanupRegistry;
use proto::jobworkerp::data::StorageType;
use std::sync::Arc;
use tokio::sync::watch;
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
        let runner_pool_map = Arc::new(RunnerFactoryWithPoolMap::new(
            runner_factory.clone(),
            config_module.worker_config.clone(),
        ));
        Self::new_with_pool_map(
            config_module,
            id_generator,
            app_module,
            runner_factory,
            worker_instance_session,
            runner_pool_map,
        )
    }

    pub fn new_with_session_and_shutdown(
        config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
        lock: ShutdownLock,
        shutdown: watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        let (registry_shutdown_send, registry_shutdown_recv) = watch::channel(false);
        let cleanup_registry = SandboxCleanupRegistry::new(lock.clone(), registry_shutdown_recv)?;
        let runner_pool_map = Arc::new(
            RunnerFactoryWithPoolMap::new_with_cleanup_registry_and_shutdown(
                runner_factory.clone(),
                config_module.worker_config.clone(),
                cleanup_registry.clone(),
                shutdown.clone(),
            ),
        );

        // Start the coordinator before the dispatcher can receive its shutdown lock.
        tokio::spawn(coordinate_sandbox_shutdown(
            shutdown,
            runner_pool_map.clone(),
            cleanup_registry,
            registry_shutdown_send,
            lock.clone(),
        ));

        Ok(Self::new_with_pool_map(
            config_module,
            id_generator,
            app_module,
            runner_factory,
            worker_instance_session,
            runner_pool_map,
        ))
    }

    fn new_with_pool_map(
        config_module: Arc<AppConfigModule>,
        id_generator: Arc<IdGeneratorWrapper>,
        app_module: Arc<AppModule>,
        runner_factory: Arc<RunnerFactory>,
        worker_instance_session: Option<WorkerInstanceSessionHandle>,
        runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    ) -> Self {
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

pub(crate) async fn coordinate_sandbox_shutdown(
    mut shutdown: watch::Receiver<bool>,
    runner_pool_map: Arc<RunnerFactoryWithPoolMap>,
    cleanup_registry: SandboxCleanupRegistry,
    registry_shutdown_send: watch::Sender<bool>,
    _coordinator_lock: ShutdownLock,
) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        if shutdown.changed().await.is_err() {
            break;
        }
    }

    runner_pool_map.shutdown_and_clear().await;
    let _ = registry_shutdown_send.send(true);
    cleanup_registry.shutdown().await;
}
