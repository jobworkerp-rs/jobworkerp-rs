//! AG-UI HTTP Server binary (async/decoupled architecture).
//!
//! This binary starts an AG-UI server using the decoupled architecture where
//! workflow execution is delegated to worker processes via job queue.
//!
//! # Architecture
//!
//! Unlike `ag_ui_http` which executes workflows directly in the server process,
//! this binary uses `FunctionApp::handle_runner_for_front()` to enqueue jobs
//! to the job queue. Worker processes then execute the jobs.
//!
//! Benefits:
//! - **Fault tolerance**: Jobs persist in queue even if server crashes
//! - **Scalability**: Multiple workers can process jobs in parallel
//! - **Checkpoint recovery**: Jobs can resume from checkpoints
//!
//! Trade-offs:
//! - **Higher latency**: Queue overhead adds latency
//! - **Requires workers**: Worker process(es) must be running
//!
//! # Requirements
//!
//! - Worker process(es) must be running to process jobs
//! - Redis and MySQL/SQLite must be configured (Scalable mode required)
//!
//! # Environment Variables
//!
//! - `AG_UI_ADDR`: HTTP server bind address (default: 127.0.0.1:8001)
//! - `AG_UI_AUTH_ENABLED`: Enable Bearer authentication (default: false)
//! - `AG_UI_AUTH_TOKENS`: Valid tokens, comma-separated (default: demo-token)
//! - `AG_UI_REQUEST_TIMEOUT_SEC`: Request timeout (default: 60)
//! - `AG_UI_SESSION_TTL_SEC`: Session TTL in seconds (default: 3600)
//! - `STORAGE_TYPE`: Must be `Scalable` for async mode (Redis required)
//!
//! See `ServerConfig` and `AgUiServerConfig` for additional configuration options.

use ag_ui_front::handler_async::AsyncAgUiHandler;
use ag_ui_front::server::http_async::boot_ag_ui_async_server;
use ag_ui_front::{
    InMemoryEventStore, InMemorySessionManager, RedisEventStore, RedisSessionManager, ServerConfig,
};
use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::shutdown;
use command_utils::util::tracing::LoggingConfig;
use dotenvy::dotenv;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::runner::mcp::config::McpConfig;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::{factory::RunnerSpecFactory, plugins::Plugins};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    // Initialize tracing
    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix(APP_NAME, "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename.replace(APP_NAME, &format!("{}-async", APP_NAME))),
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

    tracing::info!("Starting AG-UI HTTP Async Server (decoupled architecture)");

    // Initialize plugins
    let plugins = Arc::new(Plugins::new());

    // Load MCP configuration if available
    let mcp_clients = match McpConfig::load(&jobworkerp_base::MCP_CONFIG_PATH.clone()).await {
        Ok(mcp_clients) => {
            let c = Arc::new(McpServerFactory::new(mcp_clients));
            if let Err(e) = c.test_all().await {
                tracing::warn!("Some MCP servers failed connection test: {:#?}", e);
            }
            c
        }
        Err(e) => {
            tracing::info!("MCP config not loaded (optional): {:#?}", e);
            Arc::new(McpServerFactory::default())
        }
    };

    // Initialize runner spec factory
    let runner_spec_factory =
        Arc::new(RunnerSpecFactory::new(plugins.clone(), mcp_clients.clone()));

    // Initialize app config module
    let app_config_module = Arc::new(AppConfigModule::new_by_env(runner_spec_factory.clone()));

    // Initialize app module
    let app_module = Arc::new(AppModule::new_by_env(app_config_module).await?);

    // Start app services (load runners, but don't restore jobs - workers do that)
    app_module.on_start_front().await?;

    // Load AG-UI server configuration
    let server_config = ServerConfig::from_env();
    let ag_ui_config = server_config.ag_ui.clone();

    tracing::info!(
        "AG-UI Async configuration: bind_addr={}, auth_enabled={}, timeout={}s, session_ttl={}s",
        server_config.bind_addr,
        server_config.auth.enabled,
        ag_ui_config.timeout_sec,
        ag_ui_config.session_ttl_sec
    );

    // Create shutdown lock
    let (lock, mut wait) = shutdown::create_lock_and_wait();

    // Determine storage type from environment variable (case-insensitive)
    let storage_type = std::env::var("STORAGE_TYPE")
        .unwrap_or_else(|_| "Standalone".to_string())
        .to_lowercase();

    // Start server based on storage type
    let server_result = match storage_type.as_str() {
        "scalable" => {
            // Scalable mode: Use Redis-backed session manager and event store
            tracing::info!("Using Scalable mode with Redis session/event storage");

            let redis_pool = app_module
                .repositories
                .redis_module
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Redis module required for Scalable mode"))?
                .redis_pool;

            let session_manager = Arc::new(RedisSessionManager::new(
                redis_pool,
                ag_ui_config.session_ttl_sec,
            ));
            let event_store = Arc::new(RedisEventStore::new(
                redis_pool,
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec,
            ));

            let handler = AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager,
                event_store,
                ag_ui_config,
            );

            let server_future = boot_ag_ui_async_server(handler, server_config, lock, None);
            match server_future.await {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::error!("AG-UI HTTP Async Server error: {:#?}", e);
                    Err(e)
                }
            }
        }
        _ => {
            // Standalone mode: Use in-memory session manager and event store
            // Note: Async mode benefits most from Scalable storage, but works with Standalone too
            tracing::info!("Using Standalone mode with in-memory session/event storage");
            tracing::warn!(
                "Standalone mode with async architecture may have limited benefits. \
                Consider using Scalable mode for full fault tolerance."
            );

            let session_manager =
                Arc::new(InMemorySessionManager::new(ag_ui_config.session_ttl_sec));
            let event_store = Arc::new(InMemoryEventStore::new(
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec,
            ));

            let handler = AsyncAgUiHandler::new(
                app_module.clone(),
                session_manager,
                event_store,
                ag_ui_config,
            );

            let server_future = boot_ag_ui_async_server(handler, server_config, lock, None);
            match server_future.await {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::error!("AG-UI HTTP Async Server error: {:#?}", e);
                    Err(e)
                }
            }
        }
    };

    // Wait for shutdown completion only if server started successfully
    if server_result.is_ok() {
        wait.wait().await;
    }

    // Always shutdown tracer provider
    tracing::info!("AG-UI HTTP Async Server shutdown complete");
    command_utils::util::tracing::shutdown_tracer_provider();

    server_result
}
