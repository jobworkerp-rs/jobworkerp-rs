//! Text chunking for the embedding runner.
//!
//! Reuses `command_utils::text::chunking::HierarchicalChunker` via the
//! tokenizer-free `CharacterEstimation` fallback (Phase 1). Higher-fidelity
//! token estimation (tiktoken / HF tokenizer) is deferred to later phases and
//! would replace [`NoopTokenProvider`] with a real `TokenProvider`.

use anyhow::{Result, anyhow};
use command_utils::text::chunking::{
    FallbackStrategy, HierarchicalChunker, HierarchicalChunkingConfig, TokenProvider,
};

/// A single chunk of the source text.
///
/// `char_start`/`char_end` are half-open Unicode scalar (char) indices — NOT
/// byte offsets — into the original input text. `content` always equals
/// `text.chars().skip(char_start).take(char_end - char_start)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkResult {
    pub content: String,
    pub char_start: usize,
    pub char_end: usize,
}

/// Resolved (non-optional) chunking parameters. Built from the proto
/// `ChunkingConfig` after applying defaults (see runner layer).
///
/// Not `Copy`: `TokenEstimationStrategy` may carry an `Arc` to a loaded
/// tokenizer. `Clone` is cheap (an `Arc` refcount bump).
#[derive(Debug, Clone)]
pub struct ResolvedChunkingConfig {
    pub max_chunk_tokens: u32,
    pub min_chunk_tokens: u32,
    pub token_estimation: TokenEstimationStrategy,
}

/// Token-length estimation strategy.
///
/// `CharacterEstimation` needs no tokenizer (Phase 1). `HfTokenizer`/`Tiktoken`
/// carry a loaded provider so chunking can measure exact token counts (Phase
/// 3 / Phase 2). No `Copy`/`PartialEq` because they hold an `Arc<...tokenizer>`.
#[derive(Debug, Clone)]
pub enum TokenEstimationStrategy {
    /// ~4 chars/token, no tokenizer required (Phase 1).
    CharacterEstimation,
    /// HuggingFace tokenizer with exact token counts (Phase 3).
    HfTokenizer(std::sync::Arc<super::token_provider::hf::HfTokenProvider>),
    /// tiktoken OpenAI BPE with exact token counts (Phase 2).
    Tiktoken(std::sync::Arc<super::token_provider::tiktoken::TiktokenProvider>),
}

impl Default for ResolvedChunkingConfig {
    fn default() -> Self {
        // Safe defaults when settings/args leave chunking unspecified. 512
        // tokens is a conservative context budget shared by common embedding
        // models (e.g. nomic-embed-text, text-embedding-3).
        Self {
            max_chunk_tokens: 512,
            min_chunk_tokens: 0,
            token_estimation: TokenEstimationStrategy::CharacterEstimation,
        }
    }
}

/// Zero-sized `TokenProvider` used to satisfy `HierarchicalChunker<T>`'s type
/// parameter when running purely on the `CharacterEstimation` fallback (which
/// never actually invokes the provider). Every method errors so that any
/// accidental use surfaces loudly rather than returning bogus token data.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopTokenProvider;

/// Shared error for every `NoopTokenProvider` method: the provider is never
/// invoked under the `CharacterEstimation` fallback, so any call is a bug.
fn noop_provider_err<T>() -> std::result::Result<T, std::io::Error> {
    Err(std::io::Error::other(
        "NoopTokenProvider must not be called (CharacterEstimation fallback only)",
    ))
}

impl TokenProvider for NoopTokenProvider {
    type Error = std::io::Error;

    fn tokenize(&self, _text: &str) -> std::result::Result<Vec<u32>, Self::Error> {
        noop_provider_err()
    }

    fn estimate_token_count(&self, _text: &str) -> std::result::Result<usize, Self::Error> {
        noop_provider_err()
    }

    fn token_to_char(
        &self,
        _text: &str,
        _token_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        noop_provider_err()
    }

    fn char_to_token(
        &self,
        _text: &str,
        _char_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        noop_provider_err()
    }

    fn get_token_spans(
        &self,
        _text: &str,
    ) -> std::result::Result<Option<Vec<(usize, usize)>>, Self::Error> {
        noop_provider_err()
    }
}

/// Rough char-count → token-count estimate (~4 chars/token), matching the
/// chunker's `CharacterEstimation` heuristic, used only for the short-input
/// fast path below. Takes a precomputed char count to avoid re-walking the
/// string.
fn estimate_tokens(char_count: usize) -> usize {
    char_count.div_ceil(4)
}

/// A single chunk spanning the whole input (short-input fast path).
fn whole_text_chunk(text: &str, total_chars: usize) -> ChunkResult {
    ChunkResult {
        content: text.to_string(),
        char_start: 0,
        char_end: total_chars,
    }
}

/// Split `text` into chunks per `config`.
///
/// Returns a single whole-text chunk when the input is short enough. `content`
/// is always rebuilt from `char_start`/`char_end` char slicing so it exactly
/// matches the original substring, because the chunker's own `content` field
/// can diverge from the source under internal normalization (see
/// `mm-embedding-runner/src/chunking.rs` contract).
///
/// # Errors
/// Returns an error for empty/whitespace-only input or if the underlying
/// chunker fails to initialize or run.
pub fn chunk_text(text: &str, config: &ResolvedChunkingConfig) -> Result<Vec<ChunkResult>> {
    if text.trim().is_empty() {
        return Err(anyhow!("cannot chunk empty or whitespace-only text"));
    }

    // Materialize chars once: reused for the length estimate, the whole-text
    // fast path, and per-chunk content slicing below (avoids O(M*N) re-walks).
    let chars: Vec<char> = text.chars().collect();
    let total_chars = chars.len();
    let max = config.max_chunk_tokens as usize;

    let hc_config = HierarchicalChunkingConfig {
        max_chunk_tokens: max,
        min_chunk_tokens: config.min_chunk_tokens as usize,
        ..HierarchicalChunkingConfig::for_embedding(max)
    };

    // Dispatch on the estimation strategy. CharacterEstimation uses the
    // tokenizer-free fallback (Phase 1). HfTokenizer/Tiktoken measure exact
    // token counts via the loaded provider (Phase 3 / Phase 2); their fast-path
    // check must use the provider too, since the ~4 chars/token heuristic would
    // misjudge. Both tokenizer strategies share the same provider-driven path,
    // differing only in which unified provider they wrap.
    use super::token_provider::EmbeddingTokenProvider;
    let unified_provider = match &config.token_estimation {
        TokenEstimationStrategy::CharacterEstimation => None,
        TokenEstimationStrategy::HfTokenizer(p) => {
            Some(EmbeddingTokenProvider::Hf(std::sync::Arc::clone(p)))
        }
        TokenEstimationStrategy::Tiktoken(p) => {
            Some(EmbeddingTokenProvider::Tiktoken(std::sync::Arc::clone(p)))
        }
    };

    let raw_chunks = match unified_provider {
        None => {
            if estimate_tokens(total_chars) <= max {
                return Ok(vec![whole_text_chunk(text, total_chars)]);
            }
            let mut chunker = HierarchicalChunker::<NoopTokenProvider>::new_fallback(
                hc_config,
                FallbackStrategy::CharacterEstimation,
            )
            .map_err(|e| anyhow!("chunker init failed: {e}"))?;
            chunker
                .chunk_efficiently(text)
                .map_err(|e| anyhow!("chunking failed: {e}"))?
        }
        Some(provider) => {
            // Exact fast-path check using real token counts.
            let count = TokenProvider::estimate_token_count(&provider, text)
                .map_err(|e| anyhow!("token count failed: {e}"))?;
            if count <= max {
                return Ok(vec![whole_text_chunk(text, total_chars)]);
            }
            let mut chunker = HierarchicalChunker::new(
                hc_config,
                provider,
                Some(FallbackStrategy::CharacterEstimation),
            )
            .map_err(|e| anyhow!("chunker init failed: {e}"))?;
            chunker
                .chunk_efficiently(text)
                .map_err(|e| anyhow!("chunking failed: {e}"))?
        }
    };

    let mut out = Vec::with_capacity(raw_chunks.len());
    for c in raw_chunks {
        // Clamp offsets defensively, then rebuild content from the source text.
        let char_start = c.char_start.min(total_chars);
        let char_end = c.char_end.min(total_chars).max(char_start);
        let content: String = chars[char_start..char_end].iter().collect();
        // Skip empty ranges (can occur at boundaries after clamping).
        if content.is_empty() {
            continue;
        }
        out.push(ChunkResult {
            content,
            char_start,
            char_end,
        });
    }

    if out.is_empty() {
        return Err(anyhow!("chunking produced no chunks for non-empty input"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: u32) -> ResolvedChunkingConfig {
        ResolvedChunkingConfig {
            max_chunk_tokens: max,
            min_chunk_tokens: 0,
            token_estimation: TokenEstimationStrategy::CharacterEstimation,
        }
    }

    #[test]
    fn test_short_text_single_chunk() {
        let text = "Hello, world.";
        let chunks = chunk_text(text, &cfg(512)).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, text);
        assert_eq!(chunks[0].char_start, 0);
        assert_eq!(chunks[0].char_end, text.chars().count());
    }

    #[test]
    fn test_empty_input_errors() {
        assert!(chunk_text("", &cfg(512)).is_err());
        assert!(chunk_text("   \n\t ", &cfg(512)).is_err());
    }

    #[test]
    fn test_long_text_multiple_chunks_contiguous_no_gap() {
        // Force splitting: many sentences, tiny max_chunk_tokens.
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("This is sentence number {i}. "));
        }
        let chunks = chunk_text(&text, &cfg(8)).unwrap();
        assert!(chunks.len() > 1, "expected multiple chunks");

        // Each chunk's content must exactly match the char-sliced source.
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected, "content must match char slice");
            assert!(c.char_start < c.char_end, "range must be non-empty");
        }
    }

    #[test]
    fn test_japanese_multibyte_content_not_corrupted() {
        // Multibyte text: char offsets must not split code points.
        let mut text = String::new();
        for _ in 0..200 {
            text.push_str("これはテスト文章です。日本語の埋め込みを確認します。");
        }
        let chunks = chunk_text(&text, &cfg(8)).unwrap();
        assert!(chunks.len() > 1);

        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            // char_end within bounds and content is valid UTF-8 (String) built
            // from char slicing — no partial code points.
            assert!(c.char_end <= all_chars.len());
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected);
        }
    }

    #[test]
    fn test_content_contract_char_slice() {
        let text = "abc def ghi jkl mno pqr stu vwx yz. ".repeat(100);
        let chunks = chunk_text(&text, &cfg(6)).unwrap();
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            let sliced: String = all_chars
                .iter()
                .skip(c.char_start)
                .take(c.char_end - c.char_start)
                .collect();
            assert_eq!(c.content, sliced);
        }
    }

    // --- HF tokenizer (Phase 3) path -------------------------------------

    /// A whitespace-splitting tokenizer that maps every whitespace-delimited
    /// word to one token (unknowns → [UNK]); lets us drive the HF chunk_text
    /// path offline with predictable token counts (~1 token per word).
    fn hf_cfg(max: u32) -> ResolvedChunkingConfig {
        use std::io::Write;
        let json = serde_json::json!({
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": { "type": "Whitespace" },
            "post_processor": null,
            "decoder": null,
            "model": { "type": "WordLevel", "vocab": { "[UNK]": 0 }, "unk_token": "[UNK]" }
        })
        .to_string();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f.flush().unwrap();
        let provider =
            super::super::token_provider::hf::HfTokenProvider::from_file(f.path()).unwrap();
        ResolvedChunkingConfig {
            max_chunk_tokens: max,
            min_chunk_tokens: 0,
            token_estimation: TokenEstimationStrategy::HfTokenizer(std::sync::Arc::new(provider)),
        }
    }

    #[test]
    fn test_hf_short_text_single_chunk() {
        // "hello world" = 2 tokens; max 512 → single whole-text chunk.
        let chunks = chunk_text("hello world", &hf_cfg(512)).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "hello world");
    }

    #[test]
    fn test_hf_long_text_multiple_chunks_content_contract() {
        // ~200 words, tiny max → must split into multiple chunks whose content
        // matches the char slice (HF path must preserve the char-offset
        // contract just like CharacterEstimation).
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("word{i} "));
        }
        let chunks = chunk_text(&text, &hf_cfg(8)).unwrap();
        assert!(chunks.len() > 1, "expected multiple chunks");
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected, "content must match char slice");
            assert!(c.char_start < c.char_end);
        }
    }

    #[test]
    fn test_hf_japanese_not_corrupted() {
        let mut text = String::new();
        for _ in 0..100 {
            text.push_str("これは テスト です 日本語 埋め込み ");
        }
        let chunks = chunk_text(&text, &hf_cfg(8)).unwrap();
        assert!(chunks.len() > 1);
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            assert!(c.char_end <= all_chars.len());
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected);
        }
    }

    // --- tiktoken (Phase 2) path -----------------------------------------

    /// A config driving the tiktoken (cl100k_base) chunk_text path. tiktoken
    /// needs no fixture (encoding tables ship with the crate).
    fn tiktoken_cfg(max: u32) -> ResolvedChunkingConfig {
        let provider =
            super::super::token_provider::tiktoken::TiktokenProvider::new("cl100k_base").unwrap();
        ResolvedChunkingConfig {
            max_chunk_tokens: max,
            min_chunk_tokens: 0,
            token_estimation: TokenEstimationStrategy::Tiktoken(std::sync::Arc::new(provider)),
        }
    }

    #[test]
    fn test_tiktoken_short_text_single_chunk() {
        // "hello world" = 2 tokens under cl100k_base; max 512 → single chunk.
        let chunks = chunk_text("hello world", &tiktoken_cfg(512)).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].content, "hello world");
    }

    #[test]
    fn test_tiktoken_long_text_multiple_chunks_content_contract() {
        // tiktoken returns no char spans (get_token_spans == Ok(None)), so the
        // chunker falls back to string search for char boundaries. The content
        // contract (content == char slice) must still hold.
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("This is sentence number {i}. "));
        }
        let chunks = chunk_text(&text, &tiktoken_cfg(8)).unwrap();
        assert!(chunks.len() > 1, "expected multiple chunks");
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected, "content must match char slice");
            assert!(c.char_start < c.char_end);
        }
    }

    #[test]
    fn test_tiktoken_japanese_not_corrupted() {
        let mut text = String::new();
        for _ in 0..100 {
            text.push_str("これはテスト文章です。日本語の埋め込みを確認します。");
        }
        let chunks = chunk_text(&text, &tiktoken_cfg(8)).unwrap();
        assert!(chunks.len() > 1);
        let all_chars: Vec<char> = text.chars().collect();
        for c in &chunks {
            assert!(c.char_end <= all_chars.len());
            let expected: String = all_chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(c.content, expected);
        }
    }
}
