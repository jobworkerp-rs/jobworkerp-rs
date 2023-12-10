#[macro_use]
extern crate debug_stub_derive;

pub mod front;
pub mod proto;
pub mod service;

use anyhow::Result;
use app::module::AppModule;
use common::util::shutdown::ShutdownLock;
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

pub async fn start_front_server(app_module: Arc<AppModule>, lock: ShutdownLock) -> Result<()> {
    let grpc_addr: SocketAddr = env::var("GRPC_ADDR")
        .unwrap_or_else(|_| {
            tracing::info!("GRPC_ADDR not specified. set default 127.0.0.1:9000");
            "0.0.0.0:9000".to_string()
        })
        .parse()
        .unwrap();

    let use_web: bool = env::var("USE_GRPC_WEB")
        .unwrap_or("false".to_owned())
        .parse()
        .unwrap();

    let res = front::server::start_server(app_module, lock, grpc_addr, use_web)
        .await
        .map_err(|err| {
            tracing::error!("failed to create server: {:?}", err);
            err
        });
    tracing::debug!("server shutdown. res:{:?}", res);
    res
}
