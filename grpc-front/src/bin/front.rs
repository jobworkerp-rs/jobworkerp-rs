use std::sync::Arc;

use anyhow::Result;
use app::app::worker_instance::InstanceCleanupTask;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::shutdown;
use dotenvy::dotenv;
use infra::infra::worker_instance::UseWorkerInstanceRepository;
use jobworkerp_runner::runner::{
    factory::RunnerSpecFactory,
    mcp::{config::McpConfig, proxy::McpServerFactory},
    plugins::Plugins,
};

// start front_server
#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    command_utils::util::tracing::init_from_env_and_filename("jobworkerp-front", "log").await?;

    // Create shutdown signal for coordinated shutdown
    let (shutdown_send, shutdown_recv) = tokio::sync::watch::channel(false);

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
    let runner_factory = Arc::new(RunnerSpecFactory::new(
        Arc::new(Plugins::new()),
        mcp_clients,
    ));
    let config_module = Arc::new(AppConfigModule::new_by_env(runner_factory));
    let app_module = Arc::new(AppModule::new_by_env(config_module.clone()).await?);
    // setup runner on start front
    app_module.on_start_front().await?;

    // Start instance cleanup task (grpc-front only: cleanup expired instances)
    let storage_type = config_module.storage_type();
    let repository = app_module.repositories.worker_instance_repository();
    let cleanup_task = InstanceCleanupTask::new(repository, storage_type);
    let cleanup_handle =
        tokio::spawn(async move { cleanup_task.start_cleanup_loop(shutdown_recv).await });

    let (lock, mut wait) = shutdown::create_lock_and_wait();

    // Spawn shutdown signal handler (SIGINT + SIGTERM on Unix)
    shutdown::spawn_shutdown_handler(shutdown_send.clone());

    // trace::tracing_init_jaeger(&jeager_addr);
    let ret = grpc_front::start_front_server(app_module, lock).await;

    tracing::info!("waiting shutdown");
    wait.wait().await;

    // Send shutdown signal (in case not already sent)
    let _ = shutdown_send.send(true);

    // Wait for cleanup task to finish
    let _ = cleanup_handle.await;

    command_utils::util::tracing::shutdown_tracer_provider();
    tracing::info!("shutdown");

    ret
}
