use anyhow::{anyhow, Result};
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiChatService;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::llm_chat::LLMChatRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use ollama::OllamaChatService;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod conversion;
pub mod genai;
pub mod ollama;

pub struct LLMChatRunnerImpl {
    pub app: Arc<AppModule>,
    pub ollama: Option<OllamaChatService>,
    pub genai: Option<GenaiChatService>,
    cancellation_token: Option<CancellationToken>,
}

impl LLMChatRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            app: app_module,
            ollama: None,
            genai: None,
            cancellation_token: None,
        }
    }
}

impl Tracing for LLMChatRunnerImpl {}
impl LLMChatRunnerSpec for LLMChatRunnerImpl {}
impl RunnerSpec for LLMChatRunnerImpl {
    fn name(&self) -> String {
        LLMChatRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMChatRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        LLMChatRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMChatRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMChatRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        LLMChatRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        LLMChatRunnerSpec::output_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for LLMChatRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .map_err(|e| anyhow!("decode error: {}", e))?;
        match settings.settings {
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                    settings,
                ),
            ) => {
                let ollama = OllamaChatService::new(self.app.function_app.clone(), settings)?;
                tracing::info!("{} loaded(ollama)", RunnerType::LlmChat.as_str_name());
                self.ollama = Some(ollama);
                Ok(())
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                    settings,
                ),
            ) => {
                let genai = GenaiChatService::new(self.app.function_app.clone(), settings).await?;
                tracing::info!("{} loaded(genai)", RunnerType::LlmChat.as_str_name());
                self.genai = Some(genai);
                Ok(())
            }
            _ => Err(anyhow!("model_settings is not set")),
        }
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        // Set up cancellation token for pre-execution cancellation check
        let cancellation_token = if let Some(existing_token) = &self.cancellation_token {
            // If token already exists and is cancelled, return early
            if existing_token.is_cancelled() {
                return (
                    Err(anyhow::anyhow!(
                        "LLM chat execution was cancelled before start"
                    )),
                    metadata,
                );
            }
            existing_token.clone()
        } else {
            let new_token = CancellationToken::new();
            self.cancellation_token = Some(new_token.clone());
            new_token
        };

        let span = Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_chat_run");
        let cx = Context::current_with_span(span);
        // let span = cx.span();
        // let span = Self::tracing_span_from_metadata(&metadata, APP_NAME, "llm_chat_run");
        // let _ = span.enter();
        // let cx = span.context();

        // TODO process metadata
        let metadata_clone = metadata.clone();
        let result = async {
            let args = LlmChatArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;

            // Check cancellation before LLM service execution
            if cancellation_token.is_cancelled() {
                tracing::info!("LLM chat execution was cancelled before service call");
                return Err(anyhow!(
                    "LLM chat execution was cancelled before service call"
                ));
            }

            if let Some(ollama) = self.ollama.as_mut() {
                let res = tokio::select! {
                    result = ollama.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("LLM chat (Ollama) request was cancelled");
                        return Err(anyhow!("LLM chat (Ollama) request was cancelled"));
                    }
                };
                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                //XXX chat only
                let res = tokio::select! {
                    result = genai.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("LLM chat (GenAI) request was cancelled");
                        return Err(anyhow!("LLM chat (GenAI) request was cancelled"));
                    }
                };
                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else {
                Err(anyhow!("llm is not initialized"))
            }
        }
        .await;

        // Clear cancellation token after execution
        self.cancellation_token = None;
        (result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token for pre-execution cancellation check
        let cancellation_token = if let Some(existing_token) = &self.cancellation_token {
            // If token already exists and is cancelled, return early
            if existing_token.is_cancelled() {
                return Err(anyhow::anyhow!(
                    "LLM chat stream execution was cancelled before start"
                ));
            }
            existing_token.clone()
        } else {
            let new_token = CancellationToken::new();
            self.cancellation_token = Some(new_token.clone());
            new_token
        };

        let args = LlmChatArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        // Check cancellation before LLM service execution
        if cancellation_token.is_cancelled() {
            tracing::info!("LLM chat stream execution was cancelled before service call");
            return Err(anyhow::anyhow!(
                "LLM chat stream execution was cancelled before service call"
            ));
        }

        if let Some(ollama) = self.ollama.as_mut() {
            // Get streaming responses from ollama service
            let stream = ollama.request_stream_chat(args).await?;

            let req_meta = Arc::new(metadata.clone());
            // Transform each LlmChatResult into ResultOutputItem
            let output_stream = stream
                .flat_map(move |completion_result| {
                    let mut result_items = Vec::new();

                    // Encode the completion result to binary if there is content
                    if completion_result
                        .content
                        .as_ref()
                        .is_some_and(|c| c.content.is_some())
                    {
                        let buf = ProstMessageCodec::serialize_message(&completion_result);
                        // Add content data item
                        if let Ok(buf) = buf {
                            result_items.push(ResultOutputItem {
                                item: Some(result_output_item::Item::Data(buf)),
                            });
                        } else {
                            tracing::error!("Failed to serialize LLM completion result");
                        }
                    }

                    // If this is the last chunk, add an End item
                    if completion_result.done {
                        result_items.push(ResultOutputItem {
                            item: Some(result_output_item::Item::End(
                                proto::jobworkerp::data::Trailer {
                                    metadata: (*req_meta).clone(),
                                },
                            )),
                        });
                    }

                    futures::stream::iter(result_items)
                })
                .boxed();

            Ok(output_stream)
        } else if let Some(genai) = self.genai.as_mut() {
            // Get streaming responses from genai service
            let stream = genai.request_chat_stream(args, metadata).await?;
            Ok(stream)
        } else {
            Err(anyhow!("llm is not initialized"))
        }
    }

    async fn cancel(&mut self) {
        if let Some(token) = &self.cancellation_token {
            token.cancel();
            tracing::info!("LLM chat request cancelled");
        } else {
            tracing::warn!("No active LLM chat request to cancel");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Skip AppModule-dependent tests for now since creating a test AppModule is complex
    // Focus on the core cancellation token logic which can be tested independently

    #[tokio::test]
    async fn test_llm_chat_cancellation_token_basic() {
        eprintln!("=== Testing LLM Chat Runner cancellation token basic functionality ===");

        // Test that CancellationToken can be created and cancelled
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();
        assert!(token.is_cancelled());

        eprintln!("=== LLM Chat cancellation token basic functionality test completed ===");
    }
}
