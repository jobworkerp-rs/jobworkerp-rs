use anyhow::{anyhow, Result};
use app::module::AppModule;
use async_stream::stream;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiChatService;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmChatArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
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
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMChatRunnerImpl {
    /// Constructor without cancellation monitoring (for backward compatibility)
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            app: app_module,
            ollama: None,
            genai: None,
            cancel_helper: None,
        }
    }

    /// Constructor with cancellation monitoring (DI integration version)
    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app: app_module,
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

impl Tracing for LLMChatRunnerImpl {}
impl LLMChatRunnerSpec for LLMChatRunnerImpl {}

// DI trait implementation (with optional support)
impl UseCancelMonitoringHelper for LLMChatRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}
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
                let ollama = OllamaChatService::new(
                    self.app.function_app.clone(),
                    self.app.function_set_app.clone(),
                    settings,
                )?;
                tracing::info!("{} loaded(ollama)", RunnerType::LlmChat.as_str_name());
                self.ollama = Some(ollama);
                Ok(())
            }
            Some(
                jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings::Genai(
                    settings,
                ),
            ) => {
                let genai = GenaiChatService::new(
                    self.app.function_app.clone(),
                    self.app.function_set_app.clone(),
                    settings,
                )
                .await?;
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
        let cancellation_token = self.get_cancellation_token().await;

        // Early cancellation check prevents wasted LLM service calls
        if cancellation_token.is_cancelled() {
            return (Err(anyhow!("LLM chat execution was cancelled")), metadata);
        }

        let span = Self::otel_span_from_metadata(&metadata, APP_WORKER_NAME, "llm_chat_run");
        let cx = Context::current_with_span(span);

        let metadata_clone = metadata.clone();
        let result = async {
            let mut args = LlmChatArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;

            // Handle potentially escaped JSON schema string from grpc-web
            if let Some(json_schema) = &args.json_schema {
                let schema_value = serde_json::to_value(json_schema)
                    .map_err(|e| anyhow!("Invalid json_schema format: {}", e))?;
                let processed_schema = match schema_value {
                    serde_json::Value::String(json_str) => {
                        // Try to parse as JSON string (in case it's escaped)
                        match serde_json::from_str::<serde_json::Value>(&json_str) {
                            Ok(_) => json_str,             // Valid JSON string, use as-is
                            Err(_) => json_schema.clone(), // Parse failed, use original
                        }
                    }
                    _ => json_schema.clone(), // Not a string, use original
                };
                args.json_schema = Some(processed_schema);
            }

            if let Some(ollama) = self.ollama.as_mut() {
                // Race between LLM completion and cancellation signal
                let res = tokio::select! {
                    result = ollama.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
                        return Err(anyhow!("LLM chat (Ollama) request was cancelled"));
                    }
                };

                let mut buf = Vec::with_capacity(res.encoded_len());
                res.encode(&mut buf)
                    .map_err(|e| anyhow!("encode error: {}", e))?;
                Ok(buf)
            } else if let Some(genai) = self.genai.as_mut() {
                // Race between LLM completion and cancellation signal
                let res = tokio::select! {
                    result = genai.request_chat(args, cx, metadata_clone.clone()) => result?,
                    _ = cancellation_token.cancelled() => {
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

        (result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        args: &[u8],
        metadata: HashMap<String, String>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        let mut args = LlmChatArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

        // Handle potentially escaped JSON schema string from grpc-web
        if let Some(json_schema) = &args.json_schema {
            let schema_value = serde_json::to_value(json_schema)
                .map_err(|e| anyhow!("Invalid json_schema format: {}", e))?;
            let processed_schema = match schema_value {
                serde_json::Value::String(json_str) => {
                    // Try to parse as JSON string (in case it's escaped)
                    match serde_json::from_str::<serde_json::Value>(&json_str) {
                        Ok(_) => json_str,             // Valid JSON string, use as-is
                        Err(_) => json_schema.clone(), // Parse failed, use original
                    }
                }
                _ => json_schema.clone(), // Not a string, use original
            };
            args.json_schema = Some(processed_schema);
        }

        if let Some(ollama) = self.ollama.as_mut() {
            let stream = ollama.request_stream_chat(args).await?;

            let req_meta = Arc::new(metadata.clone());
            let cancel_token = cancellation_token.clone();

            // Stream processing with mid-stream cancellation capability
            let output_stream = stream! {
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        item = stream.next() => {
                            match item {
                                Some(completion_result) => {
                                    if completion_result
                                        .content
                                        .as_ref()
                                        .is_some_and(|c| c.content.is_some())
                                    {
                                        let buf = ProstMessageCodec::serialize_message(&completion_result);
                                        if let Ok(buf) = buf {
                                            yield ResultOutputItem {
                                                item: Some(result_output_item::Item::Data(buf)),
                                            };
                                        } else {
                                            tracing::error!("Failed to serialize LLM completion result");
                                        }
                                    }

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

            Ok(output_stream)
        } else if let Some(genai) = self.genai.as_mut() {
            let stream = genai.request_chat_stream(args, metadata).await?;

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

            Ok(cancellable_stream)
        } else {
            Err(anyhow!("llm is not initialized"))
        }
    }
}

#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMChatRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for LLM Chat job {}",
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

    /// Signals cancellation token for LLMChatRunnerImpl
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("LLMChatRunnerImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("LLMChatRunnerImpl: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        // Quick completion requires immediate cleanup to prevent resource leaks
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }

        tracing::debug!("LLMChatRunnerImpl reset for pooling");
        Ok(())
    }
}
