//! Cross-encoder re-ranking for second-pass scoring
//!
//! Reorders search results using a cross-encoder model that scores
//! (query, passage) pairs directly, producing more accurate rankings
//! than embedding cosine similarity alone.
//!
//! Uses `cross-encoder/ms-marco-MiniLM-L-6-v2` (~91MB ONNX, 22M params).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use once_cell::sync::OnceCell;
use ort::session::Session;

use crate::aux_model::{self, AuxModelKind};
use crate::config::AuxModelSection;
use crate::embedder::{create_session, pad_2d_i64, select_provider, ExecutionProvider};
use crate::store::SearchResult;

/// Filename within the HF repo layout — kept local so
/// [`Reranker::model_paths`] can still construct the expected layout when
/// the HF Hub fetch succeeds. Matches the convention in
/// [`crate::aux_model::config_from_dir`] for `AuxModelKind::Reranker`.
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

// blake3 checksums -- empty to skip validation (set after pinning a model version)
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

/// Default batch size for reranker ORT runs.
///
/// Caps the candidate set fed to each `session.run()` call so a large `k`
/// (e.g. `--rerank-k 100` with `max_length=512`) doesn't allocate a single
/// `[100, 512]` token tensor that OOMs on small GPUs or after SPLADE has
/// claimed VRAM. Mirrors the `CQS_EMBED_BATCH_SIZE=64` pattern in the
/// embed path; 32 is conservative because cross-encoder runs produce larger
/// activations than plain encoder forward passes.
const DEFAULT_RERANKER_BATCH: usize = 32;

/// Maximum number of candidates per ORT `session.run()` in the reranker.
///
/// Reads `CQS_RERANKER_BATCH`; falls back to [`DEFAULT_RERANKER_BATCH`] when
/// unset, unparseable, or zero.
fn reranker_batch_size() -> usize {
    std::env::var("CQS_RERANKER_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(DEFAULT_RERANKER_BATCH)
}

/// Resolve the reranker model source via the shared auxiliary-model
/// resolver, threading an optional `[reranker]` config section through the
/// same preset registry SPLADE uses. Returns a fully-populated
/// [`aux_model::AuxModelConfig`] — the caller (`model_paths`) dispatches
/// on `repo.is_some()` to decide between local-dir and HF Hub fetch paths.
///
/// Precedence: CLI → `CQS_RERANKER_MODEL` → `[reranker] model_path` →
/// `[reranker] preset` → hardcoded `ms-marco-minilm`.
fn resolve_reranker(
    section: Option<&AuxModelSection>,
) -> Result<aux_model::AuxModelConfig, RerankerError> {
    let preset = section.and_then(|s| s.preset.as_deref());
    let model_path = section.and_then(|s| s.model_path.as_deref());
    let tokenizer_path = section.and_then(|s| s.tokenizer_path.as_deref());
    aux_model::resolve(
        AuxModelKind::Reranker,
        None,
        "CQS_RERANKER_MODEL",
        preset,
        model_path,
        tokenizer_path,
        aux_model::default_preset_name(AuxModelKind::Reranker),
    )
    .map_err(|e| RerankerError::ModelDownload(e.to_string()))
}

#[derive(Debug, thiserror::Error)]
pub enum RerankerError {
    #[error("Model download failed: {0}")]
    ModelDownload(String),
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),
    #[error("Inference error: {0}")]
    Inference(String),
    #[error("Checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}

/// Convert any ort error to [`RerankerError::Inference`] via `.to_string()`.
///
/// Function instead of `From` impl — see [`crate::embedder::ort_err`] for rationale.
fn ort_err<T>(e: ort::Error<T>) -> RerankerError {
    RerankerError::Inference(e.to_string())
}

/// Cross-encoder reranker for second-pass scoring
///
/// Lazy-loads the model on first use, same pattern as [`crate::Embedder`].
/// Scores (query, passage) pairs with a cross-encoder, then re-sorts results.
pub struct Reranker {
    session: Mutex<Option<Session>>,
    /// Lazy-loaded tokenizer.
    ///
    /// RM-V1.25-15: `Mutex<Option<Arc<Tokenizer>>>` so `clear_session` can
    /// drop the tokenizer (~20MB for ms-marco MiniLM) alongside the ONNX
    /// session. Callers receive an `Arc<Tokenizer>` clone and release the
    /// mutex before running inference.
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    /// Whether the loaded ONNX session expects a `token_type_ids` input.
    /// BERT-family models do; RoBERTa-family (UniXcoder, CodeBERT, all
    /// XLM-R variants) do not. Computed at session-init time by inspecting
    /// the model's input names. `None` means "session not yet loaded."
    expects_token_type_ids: Mutex<Option<bool>>,
}

impl Reranker {
    /// Create a new reranker with lazy model loading
    pub fn new() -> Result<Self, RerankerError> {
        let provider = select_provider();
        let max_length = match std::env::var("CQS_RERANKER_MAX_LENGTH") {
            Ok(val) => match val.parse::<usize>() {
                Ok(len) => {
                    tracing::info!(max_length = len, "Using custom reranker max_length");
                    len
                }
                Err(e) => {
                    tracing::warn!(
                        value = %val,
                        error = %e,
                        "Invalid CQS_RERANKER_MAX_LENGTH, using default 512"
                    );
                    512
                }
            },
            Err(_) => 512,
        };
        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            model_paths: OnceCell::new(),
            provider,
            max_length,
            expects_token_type_ids: Mutex::new(None),
        })
    }

    /// Re-rank search results using cross-encoder scoring
    ///
    /// Scores each (query, result.content) pair, re-sorts by score descending,
    /// and truncates to `limit`. No-op for 0 or 1 results.
    pub fn rerank(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        limit: usize,
    ) -> Result<(), RerankerError> {
        // PF-V1.25-5: borrow passages from results directly instead of
        // cloning content strings. The previous impl did
        // `results.iter().map(|r| r.chunk.content.clone()).collect()`,
        // allocating a fresh String per candidate (N allocations × content
        // length bytes each) only to feed them to `rerank_with_passages`.
        // Score computation happens in a scoped borrow so the subsequent
        // `&mut results` write back is valid.
        //
        // We inline the compute-score-then-apply pattern rather than
        // reusing `rerank_with_passages`, because passages that borrow
        // from `results` conflict with `&mut results` at the call site.
        let scores = {
            let passages: Vec<&str> = results.iter().map(|r| r.chunk.content.as_str()).collect();
            self.compute_scores(query, &passages)?
        };
        apply_rerank_scores(results, scores, limit);
        Ok(())
    }

    /// Re-rank search results using custom passage text per result.
    ///
    /// Like [`rerank`](Self::rerank) but scores `(query, passages[i])` instead of
    /// `(query, result.content)`. Useful for reranking on NL descriptions or
    /// other derived text. `passages` must have the same length as `results`.
    pub fn rerank_with_passages(
        &self,
        query: &str,
        results: &mut Vec<SearchResult>,
        passages: &[&str],
        limit: usize,
    ) -> Result<(), RerankerError> {
        let _span = tracing::info_span!(
            "rerank",
            count = results.len(),
            limit,
            query_len = query.len()
        )
        .entered();
        if results.len() <= 1 {
            return Ok(());
        }
        if results.len() != passages.len() {
            return Err(RerankerError::Inference(format!(
                "passages length ({}) must match results length ({})",
                passages.len(),
                results.len()
            )));
        }

        let Some(scores) = self.compute_scores_opt(query, passages)? else {
            return Ok(());
        };
        apply_rerank_scores(results, scores, limit);
        Ok(())
    }

    /// Compute cross-encoder scores for (query, passage) pairs.
    ///
    /// Returns `Some(scores)` on success, or `None` when tokenization produced
    /// zero-length encodings across all passages (nothing to score).
    /// `scores.len() == passages.len()` on `Some(...)`.
    ///
    /// PF-V1.25-5: extracted so `rerank` can feed passages borrowed directly
    /// from `&Vec<SearchResult>` without cloning contents, then apply scores
    /// via `apply_rerank_scores` in a subsequent `&mut` scope.
    ///
    /// Issue #963: passages are chunked into `CQS_RERANKER_BATCH`-sized
    /// groups (default 32) before feeding each chunk to `session.run()`. This
    /// keeps the `[chunk_len, max_length]` token tensor bounded so large `k`
    /// values don't OOM on small GPUs or after SPLADE has claimed VRAM.
    /// Scoring semantics are preserved — each candidate gets the same
    /// cross-encoder score, just computed in smaller ORT runs.
    fn compute_scores_opt(
        &self,
        query: &str,
        passages: &[&str],
    ) -> Result<Option<Vec<f32>>, RerankerError> {
        let tokenizer = self.tokenizer()?;

        // 1. Tokenize (query, passage) pairs once up front. Tokenization is
        //    cheap relative to ORT inference and doing it here lets us
        //    short-circuit (return None) when the entire input is degenerate,
        //    matching the pre-#963 semantics.
        let encodings: Vec<tokenizers::Encoding> = passages
            .iter()
            .map(|passage| {
                tokenizer
                    .encode((query, *passage), true)
                    .map_err(|e| RerankerError::Tokenizer(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let overall_max = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);
        if overall_max == 0 {
            return Ok(None); // Nothing to score — empty tokenization
        }

        let batch_cap = reranker_batch_size();
        let mut scores = Vec::with_capacity(passages.len());
        for chunk in encodings.chunks(batch_cap) {
            scores.extend(self.run_chunk(chunk)?);
        }
        Ok(Some(scores))
    }

    /// Run one reranker batch: build tensors from `chunk` and score via ORT.
    ///
    /// `chunk` is a slice of tokenized (query, passage) encodings sized to at
    /// most `CQS_RERANKER_BATCH`. The per-chunk `max_len` is the longest
    /// encoding in this chunk capped at `self.max_length`, so shorter chunks
    /// use smaller tensors.
    ///
    /// Returns one score per encoding in `chunk`.
    fn run_chunk(&self, chunk: &[tokenizers::Encoding]) -> Result<Vec<f32>, RerankerError> {
        let batch_size = chunk.len();
        debug_assert!(batch_size > 0, "run_chunk called with empty chunk");

        // Build per-chunk padded tensors.
        let input_ids: Vec<Vec<i64>> = chunk
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i64).collect())
            .collect();
        let attention_mask: Vec<Vec<i64>> = chunk
            .iter()
            .map(|e| e.get_attention_mask().iter().map(|&m| m as i64).collect())
            .collect();
        let max_len = input_ids
            .iter()
            .map(|v| v.len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);
        if max_len == 0 {
            // This chunk's passages all tokenized empty but the aggregate
            // check in compute_scores_opt already guaranteed overall_max > 0.
            // Return zero scores for this chunk; the non-empty chunks carry
            // the ranking signal.
            return Ok(vec![sigmoid(0.0); batch_size]);
        }

        // token_type_ids come from the tokenizer — BERT-family rerankers use
        // them to distinguish query (0) from passage (1). Zeroing them out (the
        // prior behavior) silently broke fine-tuned models that learned to
        // use the segment signal (caught during reranker v2 eval: gold chunks
        // got pushed below negatives because the model saw "query query" when
        // the tokenizer had emitted "query passage"). RoBERTa-family models
        // (UniXcoder, CodeBERT, XLM-R) don't accept this input at all —
        // session() detects which family the loaded model is and we skip
        // building the type tensor when the session doesn't expect it.
        let mut session_guard = self.session()?;
        let session = session_guard
            .as_mut()
            .expect("session() guarantees initialized after Ok return");
        let expects_tti = self
            .expects_token_type_ids
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unwrap_or(true); // session() always sets this; fallback to true matches BERT default

        let ids_arr = pad_2d_i64(&input_ids, max_len, 0);
        let mask_arr = pad_2d_i64(&attention_mask, max_len, 0);

        use ort::value::Tensor;
        let ids_tensor = Tensor::from_array(ids_arr).map_err(ort_err)?;
        let mask_tensor = Tensor::from_array(mask_arr).map_err(ort_err)?;

        let outputs = if expects_tti {
            let token_type_ids: Vec<Vec<i64>> = chunk
                .iter()
                .map(|e| e.get_type_ids().iter().map(|&t| t as i64).collect())
                .collect();
            let type_arr = pad_2d_i64(&token_type_ids, max_len, 0);
            let type_tensor = Tensor::from_array(type_arr).map_err(ort_err)?;
            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                    "token_type_ids" => type_tensor,
                ])
                .map_err(ort_err)?
        } else {
            session
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "attention_mask" => mask_tensor,
                ])
                .map_err(ort_err)?
        };

        // Extract logits, apply sigmoid.
        if outputs.len() == 0 {
            return Err(RerankerError::Inference(
                "ONNX model produced no outputs".to_string(),
            ));
        }
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;

        // AC-V1.29-6: ORT's `shape[1]` is `i64` and can be -1 when a
        // dynamic axis is unbound (or, in principle, any negative value
        // the model exporter emits). Casting `-1 as usize` gives
        // `usize::MAX` — the subsequent `batch_size * stride` then wraps,
        // `data.len() < expected_len` flips direction, and we read past
        // the buffer. Guard the cast first, then use `checked_mul` so a
        // large legitimate stride can't silently overflow either.
        let stride = if shape.len() == 2 {
            let dim = shape[1];
            if dim < 0 {
                return Err(RerankerError::Inference(format!(
                    "Model returned negative output dim {dim} (dynamic axis not bound?)"
                )));
            }
            dim as usize
        } else {
            1
        };
        if stride == 0 {
            return Err(RerankerError::Inference(
                "Model returned zero-width output tensor".to_string(),
            ));
        }
        let expected_len = batch_size.checked_mul(stride).ok_or_else(|| {
            RerankerError::Inference(format!(
                "Reranker output too large: batch_size={batch_size} * stride={stride} overflows usize"
            ))
        })?;
        if data.len() < expected_len {
            return Err(RerankerError::Inference(format!(
                "Model output too short: expected {} elements, got {}",
                expected_len,
                data.len()
            )));
        }

        let scores: Vec<f32> = (0..batch_size).map(|i| sigmoid(data[i * stride])).collect();
        Ok(scores)
    }

    /// Like [`compute_scores_opt`] but returns an empty vec instead of `None`
    /// when tokenization produces zero-length encodings. Used by [`rerank`]
    /// where a degenerate empty input just means a no-op.
    fn compute_scores(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>, RerankerError> {
        if passages.len() <= 1 {
            return Ok(Vec::new());
        }
        Ok(self
            .compute_scores_opt(query, passages)?
            .unwrap_or_default())
    }

    /// Resolve paths to `model.onnx` and `tokenizer.json`.
    ///
    /// Delegates to [`resolve_reranker`] for precedence handling (CLI → env
    /// → TOML `[reranker] model_path` → TOML `[reranker] preset` → hardcoded
    /// default). When the resolver returns a local-path config, the files
    /// are used directly; when it returns an HF repo id, the Hub API fetches
    /// the bundle.
    ///
    /// Local-bundle layout (shared with [`crate::aux_model`]):
    /// `{dir}/onnx/model.onnx` + `{dir}/tokenizer.json` — matches the
    /// HuggingFace cross-encoder repo layout so an unpacked HF checkout
    /// works without surgery.
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), RerankerError> {
        self.model_paths.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_model_resolve").entered();

            let resolved = resolve_reranker(None)?;

            // Local-bundle branch: resolver already verified the directory
            // existed when the override was path-like. For preset/default
            // cases, `repo` is set and we go through the Hub API below.
            if resolved.repo.is_none() {
                let model_path = resolved.model_path;
                let tokenizer_path = resolved.tokenizer_path;
                if !model_path.exists() || !tokenizer_path.exists() {
                    return Err(RerankerError::ModelDownload(format!(
                        "local reranker bundle missing {} or {} (model_path = {})",
                        MODEL_FILE,
                        TOKENIZER_FILE,
                        model_path.display()
                    )));
                }
                tracing::info!(
                    path = %model_path.display(),
                    preset = ?resolved.preset,
                    "Using local reranker model (no HF download)"
                );
                return Ok((model_path, tokenizer_path));
            }

            let repo_id = resolved
                .repo
                .as_deref()
                .expect("repo.is_some() checked above");
            use hf_hub::api::sync::Api;
            let api = Api::new().map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let repo = api.model(repo_id.to_string());

            let model_path = repo
                .get(MODEL_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let tokenizer_path = repo
                .get(TOKENIZER_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;

            // Verify checksums (skip if already verified via marker file)
            if !MODEL_BLAKE3.is_empty() || !TOKENIZER_BLAKE3.is_empty() {
                let marker = model_path
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .join(".cqs_reranker_verified");
                let expected_marker = format!("{}\n{}", MODEL_BLAKE3, TOKENIZER_BLAKE3);
                let already_verified = std::fs::read_to_string(&marker)
                    .map(|s| s == expected_marker)
                    .unwrap_or(false);

                if !already_verified {
                    if !MODEL_BLAKE3.is_empty() {
                        verify_checksum(&model_path, MODEL_BLAKE3)?;
                    }
                    if !TOKENIZER_BLAKE3.is_empty() {
                        verify_checksum(&tokenizer_path, TOKENIZER_BLAKE3)?;
                    }
                    // Write marker after successful verification
                    let _ = std::fs::write(&marker, &expected_marker);
                }
            }

            tracing::info!(model = %model_path.display(), "Reranker model ready");
            Ok((model_path, tokenizer_path))
        })
    }

    /// Get or initialize the ONNX session
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Option<Session>>, RerankerError> {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let _span = tracing::info_span!("reranker_session_init").entered();
            let (model_path, _) = self.model_paths()?;
            let session = create_session(model_path, self.provider)
                .map_err(|e| RerankerError::Inference(e.to_string()))?;
            // Inspect input names so run_chunk knows whether to send
            // token_type_ids. BERT-family expects it; RoBERTa-family
            // (UniXcoder, CodeBERT, XLM-R) doesn't.
            let has_tti = session
                .inputs()
                .iter()
                .any(|i| i.name() == "token_type_ids");
            *self
                .expects_token_type_ids
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(has_tti);
            tracing::info!(
                expects_token_type_ids = has_tti,
                "Reranker session initialized"
            );
            *guard = Some(session);
        }
        Ok(guard)
    }

    /// Clear the ONNX session to free memory (~91MB model).
    ///
    /// Session re-initializes lazily on next `rerank()` call.
    /// Use this during idle periods in long-running processes.
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        // Reset the input-shape probe so the next session re-detects
        // the loaded model's token_type_ids contract.
        *self
            .expects_token_type_ids
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
        // RM-V1.25-15: Drop the tokenizer too (~20MB for ms-marco MiniLM).
        // In-flight rerank() calls that grabbed an Arc clone before this
        // call keep their own copy; the slot is cleared and lazy-reloads
        // on next tokenizer() access.
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        *tok = None;
        tracing::info!("Reranker session and tokenizer cleared");
    }

    /// Get or initialize the tokenizer.
    ///
    /// RM-V1.25-15: Returns `Arc<Tokenizer>` so callers drop the mutex
    /// before running inference and `clear_session` can replace the inner
    /// slot without racing against encode.
    fn tokenizer(&self) -> Result<Arc<tokenizers::Tokenizer>, RerankerError> {
        {
            let guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = guard.as_ref() {
                return Ok(Arc::clone(t));
            }
        }
        let (_, tokenizer_path) = self.model_paths()?;
        let _span = tracing::info_span!("reranker_tokenizer_init").entered();
        let loaded = Arc::new(
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| RerankerError::Tokenizer(e.to_string()))?,
        );
        let mut guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        *guard = Some(Arc::clone(&loaded));
        Ok(loaded)
    }
}

/// Verify file checksum using blake3
fn verify_checksum(path: &std::path::Path, expected: &str) -> Result<(), RerankerError> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        RerankerError::ModelDownload(format!("Cannot open {}: {}", path.display(), e))
    })?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| {
        RerankerError::ModelDownload(format!("Read error on {}: {}", path.display(), e))
    })?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        return Err(RerankerError::ChecksumMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Computes the sigmoid activation function.
///
/// The sigmoid function maps any input value to a range between 0 and 1, making it useful for neural networks and probability calculations. It is defined as 1 / (1 + e^(-x)).
///
/// # Arguments
///
/// * `x` - The input value
///
/// # Returns
///
/// The sigmoid of x, a value in the range (0, 1)
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Apply freshly computed cross-encoder scores, then sort and truncate.
///
/// When `scores` is empty, leaves `results` unchanged (used for the degenerate
/// paths in [`Reranker::rerank`]). Otherwise, writes each score onto the
/// corresponding result, sorts descending with a chunk-id secondary key for
/// deterministic tie-breaking, and truncates to `limit`.
///
/// PF-V1.25-5: extracted from the impl block so the &mut results write and
/// the earlier &results passage borrow live in disjoint scopes at the call
/// site (`rerank`).
fn apply_rerank_scores(results: &mut Vec<SearchResult>, scores: Vec<f32>, limit: usize) {
    if scores.is_empty() {
        return;
    }
    let n = scores.len().min(results.len());
    for (i, score) in scores.into_iter().take(n).enumerate() {
        results[i].score = score;
    }
    let batch_size = results.len();
    // 5. Sort descending by score, truncate. Secondary sort on chunk id keeps
    // equal-score candidates deterministically ordered so the truncate()
    // drops the same candidates on every invocation.
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.chunk.id.cmp(&b.chunk.id))
    });
    results.truncate(limit);
    tracing::info!(reranked = results.len(), batch_size, "Re-ranking complete");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sigmoid_zero() {
        let result = sigmoid(0.0);
        assert!((result - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_sigmoid_large_positive() {
        let result = sigmoid(10.0);
        assert!(result > 0.999);
    }

    #[test]
    fn test_sigmoid_large_negative() {
        let result = sigmoid(-10.0);
        assert!(result < 0.001);
    }

    #[test]
    fn test_sigmoid_extreme_negative() {
        // Should not panic or produce NaN
        let result = sigmoid(-100.0);
        assert!(result >= 0.0 && result.is_finite());
    }

    #[test]
    fn test_sigmoid_nan_does_not_panic() {
        // TC-1: If the model returns NaN logits, sigmoid should not panic.
        // NaN propagates through arithmetic, producing NaN output.
        // The reranker's total_cmp sort handles NaN (sorts to end).
        let result = sigmoid(f32::NAN);
        assert!(result.is_nan(), "sigmoid(NaN) should be NaN, got {result}");
    }

    #[test]
    fn test_sigmoid_infinity_does_not_panic() {
        let pos = sigmoid(f32::INFINITY);
        assert!(
            pos.is_finite() || pos.is_nan(),
            "sigmoid(+inf) should not panic"
        );
        let neg = sigmoid(f32::NEG_INFINITY);
        assert!(
            neg.is_finite() || neg.is_nan(),
            "sigmoid(-inf) should not panic"
        );
    }

    #[test]
    fn test_reranker_new() {
        // Construction should succeed (no model download yet — lazy)
        let reranker = Reranker::new();
        assert!(reranker.is_ok());
    }

    #[test]
    fn test_rerank_empty_results() {
        let reranker = Reranker::new().unwrap();
        let mut results = Vec::new();
        let result = reranker.rerank("test query", &mut results, 10);
        assert!(result.is_ok());
        assert!(results.is_empty());
    }
}
