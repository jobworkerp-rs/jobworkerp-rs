use anyhow::{Result, anyhow};
use command_utils::trace::impls::GenericOtelClient;
use futures::StreamExt;
use futures::stream::BoxStream;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{self, LlmCompletionArgs, LlmCompletionResult};
use ollama_rs::generation::completion;
use ollama_rs::generation::parameters::{FormatType, JsonStructure};
use ollama_rs::{
    Ollama,
    generation::completion::{GenerationResponse, request::GenerationRequest},
    models::ModelOptions,
};
use proto::jobworkerp::data::RunnerType;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::super::generic_tracing_helper::{
    ChatResponse, GenericLLMTracingHelper, LLMMessage, UsageData,
};
use crate::llm::ThinkTagHelper;

#[derive(Clone)]
pub struct OllamaService {
    pub ollama: Arc<Ollama>,
    pub model: String,
    pub system_prompt: Option<String>,
    pub otel_client: Option<Arc<GenericOtelClient>>,
}
// static DATA: OnceCell<Bytes> = OnceCell::new();

impl ThinkTagHelper for OllamaService {}

impl OllamaService {
    const URL_BASE: &'static str = "http://localhost:11434";
    pub async fn new(settings: OllamaRunnerSettings) -> Result<Self> {
        let ollama = Ollama::try_new(settings.base_url.unwrap_or(Self::URL_BASE.to_string()))?;
        if settings.pull_model.unwrap_or(true) {
            let pull = ollama
                .pull_model(settings.model.clone(), false)
                .await
                .map_err(|e| anyhow!("{}", e))?;
            tracing::debug!("model loaded: result = {:?}", pull);
        }
        Ok(Self {
            ollama: Arc::new(ollama),
            model: settings.model,
            system_prompt: settings.system_prompt,
            otel_client: Some(Arc::new(GenericOtelClient::new(
                "ollama.completion_service",
            ))),
        })
    }
    pub fn create_completion_options(args: &LlmCompletionArgs) -> ModelOptions {
        let mut options = ModelOptions::default();

        if let Some(opts) = args.options.as_ref() {
            if let Some(max_tokens) = opts.max_tokens {
                options = options.num_predict(max_tokens);
            } else {
                // default: fill context
                options = options.num_predict(-2);
            }
            if let Some(temperature) = opts.temperature {
                options = options.temperature(temperature);
            }
            if let Some(top_p) = opts.top_p {
                options = options.top_p(top_p);
            }
            if let Some(repeat_penalty) = opts.repeat_penalty {
                options = options.repeat_penalty(repeat_penalty);
            }
            if let Some(repeat_last_n) = opts.repeat_last_n {
                options = options.repeat_last_n(repeat_last_n);
            }
            if let Some(seed) = opts.seed {
                options = options.seed(seed);
            }
        }
        options
    }

    pub fn with_otel_client(mut self, client: Arc<GenericOtelClient>) -> Self {
        self.otel_client = Some(client);
        self
    }

    fn apply_json_schema_format<'a>(
        mut request: GenerationRequest<'a>,
        json_schema: Option<&str>,
    ) -> GenerationRequest<'a> {
        if let Some(schema_str) = json_schema {
            match serde_json::from_str(schema_str) {
                Ok(schema) => {
                    let format =
                        FormatType::StructuredJson(Box::new(JsonStructure::new_for_schema(schema)));
                    request = request.format(format);
                    tracing::debug!("Applied JSON schema format: {}", schema_str);
                }
                Err(e) => {
                    tracing::warn!("Invalid JSON schema, ignoring format: {}", e);
                }
            }
        }
        request
    }

    pub async fn request_stream_generation(
        &self,
        args: LlmCompletionArgs,
    ) -> Result<BoxStream<'static, LlmCompletionResult>> {
        let options = Self::create_completion_options(&args);
        let model = if let Some(model) = args.model.as_ref() {
            model.clone()
        } else {
            self.model.clone()
        };
        let mut request = GenerationRequest::new(model, args.prompt);
        request = request.options(options);
        if let Some(t) = args.options.as_ref().map(|o| o.extract_reasoning_content()) {
            request = request.think(t);
        }

        request = Self::apply_json_schema_format(request, args.json_schema.as_deref());

        if let Some(system_prompt) = self.system_prompt.clone() {
            request = request.system(system_prompt);
        }
        if let Some(llm::llm_completion_args::GenerationContext {
            context:
                Some(llm::llm_completion_args::generation_context::Context::OllamaContext(context)),
        }) = args.context
        {
            request = request.context(completion::GenerationContext(context.data));
        }

        let stream = self
            .ollama
            .generate_stream(request)
            .await
            .map_err(|e| anyhow!("Stream generation error: {}", e))?;

        let extract_reasoning = args
            .options
            .as_ref()
            .is_some_and(|o| o.extract_reasoning_content());
        let model = self.model.clone();

        let mut is_in_reasoning = false;
        let stream = stream.flat_map(move |result| {
            let mut stream_vec = Vec::new();
            match result {
                Ok(chunks) => {
                    for chunk in chunks {
                        let mut result = LlmCompletionResult {
                            content: None,
                            reasoning_content: None,
                            done: chunk.done,
                            context: chunk.context.map(|c| llm::llm_completion_result::GenerationContext {
                                context: Some(llm::llm_completion_result::generation_context::Context::Ollama(llm::llm_completion_result::OllamaContext {
                                    data: c.0,
                                })),
                            }),
                            usage: Some(llm::llm_completion_result::Usage {
                                model: model.clone(),
                                prompt_tokens: chunk.prompt_eval_count.map(|d| d as u32),
                                completion_tokens: chunk.eval_count.map(|d| d as u32),
                                total_prompt_time_sec: chunk
                                    .prompt_eval_duration
                                    .map(|d| (d as f64 / 1_000_000_000.0) as f32),
                                total_completion_time_sec: chunk
                                    .eval_duration
                                    .map(|d| (d as f64 / 1_000_000_000.0) as f32),
                            }),
                        };

                        if extract_reasoning {
                            if chunk.response == Self::START_THINK_TAG {
                                is_in_reasoning = true;
                                if !chunk.done{
                                    continue;
                                }
                            } else if chunk.response == Self::END_THINK_TAG {
                                is_in_reasoning = false;
                                if !chunk.done{
                                    continue;
                                }
                            }
                            if is_in_reasoning {
                                result.reasoning_content = Some(chunk.response);
                            } else {
                            result.content = Some(
                                llm::llm_completion_result::MessageContent {
                                    content: Some(message_content::Content::Text(chunk.response)),
                                }
                            );
                            }
                        } else {
                            result.content = Some(
                                llm::llm_completion_result::MessageContent {
                                    content: Some(message_content::Content::Text(chunk.response)),
                                },
                            );
                        }
                        stream_vec.push(result);

                        if chunk.done {
                            tracing::debug!(
                                "END OF stream generation {}: duration: {}",
                                RunnerType::LlmCompletion.as_str_name(),
                                chunk.total_duration.unwrap_or_default()
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error in stream generation: {}", e);
                }
            }
            futures::stream::iter(stream_vec)
        }).boxed();

        Ok(stream)
    }

    /// Cancellable version of request_generation
    pub async fn request_generation_with_cancellation(
        &self,
        args: LlmCompletionArgs,
        cancellation_token: CancellationToken,
        _cx: opentelemetry::Context,
        _metadata: HashMap<String, String>,
    ) -> Result<LlmCompletionResult> {
        let options = Self::create_completion_options(&args);
        let think = args.options.as_ref().map(|o| o.extract_reasoning_content());
        let mut request = GenerationRequest::new(self.model.clone(), args.prompt);
        request = request.options(options.clone());
        if let Some(t) = think {
            request = request.think(t);
        }

        request = Self::apply_json_schema_format(request, args.json_schema.as_deref());

        if let Some(system_prompt) = self.system_prompt.clone() {
            request = request.system(system_prompt);
        }
        if let Some(llm::llm_completion_args::GenerationContext {
            context:
                Some(llm::llm_completion_args::generation_context::Context::OllamaContext(context)),
        }) = args.context
        {
            request = request.context(completion::GenerationContext(context.data));
        }

        // Cancellable Ollama generation call
        let res = tokio::select! {
            generation_result = self.ollama.generate(request) => {
                generation_result.map_err(|e| anyhow!("Generation error(generation): {}", e))?
            }
            _ = cancellation_token.cancelled() => {
                return Err(JobWorkerError::CancelledError("Ollama generation was cancelled".to_string()).into());
            }
        };

        tracing::debug!(
            "END OF generation {}: duration: {}",
            RunnerType::LlmCompletion.as_str_name(),
            res.total_duration.unwrap_or_default()
        );
        let mut result = LlmCompletionResult {
            content: None,
            reasoning_content: None,
            done: true,
            context: res
                .context
                .map(|c| llm::llm_completion_result::GenerationContext {
                    context: Some(
                        llm::llm_completion_result::generation_context::Context::Ollama(
                            llm::llm_completion_result::OllamaContext { data: c.0 },
                        ),
                    ),
                }),
            usage: Some(llm::llm_completion_result::Usage {
                model: self.model.clone(),
                prompt_tokens: res.prompt_eval_count.map(|d| d as u32),
                completion_tokens: res.eval_count.map(|d| d as u32),
                total_prompt_time_sec: res
                    .prompt_eval_duration
                    .map(|d| (d as f64 / 1_000_000_000.0) as f32),
                total_completion_time_sec: res
                    .eval_duration
                    .map(|d| (d as f64 / 1_000_000_000.0) as f32),
            }),
        };
        // Use ollama-rs thinking field if available, otherwise fall back to tag parsing
        let (content_text, reasoning) = if let Some(thinking) = res.thinking.clone() {
            // ollama-rs provides thinking separately
            (res.response.clone(), Some(thinking))
        } else if args
            .options
            .as_ref()
            .is_some_and(|o| o.extract_reasoning_content())
        {
            // Fall back to manual tag parsing
            Self::divide_think_tag(res.response.clone())
        } else {
            (res.response.clone(), None)
        };

        result.content = Some(llm::llm_completion_result::MessageContent {
            content: Some(message_content::Content::Text(content_text)),
        });
        result.reasoning_content = reasoning;
        Ok(result)
    }
}

// Trait implementations for Ollama completion service
impl LLMMessage for GenerationRequest<'_> {
    fn get_role(&self) -> &str {
        "user" // Completion requests are typically user prompts
    }

    fn get_content(&self) -> &str {
        &self.prompt
    }
}

impl ChatResponse for GenerationResponse {
    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "content": self.response,
            "model": "", // GenerationResponse does not have model info
            "done": self.done,
            "total_duration": self.total_duration,
            "prompt_eval_count": self.prompt_eval_count,
            "eval_count": self.eval_count
        })
    }
}

impl UsageData for llm::llm_completion_result::Usage {
    fn to_usage_map(&self) -> HashMap<String, i64> {
        let mut usage = HashMap::new();
        usage.insert(
            "prompt_tokens".to_string(),
            self.prompt_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "completion_tokens".to_string(),
            self.completion_tokens.unwrap_or(0) as i64,
        );
        usage.insert(
            "total_tokens".to_string(),
            (self.prompt_tokens.unwrap_or(0) + self.completion_tokens.unwrap_or(0)) as i64,
        );
        usage
    }

    fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": self.prompt_tokens.unwrap_or(0),
            "completion_tokens": self.completion_tokens.unwrap_or(0),
            "total_tokens": self.prompt_tokens.unwrap_or(0) + self.completion_tokens.unwrap_or(0)
        })
    }
}

// Implement traits for OllamaService (completion version)
impl GenericLLMTracingHelper for OllamaService {
    fn get_otel_client(&self) -> Option<&Arc<GenericOtelClient>> {
        self.otel_client.as_ref()
    }

    fn convert_messages_to_input(&self, messages: &[impl LLMMessage]) -> serde_json::Value {
        // For completion, we treat the messages as simple text input
        let messages_text: Vec<String> = messages
            .iter()
            .map(|m| format!("{}: {}", m.get_role(), m.get_content()))
            .collect();
        serde_json::json!(messages_text)
    }

    fn get_provider_name(&self) -> &str {
        "ollama"
    }
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions;
    // use tracing::Level;
    #[ignore = "need to run with ollama server"]
    #[tokio::test]
    async fn test_run() {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);

        let settings = OllamaRunnerSettings {
            base_url: Some("http://ollama.ollama.svc.cluster.local:11434".to_string()),
            // base_url: Some("http://localhost:11434".to_string()),
            model: "phi4".to_string(),
            system_prompt: Some(
                "Please translate the following text to Japanese. Output only the translation result.".to_string(),
            ),
            pull_model: Some(false),
        };
        let plugin = OllamaService::new(settings).await.unwrap();

        let user_prompt = r#"
Translation Test
This is a test of the translation functionality in our Ollama integration.
The model should translate this English text into Japanese.
We want to verify that the translation process works correctly and the output is properly formatted.
The test checks that the response contains the expected content and meets our quality requirements.
        "#;
        let prompt = user_prompt.to_string();

        let request = LlmCompletionArgs {
            prompt,
            options: Some(LlmOptions {
                max_tokens: Some(2048),
                temperature: Some(0.4),
                top_p: Some(0.9),
                repeat_penalty: Some(0.9),
                repeat_last_n: Some(8),
                seed: Some(32),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cancellation_token = CancellationToken::new();
        let res = plugin
            .request_generation_with_cancellation(
                request,
                cancellation_token,
                opentelemetry::Context::current(),
                HashMap::new(),
            )
            .await
            .expect("failed to run plugin");
        println!("response: {:?}", res.content);
        assert!(
            res.content
                .is_some_and(|r| r.content.is_some_and(|res| match res {
                    message_content::Content::Text(text) => {
                        text.len() > 10 && text.len() < 4096
                    }
                }))
        );
        println!("Usage: {:?}", res.usage);
    }

    #[ignore = "need to run with local server"]
    #[tokio::test]
    async fn test_run_stream() {
        // command_utils::util::tracing::tracing_init_test(Level::DEBUG);

        let settings = OllamaRunnerSettings {
            base_url: Some("http://ollama.ollama.svc.cluster.local:11434".to_string()),
            model: "deepseek-r1:32b".to_string(),
            system_prompt: Some(
                "Please translate the following text to Japanese. Output only the translation result.".to_string(),
            ),
            pull_model: Some(false),
        };
        let plugin = OllamaService::new(settings).await.unwrap();

        let user_prompt = r#"
Streaming API Test
This is a test of the streaming API functionality in our Ollama integration.
The API should break this text into chunks and stream them back one by one.
We want to verify that all chunks are properly received and processed.
        "#;
        let prompt = user_prompt.to_string();

        let request = LlmCompletionArgs {
            prompt,
            options: Some(LlmOptions {
                max_tokens: Some(512),
                temperature: Some(0.4),
                top_p: Some(0.9),
                repeat_penalty: Some(0.9),
                repeat_last_n: Some(8),
                seed: Some(32),
                extract_reasoning_content: Some(true),
            }),
            ..Default::default()
        };

        // Request the streaming response
        let stream_result = plugin
            .request_stream_generation(request)
            .await
            .expect("failed to run streaming plugin");

        // Collect all chunks to verify the streaming functionality
        let responses = stream_result.collect::<Vec<_>>().await;

        // Make sure we got some responses
        assert!(!responses.is_empty(), "No streaming responses received");

        let has_content = responses.iter().any(|res| {
            res.content.as_ref().is_some_and(|c| {
                c.content.as_ref().is_some_and(|content| match content {
                    message_content::Content::Text(text) => !text.is_empty(),
                })
            })
        });
        assert!(has_content, "No text content found in streaming responses");

        let last_response = responses.last().expect("No responses received");
        assert!(last_response.done, "Final chunk doesn't have done=true");
        let mut reasoning_text = String::new();
        // Print out combined response for debugging
        let combined_text = responses
            .iter()
            .filter_map(|res| {
                res.reasoning_content.as_ref().inspect(|c| {
                    println!("Reasoning: {c}");
                    std::io::stdout().flush().unwrap();
                    reasoning_text.push_str(c);
                });
                res.content.as_ref().and_then(|c| {
                    c.content.as_ref().map(|content| match content {
                        message_content::Content::Text(text) => {
                            println!("Chunk: {text}");
                            if res.done {
                                println!("Usage: {:?}", res.usage);
                            }
                            std::io::stdout().flush().unwrap();
                            text.clone()
                        }
                    })
                })
            })
            .collect::<Vec<_>>()
            .join("");

        println!("Combined reasoning response: {reasoning_text}");
        println!("Combined streaming response: {combined_text}");
        assert!(combined_text.len() > 10, "Combined response too short");
    }

    #[cfg(test)]
    #[test]
    fn test_divide_think_tag() {
        let (prompt, think) = OllamaService::divide_think_tag(
            "aaa\n<think>\nbbb\nccc\n</think>\nddd\neee".to_string(),
        );
        assert_eq!(prompt, "aaa\n\nddd\neee");
        assert_eq!(think, Some("bbb\nccc".to_string()));

        let (prompt, think) =
            OllamaService::divide_think_tag("<think>\naaa\nbbb\nccc\nddd\neee".to_string());
        assert_eq!(prompt, "<think>\naaa\nbbb\nccc\nddd\neee");
        assert_eq!(think, None);
    }
}
