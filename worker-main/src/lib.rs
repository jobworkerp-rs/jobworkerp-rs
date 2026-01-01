mod instance;

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::modules::AppWrapperModule;
use app_wrapper::runner::RunnerFactory;
use command_utils::util::shutdown;
use command_utils::util::shutdown::ShutdownLock;
use infra::infra::IdGeneratorWrapper;
use instance::WorkerInstanceManager;
use jobworkerp_runner::runner::mcp::config::McpConfig;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::{factory::RunnerSpecFactory, plugins::Plugins};
use mcp_server::{McpHandler, McpServerConfig};
use std::sync::Arc;
use tokio::sync::OnceCell;
use worker_app::worker::dispatcher::JobDispatcher;
use worker_app::WorkerModules;

const MCP_DEFAULT_ADDR: &str = "127.0.0.1:8000";

/// Check if AG-UI server is enabled via environment variable.
/// Accepts "true" or "1" (case-insensitive, with whitespace trimmed).
fn is_ag_ui_enabled() -> bool {
    std::env::var("AG_UI_ENABLED")
        .map(|v| {
            let v = v.trim().to_lowercase();
            v == "true" || v == "1"
        })
        .unwrap_or(false)
}

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

    // Create shutdown signal for coordinated shutdown
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
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));

    app_module.on_start_all_in_one().await?;

    // Initialize Worker Instance Registry (registers instance and starts heartbeat)
    let instance_manager =
        WorkerInstanceManager::initialize(&app_module, shutdown_recv.clone()).await?;

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module.clone(),
        mcp_clients,
    ));

    // Check if AG-UI server is enabled
    let ag_ui_enabled = is_ag_ui_enabled();

    if ag_ui_enabled {
        tracing::info!("start worker, gRPC server, and AG-UI server");
    } else {
        tracing::info!("start worker and gRPC server");
    }

    // Worker future
    let worker_future = start_worker(app_module.clone(), runner_factory, lock.clone());

    // gRPC Front Server future with coordinated shutdown
    let grpc_lock = lock.clone();
    let grpc_app_module = app_module.clone();
    let mut grpc_shutdown_recv = shutdown_recv.clone();
    let grpc_future = async move {
        let addr = *jobworkerp_base::GRPC_ADDR;
        let use_web = *jobworkerp_base::USE_WEB;
        let max_frame_size = *jobworkerp_base::MAX_FRAME_SIZE;

        let (grpc_tx, grpc_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = grpc_shutdown_recv.changed().await;
            tracing::info!("gRPC server received shutdown signal");
            let _ = grpc_tx.send(());
        });

        let result = grpc_front::front::server::start_server_with_shutdown(
            grpc_app_module,
            addr,
            use_web,
            max_frame_size,
            grpc_rx,
        )
        .await;
        grpc_lock.unlock();
        result
    };

    // Spawn ctrl_c handler that broadcasts shutdown to all servers
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

    // Run futures concurrently based on configuration
    if ag_ui_enabled {
        let ag_ui_lock = lock.clone();

        // Release the original lock since all services have their own clones
        lock.unlock();

        let ag_ui_future = ag_ui_front::boot_embedded_server(
            app_wrapper_module,
            app_module,
            ag_ui_lock,
            shutdown_recv.clone(),
        );

        let (worker_result, grpc_result, ag_ui_result) =
            tokio::join!(worker_future, grpc_future, ag_ui_future);

        tracing::debug!("worker completed: {:?}", worker_result);
        tracing::debug!("grpc server completed: {:?}", grpc_result);
        tracing::debug!("ag-ui server completed: {:?}", ag_ui_result);

        worker_result?;
        grpc_result?;
        ag_ui_result?;
    } else {
        // Release the original lock since all services have their own clones
        lock.unlock();

        let (worker_result, grpc_result) = tokio::join!(worker_future, grpc_future);

        tracing::debug!("worker completed: {:?}", worker_result);
        tracing::debug!("grpc server completed: {:?}", grpc_result);

        worker_result?;
        grpc_result?;
    }

    // Send shutdown signal (in case not already sent)
    let _ = shutdown_send.send(true);

    // Unregister worker instance
    if let Err(e) = instance_manager.shutdown().await {
        tracing::warn!("Failed to shutdown instance manager: {}", e);
    }

    // Wait for all locks to be released with timeout
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

/// Boot all-in-one with MCP Server in addition to gRPC Front.
///
/// This function starts the Worker, gRPC Server, and MCP Server together in a single process,
/// allowing both gRPC clients and MCP clients (like Claude Desktop) to interact with jobworkerp.
/// Optionally starts AG-UI server if AG_UI_ENABLED=true.
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
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));

    app_module.on_start_all_in_one().await?;

    // Initialize Worker Instance Registry (registers instance and starts heartbeat)
    let instance_manager =
        WorkerInstanceManager::initialize(&app_module, shutdown_recv.clone()).await?;

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module.clone(),
        mcp_clients,
    ));

    // MCP Handler setup
    let function_app = app_module.function_app.clone();
    let function_set_app = app_module.function_set_app.clone();
    let mcp_config = McpServerConfig::from_env();

    // Check if AG-UI server is enabled
    let ag_ui_enabled = is_ag_ui_enabled();

    if ag_ui_enabled {
        tracing::info!("start worker, gRPC server, MCP server, and AG-UI server");
    } else {
        tracing::info!("start worker, gRPC server, and MCP server");
    }

    // Worker future
    let worker_future = start_worker(app_module.clone(), runner_factory, lock.clone());

    // gRPC Front Server future
    let grpc_lock = lock.clone();
    let grpc_app_module = app_module.clone();
    let mut grpc_shutdown_recv = shutdown_recv.clone();
    let grpc_future = async move {
        let addr = *jobworkerp_base::GRPC_ADDR;
        let use_web = *jobworkerp_base::USE_WEB;
        let max_frame_size = *jobworkerp_base::MAX_FRAME_SIZE;

        let (grpc_tx, grpc_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = grpc_shutdown_recv.changed().await;
            tracing::info!("gRPC server received shutdown signal");
            let _ = grpc_tx.send(());
        });

        let result = grpc_front::front::server::start_server_with_shutdown(
            grpc_app_module,
            addr,
            use_web,
            max_frame_size,
            grpc_rx,
        )
        .await;
        grpc_lock.unlock();
        result
    };

    // MCP Server future
    let mcp_lock = lock.clone();
    let mut mcp_shutdown_recv = shutdown_recv.clone();
    let mcp_future = async move {
        let bind_addr = std::env::var("MCP_ADDR").unwrap_or_else(|_| MCP_DEFAULT_ADDR.to_string());
        let handler_factory = move || {
            Ok(McpHandler::new(
                function_app.clone(),
                function_set_app.clone(),
                mcp_config.clone(),
            ))
        };
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

    // Spawn ctrl_c handler that broadcasts shutdown to all servers
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

    // Run futures concurrently based on configuration
    if ag_ui_enabled {
        let ag_ui_lock = lock.clone();

        // Release the original lock since all services have their own clones
        lock.unlock();

        let ag_ui_future = ag_ui_front::boot_embedded_server(
            app_wrapper_module,
            app_module,
            ag_ui_lock,
            shutdown_recv.clone(),
        );

        let (worker_result, grpc_result, mcp_result, ag_ui_result) =
            tokio::join!(worker_future, grpc_future, mcp_future, ag_ui_future);

        tracing::debug!("worker completed: {:?}", worker_result);
        tracing::debug!("grpc server completed: {:?}", grpc_result);
        tracing::debug!("mcp server completed: {:?}", mcp_result);
        tracing::debug!("ag-ui server completed: {:?}", ag_ui_result);

        worker_result?;
        grpc_result?;
        mcp_result?;
        ag_ui_result?;
    } else {
        // Release the original lock since all services have their own clones
        lock.unlock();

        let (worker_result, grpc_result, mcp_result) =
            tokio::join!(worker_future, grpc_future, mcp_future);

        tracing::debug!("worker completed: {:?}", worker_result);
        tracing::debug!("grpc server completed: {:?}", grpc_result);
        tracing::debug!("mcp server completed: {:?}", mcp_result);

        worker_result?;
        grpc_result?;
        mcp_result?;
    }

    // Send shutdown signal (in case not already sent)
    let _ = shutdown_send.send(true);

    // Unregister worker instance
    if let Err(e) = instance_manager.shutdown().await {
        tracing::warn!("Failed to shutdown instance manager: {}", e);
    }

    // Wait for all locks to be released with timeout
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
