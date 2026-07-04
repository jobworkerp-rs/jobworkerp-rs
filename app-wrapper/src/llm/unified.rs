//! Unified LLM Runner implementation for app-wrapper
//!
//! This module provides a unified LLM runner that supports both 'completion' and 'chat' methods
//! via the `using` parameter.

use super::chat::LLMChatRunnerImpl;
use super::completion::LLMCompletionRunnerImpl;
use super::embedding::LLMEmbeddingRunnerImpl;
use anyhow::{Result, anyhow};
use app::module::AppModule;
use async_trait::async_trait;
use futures::stream::BoxStream;
use jobworkerp_runner::runner::cancellation::CancelMonitoring;
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::llm_unified::{
    LLMUnifiedRunnerSpecImpl, METHOD_CHAT, METHOD_COMPLETION, METHOD_EMBEDDING,
};
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use proto::jobworkerp::data::{JobData, JobId, JobResult, ResultOutputItem};
use std::collections::HashMap;
use std::sync::Arc;

use jobworkerp_runner::jobworkerp::runner::llm::LlmRunnerSettings;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::Settings;
use ollama_rs::Ollama;
use prost::Message;
use std::io::Cursor;

const OLLAMA_DEFAULT_URL: &str = "http://localhost:11434";

/// Pull the Ollama model once for the whole unified runner, then return
/// settings bytes with `pull_model=false` so the per-method runners initialize
/// their clients without re-pulling. Non-Ollama (GenAI) settings and
/// already-disabled pulls pass through unchanged.
async fn pull_ollama_model_once(settings: Vec<u8>) -> Result<Vec<u8>> {
    let mut decoded = LlmRunnerSettings::decode(&mut Cursor::new(&settings))
        .map_err(|e| anyhow!("decode error: {e}"))?;

    let Some(Settings::Ollama(ollama)) = decoded.settings.as_mut() else {
        // GenAI or unset: nothing to pull; leave bytes untouched.
        return Ok(settings);
    };
    // Respect an explicit opt-out; default (None) pulls, matching prior behavior.
    if ollama.pull_model == Some(false) {
        return Ok(settings);
    }

    let base_url = ollama
        .base_url
        .clone()
        .unwrap_or_else(|| OLLAMA_DEFAULT_URL.to_string());
    let client = Ollama::try_new(base_url)?;
    client
        .pull_model(ollama.model.clone(), false)
        .await
        .map_err(|e| anyhow!("failed to pull model '{}': {e}", ollama.model))?;
    tracing::info!("LLM(unified) pulled ollama model '{}' once", ollama.model);

    // Disable per-runner pulls now that the model is present server-side.
    ollama.pull_model = Some(false);
    Ok(decoded.encode_to_vec())
}

/// Unified LLM Runner implementation that delegates to completion or chat runners
pub struct LLMUnifiedRunnerImpl {
    completion_runner: LLMCompletionRunnerImpl,
    chat_runner: LLMChatRunnerImpl,
    embedding_runner: LLMEmbeddingRunnerImpl,
    spec: LLMUnifiedRunnerSpecImpl,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMUnifiedRunnerImpl {
    pub fn new(app_module: Arc<AppModule>) -> Self {
        Self {
            completion_runner: LLMCompletionRunnerImpl::new(app_module.clone()),
            chat_runner: LLMChatRunnerImpl::new(app_module.clone()),
            embedding_runner: LLMEmbeddingRunnerImpl::new(app_module),
            spec: LLMUnifiedRunnerSpecImpl::new(),
            cancel_helper: None,
        }
    }

    pub fn new_with_cancel_monitoring(
        app_module: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            completion_runner: LLMCompletionRunnerImpl::new_with_cancel_monitoring(
                app_module.clone(),
                cancel_helper.clone(),
            ),
            chat_runner: LLMChatRunnerImpl::new_with_cancel_monitoring(
                app_module.clone(),
                cancel_helper.clone(),
            ),
            embedding_runner: LLMEmbeddingRunnerImpl::new_with_cancel_monitoring(
                app_module,
                cancel_helper.clone(),
            ),
            spec: LLMUnifiedRunnerSpecImpl::new(),
            cancel_helper: Some(cancel_helper),
        }
    }
}

impl UseCancelMonitoringHelper for LLMUnifiedRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

impl RunnerSpec for LLMUnifiedRunnerImpl {
    fn name(&self) -> String {
        self.spec.name()
    }

    fn runner_settings_proto(&self) -> String {
        self.spec.runner_settings_proto()
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        self.spec.method_proto_map()
    }

    fn method_json_schema_map(&self) -> HashMap<String, proto::jobworkerp::data::MethodJsonSchema> {
        self.spec.method_json_schema_map()
    }

    fn settings_schema(&self) -> String {
        self.spec.settings_schema()
    }

    fn collect_stream(
        &self,
        stream: BoxStream<'static, ResultOutputItem>,
        using: Option<&str>,
    ) -> jobworkerp_runner::runner::CollectStreamFuture {
        self.spec.collect_stream(stream, using)
    }
}

#[async_trait]
impl RunnerTrait for LLMUnifiedRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        // The three method runners share one settings schema and one model.
        // Each Ollama-backed runner used to pull the model in its own `load`,
        // so loading the unified runner pulled the same model up to twice
        // (completion + embedding). Pull once here as the shared load-time
        // pre-download, then hand each runner settings with pull_model=false so
        // none of them re-pull while still initializing their clients.
        let settings = pull_ollama_model_once(settings).await?;

        self.completion_runner.load(settings.clone()).await?;
        self.chat_runner.load(settings.clone()).await?;
        self.embedding_runner.load(settings).await?;
        Ok(())
    }

    async fn run(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> (Result<Vec<u8>>, HashMap<String, String>) {
        match LLMUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_COMPLETION) => self.completion_runner.run(arg, metadata, None).await,
            Ok(METHOD_CHAT) => self.chat_runner.run(arg, metadata, None).await,
            Ok(METHOD_EMBEDDING) => self.embedding_runner.run(arg, metadata, None).await,
            Ok(_) => (
                Err(anyhow!("Internal error: unknown method after validation")),
                metadata,
            ),
            Err(e) => (Err(e), metadata),
        }
    }

    async fn run_stream(
        &mut self,
        arg: &[u8],
        metadata: HashMap<String, String>,
        using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        match LLMUnifiedRunnerSpecImpl::resolve_method(using) {
            Ok(METHOD_COMPLETION) => self.completion_runner.run_stream(arg, metadata, None).await,
            Ok(METHOD_CHAT) => self.chat_runner.run_stream(arg, metadata, None).await,
            // Embedding is non-streaming; delegate so the runner returns its
            // own unsupported error.
            Ok(METHOD_EMBEDDING) => self.embedding_runner.run_stream(arg, metadata, None).await,
            Ok(_) => Err(anyhow!("Internal error: unknown method after validation")),
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl CancelMonitoring for LLMUnifiedRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: JobId,
        job_data: &JobData,
    ) -> Result<Option<JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            Ok(None)
        }
    }

    async fn cleanup_cancellation_monitoring(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.cleanup_monitoring_impl().await
        } else {
            Ok(())
        }
    }

    async fn request_cancellation(&mut self) -> Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
            }
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jobworkerp_runner::runner::RunnerSpec;

    #[test]
    fn test_resolve_method() {
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("completion")).is_ok());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("chat")).is_ok());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("embedding")).is_ok());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(None).is_err());
        assert!(LLMUnifiedRunnerSpecImpl::resolve_method(Some("unknown")).is_err());
    }

    use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{
        GenaiRunnerSettings, OllamaRunnerSettings,
    };

    #[tokio::test]
    async fn test_pull_once_passthrough_for_genai() {
        // GenAI settings have no model to pull; bytes must pass through
        // unchanged (and no network access is attempted).
        let settings = LlmRunnerSettings {
            settings: Some(Settings::Genai(GenaiRunnerSettings {
                model: "gpt-4o-mini".to_string(),
                ..Default::default()
            })),
            embedding_chunking: None,
        }
        .encode_to_vec();
        let out = pull_ollama_model_once(settings.clone()).await.unwrap();
        assert_eq!(out, settings, "GenAI settings must be untouched");
    }

    #[tokio::test]
    async fn test_pull_once_passthrough_when_pull_disabled() {
        // pull_model=false must skip the pull entirely (no network) and leave
        // the bytes unchanged.
        let settings = LlmRunnerSettings {
            settings: Some(Settings::Ollama(OllamaRunnerSettings {
                model: "nomic-embed-text".to_string(),
                base_url: Some("http://127.0.0.1:1/".to_string()),
                system_prompt: None,
                pull_model: Some(false),
            })),
            embedding_chunking: None,
        }
        .encode_to_vec();
        let out = pull_ollama_model_once(settings.clone()).await.unwrap();
        assert_eq!(out, settings, "pull_model=false must be untouched");
    }

    #[test]
    fn test_runner_spec_name() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        assert_eq!(spec.name(), "LLM");
    }

    #[test]
    fn test_method_proto_map_has_three_methods() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        let methods = spec.method_proto_map();

        assert!(methods.contains_key("completion"));
        assert!(methods.contains_key("chat"));
        assert!(methods.contains_key("embedding"));
        assert_eq!(methods.len(), 3);

        // Verify schemas are not empty
        let completion = methods.get("completion").unwrap();
        assert!(!completion.args_proto.is_empty());
        assert!(!completion.result_proto.is_empty());

        let chat = methods.get("chat").unwrap();
        assert!(!chat.args_proto.is_empty());
        assert!(!chat.result_proto.is_empty());

        let embedding = methods.get("embedding").unwrap();
        assert!(!embedding.args_proto.is_empty());
        assert!(!embedding.result_proto.is_empty());
    }

    #[test]
    fn test_method_json_schema_map_has_three_methods() {
        let spec = LLMUnifiedRunnerSpecImpl::new();
        let schemas = spec.method_json_schema_map();

        assert!(schemas.contains_key("completion"));
        assert!(schemas.contains_key("chat"));
        assert!(schemas.contains_key("embedding"));
        assert_eq!(schemas.len(), 3);

        // Verify schemas are valid JSON (and embedding is not degraded to "{}")
        for (method_name, schema) in &schemas {
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&schema.args_schema);
            assert!(
                parsed.is_ok(),
                "Invalid JSON in args_schema for method '{}'",
                method_name
            );
        }
        assert_ne!(schemas.get("embedding").unwrap().args_schema, "{}");
    }
}
