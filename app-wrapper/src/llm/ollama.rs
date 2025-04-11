use anyhow::{anyhow, Result};
use futures::stream::BoxStream;
use futures::StreamExt;
use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_result::message_content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::{self, LlmCompletionArgs, LlmCompletionResult};
use ollama_rs::generation::completion;
use ollama_rs::{
    generation::completion::{request::GenerationRequest, GenerationResponse},
    models::ModelOptions,
    Ollama,
};
use proto::jobworkerp::data::RunnerType;

pub struct OllamaService {
    pub ollama: Ollama,
    pub model: String,
    pub system_prompt: Option<String>,
}
// static DATA: OnceCell<Bytes> = OnceCell::new();

impl OllamaService {
    const URL_BASE: &'static str = "http://localhost:11434";
    const START_THINK_TAG: &'static str = "<think>";
    const END_THINK_TAG: &'static str = "</think>";

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
            ollama,
            model: settings.model,
            system_prompt: settings.system_prompt,
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

    pub async fn request_stream_generation(
        &mut self,
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

        // Convert the Ollama stream into our own BoxStream
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
                                context: Some(llm::llm_completion_result::generation_context::Context::OllamaContext(llm::llm_completion_result::OllamaContext {
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

    pub async fn request_generation(
        &mut self,
        args: LlmCompletionArgs,
    ) -> Result<LlmCompletionResult> {
        let options = Self::create_completion_options(&args);
        let mut request = GenerationRequest::new(self.model.clone(), args.prompt);
        request = request.options(options);
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
        let res: GenerationResponse = self
            .ollama
            .generate(request)
            .await
            .map_err(|e| anyhow!("Generation error(generation): {}", e))?;

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
                        llm::llm_completion_result::generation_context::Context::OllamaContext(
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
        if args
            .options
            .as_ref()
            .is_some_and(|o| o.extract_reasoning_content())
        {
            let (prompt, think) = Self::divide_think_tag(res.response);
            result.content = Some(llm::llm_completion_result::MessageContent {
                content: Some(message_content::Content::Text(prompt)),
            });
            result.reasoning_content = think;
        } else {
            result.content = Some(llm::llm_completion_result::MessageContent {
                content: Some(message_content::Content::Text(res.response)),
            });
        }
        Ok(result)
    }
    fn divide_think_tag(prompt: String) -> (String, Option<String>) {
        if let Some(think_start) = prompt.find(Self::START_THINK_TAG) {
            if let Some(think_end) = prompt.find(Self::END_THINK_TAG) {
                let think = Some(prompt[think_start + 7..think_end].trim().to_string());
                let new_prompt = prompt[..think_start].to_string() + &prompt[think_end + 8..];
                return (new_prompt.trim().to_string(), think);
            }
        }
        (prompt.trim().to_string(), None)
    }
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions;
    use tracing::Level;
    #[ignore = "need to run with local server"]
    #[tokio::test]
    async fn test_run() {
        command_utils::util::tracing::tracing_init_test(Level::DEBUG);

        let settings = OllamaRunnerSettings {
            base_url: Some("http://localhost:11434".to_string()),
            model: "phi4".to_string(),
            system_prompt: Some(
                "次の文章を日本語に翻訳してください。翻訳結果のみを出力してください".to_string(),
            ),
            pull_model: Some(false),
        };
        let mut plugin = OllamaService::new(settings).await.unwrap();

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
        let res = plugin
            .request_generation(request)
            .await
            .expect("failed to run plugin");
        println!("response: {:?}", res.content);
        assert!(res
            .content
            .is_some_and(|r| r.content.is_some_and(|res| match res {
                message_content::Content::Text(text) => {
                    text.len() > 10 && text.len() < 4096
                }
            })));
        println!("Usage: {:?}", res.usage);
    }

    #[ignore = "need to run with local server"]
    #[tokio::test]
    async fn test_run_stream() {
        command_utils::util::tracing::tracing_init_test(Level::DEBUG);

        let settings = OllamaRunnerSettings {
            base_url: Some("http://ollama.ollama.svc.cluster.local:11434".to_string()),
            model: "deepseek-r1:32b".to_string(),
            system_prompt: Some(
                "次の文章を日本語に翻訳してください。翻訳結果のみを出力してください".to_string(),
            ),
            pull_model: Some(false),
        };
        let mut plugin = OllamaService::new(settings).await.unwrap();

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

        // Check that at least one chunk contains text content
        let has_content = responses.iter().any(|res| {
            res.content.as_ref().is_some_and(|c| {
                c.content.as_ref().is_some_and(|content| match content {
                    message_content::Content::Text(text) => !text.is_empty(),
                })
            })
        });
        assert!(has_content, "No text content found in streaming responses");

        // Check that the last chunk has the done flag set to true
        let last_response = responses.last().expect("No responses received");
        assert!(last_response.done, "Final chunk doesn't have done=true");
        let mut reasoning_text = String::new();
        // Print out combined response for debugging
        let combined_text = responses
            .iter()
            .filter_map(|res| {
                res.reasoning_content.as_ref().inspect(|c| {
                    println!("Reasoning: {}", c);
                    std::io::stdout().flush().unwrap();
                    reasoning_text.push_str(c);
                });
                res.content.as_ref().and_then(|c| {
                    c.content.as_ref().map(|content| match content {
                        message_content::Content::Text(text) => {
                            println!("Chunk: {}", text);
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

        println!("Combined reasoning response: {}", reasoning_text);
        println!("Combined streaming response: {}", combined_text);
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
