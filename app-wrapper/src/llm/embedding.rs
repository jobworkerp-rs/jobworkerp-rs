//! LLM embedding runner implementation layer.
//!
//! Backs the `embedding` method of `RunnerType::Llm`. Text inputs are chunked
//! (see [`chunking`]) and embedded via a provider [`EmbeddingBackend`]; non-text
//! inputs return an unsupported error until provider clients gain multimodal
//! support.
//!
//! # Provider abstraction
//! Unlike `completion.rs` (which holds concrete `Option<OllamaService>` /
//! `Option<GenaiCompletionService>` and dispatches with `if let Some(...)`),
//! embedding routes through the [`EmbeddingBackend`] trait so that retry-split,
//! response↔chunk mapping, and usage aggregation can be unit-tested with a mock
//! backend (see [`MockEmbeddingBackend`] in tests).

pub mod chunking;
pub mod core;
pub mod genai;
pub mod ollama;
pub mod token_provider;

use anyhow::{Result, anyhow};
use app::module::AppModule;
use async_trait::async_trait;
use command_utils::trace::Tracing;
use futures::stream::BoxStream;
use jobworkerp_base::APP_WORKER_NAME;
use jobworkerp_base::error::JobWorkerError;
use jobworkerp_runner::jobworkerp::runner::llm::llm_runner_settings::{ChunkingConfig, Settings};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmEmbeddingArgs, LlmRunnerSettings};
use jobworkerp_runner::runner::cancellation_helper::{
    CancelMonitoringHelper, UseCancelMonitoringHelper,
};
use jobworkerp_runner::runner::llm_embedding::LLMEmbeddingRunnerSpec;
use jobworkerp_runner::runner::{RunnerSpec, RunnerTrait};
use opentelemetry::Context;
use opentelemetry::trace::TraceContextExt;
use prost::Message;
use proto::jobworkerp::data::ResultOutputItem;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use chunking::{ResolvedChunkingConfig, TokenEstimationStrategy};
use core::{build_chunk_refs, build_result, embed_refs};
use genai::GenaiEmbeddingService;
use ollama::OllamaEmbeddingService;
use token_provider::hf::HfTokenProvider;
use token_provider::tiktoken::TiktokenProvider;
use tokio::sync::Mutex as AsyncMutex;

/// Which token estimation the caller asked for, with any tokenizer source, but
/// before the (possibly expensive, async) provider is loaded. Kept separate
/// from [`ResolvedChunkingConfig`] so the proto→numbers mapping stays pure and
/// synchronously testable; provider loading happens in [`LLMEmbeddingRunnerImpl::build_chunking`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum StrategyKind {
    Character,
    /// HuggingFace tokenizer; exactly one source must be provided.
    Hf {
        repo: Option<String>,
        file: Option<String>,
    },
    /// tiktoken OpenAI BPE; `encoding` is a tiktoken encoding name
    /// (e.g. "cl100k_base", "o200k_base").
    Tiktoken {
        encoding: String,
    },
}

/// Pure (provider-unresolved) chunking parameters derived from proto.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChunkingSpec {
    max_chunk_tokens: u32,
    min_chunk_tokens: u32,
    strategy: StrategyKind,
}

impl Default for ChunkingSpec {
    fn default() -> Self {
        let d = ResolvedChunkingConfig::default();
        Self {
            max_chunk_tokens: d.max_chunk_tokens,
            min_chunk_tokens: d.min_chunk_tokens,
            strategy: StrategyKind::Character,
        }
    }
}

/// Map a proto `ChunkingConfig` to a pure [`ChunkingSpec`] (no provider load).
///
/// Validates the numeric fields and the token-estimation selection: HF source
/// presence and the tiktoken encoding name are both checked here, so a bad
/// chunking config fails synchronously at resolution rather than later during
/// provider loading. The actual tokenizer load is deferred to
/// [`LLMEmbeddingRunnerImpl::build_chunking`].
fn resolve_chunking_spec(proto: Option<&ChunkingConfig>) -> Result<ChunkingSpec> {
    let default = ChunkingSpec::default();
    let Some(c) = proto else {
        return Ok(default);
    };
    // token_estimation: proto enum i32. 0=UNSPECIFIED (default), 1=CHARACTER,
    // 2=TIKTOKEN (Phase 2), 3=HF (Phase 3).
    let strategy = match c.token_estimation.unwrap_or(0) {
        0 | 1 => StrategyKind::Character,
        2 => {
            // tiktoken: resolve+validate the encoding name here (symmetric with
            // the HF source check below), so an unknown encoding fails at spec
            // resolution rather than later at provider construction. Unset/empty
            // resolves to the default (cl100k_base).
            let encoding =
                token_provider::tiktoken::resolve_encoding(c.tiktoken_encoding.as_deref())?
                    .to_string();
            StrategyKind::Tiktoken { encoding }
        }
        3 => {
            let repo = c.tokenizer_hf_repo.clone().filter(|s| !s.is_empty());
            let file = c.tokenizer_file_path.clone().filter(|s| !s.is_empty());
            if repo.is_none() && file.is_none() {
                return Err(anyhow!(
                    "HF_TOKENIZER requires tokenizer_hf_repo or tokenizer_file_path"
                ));
            }
            StrategyKind::Hf { repo, file }
        }
        other => return Err(anyhow!("unknown token_estimation value {other}")),
    };
    let max_chunk_tokens = c.max_chunk_tokens.unwrap_or(default.max_chunk_tokens);
    let min_chunk_tokens = c.min_chunk_tokens.unwrap_or(default.min_chunk_tokens);
    if max_chunk_tokens == 0 {
        return Err(anyhow!("max_chunk_tokens must be > 0"));
    }
    Ok(ChunkingSpec {
        max_chunk_tokens,
        min_chunk_tokens: min_chunk_tokens.min(max_chunk_tokens.saturating_sub(1)),
        strategy,
    })
}

/// Embedding runner. Holds one backend (selected at `load`) behind the
/// [`EmbeddingBackend`] trait so orchestration stays provider-agnostic.
pub struct LLMEmbeddingRunnerImpl {
    pub app: Arc<AppModule>,
    backend: Option<Box<dyn EmbeddingBackend + Send + Sync>>,
    /// Model name from settings; overridable per-job via args.model.
    default_model: Option<String>,
    /// settings.embedding_chunking resolved once at load.
    chunking_default: ResolvedChunkingConfig,
    /// Loaded HF tokenizers keyed by source ("repo:<id>" / "file:<path>") so a
    /// tokenizer is fetched/parsed at most once across jobs and per-job
    /// chunking overrides that reuse the same source.
    tokenizer_cache: Arc<AsyncMutex<HashMap<String, Arc<HfTokenProvider>>>>,
    /// Loaded tiktoken providers keyed by encoding name so a provider is
    /// constructed at most once per encoding across jobs.
    tiktoken_cache: Arc<AsyncMutex<HashMap<String, Arc<TiktokenProvider>>>>,
    cancel_helper: Option<CancelMonitoringHelper>,
}

impl LLMEmbeddingRunnerImpl {
    pub fn new(app: Arc<AppModule>) -> Self {
        Self {
            app,
            backend: None,
            default_model: None,
            chunking_default: ResolvedChunkingConfig::default(),
            tokenizer_cache: Arc::new(AsyncMutex::new(HashMap::new())),
            tiktoken_cache: Arc::new(AsyncMutex::new(HashMap::new())),
            cancel_helper: None,
        }
    }

    pub fn new_with_cancel_monitoring(
        app: Arc<AppModule>,
        cancel_helper: CancelMonitoringHelper,
    ) -> Self {
        Self {
            app,
            backend: None,
            default_model: None,
            chunking_default: ResolvedChunkingConfig::default(),
            tokenizer_cache: Arc::new(AsyncMutex::new(HashMap::new())),
            tiktoken_cache: Arc::new(AsyncMutex::new(HashMap::new())),
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

    /// Load (or fetch from cache) an HF tokenizer for `spec`, returning a fully
    /// resolved [`ResolvedChunkingConfig`]. `file` takes precedence over `repo`
    /// when both are set. The download/parse happens at most once per source.
    async fn build_chunking(&self, spec: &ChunkingSpec) -> Result<ResolvedChunkingConfig> {
        let token_estimation = match &spec.strategy {
            StrategyKind::Character => TokenEstimationStrategy::CharacterEstimation,
            StrategyKind::Hf { repo, file } => {
                let provider = self
                    .load_hf_provider(repo.as_deref(), file.as_deref())
                    .await?;
                TokenEstimationStrategy::HfTokenizer(provider)
            }
            StrategyKind::Tiktoken { encoding } => {
                let provider = self.load_tiktoken_provider(encoding).await?;
                TokenEstimationStrategy::Tiktoken(provider)
            }
        };
        Ok(ResolvedChunkingConfig {
            max_chunk_tokens: spec.max_chunk_tokens,
            min_chunk_tokens: spec.min_chunk_tokens,
            token_estimation,
        })
    }

    /// Load an HF tokenizer, caching by source. `file` wins over `repo`.
    async fn load_hf_provider(
        &self,
        repo: Option<&str>,
        file: Option<&str>,
    ) -> Result<Arc<HfTokenProvider>> {
        // Cache key encodes the source so file and repo never collide.
        let (key, is_file) = match (file, repo) {
            (Some(f), _) => (format!("file:{f}"), true),
            (None, Some(r)) => (format!("repo:{r}"), false),
            (None, None) => return Err(anyhow!("HF tokenizer source missing")),
        };

        let mut cache = self.tokenizer_cache.lock().await;
        if let Some(p) = cache.get(&key) {
            return Ok(p.clone());
        }
        let provider = if is_file {
            let path = file.unwrap();
            HfTokenProvider::from_file(std::path::Path::new(path))?
        } else {
            HfTokenProvider::from_hf_repo(repo.unwrap()).await?
        };
        let provider = Arc::new(provider);
        cache.insert(key, provider.clone());
        Ok(provider)
    }

    /// Load a tiktoken provider for `encoding`, caching by encoding name so the
    /// provider is constructed at most once per encoding.
    async fn load_tiktoken_provider(&self, encoding: &str) -> Result<Arc<TiktokenProvider>> {
        let mut cache = self.tiktoken_cache.lock().await;
        if let Some(p) = cache.get(encoding) {
            return Ok(p.clone());
        }
        let provider = Arc::new(TiktokenProvider::new(encoding)?);
        cache.insert(encoding.to_string(), provider.clone());
        Ok(provider)
    }

    /// Resolve the effective options (dimensions/truncate/embedding_type) from
    /// args, and the effective model (args.model > settings model). Async
    /// because per-job chunking overrides may load an HF tokenizer.
    async fn resolve_run_params(
        &self,
        args: &LlmEmbeddingArgs,
    ) -> Result<(String, ResolvedEmbeddingOptions, ResolvedChunkingConfig)> {
        let model = args
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .ok_or_else(|| anyhow!("no embedding model specified (args.model or settings)"))?;

        let opts = args
            .options
            .as_ref()
            .map(|o| ResolvedEmbeddingOptions {
                dimensions: o.dimensions,
                truncate: o.truncate.clone(),
                embedding_type: o.embedding_type.clone(),
            })
            .unwrap_or_default();

        // Chunking precedence: args.chunking > settings default.
        let chunking = match args.chunking.as_ref() {
            Some(c) => {
                let spec = resolve_chunking_spec(Some(c))?;
                self.build_chunking(&spec).await?
            }
            None => self.chunking_default.clone(),
        };

        Ok((model, opts, chunking))
    }
}

impl Tracing for LLMEmbeddingRunnerImpl {}

impl UseCancelMonitoringHelper for LLMEmbeddingRunnerImpl {
    fn cancel_monitoring_helper(&self) -> Option<&CancelMonitoringHelper> {
        self.cancel_helper.as_ref()
    }
}

impl LLMEmbeddingRunnerSpec for LLMEmbeddingRunnerImpl {}
impl RunnerSpec for LLMEmbeddingRunnerImpl {
    fn name(&self) -> String {
        LLMEmbeddingRunnerSpec::name(self)
    }

    fn runner_settings_proto(&self) -> String {
        LLMEmbeddingRunnerSpec::runner_settings_proto(self)
    }

    fn method_proto_map(
        &self,
    ) -> std::collections::HashMap<String, proto::jobworkerp::data::MethodSchema> {
        LLMEmbeddingRunnerSpec::method_proto_map(self)
    }

    fn settings_schema(&self) -> String {
        LLMEmbeddingRunnerSpec::settings_schema(self)
    }
}

#[async_trait]
impl RunnerTrait for LLMEmbeddingRunnerImpl {
    async fn load(&mut self, settings: Vec<u8>) -> Result<()> {
        let settings = LlmRunnerSettings::decode(&mut Cursor::new(settings))
            .map_err(|e| anyhow!("decode error: {}", e))?;

        // Resolve settings-level chunking default (used when args omit it),
        // loading the HF tokenizer once here rather than per job.
        let spec = resolve_chunking_spec(settings.embedding_chunking.as_ref())?;
        self.chunking_default = self.build_chunking(&spec).await?;

        match settings.settings {
            Some(Settings::Ollama(s)) => {
                self.default_model = Some(s.model.clone());
                let svc = OllamaEmbeddingService::new(s).await?;
                self.backend = Some(Box::new(svc));
                tracing::info!("LLM(embedding) loaded(ollama)");
                Ok(())
            }
            Some(Settings::Genai(s)) => {
                self.default_model = Some(s.model.clone());
                let svc = GenaiEmbeddingService::new(s).await?;
                self.backend = Some(Box::new(svc));
                tracing::info!("LLM(embedding) loaded(genai)");
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
        if cancellation_token.is_cancelled() {
            return (
                Err(JobWorkerError::CancelledError(
                    "LLM embedding execution was cancelled".to_string(),
                )
                .into()),
                metadata,
            );
        }

        let metadata_clone = metadata.clone();
        let result = async {
            let span = Self::otel_span_from_metadata(
                &metadata_clone,
                APP_WORKER_NAME,
                "llm_embedding_run",
            );
            let _cx = Context::current_with_span(span);

            let args = LlmEmbeddingArgs::decode(&mut Cursor::new(arg))
                .map_err(|e| anyhow!("decode error: {}", e))?;

            let backend = self
                .backend
                .as_ref()
                .ok_or_else(|| anyhow!("embedding backend is not initialized"))?;

            let (model, opts, chunking) = self.resolve_run_params(&args).await?;

            // Flatten inputs → chunk refs (absolute offsets).
            let refs = build_chunk_refs(&args, &chunking)?;

            // Embed with retry-split, racing cancellation.
            let (out_refs, vectors, usage) = tokio::select! {
                res = embed_refs(backend.as_ref(), &model, &opts, refs, &chunking) => res?,
                _ = cancellation_token.cancelled() => {
                    return Err(JobWorkerError::CancelledError(
                        "LLM embedding request was cancelled".to_string(),
                    ).into());
                }
            };

            let result = build_result(&out_refs, vectors, usage);
            let mut buf = Vec::with_capacity(result.encoded_len());
            result
                .encode(&mut buf)
                .map_err(|e| anyhow!("encode error: {}", e))?;
            Ok(buf)
        }
        .await;

        (result, metadata_clone)
    }

    async fn run_stream(
        &mut self,
        _args: &[u8],
        _metadata: HashMap<String, String>,
        _using: Option<&str>,
    ) -> Result<BoxStream<'static, ResultOutputItem>> {
        Err(anyhow!(
            "streaming is not supported for the embedding method"
        ))
    }
}

#[async_trait]
impl jobworkerp_runner::runner::cancellation::CancelMonitoring for LLMEmbeddingRunnerImpl {
    async fn setup_cancellation_monitoring(
        &mut self,
        job_id: proto::jobworkerp::data::JobId,
        job_data: &proto::jobworkerp::data::JobData,
    ) -> anyhow::Result<Option<proto::jobworkerp::data::JobResult>> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.setup_monitoring_impl(job_id, job_data).await
        } else {
            tracing::debug!(
                "No cancel monitoring configured for LLM Embedding job {}",
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

    async fn request_cancellation(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &self.cancel_helper {
            let token = helper.get_cancellation_token().await;
            if !token.is_cancelled() {
                token.cancel();
                tracing::info!("LLMEmbeddingRunnerImpl: cancellation token signaled");
            }
        } else {
            tracing::warn!("LLMEmbeddingRunnerImpl: no cancellation helper available");
        }
        Ok(())
    }

    async fn reset_for_pooling(&mut self) -> anyhow::Result<()> {
        if let Some(helper) = &mut self.cancel_helper {
            helper.reset_for_pooling_impl().await?;
        } else {
            self.cleanup_cancellation_monitoring().await?;
        }
        tracing::debug!("LLMEmbeddingRunnerImpl reset for pooling");
        Ok(())
    }
}

/// Provider-neutral embedding request options (resolved from proto
/// `EmbeddingOptions`). `truncate` stays as the provider-neutral string; each
/// backend maps it (see §4.4).
#[derive(Debug, Clone, Default)]
pub struct ResolvedEmbeddingOptions {
    pub dimensions: Option<u32>,
    /// Provider-neutral truncation: "NONE" / "START" / "END".
    pub truncate: Option<String>,
    pub embedding_type: Option<String>,
}

/// One embedding vector returned by a backend, tagged with the input index it
/// corresponds to when the backend reports one (GenAI), or `None` when the
/// backend guarantees input order (Ollama).
#[derive(Debug, Clone)]
pub struct ProviderEmbedding {
    pub vector: Vec<f32>,
    /// 0-based index into the batch input array this embedding derives from.
    /// `None` means "same position as input order" (Ollama contract).
    pub index: Option<usize>,
}

/// Token accounting reported by a backend. Ollama returns no token counts, so
/// only `model` is populated there; GenAI fills the token fields from
/// `EmbedResponse.usage`.
#[derive(Debug, Clone, Default)]
pub struct ProviderUsage {
    pub model: String,
    pub prompt_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

/// A backend's response for one batch: the embeddings plus any usage. Usage is
/// returned alongside the vectors (not dropped) so the runner can aggregate it
/// across retry batches.
#[derive(Debug, Clone)]
pub struct EmbeddingBackendResponse {
    pub embeddings: Vec<ProviderEmbedding>,
    pub usage: Option<ProviderUsage>,
}

/// A too-long / context-length error that the retry-split logic can act on.
/// Backends classify their provider-specific errors into this so the runner
/// stays provider-agnostic.
#[derive(Debug)]
pub enum EmbeddingError {
    /// The batch exceeded the model's context length; retry-split may help.
    TooLong(anyhow::Error),
    /// Any other error; propagated without retry.
    Other(anyhow::Error),
}

impl EmbeddingError {
    pub fn is_too_long(&self) -> bool {
        matches!(self, EmbeddingError::TooLong(_))
    }
}

impl std::fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmbeddingError::TooLong(e) => write!(f, "context length exceeded: {e}"),
            EmbeddingError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EmbeddingError {}

/// Swappable embedding provider. Held as `Box<dyn EmbeddingBackend + Send +
/// Sync>` by the runner (mirroring `Box<dyn CancellableRunner + Send + Sync>`).
///
/// `#[async_trait]` is used to match the established LLM-layer style
/// (chat.rs/completion.rs/unified.rs); no hand-written `BoxFuture`.
#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a batch of texts. Implementations must return one embedding per
    /// input (order-preserving for Ollama, `index`-tagged for GenAI). On a
    /// context-length error they must return [`EmbeddingError::TooLong`].
    async fn embed_batch(
        &self,
        model: &str,
        texts: Vec<String>,
        opts: &ResolvedEmbeddingOptions,
    ) -> std::result::Result<EmbeddingBackendResponse, EmbeddingError>;
}

/// Map the provider-neutral truncate string to Ollama's boolean `truncate`.
/// "NONE" => false, "START"/"END" => true (Ollama has no START/END
/// distinction; START logs a warning). Unknown values => None (provider
/// default) with a warning.
pub fn truncate_to_ollama_bool(truncate: &Option<String>) -> Option<bool> {
    match truncate.as_deref() {
        None => None,
        Some(s) => match s.to_ascii_uppercase().as_str() {
            "NONE" => Some(false),
            "END" => Some(true),
            "START" => {
                tracing::warn!(
                    "Ollama has no START/END truncation distinction; treating START as truncate=true"
                );
                Some(true)
            }
            other => {
                tracing::warn!("unknown truncate value '{other}'; using provider default");
                None
            }
        },
    }
}

/// Validate the provider-neutral truncate string for GenAI (which receives it
/// as-is). Returns the normalized value or `None` (with a warning) for unknown
/// values so a bad string is not forwarded to the provider.
pub fn truncate_for_genai(truncate: &Option<String>) -> Option<String> {
    match truncate.as_deref() {
        None => None,
        Some(s) => {
            let upper = s.to_ascii_uppercase();
            match upper.as_str() {
                "NONE" | "START" | "END" => Some(upper),
                other => {
                    tracing::warn!("unknown truncate value '{other}'; using provider default");
                    None
                }
            }
        }
    }
}

/// Aggregate usage across multiple backend responses (retry batches). Token
/// counts sum when present; the model name is taken from the first response
/// that carries one. Returns `None` if no response carried usage.
pub fn aggregate_usage(responses: &[Option<ProviderUsage>]) -> Option<ProviderUsage> {
    let mut model: Option<String> = None;
    let mut prompt: Option<u32> = None;
    let mut total: Option<u32> = None;
    let mut any = false;

    for u in responses.iter().flatten() {
        any = true;
        if model.is_none() && !u.model.is_empty() {
            model = Some(u.model.clone());
        }
        if let Some(p) = u.prompt_tokens {
            prompt = Some(prompt.unwrap_or(0) + p);
        }
        if let Some(t) = u.total_tokens {
            total = Some(total.unwrap_or(0) + t);
        }
    }

    if !any {
        return None;
    }
    Some(ProviderUsage {
        model: model.unwrap_or_default(),
        prompt_tokens: prompt,
        total_tokens: total,
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;
    use std::sync::Mutex;

    /// Programmable mock backend for TDD. Each call pops the next scripted
    /// outcome; records the texts it was asked to embed for assertions.
    pub struct MockEmbeddingBackend {
        pub calls: Arc<Mutex<Vec<Vec<String>>>>,
        outcomes: Mutex<std::collections::VecDeque<MockOutcome>>,
    }

    pub enum MockOutcome {
        Ok(EmbeddingBackendResponse),
        TooLong,
        Err(String),
    }

    impl MockEmbeddingBackend {
        pub fn new(outcomes: Vec<MockOutcome>) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                outcomes: Mutex::new(outcomes.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl EmbeddingBackend for MockEmbeddingBackend {
        async fn embed_batch(
            &self,
            _model: &str,
            texts: Vec<String>,
            _opts: &ResolvedEmbeddingOptions,
        ) -> std::result::Result<EmbeddingBackendResponse, EmbeddingError> {
            self.calls.lock().unwrap().push(texts.clone());
            let next = self.outcomes.lock().unwrap().pop_front();
            match next {
                Some(MockOutcome::Ok(resp)) => Ok(resp),
                Some(MockOutcome::TooLong) => {
                    Err(EmbeddingError::TooLong(anyhow::anyhow!("mock too long")))
                }
                Some(MockOutcome::Err(msg)) => Err(EmbeddingError::Other(anyhow::anyhow!(msg))),
                None => {
                    // Default: order-preserving unit vectors, no usage.
                    let embeddings = texts
                        .iter()
                        .map(|_| ProviderEmbedding {
                            vector: vec![0.0_f32],
                            index: None,
                        })
                        .collect();
                    Ok(EmbeddingBackendResponse {
                        embeddings,
                        usage: None,
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_chunking_spec_defaults_and_precedence() {
        // None → defaults (Character strategy).
        let d = resolve_chunking_spec(None).unwrap();
        assert_eq!(d.max_chunk_tokens, 512);
        assert_eq!(d.strategy, StrategyKind::Character);

        // Explicit values applied; min clamped below max.
        let c = ChunkingConfig {
            max_chunk_tokens: Some(100),
            min_chunk_tokens: Some(200),
            token_estimation: Some(1),
            ..Default::default()
        };
        let r = resolve_chunking_spec(Some(&c)).unwrap();
        assert_eq!(r.max_chunk_tokens, 100);
        assert!(r.min_chunk_tokens < r.max_chunk_tokens);
    }

    #[test]
    fn test_resolve_chunking_spec_tiktoken_defaults_and_explicit_encoding() {
        // Phase 2: TIKTOKEN with no encoding → defaults to cl100k_base.
        let tiktoken = ChunkingConfig {
            token_estimation: Some(2),
            ..Default::default()
        };
        let spec = resolve_chunking_spec(Some(&tiktoken)).unwrap();
        assert_eq!(
            spec.strategy,
            StrategyKind::Tiktoken {
                encoding: "cl100k_base".to_string()
            }
        );

        // Explicit encoding is carried through (no provider load here).
        let o200k = ChunkingConfig {
            token_estimation: Some(2),
            tiktoken_encoding: Some("o200k_base".to_string()),
            ..Default::default()
        };
        let spec = resolve_chunking_spec(Some(&o200k)).unwrap();
        assert_eq!(
            spec.strategy,
            StrategyKind::Tiktoken {
                encoding: "o200k_base".to_string()
            }
        );

        // Empty string is treated as unset → default encoding.
        let empty = ChunkingConfig {
            token_estimation: Some(2),
            tiktoken_encoding: Some(String::new()),
            ..Default::default()
        };
        let spec = resolve_chunking_spec(Some(&empty)).unwrap();
        assert_eq!(
            spec.strategy,
            StrategyKind::Tiktoken {
                encoding: "cl100k_base".to_string()
            }
        );

        // Unknown encoding fails at spec resolution (synchronous, no provider
        // load) — symmetric with the HF source-presence check.
        let bad = ChunkingConfig {
            token_estimation: Some(2),
            tiktoken_encoding: Some("not_a_real_encoding".to_string()),
            ..Default::default()
        };
        assert!(resolve_chunking_spec(Some(&bad)).is_err());
    }

    #[test]
    fn test_resolve_chunking_spec_hf_requires_source() {
        // HF with no repo/file → explicit error.
        let hf_no_src = ChunkingConfig {
            token_estimation: Some(3),
            ..Default::default()
        };
        assert!(resolve_chunking_spec(Some(&hf_no_src)).is_err());

        // HF with a repo → Ok spec carrying the source (no load yet).
        let hf = ChunkingConfig {
            token_estimation: Some(3),
            tokenizer_hf_repo: Some("nomic-ai/nomic-embed-text-v1.5".to_string()),
            ..Default::default()
        };
        let spec = resolve_chunking_spec(Some(&hf)).unwrap();
        assert_eq!(
            spec.strategy,
            StrategyKind::Hf {
                repo: Some("nomic-ai/nomic-embed-text-v1.5".to_string()),
                file: None
            }
        );
    }

    #[test]
    fn test_resolve_chunking_spec_zero_max_errors() {
        let c = ChunkingConfig {
            max_chunk_tokens: Some(0),
            token_estimation: Some(1),
            ..Default::default()
        };
        assert!(resolve_chunking_spec(Some(&c)).is_err());
    }

    #[test]
    fn test_truncate_to_ollama_bool() {
        assert_eq!(truncate_to_ollama_bool(&None), None);
        assert_eq!(
            truncate_to_ollama_bool(&Some("NONE".to_string())),
            Some(false)
        );
        assert_eq!(
            truncate_to_ollama_bool(&Some("END".to_string())),
            Some(true)
        );
        assert_eq!(
            truncate_to_ollama_bool(&Some("start".to_string())),
            Some(true)
        );
        assert_eq!(truncate_to_ollama_bool(&Some("bogus".to_string())), None);
    }

    #[test]
    fn test_truncate_for_genai() {
        assert_eq!(truncate_for_genai(&None), None);
        assert_eq!(
            truncate_for_genai(&Some("none".to_string())),
            Some("NONE".to_string())
        );
        assert_eq!(
            truncate_for_genai(&Some("START".to_string())),
            Some("START".to_string())
        );
        assert_eq!(truncate_for_genai(&Some("weird".to_string())), None);
    }

    #[test]
    fn test_aggregate_usage_sums_tokens_across_batches() {
        let a = Some(ProviderUsage {
            model: "m1".to_string(),
            prompt_tokens: Some(10),
            total_tokens: Some(12),
        });
        let b = Some(ProviderUsage {
            model: "m1".to_string(),
            prompt_tokens: Some(5),
            total_tokens: Some(6),
        });
        let agg = aggregate_usage(&[a, b]).unwrap();
        assert_eq!(agg.model, "m1");
        assert_eq!(agg.prompt_tokens, Some(15));
        assert_eq!(agg.total_tokens, Some(18));
    }

    #[test]
    fn test_aggregate_usage_none_when_all_missing() {
        assert!(aggregate_usage(&[None, None]).is_none());
    }

    #[test]
    fn test_aggregate_usage_model_only_no_tokens() {
        // Ollama-style: model name present, tokens None.
        let a = Some(ProviderUsage {
            model: "nomic".to_string(),
            prompt_tokens: None,
            total_tokens: None,
        });
        let agg = aggregate_usage(&[a, None]).unwrap();
        assert_eq!(agg.model, "nomic");
        assert_eq!(agg.prompt_tokens, None);
        assert_eq!(agg.total_tokens, None);
    }
}
