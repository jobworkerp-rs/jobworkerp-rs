use anyhow::{anyhow, Result};
use app::module::AppModule;
use async_stream::stream;
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

use super::cancellation_helper::{execute_cancellable, handle_run_result, CancellationHelper};

pub mod conversion;
pub mod genai;
pub mod ollama;

pub struct LLMChatRunnerImpl {
    pub app: Arc<AppModule>,
    pub ollama: Option<OllamaChatService>,
    pub genai: Option<GenaiChatService>,
    cancellation_helper: CancellationHelper,
}

impl LLMChatRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            app: app_module,
            ollama: None,
            genai: None,
            cancellation_helper: CancellationHelper::new(),
        }
    }

    /// Set a cancellation token for this runner instance
    /// This allows external control over cancellation behavior (for test)
    #[allow(dead_code)]
    pub fn set_cancellation_token(&mut self, token: tokio_util::sync::CancellationToken) {
        self.cancellation_helper.set_cancellation_token(token);
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
        // Set up cancellation token using helper
        let cancellation_token = match self.cancellation_helper.setup_execution_token() {
            Ok(token) => token,
            Err(e) => return (Err(e), metadata),
        };

        let span = Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_chat_run");
        let cx = Context::current_with_span(span);

        let metadata_clone = metadata.clone();
        let result = async {
            let args = LlmChatArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;

            if let Some(ollama) = self.ollama.as_mut() {
                let res = execute_cancellable(
                    ollama.request_chat(args, cx, metadata_clone.clone()),
                    &cancellation_token,
                    "LLM chat (Ollama) request",
                )
                .await?;

                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                let res = execute_cancellable(
                    genai.request_chat(args, cx, metadata_clone.clone()),
                    &cancellation_token,
                    "LLM chat (GenAI) request",
                )
                .await?;

                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else {
                Err(anyhow!("llm is not initialized"))
            }
        }
        .await;

        handle_run_result(&mut self.cancellation_helper, result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token using helper
        let cancellation_token = self.cancellation_helper.setup_execution_token()?;

        let args = LlmChatArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        if let Some(ollama) = self.ollama.as_mut() {
            // Get streaming responses from ollama service
            let stream = ollama.request_stream_chat(args).await?;

            let req_meta = Arc::new(metadata.clone());
            let cancel_token = cancellation_token.clone();

            // Transform each LlmChatResult into ResultOutputItem with cancellation support
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

                                    // If this is the last chunk, add an End item
                                    if completion_result.done {
                                        yield ResultOutputItem {
                                            item: Some(result_output_item::Item::End(
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
                            tracing::info!("LLM chat stream was cancelled");
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
            // Get streaming responses from genai service
            let stream = genai.request_chat_stream(args, metadata).await?;

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
                            tracing::info!("LLM chat GenAI stream was cancelled");
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
            self.cancellation_helper.clear_token();
            Err(anyhow!("llm is not initialized"))
        }
    }

    async fn cancel(&mut self) {
        self.cancellation_helper.cancel();
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// CancelMonitoring implementation for LLMChatRunnerImpl
#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMChatRunnerImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        _job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for LLMChatRunnerImpl job {}",
            job_id.value
        );

        // For LLMChatRunnerImpl, we use the same pattern as CommandRunner
        // The actual cancellation monitoring will be handled by the CancellationHelper
        // LLM API requests will be cancelled automatically when the token is cancelled

        tracing::trace!("Cancellation monitoring started for job {}", job_id.value);
        Ok(None) // Continue with normal execution
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for LLMChatRunnerImpl");

        // Clear the cancellation helper
        self.cancellation_helper.clear_token();

        Ok(())
    }
}

// CancelMonitoringCapable implementation for LLMChatRunnerImpl
impl jobworkerp_runner::runner::cancellation::CancelMonitoringCapable for LLMChatRunnerImpl {
    fn as_cancel_monitoring(
        &mut self,
    ) -> &mut dyn jobworkerp_runner::runner::cancellation::CancelMonitoring {
        self
    }
}
