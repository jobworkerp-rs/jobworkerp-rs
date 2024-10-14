use anyhow::Result;
use async_trait::async_trait;
use common::util::datetime;
use std::{alloc::System, time::Duration};

#[global_allocator]
static ALLOCATOR: System = System;

pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
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
    Box::new(HelloPlugin {})
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

impl HelloPlugin {
    pub async fn hello(&self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let start = datetime::now().to_rfc3339();
        let data = String::from_utf8_lossy(arg);
        println!(
            "========== [{}] HelloPlugin run: Hello! {} ==========",
            start, &data
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!(
            "========== [{}] END OF HelloPlugin: until {} ==========",
            start,
            datetime::now().to_rfc3339()
        );
        Ok(vec![format!(
            "SUCCESS: arg={}",
            String::from_utf8_lossy(arg)
        )
        .into_bytes()])
    }
}

#[async_trait]
impl PluginRunner for HelloPlugin {
    fn name(&self) -> String {
        // specify as same string as worker.operation
        String::from("HelloPlugin")
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
        include_str!("../proto/hello_operation.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../proto/hello_job_args.proto").to_string()
    }
    // if true, use job result of before job, else use job args from request
    fn use_job_result(&self) -> bool {
        false
    }

}
