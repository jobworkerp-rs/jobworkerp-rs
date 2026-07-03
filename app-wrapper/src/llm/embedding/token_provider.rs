//! Token providers for embedding chunking.
//!
//! Phase 1 uses only the tokenizer-free `CharacterEstimation` fallback via
//! [`super::chunking::NoopTokenProvider`], so no real provider lives here yet.
//!
//! Phase 2 will add `TiktokenProvider` (OpenAI `cl100k_base` via `tiktoken-rs`)
//! and Phase 3 `HfTokenProvider` (`tokenizers` + `hf-hub`, see
//! `mm-embedding-runner/src/tokenization.rs`). Both will implement
//! `command_utils::text::chunking::TokenProvider` and be selected by
//! `TokenEstimation` in the resolved chunking config.

// Intentionally empty for Phase 1; see module docs.
