//! Exact, provider-authoritative token counting for the unified LLM runner.
//!
//! This method is observational only. It does not enqueue generation work or
//! establish ordering, reservation, or correspondence with a later job.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_runner::jobworkerp::runner::llm::{
    LlmRunnerSettings, TokenCountArgs, TokenCountResult,
    llm_runner_settings::Settings,
    token_count_args::{ChatCountRequest, Target},
    token_count_result::{
        Error, Outcome, RenderedRequestSuccess, TextSuccess, TokenCountErrorCode,
    },
};
use jobworkerp_runner::runner::llm_token_count::{
    LLMTokenCountRunnerSpec, LLMTokenCountRunnerSpecImpl,
};
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use prost::Message;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;

mod input;
mod provider;

type TokenCountOutcomeResult<T> = std::result::Result<T, TokenCountResult>;

fn typed_error(code: TokenCountErrorCode, detail: &'static str) -> TokenCountResult {
    TokenCountResult {
        outcome: Some(Outcome::Error(Error {
            code: code as i32,
            detail: Some(detail.to_string()),
        })),
    }
}

fn invalid(detail: &'static str) -> TokenCountResult {
    typed_error(TokenCountErrorCode::InvalidTokenCountArgument, detail)
}

fn exact_unavailable() -> TokenCountResult {
    typed_error(
        TokenCountErrorCode::TokenCountExactUnavailable,
        "exact token count is not available for this execution protocol",
    )
}

fn capability_unavailable() -> TokenCountResult {
    typed_error(
        TokenCountErrorCode::ProviderCapabilityUnavailable,
        "provider token-count capability is unavailable for this request",
    )
}

#[derive(Clone)]
enum LoadedSettings {
    Ollama { model: String },
    Genai(jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings),
}

pub struct LLMTokenCountRunnerImpl {
    settings: Option<LoadedSettings>,
    client: reqwest::Client,
}

impl Default for LLMTokenCountRunnerImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl LLMTokenCountRunnerImpl {
    pub fn new() -> Self {
        Self {
            settings: None,
            client: reqwest::Client::new(),
        }
    }

    fn resolved_model_for_empty_texts(&self) -> Result<String> {
        match self
            .settings
            .as_ref()
            .ok_or_else(|| anyhow!("LLM token-count runner is not loaded"))?
        {
            LoadedSettings::Ollama { model } => Ok(model.clone()),
            LoadedSettings::Genai(settings) => Ok(settings.model.clone()),
        }
    }

    async fn resolve_target(&self) -> Result<TokenCountOutcomeResult<provider::ResolvedTarget>> {
        let settings = self
            .settings
            .as_ref()
            .ok_or_else(|| anyhow!("LLM token-count runner is not loaded"))?;
        let LoadedSettings::Genai(settings) = settings else {
            return Ok(Err(exact_unavailable()));
        };
        // Use the same fixed settings model passed to existing GenAI services.
        let target = crate::llm::common::resolve_fixed_genai_service_target(
            &settings.model,
            settings.base_url.as_deref(),
        )
        .await
        .context("failed to resolve GenAI service target")?;
        let custom_endpoint = settings
            .base_url
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        Ok(Ok(provider::resolved_target(
            target,
            settings.model.clone(),
            custom_endpoint,
        )?))
    }
    async fn send_count(
        &self,
        target: &provider::ResolvedTarget,
        body: Value,
        raw_tokenize: bool,
    ) -> Result<TokenCountOutcomeResult<(u64, Option<u64>)>> {
        let url = match provider::count_url(target, raw_tokenize)? {
            Ok(url) => url,
            Err(outcome) => return Ok(Err(outcome)),
        };
        provider::send_count(&self.client, target, url, body, raw_tokenize).await
    }

    async fn count_text(&self, texts: Vec<String>) -> Result<TokenCountResult> {
        if let Err(error) = input::validate_texts(&texts) {
            return Ok(error);
        }
        if texts.is_empty() {
            return Ok(TokenCountResult {
                outcome: Some(Outcome::TextSuccess(TextSuccess {
                    resolved_model: self.resolved_model_for_empty_texts()?,
                    tokenizer_id: None,
                    counts: Vec::new(),
                    total_tokens: 0,
                })),
            });
        }
        let target = match self.resolve_target().await? {
            Ok(target) => target,
            Err(outcome) => return Ok(outcome),
        };
        if target.protocol != provider::CountProtocol::OpenAiChat || !target.custom_endpoint {
            return Ok(exact_unavailable());
        }
        let mut counts = Vec::with_capacity(texts.len());
        let mut total_tokens = 0u64;
        for text in texts {
            let (count, _) = match self
                .send_count(&target, provider::text_body(&target, &text), true)
                .await?
            {
                Ok(value) => value,
                Err(outcome) => return Ok(outcome),
            };
            total_tokens = total_tokens
                .checked_add(count)
                .ok_or_else(|| anyhow!("token count sum overflow"))?;
            counts.push(count);
        }
        Ok(TokenCountResult {
            outcome: Some(Outcome::TextSuccess(TextSuccess {
                resolved_model: target.model,
                tokenizer_id: None,
                counts,
                total_tokens,
            })),
        })
    }
    async fn count_rendered(
        &self,
        messages: std::result::Result<Vec<Value>, TokenCountResult>,
        max_tokens: Option<i32>,
    ) -> Result<TokenCountResult> {
        if let Err(error) = input::validate_max_tokens(max_tokens) {
            return Ok(error);
        }
        let messages = match messages {
            Ok(messages) => messages,
            Err(error) => return Ok(error),
        };
        let target = match self.resolve_target().await? {
            Ok(target) => target,
            Err(outcome) => return Ok(outcome),
        };
        if target.protocol == provider::CountProtocol::Unsupported {
            return Ok(exact_unavailable());
        }
        let raw_tokenize = target.protocol == provider::CountProtocol::OpenAiChat;
        if raw_tokenize && !target.custom_endpoint {
            return Ok(exact_unavailable());
        }
        let body = match provider::request_body(&target, messages, raw_tokenize) {
            Ok(body) => body,
            Err(outcome) => return Ok(outcome),
        };
        let (rendered_input_tokens, context_window_tokens) =
            match self.send_count(&target, body, raw_tokenize).await? {
                Ok(value) => value,
                Err(outcome) => return Ok(outcome),
            };
        Ok(TokenCountResult {
            outcome: Some(Outcome::RenderedRequestSuccess(RenderedRequestSuccess {
                resolved_model: target.model,
                tokenizer_id: None,
                context_window_tokens,
                max_completion_tokens: max_tokens.map(|value| value as u64),
                rendered_input_tokens,
            })),
        })
    }
    async fn count(&self, args: TokenCountArgs) -> Result<TokenCountResult> {
        match args.target {
            Some(Target::Text(request)) => self.count_text(request.texts).await,
            Some(Target::Completion(args)) => {
                let max_tokens = args.options.as_ref().and_then(|options| options.max_tokens);
                if let Err(error) = input::validate_max_tokens(max_tokens) {
                    return Ok(error);
                }
                let settings = match self.settings.as_ref() {
                    Some(LoadedSettings::Genai(settings)) => settings,
                    Some(LoadedSettings::Ollama { .. }) => return Ok(exact_unavailable()),
                    None => return Err(anyhow!("LLM token-count runner is not loaded")),
                };
                if args
                    .json_schema
                    .as_deref()
                    .is_some_and(|schema| serde_json::from_str::<Value>(schema).is_ok())
                {
                    return Ok(capability_unavailable());
                }
                self.count_rendered(Ok(input::completion_messages(&args, settings)), max_tokens)
                    .await
            }
            Some(Target::Chat(ChatCountRequest {
                request: Some(args),
            })) => {
                let max_tokens = args.options.as_ref().and_then(|options| options.max_tokens);
                if let Err(error) = input::validate_max_tokens(max_tokens) {
                    return Ok(error);
                }
                let settings = match self.settings.as_ref() {
                    Some(LoadedSettings::Genai(settings)) => settings,
                    Some(LoadedSettings::Ollama { .. }) => return Ok(exact_unavailable()),
                    None => return Err(anyhow!("LLM token-count runner is not loaded")),
                };
                self.count_rendered(input::chat_messages(&args, settings), max_tokens)
                    .await
            }
            Some(Target::Chat(_)) => Ok(invalid("chat request is required")),
            None => Ok(invalid("token-count target is required")),
        }
    }
}

impl LLMTokenCountRunnerSpec for LLMTokenCountRunnerImpl {}
impl RunnerSpec for LLMTokenCountRunnerImpl {
    fn name(&self) -> String {
        RunnerSpec::name(&LLMTokenCountRunnerSpecImpl::new())
    }
    fn runner_settings_proto(&self) -> String {
        RunnerSpec::runner_settings_proto(&LLMTokenCountRunnerSpecImpl::new())
    }
    fn method_proto_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMTokenCountRunnerSpec::method_proto_map(self)
    }
    fn settings_schema(&self) -> String {
        RunnerSpec::settings_schema(&LLMTokenCountRunnerSpecImpl::new())
    }
}

#[async_trait]
impl RunnerTrait for LLMTokenCountRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .context("decode LLM runner settings")?;
        self.settings = match settings.settings {
            Some(Settings::Ollama(settings)) => Some(LoadedSettings::Ollama {
                model: settings.model,
            }),
            Some(Settings::Genai(settings)) => Some(LoadedSettings::Genai(settings)),
            None => None,
        };
        self.settings
            .as_ref()
            .ok_or_else(|| anyhow!("LLM runner settings are required"))?;
        Ok(())
    }
    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        let result = async {
            let args =
                TokenCountArgs::decode(&mut Cursor::new(arg)).context("decode token-count args")?;
            Ok(self.count(args).await?.encode_to_vec())
        }
        .await;
        (result, metadata)
    }
    async fn run_stream(
        &mut self,
        _arg: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, proto::jobworkerp::data::ResultOutputItem>> {
        Err(anyhow!(
            "streaming is not supported for the token_count method"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::jobworkerp::runner::llm::{
        LlmCompletionArgs, llm_runner_settings::OllamaRunnerSettings,
    };

    fn outcome(result: TokenCountResult) -> Outcome {
        result.outcome.expect("outcome")
    }

    #[tokio::test]
    async fn empty_texts_succeed_without_provider_capability() {
        let settings = LlmRunnerSettings {
            settings: Some(Settings::Ollama(OllamaRunnerSettings {
                model: "local-model".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut runner = LLMTokenCountRunnerImpl::new();
        runner.load(settings.encode_to_vec()).await.unwrap();
        let result = runner.count_text(Vec::new()).await.unwrap();
        assert!(
            matches!(outcome(result), Outcome::TextSuccess(TextSuccess { counts, total_tokens: 0, .. }) if counts.is_empty())
        );
    }
    #[tokio::test]
    async fn invalid_completion_options_precede_ollama_capability() {
        use jobworkerp_runner::jobworkerp::runner::llm::llm_completion_args::LlmOptions;
        let settings = LlmRunnerSettings {
            settings: Some(Settings::Ollama(OllamaRunnerSettings {
                model: "local-model".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut runner = LLMTokenCountRunnerImpl::new();
        runner.load(settings.encode_to_vec()).await.unwrap();
        let result = runner
            .count(TokenCountArgs {
                target: Some(Target::Completion(LlmCompletionArgs {
                    options: Some(LlmOptions {
                        max_tokens: Some(0),
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
            })
            .await
            .unwrap();
        assert!(
            matches!(outcome(result), Outcome::Error(Error { code, .. }) if code == TokenCountErrorCode::InvalidTokenCountArgument as i32)
        );
    }
}
