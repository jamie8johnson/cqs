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

const MODEL_REPO: &str = "cross-encoder/ms-marco-MiniLM-L-6-v2";
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

#[derive(Debug, thiserror::Error)]
pub enum RerankerError {
    #[error("Model download failed: {0}")]
    ModelDownload(String),
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),
    #[error("Inference error: {0}")]
    Inference(String),
}

impl From<ort::Error> for RerankerError {
    fn from(e: ort::Error) -> Self {
        RerankerError::Inference(e.to_string())
    }
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
        Ok(Self {
            session: Mutex::new(None),
            tokenizer: OnceCell::new(),
            model_paths: OnceCell::new(),
            provider,
            max_length: 512,
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

        let tokenizer = self.tokenizer()?;

        // 1. Tokenize (query, passage) pairs
        let encodings: Vec<tokenizers::Encoding> = results
            .iter()
            .map(|r| {
                tokenizer
                    .encode((query, r.chunk.content.as_str()), true)
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
        let ids_tensor = Tensor::from_array(ids_arr)?;
        let mask_tensor = Tensor::from_array(mask_arr)?;
        let type_tensor = Tensor::from_array(type_arr)?;

        // 3. Run inference
        let mut session_guard = self.session()?;
        let session = session_guard.as_mut().unwrap(); // Safe: session() guarantees Some
        let outputs = session.run(ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ])?;

        // 4. Extract logits, apply sigmoid
        // Cross-encoder output is typically "logits" with shape [batch, 1] or [batch]
        // ort rc.11 try_extract_tensor returns (Vec<i64>, Vec<f32>)
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let batch_size = results.len();

        // Handle [batch, 1] → stride 1, or [batch] → stride 1
        let stride = if shape.len() == 2 {
            shape[1] as usize
        } else {
            1
        };

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

        // 5. Sort descending by score, truncate
        results.sort_by(|a, b| b.score.total_cmp(&a.score));
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
            let repo = api.model(MODEL_REPO.to_string());

            let model_path = repo
                .get(MODEL_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;
            let tokenizer_path = repo
                .get(TOKENIZER_FILE)
                .map_err(|e| RerankerError::ModelDownload(e.to_string()))?;

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
