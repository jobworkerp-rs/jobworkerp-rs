use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use command_utils::util::shutdown;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::IdGeneratorWrapper;
use jobworkerp_runner::runner::mcp::config::McpConfig;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::{factory::RunnerSpecFactory, plugins::Plugins};
use mcp_server::{McpHandler, McpServerConfig};
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

    // Create shutdown signal for cleanup task
    let (shutdown_send, _shutdown_recv) = tokio::sync::watch::channel(false);

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
    let app_wrapper_module = Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));

    app_module.on_start_all_in_one().await?;

    // TODO use internal clean-up job
    // Start JobStatusCleanupTask if RDB indexing is enabled
    // let cleanup_handle = app_module.start_job_status_cleanup_task(shutdown_recv);

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module.clone(),
        mcp_clients,
    ));

    tracing::info!("start worker and server");
    let worker_future = start_worker(app_module.clone(), runner_factory, lock.clone());
    let server_future = grpc_front::start_front_server(app_module, lock);

    // Run both futures concurrently and wait for both to complete
    let (worker_result, server_result) = tokio::join!(worker_future, server_future);

    tracing::debug!("worker completed: {:?}", worker_result);
    tracing::debug!("server completed: {:?}", server_result);

    // Handle results
    worker_result?;
    server_result?;

    // shutdown
    tracing::info!("waiting shutdown signal");
    wait.wait().await;

    // Send shutdown signal to cleanup task
    let _ = shutdown_send.send(true);

    // TODO use internal clean-up job
    // // Wait for cleanup task to finish if it was started
    // if let Some(handle) = cleanup_handle {
    //     tracing::debug!("waiting for JobStatusCleanupTask to finish");
    //     let _ = handle.await;
    //     tracing::debug!("JobStatusCleanupTask finished");
    // }

    tracing::debug!("shutdown telemetry");

    command_utils::util::tracing::shutdown_tracer_provider();
    tracing::info!("shutdown normally");
    // ret
    Ok(())
}

/// Boot all-in-one with MCP Server instead of gRPC Front.
///
/// This function starts the Worker and MCP Server together in a single process,
/// allowing MCP clients (like Claude Desktop) to directly interact with jobworkerp
/// without going through gRPC.
pub async fn boot_all_in_one_mcp() -> Result<()> {
    let (lock, mut wait) = shutdown::create_lock_and_wait();

    let (shutdown_send, shutdown_recv) = tokio::sync::watch::channel(false);

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
    let app_wrapper_module = Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));

    app_module.on_start_all_in_one().await?;

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module.clone(),
        mcp_clients,
    ));

    // MCP Handler setup
    let function_app = app_module.function_app.clone();
    let function_set_app = app_module.function_set_app.clone();
    let mcp_config = McpServerConfig::from_env();

    tracing::info!("start worker and MCP server");
    let worker_lock = lock.clone();
    let mcp_lock = lock; // Move original lock to MCP server (no clone needed)
    let worker_future = start_worker(app_module.clone(), runner_factory, worker_lock);

    // MCP Server (instead of gRPC Front)
    let mut mcp_shutdown_recv = shutdown_recv.clone();
    let mcp_future = async move {
        let bind_addr = std::env::var("MCP_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());
        let handler_factory = move || {
            Ok(McpHandler::new(
                function_app.clone(),
                function_set_app.clone(),
                mcp_config.clone(),
            ))
        };
        // Create shutdown signal from watch receiver
        let shutdown_signal: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            Box::pin(async move {
                let _ = mcp_shutdown_recv.changed().await;
                tracing::info!("MCP server received shutdown signal");
            });
        mcp_server::boot_streamable_http_server(
            handler_factory,
            &bind_addr,
            mcp_lock,
            Some(shutdown_signal),
        )
        .await
    };

    // Spawn ctrl_c handler that broadcasts shutdown to MCP server
    let ctrl_c_shutdown_send = shutdown_send.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                tracing::info!("received ctrl_c, sending shutdown signal");
                let _ = ctrl_c_shutdown_send.send(true);
            }
            Err(e) => tracing::error!("failed to listen for ctrl_c: {:?}", e),
        }
    });

    // Run both futures concurrently and wait for both to complete
    let (worker_result, mcp_result) = tokio::join!(worker_future, mcp_future);

    tracing::debug!("worker completed: {:?}", worker_result);
    tracing::debug!("mcp server completed: {:?}", mcp_result);

    // Handle results
    worker_result?;
    mcp_result?;

    // Send shutdown signal to cleanup task (in case not already sent)
    let _ = shutdown_send.send(true);

    // Wait for all locks to be released with timeout
    // Worker tasks should release their locks after receiving ctrl_c
    tracing::info!("waiting shutdown signal (with timeout)");
    tokio::select! {
        _ = wait.wait() => {
            tracing::debug!("all locks released normally");
        }
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {
            tracing::warn!("shutdown timeout - forcing exit");
        }
    }

    tracing::debug!("shutdown telemetry");

    command_utils::util::tracing::shutdown_tracer_provider();
    tracing::info!("shutdown normally");
    Ok(())
}
