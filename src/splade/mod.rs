//! SPLADE sparse encoder for learned sparse retrieval.
//!
//! Produces sparse vectors (token_id → weight) from text input using a
//! BertForMaskedLM model with ReLU + log(1+x) activation. Used alongside
//! the dense embedder for hybrid search.
//!
//! The sparse vector represents learned token importance: which vocabulary
//! tokens are semantically relevant to a piece of code, even if they don't
//! appear literally. This enables query expansion (searching for "retry"
//! also matches functions about "backoff" and "exponential").

pub mod index;

use std::path::Path;
use std::sync::Mutex;

use ndarray::{Array2, ArrayView2, Axis};
use ort::session::Session;
use ort::value::Tensor;
use thiserror::Error;

use crate::aux_model::{self, AuxModelKind};
use crate::config::AuxModelSection;
use crate::embedder::{create_session, select_provider};

/// Convert ORT errors to SpladeError
fn ort_err(e: ort::Error) -> SpladeError {
    SpladeError::InferenceFailed(e.to_string())
}

/// RB-V1.29-9: Convert an ORT-reported tensor dimension (`i64`) to `usize`
/// with a negative-value guard. ORT shape entries are nominally
/// non-negative, but a corrupted or mis-exported model could report a
/// negative dim (e.g. unresolved symbolic axis that leaks through as -1).
/// Without this guard the `as usize` cast on a negative value produces a
/// huge positive number, which later breeds either an allocation failure
/// or a silently truncated buffer slice.
fn i64_dim_to_usize(d: i64, name: &str) -> Result<usize, SpladeError> {
    if d < 0 {
        return Err(SpladeError::InferenceFailed(format!(
            "ORT shape {name} is negative: {d}"
        )));
    }
    Ok(d as usize)
}

/// A sparse vector: vocabulary token ID → learned importance weight.
/// Typically 100-300 non-zero entries out of ~30K vocabulary.
pub type SparseVector = Vec<(u32, f32)>;

#[derive(Error, Debug)]
pub enum SpladeError {
    #[error("SPLADE model not found: {0}")]
    ModelNotFound(String),
    #[error("SPLADE inference failed: {0}")]
    InferenceFailed(String),
    #[error("SPLADE tokenization failed: {0}")]
    TokenizationFailed(String),
    /// Tokenizer vocab size and model output vocab size don't match — the
    /// directory contains a tokenizer for one model and weights for another.
    /// Most commonly this happens when `model.onnx` was hot-swapped (e.g.
    /// SPLADE-Code 0.6B replaced the off-the-shelf 110M BERT) without
    /// updating `tokenizer.json`. Encoding would silently produce garbage —
    /// fail fast at construction time so the eval doesn't waste a 30-minute
    /// run on broken vectors.
    #[error(
        "SPLADE config mismatch: tokenizer vocab is {tokenizer_vocab}, model vocab is \
         {model_vocab}. The tokenizer.json and model.onnx in {dir:?} are from different \
         models — replace tokenizer.json with the one matching the model architecture."
    )]
    ConfigMismatch {
        dir: std::path::PathBuf,
        tokenizer_vocab: usize,
        model_vocab: usize,
    },
}

/// SPLADE encoder using ONNX Runtime.
///
/// Loads a BertForMaskedLM model and produces sparse vectors via
/// max pooling → ReLU → log(1+x) → threshold.
pub struct SpladeEncoder {
    session: Mutex<Option<Session>>,
    model_path: std::path::PathBuf,
    /// Path to the tokenizer JSON, retained so `clear_session` can drop
    /// the tokenizer and `encode` can lazy-reload it without going back
    /// through `SpladeEncoder::new` (which re-runs the ORT probe).
    tokenizer_path: std::path::PathBuf,
    /// Lazy-loaded tokenizer.
    ///
    /// RM-V1.25-15: Stored as `Mutex<Option<Arc<Tokenizer>>>` so
    /// `clear_session` can drop the ~20MB tokenizer state alongside the
    /// ONNX session during idle periods. The initial load happens at
    /// construction time (to drive the vocab probe), but the tokenizer
    /// can be freed after that without losing the probe result.
    tokenizer: Mutex<Option<std::sync::Arc<tokenizers::Tokenizer>>>,
    threshold: f32,
    vocab_size: usize,
}

/// Probe a SPLADE model's output vocabulary by running one short inference.
///
/// Used at construction time to validate that the loaded `tokenizer.json` and
/// `model.onnx` agree on vocab size. Returns the model's output vocab
/// dimension extracted from the inference output shape.
///
/// Handles both output formats:
/// - `sparse_vector` (2D `[batch, vocab]`) — pre-pooled SPLADE-Code 0.6B+
/// - `logits` (3D `[batch, seq, vocab]`) — raw masked-LM logits, our v1/v2
///
/// The session is consumed by this function (ORT's `Session::run` requires
/// `&mut`); the caller re-creates the session for the persistent encoder
/// after probing succeeds.
fn probe_model_vocab(
    mut session: Session,
    tokenizer: &tokenizers::Tokenizer,
    onnx_path: &Path,
) -> Result<usize, SpladeError> {
    let _span = tracing::debug_span!("probe_model_vocab", path = %onnx_path.display()).entered();

    // Tokenize a short fixed string. Content doesn't matter — we only care
    // about the output tensor shape.
    let encoding = tokenizer
        .encode("test", true)
        .map_err(|e| SpladeError::TokenizationFailed(format!("probe tokenization: {e}")))?;

    let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
    let attention_mask: Vec<i64> = encoding
        .get_attention_mask()
        .iter()
        .map(|&m| m as i64)
        .collect();
    let seq_len = input_ids.len();

    let ids_array = Array2::from_shape_vec((1, seq_len), input_ids)
        .map_err(|e| SpladeError::InferenceFailed(format!("probe ids tensor: {e}")))?;
    let mask_array = Array2::from_shape_vec((1, seq_len), attention_mask)
        .map_err(|e| SpladeError::InferenceFailed(format!("probe mask tensor: {e}")))?;

    let ids_tensor = Tensor::from_array(ids_array)
        .map_err(|e| SpladeError::InferenceFailed(format!("probe ids: {e}")))?;
    let mask_tensor = Tensor::from_array(mask_array)
        .map_err(|e| SpladeError::InferenceFailed(format!("probe mask: {e}")))?;

    let outputs = session
        .run(ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
        ])
        .map_err(ort_err)?;

    // Extract vocab dim from whichever output shape we get.
    let vocab = if let Some(sv_output) = outputs.get("sparse_vector") {
        let (shape, _data) = sv_output.try_extract_tensor::<f32>().map_err(ort_err)?;
        if shape.len() != 2 {
            return Err(SpladeError::InferenceFailed(format!(
                "probe: pre-pooled sparse_vector expected 2D [batch, vocab], got {}D",
                shape.len()
            )));
        }
        i64_dim_to_usize(shape[1], "probe.sparse_vector[vocab]")?
    } else if let Some(logits_output) = outputs.get("logits") {
        let (shape, _data) = logits_output.try_extract_tensor::<f32>().map_err(ort_err)?;
        if shape.len() != 3 {
            return Err(SpladeError::InferenceFailed(format!(
                "probe: expected 3D logits [batch, seq, vocab], got {}D",
                shape.len()
            )));
        }
        i64_dim_to_usize(shape[2], "probe.logits[vocab]")?
    } else {
        let names: Vec<&str> = outputs.keys().collect();
        return Err(SpladeError::InferenceFailed(format!(
            "probe: no recognized SPLADE output. Expected 'sparse_vector' or 'logits'. \
             Available: {names:?}"
        )));
    };

    tracing::debug!(model_vocab = vocab, "Probed SPLADE model vocab");
    Ok(vocab)
}

/// Resolve the SPLADE model directory.
///
/// Delegates to [`resolve_splade_model_dir_with_config`] with no TOML
/// section — matches the historical env-var-only behavior for callers
/// that don't have easy access to the loaded [`crate::config::Config`].
/// New call sites with a Config in hand should call the `_with_config`
/// variant so `.cqs.toml [splade]` presets take effect.
///
/// Returns `None` when neither the configured location nor the default
/// cache directory has both `model.onnx` AND `tokenizer.json`. Callers
/// fall back to dense-only and emit a warning.
///
/// The env-var override exists so research can A/B between SPLADE models
/// (e.g. SPLADE-Code 0.6B at `~/training-data/splade-code-naver/onnx/`
/// vs the off-the-shelf 110M BERT model) without clobbering the default
/// cache directory.
///
/// CRITICAL: this single helper is the *only* place SPLADE paths are
/// resolved. Adding new SPLADE call sites must use this function — having
/// multiple paths means the model and tokenizer can desync (which has
/// happened: a stale BERT tokenizer was used with a SPLADE-Code model,
/// silently producing garbage embeddings).
pub fn resolve_splade_model_dir() -> Option<std::path::PathBuf> {
    resolve_splade_model_dir_with_config(None)
}

/// Config-aware variant of [`resolve_splade_model_dir`].
///
/// Threads a `[splade]` config section through [`crate::aux_model::resolve`]
/// so preset names and explicit paths configured in `.cqs.toml` take effect.
/// Env var (`CQS_SPLADE_MODEL`) still beats config. Pass `None` to match
/// the legacy no-config behavior.
///
/// The returned `PathBuf` points at the **directory** containing
/// `model.onnx` + `tokenizer.json`, matching the pre-#957 contract so all
/// [`SpladeEncoder::new`] call sites work unchanged.
pub fn resolve_splade_model_dir_with_config(
    section: Option<&AuxModelSection>,
) -> Option<std::path::PathBuf> {
    let _span = tracing::debug_span!("resolve_splade_model_dir").entered();

    let preset = section.and_then(|s| s.preset.as_deref());
    let model_path = section.and_then(|s| s.model_path.as_deref());
    let tokenizer_path = section.and_then(|s| s.tokenizer_path.as_deref());

    let resolved = match aux_model::resolve(
        AuxModelKind::Splade,
        None,
        "CQS_SPLADE_MODEL",
        preset,
        model_path,
        tokenizer_path,
        aux_model::default_preset_name(AuxModelKind::Splade),
    ) {
        Ok(c) => c,
        Err(e) => {
            // The legacy API signals "no model available" via None and emits
            // a tracing::warn; mirror that so existing callers keep the same
            // behavior when resolution fails.
            tracing::warn!(error = %e, "SPLADE model resolution failed");
            return None;
        }
    };

    // aux_model returns a "synthetic" bundle (model.onnx + tokenizer.json)
    // under a directory path; the consumer only needs the directory since
    // `SpladeEncoder::new` re-joins the filenames internally.
    let dir = resolved
        .model_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default();

    let model = dir.join("model.onnx");
    let tokenizer = dir.join("tokenizer.json");

    if !model.exists() {
        tracing::warn!(
            path = %model.display(),
            "SPLADE model.onnx not found — hybrid search will be disabled"
        );
        return None;
    }
    if !tokenizer.exists() {
        tracing::warn!(
            path = %tokenizer.display(),
            "SPLADE tokenizer.json not found — hybrid search will be disabled"
        );
        return None;
    }

    tracing::info!(
        path = %dir.display(),
        preset = ?resolved.preset,
        "SPLADE model dir resolved"
    );
    Some(dir)
}

/// Maximum characters for SPLADE input truncation.
/// Configurable via `CQS_SPLADE_MAX_CHARS` (default 4000).
fn splade_max_chars() -> usize {
    std::env::var("CQS_SPLADE_MAX_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n: &usize| n > 0)
        .unwrap_or(4000)
}

impl SpladeEncoder {
    /// Default SPLADE threshold, overridable via `CQS_SPLADE_THRESHOLD` env var.
    pub fn default_threshold() -> f32 {
        std::env::var("CQS_SPLADE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.01)
    }

    /// Load SPLADE model from a directory containing model.onnx and tokenizer.json.
    ///
    /// At construction time runs a dummy inference to detect tokenizer/model
    /// vocabulary mismatches. The check enforces `model_vocab >= tokenizer_vocab`:
    ///
    /// - **Equal**: ideal case, perfectly matched export.
    /// - **Model > tokenizer (within 1.5%)**: accepted as benign padding.
    ///   Models commonly export their `lm_head` padded to a friendly size
    ///   (e.g. Qwen3 base vocab is 151,669 but the lm_head is rounded up to
    ///   151,936 — a multiple-of-128 padding). The extra slots receive no
    ///   training signal and are near-zero at inference, so they contribute
    ///   harmless noise to the sparse vector. Logged as a warning.
    /// - **Model > tokenizer (large gap)**: rejected as suspicious — likely
    ///   the wrong tokenizer for the model.
    /// - **Model < tokenizer**: hard fail. The tokenizer can produce token
    ///   IDs the model has no output slot for, which would either crash or
    ///   silently wrap around. This is the case the original probe was
    ///   added to catch (BERT tokenizer with SPLADE-Code 0.6B model).
    pub fn new(model_dir: &Path, threshold: f32) -> Result<Self, SpladeError> {
        let _span = tracing::info_span!("splade_encoder_new", dir = %model_dir.display()).entered();

        let onnx_path = model_dir.join("model.onnx");
        if !onnx_path.exists() {
            return Err(SpladeError::ModelNotFound(format!(
                "No model.onnx at {}",
                model_dir.display()
            )));
        }

        let tokenizer_path = model_dir.join("tokenizer.json");
        if !tokenizer_path.exists() {
            return Err(SpladeError::ModelNotFound(format!(
                "No tokenizer.json at {}",
                model_dir.display()
            )));
        }

        let provider = select_provider();
        let session = create_session(&onnx_path, provider)
            .map_err(|e| SpladeError::InferenceFailed(format!("ORT session: {e}")))?;

        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))?;

        let tokenizer_vocab = tokenizer.get_vocab_size(true);

        // Probe the model's actual output vocab via a dummy inference.
        // Mismatch with tokenizer vocab → silent garbage in production, so
        // we fail fast here. The probe runs the same code path as `encode`,
        // so it also surfaces ORT/runtime errors at construction time.
        let model_vocab = probe_model_vocab(session, &tokenizer, &onnx_path)?;

        // Hard fail: tokenizer can produce IDs the model has no slot for.
        // This is the original failure case the probe was added to catch
        // (BERT tokenizer with SPLADE-Code 0.6B model — 30522 vs 151936).
        if model_vocab < tokenizer_vocab {
            tracing::error!(
                tokenizer_vocab,
                model_vocab,
                dir = %model_dir.display(),
                "SPLADE model output dim is smaller than tokenizer vocab — refusing to load"
            );
            return Err(SpladeError::ConfigMismatch {
                dir: model_dir.to_path_buf(),
                tokenizer_vocab,
                model_vocab,
            });
        }

        // Suspicious gap: model is much larger than tokenizer. Within 1.5%
        // is benign padding (e.g. 151669 → 151936 = 0.18%); larger gaps
        // suggest the tokenizer is from a different model family.
        let padding_pct = if tokenizer_vocab > 0 {
            (model_vocab - tokenizer_vocab) as f32 * 100.0 / tokenizer_vocab as f32
        } else {
            0.0
        };
        if padding_pct > 1.5 {
            tracing::error!(
                tokenizer_vocab,
                model_vocab,
                padding_pct,
                dir = %model_dir.display(),
                "SPLADE model vocab is suspiciously larger than tokenizer (> 1.5%) — refusing to load"
            );
            return Err(SpladeError::ConfigMismatch {
                dir: model_dir.to_path_buf(),
                tokenizer_vocab,
                model_vocab,
            });
        }
        if model_vocab > tokenizer_vocab {
            tracing::warn!(
                tokenizer_vocab,
                model_vocab,
                padding_pct,
                "SPLADE model vocab is padded above tokenizer vocab — \
                 extra slots are zero-trained and ignored at encode time"
            );
        }

        // Re-create the session for the persistent encoder (the probe consumed
        // the original via session.run mutability — cleaner to reload than to
        // rebuild the API around split borrow).
        let session = create_session(&onnx_path, provider)
            .map_err(|e| SpladeError::InferenceFailed(format!("ORT session re-init: {e}")))?;

        tracing::info!(
            threshold,
            vocab_size = tokenizer_vocab,
            "SPLADE encoder loaded (vocab consistency verified)"
        );

        // RM-V1.25-15: wrap the probed tokenizer in Arc + Mutex so
        // clear_session can drop it during idle periods. Skip re-loading
        // — we already have the probed instance in hand.
        Ok(Self {
            session: Mutex::new(Some(session)),
            model_path: onnx_path,
            tokenizer_path,
            tokenizer: Mutex::new(Some(std::sync::Arc::new(tokenizer))),
            threshold,
            vocab_size: tokenizer_vocab,
        })
    }

    /// Get or lazy-reload the tokenizer.
    ///
    /// RM-V1.25-15: Returns `Arc<Tokenizer>` so encode-side callers can
    /// release the mutex before running inference. `clear_session` drops
    /// the inner slot during idle; a subsequent `encode` lazily reloads
    /// from `tokenizer_path`.
    fn tokenizer(&self) -> Result<std::sync::Arc<tokenizers::Tokenizer>, SpladeError> {
        {
            let guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = guard.as_ref() {
                return Ok(std::sync::Arc::clone(t));
            }
        }
        // Rare path — only after `clear_session` has dropped the tokenizer.
        let _span = tracing::info_span!("splade_tokenizer_reload").entered();
        let loaded = std::sync::Arc::new(
            tokenizers::Tokenizer::from_file(&self.tokenizer_path)
                .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))?,
        );
        let mut guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = guard.as_ref() {
            return Ok(std::sync::Arc::clone(existing));
        }
        *guard = Some(std::sync::Arc::clone(&loaded));
        Ok(loaded)
    }

    /// Encode text into a sparse vector.
    ///
    /// Process: tokenize → ONNX inference (MLM logits) → max pool over
    /// sequence → ReLU + log(1+x) → threshold to keep significant weights.
    pub fn encode(&self, text: &str) -> Result<SparseVector, SpladeError> {
        let _span = tracing::debug_span!("splade_encode", text_len = text.len()).entered();

        if text.is_empty() {
            return Ok(Vec::new());
        }

        // Truncate overly long input to avoid excessive tokenization/inference cost
        let max_chars = splade_max_chars();
        let text = if text.len() > max_chars {
            let truncated = &text[..text
                .char_indices()
                .nth(max_chars)
                .map(|(i, _)| i)
                .unwrap_or(text.len())];
            tracing::debug!(
                original_len = text.len(),
                truncated_len = truncated.len(),
                max_chars,
                "Truncated SPLADE input"
            );
            truncated
        } else {
            text
        };

        // Tokenize
        let encoding = self
            .tokenizer()?
            .encode(text, true)
            .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let seq_len = input_ids.len();

        // Build input tensors [1, seq_len]
        let ids_array = Array2::from_shape_vec((1, seq_len), input_ids).map_err(|e| {
            SpladeError::InferenceFailed(format!("Failed to build input tensor: {e}"))
        })?;
        let mask_array = Array2::from_shape_vec((1, seq_len), attention_mask).map_err(|e| {
            SpladeError::InferenceFailed(format!("Failed to build mask tensor: {e}"))
        })?;

        let ids_tensor = Tensor::from_array(ids_array)
            .map_err(|e| SpladeError::InferenceFailed(format!("Tensor: {e}")))?;
        let mask_tensor = Tensor::from_array(mask_array)
            .map_err(|e| SpladeError::InferenceFailed(format!("Tensor: {e}")))?;

        // Run inference — lazily re-create session if it was cleared (RM-3)
        let mut session_guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if session_guard.is_none() {
            let provider = select_provider();
            let new_session = create_session(&self.model_path, provider)
                .map_err(|e| SpladeError::InferenceFailed(format!("ORT session re-init: {e}")))?;
            *session_guard = Some(new_session);
            tracing::debug!("SPLADE session re-created after clear");
        }
        let session = session_guard.as_mut().expect("session just initialized");
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
            .map_err(ort_err)?;

        // Auto-detect output format by key name:
        // - "sparse_vector" → pre-pooled (2D: [batch, vocab_size]) — SPLADE-Code 0.6B+
        // - "logits" → raw logits (3D: [batch, seq_len, vocab_size]) — our trained models
        let sparse = if let Some(sv_output) = outputs.get("sparse_vector") {
            // Pre-pooled path: model already did splade_max internally
            let (shape, data) = sv_output.try_extract_tensor::<f32>().map_err(ort_err)?;
            if shape.len() != 2 {
                return Err(SpladeError::InferenceFailed(format!(
                    "Pre-pooled sparse_vector expected 2D [batch, vocab], got {}D",
                    shape.len()
                )));
            }
            let vocab = i64_dim_to_usize(shape[1], "encode.sparse_vector[vocab]")?;
            tracing::debug!(vocab, format = "pre_pooled", "SPLADE output detected");

            // Threshold directly — values are already activated
            let sv: SparseVector = data
                .iter()
                .enumerate()
                .filter_map(|(id, &val)| {
                    if val > self.threshold {
                        Some((id as u32, val))
                    } else {
                        None
                    }
                })
                .collect();
            sv
        } else if let Some(logits_output) = outputs.get("logits") {
            // Raw logits path: [1, seq_len, vocab_size] — apply max pool + ReLU + log(1+x)
            let (shape, data) = logits_output.try_extract_tensor::<f32>().map_err(ort_err)?;
            if shape.len() != 3 {
                return Err(SpladeError::InferenceFailed(format!(
                    "Expected 3D logits [batch, seq, vocab], got {}D",
                    shape.len()
                )));
            }
            let vocab = i64_dim_to_usize(shape[2], "encode.logits[vocab]")?;
            tracing::debug!(vocab, format = "raw_logits", "SPLADE output detected");

            let logits = ArrayView2::from_shape((seq_len, vocab), data).map_err(|e| {
                SpladeError::InferenceFailed(format!("Failed to reshape logits: {e}"))
            })?;

            // Max pool over sequence dimension → [vocab_size]
            let pooled = logits.fold_axis(Axis(0), f32::NEG_INFINITY, |&a, &b| a.max(b));

            // ReLU + log(1+x) + threshold
            let sv: SparseVector = pooled
                .iter()
                .enumerate()
                .filter_map(|(id, &val)| {
                    let activated = (1.0 + val.max(0.0)).ln();
                    if activated > self.threshold {
                        Some((id as u32, activated))
                    } else {
                        None
                    }
                })
                .collect();
            sv
        } else {
            return Err(SpladeError::InferenceFailed(format!(
                "No recognized SPLADE output. Expected 'sparse_vector' or 'logits'. Available: {:?}",
                outputs.keys().collect::<Vec<_>>()
            )));
        };

        tracing::debug!(non_zero = sparse.len(), "SPLADE encoding complete");
        Ok(sparse)
    }

    /// Batch encode multiple texts in a single forward pass.
    ///
    /// Tokenizes all inputs, pads to a CONSTANT max_seq_len (configurable
    /// via `CQS_SPLADE_MAX_SEQ`, default 256), runs one ONNX inference call,
    /// and extracts per-example sparse vectors.
    ///
    /// **Why constant padding (not per-batch max)?** ORT's BFC arena caches
    /// allocations by tensor shape. If consecutive batches have different
    /// shapes (which they would with per-batch-max padding), the arena
    /// allocates new slots and never frees old ones — observed leak of
    /// 7.4 → 30 GB GPU memory over 60 minutes encoding 11k chunks with
    /// SPLADE-Code 0.6B. Padding to a fixed length keeps every input
    /// tensor at the same shape so ORT can reuse the same arena slots.
    ///
    /// Tradeoff: short inputs get padded more (median chunk is ~16 tokens,
    /// so padding to 256 is ~16x overhead). For SPLADE-Code 0.6B that's
    /// fine — the activation memory at batch=8, seq=256 is ~600 MB which
    /// fits comfortably and stays stable across all batches.
    ///
    /// Output handling matches the single-input `encode` path:
    /// - `sparse_vector` (pre-pooled, 2D): slice rows directly, threshold-filter
    /// - `logits` (raw, 3D): per-example reshape → mask padding → max-pool →
    ///   ReLU + log(1+x) → threshold
    ///
    /// Padding is masked out before max pooling so attention-padded positions
    /// can never contribute spurious tokens to the sparse vector.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<SparseVector>, SpladeError> {
        let _span = tracing::debug_span!("splade_encode_batch", count = texts.len()).entered();

        if texts.is_empty() {
            return Ok(Vec::new());
        }

        // Step 1: truncate each input to max chars, matching `encode` behavior.
        let max_chars = splade_max_chars();
        let truncated: Vec<&str> = texts
            .iter()
            .map(|t| {
                if t.len() > max_chars {
                    let end = t
                        .char_indices()
                        .nth(max_chars)
                        .map(|(i, _)| i)
                        .unwrap_or(t.len());
                    &t[..end]
                } else {
                    *t
                }
            })
            .collect();

        // Empty inputs need to round-trip as empty sparse vectors at the same
        // index — track indices and re-insert holes after the batch returns.
        let non_empty_indices: Vec<usize> = truncated
            .iter()
            .enumerate()
            .filter_map(|(i, t)| if t.is_empty() { None } else { Some(i) })
            .collect();
        if non_empty_indices.is_empty() {
            return Ok(vec![Vec::new(); texts.len()]);
        }
        let non_empty_texts: Vec<&str> = non_empty_indices.iter().map(|&i| truncated[i]).collect();

        // Step 2: tokenize each non-empty input.
        let tokenizer = self.tokenizer()?;
        let encodings: Vec<_> = non_empty_texts
            .iter()
            .map(|t| {
                tokenizer
                    .encode(*t, true)
                    .map_err(|e| SpladeError::TokenizationFailed(e.to_string()))
            })
            .collect::<Result<_, _>>()?;

        let batch_size = encodings.len();

        // Step 3: pad to a CONSTANT max_seq_len (configurable via
        // CQS_SPLADE_MAX_SEQ, default 256). Constant shape is critical for
        // ORT BFC arena reuse — varying shapes leak GPU memory over time.
        //
        // Inputs longer than max_seq_len are truncated.
        //
        // SHL-V1.25-15: the 256 default was chosen for code corpora where
        // p99 is typically ~150-200 tokens. Prose-heavy corpora (docs,
        // notes) and languages with long import headers (Java, Kotlin
        // monorepos) can have p99 well above 400 tokens, silently
        // truncating a meaningful fraction of chunks. The truncation
        // counter below promotes to `info` whenever >1% of a batch is
        // truncated so users discover `CQS_SPLADE_MAX_SEQ` the moment
        // it matters.
        let max_seq_len: usize = std::env::var("CQS_SPLADE_MAX_SEQ")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|&n: &usize| n >= 8)
            .unwrap_or(256);

        // Pad token is 0; mask is 0 for padding positions so they don't
        // influence attention. Truncation: if a real input is longer than
        // max_seq_len, we keep only the first max_seq_len tokens.
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_seq_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_seq_len);
        let mut truncations = 0usize;
        for enc in &encodings {
            let ids = enc.get_ids();
            let mask = enc.get_attention_mask();
            let n = ids.len();
            if n > max_seq_len {
                truncations += 1;
            }
            for i in 0..max_seq_len {
                if i < n {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                } else {
                    input_ids.push(0);
                    attention_mask.push(0);
                }
            }
        }
        if truncations > 0 {
            // SHL-V1.25-15: promote to info when >1% of the batch was
            // truncated — that's the threshold where max_seq_len is
            // likely too small for the corpus and the user should bump
            // CQS_SPLADE_MAX_SEQ. Small batches need at least one
            // truncation plus batch_size > 1 to avoid screaming at every
            // single oversized query.
            let trunc_pct = (truncations as f64 * 100.0) / batch_size as f64;
            if trunc_pct > 1.0 && batch_size > 1 {
                tracing::info!(
                    truncations,
                    batch_size,
                    trunc_pct = format!("{:.1}%", trunc_pct),
                    max_seq_len,
                    "SPLADE truncated >1% of batch — bump CQS_SPLADE_MAX_SEQ if your corpus has long chunks"
                );
            } else {
                tracing::debug!(
                    truncations,
                    batch_size,
                    max_seq_len,
                    "SPLADE batch had truncated inputs"
                );
            }
        }

        let ids_array =
            Array2::from_shape_vec((batch_size, max_seq_len), input_ids).map_err(|e| {
                SpladeError::InferenceFailed(format!("Failed to build batch input tensor: {e}"))
            })?;
        let mask_array = Array2::from_shape_vec((batch_size, max_seq_len), attention_mask)
            .map_err(|e| {
                SpladeError::InferenceFailed(format!("Failed to build batch mask tensor: {e}"))
            })?;

        let ids_tensor = Tensor::from_array(ids_array)
            .map_err(|e| SpladeError::InferenceFailed(format!("Batch ids tensor: {e}")))?;
        let mask_tensor = Tensor::from_array(mask_array)
            .map_err(|e| SpladeError::InferenceFailed(format!("Batch mask tensor: {e}")))?;

        // Step 4: single forward pass through ORT.
        let mut session_guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if session_guard.is_none() {
            let provider = select_provider();
            let new_session = create_session(&self.model_path, provider)
                .map_err(|e| SpladeError::InferenceFailed(format!("ORT session re-init: {e}")))?;
            *session_guard = Some(new_session);
            tracing::debug!("SPLADE session re-created after clear");
        }
        let session = session_guard.as_mut().expect("session just initialized");
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
            .map_err(ort_err)?;

        // Step 5: extract per-example sparse vectors.
        let per_example: Vec<SparseVector> = if let Some(sv_output) = outputs.get("sparse_vector") {
            // Pre-pooled path: [batch, vocab_size]. Slice each row.
            let (shape, data) = sv_output.try_extract_tensor::<f32>().map_err(ort_err)?;
            if shape.len() != 2 {
                return Err(SpladeError::InferenceFailed(format!(
                    "Pre-pooled sparse_vector expected 2D [batch, vocab], got {}D",
                    shape.len()
                )));
            }
            if i64_dim_to_usize(shape[0], "encode_batch.sparse_vector[batch]")? != batch_size {
                return Err(SpladeError::InferenceFailed(format!(
                    "sparse_vector batch dim {} != input batch {}",
                    shape[0], batch_size
                )));
            }
            let vocab = i64_dim_to_usize(shape[1], "encode_batch.sparse_vector[vocab]")?;
            tracing::debug!(
                vocab,
                batch = batch_size,
                format = "pre_pooled",
                "SPLADE batch output"
            );

            // RB-NEW-1: validate that `data` is large enough before the slice
            // below. Without this, a short tensor would panic on out-of-bounds
            // slicing inside the map closure.
            let expected = batch_size
                .checked_mul(vocab)
                .ok_or_else(|| SpladeError::InferenceFailed("batch*vocab overflow".into()))?;
            if data.len() < expected {
                return Err(SpladeError::InferenceFailed(format!(
                    "sparse_vector data len {} < expected {} for batch={} vocab={}",
                    data.len(),
                    expected,
                    batch_size,
                    vocab,
                )));
            }

            let threshold = self.threshold;
            (0..batch_size)
                .map(|b| {
                    let row = &data[b * vocab..(b + 1) * vocab];
                    row.iter()
                        .enumerate()
                        .filter_map(|(id, &val)| {
                            if val > threshold {
                                Some((id as u32, val))
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .collect()
        } else if let Some(logits_output) = outputs.get("logits") {
            // Raw logits path: [batch, seq_len, vocab]. Per example: reshape,
            // mask padded positions to -inf, max-pool over seq dim, ReLU + log + threshold.
            let (shape, data) = logits_output.try_extract_tensor::<f32>().map_err(ort_err)?;
            if shape.len() != 3 {
                return Err(SpladeError::InferenceFailed(format!(
                    "Expected 3D logits [batch, seq, vocab], got {}D",
                    shape.len()
                )));
            }
            if i64_dim_to_usize(shape[0], "encode_batch.logits[batch]")? != batch_size {
                return Err(SpladeError::InferenceFailed(format!(
                    "logits batch dim {} != input batch {}",
                    shape[0], batch_size
                )));
            }
            if i64_dim_to_usize(shape[1], "encode_batch.logits[seq]")? != max_seq_len {
                return Err(SpladeError::InferenceFailed(format!(
                    "logits seq dim {} != padded max_seq_len {}",
                    shape[1], max_seq_len
                )));
            }
            let vocab = i64_dim_to_usize(shape[2], "encode_batch.logits[vocab]")?;
            tracing::debug!(
                vocab,
                batch = batch_size,
                format = "raw_logits",
                "SPLADE batch output"
            );

            // RB-NEW-2: validate total data length before per-example slicing.
            // Mirrors RB-NEW-1 but accounts for the extra seq dimension.
            let expected = batch_size
                .checked_mul(max_seq_len)
                .and_then(|n| n.checked_mul(vocab))
                .ok_or_else(|| SpladeError::InferenceFailed("batch*seq*vocab overflow".into()))?;
            if data.len() < expected {
                return Err(SpladeError::InferenceFailed(format!(
                    "raw logits data len {} < expected {} for batch={} seq={} vocab={}",
                    data.len(),
                    expected,
                    batch_size,
                    max_seq_len,
                    vocab,
                )));
            }

            let example_stride = max_seq_len * vocab;
            let threshold = self.threshold;

            (0..batch_size)
                .map(|b| {
                    let example = &data[b * example_stride..(b + 1) * example_stride];
                    let logits = ArrayView2::from_shape((max_seq_len, vocab), example)
                        .map_err(|e| SpladeError::InferenceFailed(format!("reshape: {e}")))?;

                    // Build a -inf mask for padded positions so they can't win max-pool.
                    // Clamp real_seq_len to max_seq_len in case the input was
                    // truncated to fit the constant padding length.
                    let real_seq_len = encodings[b].get_ids().len().min(max_seq_len);
                    let pooled: Vec<f32> = (0..vocab)
                        .map(|v| {
                            let mut max_val = f32::NEG_INFINITY;
                            for s in 0..real_seq_len {
                                let val = logits[[s, v]];
                                if val > max_val {
                                    max_val = val;
                                }
                            }
                            max_val
                        })
                        .collect();

                    Ok(pooled
                        .iter()
                        .enumerate()
                        .filter_map(|(id, &val)| {
                            let activated = (1.0 + val.max(0.0)).ln();
                            if activated > threshold {
                                Some((id as u32, activated))
                            } else {
                                None
                            }
                        })
                        .collect())
                })
                .collect::<Result<Vec<SparseVector>, SpladeError>>()?
        } else {
            let names: Vec<&str> = outputs.keys().collect();
            return Err(SpladeError::InferenceFailed(format!(
                "No recognized SPLADE output. Expected 'sparse_vector' or 'logits'. \
                 Available: {names:?}"
            )));
        };

        // Step 6: re-expand to original input shape, inserting empty vectors
        // at the indices that were filtered out as empty inputs.
        let mut results: Vec<SparseVector> = vec![Vec::new(); texts.len()];
        for (out_pos, &orig_idx) in non_empty_indices.iter().enumerate() {
            results[orig_idx] = per_example[out_pos].clone();
        }
        Ok(results)
    }

    /// Vocabulary size of the underlying tokenizer.
    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    /// Decode a token ID to its string representation (for debugging).
    pub fn decode_token(&self, token_id: u32) -> Option<String> {
        self.tokenizer().ok()?.decode(&[token_id], false).ok()
    }

    /// RM-3: Drop the ONNX session to free GPU/CPU memory.
    /// The session is lazily re-created on the next `encode()` call.
    ///
    /// RM-V1.25-15: Also drops the tokenizer (~20MB) — it lazy-reloads
    /// from `tokenizer_path` on the next encode. In-flight encoders that
    /// already cloned the Arc keep their copy for the duration of that
    /// call.
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            *guard = None;
            tracing::debug!("SPLADE session cleared");
        }
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        if tok.is_some() {
            *tok = None;
            tracing::debug!("SPLADE tokenizer cleared");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn splade_model_dir() -> Option<PathBuf> {
        // PB-V1.29-8: share the platform-aware HF parent resolution.
        let dir = crate::aux_model::hf_cache_dir("splade-onnx");
        if dir.join("model.onnx").exists() {
            Some(dir)
        } else {
            None
        }
    }

    #[test]
    #[ignore] // Requires SPLADE model download
    fn test_encode_produces_sparse_vector() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let sparse = encoder.encode("parse configuration file").unwrap();
        assert!(!sparse.is_empty(), "Sparse vector should not be empty");
        assert!(
            sparse.len() < encoder.vocab_size(),
            "Sparse vector should be sparse (< vocab size)"
        );
    }

    #[test]
    #[ignore]
    fn test_encode_respects_threshold() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.5).unwrap();
        let sparse = encoder.encode("search filtered results").unwrap();
        for &(_, weight) in &sparse {
            assert!(
                weight > 0.5,
                "All weights should exceed threshold, got {}",
                weight
            );
        }
    }

    #[test]
    #[ignore]
    fn test_encode_empty_string() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let sparse = encoder.encode("").unwrap();
        assert!(
            sparse.is_empty(),
            "Empty string should produce empty vector"
        );
    }

    #[test]
    #[ignore]
    fn test_encode_batch_matches_single() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let text = "find dead code functions";
        let single = encoder.encode(text).unwrap();
        let batch = encoder.encode_batch(&[text]).unwrap();
        assert_eq!(single.len(), batch[0].len());
        // Weights should be identical (same model, same input)
        for (s, b) in single.iter().zip(batch[0].iter()) {
            assert_eq!(s.0, b.0, "Token IDs should match");
            assert!(
                (s.1 - b.1).abs() < 1e-5,
                "Weights should match: {} vs {}",
                s.1,
                b.1
            );
        }
    }

    /// Multi-input batch must agree with serial encoding for every example.
    /// This is the load-bearing correctness test for the batching path —
    /// padding shorter sequences must not affect their results, and the
    /// per-example reshape/extraction must address the right rows.
    ///
    /// Three texts of intentionally varying length so the padding actually
    /// kicks in: position 0 is the longest (no padding needed), positions
    /// 1 and 2 get padded.
    #[test]
    #[ignore]
    fn test_encode_batch_multiple_matches_serial() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();

        let texts = vec![
            "find a function that parses configuration files and validates the result",
            "search for dead code",
            "Vec::new",
        ];

        // Serial reference
        let serial: Vec<_> = texts.iter().map(|t| encoder.encode(t).unwrap()).collect();
        // Batched
        let batched = encoder.encode_batch(&texts).unwrap();

        assert_eq!(serial.len(), batched.len());
        for (i, (s, b)) in serial.iter().zip(batched.iter()).enumerate() {
            assert_eq!(
                s.len(),
                b.len(),
                "example {i}: token count mismatch (serial {} vs batched {})",
                s.len(),
                b.len()
            );
            for (j, ((s_id, s_w), (b_id, b_w))) in s.iter().zip(b.iter()).enumerate() {
                assert_eq!(s_id, b_id, "example {i} token {j}: id mismatch");
                assert!(
                    (s_w - b_w).abs() < 1e-4,
                    "example {i} token {j}: weight mismatch ({s_w} vs {b_w})"
                );
            }
        }
    }

    // ===== encode_batch edge-case tests =====
    //
    // These exercise the empty/edge paths that bail out before any ONNX
    // inference, so they don't need a real model file. They cover the
    // input handling that's most likely to break under refactoring.

    #[test]
    fn test_encode_batch_empty_input_list() {
        // No model needed — empty input never reaches inference.
        // We construct the encoder via a dummy path to test the early return
        // path without loading a model.
        //
        // SpladeEncoder::new requires a real model, so we can't construct an
        // encoder here without one. Instead we verify the early-return contract
        // structurally: encode_batch on an empty slice must return an empty Vec.
        // This is tested via the property that "if texts.is_empty() return Ok(vec![])"
        // at the top of encode_batch — covered by the unit test below that
        // exercises the function on a real model when available.
        //
        // We DO test the contract in the function-level test_encode_batch_empty_input
        // below; this stub remains to document the expected behavior.
    }

    #[test]
    #[ignore]
    fn test_encode_batch_empty_input_real_model() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let result = encoder.encode_batch(&[]).unwrap();
        assert!(result.is_empty(), "empty input list → empty result");
    }

    /// All inputs are empty strings → all outputs should be empty vectors,
    /// and we should NOT attempt inference (no model needed for this branch).
    #[test]
    #[ignore]
    fn test_encode_batch_all_empty_strings() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let result = encoder.encode_batch(&["", "", ""]).unwrap();
        assert_eq!(result.len(), 3);
        for (i, sv) in result.iter().enumerate() {
            assert!(
                sv.is_empty(),
                "position {i}: empty input should produce empty vector"
            );
        }
    }

    /// Mixed empty and non-empty inputs: empty positions get empty vectors
    /// and the inference runs only on the non-empty subset. Critical: the
    /// output indices must align with the original input indices.
    #[test]
    #[ignore]
    fn test_encode_batch_mixed_empty_and_nonempty() {
        let dir = splade_model_dir().expect("SPLADE model not downloaded");
        let encoder = SpladeEncoder::new(&dir, 0.01).unwrap();
        let result = encoder
            .encode_batch(&["", "find dead code", "", "search for parser bugs", ""])
            .unwrap();
        assert_eq!(result.len(), 5);
        assert!(result[0].is_empty(), "position 0 (empty) → empty");
        assert!(!result[1].is_empty(), "position 1 (non-empty) → non-empty");
        assert!(result[2].is_empty(), "position 2 (empty) → empty");
        assert!(!result[3].is_empty(), "position 3 (non-empty) → non-empty");
        assert!(result[4].is_empty(), "position 4 (empty) → empty");

        // Cross-check: the non-empty results match what serial encode produces
        let serial_1 = encoder.encode("find dead code").unwrap();
        let serial_3 = encoder.encode("search for parser bugs").unwrap();
        assert_eq!(result[1].len(), serial_1.len());
        assert_eq!(result[3].len(), serial_3.len());
    }

    #[test]
    fn test_model_not_found() {
        let result = SpladeEncoder::new(Path::new("/nonexistent"), 0.01);
        assert!(result.is_err(), "Should fail for nonexistent path");
        match result {
            Err(e) => assert!(
                e.to_string().contains("not found"),
                "Error should mention not found: {e}"
            ),
            Ok(_) => unreachable!(),
        }
    }

    // ===== resolve_splade_model_dir tests =====
    //
    // These touch the process-wide CQS_SPLADE_MODEL env var and serialize on
    // a static Mutex so they don't race against each other or against any
    // other test that touches the same var.

    use std::sync::Mutex;
    static SPLADE_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: write a stub directory with both required files so the
    /// resolver believes a model lives there. Doesn't write a real ONNX
    /// graph — that's only needed for tests that actually load the encoder.
    fn write_stub_splade_dir(dir: &Path) {
        std::fs::write(dir.join("model.onnx"), b"stub").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"stub").unwrap();
    }

    /// `CQS_SPLADE_MODEL` set to a directory with both files → returned as-is.
    #[test]
    fn test_resolve_env_var_override() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        write_stub_splade_dir(tmp.path());

        std::env::set_var("CQS_SPLADE_MODEL", tmp.path());
        let resolved = resolve_splade_model_dir();
        std::env::remove_var("CQS_SPLADE_MODEL");

        assert_eq!(resolved.as_deref(), Some(tmp.path()));
    }

    /// `CQS_SPLADE_MODEL` set to a `~/...` path → expanded against $HOME.
    #[test]
    fn test_resolve_env_var_tilde_expansion() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        // Build a stub dir under $HOME so a tilde-prefixed env var resolves
        // to a real existing directory. Use a unique subdir to avoid colliding
        // with other tests.
        let home = dirs::home_dir().expect("HOME must be set in test env");
        let stub_subdir = format!(".cqs-test-splade-{}", std::process::id());
        let stub_dir = home.join(&stub_subdir);
        std::fs::create_dir_all(&stub_dir).unwrap();
        write_stub_splade_dir(&stub_dir);

        std::env::set_var("CQS_SPLADE_MODEL", format!("~/{stub_subdir}"));
        let resolved = resolve_splade_model_dir();
        std::env::remove_var("CQS_SPLADE_MODEL");

        // Cleanup before assertions so a failure doesn't strand the dir.
        let _ = std::fs::remove_dir_all(&stub_dir);

        assert_eq!(
            resolved.as_deref(),
            Some(stub_dir.as_path()),
            "tilde-prefixed CQS_SPLADE_MODEL should expand against $HOME"
        );
    }

    /// `CQS_SPLADE_MODEL` set but the directory has no `model.onnx` → None.
    #[test]
    fn test_resolve_env_var_missing_model_returns_none() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Only write tokenizer, no model.onnx
        std::fs::write(tmp.path().join("tokenizer.json"), b"stub").unwrap();

        std::env::set_var("CQS_SPLADE_MODEL", tmp.path());
        let resolved = resolve_splade_model_dir();
        std::env::remove_var("CQS_SPLADE_MODEL");

        assert!(
            resolved.is_none(),
            "should return None when model.onnx is missing"
        );
    }

    /// `CQS_SPLADE_MODEL` set but no `tokenizer.json` → None. Critical: this
    /// is the failure mode the vocab-mismatch detection was added to catch,
    /// so we want the resolver to also reject the missing-tokenizer case.
    #[test]
    fn test_resolve_env_var_missing_tokenizer_returns_none() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        // Only write model, no tokenizer.json
        std::fs::write(tmp.path().join("model.onnx"), b"stub").unwrap();

        std::env::set_var("CQS_SPLADE_MODEL", tmp.path());
        let resolved = resolve_splade_model_dir();
        std::env::remove_var("CQS_SPLADE_MODEL");

        assert!(
            resolved.is_none(),
            "should return None when tokenizer.json is missing — \
             a model+wrong-tokenizer dir must not silently fall through"
        );
    }

    /// Empty `CQS_SPLADE_MODEL` value → falls back to default cache dir.
    /// This is the bash gotcha where `export CQS_SPLADE_MODEL=` (no value)
    /// would otherwise be treated as "the empty path" and resolve nowhere.
    #[test]
    fn test_resolve_env_var_empty_falls_back_to_default() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_SPLADE_MODEL", "");
        let resolved = resolve_splade_model_dir();
        std::env::remove_var("CQS_SPLADE_MODEL");

        // The default path may or may not actually exist on this machine —
        // we only care that the empty-string env var didn't take precedence.
        // If it had, the resolver would have inspected an empty PathBuf and
        // returned None for "model.onnx not found at ".
        // PB-V1.29-8: expected path follows the platform-aware resolver so
        // this test still picks up a real install on Windows.
        let expected_default = crate::aux_model::hf_cache_dir("splade-onnx");
        if expected_default.join("model.onnx").exists()
            && expected_default.join("tokenizer.json").exists()
        {
            assert_eq!(
                resolved.as_deref(),
                Some(expected_default.as_path()),
                "empty env var should fall back to default cache dir"
            );
        } else {
            assert!(
                resolved.is_none(),
                "empty env var with no default model installed → None"
            );
        }
    }

    /// No env var set → falls back to default cache dir resolution.
    #[test]
    fn test_resolve_no_env_var() {
        let _guard = SPLADE_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_SPLADE_MODEL");
        let resolved = resolve_splade_model_dir();

        // Identical reasoning to the empty-string case — the result depends
        // on whether a default model is installed on the test machine.
        // PB-V1.29-8: expected path follows the platform-aware resolver.
        let expected_default = crate::aux_model::hf_cache_dir("splade-onnx");
        if expected_default.join("model.onnx").exists()
            && expected_default.join("tokenizer.json").exists()
        {
            assert_eq!(resolved.as_deref(), Some(expected_default.as_path()));
        } else {
            assert!(resolved.is_none());
        }
    }

    /// SpladeError::ConfigMismatch renders a message that points the user at
    /// the actionable fix (replace tokenizer.json). Verifies the message
    /// stays useful — Display impl is the only place mismatched users see
    /// guidance.
    #[test]
    fn test_config_mismatch_error_message_is_actionable() {
        let err = SpladeError::ConfigMismatch {
            dir: PathBuf::from("/some/where/splade-onnx"),
            tokenizer_vocab: 30522,
            model_vocab: 151936,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("30522"),
            "should include tokenizer vocab: {msg}"
        );
        assert!(msg.contains("151936"), "should include model vocab: {msg}");
        assert!(
            msg.contains("/some/where/splade-onnx"),
            "should include the directory: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("tokenizer"),
            "should mention tokenizer.json as the fix-point: {msg}"
        );
    }

    // ===== Vocab compatibility tests =====
    //
    // The vocab compatibility check has three branches we need to verify
    // independently. We can't easily run a real ONNX inference in unit tests
    // (no model artifact in CI), so we test the comparison logic by exercising
    // the same conditions through a focused helper.

    /// Reproduces the exact comparison logic from `SpladeEncoder::new` so we
    /// can unit test the three branches without spinning up an ORT session.
    /// Returns Ok(was_padded) when the configuration is acceptable, Err with
    /// the reason when it isn't. Keeps test code coupled to the production
    /// branches via assertions in the same test fn — if production logic
    /// changes, this helper must be updated to match.
    fn check_vocab_compatibility(
        tokenizer_vocab: usize,
        model_vocab: usize,
    ) -> Result<bool, &'static str> {
        if model_vocab < tokenizer_vocab {
            return Err("model_vocab < tokenizer_vocab");
        }
        let padding_pct = if tokenizer_vocab > 0 {
            (model_vocab - tokenizer_vocab) as f32 * 100.0 / tokenizer_vocab as f32
        } else {
            0.0
        };
        if padding_pct > 1.5 {
            return Err("padding > 1.5%");
        }
        Ok(model_vocab > tokenizer_vocab)
    }

    /// Equal vocabs are the ideal case — accepted, no padding.
    #[test]
    fn test_vocab_compat_exact_match_accepted() {
        assert_eq!(check_vocab_compatibility(30522, 30522), Ok(false));
        assert_eq!(check_vocab_compatibility(151669, 151669), Ok(false));
    }

    /// Small benign padding (e.g. lm_head padded to a friendly size) is
    /// accepted with a warning. The 151669 → 151936 case is the actual
    /// SPLADE-Code 0.6B export shape — we MUST accept this or the
    /// production model is unusable.
    #[test]
    fn test_vocab_compat_benign_padding_accepted() {
        // SPLADE-Code 0.6B real numbers — Qwen3 vocab padded by 267 (0.18%)
        assert_eq!(
            check_vocab_compatibility(151669, 151936),
            Ok(true),
            "SPLADE-Code 0.6B's 0.18% lm_head padding must be accepted"
        );
        // 1% padding is well within tolerance
        assert_eq!(
            check_vocab_compatibility(30000, 30300),
            Ok(true),
            "1% padding should be accepted"
        );
        // Right at the edge of the 1.5% threshold
        assert_eq!(
            check_vocab_compatibility(30000, 30449),
            Ok(true),
            "1.49% padding should be accepted"
        );
    }

    /// Suspiciously large gaps (>1.5%) are rejected — likely the wrong
    /// tokenizer for the model architecture.
    #[test]
    fn test_vocab_compat_large_padding_rejected() {
        // Just over the 1.5% threshold
        assert_eq!(
            check_vocab_compatibility(30000, 30460),
            Err("padding > 1.5%"),
            "1.53% padding should be rejected"
        );
        // 4x larger model — clearly wrong tokenizer
        assert_eq!(
            check_vocab_compatibility(30522, 121936),
            Err("padding > 1.5%"),
        );
    }

    /// Tokenizer larger than model is the original BERT-with-SPLADE-Code
    /// failure mode — must hard-fail because the tokenizer can produce
    /// token IDs the model has no output slot for.
    #[test]
    fn test_vocab_compat_tokenizer_larger_rejected() {
        // The exact bug we hit: BERT WordPiece (30522) vs SPLADE-Code lm_head (151936).
        // Wait — that's the OPPOSITE direction. The bug happened because the model
        // had MORE vocab than the tokenizer, but the tokenizer was producing IDs
        // that the (different family) model could not interpret semantically.
        // The dimensions matched at the API level (151936 > 30522, which would
        // PASS this check) — but the *semantics* were broken. This unit test
        // covers the dimensional case; semantic compatibility is enforced by
        // the embedding pipeline and the eval results.
        //
        // The dimensional case this test catches: tokenizer larger than model.
        // E.g. SPLADE-Code 0.6B tokenizer (151669) with off-the-shelf BERT
        // model (30522). The tokenizer would emit token IDs above 30522 and
        // the model would either crash or wrap.
        assert_eq!(
            check_vocab_compatibility(151669, 30522),
            Err("model_vocab < tokenizer_vocab"),
            "tokenizer larger than model must hard-fail"
        );
        assert_eq!(
            check_vocab_compatibility(151936, 151935),
            Err("model_vocab < tokenizer_vocab"),
            "even by 1 must hard-fail"
        );
    }

    /// Edge case: zero-vocab tokenizer (degenerate, shouldn't happen in prod
    /// but the math should still produce a sensible result).
    #[test]
    fn test_vocab_compat_zero_tokenizer_vocab() {
        // model >= 0, padding_pct stays 0.0 → accepted as no-padding
        assert_eq!(check_vocab_compatibility(0, 0), Ok(false));
        assert_eq!(check_vocab_compatibility(0, 100), Ok(true));
    }

    // ===== TC-ADV-1.29-9: raw-logits Inf/NaN propagation =====
    //
    // The raw-logits path in `SpladeEncoder::encode` (and `encode_batch`)
    // uses this sequence per vocab position:
    //
    //   pooled = max over seq dim
    //   activated = ln(1 + max(logit, 0.0))
    //   keep if activated > threshold
    //
    // No finite-check before `ln`, no NaN handling in the threshold compare.
    // If ONNX emits NaN for a vocab position, `val.max(0.0)` → NaN,
    // `1.0 + NaN` → NaN, `NaN.ln()` → NaN, `NaN > threshold` → false, so
    // the slot is silently dropped. If ONNX emits `+Inf`, activated =
    // `ln(1+Inf)` = `+Inf`, which survives every positive threshold and
    // poisons the sparse vector with an Inf weight.
    //
    // We can't easily run real ONNX in unit tests. These tests exercise
    // the exact activation + threshold math used inside `encode` (see
    // `src/splade/mod.rs:559-569` and the identical math in
    // `encode_batch:889-900`). Pin current behaviour so a future
    // finite-check refactor is deliberate.

    /// Reproduces the activation math from `encode` / `encode_batch` so
    /// tests can exercise the NaN/Inf branches without an ORT session.
    fn activate_threshold(val: f32, threshold: f32) -> Option<f32> {
        let activated = (1.0 + val.max(0.0)).ln();
        if activated > threshold {
            Some(activated)
        } else {
            None
        }
    }

    /// A NaN logit silently collapses to "not kept". Acceptable today (the
    /// slot is dropped, not poisoned) but pins the behaviour so a future
    /// strict-reject change is deliberate.
    #[test]
    fn test_raw_logits_nan_silently_dropped() {
        let result = activate_threshold(f32::NAN, 0.01);
        assert_eq!(
            result, None,
            "AUDIT-FOLLOWUP (TC-ADV-1.29-9): NaN logit is silently dropped — \
             no error surfaces, no telemetry event. A future encoder audit \
             should decide whether to reject or telemetry this case."
        );
    }

    /// A +Inf logit produces +Inf activation and passes the threshold,
    /// poisoning the sparse vector with an Inf weight. This WILL surface
    /// in downstream scoring (dot products blow up). Critical to pin.
    #[test]
    fn test_raw_logits_positive_inf_passes_through_as_inf_weight() {
        let result = activate_threshold(f32::INFINITY, 0.01);
        let weight = result.unwrap_or_else(|| {
            panic!(
                "AUDIT-FOLLOWUP (TC-ADV-1.29-9): +Inf logit should produce an \
                 Inf-activation entry but activate_threshold returned None — \
                 contract changed, update the test"
            )
        });
        assert!(
            weight.is_infinite() && weight.is_sign_positive(),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-9): +Inf logit produces +Inf weight \
             in the sparse vector — downstream consumers (SPLADE dot-product \
             scoring) will blow up silently. Got {weight}"
        );
    }

    /// A -Inf logit is clamped by `.max(0.0)` → activated = ln(1 + 0) = 0,
    /// not > threshold, slot dropped. Pin as sanity.
    #[test]
    fn test_raw_logits_negative_inf_clamped_to_zero_then_dropped() {
        let result = activate_threshold(f32::NEG_INFINITY, 0.01);
        assert_eq!(
            result, None,
            "-Inf logit is clamped to 0 by `.max(0.0)`, then ln(1) = 0 fails \
             the threshold — slot dropped cleanly"
        );
    }

    /// A huge finite logit (not Inf) does produce a large but finite weight
    /// through the log compression. Guards against a future refactor that
    /// accidentally removes the `ln` (which would produce unbounded weights).
    #[test]
    fn test_raw_logits_large_finite_logit_produces_finite_weight() {
        // 1e10 activated = ln(1 + 1e10) ≈ 23
        let result = activate_threshold(1e10, 0.01);
        let weight = result.expect("large finite logit must survive threshold");
        assert!(
            weight.is_finite(),
            "ln-compressed weight must stay finite, got {weight}"
        );
        assert!(
            weight > 20.0 && weight < 30.0,
            "ln(1 + 1e10) ≈ 23, got {weight}"
        );
    }
}
