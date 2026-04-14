//! Cross-encoder re-ranking for second-pass scoring
//!
//! Reorders search results using a cross-encoder model that scores
//! (query, passage) pairs directly, producing more accurate rankings
//! than embedding cosine similarity alone.
//!
//! Uses `cross-encoder/ms-marco-MiniLM-L-6-v2` (~91MB ONNX, 22M params).

use std::path::PathBuf;
use std::sync::Mutex;

use ndarray::Array2;
use once_cell::sync::OnceCell;
use ort::session::Session;

use crate::embedder::{create_session, pad_2d_i64, select_provider, ExecutionProvider};
use crate::store::SearchResult;

const DEFAULT_MODEL_REPO: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

// blake3 checksums -- empty to skip validation (set after pinning a model version)
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

/// Retrieves the reranker model repository path from the environment or returns the default.
///
/// # Returns
///
/// A string containing the model repository path. If the `CQS_RERANKER_MODEL` environment variable is set, returns its value; otherwise returns the default model repository.
fn model_repo() -> String {
    match std::env::var("CQS_RERANKER_MODEL") {
        Ok(repo) => {
            tracing::info!(model = %repo, "Using custom reranker model");
            repo
        }
        Err(_) => DEFAULT_MODEL_REPO.to_string(),
    }
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
    tokenizer: OnceCell<tokenizers::Tokenizer>,
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
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
            tokenizer: OnceCell::new(),
            model_paths: OnceCell::new(),
            provider,
            max_length,
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
        let passages: Vec<String> = results.iter().map(|r| r.chunk.content.clone()).collect();
        let refs: Vec<&str> = passages.iter().map(|s| s.as_str()).collect();
        self.rerank_with_passages(query, results, &refs, limit)
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

        let tokenizer = self.tokenizer()?;

        // 1. Tokenize (query, passage) pairs
        let encodings: Vec<tokenizers::Encoding> = passages
            .iter()
            .map(|passage| {
                tokenizer
                    .encode((query, *passage), true)
                    .map_err(|e| RerankerError::Tokenizer(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        // 2. Build padded tensors
        let input_ids: Vec<Vec<i64>> = encodings
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i64).collect())
            .collect();
        let attention_mask: Vec<Vec<i64>> = encodings
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
            return Ok(()); // Nothing to score — empty tokenization
        }

        let ids_arr = pad_2d_i64(&input_ids, max_len, 0);
        let mask_arr = pad_2d_i64(&attention_mask, max_len, 0);
        let type_arr = Array2::<i64>::zeros((results.len(), max_len));

        // Create tensors (ort requires Value, not raw ndarray)
        use ort::value::Tensor;
        let ids_tensor = Tensor::from_array(ids_arr).map_err(ort_err)?;
        let mask_tensor = Tensor::from_array(mask_arr).map_err(ort_err)?;
        let type_tensor = Tensor::from_array(type_arr).map_err(ort_err)?;

        // 3. Run inference
        let mut session_guard = self.session()?;
        let session = session_guard
            .as_mut()
            .expect("session() guarantees initialized after Ok return");
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
                "token_type_ids" => type_tensor,
            ])
            .map_err(ort_err)?;

        // 4. Extract logits, apply sigmoid
        // Cross-encoder output is typically "logits" with shape [batch, 1] or [batch]
        // ort rc.11 try_extract_tensor returns (Vec<i64>, Vec<f32>)
        if outputs.len() == 0 {
            return Err(RerankerError::Inference(
                "ONNX model produced no outputs".to_string(),
            ));
        }
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(ort_err)?;
        let batch_size = results.len();

        // Handle [batch, 1] → stride 1, or [batch] → stride 1
        let stride = if shape.len() == 2 {
            shape[1] as usize
        } else {
            1
        };

        if stride == 0 {
            return Err(RerankerError::Inference(
                "Model returned zero-width output tensor".to_string(),
            ));
        }

        let expected_len = batch_size * stride;
        if data.len() < expected_len {
            return Err(RerankerError::Inference(format!(
                "Model output too short: expected {} elements, got {}",
                expected_len,
                data.len()
            )));
        }

        for (i, result) in results.iter_mut().enumerate() {
            let logit = data[i * stride];
            result.score = sigmoid(logit);
        }

        // 5. Sort descending by score, truncate. Secondary sort on chunk id
        // keeps equal-score candidates deterministically ordered so the
        // truncate() drops the same candidates on every invocation.
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk.id.cmp(&b.chunk.id))
        });
        results.truncate(limit);

        tracing::info!(reranked = results.len(), batch_size, "Re-ranking complete");
        Ok(())
    }

    /// Download model and tokenizer from HuggingFace Hub
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), RerankerError> {
        self.model_paths.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_model_download").entered();
            use hf_hub::api::sync::Api;

            let api = Api::new().map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let repo = api.model(model_repo());

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
            *guard = Some(
                create_session(model_path, self.provider)
                    .map_err(|e| RerankerError::Inference(e.to_string()))?,
            );
            tracing::info!("Reranker session initialized");
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
        tracing::info!("Reranker session cleared");
    }

    /// Get or initialize the tokenizer
    fn tokenizer(&self) -> Result<&tokenizers::Tokenizer, RerankerError> {
        let (_, tokenizer_path) = self.model_paths()?;
        self.tokenizer.get_or_try_init(|| {
            let _span = tracing::info_span!("reranker_tokenizer_init").entered();
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| RerankerError::Tokenizer(e.to_string()))
        })
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
