//! Token providers for embedding chunking.
//!
//! Each backend-specific provider lives in its own submodule and implements
//! `command_utils::text::chunking::TokenProvider` so the hierarchical chunker
//! can measure and split text with real token counts:
//!
//! - [`offset`]: byte↔char span conversion shared by all byte-oriented
//!   tokenizers.
//! - [`hf`]: HuggingFace tokenizer (`tokenizers` + `hf-hub`), for Ollama models
//!   (Phase 3).
//! - `tiktoken`: OpenAI `cl100k_base`/`o200k_base` (Phase 2, added later).
//!
//! [`EmbeddingTokenProvider`] is the single unified provider the chunker is
//! parameterized over: `TokenProvider` has an associated `Error` type and so
//! cannot be used as `dyn`, so all concrete providers are dispatched through
//! this one enum that implements `TokenProvider` once.

pub mod hf;
pub mod offset;

/// Error type for all embedding token providers.
///
/// `command_utils::text::chunking::TokenProvider` requires its associated
/// `Error: std::error::Error + Send + Sync + 'static`, which `anyhow::Error`
/// does not satisfy (it has no blanket `std::error::Error` impl). This thin
/// wrapper carries an `anyhow::Error` while implementing `std::error::Error`.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct TokenProviderError(#[from] anyhow::Error);

impl TokenProviderError {
    pub fn msg(m: impl Into<String>) -> Self {
        Self(anyhow::anyhow!(m.into()))
    }
}

use std::sync::Arc;

use command_utils::text::chunking::TokenProvider;
use hf::HfTokenProvider;

/// Unified token provider the hierarchical chunker is parameterized over.
///
/// `TokenProvider` cannot be used as a trait object (associated `Error` type),
/// so every concrete provider is dispatched through this single enum, which
/// implements `TokenProvider` once. New tokenizers add a variant here plus a
/// match arm — the chunking call site stays generic over one type.
#[derive(Debug, Clone)]
pub enum EmbeddingTokenProvider {
    /// HuggingFace tokenizer (Ollama models, Phase 3).
    Hf(Arc<HfTokenProvider>),
    // Tiktoken(Arc<tiktoken::TiktokenProvider>) is added in Phase 2.
}

impl TokenProvider for EmbeddingTokenProvider {
    type Error = TokenProviderError;

    fn tokenize(&self, text: &str) -> std::result::Result<Vec<u32>, Self::Error> {
        match self {
            Self::Hf(p) => p.tokenize(text),
        }
    }

    fn estimate_token_count(&self, text: &str) -> std::result::Result<usize, Self::Error> {
        match self {
            Self::Hf(p) => p.estimate_token_count(text),
        }
    }

    fn token_to_char(
        &self,
        text: &str,
        token_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        match self {
            Self::Hf(p) => p.token_to_char(text, token_pos),
        }
    }

    fn char_to_token(
        &self,
        text: &str,
        char_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        match self {
            Self::Hf(p) => p.char_to_token(text, char_pos),
        }
    }

    fn get_token_spans(
        &self,
        text: &str,
    ) -> std::result::Result<Option<Vec<(usize, usize)>>, Self::Error> {
        match self {
            Self::Hf(p) => p.get_token_spans(text),
        }
    }
}
