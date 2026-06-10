//! Tensor padding, L2 normalization, pooling strategies, and text
//! truncation helpers.
//!
//! Split out of the former monolithic `embedder/mod.rs` (issue #1691).

use ndarray::{Array2, Array3, Axis};

/// Truncate `text` to at most `max_bytes` bytes, snapping back to a
/// valid UTF-8 char boundary. Returns the original `text` unchanged if
/// it already fits.
///
/// The naive `&text[..max_bytes]` panics on multi-byte boundary crossings
/// (a 4-byte emoji at byte position `max_bytes - 1` would slice
/// mid-codepoint); this walks back at most `c-1 ≤ 3` bytes where `c` is
/// the longest UTF-8 sequence length.
pub(crate) fn truncate_at_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    tracing::warn!(
        len = text.len(),
        max = max_bytes,
        "Query text truncated before embedding"
    );
    let mut end = max_bytes;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }
    &text[..end]
}

/// Pad 2D sequences to a fixed length.
///
/// Production embed/rerank uses [`pad_2d_i64_from_encodings`], which fills the
/// `Array2` directly from tokenizer encodings. This helper is for tests +
/// callers that have a `&[Vec<i64>]` already in hand.
#[allow(dead_code)]
pub(crate) fn pad_2d_i64(inputs: &[Vec<i64>], max_len: usize, pad_value: i64) -> Array2<i64> {
    let batch_size = inputs.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, seq) in inputs.iter().enumerate() {
        for (j, &val) in seq.iter().take(max_len).enumerate() {
            arr[[i, j]] = val;
        }
    }
    arr
}

/// Build the padded `Array2<i64>` directly from tokenizer encodings, with no
/// per-batch `Vec<Vec<i64>>` intermediate.
///
/// `extract` selects which encoding field to pull (`get_ids`,
/// `get_attention_mask`, `get_type_ids`); the same helper covers all three
/// fields. The cast from `u32` → `i64` happens in the inner loop alongside the
/// array write, so no intermediate `Vec<i64>` is allocated — zero auxiliary
/// heap allocations beyond the final `Array2`.
pub(crate) fn pad_2d_i64_from_encodings<F>(
    encodings: &[tokenizers::Encoding],
    extract: F,
    max_len: usize,
    pad_value: i64,
) -> Array2<i64>
where
    F: Fn(&tokenizers::Encoding) -> &[u32],
{
    let batch_size = encodings.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, enc) in encodings.iter().enumerate() {
        for (j, &val) in extract(enc).iter().take(max_len).enumerate() {
            arr[[i, j]] = val as i64;
        }
    }
    arr
}

/// L2 normalize a vector (single-pass, in-place)
pub(crate) fn normalize_l2(mut v: Vec<f32>) -> Vec<f32> {
    let norm_sq: f32 = v.iter().fold(0.0, |acc, &x| acc + x * x);
    if norm_sq > 0.0 {
        let inv_norm = 1.0 / norm_sq.sqrt();
        v.iter_mut().for_each(|x| *x *= inv_norm);
    }
    v
}

// ---------------------------------------------------------------------------
// Pooling strategies
// ---------------------------------------------------------------------------
//
// Each pooler takes the `[batch, seq, dim]` hidden-state tensor and returns
// one `Vec<f32>` per batch item (unnormalized). The caller normalizes.
//
// Mean pooling is the BGE / E5 / v9-200k path. CLS and LastToken cover
// non-BERT models, dispatched via `ModelConfig::pooling`.

/// Mean-pool the masked token positions.
///
/// Builds the attention mask as a `[batch, seq, 1]` broadcast tensor, multiplies
/// in-place against hidden states, sums along the sequence axis, and divides
/// by the mask sum. Matches BGE reference / sentence-transformers mean pooling.
///
/// Batches whose attention mask is all zero return a zero vector and log a
/// warning.
pub(crate) fn mean_pool(
    hidden: &Array3<f32>,
    attention_mask: &Array2<i64>,
    embedding_dim: usize,
) -> Vec<Vec<f32>> {
    // Takes the already-built `Array2<i64>` directly so the embed pipeline
    // doesn't keep a parallel `Vec<Vec<i64>>` of the mask alongside the tensor.
    let (batch_size, seq_len, _) = hidden.dim();
    let mask_2d = Array2::from_shape_fn((batch_size, seq_len), |(i, j)| {
        attention_mask.get([i, j]).copied().unwrap_or(0) as f32
    });
    let mask_3d = mask_2d.clone().insert_axis(Axis(2));

    let masked = hidden * &mask_3d;
    let summed = masked.sum_axis(Axis(1)); // [batch, dim]
    let counts = mask_2d.sum_axis(Axis(1)).insert_axis(Axis(1)); // [batch, 1]

    (0..batch_size)
        .map(|i| {
            let count = counts[[i, 0]];
            let row = summed.row(i);
            if count > 0.0 {
                row.iter().map(|v| v / count).collect()
            } else {
                tracing::warn!(batch_idx = i, "Zero attention mask — producing zero vector");
                vec![0.0f32; embedding_dim]
            }
        })
        .collect()
}

/// CLS-pool: return the hidden state of the first token for each batch item.
///
/// Used by some DistilBERT-derived embedders trained specifically for CLS
/// pooling. On those models, using mean pooling degrades quality silently
/// (no error; just worse retrieval) — hence the configurable dispatch.
pub(crate) fn cls_pool(hidden: &Array3<f32>) -> Vec<Vec<f32>> {
    let (batch_size, _, _) = hidden.dim();
    (0..batch_size)
        .map(|i| hidden.slice(ndarray::s![i, 0usize, ..]).to_vec())
        .collect()
}

/// Last-token pool: return the hidden state of the last non-padding token,
/// located via the attention mask (rightmost `1`).
///
/// Used by autoregressive / decoder-only embedders (Qwen3-Embedding,
/// some Mistral-based embedders) where the final token's hidden state is the
/// trained embedding location.
///
/// If the mask is all zero (pathological) the function falls back to the
/// first token and logs a warning. If a batch item's mask has no `1`s we
/// use index 0.
pub(crate) fn last_token_pool(hidden: &Array3<f32>, attention_mask: &Array2<i64>) -> Vec<Vec<f32>> {
    // Takes the `Array2<i64>` directly — see `mean_pool` for the rationale.
    let (batch_size, seq_len, _) = hidden.dim();
    let (mask_batch, mask_seq) = attention_mask.dim();
    (0..batch_size)
        .map(|i| {
            // Find the last position where the mask is set. `i` may be beyond
            // the mask Array2's first dim only if a caller passes a mismatched
            // shape — fall back to index 0 and warn.
            let last_idx = if i < mask_batch {
                let bound = seq_len.min(mask_seq);
                let mut found = None;
                for j in (0..bound).rev() {
                    if attention_mask[[i, j]] != 0 {
                        found = Some(j);
                        break;
                    }
                }
                found.unwrap_or_else(|| {
                    tracing::warn!(
                        batch_idx = i,
                        "last_token_pool: zero attention mask — using index 0"
                    );
                    0
                })
            } else {
                tracing::warn!(
                    batch_idx = i,
                    "last_token_pool: mask shorter than batch — using index 0"
                );
                0
            };
            hidden.slice(ndarray::s![i, last_idx, ..]).to_vec()
        })
        .collect()
}
