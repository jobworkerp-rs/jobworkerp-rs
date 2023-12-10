use anyhow::Result;
use app::module::AppModule;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::IdGeneratorWrapper;
use std::sync::Arc;
use tokio::sync::OnceCell;
use worker_app::plugins::Plugins;
use worker_app::worker::dispatcher::JobDispatcher;
use worker_app::WorkerModules;

pub async fn start_worker(app_module: Arc<AppModule>, lock: ShutdownLock) -> Result<()> {
    let config_module = app_module.config_module.clone();
    let mut plugins = Plugins::new();
    plugins.load_plugins_from_env()?;
    let plugins_module = Arc::new(plugins);

    // reload jobs from rdb (if necessary)
    app_module.reload_jobs_from_rdb_with_config().await?;
    let wm = WorkerModules::new(
        config_module.clone(),
        Arc::new(IdGeneratorWrapper::new()), // use for job_result.id
        app_module.clone(),
        plugins_module.clone(),
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
