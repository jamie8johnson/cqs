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

use crate::embedder::{create_session, select_provider};

/// Convert ORT errors to SpladeError
fn ort_err(e: ort::Error) -> SpladeError {
    SpladeError::InferenceFailed(e.to_string())
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
    tokenizer: tokenizers::Tokenizer,
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
        shape[1] as usize
    } else if let Some(logits_output) = outputs.get("logits") {
        let (shape, _data) = logits_output.try_extract_tensor::<f32>().map_err(ort_err)?;
        if shape.len() != 3 {
            return Err(SpladeError::InferenceFailed(format!(
                "probe: expected 3D logits [batch, seq, vocab], got {}D",
                shape.len()
            )));
        }
        shape[2] as usize
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
/// Resolution order:
/// 1. `CQS_SPLADE_MODEL` env var (absolute or `~`-prefixed path) — overrides
///    everything. The directory must contain `model.onnx` AND `tokenizer.json`.
/// 2. `~/.cache/huggingface/splade-onnx/` (default location)
///
/// Returns `None` when neither location has both required files. Callers
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
    let _span = tracing::debug_span!("resolve_splade_model_dir").entered();

    let dir = match std::env::var("CQS_SPLADE_MODEL") {
        Ok(p) if !p.is_empty() => {
            // Expand a leading "~/" using $HOME so users can write
            // CQS_SPLADE_MODEL=~/training-data/splade-code-naver/onnx
            let expanded = if let Some(stripped) = p.strip_prefix("~/") {
                dirs::home_dir()
                    .map(|h| h.join(stripped))
                    .unwrap_or_else(|| p.into())
            } else {
                p.into()
            };
            tracing::info!(
                source = "CQS_SPLADE_MODEL",
                path = %expanded.display(),
                "SPLADE model dir resolved from env var"
            );
            expanded
        }
        _ => {
            let default = dirs::home_dir()
                .map(|h| h.join(".cache/huggingface/splade-onnx"))
                .unwrap_or_default();
            tracing::debug!(path = %default.display(), "Using default SPLADE model dir");
            default
        }
    };

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

    Some(dir)
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
    /// vocabulary mismatches. If the tokenizer vocab and the model output vocab
    /// disagree, returns [`SpladeError::ConfigMismatch`] — encoding would
    /// otherwise silently produce garbage. This catches the failure mode where
    /// `model.onnx` is hot-swapped (e.g. SPLADE-Code 0.6B replaces BERT 110M)
    /// without updating `tokenizer.json`.
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

        if tokenizer_vocab != model_vocab {
            tracing::error!(
                tokenizer_vocab,
                model_vocab,
                dir = %model_dir.display(),
                "SPLADE tokenizer/model vocab mismatch — refusing to load"
            );
            return Err(SpladeError::ConfigMismatch {
                dir: model_dir.to_path_buf(),
                tokenizer_vocab,
                model_vocab,
            });
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

        Ok(Self {
            session: Mutex::new(Some(session)),
            model_path: onnx_path,
            tokenizer,
            threshold,
            vocab_size: tokenizer_vocab,
        })
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
        let text = if text.len() > 4000 {
            let truncated = &text[..text
                .char_indices()
                .nth(4000)
                .map(|(i, _)| i)
                .unwrap_or(text.len())];
            tracing::debug!(
                original_len = text.len(),
                truncated_len = truncated.len(),
                "Truncated SPLADE input to 4000 chars"
            );
            truncated
        } else {
            text
        };

        // Tokenize
        let encoding = self
            .tokenizer
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
            let vocab = shape[1] as usize;
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
            let vocab = shape[2] as usize;
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

    /// Batch encode multiple texts.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<SparseVector>, SpladeError> {
        let _span = tracing::debug_span!("splade_encode_batch", count = texts.len()).entered();
        // Sequential for now — SPLADE models are small enough that batching
        // doesn't save much vs the overhead of padding/unpadding.
        texts.iter().map(|t| self.encode(t)).collect()
    }

    /// Vocabulary size of the underlying tokenizer.
    pub fn vocab_size(&self) -> usize {
        self.vocab_size
    }

    /// Decode a token ID to its string representation (for debugging).
    pub fn decode_token(&self, token_id: u32) -> Option<String> {
        self.tokenizer.decode(&[token_id], false).ok()
    }

    /// RM-3: Drop the ONNX session to free GPU/CPU memory.
    /// The session is lazily re-created on the next `encode()` call.
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_some() {
            *guard = None;
            tracing::debug!("SPLADE session cleared");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn splade_model_dir() -> Option<PathBuf> {
        let dir = dirs::home_dir()?.join(".cache/huggingface/splade-onnx");
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
        let expected_default = dirs::home_dir()
            .map(|h| h.join(".cache/huggingface/splade-onnx"))
            .unwrap_or_default();
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
        let expected_default = dirs::home_dir()
            .map(|h| h.join(".cache/huggingface/splade-onnx"))
            .unwrap_or_default();
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
}
