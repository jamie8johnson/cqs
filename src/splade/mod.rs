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

use ndarray::{Array2, Axis};
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
}

/// SPLADE encoder using ONNX Runtime.
///
/// Loads a BertForMaskedLM model and produces sparse vectors via
/// max pooling → ReLU → log(1+x) → threshold.
pub struct SpladeEncoder {
    session: Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    threshold: f32,
    vocab_size: usize,
}

impl SpladeEncoder {
    /// Load SPLADE model from a directory containing model.onnx and tokenizer.json.
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

        // BERT vocabulary is typically 30522
        let vocab_size = tokenizer.get_vocab_size(true);

        tracing::info!(threshold, vocab_size, "SPLADE encoder loaded");

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            threshold,
            vocab_size,
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

        // Run inference
        let mut session = self.session.lock().unwrap();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
            .map_err(ort_err)?;

        // Get logits: shape [1, seq_len, vocab_size]
        let logits_output = outputs.get("logits").ok_or_else(|| {
            SpladeError::InferenceFailed(format!(
                "No 'logits' output. Available: {:?}",
                outputs.keys().collect::<Vec<_>>()
            ))
        })?;
        let (shape, data) = logits_output.try_extract_tensor::<f32>().map_err(ort_err)?;

        if shape.len() != 3 {
            return Err(SpladeError::InferenceFailed(format!(
                "Expected 3D logits [batch, seq, vocab], got {}D",
                shape.len()
            )));
        }

        let vocab = shape[2] as usize;
        let logits = Array2::from_shape_vec((seq_len, vocab), data.iter().copied().collect())
            .map_err(|e| SpladeError::InferenceFailed(format!("Failed to reshape logits: {e}")))?;

        // Max pool over sequence dimension → [vocab_size]
        let pooled = logits.fold_axis(Axis(0), f32::NEG_INFINITY, |&a, &b| a.max(b));

        // ReLU + log(1+x) + threshold
        let sparse: SparseVector = pooled
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

        tracing::debug!(non_zero = sparse.len(), vocab, "SPLADE encoding complete");
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
}
