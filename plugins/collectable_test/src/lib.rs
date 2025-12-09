use anyhow::Result;
use async_trait::async_trait;
use collectable::{CollectableArgs, CollectableResult, CollectableRunnerSettings};
use futures::stream::BoxStream;
use futures::StreamExt;
use jobworkerp_runner::runner::plugins::CollectablePluginRunner;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, StreamingOutputType};
use std::{alloc::System, collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::Level;

pub mod collectable {
    tonic::include_proto!("collectable");
}

#[global_allocator]
static ALLOCATOR: System = System;

/// FFI symbol for collectable plugins (single-method with collect_stream support)
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn load_collectable_plugin() -> Box<dyn CollectablePluginRunner + Send + Sync> {
    Box::new(CollectableTestPlugin::new())
}

#[unsafe(no_mangle)]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn free_collectable_plugin(ptr: Box<dyn CollectablePluginRunner + Send + Sync>) {
    drop(ptr);
}

/// Collectable plugin test implementation
///
/// This plugin implements CollectablePluginRunner trait with streaming support
/// and custom collect_stream implementation.
pub struct CollectableTestPlugin {
    rt: tokio::runtime::Runtime,
    settings: Option<CollectableRunnerSettings>,
    args: Option<CollectableArgs>,
    stream_state: Arc<Mutex<StreamState>>,
}

struct StreamState {
    current_chunk: i32,
    total_chunks: i32,
    input: String,
    canceled: bool,
}

impl Default for CollectableTestPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectableTestPlugin {
    pub fn new() -> Self {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::INFO)
            .try_init();
        CollectableTestPlugin {
            rt: tokio::runtime::Runtime::new().unwrap(),
            settings: None,
            args: None,
            stream_state: Arc::new(Mutex::new(StreamState {
                current_chunk: 0,
                total_chunks: 0,
                input: String::new(),
                canceled: false,
            })),
        }
    }

    fn execute(&self, args: &[u8]) -> Result<Vec<u8>> {
        let input = CollectableArgs::decode(args).unwrap_or(CollectableArgs {
            input: String::from_utf8_lossy(args).to_string(),
            chunk_count: 1,
        });

        tracing::info!(
            "CollectableTestPlugin executing with input: {}, chunks: {}",
            input.input,
            input.chunk_count
        );

        let result = CollectableResult {
            message: format!("Processed: {}", input.input),
            chunk_index: 0,
            is_final: true,
        };

        Ok(result.encode_to_vec())
    }
}

#[async_trait]
impl CollectablePluginRunner for CollectableTestPlugin {
    fn name(&self) -> String {
        "CollectableTest".to_string()
    }

    fn description(&self) -> String {
        "Collectable plugin test with streaming and collect_stream support".to_string()
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        tracing::info!("CollectableTestPlugin load called");
        if !settings.is_empty() {
            self.settings = Some(CollectableRunnerSettings::decode(settings.as_slice())?);
        }
        Ok(())
    }

    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        (self.execute(&args), metadata)
    }

    fn begin_stream(&mut self, arg: Vec<u8>, _metadata: HashMap<String, String>) -> Result<()> {
        let args = CollectableArgs::decode(arg.as_slice()).unwrap_or(CollectableArgs {
            input: String::from_utf8_lossy(&arg).to_string(),
            chunk_count: 3,
        });

        self.args = Some(args.clone());

        self.rt.block_on(async {
            let mut state = self.stream_state.lock().await;
            state.current_chunk = 0;
            state.total_chunks = args.chunk_count.max(1);
            state.input = args.input;
            state.canceled = false;
        });

        tracing::info!(
            "CollectableTestPlugin begin_stream: {} chunks",
            args.chunk_count
        );
        Ok(())
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        self.rt.block_on(async {
            let mut state = self.stream_state.lock().await;

            if state.canceled {
                return Ok(None);
            }

            if state.current_chunk >= state.total_chunks {
                return Ok(None);
            }

            let chunk_index = state.current_chunk;
            let is_final = chunk_index == state.total_chunks - 1;

            let result = CollectableResult {
                message: format!("Chunk {}: {}", chunk_index, state.input),
                chunk_index,
                is_final,
            };

            state.current_chunk += 1;

            // Simulate some processing delay
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

            Ok(Some(result.encode_to_vec()))
        })
    }

    fn cancel(&mut self) -> bool {
        self.rt.block_on(async {
            let mut state = self.stream_state.lock().await;
            state.canceled = true;
        });
        tracing::info!("CollectableTestPlugin: canceled");
        true
    }

    fn is_canceled(&self) -> bool {
        self.rt
            .block_on(async { self.stream_state.lock().await.canceled })
    }

    fn runner_settings_proto(&self) -> String {
        include_str!("../protobuf/collectable_runner.proto").to_string()
    }

    fn job_args_proto(&self) -> String {
        include_str!("../protobuf/collectable_args.proto").to_string()
    }

    fn result_output_proto(&self) -> Option<String> {
        Some(include_str!("../protobuf/collectable_result.proto").to_string())
    }

    fn output_type(&self) -> StreamingOutputType {
        StreamingOutputType::Both
    }

    fn settings_schema(&self) -> String {
        let schema = schemars::schema_for!(CollectableRunnerSettings);
        serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize settings schema: {:?}", e);
            "{}".to_string()
        })
    }

    fn arguments_schema(&self) -> String {
        let schema = schemars::schema_for!(CollectableArgs);
        serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize arguments schema: {:?}", e);
            "{}".to_string()
        })
    }

    fn output_json_schema(&self) -> Option<String> {
        let schema = schemars::schema_for!(CollectableResult);
        Some(serde_json::to_string(&schema).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize output schema: {:?}", e);
            "{}".to_string()
        }))
    }

    /// Custom collect_stream implementation
    ///
    /// Merges multiple CollectableResult chunks by concatenating messages
    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(Vec<u8>, HashMap<String, String>)>> + Send>,
    > {
        Box::pin(async move {
            let mut collected_messages = Vec::new();
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        if let Ok(result) = CollectableResult::decode(data.as_slice()) {
                            collected_messages.push(result.message);
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    None => {}
                }
            }

            // Create final merged result
            let final_result = CollectableResult {
                message: collected_messages.join(" | "),
                chunk_index: collected_messages.len() as i32 - 1,
                is_final: true,
            };

            Ok((final_result.encode_to_vec(), metadata))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collectable_plugin_initialization() {
        let plugin = CollectableTestPlugin::new();
        assert_eq!(plugin.name(), "CollectableTest");
        assert!(plugin.description().contains("collect_stream"));
    }

    #[test]
    fn test_collectable_plugin_schemas() {
        let plugin = CollectableTestPlugin::new();

        assert!(!plugin.job_args_proto().is_empty());
        assert!(plugin.result_output_proto().is_some());
        assert!(!plugin.arguments_schema().is_empty());
        assert!(plugin.output_json_schema().is_some());
        assert_eq!(plugin.output_type(), StreamingOutputType::Both);
    }

    #[test]
    fn test_collectable_plugin_execution() {
        let mut plugin = CollectableTestPlugin::new();

        let args = CollectableArgs {
            input: "test".to_string(),
            chunk_count: 1,
        };

        let (result, _) = plugin.run(args.encode_to_vec(), HashMap::new());
        assert!(result.is_ok());

        let output = result.unwrap();
        let decoded = CollectableResult::decode(output.as_slice()).unwrap();
        assert!(decoded.is_final);
        assert!(decoded.message.contains("test"));
    }

    #[test]
    fn test_collectable_plugin_streaming() {
        let mut plugin = CollectableTestPlugin::new();

        let args = CollectableArgs {
            input: "stream_test".to_string(),
            chunk_count: 3,
        };

        // Begin stream
        let result = plugin.begin_stream(args.encode_to_vec(), HashMap::new());
        assert!(result.is_ok());

        // Receive chunks
        let mut chunks = Vec::new();
        loop {
            match plugin.receive_stream() {
                Ok(Some(data)) => {
                    let result = CollectableResult::decode(data.as_slice()).unwrap();
                    chunks.push(result);
                }
                Ok(None) => break,
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }

        assert_eq!(chunks.len(), 3);
        assert!(chunks[2].is_final);
    }
}
