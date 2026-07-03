//! HuggingFace tokenizer provider (Phase 3).
//!
//! Wraps a `tokenizers::Tokenizer` and exposes it as a
//! `command_utils::text::chunking::TokenProvider` so the hierarchical chunker
//! can split embedding inputs by real token counts for Ollama models. The
//! tokenizer's byte-oriented spans are converted to char spans via
//! [`super::offset::CharByteMap`].

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use command_utils::text::chunking::TokenProvider;
use tokenizers::Tokenizer;

use super::TokenProviderError;
use super::offset::CharByteMap;

/// One token with its char span into the original text.
#[derive(Debug, Clone)]
struct TokenWithSpan {
    id: u32,
    char_start: usize,
    char_end: usize,
}

/// Encoded output for one text: token ids plus their char spans.
#[derive(Debug, Clone)]
struct TokenizationOutput {
    tokens: Vec<TokenWithSpan>,
}

/// HuggingFace tokenizer-backed [`TokenProvider`].
///
/// The chunker calls `estimate_token_count` then `tokenize`/`get_token_spans`
/// on the same substring in quick succession, so the last (text, output) pair
/// is memoized to avoid re-encoding. `Mutex` is used because `TokenProvider`
/// requires `Send + Sync` and the trait methods take `&self`.
pub struct HfTokenProvider {
    tokenizer: Tokenizer,
    memo: Mutex<Option<(String, Arc<TokenizationOutput>)>>,
}

impl std::fmt::Debug for HfTokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tokenizer is not Debug; expose only that a provider exists.
        f.debug_struct("HfTokenProvider").finish_non_exhaustive()
    }
}

impl HfTokenProvider {
    /// Load from a local `tokenizer.json` file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let tokenizer = Tokenizer::from_file(path)
            .map_err(|e| anyhow!("failed to load tokenizer from {}: {e}", path.display()))?;
        Ok(Self {
            tokenizer,
            memo: Mutex::new(None),
        })
    }

    /// Download `tokenizer.json` from a HuggingFace repo (via hf-hub, cached in
    /// `HF_HOME`) and load it. Uses the async hf-hub API; the download runs on
    /// the current tokio runtime.
    pub async fn from_hf_repo(repo_id: &str) -> Result<Self> {
        let api = hf_hub::api::tokio::ApiBuilder::from_env()
            .with_progress(false)
            .build()
            .map_err(|e| anyhow!("failed to build hf-hub api: {e}"))?;
        let repo = api.model(repo_id.to_string());
        let tokenizer_file = repo
            .get("tokenizer.json")
            .await
            .map_err(|e| anyhow!("failed to fetch tokenizer.json from '{repo_id}': {e}"))?;
        Self::from_file(&tokenizer_file)
    }

    /// Encode `text`, returning token ids with char spans. Uses the 1-entry
    /// memo when the same text was just encoded.
    fn tokenized(&self, text: &str) -> Result<Arc<TokenizationOutput>> {
        {
            let guard = self.memo.lock().unwrap();
            if let Some((cached_text, out)) = guard.as_ref()
                && cached_text == text
            {
                return Ok(out.clone());
            }
        }

        let encoding = self
            .tokenizer
            .encode(text, false)
            .map_err(|e| anyhow!("tokenization failed: {e}"))?;
        let ids = encoding.get_ids();
        let offsets = encoding.get_offsets();
        if ids.len() != offsets.len() {
            return Err(anyhow!(
                "tokenizer returned {} ids but {} offsets",
                ids.len(),
                offsets.len()
            ));
        }

        let map = CharByteMap::new(text);
        let mut tokens = Vec::with_capacity(ids.len());
        for (&id, &(byte_start, byte_end)) in ids.iter().zip(offsets.iter()) {
            // Special tokens (BOS/EOS) report (0, 0); pass them through as an
            // empty span at the start instead of failing the byte→char lookup.
            let (char_start, char_end) = if byte_start == 0 && byte_end == 0 {
                (0, 0)
            } else {
                map.byte_range_to_char_range(byte_start, byte_end)?
            };
            tokens.push(TokenWithSpan {
                id,
                char_start,
                char_end,
            });
        }

        let out = Arc::new(TokenizationOutput { tokens });
        *self.memo.lock().unwrap() = Some((text.to_string(), out.clone()));
        Ok(out)
    }
}

impl TokenProvider for HfTokenProvider {
    type Error = TokenProviderError;

    fn tokenize(&self, text: &str) -> std::result::Result<Vec<u32>, Self::Error> {
        Ok(self.tokenized(text)?.tokens.iter().map(|t| t.id).collect())
    }

    fn estimate_token_count(&self, text: &str) -> std::result::Result<usize, Self::Error> {
        // Exact count (not a heuristic): the chunker's size decisions use this.
        Ok(self.tokenized(text)?.tokens.len())
    }

    fn token_to_char(
        &self,
        text: &str,
        token_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        Ok(self
            .tokenized(text)?
            .tokens
            .get(token_pos)
            .map(|t| t.char_start))
    }

    fn char_to_token(
        &self,
        text: &str,
        char_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        let out = self.tokenized(text)?;
        for (idx, tok) in out.tokens.iter().enumerate() {
            if char_pos >= tok.char_start && char_pos < tok.char_end {
                return Ok(Some(idx));
            }
        }
        // Past the last token: clamp to the final index rather than erroring, to
        // match the chunker's contract for end-of-text positions.
        Ok(out
            .tokens
            .last()
            .map(|_| out.tokens.len().saturating_sub(1)))
    }

    fn get_token_spans(
        &self,
        text: &str,
    ) -> std::result::Result<Option<Vec<(usize, usize)>>, Self::Error> {
        Ok(Some(
            self.tokenized(text)?
                .tokens
                .iter()
                .map(|t| (t.char_start, t.char_end))
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal whitespace-pretokenizer tokenizer.json with a tiny WordLevel
    /// vocab. Enough to exercise ids/offsets/spans offline without a network
    /// download. Unknown words map to [UNK].
    fn fixture_tokenizer_json() -> String {
        serde_json::json!({
            "version": "1.0",
            "truncation": null,
            "padding": null,
            "added_tokens": [],
            "normalizer": null,
            "pre_tokenizer": { "type": "Whitespace" },
            "post_processor": null,
            "decoder": null,
            "model": {
                "type": "WordLevel",
                "vocab": { "[UNK]": 0, "hello": 1, "world": 2, "foo": 3 },
                "unk_token": "[UNK]"
            }
        })
        .to_string()
    }

    fn fixture_provider() -> HfTokenProvider {
        // NamedTempFile gives each test a unique path; keep it alive until the
        // tokenizer is loaded (from_file reads it synchronously).
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(fixture_tokenizer_json().as_bytes()).unwrap();
        f.flush().unwrap();
        HfTokenProvider::from_file(f.path()).unwrap()
    }

    #[test]
    fn test_known_token_count() {
        let p = fixture_provider();
        // "hello world" → 2 tokens with the Whitespace pretokenizer.
        assert_eq!(p.estimate_token_count("hello world").unwrap(), 2);
        assert_eq!(p.tokenize("hello world").unwrap().len(), 2);
    }

    #[test]
    fn test_spans_align_to_char_boundaries() {
        let p = fixture_provider();
        let text = "hello world";
        let spans = p.get_token_spans(text).unwrap().unwrap();
        assert_eq!(spans.len(), 2);
        // First token "hello" → chars 0..5, second "world" → 6..11.
        assert_eq!(spans[0], (0, 5));
        assert_eq!(spans[1], (6, 11));
    }

    #[test]
    fn test_char_to_token_clamps_past_end() {
        let p = fixture_provider();
        let text = "hello world";
        // A position past the final token's end clamps to the last index.
        let last = p.char_to_token(text, text.chars().count()).unwrap();
        assert_eq!(last, Some(1));
    }

    #[test]
    fn test_token_to_char_out_of_range_is_none() {
        let p = fixture_provider();
        assert_eq!(p.token_to_char("hello world", 99).unwrap(), None);
    }

    #[test]
    fn test_unknown_words_still_tokenize() {
        let p = fixture_provider();
        // Unknown words map to [UNK] but still produce one token each.
        assert_eq!(p.estimate_token_count("foo bar baz").unwrap(), 3);
    }

    #[test]
    fn test_from_file_rejects_broken_json() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"{ not valid tokenizer json").unwrap();
        f.flush().unwrap();
        assert!(HfTokenProvider::from_file(f.path()).is_err());
    }

    #[tokio::test]
    #[ignore = "requires network access to HuggingFace Hub"]
    async fn test_from_hf_repo_downloads() {
        // Manual: cargo test ... -- --ignored test_from_hf_repo_downloads
        let p = HfTokenProvider::from_hf_repo("nomic-ai/nomic-embed-text-v1.5")
            .await
            .unwrap();
        assert!(p.estimate_token_count("hello world").unwrap() > 0);
    }
}
