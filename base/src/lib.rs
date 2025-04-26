use once_cell::sync::Lazy;

pub mod codec;
pub mod error;

pub static MCP_CONFIG_PATH: Lazy<String> =
    Lazy::new(|| std::env::var("MCP_CONFIG").unwrap_or_else(|_| "mcp-settings.toml".to_string()));
