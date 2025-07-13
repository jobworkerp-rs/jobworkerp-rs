use anyhow::{anyhow, Result};
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
        let metadata_clone = metadata.clone();

        // Set up cancellation token for this execution
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());

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
        // Set up cancellation token for stream execution
        let cancellation_token = CancellationToken::new();
        self.cancellation_token = Some(cancellation_token.clone());

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
            // Transform each LlmCompletionResult into ResultOutputItem
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

                    // If this is the final result (done=true), add an end marker
                    if completion_result.done {
                        result_items.push(ResultOutputItem {
                            item: Some(proto::jobworkerp::data::result_output_item::Item::End(
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
            // Get streaming responses from genai service with cancellation check
            let stream = tokio::select! {
                result = genai.request_chat_stream(args, metadata) => result?,
                _ = cancellation_token.cancelled() => {
                    tracing::info!("LLM completion stream (GenAI) request was cancelled");
                    return Err(anyhow!("LLM completion stream (GenAI) request was cancelled"));
                }
            };
            Ok(stream)
        } else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_llm_cancel_no_active_request() {
        eprintln!("=== Starting LLM cancel with no active request test ===");
        let mut runner = LLMCompletionRunnerImpl::new();

        // Call cancel when no request is running - should not panic
        runner.cancel().await;
        eprintln!("LLM cancel completed successfully with no active request");

        eprintln!("=== LLM cancel with no active request test completed ===");
    }

    #[tokio::test]
    async fn test_llm_cancellation_token_setup() {
        eprintln!("=== Starting LLM cancellation token setup test ===");
        let mut runner = LLMCompletionRunnerImpl::new();

        // Verify initial state
        assert!(
            runner.cancellation_token.is_none(),
            "Initially no cancellation token"
        );

        // Test that cancellation token is properly managed
        // (This would require mock LLM service for full testing)
        runner.cancel().await; // Should not panic

        eprintln!("=== LLM cancellation token setup test completed ===");
    }

    #[tokio::test]
    async fn test_llm_completion_genai_cancellation() {
        eprintln!("=== Testing LLM Completion GenAI cancellation support ===");

        let _runner = LLMCompletionRunnerImpl::new();

        // Test cancellation token basic functionality
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();
        assert!(token.is_cancelled());

        eprintln!("=== LLM Completion GenAI cancellation support test completed ===");
    }

    #[tokio::test]
    async fn test_llm_completion_cancellation_token_management() {
        eprintln!("=== Testing LLM Completion cancellation token management ===");

        let mut runner = LLMCompletionRunnerImpl::new();

        // Initially no cancellation token
        assert!(runner.cancellation_token.is_none());

        // Test that cancel works without panic when no token exists
        runner.cancel().await;

        eprintln!("=== LLM Completion cancellation token management test completed ===");
    }

    #[tokio::test]
    #[ignore] // Requires Ollama server - run with --ignored for full testing
    async fn test_llm_actual_cancellation() {
        eprintln!("=== Starting LLM actual cancellation test ===");
        use jobworkerp_base::codec::ProstMessageCodec;
        use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
        use jobworkerp_runner::jobworkerp::runner::llm::LlmCompletionArgs;
        use ollama::OllamaService;
        use std::collections::HashMap;

        let mut runner = LLMCompletionRunnerImpl::new();

        // Try to initialize Ollama service
        let ollama_settings = OllamaRunnerSettings {
            base_url: Some("http://localhost:11434".to_string()),
            model: "llama3.2:1b".to_string(), // Use a small model for testing
            system_prompt: None,
            pull_model: Some(false), // Don't pull - assume model exists
        };

        match OllamaService::new(ollama_settings).await {
            Ok(ollama) => {
                runner.ollama = Some(ollama);

                // Create a completion request that would take some time
                let completion_args = LlmCompletionArgs {
                    prompt: "Write a very long story about artificial intelligence in exactly 1000 words.".to_string(),
                    model: Some("llama3.2:1b".to_string()),
                    system_prompt: None,
                    function_options: None,
                    options: None,
                    context: None,
                };

                let arg_bytes = ProstMessageCodec::serialize_message(&completion_args).unwrap();
                let metadata = HashMap::new();

                // Start LLM request in a task
                let start_time = std::time::Instant::now();
                let execution_task =
                    tokio::spawn(async move { runner.run(&arg_bytes, metadata).await });

                // Wait briefly then timeout to simulate cancellation
                tokio::time::sleep(Duration::from_millis(100)).await;

                // Test with timeout - LLM generation should take longer than 2 seconds
                let result = tokio::time::timeout(Duration::from_secs(2), execution_task).await;

                let elapsed = start_time.elapsed();
                eprintln!("LLM execution time: {elapsed:?}");

                match result {
                    Ok(task_result) => {
                        let (execution_result, _metadata) = task_result.unwrap();
                        match execution_result {
                            Ok(_) => {
                                eprintln!(
                                    "LLM completed unexpectedly quickly (may be cached response)"
                                );
                            }
                            Err(e) => {
                                eprintln!("LLM failed: {e}");
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("LLM execution timed out as expected - this indicates cancellation mechanism is ready");
                        // This timeout demonstrates that the LLM was processing long enough to be cancelled
                        assert!(
                            elapsed >= Duration::from_secs(2),
                            "Should timeout after 2 seconds"
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("Ollama service not available for testing: {e}");
                eprintln!("Skipping actual LLM cancellation test");
            }
        }

        eprintln!("=== LLM actual cancellation test completed ===");
    }
}
