use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use command_utils::util::shutdown;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::IdGeneratorWrapper;
use jobworkerp_runner::runner::mcp::config::McpConfig;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::{factory::RunnerSpecFactory, plugins::Plugins};
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

pub async fn boot_all_in_one() -> Result<()> {
    let (lock, mut wait) = shutdown::create_lock_and_wait();

    let plugins = Arc::new(Plugins::new());
    // load mcp config
    let mcp_clients = match McpConfig::load(&jobworkerp_base::MCP_CONFIG_PATH.clone()).await {
        Ok(mcp_clients) => {
            let c = Arc::new(McpServerFactory::new(mcp_clients));
            c.test_all().await?;
            c
        }
        Err(e) => {
            tracing::info!("mcp config not loaded: {:#?}", e);
            Arc::new(McpServerFactory::default())
        }
    };

    let runner_spec_factory =
        Arc::new(RunnerSpecFactory::new(plugins.clone(), mcp_clients.clone()));
    let app_config_module = Arc::new(AppConfigModule::new_by_env(runner_spec_factory.clone()));

    let app_module = Arc::new(AppModule::new_by_env(app_config_module).await?);
    app_module.on_start_all_in_one().await?;

    let runner_factory = Arc::new(RunnerFactory::new(app_module.clone(), mcp_clients));

    tracing::info!("start worker");
    let jh = tokio::spawn(start_worker(
        app_module.clone(),
        runner_factory,
        lock.clone(),
    ));
    tracing::info!("start server");
    let jh2 = tokio::spawn(grpc_front::start_front_server(app_module, lock));

    // shutdown
    tracing::info!("waiting worker");
    wait.wait().await;

    tracing::debug!("shutdown telemetry");

    tracing::debug!("worker handler");
    let _ret = jh.await?;
    let _ret2 = jh2.await?;

    command_utils::util::tracing::shutdown_tracer_provider();
    tracing::info!("shutdown normally");
    // ret
    Ok(())
}
