use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use command_utils::util::{self, tracing::LoggingConfig};
use dotenvy::dotenv;
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::runner::{
    factory::RunnerSpecFactory,
    mcp::{config::McpConfig, proxy::McpServerFactory},
};
use std::sync::Arc;

// #[tokio::main]
#[tokio::main(flavor = "multi_thread")]
pub async fn main() -> Result<()> {
    dotenv().ok();
    // node specific str (based on ip)
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix(APP_WORKER_NAME, "log");
    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    command_utils::util::tracing::tracing_init(LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    })
    .await?;

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

    let plugins = Arc::new(jobworkerp_runner::runner::plugins::Plugins::new());
    let runner_spec_factory =
        Arc::new(RunnerSpecFactory::new(plugins.clone(), mcp_clients.clone()));

    let (lock, mut wait) = util::shutdown::create_lock_and_wait();

    let config_module = Arc::new(AppConfigModule::new_by_env(runner_spec_factory));
    let app_module = Arc::new(AppModule::new_by_env(config_module).await?);
    let app_wrapper_module = Arc::new(app_wrapper::modules::AppWrapperModule::new_by_env(
        app_module
            .repositories
            .redis_module
            .as_ref()
            .map(|r| r.redis_pool),
    ));

    // reload jobs from rdb (if necessary) on start worker
    app_module.on_start_worker().await?;

    let runner_factory = Arc::new(RunnerFactory::new(
        app_module.clone(),
        app_wrapper_module,
        mcp_clients,
    ));
    let jh = tokio::spawn(jobworkerp_main::start_worker(
        app_module,
        runner_factory,
        lock,
    ));
    // tokio::time::sleep(Duration::from_secs(10)).await;

    tracing::info!("wait for processing ...");
    wait.wait().await;
    tracing::info!("shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();
    jh.await??;
    Ok(())
}
