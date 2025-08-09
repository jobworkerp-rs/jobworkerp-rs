// Phase 1: 不要な中間レイヤー削除完了
// pub mod tool_calling_service;  // Phase 2で追加予定
// pub mod message_converter;     // Phase 2で追加予定

use super::super::mistral::{
    DefaultLLMResultConverter, LLMRequestConverter, LLMResultConverter,
    MistralLlmServiceImpl,
};
use anyhow::Result;
use app::app::function::{FunctionApp, FunctionAppImpl, UseFunctionApp};
use futures::stream::{BoxStream, StreamExt};
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_chat_result::message_content, llm_runner_settings::LocalRunnerSettings, LlmChatArgs,
    LlmChatResult,
};
use opentelemetry::Context;
use serde_json;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MistralChatService {
    pub service: Arc<MistralLlmServiceImpl>,
    pub function_app: Arc<FunctionAppImpl>,
}

impl MistralChatService {
    pub async fn new(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
    ) -> Result<Self> {
        let service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);
        Ok(Self {
            service,
            function_app,
        })
    }

    pub async fn new_with_function_app(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
    ) -> Result<Self> {
        let service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);
        Ok(Self {
            service,
            function_app,
        })
    }

    pub async fn request_chat(
        &self,
        args: LlmChatArgs,
        _cx: Context,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        if self.has_function_calling(&args) {
            // Handle function calling with tool calls
            self.request_chat_with_tools(args, metadata).await
        } else {
            // Simple chat without tools
            let request_builder = self.build_request(&args, false).await?;
            let response = self.service.request_chat(request_builder).await?;
            let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);
            Ok(result)
        }
    }

    fn has_function_calling(&self, args: &LlmChatArgs) -> bool {
        args.function_options
            .as_ref()
            .map(|opts| opts.use_function_calling)
            .unwrap_or(false)
    }

    async fn request_chat_with_tools(
        &self,
        mut args: LlmChatArgs,
        metadata: HashMap<String, String>,
    ) -> Result<LlmChatResult> {
        const MAX_TOOL_ITERATIONS: usize = 5;
        // DEFAULT_TIMEOUT_SEC removed as it's unused in Phase 1

        let metadata = Arc::new(metadata);
        let mut iteration = 0;

        loop {
            iteration += 1;
            if iteration > MAX_TOOL_ITERATIONS {
                tracing::warn!("Max tool call iterations ({}) reached", MAX_TOOL_ITERATIONS);
                break;
            }

            // Build request with tools
            let request_builder = self.build_request(&args, false).await?;

            // Send request to model
            let response = self.service.request_chat(request_builder).await?;

            // Convert response to LlmChatResult
            let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

            // Check if response contains tool calls
            if let Some(content) = &result.content {
                if let Some(message_content::Content::ToolCalls(tool_calls)) = &content.content {
                    tracing::debug!("Processing {} tool calls", tool_calls.calls.len());

                    // Process tool calls
                    for tool_call in &tool_calls.calls {
                        let tool_result = self.execute_tool_call(tool_call, metadata.clone()).await;

                        // Add tool result to conversation
                        args.messages.push(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatMessage {
                            role: jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::ChatRole::Tool as i32,
                            content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::MessageContent {
                                content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_args::message_content::Content::Text(tool_result)),
                            }),
                        });
                    }

                    // Continue the loop for next iteration
                    continue;
                } else {
                    // No tool calls, return result
                    return Ok(result);
                }
            } else {
                // No content, return result
                return Ok(result);
            }
        }

        // Fallback: return last result without tools
        let request_builder = self.build_request(&args, false).await?;
        let response = self.service.request_chat(request_builder).await?;
        let result = DefaultLLMResultConverter::convert_chat_completion_result(&response);
        Ok(result)
    }

    async fn execute_tool_call(
        &self,
        tool_call: &jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::ToolCall,
        metadata: Arc<HashMap<String, String>>,
    ) -> String {
        // DEFAULT_TIMEOUT_SEC removed as it's unused in Phase 1

        let function_app = &self.function_app;
        tracing::debug!(
            "Executing tool: {} with args: {}",
            tool_call.fn_name,
            tool_call.fn_arguments
        );

        // Parse arguments
        let arguments_obj = match serde_json::from_str(&tool_call.fn_arguments) {
            Ok(serde_json::Value::Object(obj)) => obj,
            Ok(_) => {
                tracing::warn!("Tool arguments are not an object, using empty object");
                serde_json::Map::new()
            }
            Err(e) => {
                tracing::warn!("Failed to parse tool arguments: {}, using empty object", e);
                serde_json::Map::new()
            }
        };

        // Execute tool
        match function_app
            .call_function_for_llm(
                metadata,
                &tool_call.fn_name,
                Some(arguments_obj),
                30, // DEFAULT_TIMEOUT_SEC hardcoded for Phase 1
            )
            .await
        {
            Ok(result) => {
                tracing::debug!("Tool execution succeeded: {}", result);
                result.to_string()
            }
            Err(error) => {
                tracing::info!(
                    "Tool execution failed for: {}, error: {}",
                    tool_call.fn_name,
                    error
                );
                format!("Error executing tool '{}': {}", tool_call.fn_name, error)
            }
        }
    }

    pub async fn request_stream_chat(
        &self,
        args: LlmChatArgs,
    ) -> Result<BoxStream<'static, LlmChatResult>> {
        // Convert args to request builder for streaming
        let request_builder = self.build_request(&args, true).await?;

        // Get the underlying model for streaming
        let model = self.service.model.clone();

        // Create channel for streaming results
        let (tx, rx) = futures::channel::mpsc::unbounded();

        // Spawn task to handle MistralRS streaming
        tokio::spawn(async move {
            let result = async {
                let mut mistral_stream = model.stream_chat_request(request_builder).await?;

                while let Some(chunk) = mistral_stream.next().await {
                    let llm_result = match chunk {
                        mistralrs::Response::Chunk(chunk_response) => {
                            // Convert chunk to LlmChatResult using existing converter
                            DefaultLLMResultConverter::convert_chat_completion_chunk_result(
                                &chunk_response,
                            )
                        }
                        mistralrs::Response::CompletionChunk(_completion_chunk) => {
                            // Handle completion chunks (fallback case)
                            tracing::warn!("Received unexpected completion chunk in chat stream");
                            LlmChatResult {
                                content: None,
                                reasoning_content: None,
                                done: true,
                                usage: None,
                            }
                        }
                        _ => {
                            // Handle unexpected response types
                            tracing::warn!("Received unexpected response type in stream");
                            LlmChatResult {
                                content: None,
                                reasoning_content: None,
                                done: true,
                                usage: None,
                            }
                        }
                    };

                    // Send result through channel
                    if tx.unbounded_send(llm_result).is_err() {
                        // Receiver dropped, stop streaming
                        break;
                    }
                }

                anyhow::Result::<()>::Ok(())
            }
            .await;

            if let Err(e) = result {
                tracing::error!("Error in MistralRS chat stream: {}", e);
            }
        });

        // Convert receiver to BoxStream
        Ok(rx.boxed())
    }
}

impl UseFunctionApp for MistralChatService {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

impl LLMRequestConverter for MistralChatService {}
