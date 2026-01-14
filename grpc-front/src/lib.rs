#[macro_use]
extern crate debug_stub_derive;

pub mod front;
pub mod proto;
pub mod service;

use anyhow::Result;
use app::module::AppModule;
use app_wrapper::modules::AppWrapperModule;
use command_utils::util::shutdown::ShutdownLock;
use jobworkerp_base::{GRPC_ADDR, MAX_FRAME_SIZE, USE_WEB};
use std::sync::Arc;

pub async fn start_front_server(app_module: Arc<AppModule>, app_wrapper_module: Arc<AppWrapperModule>, lock: ShutdownLock) -> Result<()> {
    let res = front::server::start_server(app_module, app_wrapper_module, lock, *GRPC_ADDR, *USE_WEB, *MAX_FRAME_SIZE)
        .await
        .map_err(|err| {
            tracing::error!("failed to create server: {:?}", err);
            err
        });

    tracing::debug!("server shutdown. res:{:?}", res);
    command_utils::util::tracing::shutdown_tracer_provider();
    res
}
