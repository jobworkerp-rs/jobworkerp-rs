use anyhow::{anyhow, Result};
use async_stream::stream;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiCompletionService;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmCompletionArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::llm::LLMCompletionRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use ollama::OllamaService;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, RunnerType};
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod genai;
pub mod ollama;

pub struct LLMCompletionRunnerImpl {
    pub ollama: Option<OllamaService>,
    pub genai: Option<GenaiCompletionService>,
    cancellation_token: Option<CancellationToken>,
}

impl LLMCompletionRunnerImpl {
    pub fn new() -> Self {
        Self {
            ollama: None,
            genai: None,
            cancellation_token: None,
        }
    }

    /// Set a cancellation token for this runner instance
    /// This allows external control over cancellation behavior (for test)
    pub fn set_cancellation_token(&mut self, token: CancellationToken) {
        self.cancellation_token = Some(token);
    }
}

impl Tracing for LLMCompletionRunnerImpl {}

impl Default for LLMCompletionRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl LLMCompletionRunnerSpec for LLMCompletionRunnerImpl {}
impl RunnerSpec for LLMCompletionRunnerImpl {
    fn name(&self) -> String {
        LLMCompletionRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMCompletionRunnerSpec::runner_settings_proto(self)
    }

    fn job_args_proto(&self) -> String {
        LLMCompletionRunnerSpec::job_args_proto(self)
    }

    fn result_output_proto(&self) -> Option<String> {
        LLMCompletionRunnerSpec::result_output_proto(self)
    }

    fn output_type(&self) -> proto::jobworkerp::data::StreamingOutputType {
        LLMCompletionRunnerSpec::output_type(self)
    }

    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }

    fn arguments_schema(&self) -> String {
        LLMCompletionRunnerSpec::arguments_schema(self)
    }

    fn output_schema(&self) -> Option<String> {
        LLMCompletionRunnerSpec::output_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for LLMCompletionRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .map_err(|e| anyhow!("decode error: {}", e))?;
        match settings.settings {
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Ollama(
                    settings,
                ),
            ) => {
                let ollama = OllamaService::new(settings).await?;
                tracing::info!("{} loaded(ollama)", RunnerType::LlmCompletion.as_str_name());
                self.ollama = Some(ollama);
                Ok(())
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                    settings,
                ),
            ) => {
                let genai = GenaiCompletionService::new(settings).await?;
                tracing::info!("{} loaded(genai)", RunnerType::LlmCompletion.as_str_name());
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
                        "LLM completion execution was cancelled before start"
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

        let metadata_clone = metadata.clone();

        let result = async {
            let span =
                Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_completion_run");
            let cx = Context::current_with_span(span);

            let args = LlmCompletionArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;
            if let Some(ollama) = self.ollama.as_mut() {
                // Use cancellable version
                let res = ollama
                    .request_generation_with_cancellation(
                        args,
                        cancellation_token,
                        cx,
                        metadata_clone,
                    )
                    .await?;
                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                // Add cancellation support for GenAI
                let res = tokio::select! {
                    result = genai.request_chat(args, cx, metadata_clone) => result?,
                    _ = cancellation_token.cancelled() => {
                        tracing::info!("LLM completion (GenAI) request was cancelled");
                        return Err(anyhow!("LLM completion (GenAI) request was cancelled"));
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
        (result, metadata)
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
                    "LLM completion stream execution was cancelled before start"
                ));
            }
            existing_token.clone()
        } else {
            let new_token = CancellationToken::new();
            self.cancellation_token = Some(new_token.clone());
            new_token
        };

        let args = LlmCompletionArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        // Check cancellation before LLM service execution
        if cancellation_token.is_cancelled() {
            tracing::info!("LLM completion stream execution was cancelled before service call");
            return Err(anyhow!(
                "LLM completion stream execution was cancelled before service call"
            ));
        }

        if let Some(ollama) = self.ollama.as_mut() {
            // Get streaming responses from ollama service
            let stream = ollama.request_stream_generation(args).await?;

            let req_meta = Arc::new(metadata.clone());
            let cancel_token = cancellation_token.clone();

            // Transform each LlmCompletionResult into ResultOutputItem with cancellation support
            let output_stream = stream! {
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(completion_result) => {
                                    // Encode the completion result to binary if there is content
                                    if completion_result
                                        .content
                                        .as_ref()
                                        .is_some_and(|c| c.content.is_some())
                                    {
                                        let buf = ProstMessageCodec::serialize_message(&completion_result);
                                        // Add content data item
                                        if let Ok(buf) = buf {
                                            yield ResultOutputItem {
                                                item: Some(result_output_item::Item::Data(buf)),
                                            };
                                        } else {
                                            tracing::error!("Failed to serialize LLM completion result");
                                        }
                                    }

                                    // If this is the final result (done=true), add an end marker
                                    if completion_result.done {
                                        yield ResultOutputItem {
                                            item: Some(proto::jobworkerp::data::result_output_item::Item::End(
                                                proto::jobworkerp::data::Trailer {
                                                    metadata: (*req_meta).clone(),
                                                },
                                            )),
                                        };
                                        break;
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("LLM completion stream was cancelled");
                            break;
                        }
                    }
                }
            }.boxed();

            // Keep cancellation token for mid-stream cancellation
            // The token will be used by cancel() method during stream processing
            // Note: cancellation_token is NOT reset here because stream is still active
            Ok(output_stream)
        } else if let Some(genai) = self.genai.as_mut() {
            // Get streaming responses from genai service with cancellation check
            let stream = tokio::select! {
                result = genai.request_chat_stream(args, metadata) => result?,
                _ = cancellation_token.cancelled() => {
                    tracing::info!("LLM completion stream (GenAI) request was cancelled");
                    return Err(anyhow!("LLM completion stream (GenAI) request was cancelled"));
                }
            };

            // Add cancellation support to GenAI stream
            let cancel_token = cancellation_token.clone();
            let cancellable_stream = stream! {
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(result_item) => yield result_item,
                                None => break,
                            }
                        }
                        _ = cancel_token.cancelled() => {
                            tracing::info!("LLM completion GenAI stream was cancelled");
                            break;
                        }
                    }
                }
            }
            .boxed();

            // Keep cancellation token for mid-stream cancellation
            // The token will be used by cancel() method during stream processing
            // Note: cancellation_token is NOT reset here because stream is still active
            Ok(cancellable_stream)
        } else {
            // Clear cancellation token even on error
            self.cancellation_token = None;
            Err(anyhow!("llm is not initialized"))
        }
    }

    async fn cancel(&mut self) {
        if let Some(token) = &self.cancellation_token {
            token.cancel();
            tracing::info!("LLM completion request cancelled");
        } else {
            tracing::warn!("No active LLM completion request to cancel");
        }
    }
}
