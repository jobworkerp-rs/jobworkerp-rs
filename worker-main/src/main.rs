// #[macro_use]
// extern crate debug_stub_derive;

use anyhow::Result;
use command_utils::util::tracing::LoggingConfig;
use dotenvy::dotenv;
use jobworkerp_base::APP_NAME;

// start all-in-one server
// #[tokio::main]
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

    jobworkerp_main::boot_all_in_one().await
}
