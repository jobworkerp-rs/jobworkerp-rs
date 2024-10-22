use anyhow::Result;
use app::module::AppModule;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::runner::factory::RunnerFactory;
use infra::infra::IdGeneratorWrapper;
use std::sync::Arc;
use tokio::sync::OnceCell;
use worker_app::worker::dispatcher::JobDispatcher;
use worker_app::WorkerModules;

pub async fn start_worker(
    app_module: Arc<AppModule>,
    runner_factory: Arc<RunnerFactory>,
    lock: ShutdownLock,
) -> Result<()> {
    let config_module = app_module.config_module.clone();

    let wm = WorkerModules::new(
        config_module.clone(),
        Arc::new(IdGeneratorWrapper::new()), // use for job_result.id
        app_module.clone(),
        runner_factory.clone(),
    );

    // create and start job dispatcher
    static JOB_DISPATCHER: OnceCell<Box<dyn JobDispatcher + 'static>> = OnceCell::const_new();
    let dispatcher = JOB_DISPATCHER
        .get_or_init(|| async move { wm.job_dispatcher })
        .await;

    // start dispatching jobs
    dispatcher.dispatch_jobs(lock)?;

    tracing::debug!("worker started");
    Ok(())
}
