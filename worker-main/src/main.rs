// #[macro_use]
// extern crate debug_stub_derive;

use std::sync::Arc;

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use command_utils::util::{shutdown, tracing::LoggingConfig};
use dotenvy::dotenv;
use infra::infra::plugins::Plugins;

// start front_server
// #[tokio::main]
#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix("jobworkerp", "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

    let plugins = Arc::new(Plugins::new());

    let (lock, mut wait) = shutdown::create_lock_and_wait();

    let app_config_module = Arc::new(AppConfigModule::new_by_env(plugins.clone()));

    let app_module = Arc::new(AppModule::new_by_env(app_config_module).await?);
    app_module.on_start_all_in_one().await?;

    tracing::info!("start worker");
    let jh = tokio::spawn(lib::start_worker(app_module.clone(), plugins, lock.clone()));
    tracing::info!("start server");
    let jh2 = tokio::spawn(grpc_front::start_front_server(app_module, lock));

    // shutdown
    tracing::info!("waiting worker");
    wait.wait().await;

    tracing::debug!("shutdown telemetry");
    command_utils::util::tracing::shutdown_tracer_provider();

    tracing::debug!("worker handler");
    let _ret = jh.await?;
    let _ret2 = jh2.await?;

    tracing::info!("shutdown normally");
    // ret
    Ok(())
}
