//! tiktoken (OpenAI BPE) token provider (Phase 2).
//!
//! Wraps a `tiktoken_rs::CoreBPE` and exposes it as a
//! `command_utils::text::chunking::TokenProvider` so the hierarchical chunker
//! can split embedding inputs by real OpenAI token counts (for
//! text-embedding-3 / GPT-4o family models on GenAI/OpenAI backends).
//!
//! # Char spans
//! tiktoken does not expose per-token char offsets, so `get_token_spans`
//! returns `Ok(None)` and `token_to_char`/`char_to_token` return `Ok(None)`.
//! Token *counts* are exact (so chunk-size and split-count decisions are
//! precise); only the char boundary of each split falls back to the chunker's
//! internal string-search, matching `CharacterEstimation` char precision.
//! Exact spans (via incremental decode) are a future refinement if required.

use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use command_utils::text::chunking::TokenProvider;
use tiktoken_rs::CoreBPE;

use super::TokenProviderError;

/// The default tiktoken encoding when none is specified. `cl100k_base` is the
/// encoding used by `text-embedding-3-*` and GPT-3.5/4.
pub const DEFAULT_ENCODING: &str = "cl100k_base";

/// tiktoken-backed [`TokenProvider`].
///
/// The chunker calls `estimate_token_count` then `tokenize` on the same
/// substring in quick succession, so the last (text, ids) pair is memoized to
/// avoid re-encoding. `Mutex` is used because `TokenProvider` requires
/// `Send + Sync` and the trait methods take `&self`.
pub struct TiktokenProvider {
    bpe: &'static CoreBPE,
    memo: Mutex<Option<(String, Arc<Vec<u32>>)>>,
}

impl std::fmt::Debug for TiktokenProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // CoreBPE is not Debug; expose only that a provider exists.
        f.debug_struct("TiktokenProvider").finish_non_exhaustive()
    }
}

impl TiktokenProvider {
    /// Build a provider for the given tiktoken encoding name. Uses the crate's
    /// cached singletons so the (expensive) BPE table is built at most once per
    /// encoding across the whole process. Unknown encodings error.
    pub fn new(encoding: &str) -> Result<Self> {
        let bpe = match encoding {
            "cl100k_base" => tiktoken_rs::cl100k_base_singleton(),
            "o200k_base" => tiktoken_rs::o200k_base_singleton(),
            other => {
                return Err(anyhow!(
                    "unknown tiktoken encoding '{other}' (supported: cl100k_base, o200k_base)"
                ));
            }
        };
        Ok(Self {
            bpe,
            memo: Mutex::new(None),
        })
    }

    /// Encode `text` into token ids (special tokens treated as ordinary text,
    /// as appropriate for embedding inputs). Uses the 1-entry memo when the
    /// same text was just encoded.
    fn tokenized(&self, text: &str) -> Arc<Vec<u32>> {
        {
            let guard = self.memo.lock().unwrap();
            if let Some((cached_text, ids)) = guard.as_ref()
                && cached_text == text
            {
                return ids.clone();
            }
        }
        let ids = Arc::new(self.bpe.encode_ordinary(text));
        *self.memo.lock().unwrap() = Some((text.to_string(), ids.clone()));
        ids
    }
}

impl TokenProvider for TiktokenProvider {
    type Error = TokenProviderError;

    fn tokenize(&self, text: &str) -> std::result::Result<Vec<u32>, Self::Error> {
        Ok((*self.tokenized(text)).clone())
    }

    fn estimate_token_count(&self, text: &str) -> std::result::Result<usize, Self::Error> {
        // Exact count (not a heuristic): the chunker's size decisions use this.
        Ok(self.tokenized(text).len())
    }

    fn token_to_char(
        &self,
        _text: &str,
        _token_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        // tiktoken exposes no char offsets; the chunker falls back to string
        // search for char boundaries.
        Ok(None)
    }

    fn char_to_token(
        &self,
        _text: &str,
        _char_pos: usize,
    ) -> std::result::Result<Option<usize>, Self::Error> {
        Ok(None)
    }

    fn get_token_spans(
        &self,
        _text: &str,
    ) -> std::result::Result<Option<Vec<(usize, usize)>>, Self::Error> {
        // No per-token spans available; signals the chunker to use its
        // string-search fallback for char boundaries.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cl100k_known_token_count() {
        let p = TiktokenProvider::new("cl100k_base").unwrap();
        // "hello world" is 2 tokens under cl100k_base.
        assert_eq!(p.estimate_token_count("hello world").unwrap(), 2);
        assert_eq!(p.tokenize("hello world").unwrap().len(), 2);
    }

    #[test]
    fn test_o200k_switch() {
        // o200k_base must load and produce a positive token count.
        let p = TiktokenProvider::new("o200k_base").unwrap();
        assert!(p.estimate_token_count("hello world").unwrap() > 0);
    }

    #[test]
    fn test_unknown_encoding_errors() {
        assert!(TiktokenProvider::new("not_a_real_encoding").is_err());
    }

    #[test]
    fn test_spans_none_but_counts_exact() {
        let p = TiktokenProvider::new("cl100k_base").unwrap();
        // No char spans (Ok(None)), but token counts are still available.
        assert_eq!(p.get_token_spans("hello world").unwrap(), None);
        assert_eq!(p.token_to_char("hello world", 0).unwrap(), None);
        assert_eq!(p.char_to_token("hello world", 0).unwrap(), None);
    }

    #[test]
    fn test_japanese_counts_more_precise_than_char_heuristic() {
        let p = TiktokenProvider::new("cl100k_base").unwrap();
        // Japanese text: tiktoken splits multibyte text into several tokens.
        // The exact count must be positive and match a fresh encode (no memo
        // corruption across calls).
        let text = "これはテスト文章です。";
        let first = p.estimate_token_count(text).unwrap();
        assert!(first > 0);
        // Re-encoding the same text hits the memo and returns the same count.
        assert_eq!(p.tokenize(text).unwrap().len(), first);
        // A different text must not return the previous memoized ids.
        let other = p.estimate_token_count("hello").unwrap();
        assert!(other > 0);
        assert_eq!(p.tokenize(text).unwrap().len(), first);
    }
}
