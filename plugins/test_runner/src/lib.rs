use anyhow::Result;
use async_trait::async_trait;
use prost::Message;
use std::alloc::System;
use test::{TestArgs, TestRunnerSettings};
use tracing::Level; // Add this line to import the Message trait

pub mod test {
    tonic::include_proto!("_");
}

#[global_allocator]
static ALLOCATOR: System = System;

pub trait PluginRunner: Send + Sync {
    fn name(&self) -> String;
    fn load(&mut self, settings: Vec<u8>) -> Result<()>;
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>>;
    // REMOVE
    fn begin_stream(&mut self, arg: Vec<u8>) -> Result<()>;
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>>;
    fn cancel(&self) -> bool;
    fn is_canceled(&self) -> bool;
    fn runner_settings_proto(&self) -> String;
    fn job_args_proto(&self) -> String;
    fn result_output_proto(&self) -> Option<String>;
    // if true, use job result of before job, else use job args from request
    fn use_job_result(&self) -> bool;
    fn output_as_stream(&self) -> bool;
}

// suppress warn improper_ctypes_definitions
#[allow(improper_ctypes_definitions)]
#[no_mangle]
pub extern "C" fn load_plugin() -> Box<dyn PluginRunner + Send + Sync> {
    Box::new(TestPlugin::new())
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_plugin(ptr: Box<dyn PluginRunner + Send + Sync>) {
    drop(ptr);
}

pub struct TestPlugin {}

impl Default for TestPlugin {
    fn default() -> Self {
        Self::new()
    }
}
impl TestPlugin {
    pub fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        TestPlugin {}
    }
    pub async fn test(&self, arg: &[u8]) -> Result<Vec<Vec<u8>>> {
        let arg = TestArgs::decode(arg).unwrap_or(TestArgs {
            args: vec![String::from_utf8_lossy(arg).to_string()],
        });
        let data = arg.args;
        Ok(vec![format!("end test arg={:?}", &data).into_bytes()])
    }
}

#[async_trait]
impl PluginRunner for TestPlugin {
    fn name(&self) -> String {
        // specify as same string as worker.runner
        String::from("Test")
    }
    fn load(&mut self, runner_settings: Vec<u8>) -> Result<()> {
        tracing::info!("Test plugin load!");
        TestRunnerSettings::decode(runner_settings.as_slice())?;
        Ok(())
    }
    fn run(&mut self, arg: Vec<u8>) -> Result<Vec<Vec<u8>>> {
        let arg_clone = arg.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move { self.test(arg_clone.as_slice()).await })
    }
    fn begin_stream(&mut self, arg: Vec<u8>) -> Result<()> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        // default implementation (return empty)
        Err(anyhow::anyhow!("not implemented"))
    }
    fn cancel(&self) -> bool {
        tracing::warn!("Test plugin cancel: not implemented!");
        false
    }
    fn is_canceled(&self) -> bool {
        tracing::warn!("Test plugin is_canceled: not implemented!");
        false
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../../../proto/protobuf/test_runner.proto").to_string()
    }
    fn job_args_proto(&self) -> String {
        include_str!("../../../proto/protobuf/test_args.proto").to_string()
    }
    fn result_output_proto(&self) -> Option<String> {
        None
    }
    // if true, use job result of before job, else use job args from request
    fn use_job_result(&self) -> bool {
        false
    }
    fn output_as_stream(&self) -> bool {
        false
    }
}
