use anyhow::{anyhow, Result};
use app::module::AppModule;
use async_stream::stream;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::{BoxStream, StreamExt};
use genai::GenaiCompletionService;
use jobworkerp_base::codec::{ProstMessageCodec, UseProstCodec};
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_runner::jobworkerp::runner::llm::{LlmCompletionArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::llm::LLMCompletionRunnerSpec;
use jobworkerp_runner::runner::{CollectStreamFuture, RunnerSpec, RunnerTrait};
use ollama::OllamaService;
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use prost::Message;
use proto::jobworkerp::data::{result_output_item, ResultOutputItem, RunnerType};
use serde_json;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod genai;
pub mod ollama;

pub struct LLMCompletionRunnerImpl {
    pub app: Arc<AppModule>,
    pub ollama: Option<OllamaService>,
    pub genai: Option<GenaiCompletionService>,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMCompletionRunnerImpl {
    pub fn new(app: Arc<AppModule>) -> Self {
        Self {
            app,
            ollama: None,
            genai: None,
            cancel_helper: None,
        }
    }

    pub fn new_with_cancel_monitoring(
        app: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app,
            ollama: None,
            genai: None,
            cancel_helper: Some(cancel_helper),
        }
    }

    async fn get_cancellation_token(&self) -> CancellationToken {
        if let Some(helper) = &self.cancel_helper {
            helper.get_cancellation_token().await
        } else {
            CancellationToken::new()
        }
    }
}

impl Tracing for LLMCompletionRunnerImpl {}

impl UseCancelMonitoringHelper for LLMCompletionRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
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

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMCompletionRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMCompletionRunnerSpec::settings_schema(self)
    }

    /// Collect streaming LLM completion results into a single LlmCompletionResult
    ///
    /// Strategy:
    /// - Concatenates text content from all chunks
    /// - Concatenates reasoning content from all chunks
    /// - Uses context and usage from the final chunk (done=true)
    fn collect_stream(&self, stream: BoxStream<'static, ResultOutputItem>) -> CollectStreamFuture {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::{
            message_content, GenerationContext, MessageContent, Usage,
        };
        use jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionResult;

        Box::pin(async move {
            let mut combined_text = String::new();
            let mut combined_reasoning = String::new();
            let mut final_context: Option<GenerationContext> = None;
            let mut final_usage: Option<Usage> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        if let Ok(chunk) = LlmCompletionResult::decode(data.as_slice()) {
                            // Concatenate text content
                            if let Some(content) = chunk.content {
                                if let Some(message_content::Content::Text(text)) = content.content
                                {
                                    combined_text.push_str(&text);
                                }
                            }

                            // Concatenate reasoning content
                            if let Some(reasoning) = chunk.reasoning_content {
                                combined_reasoning.push_str(&reasoning);
                            }

                            // Use final chunk's context and usage
                            if chunk.done {
                                final_context = chunk.context;
                                final_usage = chunk.usage;
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    Some(result_output_item::Item::FinalCollected(_)) | None => {}
                }
            }

            // Build collected result
            let result = LlmCompletionResult {
                content: if combined_text.is_empty() {
                    None
                } else {
                    Some(MessageContent {
                        content: Some(message_content::Content::Text(combined_text)),
                    })
                },
                reasoning_content: if combined_reasoning.is_empty() {
                    None
                } else {
                    Some(combined_reasoning)
                },
                done: true,
                context: final_context,
                usage: final_usage,
            };

            let bytes = result.encode_to_vec();
            Ok((bytes, metadata))
        })
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
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let cancellation_token = self.get_cancellation_token().await;

        // Early cancellation check prevents wasted LLM service calls
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

            let mut args = LlmCompletionArgs::decode(&mut Cursor::new(arg))
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
                // Race between LLM completion and cancellation signal
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
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        let cancellation_token = self.get_cancellation_token().await;

        let mut args =
            LlmCompletionArgs::decode(args).map_err(|e| anyhow!("decode error: {}", e))?;

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

        // Early cancellation check prevents wasted LLM service calls
        if cancellation_token.is_cancelled() {
            tracing::info!("LLM completion stream execution was cancelled before service call");
            return Err(anyhow!(
                "LLM completion stream execution was cancelled before service call"
            ));
        }

        if let Some(ollama) = self.ollama.as_mut() {
            let stream = ollama.request_stream_generation(args).await?;

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
            let stream = tokio::select! {
                result = genai.request_chat_stream(args, metadata) => result?,
                _ = cancellation_token.cancelled() => {
                    tracing::info!("LLM completion stream (GenAI) request was cancelled");
                    return Err(anyhow!("LLM completion stream (GenAI) request was cancelled"));
                }
            };

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
            Err(anyhow!("llm is not initialized"))
        }
    }
}

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

    /// Signals cancellation token for LLMCompletionRunnerImpl
    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        // Signal cancellation token
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("LLMCompletionRunnerImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("LLMCompletionRunnerImpl: no cancellation helper available");
        }

        // No additional resource cleanup needed
        Ok(())
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
    use super::*;
    use app::module::test::create_hybrid_test_app;
    use futures::stream::BoxStream;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::{
        generation_context, message_content, GenerationContext, MessageContent, OllamaContext,
        Usage,
    };
    use jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionResult;
    use proto::jobworkerp::data::result_output_item::Item;

    /// Helper function to create a mock stream from LlmCompletionResult chunks
    fn create_mock_llm_stream(
        chunks: Vec<LlmCompletionResult>,
        metadata: HashMap<String, String>,
    ) -> BoxStream<'static, ResultOutputItem> {
        let stream = async_stream::stream! {
            for chunk in chunks {
                yield ResultOutputItem {
                    item: Some(Item::Data(chunk.encode_to_vec())),
                };
            }
            yield ResultOutputItem {
                item: Some(Item::End(proto::jobworkerp::data::Trailer { metadata })),
            };
        };
        Box::pin(stream)
    }

    fn text_chunk(text: &str, done: bool) -> LlmCompletionResult {
        LlmCompletionResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(text.to_string())),
            }),
            reasoning_content: None,
            done,
            context: None,
            usage: None,
        }
    }

    #[test]
    fn test_collect_stream_single_chunk() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await.unwrap());
            let runner = LLMCompletionRunnerImpl::new(app);

            let chunk = LlmCompletionResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text("Hello, World!".to_string())),
                }),
                reasoning_content: Some("Thinking...".to_string()),
                done: true,
                context: Some(GenerationContext {
                    context: Some(generation_context::Context::Ollama(OllamaContext {
                        data: vec![1, 2, 3],
                    })),
                }),
                usage: Some(Usage {
                    model: "test-model".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    total_prompt_time_sec: None,
                    total_completion_time_sec: None,
                }),
            };

            let mut metadata = HashMap::new();
            metadata.insert("model".to_string(), "test-model".to_string());

            let stream = create_mock_llm_stream(vec![chunk], metadata.clone());
            let (result_bytes, result_metadata) = runner.collect_stream(stream).await.unwrap();

            let result =
                ProstMessageCodec::deserialize_message::<LlmCompletionResult>(&result_bytes)
                    .unwrap();

            // Check text content
            assert!(result.content.is_some());
            if let Some(content) = result.content {
                assert_eq!(
                    content.content,
                    Some(message_content::Content::Text("Hello, World!".to_string()))
                );
            }

            // Check reasoning
            assert_eq!(result.reasoning_content, Some("Thinking...".to_string()));

            // Check done flag
            assert!(result.done);

            // Check context and usage
            assert!(result.context.is_some());
            assert!(result.usage.is_some());
            if let Some(usage) = result.usage {
                assert_eq!(usage.prompt_tokens, Some(10));
                assert_eq!(usage.completion_tokens, Some(5));
            }

            assert_eq!(
                result_metadata.get("model"),
                Some(&"test-model".to_string())
            );
        })
    }

    #[test]
    fn test_collect_stream_multiple_chunks_concatenates_text() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await.unwrap());
            let runner = LLMCompletionRunnerImpl::new(app);

            let chunks = vec![
                text_chunk("Hello, ", false),
                text_chunk("World!", false),
                LlmCompletionResult {
                    content: Some(MessageContent {
                        content: Some(message_content::Content::Text(" Done.".to_string())),
                    }),
                    reasoning_content: None,
                    done: true,
                    context: Some(GenerationContext {
                        context: Some(generation_context::Context::Ollama(OllamaContext {
                            data: vec![4, 5, 6],
                        })),
                    }),
                    usage: Some(Usage {
                        model: "test-model".to_string(),
                        prompt_tokens: Some(20),
                        completion_tokens: Some(10),
                        total_prompt_time_sec: None,
                        total_completion_time_sec: None,
                    }),
                },
            ];

            let stream = create_mock_llm_stream(chunks, HashMap::new());
            let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

            let result =
                ProstMessageCodec::deserialize_message::<LlmCompletionResult>(&result_bytes)
                    .unwrap();

            // Text should be concatenated
            if let Some(content) = result.content {
                assert_eq!(
                    content.content,
                    Some(message_content::Content::Text(
                        "Hello, World! Done.".to_string()
                    ))
                );
            }

            // Should use final chunk's context and usage
            assert!(result.context.is_some());
            assert!(result.usage.is_some());
            if let Some(usage) = result.usage {
                assert_eq!(usage.prompt_tokens, Some(20));
                assert_eq!(usage.completion_tokens, Some(10));
            }
        })
    }

    #[test]
    fn test_collect_stream_concatenates_reasoning() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await.unwrap());
            let runner = LLMCompletionRunnerImpl::new(app);

            let chunks = vec![
                LlmCompletionResult {
                    content: Some(MessageContent {
                        content: Some(message_content::Content::Text("Answer: ".to_string())),
                    }),
                    reasoning_content: Some("Step 1: ".to_string()),
                    done: false,
                    context: None,
                    usage: None,
                },
                LlmCompletionResult {
                    content: Some(MessageContent {
                        content: Some(message_content::Content::Text("42".to_string())),
                    }),
                    reasoning_content: Some("Calculate result".to_string()),
                    done: true,
                    context: None,
                    usage: None,
                },
            ];

            let stream = create_mock_llm_stream(chunks, HashMap::new());
            let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

            let result =
                ProstMessageCodec::deserialize_message::<LlmCompletionResult>(&result_bytes)
                    .unwrap();

            // Reasoning should be concatenated
            assert_eq!(
                result.reasoning_content,
                Some("Step 1: Calculate result".to_string())
            );
        })
    }

    #[test]
    fn test_collect_stream_empty_chunks() {
        infra_utils::infra::test::TEST_RUNTIME.block_on(async {
            let app = Arc::new(create_hybrid_test_app().await.unwrap());
            let runner = LLMCompletionRunnerImpl::new(app);

            let chunks: Vec<LlmCompletionResult> = vec![];

            let stream = create_mock_llm_stream(chunks, HashMap::new());
            let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

            let result =
                ProstMessageCodec::deserialize_message::<LlmCompletionResult>(&result_bytes)
                    .unwrap();

            assert!(result.content.is_none());
            assert!(result.reasoning_content.is_none());
            assert!(result.done); // Always set to true after collection
        })
    }
}
