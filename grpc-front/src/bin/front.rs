use std::sync::Arc;

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::shutdown;
use dotenvy::dotenv;
use jobworkerp_runner::runner::{
    factory::RunnerSpecFactory,
    mcp::client::{McpConfig, McpServerFactory},
    plugins::Plugins,
};

// start front_server
#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    command_utils::util::tracing::init_from_env_and_filename("jobworkerp-front", "log").await?;

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
    let app_module = Arc::new(AppModule::new_by_env(config_module).await?);
    // setup runner on start front
    app_module.on_start_front().await?;

    let (lock, mut wait) = shutdown::create_lock_and_wait();

    // trace::tracing_init_jaeger(&jeager_addr);
    let ret = grpc_front::start_front_server(app_module, lock).await;

    tracing::info!("waiting shutdown");
    wait.wait().await;
    tracing::info!("shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();

    ret
}
