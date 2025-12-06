//! MCP Server with stdio transport.
//!
//! This binary starts an MCP server with stdio transport, suitable for
//! integration with MCP clients like Claude Desktop that communicate via
//! standard input/output.
//!
//! # Architecture
//!
//! This binary is designed for **Scalable deployment** where the MCP server
//! and worker processes run separately:
//!
//! - **mcp-stdio**: Receives MCP requests via stdin/stdout and enqueues jobs
//! - **worker**: Processes jobs from the queue (must be running separately)
//!
//! For single-process deployment, use `MCP_ENABLED=true all-in-one` instead.
//!
//! # Requirements
//!
//! - A worker process must be running to process enqueued jobs
//! - Redis (for job queue) and/or MySQL/SQLite (for persistence) must be configured
//!
//! # Usage
//!
//! Configure in Claude Desktop's MCP settings:
//! ```json
//! {
//!   "mcpServers": {
//!     "jobworkerp": {
//!       "command": "/path/to/mcp-stdio",
//!       "env": {
//!         "DATABASE_URL": "sqlite://./jobworkerp.db",
//!         "STORAGE_TYPE": "Scalable",
//!         "REDIS_URL": "redis://localhost:6379"
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! **Note**: Ensure a worker process is running separately to process jobs.
//!
//! # Environment Variables
//!
//! - `STORAGE_TYPE`: `Standalone` or `Scalable` (Scalable requires Redis)
//! - See `McpServerConfig` for additional configuration options.

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
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

    // Stdio mode logs to file only to avoid interfering with MCP protocol
    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix(APP_NAME, "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename),
        use_stdout: false, // Disable stdout logging in stdio mode
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

    tracing::info!("Starting MCP Stdio Server");

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

    let handler = McpHandler::new(function_app, function_set_app, mcp_config);

    mcp_server::boot_stdio_server(handler).await?;

    tracing::info!("MCP Stdio Server shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();

    Ok(())
}
