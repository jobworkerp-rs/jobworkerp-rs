//! AG-UI HTTP Server binary.
//!
//! This binary starts an AG-UI server with HTTP SSE transport for streaming
//! workflow execution events to UI clients.
//!
//! # Architecture
//!
//! This binary supports both Standalone and Scalable deployment:
//!
//! - **Standalone**: Single-process mode with in-memory storage
//! - **Scalable**: Multi-process mode requiring separate worker processes with Redis
//!
//! # Requirements
//!
//! - For Scalable mode: Worker process(es) must be running to process jobs
//! - Redis and/or MySQL/SQLite must be configured based on STORAGE_TYPE
//!
//! # Environment Variables
//!
//! - `AG_UI_ADDR`: HTTP server bind address (default: 127.0.0.1:8001)
//! - `AG_UI_AUTH_ENABLED`: Enable Bearer authentication (default: false)
//! - `AG_UI_AUTH_TOKENS`: Valid tokens, comma-separated (default: demo-token)
//! - `AG_UI_REQUEST_TIMEOUT_SEC`: Request timeout (default: 60)
//! - `AG_UI_SESSION_TTL_SEC`: Session TTL in seconds (default: 3600)
//! - `STORAGE_TYPE`: `Standalone` (in-memory) or `Scalable` (Redis-backed)
//!
//! See `ServerConfig` and `AgUiServerConfig` for additional configuration options.

use ag_ui_front::handler::AgUiHandler;
use ag_ui_front::server::http::boot_ag_ui_server;
use ag_ui_front::{
    InMemoryEventStore, InMemorySessionManager, RedisEventStore, RedisSessionManager, ServerConfig,
};
use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::modules::AppWrapperModule;
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
        file_name: Some(log_filename),
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

    tracing::info!("Starting AG-UI HTTP Server");

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

    // Start app services
    app_module.on_start_all_in_one().await?;

    // Initialize app wrapper module
    let redis_pool = app_module
        .repositories
        .redis_module
        .as_ref()
        .map(|r| r.redis_pool);
    let app_wrapper_module = Arc::new(AppWrapperModule::new_by_env(redis_pool));

    // Load AG-UI server configuration
    let server_config = ServerConfig::from_env();
    let ag_ui_config = server_config.ag_ui.clone();

    tracing::info!(
        "AG-UI configuration: bind_addr={}, auth_enabled={}, timeout={}s, session_ttl={}s",
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
    // Capture result to ensure tracer shutdown happens even on error
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
                redis_pool.clone(),
                ag_ui_config.session_ttl_sec,
            ));
            let event_store = Arc::new(RedisEventStore::new(
                redis_pool.clone(),
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec, // Use session TTL for event TTL
            ));

            let handler = AgUiHandler::new(
                app_wrapper_module,
                app_module.clone(),
                session_manager,
                event_store,
                ag_ui_config,
            );

            let server_future = boot_ag_ui_server(handler, server_config, lock, None);
            match server_future.await {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::error!("AG-UI HTTP Server error: {:#?}", e);
                    Err(e)
                }
            }
        }
        _ => {
            // Standalone mode (default): Use in-memory session manager and event store
            tracing::info!("Using Standalone mode with in-memory session/event storage");

            let session_manager =
                Arc::new(InMemorySessionManager::new(ag_ui_config.session_ttl_sec));
            let event_store = Arc::new(InMemoryEventStore::new(
                ag_ui_config.max_events_per_run,
                ag_ui_config.session_ttl_sec,
            ));

            let handler = AgUiHandler::new(
                app_wrapper_module,
                app_module.clone(),
                session_manager,
                event_store,
                ag_ui_config,
            );

            let server_future = boot_ag_ui_server(handler, server_config, lock, None);
            match server_future.await {
                Ok(()) => Ok(()),
                Err(e) => {
                    tracing::error!("AG-UI HTTP Server error: {:#?}", e);
                    Err(e)
                }
            }
        }
    };

    // Wait for shutdown completion only if server started successfully
    if server_result.is_ok() {
        wait.wait().await;
    }

    // Always shutdown tracer provider, even on error
    tracing::info!("AG-UI HTTP Server shutdown complete");
    command_utils::util::tracing::shutdown_tracer_provider();

    server_result
}
