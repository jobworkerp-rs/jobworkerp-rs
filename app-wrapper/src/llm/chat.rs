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
use jobworkerp_runner::runner::{CollectStreamFuture, RunnerSpec, RunnerTrait};
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

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMChatRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMChatRunnerSpec::settings_schema(self)
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
        _using: Option<&str>,
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
        _using: Option<&str>,
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

    /// Collect streaming LLM chat results into a single LlmChatResult
    ///
    /// Strategy:
    /// - Concatenates text content from all chunks
    /// - Collects all tool_calls (tool_calls takes precedence over text in final result)
    /// - Concatenates reasoning content from all chunks
    /// - Uses usage from the final chunk (done=true)
    fn collect_stream(&self, stream: BoxStream<'static, ResultOutputItem>) -> CollectStreamFuture {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::{
            message_content, MessageContent, Usage,
        };
        use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;

        Box::pin(async move {
            let mut combined_text = String::new();
            let mut combined_reasoning = String::new();
            let mut collected_tool_calls: Vec<message_content::ToolCall> = Vec::new();
            let mut final_usage: Option<Usage> = None;
            let mut metadata = HashMap::new();
            let mut stream = stream;

            while let Some(item) = stream.next().await {
                match item.item {
                    Some(result_output_item::Item::Data(data)) => {
                        if let Ok(chunk) = LlmChatResult::decode(data.as_slice()) {
                            // Handle content (text, image, or tool_calls)
                            if let Some(content) = chunk.content {
                                match content.content {
                                    Some(message_content::Content::Text(text)) => {
                                        combined_text.push_str(&text);
                                    }
                                    Some(message_content::Content::ToolCalls(tc)) => {
                                        collected_tool_calls.extend(tc.calls);
                                    }
                                    Some(message_content::Content::Image(_)) => {
                                        // TODO: Image content cannot be meaningfully merged
                                        tracing::error!("not implemented: image streaming response")
                                    }
                                    None => {
                                        tracing::error!("no response?")
                                    }
                                }
                            }

                            // Concatenate reasoning content
                            if let Some(reasoning) = chunk.reasoning_content {
                                combined_reasoning.push_str(&reasoning);
                            }

                            // Use final chunk's usage
                            if chunk.done {
                                final_usage = chunk.usage;
                            }
                        }
                    }
                    Some(result_output_item::Item::End(trailer)) => {
                        metadata = trailer.metadata;
                        break;
                    }
                    None => {}
                }
            }

            // Determine final content type: tool_calls takes precedence if present
            let final_content = if !collected_tool_calls.is_empty() {
                Some(MessageContent {
                    content: Some(message_content::Content::ToolCalls(
                        message_content::ToolCalls {
                            calls: collected_tool_calls,
                        },
                    )),
                })
            } else if !combined_text.is_empty() {
                Some(MessageContent {
                    content: Some(message_content::Content::Text(combined_text)),
                })
            } else {
                None
            };

            // Build collected result
            let result = LlmChatResult {
                content: final_content,
                reasoning_content: if combined_reasoning.is_empty() {
                    None
                } else {
                    Some(combined_reasoning)
                },
                done: true,
                usage: final_usage,
            };

            let bytes = result.encode_to_vec();
            Ok((bytes, metadata))
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use app::module::test::create_hybrid_test_app;
    use futures::stream::BoxStream;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_chat_result::{
        message_content, MessageContent, Usage,
    };
    use jobworkerp_runner::jobworkerp::runner::llm::LlmChatResult;
    use proto::jobworkerp::data::result_output_item::Item;

    /// Helper function to create a mock stream from LlmChatResult chunks
    fn create_mock_chat_stream(
        chunks: Vec<LlmChatResult>,
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

    fn text_chunk(text: &str, done: bool) -> LlmChatResult {
        LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::Text(text.to_string())),
            }),
            reasoning_content: None,
            done,
            usage: None,
        }
    }

    #[tokio::test]
    async fn test_collect_stream_single_text_chunk() {
        let app = Arc::new(create_hybrid_test_app().await.unwrap());
        let runner = LLMChatRunnerImpl::new(app);

        let chunk = LlmChatResult {
            content: Some(MessageContent {
                content: Some(message_content::Content::Text("Hello, World!".to_string())),
            }),
            reasoning_content: Some("Thinking...".to_string()),
            done: true,
            usage: Some(Usage {
                model: "test-model".to_string(),
                prompt_tokens: Some(10),
                completion_tokens: Some(5),
                total_prompt_time_sec: None,
                total_completion_time_sec: None,
            }),
        };

        let mut metadata = HashMap::new();
        metadata.insert("model".to_string(), "test-chat".to_string());

        let stream = create_mock_chat_stream(vec![chunk], metadata.clone());
        let (result_bytes, result_metadata) = runner.collect_stream(stream).await.unwrap();

        let result =
            ProstMessageCodec::deserialize_message::<LlmChatResult>(&result_bytes).unwrap();

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

        // Check done and usage
        assert!(result.done);
        assert!(result.usage.is_some());
        if let Some(usage) = result.usage {
            assert_eq!(usage.prompt_tokens, Some(10));
            assert_eq!(usage.completion_tokens, Some(5));
        }

        assert_eq!(result_metadata.get("model"), Some(&"test-chat".to_string()));
    }

    #[tokio::test]
    async fn test_collect_stream_multiple_text_chunks_concatenates() {
        let app = Arc::new(create_hybrid_test_app().await.unwrap());
        let runner = LLMChatRunnerImpl::new(app);

        let chunks = vec![
            text_chunk("Hello, ", false),
            text_chunk("World!", false),
            LlmChatResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text(" Done.".to_string())),
                }),
                reasoning_content: None,
                done: true,
                usage: Some(Usage {
                    model: "test-model".to_string(),
                    prompt_tokens: Some(20),
                    completion_tokens: Some(10),
                    total_prompt_time_sec: None,
                    total_completion_time_sec: None,
                }),
            },
        ];

        let stream = create_mock_chat_stream(chunks, HashMap::new());
        let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

        let result =
            ProstMessageCodec::deserialize_message::<LlmChatResult>(&result_bytes).unwrap();

        // Text should be concatenated
        if let Some(content) = result.content {
            assert_eq!(
                content.content,
                Some(message_content::Content::Text(
                    "Hello, World! Done.".to_string()
                ))
            );
        }

        // Should use final chunk's usage
        assert!(result.usage.is_some());
        if let Some(usage) = result.usage {
            assert_eq!(usage.prompt_tokens, Some(20));
            assert_eq!(usage.completion_tokens, Some(10));
        }
    }

    #[tokio::test]
    async fn test_collect_stream_tool_calls() {
        let app = Arc::new(create_hybrid_test_app().await.unwrap());
        let runner = LLMChatRunnerImpl::new(app);

        let chunks = vec![
            LlmChatResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::ToolCalls(
                        message_content::ToolCalls {
                            calls: vec![message_content::ToolCall {
                                call_id: "call_1".to_string(),
                                fn_name: "get_weather".to_string(),
                                fn_arguments: r#"{"city": "Tokyo"}"#.to_string(),
                            }],
                        },
                    )),
                }),
                reasoning_content: None,
                done: false,
                usage: None,
            },
            LlmChatResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::ToolCalls(
                        message_content::ToolCalls {
                            calls: vec![message_content::ToolCall {
                                call_id: "call_2".to_string(),
                                fn_name: "get_time".to_string(),
                                fn_arguments: r#"{"timezone": "JST"}"#.to_string(),
                            }],
                        },
                    )),
                }),
                reasoning_content: None,
                done: true,
                usage: None,
            },
        ];

        let stream = create_mock_chat_stream(chunks, HashMap::new());
        let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

        let result =
            ProstMessageCodec::deserialize_message::<LlmChatResult>(&result_bytes).unwrap();

        // Tool calls should be collected
        if let Some(content) = result.content {
            match content.content {
                Some(message_content::Content::ToolCalls(tc)) => {
                    assert_eq!(tc.calls.len(), 2);
                    assert_eq!(tc.calls[0].fn_name, "get_weather");
                    assert_eq!(tc.calls[1].fn_name, "get_time");
                }
                _ => panic!("Expected ToolCalls content"),
            }
        } else {
            panic!("Expected content");
        }
    }

    #[tokio::test]
    async fn test_collect_stream_concatenates_reasoning() {
        let app = Arc::new(create_hybrid_test_app().await.unwrap());
        let runner = LLMChatRunnerImpl::new(app);

        let chunks = vec![
            LlmChatResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text("Result: ".to_string())),
                }),
                reasoning_content: Some("First ".to_string()),
                done: false,
                usage: None,
            },
            LlmChatResult {
                content: Some(MessageContent {
                    content: Some(message_content::Content::Text("42".to_string())),
                }),
                reasoning_content: Some("Second".to_string()),
                done: true,
                usage: None,
            },
        ];

        let stream = create_mock_chat_stream(chunks, HashMap::new());
        let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

        let result =
            ProstMessageCodec::deserialize_message::<LlmChatResult>(&result_bytes).unwrap();

        // Reasoning should be concatenated
        assert_eq!(result.reasoning_content, Some("First Second".to_string()));
    }

    #[tokio::test]
    async fn test_collect_stream_empty_chunks() {
        let app = Arc::new(create_hybrid_test_app().await.unwrap());
        let runner = LLMChatRunnerImpl::new(app);

        let chunks: Vec<LlmChatResult> = vec![];

        let stream = create_mock_chat_stream(chunks, HashMap::new());
        let (result_bytes, _) = runner.collect_stream(stream).await.unwrap();

        let result =
            ProstMessageCodec::deserialize_message::<LlmChatResult>(&result_bytes).unwrap();

        assert!(result.content.is_none());
        assert!(result.reasoning_content.is_none());
        assert!(result.done); // Always set to true after collection
    }
}
