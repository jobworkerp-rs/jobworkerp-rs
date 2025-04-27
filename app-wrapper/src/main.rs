use std::sync::Arc;

use app::module::AppConfigModule;
use clap::{arg, command, Parser};
use command_utils::util::tracing::LoggingConfig;
use jobworkerp_runner::runner::{
    factory::RunnerSpecFactory,
    mcp::{proxy::McpServerFactory, config::McpConfig},
    plugins::Plugins,
};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Enable tracing (generates a trace-timestamp.json file).
    #[arg(long, short, default_value = "false")]
    debug: bool,

    #[arg(long, short, required = true)]
    workflow: String,

    #[arg(long, short, default_value = "")]
    input: String,
}

// create embedding for all articles
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let conf = command_utils::util::tracing::load_tracing_config_from_env().unwrap_or_default();
    let log_filename =
        command_utils::util::tracing::create_filename_with_ip_postfix("simple-workflow", "log");
    let conf = LoggingConfig {
        file_name: Some(log_filename),
        ..conf
    };
    command_utils::util::tracing::tracing_init(conf).await?;

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

    // args.input: filepath or json string
    // try to read file first, if failed, then parse as json string, otherwise, treat as string
    let json = match std::fs::read_to_string(&args.input) {
        Ok(content) => serde_json::from_str(&content.replace("\n", ""))
            .unwrap_or_else(|_| serde_json::Value::String(content)),
        Err(_) => serde_json::from_str(&args.input)
            .unwrap_or_else(|_| serde_json::Value::String(args.input.clone())),
    };
    tracing::info!("Input: {:#?}", json);
    let plugins = Arc::new(Plugins::new());
    let runner_spec_factory =
        Arc::new(RunnerSpecFactory::new(plugins.clone(), mcp_clients.clone()));
    let config_module = Arc::new(AppConfigModule::new_by_env(runner_spec_factory));
    let app_module = Arc::new(app::module::AppModule::new_by_env(config_module).await?);
    match app_wrapper::workflow::execute::execute_workflow(
        app_module.clone(),
        args.workflow.as_str(),
        json,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::error!("Failed to execute workflow: {:#?}", e);
            Err(e.into())
        }
    }
}
