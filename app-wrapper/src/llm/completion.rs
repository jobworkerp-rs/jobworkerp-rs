use anyhow::{anyhow, Result};
use async_stream::stream;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiCompletionService;
use infra_utils::infra::trace::Tracing;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmCompletionArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
// Note: execute_cancellable is no longer needed with new cancellation approach
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
    // Helper for dependency injection integration (optional for backward compatibility)
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMCompletionRunnerImpl {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new() -> Self {
        Self {
            ollama: None,
            genai: None,
            cancel_helper: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(cancel_helper: CancelMonitoringHelper) -> Self {
        Self {
            ollama: None,
            genai: None,
            cancel_helper: Some(cancel_helper),
        }
    }

    /// Unified cancellation token retrieval
    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }
}

impl Tracing for LLMCompletionRunnerImpl {}

// DI trait implementation (with optional support)
impl UseCancelMonitoringHelper for LLMCompletionRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }

    fn cancel_monitoring_helper_mut(&mut self) -> Option<&mut CancelMonitoringHelper> {
        self.cancel_helper.as_mut()
    }
}

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
        let cancellation_token = self.get_cancellation_token().await;

        // Check cancellation BEFORE any other processing
        if cancellation_token.is_cancelled() {
            return (
                Err(anyhow!("LLM completion execution was cancelled")),
                metadata,
            );
        }

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
                // Use tokio::select! for cancellation support
                let res = tokio::select! {
                    result = genai.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
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
        // Cancel using helper if available
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if token.is_cancelled() {
                tracing::info!("LLMCompletionRunner execution is already cancelled");
            } else {
                tracing::info!(
                    "LLMCompletionRunner cancellation requested, Helper handles token internally"
                );
            }
        } else {
            tracing::warn!("No cancellation helper set, cannot cancel");
        }
    }
}

// CancelMonitoring trait実装（Helper委譲版）
#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMCompletionRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        // Clear helper availability check to avoid optional complexity
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for LLM Completion job {}",
                job_id.value
            );
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // LLMCompletionRunner typically completes quickly, so always cleanup
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        // LLMCompletionRunner-specific state reset
        tracing::debug!("LLMCompletionRunnerImpl reset for pooling");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Tests for LLMCompletionRunnerImpl
}
