use anyhow::Result;
use async_trait::async_trait;
use hello::{HelloArg, HelloOperation};
use prost::Message;
use std::{alloc::System, time::Duration};
use tracing::Level; // Add this line to import the Message trait

pub mod hello {
    tonic::include_proto!("hello");
}

#[global_allocator]
static ALLOCATOR: System = System;

pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn load(&mut self, operation: Vec<u8>) -> Result<()>;
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    fn cancel(&self) -> bool;
    fn operation_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    // if true, use job result of before job, else use job args from request
    fn use_job_result(&self) -> bool;
}

// suppress warn improper_ctypes_definitions
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn load_plugin() -> Box<dyn PluginRunner + Send + Sync> {
    Box::new(HelloPlugin::new())
}

/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that the pointer is valid and that it was created by the
/// `load_plugin` function. The caller must also ensure that the `Box` created
/// by `Box::from_raw` is not used after it has been dropped.
#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_plugin(ptr: Box<dyn PluginRunner + Send + Sync>) {
    drop(ptr);
}

pub struct HelloPlugin {}

impl Default for HelloPlugin {
    fn default() -> Self {
        Self::new()
    }
}
impl HelloPlugin {
    pub fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        HelloPlugin {}
    }
    pub async fn hello(&self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        // XXX to test easy
        let arg = HelloArg::decode(arg).unwrap_or(HelloArg {
            arg: String::from_utf8_lossy(arg).to_string(),
        });
        let start = chrono::Utc::now().to_rfc3339();
        let data = arg.arg;
        println!(
            "========== [{}] HelloPlugin run: Hello! {} ==========",
            start, &data
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!(
            "========== [{}] END OF HelloPlugin: until {} ==========",
            start,
            chrono::Utc::now().to_rfc3339()
        );
        Ok(vec![format!("SUCCESS: arg={}", &data).into_bytes()])
    }
}

#[async_trait]
impl PluginRunner for HelloPlugin {
    fn name(&self) -> String {
        // specify as same string as worker.operation
        String::from("HelloPlugin")
    }
    fn load(&mut self, operation: Vec<u8>) -> Result<()> {
        tracing::info!("HelloPlugin load!");
        HelloOperation::decode(operation.as_slice())?;
        Ok(())
    }
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let arg_clone = arg.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move { self.hello(arg_clone.as_slice()).await })
    }
    fn cancel(&self) -> bool {
        tracing::warn!("HelloPlugin cancel: not implemented!");
        false
    }
    fn operation_proto(&self) -> String {
        include_str!("../protobuf/hello_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../protobuf/hello_job_args.proto").to_string()
    }
    // if true, use job result of before job, else use job args from request
    fn use_job_result(&self) -> bool {
        false
    }
}
