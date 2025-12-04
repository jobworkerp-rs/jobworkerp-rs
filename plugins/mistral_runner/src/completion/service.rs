//! Completion service for MistralRS plugin
//!
//! Provides text completion without tool calling

use crate::conversion::{RequestConverter, ResultConverter};
use crate::core::MistralLlmServiceImpl;
use crate::tracing_helper::{extract_parent_context, MistralOtelClient};
use anyhow::Result;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmCompletionArgs, LlmCompletionResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Completion service (no tool calling)
pub struct MistralCompletionService {
    core_service: Arc<MistralLlmServiceImpl>,
    otel_client: Option<MistralOtelClient>,
}

impl MistralCompletionService {
    pub fn new(core_service: Arc<MistralLlmServiceImpl>) -> Self {
        // Initialize OTel client if OTLP_ADDR is set
        let otel_client = if std::env::var("OTLP_ADDR").is_ok() {
            Some(MistralOtelClient::new())
        } else {
            None
        };

        Self {
            core_service,
            otel_client,
        }
    }

    /// Get model name for tracing
    fn model_name(&self) -> &str {
        self.core_service.model_name()
    }

    /// Extract model options for tracing
    fn extract_model_options(
        &self,
        args: &LlmCompletionArgs,
    ) -> Option<HashMap<String, serde_json::Value>> {
        args.options.as_ref().map(|opts| {
            let mut params = HashMap::new();
            if let Some(t) = opts.temperature {
                params.insert("temperature".to_string(), serde_json::json!(t));
            }
            if let Some(t) = opts.max_tokens {
                params.insert("max_tokens".to_string(), serde_json::json!(t));
            }
            if let Some(t) = opts.top_p {
                params.insert("top_p".to_string(), serde_json::json!(t));
            }
            params
        })
    }

    /// Execute completion request (non-streaming)
    pub async fn request_completion(
        &self,
        args: LlmCompletionArgs,
        metadata: &HashMap<String, String>,
    ) -> Result<LlmCompletionResult> {
        let request = RequestConverter::build_completion_request(&args)?;

        // Execute with OpenTelemetry tracing if enabled
        if let Some(otel) = &self.otel_client {
            let model_params = self.extract_model_options(&args);

            let span_attrs = otel.create_completion_span_attributes(
                self.model_name(),
                &args.prompt,
                model_params.as_ref(),
                metadata,
            );

            // Extract parent context from metadata for distributed tracing
            let parent_context = extract_parent_context(metadata);

            // Execute within traced span with parent context propagation
            let core_service = self.core_service.clone();
            let response = otel
                .execute_with_span(span_attrs, Some(parent_context), async move {
                    core_service.request_chat(request).await
                })
                .await?;

            tracing::debug!(
                "Completion traced with parent context: model={}, tokens={}",
                self.model_name(),
                response.usage.total_tokens
            );

            // Convert chat response to completion result
            let chat_result = ResultConverter::convert_chat_completion_result(&response);
            Ok(self.convert_chat_to_completion_result(chat_result))
        } else {
            let response = self.core_service.request_chat(request).await?;

            // Convert chat response to completion result
            let chat_result = ResultConverter::convert_chat_completion_result(&response);
            Ok(self.convert_chat_to_completion_result(chat_result))
        }
    }

    /// Stream completion to a channel
    pub async fn stream_completion(
        &self,
        args: LlmCompletionArgs,
        tx: mpsc::Sender<Vec<u8>>,
    ) -> Result<()> {
        use futures::StreamExt;
        use prost::Message;

        let request = RequestConverter::build_completion_request(&args)?;
        let mut stream = self.core_service.stream_chat(request).await?;

        while let Some(response) = stream.next().await {
            let result = match response {
                mistralrs::Response::Chunk(chunk) => {
                    let chat_result = ResultConverter::convert_chat_completion_chunk_result(&chunk);
                    self.convert_chat_to_completion_result(chat_result)
                }
                mistralrs::Response::Done(completion) => {
                    let chat_result = ResultConverter::convert_chat_completion_result(&completion);
                    self.convert_chat_to_completion_result(chat_result)
                }
                _ => continue,
            };

            let encoded = result.encode_to_vec();
            if tx.send(encoded).await.is_err() {
                tracing::debug!("Stream receiver dropped");
                break;
            }

            if result.done {
                break;
            }
        }

        Ok(())
    }

    fn convert_chat_to_completion_result(
        &self,
        chat_result: jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult,
    ) -> LlmCompletionResult {
        LlmCompletionResult {
            content: chat_result.content.map(|c| {
                jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::MessageContent {
                    content: c.content.map(|content| {
                        match content {
                            jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::message_content::Content::Text(text) => {
                                jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text(text)
                            }
                            _ => {
                                jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content::Content::Text(String::new())
                            }
                        }
                    }),
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
}
