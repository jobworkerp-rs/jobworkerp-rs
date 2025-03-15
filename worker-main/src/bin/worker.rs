use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use app_wrapper::runner::RunnerFactory;
use command_utils::util::{self, tracing::LoggingConfig};
use dotenvy::dotenv;
use jobworkerp_runner::runner::factory::RunnerSpecFactory;
use std::sync::Arc;

// #[tokio::main]
#[tokio::main(flavor = "multi_thread")]
pub async fn main() -> Result<()> {
    dotenv().ok();
    // node specific str (based on ip)
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix("jobworkerp-worker", "log");
    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    command_utils::util::tracing::tracing_init(LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    })
    .await?;

    let plugins = Arc::new(jobworkerp_runner::runner::plugins::Plugins::new());
    let runner_spec_factory = Arc::new(RunnerSpecFactory::new(plugins.clone()));

    let (lock, mut wait) = util::shutdown::create_lock_and_wait();

    let config_module = Arc::new(AppConfigModule::new_by_env(runner_spec_factory));
    let app_module = Arc::new(AppModule::new_by_env(config_module).await?);
    // reload jobs from rdb (if necessary) on start worker
    app_module.on_start_worker().await?;

    let runner_factory = Arc::new(RunnerFactory::new(app_module.clone()));
    let jh = tokio::spawn(lib::start_worker(app_module, runner_factory, lock));
    // tokio::time::sleep(Duration::from_secs(10)).await;

    tracing::info!("wait for processing ...");
    wait.wait().await;
    tracing::info!("shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();
    jh.await??;
    Ok(())
}
