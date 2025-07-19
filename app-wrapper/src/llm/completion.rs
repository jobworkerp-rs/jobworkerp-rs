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

use super::cancellation_helper::execute_cancellable;
use jobworkerp_runner::runner::cancellation::RunnerCancellationManager;
use tokio_util::sync::CancellationToken;

pub mod genai;
pub mod ollama;

pub struct LLMCompletionRunnerImpl {
    pub ollama: Option<OllamaService>,
    pub genai: Option<GenaiCompletionService>,
    cancellation_manager: Option<Arc<tokio::sync::Mutex<Box<dyn RunnerCancellationManager>>>>,
}

impl LLMCompletionRunnerImpl {
    pub fn new() -> Self {
        Self {
            ollama: None,
            genai: None,
            cancellation_manager: None,
        }
    }

    /// 統一されたtoken取得メソッド
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(manager) = &self.cancellation_manager {
            manager.lock().await.get_token().await
        } else {
            // fallback: basic token
            CancellationToken::new()
        }
    }

    pub fn set_cancellation_manager(
        &mut self,
        cancellation_manager: Box<dyn RunnerCancellationManager>,
    ) {
        self.cancellation_manager = Some(Arc::new(tokio::sync::Mutex::new(cancellation_manager)));
    }

    /// Test-only cancellation token setting method
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_cancellation_token(&mut self, token: tokio_util::sync::CancellationToken) {
        // For testing: Set token via Manager
        if let Some(_manager_arc) = &self.cancellation_manager {
            // Note: In real tests, Manager internal functionality should be used
            tracing::warn!("Test method: set_cancellation_token called, but Manager should handle token internally");
        } else {
            // Create dummy Manager for testing
            let mock_manager = jobworkerp_runner::runner::test_common::mock::MockCancellationManager::new_with_token(token);
            self.cancellation_manager =
                Some(Arc::new(tokio::sync::Mutex::new(Box::new(mock_manager))));
        }
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
        // 明確で簡潔なtoken取得
        let cancellation_token = self.get_cancellation_token().await;

        let metadata_clone = metadata.clone();

        let result = async {
            let span = Self::otel_span_from_metadata(
                &metadata_clone,
                APP_WORKER_NAME,
                "llm_completion_run",
            );
            let cx = Context::current_with_span(span);

            let args = LlmCompletionArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;
            if let Some(ollama) = self.ollama.as_mut() {
                // Use cancellable version
                let res = ollama
                    .request_generation_with_cancellation(
                        args,
                        cancellation_token.clone(),
                        cx,
                        metadata_clone.clone(),
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
                    "LLM completion (GenAI) request",
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

        // 結果処理も簡素化
        (result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        // Set up cancellation token using manager
        let cancellation_token = self.get_cancellation_token().await;

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
            // Note: token cleanup is handled by Manager
            Err(anyhow!("llm is not initialized"))
        }
    }

    async fn cancel(&mut self) {
        // Cancel using manager
        if let Some(manager) = &self.cancellation_manager {
            let manager = manager.lock().await;
            if manager.is_cancelled() {
                tracing::info!("LLMCompletionRunner execution is already cancelled");
            } else {
                tracing::info!("LLMCompletionRunner cancellation requested, but Manager handles token internally");
            }
        } else {
            tracing::warn!("No cancellation manager set, cannot cancel");
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

// CancelMonitoring implementation for LLMCompletionRunnerImpl
#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMCompletionRunnerImpl {
    /// Initialize cancellation monitoring for specific job
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        _job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        tracing::debug!(
            "Setting up cancellation monitoring for LLMCompletionRunnerImpl job {}",
            job_id.value
        );

        // For LLMCompletionRunnerImpl, we use the same pattern as CommandRunner
        // The actual cancellation monitoring will be handled by the CancellationHelper
        // LLM API requests will be cancelled automatically when the token is cancelled

        tracing::trace!("Cancellation monitoring started for job {}", job_id.value);
        Ok(None) // Continue with normal execution
    }

    /// Cleanup cancellation monitoring
    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        tracing::trace!("Cleaning up cancellation monitoring for LLMCompletionRunnerImpl");

        // Clear the cancellation helper
        // Note: token cleanup is handled by Manager

        Ok(())
    }
}

// CancelMonitoringCapable implementation for LLMCompletionRunnerImpl
impl jobworkerp_runner::runner::cancellation::CancelMonitoringCapable for LLMCompletionRunnerImpl {
    fn as_cancel_monitoring(
        &mut self,
    ) -> &mut dyn jobworkerp_runner::runner::cancellation::CancelMonitoring {
        self
    }
}

#[cfg(test)]
mod tests {
    // Tests for LLMCompletionRunnerImpl
}
