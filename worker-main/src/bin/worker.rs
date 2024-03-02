use std::sync::Arc;

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::{self, tracing::LoggingConfig};

use dotenvy::dotenv;

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

    let (lock, mut wait) = util::shutdown::create_lock_and_wait();

    let config_module = Arc::new(AppConfigModule::new_by_env());
    let app_module = Arc::new(AppModule::new_by_env(config_module).await?);

    let jh = tokio::spawn(lib::start_worker(app_module, lock));
    // tokio::time::sleep(Duration::from_secs(10)).await;

    tracing::info!("wait for processing ...");
    wait.wait().await;
    tracing::info!("shutdown");
    command_utils::util::tracing::shutdown_tracer_provider();
    jh.await??;
    Ok(())
}
