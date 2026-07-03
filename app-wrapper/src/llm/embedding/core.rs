//! Backend-agnostic embedding orchestration: input flattening, response↔chunk
//! mapping, retry-split, and usage aggregation.
//!
//! Kept separate from the `RunnerTrait` wiring in `embedding.rs` so the tricky
//! parts (retry char-offset absolutization, GenAI index mapping, usage
//! aggregation) can be unit-tested against a mock backend without a real
//! provider or the full runner.

use anyhow::{Result, anyhow};
use jobworkerp_runner::jobworkerp::runner::llm::llm_embedding_args::embedding_content::Content;
use jobworkerp_runner::jobworkerp::runner::llm::llm_embedding_result::{Embedding, Usage};
use jobworkerp_runner::jobworkerp::runner::llm::{LlmEmbeddingArgs, LlmEmbeddingResult};

use super::chunking::{ResolvedChunkingConfig, chunk_text};
use super::{
    EmbeddingBackend, EmbeddingError, ProviderUsage, ResolvedEmbeddingOptions, aggregate_usage,
};

/// A flattened text chunk tagged with its origin. `char_start`/`char_end` are
/// ALWAYS absolute char offsets into the original input text (never relative to
/// a parent chunk), so retry-split must absolutize before constructing these.
#[derive(Debug, Clone)]
pub struct ChunkRef {
    pub input_index: usize,
    pub char_start: usize,
    pub char_end: usize,
    pub content: String,
}

/// Lower bound below which we stop shrinking max_chunk_tokens during retry.
const MIN_RETRY_MAX_TOKENS: u32 = 16;
/// Shrink factor applied to max_chunk_tokens on each whole-batch retry.
const RETRY_SHRINK_NUM: u32 = 1;
const RETRY_SHRINK_DEN: u32 = 2;
/// Upper bound on whole-batch re-chunk attempts.
const MAX_WHOLE_RETRIES: usize = 3;

/// Flatten `args.inputs` into a contiguous `Vec<ChunkRef>` (input order → chunk
/// order). Text inputs are chunked; non-text inputs return an unsupported
/// error until multimodal support lands.
pub fn build_chunk_refs(
    args: &LlmEmbeddingArgs,
    config: &ResolvedChunkingConfig,
) -> Result<Vec<ChunkRef>> {
    if args.inputs.is_empty() {
        return Err(anyhow!("embedding inputs must not be empty"));
    }
    let mut refs = Vec::new();
    for (input_index, input) in args.inputs.iter().enumerate() {
        match &input.content {
            Some(Content::Text(text)) => {
                let chunks = chunk_text(text, config)?;
                for c in chunks {
                    refs.push(ChunkRef {
                        input_index,
                        char_start: c.char_start,
                        char_end: c.char_end,
                        content: c.content,
                    });
                }
            }
            Some(Content::Image(_)) | Some(Content::Media(_)) => {
                return Err(anyhow!(
                    "non-text embedding input (index {input_index}) is not supported yet"
                ));
            }
            None => {
                return Err(anyhow!("embedding input {input_index} has no content"));
            }
        }
    }
    if refs.is_empty() {
        return Err(anyhow!("no embeddable chunks produced from inputs"));
    }
    Ok(refs)
}

/// Map a backend response's embeddings to the chunk refs they were computed
/// from, producing vectors aligned to `refs` order.
///
/// - Ollama-style (`index: None`): position-based zip; counts must match.
/// - GenAI-style (`index: Some`): map `resp[i]` to `refs[index]`; every ref
///   must be covered exactly once (no out-of-range / dup / missing).
pub fn map_response_to_refs(
    refs_len: usize,
    embeddings: Vec<super::ProviderEmbedding>,
) -> Result<Vec<Vec<f32>>> {
    let has_index = embeddings.iter().any(|e| e.index.is_some());
    if !has_index {
        // Order-preserving contract.
        if embeddings.len() != refs_len {
            return Err(anyhow!(
                "backend returned {} embeddings for {} chunks",
                embeddings.len(),
                refs_len
            ));
        }
        return Ok(embeddings.into_iter().map(|e| e.vector).collect());
    }

    // Index-based mapping.
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; refs_len];
    for e in embeddings {
        let idx = e
            .index
            .ok_or_else(|| anyhow!("mixed indexed/unindexed embeddings in one response"))?;
        if idx >= refs_len {
            return Err(anyhow!(
                "embedding index {idx} out of range (chunks={refs_len})"
            ));
        }
        if slots[idx].is_some() {
            return Err(anyhow!("duplicate embedding index {idx}"));
        }
        slots[idx] = Some(e.vector);
    }
    let mut out = Vec::with_capacity(refs_len);
    for (i, slot) in slots.into_iter().enumerate() {
        out.push(slot.ok_or_else(|| anyhow!("missing embedding for chunk index {i}"))?);
    }
    Ok(out)
}

/// Shrink max_chunk_tokens for the next retry, or `None` if already at floor.
fn shrunk_config(config: &ResolvedChunkingConfig) -> Option<ResolvedChunkingConfig> {
    let next = (config.max_chunk_tokens * RETRY_SHRINK_NUM) / RETRY_SHRINK_DEN;
    let next = next.max(config.min_chunk_tokens);
    if next < MIN_RETRY_MAX_TOKENS || next >= config.max_chunk_tokens {
        return None;
    }
    Some(ResolvedChunkingConfig {
        max_chunk_tokens: next,
        ..config.clone()
    })
}

/// Re-chunk a single parent `ChunkRef`, absolutizing child offsets back to the
/// original input text via `parent.char_start + child_rel`. This is the retry
/// char-offset correction required so retried inputs don't get corrupted
/// positions.
fn rechunk_ref(parent: &ChunkRef, config: &ResolvedChunkingConfig) -> Result<Vec<ChunkRef>> {
    let children = chunk_text(&parent.content, config)?;
    Ok(children
        .into_iter()
        .map(|c| ChunkRef {
            input_index: parent.input_index,
            char_start: parent.char_start + c.char_start,
            char_end: parent.char_start + c.char_end,
            content: c.content,
        })
        .collect())
}

/// Embed all `refs` via `backend`, applying whole-batch retry-split on
/// too-long, then single-item fallback. Returns per-ref vectors (aligned to the
/// returned refs) plus aggregated usage. The returned refs may differ from the
/// input refs when retry-split occurred (they carry absolute offsets).
pub async fn embed_refs(
    backend: &dyn EmbeddingBackend,
    model: &str,
    opts: &ResolvedEmbeddingOptions,
    refs: Vec<ChunkRef>,
    config: &ResolvedChunkingConfig,
) -> Result<(Vec<ChunkRef>, Vec<Vec<f32>>, Option<ProviderUsage>)> {
    let mut usages: Vec<Option<ProviderUsage>> = Vec::new();

    // Whole-batch attempts with progressively shrunk chunking.
    let mut current_refs = refs;
    let mut current_config = config.clone();
    for attempt in 0..=MAX_WHOLE_RETRIES {
        let texts: Vec<String> = current_refs.iter().map(|r| r.content.clone()).collect();
        match backend.embed_batch(model, texts, opts).await {
            Ok(resp) => {
                usages.push(resp.usage);
                let vectors = map_response_to_refs(current_refs.len(), resp.embeddings)?;
                return Ok((current_refs, vectors, aggregate_usage(&usages)));
            }
            Err(EmbeddingError::TooLong(_)) if attempt < MAX_WHOLE_RETRIES => {
                // Re-chunk the whole batch with a smaller max_chunk_tokens.
                match shrunk_config(&current_config) {
                    Some(smaller) => {
                        let mut rebuilt = Vec::new();
                        for r in &current_refs {
                            rebuilt.extend(rechunk_ref(r, &smaller)?);
                        }
                        current_refs = rebuilt;
                        current_config = smaller;
                    }
                    None => break, // hit token floor; go to single fallback
                }
            }
            Err(EmbeddingError::TooLong(_)) => break, // exhausted whole retries
            Err(EmbeddingError::Other(e)) => return Err(e),
        }
    }

    // Single-item fallback: embed each ref alone to isolate the offender.
    single_item_fallback(
        backend,
        model,
        opts,
        current_refs,
        &current_config,
        &mut usages,
    )
    .await
}

/// Embed each ref individually; shrink-retry a too-long single ref down to the
/// token floor. Any ref still too long at the floor fails the job with its
/// origin in the message.
async fn single_item_fallback(
    backend: &dyn EmbeddingBackend,
    model: &str,
    opts: &ResolvedEmbeddingOptions,
    refs: Vec<ChunkRef>,
    config: &ResolvedChunkingConfig,
    usages: &mut Vec<Option<ProviderUsage>>,
) -> Result<(Vec<ChunkRef>, Vec<Vec<f32>>, Option<ProviderUsage>)> {
    let mut out_refs = Vec::new();
    let mut out_vecs = Vec::new();

    // Work queue so a too-long single ref can be replaced by its sub-chunks.
    let mut queue: std::collections::VecDeque<(ChunkRef, ResolvedChunkingConfig)> =
        refs.into_iter().map(|r| (r, config.clone())).collect();

    while let Some((r, cfg)) = queue.pop_front() {
        match backend
            .embed_batch(model, vec![r.content.clone()], opts)
            .await
        {
            Ok(resp) => {
                usages.push(resp.usage);
                let mut vectors = map_response_to_refs(1, resp.embeddings)?;
                out_refs.push(r);
                out_vecs.push(vectors.remove(0));
            }
            Err(EmbeddingError::TooLong(e)) => {
                let too_long = |r: &ChunkRef| {
                    anyhow!(
                        "chunk from input {} [{}..{}) still too long at min chunk size: {e}",
                        r.input_index,
                        r.char_start,
                        r.char_end
                    )
                };
                match shrunk_config(&cfg) {
                    Some(smaller) => {
                        // Replace this ref with its (absolutized) sub-chunks.
                        let children = rechunk_ref(&r, &smaller)?;
                        if children.len() == 1 && children[0].content == r.content {
                            // No further split possible.
                            return Err(too_long(&r));
                        }
                        for c in children.into_iter().rev() {
                            queue.push_front((c, smaller.clone()));
                        }
                    }
                    None => return Err(too_long(&r)),
                }
            }
            Err(EmbeddingError::Other(e)) => return Err(e),
        }
    }

    Ok((out_refs, out_vecs, aggregate_usage(usages)))
}

/// Assemble the final `LlmEmbeddingResult` from aligned refs/vectors and usage.
/// Output is in refs order (input order → chunk order).
pub fn build_result(
    refs: &[ChunkRef],
    vectors: Vec<Vec<f32>>,
    usage: Option<ProviderUsage>,
) -> LlmEmbeddingResult {
    let embeddings = refs
        .iter()
        .zip(vectors)
        .map(|(r, vector)| {
            let dimensions = vector.len() as u32;
            Embedding {
                vector,
                input_index: r.input_index as u32,
                begin_position: r.char_start as u32,
                end_position: r.char_end as u32,
                content: r.content.clone(),
                dimensions,
            }
        })
        .collect();

    LlmEmbeddingResult {
        embeddings,
        usage: usage.map(|u| Usage {
            model: u.model,
            prompt_tokens: u.prompt_tokens,
            total_tokens: u.total_tokens,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::embedding::chunking::TokenEstimationStrategy;
    use crate::llm::embedding::test_support::{MockEmbeddingBackend, MockOutcome};
    use crate::llm::embedding::{EmbeddingBackendResponse, ProviderEmbedding};
    use jobworkerp_runner::jobworkerp::runner::llm::llm_embedding_args::EmbeddingContent;

    fn text_input(s: &str) -> EmbeddingContent {
        EmbeddingContent {
            content: Some(Content::Text(s.to_string())),
        }
    }

    fn cfg(max: u32) -> ResolvedChunkingConfig {
        ResolvedChunkingConfig {
            max_chunk_tokens: max,
            min_chunk_tokens: 0,
            token_estimation: TokenEstimationStrategy::CharacterEstimation,
        }
    }

    #[test]
    fn test_build_chunk_refs_text_single() {
        let args = LlmEmbeddingArgs {
            inputs: vec![text_input("hello")],
            ..Default::default()
        };
        let refs = build_chunk_refs(&args, &cfg(512)).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].input_index, 0);
        assert_eq!(refs[0].content, "hello");
    }

    #[test]
    fn test_build_chunk_refs_rejects_empty_and_nontext() {
        let empty = LlmEmbeddingArgs {
            inputs: vec![],
            ..Default::default()
        };
        assert!(build_chunk_refs(&empty, &cfg(512)).is_err());

        let img = LlmEmbeddingArgs {
            inputs: vec![EmbeddingContent {
                content: Some(Content::Image(Default::default())),
            }],
            ..Default::default()
        };
        assert!(build_chunk_refs(&img, &cfg(512)).is_err());
    }

    #[test]
    fn test_map_response_order_preserving() {
        let embs = vec![
            ProviderEmbedding {
                vector: vec![1.0],
                index: None,
            },
            ProviderEmbedding {
                vector: vec![2.0],
                index: None,
            },
        ];
        let out = map_response_to_refs(2, embs).unwrap();
        assert_eq!(out, vec![vec![1.0], vec![2.0]]);
    }

    #[test]
    fn test_map_response_order_preserving_count_mismatch_errors() {
        let embs = vec![ProviderEmbedding {
            vector: vec![1.0],
            index: None,
        }];
        assert!(map_response_to_refs(2, embs).is_err());
    }

    #[test]
    fn test_map_response_index_based_shuffled() {
        // Response arrives out of order; index must drive placement.
        let embs = vec![
            ProviderEmbedding {
                vector: vec![2.0],
                index: Some(1),
            },
            ProviderEmbedding {
                vector: vec![0.0],
                index: Some(0),
            },
            ProviderEmbedding {
                vector: vec![1.0],
                index: Some(2),
            },
        ];
        let out = map_response_to_refs(3, embs).unwrap();
        assert_eq!(out, vec![vec![0.0], vec![2.0], vec![1.0]]);
    }

    #[test]
    fn test_map_response_index_out_of_range_dup_missing() {
        // out of range
        assert!(
            map_response_to_refs(
                1,
                vec![ProviderEmbedding {
                    vector: vec![0.0],
                    index: Some(3)
                }]
            )
            .is_err()
        );
        // duplicate
        assert!(
            map_response_to_refs(
                2,
                vec![
                    ProviderEmbedding {
                        vector: vec![0.0],
                        index: Some(0)
                    },
                    ProviderEmbedding {
                        vector: vec![1.0],
                        index: Some(0)
                    },
                ]
            )
            .is_err()
        );
        // missing (only index 0 provided for 2 chunks)
        assert!(
            map_response_to_refs(
                2,
                vec![ProviderEmbedding {
                    vector: vec![0.0],
                    index: Some(0)
                },]
            )
            .is_err()
        );
    }

    fn ok_resp(n: usize, usage: Option<ProviderUsage>) -> EmbeddingBackendResponse {
        EmbeddingBackendResponse {
            embeddings: (0..n)
                .map(|_| ProviderEmbedding {
                    vector: vec![0.5],
                    index: None,
                })
                .collect(),
            usage,
        }
    }

    #[tokio::test]
    async fn test_embed_refs_happy_path_with_usage() {
        let refs = vec![
            ChunkRef {
                input_index: 0,
                char_start: 0,
                char_end: 3,
                content: "abc".into(),
            },
            ChunkRef {
                input_index: 0,
                char_start: 3,
                char_end: 6,
                content: "def".into(),
            },
        ];
        let backend = MockEmbeddingBackend::new(vec![MockOutcome::Ok(ok_resp(
            2,
            Some(ProviderUsage {
                model: "m".into(),
                prompt_tokens: Some(4),
                total_tokens: Some(4),
            }),
        ))]);
        let (out_refs, vecs, usage) = embed_refs(
            &backend,
            "m",
            &ResolvedEmbeddingOptions::default(),
            refs,
            &cfg(512),
        )
        .await
        .unwrap();
        assert_eq!(out_refs.len(), 2);
        assert_eq!(vecs.len(), 2);
        assert_eq!(usage.unwrap().prompt_tokens, Some(4));
    }

    #[tokio::test]
    async fn test_embed_refs_usage_aggregated_across_batches() {
        // Two refs; each embedded via single-item fallback returns usage, which
        // must be summed. Script: whole-batch TooLong (→ single fallback), then
        // two per-item Ok responses each carrying usage.
        let refs = vec![
            ChunkRef {
                input_index: 0,
                char_start: 0,
                char_end: 3,
                content: "abc".into(),
            },
            ChunkRef {
                input_index: 0,
                char_start: 3,
                char_end: 6,
                content: "def".into(),
            },
        ];
        let usage = |p| {
            Some(ProviderUsage {
                model: "m".into(),
                prompt_tokens: Some(p),
                total_tokens: Some(p),
            })
        };
        let backend = MockEmbeddingBackend::new(vec![
            MockOutcome::TooLong,                  // whole batch fails
            MockOutcome::Ok(ok_resp(1, usage(3))), // ref 0 alone
            MockOutcome::Ok(ok_resp(1, usage(4))), // ref 1 alone
        ]);
        // max==min==16 so shrunk_config returns None → skip whole retries and
        // go straight to single-item fallback.
        let floor_cfg = ResolvedChunkingConfig {
            max_chunk_tokens: 16,
            min_chunk_tokens: 16,
            token_estimation: TokenEstimationStrategy::CharacterEstimation,
        };
        let (out_refs, vecs, agg) = embed_refs(
            &backend,
            "m",
            &ResolvedEmbeddingOptions::default(),
            refs,
            &floor_cfg,
        )
        .await
        .unwrap();
        assert_eq!(out_refs.len(), 2);
        assert_eq!(vecs.len(), 2);
        // Usage summed across the two single-item batches (3 + 4 = 7).
        let agg = agg.unwrap();
        assert_eq!(agg.prompt_tokens, Some(7));
        assert_eq!(agg.total_tokens, Some(7));
    }

    #[tokio::test]
    async fn test_embed_refs_retry_char_offset_absolute() {
        // A single parent ref that does NOT start at 0; after re-chunk, child
        // offsets must be absolute (parent.char_start + child_rel), not 0-based.
        let content = "alpha beta gamma delta epsilon zeta eta theta ".repeat(60);
        let parent_start = 100usize;
        let refs = vec![ChunkRef {
            input_index: 2,
            char_start: parent_start,
            char_end: parent_start + content.chars().count(),
            content: content.clone(),
        }];
        // Force whole-batch re-chunk: first call too long, then default-Ok.
        // max=512 so shrunk_config can shrink (512→256→...) and re-chunk the
        // long content into multiple sub-chunks with absolutized offsets.
        let backend = MockEmbeddingBackend::new(vec![MockOutcome::TooLong]);
        let (out_refs, _vecs, _usage) = embed_refs(
            &backend,
            "m",
            &ResolvedEmbeddingOptions::default(),
            refs,
            &cfg(64),
        )
        .await
        .unwrap();
        assert!(out_refs.len() > 1);
        // Every child offset must be >= parent_start (absolutized), and its
        // content must equal the parent's char slice at the absolute range.
        let parent_chars: Vec<char> = content.chars().collect();
        for r in &out_refs {
            assert!(
                r.char_start >= parent_start,
                "child offset must be absolute"
            );
            assert_eq!(r.input_index, 2, "input_index inherited from parent");
            let rel_start = r.char_start - parent_start;
            let rel_end = r.char_end - parent_start;
            let expected: String = parent_chars[rel_start..rel_end].iter().collect();
            assert_eq!(
                r.content, expected,
                "content must match absolute char slice"
            );
        }
    }

    #[tokio::test]
    async fn test_embed_refs_propagates_non_too_long_error() {
        let refs = vec![ChunkRef {
            input_index: 0,
            char_start: 0,
            char_end: 1,
            content: "a".into(),
        }];
        let backend = MockEmbeddingBackend::new(vec![MockOutcome::Err("boom".into())]);
        let res = embed_refs(
            &backend,
            "m",
            &ResolvedEmbeddingOptions::default(),
            refs,
            &cfg(512),
        )
        .await;
        assert!(res.is_err());
    }

    #[test]
    fn test_build_result_orders_and_positions() {
        let refs = vec![
            ChunkRef {
                input_index: 0,
                char_start: 0,
                char_end: 3,
                content: "abc".into(),
            },
            ChunkRef {
                input_index: 1,
                char_start: 0,
                char_end: 2,
                content: "xy".into(),
            },
        ];
        let vecs = vec![vec![1.0, 2.0], vec![3.0]];
        let result = build_result(&refs, vecs, None);
        assert_eq!(result.embeddings.len(), 2);
        assert_eq!(result.embeddings[0].input_index, 0);
        assert_eq!(result.embeddings[0].dimensions, 2);
        assert_eq!(result.embeddings[1].input_index, 1);
        assert_eq!(result.embeddings[1].begin_position, 0);
        assert_eq!(result.embeddings[1].end_position, 2);
    }
}
