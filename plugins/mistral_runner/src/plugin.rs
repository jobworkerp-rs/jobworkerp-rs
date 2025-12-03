//! MistralPlugin - MultiMethodPluginRunner implementation
//!
//! Main plugin entry point that implements the MultiMethodPluginRunner trait

use crate::cancel::{new_shared_cancel_state, SharedCancelState};
use crate::chat::MistralChatService;
use crate::completion::MistralCompletionService;
use crate::core::{MistralLlmServiceImpl, ToolCallingConfig};
use crate::grpc::FunctionServiceClient;
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmChatArgs, LlmCompletionArgs,
};
use jobworkerp_runner::runner::plugins::MultiMethodPluginRunner;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Stream state for receive_stream()
struct StreamState {
    receiver: mpsc::Receiver<Vec<u8>>,
}

/// Main MistralRS plugin implementation
pub struct MistralPlugin {
    /// Plugin-owned tokio runtime for CPU-bound operations
    runtime: tokio::runtime::Runtime,
    /// Chat service instance (lazy initialized)
    chat_service: Option<Arc<MistralChatService>>,
    /// Completion service instance (lazy initialized)
    completion_service: Option<Arc<MistralCompletionService>>,
    /// gRPC client for FunctionService (lazy initialized)
    grpc_client: Option<Arc<FunctionServiceClient>>,
    /// Tool calling configuration
    tool_config: ToolCallingConfig,
    /// Cancellation state
    cancel_state: SharedCancelState,
    /// Current streaming state (for receive_stream)
    stream_state: Arc<RwLock<Option<StreamState>>>,
    /// Current metadata for streaming
    current_metadata: Arc<RwLock<HashMap<String, String>>>,
}

impl MistralPlugin {
    pub fn new() -> Self {
        crate::tracing_helper::init_tracing();

        // Create dedicated runtime for CPU-bound LLM operations
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime for MistralPlugin");

        tracing::info!("MistralPlugin created");

        Self {
            runtime,
            chat_service: None,
            completion_service: None,
            grpc_client: None,
            tool_config: ToolCallingConfig::default(),
            cancel_state: new_shared_cancel_state(),
            stream_state: Arc::new(RwLock::new(None)),
            current_metadata: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MistralPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl MultiMethodPluginRunner for MistralPlugin {
    fn name(&self) -> String {
        "MistralLocalLLM".to_string()
    }

    fn description(&self) -> String {
        "Local LLM inference using MistralRS with tool calling support via gRPC".to_string()
    }

    fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // Decode LocalRunnerSettings
        let local_settings = LocalRunnerSettings::decode(settings.as_slice())?;

        // Initialize services within plugin runtime
        self.runtime.block_on(async {
            // Initialize core service
            let core_service = Arc::new(MistralLlmServiceImpl::new(&local_settings).await?);

            // Initialize gRPC client if endpoint is configured
            if let Some(endpoint) = &self.tool_config.grpc_endpoint {
                match FunctionServiceClient::connect(
                    endpoint,
                    self.tool_config.connection_timeout_sec,
                )
                .await
                {
                    Ok(client) => {
                        tracing::info!("Connected to FunctionService at {}", endpoint);
                        self.grpc_client = Some(Arc::new(client));
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to connect to FunctionService at {}: {:?}. Tool calling will be disabled.",
                            endpoint,
                            e
                        );
                    }
                }
            } else {
                tracing::info!(
                    "No MISTRAL_GRPC_ENDPOINT configured. Tool calling will be disabled."
                );
            }

            // Initialize chat service with optional gRPC client
            self.chat_service = Some(Arc::new(MistralChatService::new(
                core_service.clone(),
                self.grpc_client.clone(),
                self.tool_config.clone(),
            )));

            // Initialize completion service
            self.completion_service = Some(Arc::new(MistralCompletionService::new(core_service)));

            Ok::<(), anyhow::Error>(())
        })?;

        tracing::info!("MistralPlugin loaded successfully");
        Ok(())
    }

    fn run(
        &mut self,
        args: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Reset cancellation state
        self.runtime.block_on(async {
            let mut state = self.cancel_state.write().await;
            state.reset();
        });

        let result = self.runtime.block_on(async {
            let cancel_token = {
                let state = self.cancel_state.read().await;
                state.token()
            };

            match using {
                Some("chat") | None => {
                    let service = self
                        .chat_service
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Chat service not initialized"))?;

                    let chat_args = LlmChatArgs::decode(args.as_slice())?;
                    let result = service
                        .request_chat(chat_args, cancel_token, metadata.clone())
                        .await?;
                    Ok(result.encode_to_vec())
                }
                Some("completion") => {
                    let service = self
                        .completion_service
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("Completion service not initialized"))?;

                    let completion_args = LlmCompletionArgs::decode(args.as_slice())?;
                    let result = service
                        .request_completion(completion_args, &metadata)
                        .await?;
                    Ok(result.encode_to_vec())
                }
                Some(unknown) => Err(anyhow::anyhow!(
                    "Unknown method '{}'. Available: chat, completion",
                    unknown
                )),
            }
        });

        (result, metadata)
    }

    fn begin_stream(
        &mut self,
        arg: Vec<u8>,
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<()> {
        let method = using.unwrap_or("chat").to_string();

        // Create channel for streaming
        let (tx, rx) = mpsc::channel(100);

        // Reset cancellation state
        self.runtime.block_on(async {
            let mut state = self.cancel_state.write().await;
            state.reset();
        });

        // Store metadata for later use
        self.runtime.block_on(async {
            let mut meta = self.current_metadata.write().await;
            *meta = metadata.clone();
        });

        let chat_service = self.chat_service.clone();
        let completion_service = self.completion_service.clone();
        let cancel_state = self.cancel_state.clone();
        let metadata_for_task = metadata;

        // Spawn streaming task
        self.runtime.spawn(async move {
            let cancel_token = {
                let state = cancel_state.read().await;
                state.token()
            };

            let result = match method.as_str() {
                "chat" => {
                    if let Some(service) = chat_service {
                        let chat_args = match LlmChatArgs::decode(arg.as_slice()) {
                            Ok(a) => a,
                            Err(e) => {
                                tracing::error!("Failed to decode chat args: {:?}", e);
                                return;
                            }
                        };
                        service
                            .stream_chat(chat_args, tx, cancel_token, metadata_for_task)
                            .await
                    } else {
                        Err(anyhow::anyhow!("Chat service not initialized"))
                    }
                }
                "completion" => {
                    if let Some(service) = completion_service {
                        let completion_args = match LlmCompletionArgs::decode(arg.as_slice()) {
                            Ok(a) => a,
                            Err(e) => {
                                tracing::error!("Failed to decode completion args: {:?}", e);
                                return;
                            }
                        };
                        service.stream_completion(completion_args, tx).await
                    } else {
                        Err(anyhow::anyhow!("Completion service not initialized"))
                    }
                }
                _ => Err(anyhow::anyhow!("Unknown method for streaming")),
            };

            if let Err(e) = result {
                tracing::error!("Streaming error: {:?}", e);
            }
        });

        // Store stream state
        self.runtime.block_on(async {
            let mut state = self.stream_state.write().await;
            *state = Some(StreamState { receiver: rx });
        });

        Ok(())
    }

    fn receive_stream(&mut self) -> Result<Option<Vec<u8>>> {
        self.runtime.block_on(async {
            let mut state = self.stream_state.write().await;

            if let Some(stream_state) = state.as_mut() {
                match stream_state.receiver.recv().await {
                    Some(data) => Ok(Some(data)),
                    None => {
                        // Stream ended
                        *state = None;
                        Ok(None)
                    }
                }
            } else {
                Ok(None)
            }
        })
    }

    fn cancel(&mut self) -> bool {
        self.runtime.block_on(async {
            let mut state = self.cancel_state.write().await;
            let cancelled = state.cancel();
            if cancelled {
                tracing::info!("MistralPlugin: cancellation requested");
            }
            cancelled
        })
    }

    fn is_canceled(&self) -> bool {
        self.runtime.block_on(async {
            let state = self.cancel_state.read().await;
            state.is_cancelled()
        })
    }

    fn runner_settings_proto(&self) -> String {
        // Return the LocalRunnerSettings proto path
        "jobworkerp/runner/llm/runner.proto".to_string()
    }

    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        let mut schemas = HashMap::new();

        // Chat method
        schemas.insert(
            "chat".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: "jobworkerp/runner/llm/chat_args.proto".to_string(),
                result_proto: "jobworkerp/runner/llm/chat_result.proto".to_string(),
                description: Some("Chat completion with optional tool calling".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );

        // Completion method
        schemas.insert(
            "completion".to_string(),
            proto::jobworkerp::data::MethodSchema {
                args_proto: "jobworkerp/runner/llm/completion_args.proto".to_string(),
                result_proto: "jobworkerp/runner/llm/completion_result.proto".to_string(),
                description: Some("Text completion without tool calling".to_string()),
                output_type: proto::jobworkerp::data::StreamingOutputType::Both as i32,
            },
        );

        schemas
    }

    fn settings_schema(&self) -> String {
        let schema = schemars::schema_for!(LocalRunnerSettings);
        serde_json::to_string(&schema).unwrap_or_else(|_| "{}".to_string())
    }
}

impl Drop for MistralPlugin {
    fn drop(&mut self) {
        tracing::info!("MistralPlugin dropped");
    }
}
