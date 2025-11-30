use anyhow::Result;
use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use hello::{HelloArgs, HelloRunnerResult, HelloRunnerSettings};
use jobworkerp_runner::runner::plugins::PluginRunner;
use prost::Message;
use std::{alloc::System, collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, Mutex};
use tracing::Level; // Add this line to import the Message trait

pub mod hello {
    tonic::include_proto!("hello");
}

#[global_allocator]
static ALLOCATOR: System = System;

// suppress warn improper_ctypes_definitions
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_plugin() -> Box<dyn PluginRunner + Send + Sync> {
    Box::new(HelloPlugin::new())
}
/// # Safety
///
/// This function is unsafe because it dereferences a raw pointer. The caller
/// must ensure that the pointer is valid and that it was created by the
/// `load_plugin` function. The caller must also ensure that the `Box` created
/// by `Box::from_raw` is not used after it has been dropped.
#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_plugin(ptr: Box<dyn PluginRunner + Send + Sync>) {
    drop(ptr);
}

pub struct HelloPlugin {
    rt: tokio::runtime::Runtime,
    running: Arc<Mutex<bool>>,
    stream: Arc<Mutex<BoxStream<'static, Vec<u8>>>>,
    args: HelloArgs,
}

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
        HelloPlugin {
            rt: tokio::runtime::Runtime::new().unwrap(),
            running: Arc::new(Mutex::new(false)),
            stream: Arc::new(Mutex::new(futures::stream::empty().boxed())),
            args: HelloArgs {
                arg: "".to_string(),
            },
        }
    }
    pub async fn hello(&self, arg: &[u8]) -> Result<Vec<u8>> {
        // XXX to test easy
        let arg = HelloArgs::decode(arg).unwrap_or(HelloArgs {
            arg: String::from_utf8_lossy(arg).to_string(),
        });
        let id = uuid::Uuid::new_v4();
        let start = chrono::Utc::now().to_rfc3339();
        let data = arg.arg;
        println!(
            "========== [{}] HelloPlugin run: Hello! {}: {} ==========",
            start, &data, id
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        println!(
            "========== [{}] END OF HelloPlugin: until {}: {} ==========",
            start,
            chrono::Utc::now().to_rfc3339(),
            id
        );
        Ok(HelloRunnerResult {
            data: format!("SUCCESS: {} arg={}", id, &data),
        }
        .encode_to_vec())
    }
    pub async fn async_run(hello_name: String) -> Result<BoxStream<'static, Vec<u8>>> {
        let (tx, rx) = mpsc::channel(100);
        // heavy task
        tokio::spawn(async move {
            let start = chrono::Utc::now().to_rfc3339();
            let id = uuid::Uuid::new_v4();
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!(
                "========== [{}] HelloPlugin run_stream: Hello! {}: {} ==========",
                start, &hello_name, id
            );
            for c in hello_name.chars() {
                let c = HelloRunnerResult {
                    data: c.to_string(),
                }
                .encode_to_vec();
                let _ = tx.send(c).await;
                tokio::time::sleep(Duration::from_millis(1000)).await;
            }
            drop(tx);
            println!(
                "========== [{}] END OF HelloPlugin: until {}: {} ==========",
                start,
                chrono::Utc::now().to_rfc3339(),
                id
            );
        });
        Ok(tokio_stream::wrappers::ReceiverStream::new(rx).boxed())
    }
}

#[async_trait]
impl PluginRunner for HelloPlugin {
    fn name(&self) -> String {
        // specify as same string as worker.runner_settings
        String::from("HelloPlugin")
    }
    fn description(&self) -> String {
        String::from("HelloPlugin: Hello world plugin version 0.1")
    }
    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        tracing::info!("HelloPlugin load!");
        // setup with settings (if needed)
        HelloRunnerSettings::decode(settings.as_slice())?;
        println!("==== HelloPlugin loaded: {settings:?}");

        Ok(())
    }
    fn run(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let arg_clone = arg.clone();
        self.rt
            .block_on(async { (self.hello(arg_clone.as_slice()).await, metadata) })
    }
    fn begin_stream(&mut self, arg: Vec<u8>, _metadata: HashMap<String, String>) -> Result<()> {
        // decode the arguments
        self.args = HelloArgs::decode(arg.as_slice())?;
        // process the arguments (dummy)
        self.rt
            .block_on(async { tokio::time::sleep(Duration::from_millis(200)).await });
        Ok(())
    }
    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>>
    where
        Self: Send + 'static,
    {
        self.rt.block_on(async {
            {
                // setup running stream if not running
                let mut running = self.running.lock().await;
                if !*running {
                    let hello_name = self.args.arg.clone();
                    let new_stream = HelloPlugin::async_run(hello_name).await?;
                    *self.stream.lock().await = new_stream;
                    *running = true;
                }
            }
            let mut stream_lock = self.stream.lock().await;
            let res = stream_lock.next().await;
            if res.is_none() {
                println!("========== END OF HelloPlugin: stream finished. end running ==========");
                *self.running.lock().await = false;
                *stream_lock = futures::stream::empty().boxed();
            }
            Ok(res)
        })
    }
    fn cancel(&mut self) -> bool {
        // cancel the running task
        // *self.running.lock().unwrap() = false;
        // kill running task
        // true
        tracing::warn!("HelloPlugin cancel is not implemented");
        false
    }
    fn is_canceled(&self) -> bool {
        // check if the running task is canceled
        false
    }
    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/hello_runner.proto").to_string()
    }

    // Phase 6.6.4: Implement method_proto_map for new unified API
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();
        schemas.insert(
            "run".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: include_str!("../protobuf/hello_job_args.proto").to_string(),
                result_proto: include_str!("../protobuf/hello_result.proto").to_string(),
                description: Some("Hello world plugin execution".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );
        schemas
    }
    fn settings_schema(&self) -> String {
        let schema = schemars::schema_for!(HelloRunnerSettings);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(HelloArgs);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                "".to_string()
            }
        }
    }
    fn output_json_schema(&self) -> Option<String> {
        let schema = schemars::schema_for!(HelloRunnerResult);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in input_json_schema: {:?}", e);
                None
            }
        }
    }
}

impl Drop for HelloPlugin {
    fn drop(&mut self) {
        // println!("====== HelloPlugin dropped");
        // cancel the running task
        //     *self.running.lock().await = false;
        //     *self.stream.lock().await = futures::stream::empty().boxed();
    }
}
