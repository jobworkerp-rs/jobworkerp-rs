use anyhow::Result;
use async_trait::async_trait;
use jobworkerp_runner::runner::plugins::MultiMethodPluginRunner;
use prost::Message;
use std::{alloc::System, collections::HashMap};
use test::{TestArgs, TestRunnerSettings};
use tracing::Level; // Add this line to import the Message trait

pub mod test {
    tonic::include_proto!("_");
}

#[global_allocator]
static ALLOCATOR: System = System;

// Multi-method plugin FFI symbols
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_multi_method_plugin() -> Box<dyn MultiMethodPluginRunner + Send + Sync> {
    Box::new(TestPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_multi_method_plugin(ptr: Box<dyn MultiMethodPluginRunner + Send + Sync>) {
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
    pub async fn test(&self, arg: &[u8]) -> Result<Vec<u8>> {
        let arg = TestArgs::decode(arg).unwrap_or(TestArgs {
            args: vec![String::from_utf8_lossy(arg).to_string()],
        });
        let data = arg.args;
        Ok(format!("end test arg={:?}", &data).into_bytes())
    }
}

#[async_trait]
impl MultiMethodPluginRunner for TestPlugin {
    fn name(&self) -> String {
        // specify as same string as worker.runner
        String::from("Test")
    }
    fn description(&self) -> String {
        String::from("Test plugin description")
    }
    fn load(&mut self, runner_settings: Vec<u8>) -> Result<()> {
        tracing::info!("Test plugin load!");
        TestRunnerSettings::decode(runner_settings.as_slice()).unwrap_or(TestRunnerSettings {
            name: String::from("Test default"),
        });
        Ok(())
    }
    fn run(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let arg_clone = arg.clone();
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move { (self.test(arg_clone.as_slice()).await, metadata) })
    }
    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<()> {
        // default implementation (return empty)
        let _ = arg;
        Err(anyhow::anyhow!("not implemented"))
    }
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        // default implementation (return empty)
        Err(anyhow::anyhow!("not implemented"))
    }
    fn cancel(&mut self) -> bool {
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

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = std::collections::HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../../../proto/protobuf/test_args.proto").to_string(),
                result_proto: String::new(), // No result proto for this runner
                description: Some("Test plugin for runner testing".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::NonStreaming as i32,
            },
        );
        schemas
    }
}
