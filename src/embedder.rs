//! Embedding generation with ort + tokenizers

use lru::LruCache;
use ndarray::Array2;
use ort::ep::ExecutionProvider as OrtExecutionProvider;
use ort::session::Session;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

// Model configuration
const MODEL_REPO: &str = "nomic-ai/nomic-embed-text-v1.5";
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

// blake3 checksums for model verification (update when model changes)
const MODEL_BLAKE3: &str = "34f5f98a1bb6ecd9e6095ec8d4da7b3491517dcf1d6dd5bd57c0171bf744b749";
const TOKENIZER_BLAKE3: &str = "6e933bf59db40b8b2a0de480fe5006662770757e1e1671eb7e48ff6a5f00b0b4";

#[derive(Error, Debug)]
pub enum EmbedderError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Tokenizer error: {0}")]
    TokenizerError(String),
    #[error("Inference failed: {0}")]
    InferenceFailed(String),
    #[error("Checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("Query cannot be empty")]
    EmptyQuery,
    #[error("HuggingFace Hub error: {0}")]
    HfHubError(String),
}

impl From<ort::Error> for EmbedderError {
    fn from(e: ort::Error) -> Self {
        EmbedderError::InferenceFailed(e.to_string())
    }
}

/// A 768-dimensional L2-normalized embedding vector
///
/// Embeddings are produced by nomic-embed-text-v1.5 and can be
/// compared using cosine similarity (dot product for normalized vectors).
#[derive(Debug, Clone)]
pub struct Embedding(pub Vec<f32>);

/// Hardware execution provider for inference
#[derive(Debug, Clone, Copy)]
pub enum ExecutionProvider {
    /// NVIDIA CUDA (requires CUDA toolkit)
    CUDA { device_id: i32 },
    /// NVIDIA TensorRT (faster than CUDA, requires TensorRT)
    TensorRT { device_id: i32 },
    /// CPU fallback (always available)
    CPU,
}

impl std::fmt::Display for ExecutionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionProvider::CUDA { device_id } => write!(f, "CUDA (device {})", device_id),
            ExecutionProvider::TensorRT { device_id } => {
                write!(f, "TensorRT (device {})", device_id)
            }
            ExecutionProvider::CPU => write!(f, "CPU"),
        }
    }
}

/// Text embedding generator using nomic-embed-text-v1.5
///
/// Automatically downloads the model from HuggingFace Hub on first use.
/// Detects GPU availability and uses CUDA/TensorRT when available.
///
/// # Example
///
/// ```no_run
/// use cqs::Embedder;
///
/// let mut embedder = Embedder::new()?;
/// let embedding = embedder.embed_query("parse configuration file")?;
/// println!("Embedding dimension: {}", embedding.0.len()); // 768
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Embedder {
    session: Session,
    tokenizer: tokenizers::Tokenizer,
    provider: ExecutionProvider,
    max_length: usize,
    batch_size: usize,
    /// LRU cache for query embeddings (avoids re-computing same queries)
    query_cache: Mutex<LruCache<String, Embedding>>,
}

impl Embedder {
    /// Create a new embedder, downloading the model if necessary
    ///
    /// Automatically detects GPU and uses CUDA/TensorRT when available.
    /// Falls back to CPU if no GPU is found.
    pub fn new() -> Result<Self, EmbedderError> {
        let (model_path, tokenizer_path) = ensure_model()?;
        let provider = select_provider();
        let session = create_session(&model_path, provider)?;
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

        let batch_size = match provider {
            ExecutionProvider::CPU => 4,
            _ => 16,
        };

        // Cache up to 100 query embeddings (typical search session)
        let query_cache = Mutex::new(LruCache::new(NonZeroUsize::new(100).unwrap()));

        Ok(Self {
            session,
            tokenizer,
            provider,
            max_length: 8192,
            batch_size,
            query_cache,
        })
    }

    /// Embed documents (code chunks). Adds "search_document: " prefix.
    pub fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let prefixed: Vec<String> = texts
            .iter()
            .map(|t| format!("search_document: {}", t))
            .collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a query. Adds "search_query: " prefix. Uses LRU cache for repeated queries.
    pub fn embed_query(&mut self, text: &str) -> Result<Embedding, EmbedderError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }

        // Check cache first
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                return Ok(cached.clone());
            }
        }

        // Compute embedding
        let prefixed = format!("search_query: {}", text);
        let results = self.embed_batch(&[prefixed])?;
        let embedding = results
            .into_iter()
            .next()
            .expect("embed_batch with single item always returns one result");

        // Store in cache
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned, recovering");
                poisoned.into_inner()
            });
            cache.put(text.to_string(), embedding.clone());
        }

        Ok(embedding)
    }

    /// Get the execution provider being used
    pub fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// Get the batch size
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Warm up the model with a dummy inference
    pub fn warm(&mut self) -> Result<(), EmbedderError> {
        let _ = self.embed_query("warmup")?;
        Ok(())
    }

    fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Embedding>, EmbedderError> {
        use ort::value::Tensor;

        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

        // Prepare inputs - INT64 (i64) for ONNX model
        let input_ids: Vec<Vec<i64>> = encodings
            .iter()
            .map(|e| e.get_ids().iter().map(|&id| id as i64).collect())
            .collect();
        let attention_mask: Vec<Vec<i64>> = encodings
            .iter()
            .map(|e| e.get_attention_mask().iter().map(|&m| m as i64).collect())
            .collect();

        // Pad to max length in batch
        let max_len = input_ids
            .iter()
            .map(|v| v.len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);

        // Create padded arrays
        let input_ids_arr = pad_2d_i64(&input_ids, max_len, 0);
        let attention_mask_arr = pad_2d_i64(&attention_mask, max_len, 0);
        // token_type_ids: all zeros, same shape as input_ids
        let token_type_ids_arr = Array2::<i64>::zeros((texts.len(), max_len));

        // Create tensors
        let input_ids_tensor = Tensor::from_array(input_ids_arr)?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_arr)?;
        let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr)?;

        // Run inference
        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids_tensor,
            "attention_mask" => attention_mask_tensor,
            "token_type_ids" => token_type_ids_tensor,
        ])?;

        // Get the last_hidden_state output: shape [batch, seq_len, 768]
        let (_shape, data) = outputs["last_hidden_state"].try_extract_tensor::<f32>()?;

        // Mean pooling over sequence dimension, weighted by attention mask
        let batch_size = texts.len();
        let seq_len = max_len;
        let embedding_dim = 768;
        let mut results = Vec::with_capacity(batch_size);

        for (i, mask_vec) in attention_mask.iter().enumerate().take(batch_size) {
            let mut sum = vec![0.0f32; embedding_dim];
            let mut count = 0.0f32;

            for j in 0..seq_len {
                let mask = mask_vec.get(j).copied().unwrap_or(0) as f32;
                if mask > 0.0 {
                    count += mask;
                    let offset = i * seq_len * embedding_dim + j * embedding_dim;
                    for (k, sum_val) in sum.iter_mut().enumerate() {
                        *sum_val += data[offset + k] * mask;
                    }
                }
            }

            // Avoid division by zero
            if count > 0.0 {
                for sum_val in &mut sum {
                    *sum_val /= count;
                }
            }

            results.push(Embedding(normalize_l2(sum)));
        }

        Ok(results)
    }
}

/// Download model and tokenizer from HuggingFace Hub
fn ensure_model() -> Result<(PathBuf, PathBuf), EmbedderError> {
    use hf_hub::api::sync::Api;

    let api = Api::new().map_err(|e| EmbedderError::HfHubError(e.to_string()))?;
    let repo = api.model(MODEL_REPO.to_string());

    let model_path = repo
        .get(MODEL_FILE)
        .map_err(|e| EmbedderError::HfHubError(e.to_string()))?;
    let tokenizer_path = repo
        .get(TOKENIZER_FILE)
        .map_err(|e| EmbedderError::HfHubError(e.to_string()))?;

    // Verify checksums (skip if not configured)
    if !MODEL_BLAKE3.is_empty() {
        verify_checksum(&model_path, MODEL_BLAKE3)?;
    }
    if !TOKENIZER_BLAKE3.is_empty() {
        verify_checksum(&tokenizer_path, TOKENIZER_BLAKE3)?;
    }

    Ok((model_path, tokenizer_path))
}

/// Verify file checksum using blake3
fn verify_checksum(path: &Path, expected: &str) -> Result<(), EmbedderError> {
    let mut file =
        std::fs::File::open(path).map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| EmbedderError::ModelNotFound(e.to_string()))?;
    let actual = hasher.finalize().to_hex().to_string();

    if actual != expected {
        return Err(EmbedderError::ChecksumMismatch {
            path: path.display().to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

/// Select the best available execution provider
fn select_provider() -> ExecutionProvider {
    use ort::ep::{TensorRT, CUDA};

    // Try CUDA first
    let cuda = CUDA::default();
    if cuda.is_available().unwrap_or(false) {
        return ExecutionProvider::CUDA { device_id: 0 };
    }

    // Try TensorRT
    let tensorrt = TensorRT::default();
    if tensorrt.is_available().unwrap_or(false) {
        return ExecutionProvider::TensorRT { device_id: 0 };
    }

    ExecutionProvider::CPU
}

/// Create an ort session with the specified provider
fn create_session(
    model_path: &Path,
    provider: ExecutionProvider,
) -> Result<Session, EmbedderError> {
    use ort::ep::{TensorRT, CUDA};

    let builder = Session::builder()?;

    let session = match provider {
        ExecutionProvider::CUDA { device_id } => builder
            .with_execution_providers([CUDA::default().with_device_id(device_id).build()])?
            .commit_from_file(model_path)?,
        ExecutionProvider::TensorRT { device_id } => {
            builder
                .with_execution_providers([
                    TensorRT::default().with_device_id(device_id).build(),
                    // Fallback to CUDA for unsupported ops
                    CUDA::default().with_device_id(device_id).build(),
                ])?
                .commit_from_file(model_path)?
        }
        ExecutionProvider::CPU => builder.commit_from_file(model_path)?,
    };

    Ok(session)
}

/// Pad 2D sequences to a fixed length
fn pad_2d_i64(inputs: &[Vec<i64>], max_len: usize, pad_value: i64) -> Array2<i64> {
    let batch_size = inputs.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, seq) in inputs.iter().enumerate() {
        for (j, &val) in seq.iter().take(max_len).enumerate() {
            arr[[i, j]] = val;
        }
    }
    arr
}

/// L2 normalize a vector
fn normalize_l2(v: Vec<f32>) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm == 0.0 {
        v
    } else {
        v.into_iter().map(|x| x / norm).collect()
    }
}
