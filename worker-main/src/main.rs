// #[macro_use]
// extern crate debug_stub_derive;

use std::sync::Arc;

use anyhow::Result;
use app::module::{AppConfigModule, AppModule};
use common::util::{shutdown, tracing::LoggingConfig};
use dotenvy::dotenv;

// start front_server
// #[tokio::main]
#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    let conf = common::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename = common::util::tracing::create_filename_with_ip_postfix("jobworkerp", "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    };
    common::util::tracing::tracing_init(conf).await?;

    let (lock, mut wait) = shutdown::create_lock_and_wait();

    let app_config_module = Arc::new(AppConfigModule::new_by_env());

    let app_module = Arc::new(AppModule::new_by_env(app_config_module).await?);

    tracing::info!("start worker");
    let jh = tokio::spawn(lib::start_worker(app_module.clone(), lock.clone()));
    tracing::info!("start server");
    let jh2 = tokio::spawn(grpc_front::start_front_server(app_module, lock));

    // shutdown
    tracing::info!("waiting worker");
    wait.wait().await;

    tracing::debug!("shutdown telemetry");
    opentelemetry::global::shutdown_tracer_provider();

    tracing::debug!("worker handler");
    let _ret = jh.await?;
    let _ret2 = jh2.await?;

    tracing::info!("shutdown normally");
    // ret
    Ok(())
}
