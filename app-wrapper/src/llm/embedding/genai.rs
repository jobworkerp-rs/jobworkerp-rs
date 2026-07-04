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
        let endpoint_url = settings.base_url.clone();
        // The resolver must respect the service target genai already built from
        // the *request* model (embedding supports a per-job model override via
        // LlmEmbeddingArgs.model), and only override the endpoint with a custom
        // base URL. Re-resolving from settings.model here would silently ignore
        // the override and send requests to the wrong provider/model.
        let target_resolver = ServiceTargetResolver::from_resolver_async_fn(
            move |mut service_target: ServiceTarget| -> std::pin::Pin<
                Box<
                    dyn std::future::Future<Output = Result<ServiceTarget, genai::resolver::Error>>
                        + Send,
                >,
            > {
                let endpoint_url = endpoint_url.clone();
                Box::pin(async move {
                    if let Some(url) = endpoint_url
                        && !url.is_empty()
                    {
                        let normalized = crate::llm::common::normalize_genai_endpoint_url(&url)
                            .map_err(|e| {
                                genai::resolver::Error::Custom(format!(
                                    "Failed to parse endpoint URL={url} : {e:#?}"
                                ))
                            })?;
                        service_target.endpoint = Endpoint::from_owned(normalized);
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
        if let Some(fmt) = &opts.encoding_format {
            embed_opts = embed_opts.with_encoding_format(fmt);
        }
        if let Some(user) = &opts.user {
            embed_opts = embed_opts.with_user(user);
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
