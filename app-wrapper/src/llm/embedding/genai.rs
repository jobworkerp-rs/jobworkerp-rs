//! GenAI embedding backend.
//!
//! Wraps `genai::Client::embed_batch`. GenAI returns `Vec<Embedding{vector,
//! index}>` where `index` is the 0-based position into the request inputs
//! (adapter-dependent origin, but the "index into inputs" contract is common),
//! plus token usage. Embeddings are tagged with that `index` so the runner maps
//! by index rather than array position.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use genai::embed::EmbedOptions;
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::GenaiRunnerSettings;

use super::{
    EmbeddingBackend, EmbeddingBackendResponse, EmbeddingError, ProviderEmbedding, ProviderUsage,
    ResolvedEmbeddingOptions, truncate_for_genai,
};

pub struct GenaiEmbeddingService {
    pub client: Client,
    pub model: String,
}

impl GenaiEmbeddingService {
    pub async fn new(settings: GenaiRunnerSettings) -> Result<Self> {
        let model_name = settings.model.clone();
        let endpoint_url = settings.base_url.clone();
        // Reuse the completion service's endpoint-normalization approach so
        // custom base URLs resolve the same way for embedding.
        let target_resolver = ServiceTargetResolver::from_resolver_async_fn(
            move |_: ServiceTarget| -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<ServiceTarget, genai::resolver::Error>>
                        + Send,
                >,
            > {
                let model_name = model_name.clone();
                let endpoint_url = endpoint_url.clone();
                Box::pin(async move {
                    let client = Client::default();
                    let mut service_target = client
                        .resolve_service_target(&model_name)
                        .await
                        .map_err(|e| {
                            genai::resolver::Error::Custom(format!(
                                "Failed to resolve service target from model={model_name} : {e:#?}"
                            ))
                        })?;
                    if let Some(url) = endpoint_url
                        && !url.is_empty()
                    {
                        let mut u = url.parse::<url::Url>().map_err(|e| {
                            genai::resolver::Error::Custom(format!(
                                "Failed to parse endpoint URL={url} : {e:#?}"
                            ))
                        })?;
                        if u.path().is_empty() || u.path() == "/" {
                            u.set_path("/v1/");
                        } else if !u.path().ends_with('/') {
                            u.set_path(&format!("{}/", u.path()));
                        }
                        service_target.endpoint = Endpoint::from_owned(u.to_string());
                    }
                    Ok(service_target)
                })
            },
        );
        let client = Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();
        Ok(Self {
            client,
            model: settings.model,
        })
    }

    /// Classify a genai error as too-long (retryable) vs other, matching
    /// context-length messages conservatively.
    fn classify(err: genai::Error) -> EmbeddingError {
        let msg = err.to_string().to_ascii_lowercase();
        if msg.contains("maximum context")
            || msg.contains("context length")
            || msg.contains("too long")
            || msg.contains("too many tokens")
        {
            EmbeddingError::TooLong(anyhow!("genai: {err}"))
        } else {
            EmbeddingError::Other(anyhow!("genai: {err}"))
        }
    }
}

#[async_trait]
impl EmbeddingBackend for GenaiEmbeddingService {
    async fn embed_batch(
        &self,
        model: &str,
        texts: Vec<String>,
        opts: &ResolvedEmbeddingOptions,
    ) -> std::result::Result<EmbeddingBackendResponse, EmbeddingError> {
        let mut embed_opts = EmbedOptions::new().with_capture_usage(true);
        if let Some(d) = opts.dimensions {
            embed_opts = embed_opts.with_dimensions(d as usize);
        }
        if let Some(t) = &opts.embedding_type {
            embed_opts = embed_opts.with_embedding_type(t);
        }
        if let Some(tr) = truncate_for_genai(&opts.truncate) {
            embed_opts = embed_opts.with_truncate(tr);
        }

        let resp = self
            .client
            .embed_batch(model.to_string(), texts, Some(&embed_opts))
            .await
            .map_err(Self::classify)?;

        let embeddings = resp
            .embeddings
            .into_iter()
            .map(|e| ProviderEmbedding {
                vector: e.vector,
                // GenAI reports the input index explicitly; carry it so the
                // runner maps by index, not array position.
                index: Some(e.index),
            })
            .collect();

        // Convert genai usage (Option<i32>) to ProviderUsage (Option<u32>),
        // mirroring completion/genai.rs's `.map(|v| v as u32)` pattern.
        let usage = ProviderUsage {
            model: resp.model_iden.model_name.to_string(),
            prompt_tokens: resp.usage.prompt_tokens.map(|v| v as u32),
            total_tokens: resp.usage.total_tokens.map(|v| v as u32),
        };

        Ok(EmbeddingBackendResponse {
            embeddings,
            usage: Some(usage),
        })
    }
}
