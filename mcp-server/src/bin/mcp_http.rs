//! MCP Server with HTTP transport (Streamable HTTP).
//!
//! This binary starts an MCP server with HTTP transport, suitable for
//! browser-based clients or HTTP-based MCP proxies.
//!
//! # Architecture
//!
//! This binary is designed for **Scalable deployment** where the MCP server
//! and worker processes run separately:
//!
//! - **mcp-http**: Receives MCP requests and enqueues jobs via Redis/MySQL
//! - **worker**: Processes jobs from the queue (must be running separately)
//!
//! For single-process deployment, use `MCP_ENABLED=true all-in-one` instead.
//!
//! # Requirements
//!
//! - A worker process must be running to process enqueued jobs
//! - Redis (for job queue) and/or MySQL/SQLite (for persistence) must be configured
//!
//! # Environment Variables
//!
//! - `MCP_ADDR`: HTTP server bind address (default: 127.0.0.1:8000)
//! - `MCP_AUTH_ENABLED`: Enable Bearer authentication (default: false)
//! - `MCP_AUTH_TOKENS`: Valid tokens, comma-separated (default: demo-token)
//! - `STORAGE_TYPE`: `Standalone` or `Scalable` (Scalable requires Redis)
//! - See `McpServerConfig` for additional configuration options.

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::shutdown;
use command_utils::util::tracing::LoggingConfig;
use dotenvy::dotenv;
use jobworkerp_base::APP_NAME;
use jobworkerp_runner::runner::mcp::config::McpConfig;
use jobworkerp_runner::runner::mcp::proxy::McpServerFactory;
use jobworkerp_runner::runner::{factory::RunnerSpecFactory, plugins::Plugins};
use mcp_server::{McpHandler, McpServerConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix(APP_NAME, "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

    tracing::info!("Starting MCP HTTP Server");

    let plugins = Arc::new(Plugins::new());

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

    let function_app = app_module.function_app.clone();
    let function_set_app = app_module.function_set_app.clone();
    let mcp_config = McpServerConfig::from_env();

    let bind_addr = std::env::var("MCP_ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_string());

    let handler_factory = move || {
        Ok(McpHandler::new(
            function_app.clone(),
            function_set_app.clone(),
            mcp_config.clone(),
        ))
    };

    // Create shutdown lock for standalone mode
    let (lock, mut wait) = shutdown::create_lock_and_wait();

    // Standalone mode: use internal ctrl_c handler (pass None)
    mcp_server::boot_streamable_http_server(handler_factory, &bind_addr, lock, None).await?;

    // Wait for shutdown completion
    wait.wait().await;

    tracing::info!("MCP HTTP Server shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();

    Ok(())
}
