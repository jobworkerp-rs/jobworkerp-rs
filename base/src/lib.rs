use once_cell::sync::Lazy;
use std::{env, net::SocketAddr};

pub mod codec;
pub mod error;
pub mod limits;

pub static APP_NAME: &str = "jobworkerp";
pub static APP_WORKER_NAME: &str = "jobworkerp-worker";
pub static APP_FRONT_NAME: &str = "jobworkerp-front";

pub static MCP_CONFIG_PATH: Lazy<String> =
    Lazy::new(|| std::env::var("MCP_CONFIG").unwrap_or_else(|_| "mcp-settings.toml".to_string()));

pub static GRPC_ADDR: Lazy<SocketAddr> = Lazy::new(|| {
    env::var("GRPC_ADDR")
        .unwrap_or_else(|_| {
            // tracing::info!("GRPC_ADDR not specified. set default 127.0.0.1:9000");
            "0.0.0.0:9000".to_string()
        })
        .parse()
        .unwrap()
});

pub static USE_WEB: Lazy<bool> = Lazy::new(|| {
    env::var("USE_GRPC_WEB")
        .unwrap_or("false".to_owned())
        .parse()
        .unwrap()
});

pub static MAX_FRAME_SIZE: Lazy<Option<u32>> =
    Lazy::new(|| env::var("MAX_FRAME_SIZE").ok().map(|s| s.parse().unwrap()));
