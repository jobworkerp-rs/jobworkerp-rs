//! Ollama embedding backend.
//!
//! Wraps `ollama_rs::Ollama::generate_embeddings`. Ollama returns
//! `Vec<Vec<f32>>` in input order (no per-item index, no usage), so embeddings
//! are tagged `index: None` and usage carries only the model name.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::OllamaRunnerSettings;
use ollama_rs::Ollama;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};

use super::{
    EmbeddingBackend, EmbeddingBackendResponse, EmbeddingError, ProviderEmbedding, ProviderUsage,
    ResolvedEmbeddingOptions, truncate_to_ollama_bool,
};

pub struct OllamaEmbeddingService {
    pub ollama: Arc<Ollama>,
    pub model: String,
}

impl OllamaEmbeddingService {
    const URL_BASE: &'static str = "http://localhost:11434";

    pub async fn new(settings: OllamaRunnerSettings) -> Result<Self> {
        let ollama = Ollama::try_new(settings.base_url.unwrap_or(Self::URL_BASE.to_string()))?;
        if settings.pull_model.unwrap_or(true) {
            ollama
                .pull_model(settings.model.clone(), false)
                .await
                .map_err(|e| anyhow!("failed to pull embedding model: {e}"))?;
        }
        Ok(Self {
            ollama: Arc::new(ollama),
            model: settings.model,
        })
    }

    /// Classify an Ollama error as too-long (retryable) vs other. Ollama surfaces
    /// context-length problems as text; match conservatively to avoid retrying
    /// unrelated failures.
    fn classify(err: ollama_rs::error::OllamaError) -> EmbeddingError {
        let msg = err.to_string().to_ascii_lowercase();
        if msg.contains("context")
            || msg.contains("too long")
            || msg.contains("exceed")
            || msg.contains("maximum")
        {
            EmbeddingError::TooLong(anyhow!("ollama: {err}"))
        } else {
            EmbeddingError::Other(anyhow!("ollama: {err}"))
        }
    }
}

#[async_trait]
impl EmbeddingBackend for OllamaEmbeddingService {
    async fn embed_batch(
        &self,
        model: &str,
        texts: Vec<String>,
        opts: &ResolvedEmbeddingOptions,
    ) -> std::result::Result<EmbeddingBackendResponse, EmbeddingError> {
        let input_count = texts.len();
        let mut req =
            GenerateEmbeddingsRequest::new(model.to_string(), EmbeddingsInput::Multiple(texts));
        if let Some(d) = opts.dimensions {
            req = req.dimensions(d);
        }
        if let Some(t) = truncate_to_ollama_bool(&opts.truncate) {
            req = req.truncate(t);
        }

        let resp = self
            .ollama
            .generate_embeddings(req)
            .await
            .map_err(Self::classify)?;

        // Ollama guarantees input order and no index; require exact count match.
        if resp.embeddings.len() != input_count {
            return Err(EmbeddingError::Other(anyhow!(
                "ollama returned {} embeddings for {} inputs",
                resp.embeddings.len(),
                input_count
            )));
        }

        let embeddings = resp
            .embeddings
            .into_iter()
            .map(|vector| ProviderEmbedding {
                vector,
                index: None,
            })
            .collect();

        Ok(EmbeddingBackendResponse {
            embeddings,
            // Ollama reports no token usage; carry only the model name.
            usage: Some(ProviderUsage {
                model: model.to_string(),
                prompt_tokens: None,
                total_tokens: None,
            }),
        })
    }
}
