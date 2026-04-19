//! Chunk windowing: split long chunks into overlapping windows for embedding.

use cqs::{Chunk, Embedder};

// Windowing constants
//
// WINDOW_OVERHEAD: reserved tokens for query/passage prefix and special tokens
const WINDOW_OVERHEAD: usize = 32;

/// Compute max tokens per window from the model's max_seq_length.
/// Falls back to 480 (safe for 512-token models) if model config unavailable.
pub(crate) fn max_tokens_per_window(model_max_seq: usize) -> usize {
    if model_max_seq == 0 {
        480
    } else {
        model_max_seq.saturating_sub(WINDOW_OVERHEAD).max(128)
    }
}

/// Compute overlap tokens scaled to window size (~12.5% overlap).
/// Floor of 64 for small models, scales up for large-context models.
/// Clamped to `max_tokens / 2 - 1` to satisfy `split_into_windows`'s
/// requirement that `overlap < max_tokens / 2` (prevents exponential
/// window count).
pub(crate) fn window_overlap_tokens(max_tokens: usize) -> usize {
    if max_tokens < 4 {
        return 0;
    }
    let overlap = 64.max(max_tokens / 8);
    let ceiling = max_tokens / 2 - 1;
    overlap.min(ceiling)
}

/// Apply windowing to chunks that exceed the token limit.
/// Long chunks are split into overlapping windows; short chunks pass through unchanged.
pub(crate) fn apply_windowing(chunks: Vec<Chunk>, embedder: &Embedder) -> Vec<Chunk> {
    let _span = tracing::info_span!("apply_windowing", chunk_count = chunks.len()).entered();
    let mut result = Vec::with_capacity(chunks.len());

    // P3 #119: max_tokens and overlap are model-fixed; computed once outside the loop.
    let max_tokens = max_tokens_per_window(embedder.model_config().max_seq_length);
    let overlap = window_overlap_tokens(max_tokens);

    for chunk in chunks {
        match embedder.split_into_windows(&chunk.content, max_tokens, overlap) {
            Ok(windows) if windows.len() == 1 => {
                // Fits in one window - pass through unchanged
                result.push(chunk);
            }
            Ok(windows) => {
                // Split into multiple windows
                let parent_id = chunk.id.clone();
                for (window_content, window_idx) in windows {
                    let window_hash = blake3::hash(window_content.as_bytes()).to_hex().to_string();
                    result.push(Chunk {
                        id: format!("{}:w{}", parent_id, window_idx),
                        file: chunk.file.clone(),
                        language: chunk.language,
                        chunk_type: chunk.chunk_type,
                        name: chunk.name.clone(),
                        signature: chunk.signature.clone(),
                        content: window_content,
                        doc: if window_idx == 0 {
                            chunk.doc.clone()
                        } else {
                            None
                        }, // Doc only on first window
                        line_start: chunk.line_start,
                        line_end: chunk.line_end,
                        content_hash: window_hash,
                        parent_id: Some(parent_id.clone()),
                        window_idx: Some(window_idx),
                        parent_type_name: chunk.parent_type_name.clone(),
                        parser_version: chunk.parser_version,
                    });
                }
            }
            Err(e) => {
                // Tokenization failed - pass through unchanged and hope for the best
                tracing::warn!(chunk_id = %chunk.id, error = %e, "Windowing failed, passing through");
                result.push(chunk);
            }
        }
    }

    result
}
