use crate::mistral_runner::{MistralChatArgs, MistralChatResult, MistralRunnerSettings};
use anyhow::Result;
use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use jobworkerp_runner::runner::plugins::PluginRunner;
use prost::Message;
use std::{alloc::System, collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::Level;

pub mod conversion;
pub mod mistral;

pub mod mistral_runner {
    tonic::include_proto!("jobworkerp.runner.mistral");
}

#[global_allocator]
static ALLOCATOR: System = System;

// suppress warn improper_ctypes_definitions
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_plugin() -> Box<dyn PluginRunner + Send + Sync> {
    Box::new(MistralPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_plugin(ptr: Box<dyn PluginRunner + Send + Sync>) {
    drop(ptr);
}

pub struct MistralPlugin {
    rt: tokio::runtime::Runtime,
    service: Arc<Mutex<Option<mistral::MistralRSService>>>,
    running: Arc<Mutex<bool>>,
    stream: Arc<Mutex<BoxStream<'static, Vec<u8>>>>,
}

impl Default for MistralPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl MistralPlugin {
    pub fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        MistralPlugin {
            rt: tokio::runtime::Runtime::new().unwrap(),
            service: Arc::new(Mutex::new(None)),
            running: Arc::new(Mutex::new(false)),
            stream: Arc::new(Mutex::new(futures::stream::empty().boxed())),
        }
    }
}

#[async_trait]
impl PluginRunner for MistralPlugin {
    fn name(&self) -> String {
        String::from("MistralPlugin")
    }

    fn description(&self) -> String {
        String::from("MistralPlugin: Local LLM runner using mistral.rs")
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        tracing::info!("MistralPlugin load!");
        let settings = MistralRunnerSettings::decode(settings.as_slice())?;

        self.rt.block_on(async {
            let service = mistral::MistralRSService::new(&settings).await?;
            *self.service.lock().await = Some(service);
            Ok::<(), anyhow::Error>(())
        })?;

        Ok(())
    }

    fn run(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let arg = match MistralChatArgs::decode(arg.as_slice()) {
            Ok(a) => a,
            Err(e) => return (Err(e.into()), metadata),
        };

        // Extract trace context from metadata if needed (omitted for brevity, but should be handled)
        let ctx = opentelemetry::Context::current();

        self.rt.block_on(async {
            let service_guard = self.service.lock().await;
            if let Some(service) = service_guard.as_ref() {
                match service.request_chat(&arg).await {
                    Ok(result) => (Ok(result.encode_to_vec()), metadata),
                    Err(e) => (Err(e), metadata),
                }
            } else {
                (
                    Err(anyhow::anyhow!("Mistral service not initialized")),
                    metadata,
                )
            }
        })
    }

    fn begin_stream(&mut self, arg: Vec<u8>, metadata: HashMap<String, String>) -> Result<()> {
        let arg = MistralChatArgs::decode(arg.as_slice())?;

        // Extract trace context from metadata if needed
        let _ctx = opentelemetry::Context::current();

        self.rt.block_on(async {
            let service_guard = self.service.lock().await;
            if let Some(service) = service_guard.as_ref() {
                // Start streaming
                let stream = service.stream_chat(&arg).await?;

                // Map stream items to encoded bytes
                let byte_stream = stream.map(|result| result.encode_to_vec());

                *self.stream.lock().await = byte_stream.boxed();
                *self.running.lock().await = true;
                Ok(())
            } else {
                Err(anyhow::anyhow!("Mistral service not initialized"))
            }
        })
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>>
    where
        Self: Send + 'static,
    {
        self.rt.block_on(async {
            let mut stream_lock = self.stream.lock().await;
            let res = stream_lock.next().await;
            if res.is_none() {
                *self.running.lock().await = false;
            }
            Ok(res)
        })
    }

    fn cancel(&mut self) -> bool {
        // Cancellation logic would go here (e.g., dropping the stream or signaling a token)
        // For now, just return false as basic implementation
        false
    }

    fn is_canceled(&self) -> bool {
        false
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/mistral_runner.proto").to_string()
    }

    fn job_args_proto(&self) -> String {
        include_str!("../protobuf/mistral_chat_args.proto").to_string()
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../protobuf/mistral_result.proto").to_string())
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        proto::jobworkerp::data::StreamingOutputType::Both
    }

    fn settings_schema(&self) -> String {
        let schema = schemars::schema_for!(MistralRunnerSettings);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in settings_schema: {:?}", e);
                "".to_string()
            }
        }
    }

    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(MistralChatArgs);
        match serde_json::to_string(&schema) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("error in arguments_schema: {:?}", e);
                "".to_string()
            }
        }
    }

    fn output_json_schema(&self) -> Option<String> {
        let schema = schemars::schema_for!(MistralChatResult);
        match serde_json::to_string(&schema) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("error in output_json_schema: {:?}", e);
                None
            }
        }
    }
}
