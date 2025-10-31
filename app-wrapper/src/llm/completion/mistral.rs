use super::super::mistral::{
    DefaultLLMResultConverter, LLMRequestConverter, LLMResultConverter, MistralLlmServiceImpl,
};
use anyhow::Result;
use app::app::function::function_set::{FunctionSetAppImpl, UseFunctionSetApp};
use app::app::function::{FunctionAppImpl, UseFunctionApp};
use futures::stream::{BoxStream, StreamExt};
use jobworkerp_runner::jobworkerp::runner::llm::{
    llm_runner_settings::LocalRunnerSettings, LlmCompletionArgs, LlmCompletionResult,
};
use opentelemetry::Context;
use std::collections::HashMap;
use std::sync::Arc;

pub struct MistralCompletionService {
    pub service: Arc<MistralLlmServiceImpl>,
    pub function_app: Arc<FunctionAppImpl>,
    pub function_set_app: Arc<FunctionSetAppImpl>,
}

impl MistralCompletionService {
    pub async fn new(
        settings: LocalRunnerSettings,
        function_app: Arc<FunctionAppImpl>,
        function_set_app: Arc<FunctionSetAppImpl>,
    ) -> Result<Self> {
        let service = Arc::new(MistralLlmServiceImpl::new(&settings).await?);
        Ok(Self {
            service,
            function_app,
            function_set_app,
        })
    }

    pub async fn request_chat(
        &self,
        args: LlmCompletionArgs,
        _cx: Context,
        _metadata: HashMap<String, String>,
    ) -> Result<LlmCompletionResult> {
        // Convert args to request builder using service's converter
        let request_builder = self.build_completion_request(&args, false).await?;

        // Send request to model (using chat API for completion)
        let response = self.service.request_chat(request_builder).await?;

        // Convert response to LlmCompletionResult
        // For now, we use the chat completion response and extract text content
        let chat_result = DefaultLLMResultConverter::convert_chat_completion_result(&response);

        // Convert chat result to completion result
        let completion_result = LlmCompletionResult {
            content: chat_result.content.map(|c| match c.content {
                Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) => {
                    jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::MessageContent {
                        content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text(text)),
                    }
                }
                _ => {
                    jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::MessageContent {
                        content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text("[Unsupported content type]".to_string())),
                    }
                }
            }),
            reasoning_content: None,
            done: chat_result.done,
            context: None,
            usage: chat_result.usage.map(|u| {
                jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::Usage {
                    model: u.model,
                    prompt_tokens: u.prompt_tokens,
                    completion_tokens: u.completion_tokens,
                    total_prompt_time_sec: u.total_prompt_time_sec,
                    total_completion_time_sec: u.total_completion_time_sec,
                }
            }),
        };

        Ok(completion_result)
    }

    pub async fn request_stream_chat(
        &self,
        args: LlmCompletionArgs,
    ) -> Result<BoxStream<'static, LlmCompletionResult>> {
        // Convert args to request builder for streaming using service's converter
        let request_builder = self.build_completion_request(&args, true).await?;

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
                        mistralrs::Response::CompletionChunk(completion_chunk) => {
                            // Convert completion chunk to LlmCompletionResult using existing converter
                            DefaultLLMResultConverter::convert_completion_chunk_result(&completion_chunk)
                        }
                        mistralrs::Response::Chunk(chunk_response) => {
                            // Convert chat chunk to completion result (fallback case)
                            let chat_result = DefaultLLMResultConverter::convert_chat_completion_chunk_result(&chunk_response);
                            // Convert chat result to completion result
                            LlmCompletionResult {
                                content: chat_result.content.map(|c| match c.content {
                                    Some(jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text)) => {
                                        jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::MessageContent {
                                            content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text(text)),
                                        }
                                    }
                                    _ => {
                                        jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::MessageContent {
                                            content: Some(jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text("[Unsupported content type]".to_string())),
                                        }
                                    }
                                }),
                                reasoning_content: None,
                                done: chat_result.done,
                                context: None,
                                usage: chat_result.usage.map(|u| {
                                    jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::Usage {
                                        model: u.model,
                                        prompt_tokens: u.prompt_tokens,
                                        completion_tokens: u.completion_tokens,
                                        total_prompt_time_sec: u.total_prompt_time_sec,
                                        total_completion_time_sec: u.total_completion_time_sec,
                                    }
                                }),
                            }
                        }
                        _ => {
                            // Handle unexpected response types
                            tracing::warn!("Received unexpected response type in completion stream");
                            LlmCompletionResult {
                                content: None,
                                reasoning_content: None,
                                done: true,
                                context: None,
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
            }.await;

            if let Err(e) = result {
                tracing::error!("Error in MistralRS completion stream: {}", e);
            }
        });

        // Convert receiver to BoxStream
        Ok(rx.boxed())
    }
}

impl UseFunctionApp for MistralCompletionService {
    fn function_app(&self) -> &FunctionAppImpl {
        &self.function_app
    }
}

impl UseFunctionSetApp for MistralCompletionService {
    fn function_set_app(&self) -> &FunctionSetAppImpl {
        &self.function_set_app
    }
}

impl LLMRequestConverter for MistralCompletionService {}
