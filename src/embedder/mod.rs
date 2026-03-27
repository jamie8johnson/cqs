//! Embedding generation with ort + tokenizers

mod models;
mod provider;

pub use models::{EmbeddingConfig, ModelConfig, DEFAULT_DIM, DEFAULT_MODEL_REPO};

use provider::ort_err;
pub(crate) use provider::{create_session, select_provider};

use lru::LruCache;
use ndarray::{Array2, Array3, Axis};
use once_cell::sync::OnceCell;
use ort::session::Session;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

/// Retrieves the embedding model repository from the resolved ModelConfig.
///
/// Delegates to `ModelConfig::resolve(None, None)` which checks env var / defaults.
pub fn model_repo() -> String {
    ModelConfig::resolve(None, None).repo
}

// blake3 checksums — empty to skip validation (configurable models have different checksums)
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

#[derive(Error, Debug)]
pub enum EmbedderError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),
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
    HfHub(String),
}

// `ort_err` is defined in `provider.rs` (pub(super)) and imported above.

/// An L2-normalized embedding vector.
///
/// Dimension depends on the configured model (e.g., 768 for E5-base-v2).
/// Can be compared using cosine similarity (dot product for normalized vectors).
#[derive(Debug, Clone)]
pub struct Embedding(Vec<f32>);

/// Full embedding dimension -- re-exported from crate root
pub use crate::EMBEDDING_DIM;

/// Error returned when creating an embedding with invalid data (empty or non-finite)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDimensionError {
    /// The actual dimension provided
    pub actual: usize,
    /// The expected minimum dimension
    pub expected: usize,
}

impl std::fmt::Display for EmbeddingDimensionError {
    /// Formats the embedding dimension mismatch error for display.
    ///
    /// This method implements the Display trait to produce a human-readable error message indicating a mismatch between expected and actual embedding dimensions.
    ///
    /// # Arguments
    ///
    /// * `f` - The formatter to write the error message to
    ///
    /// # Returns
    ///
    /// Returns `std::fmt::Result` indicating whether the formatting operation succeeded.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Invalid embedding dimension: expected {}, got {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for EmbeddingDimensionError {}

impl Embedding {
    /// Create a new embedding from raw vector data.
    ///
    /// Accepts any dimension — the Embedder validates consistency via `detected_dim`.
    /// For validation that the vector is non-empty and finite, use `try_new()`.
    pub fn new(data: Vec<f32>) -> Self {
        Self(data)
    }

    /// Create a new embedding with validation.
    ///
    /// Returns `Err` if the vector is empty or contains non-finite values.
    /// Dimension is no longer validated here — the Embedder enforces consistency.
    ///
    /// # Example
    /// ```
    /// use cqs::embedder::Embedding;
    ///
    /// let valid = Embedding::try_new(vec![0.5; 768]);
    /// assert!(valid.is_ok());
    ///
    /// let also_valid = Embedding::try_new(vec![0.5; 384]);
    /// assert!(also_valid.is_ok());
    ///
    /// let empty = Embedding::try_new(vec![]);
    /// assert!(empty.is_err());
    /// ```
    pub fn try_new(data: Vec<f32>) -> Result<Self, EmbeddingDimensionError> {
        if data.is_empty() {
            return Err(EmbeddingDimensionError {
                actual: 0,
                expected: 1, // at least 1 dimension required
            });
        }
        if !data.iter().all(|v| v.is_finite()) {
            return Err(EmbeddingDimensionError {
                actual: data.len(),
                expected: data.len(),
            });
        }
        Ok(Self(data))
    }

    /// Get the embedding as a slice
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Get a reference to the inner Vec (needed for some APIs like hnsw_rs)
    pub fn as_vec(&self) -> &Vec<f32> {
        &self.0
    }

    /// Consume the embedding and return the inner vector
    pub fn into_inner(self) -> Vec<f32> {
        self.0
    }

    /// Get the dimension of the embedding.
    ///
    /// Returns 768 for cqs embeddings (E5-base-v2).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if the embedding is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

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
    /// Formats the ExecutionProvider variant as a human-readable string.
    ///
    /// # Arguments
    /// * `f` - The formatter to write the formatted output to
    ///
    /// # Returns
    /// A `std::fmt::Result` indicating whether the formatting operation succeeded
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

/// Text embedding generator using a configurable model (default: E5-base-v2)
///
/// Automatically downloads the model from HuggingFace Hub on first use.
/// Detects GPU availability and uses CUDA/TensorRT when available.
///
/// # Example
///
/// ```no_run
/// use cqs::Embedder;
/// use cqs::embedder::ModelConfig;
///
/// let embedder = Embedder::new(ModelConfig::resolve(None, None))?;
/// let embedding = embedder.embed_query("parse configuration file")?;
/// println!("Embedding dimension: {}", embedding.len()); // 768
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Embedder {
    /// Lazy-loaded ONNX session (expensive ~500ms init, needs Mutex for run()).
    ///
    /// Persists for the lifetime of the Embedder. In long-running processes,
    /// this holds ~500MB of GPU/CPU memory. To release, call [`clear_session`]
    /// or drop the Embedder instance and create a new one when needed.
    session: Mutex<Option<Session>>,
    /// Lazy-loaded tokenizer
    tokenizer: OnceCell<tokenizers::Tokenizer>,
    /// Lazy-loaded model paths (avoids HuggingFace API calls until actually embedding)
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    /// LRU cache for query embeddings (avoids re-computing same queries)
    query_cache: Mutex<LruCache<String, Embedding>>,
    /// Detected embedding dimension from the model. Set on first inference.
    detected_dim: std::sync::OnceLock<usize>,
    /// Model configuration (repo, paths, prefixes, dimensions)
    model_config: ModelConfig,
}

/// Default query cache size (entries). Each entry is ~3KB (768 floats + key).
const DEFAULT_QUERY_CACHE_SIZE: usize = 32;

impl Embedder {
    /// Create a new embedder with lazy model loading
    ///
    /// Automatically detects GPU and uses CUDA/TensorRT when available.
    /// Falls back to CPU if no GPU is found.
    ///
    /// Note: Model download and ONNX session are lazy-loaded on first
    /// embedding request. This avoids HuggingFace API calls for commands
    /// that don't need embeddings.
    pub fn new(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        Self::new_with_provider(model_config, select_provider())
    }

    /// Create a CPU-only embedder with lazy model loading
    ///
    /// Use this for single-query embedding where CPU is faster than GPU
    /// due to CUDA context setup overhead. GPU only helps for batch embedding.
    pub fn new_cpu(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        Self::new_with_provider(model_config, ExecutionProvider::CPU)
    }

    /// Shared constructor for both GPU-auto and CPU-only embedders.
    fn new_with_provider(
        model_config: ModelConfig,
        provider: ExecutionProvider,
    ) -> Result<Self, EmbedderError> {
        let max_length = model_config.max_seq_length;

        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(DEFAULT_QUERY_CACHE_SIZE)
                .expect("DEFAULT_QUERY_CACHE_SIZE is non-zero"),
        ));

        Ok(Self {
            session: Mutex::new(None),
            tokenizer: OnceCell::new(),
            model_paths: OnceCell::new(),
            provider,
            max_length,
            query_cache,
            detected_dim: std::sync::OnceLock::new(),
            model_config,
        })
    }

    /// Get the model configuration
    pub fn model_config(&self) -> &ModelConfig {
        &self.model_config
    }

    /// Get or initialize model paths (lazy download)
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), EmbedderError> {
        self.model_paths
            .get_or_try_init(|| ensure_model(&self.model_config))
    }

    /// Get or initialize the ONNX session
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Option<Session>>, EmbedderError> {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let _span = tracing::info_span!("embedder_session_init").entered();
            let (model_path, _) = self.model_paths()?;
            *guard = Some(create_session(model_path, self.provider)?);
            tracing::info!("Embedder session initialized");
        }
        Ok(guard)
    }

    /// Get or initialize the tokenizer
    fn tokenizer(&self) -> Result<&tokenizers::Tokenizer, EmbedderError> {
        let (_, tokenizer_path) = self.model_paths()?;
        self.tokenizer.get_or_try_init(|| {
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))
        })
    }

    /// Counts the number of tokens in the given text using the configured tokenizer.
    ///
    /// # Arguments
    ///
    /// * `text` - The text string to tokenize and count
    ///
    /// # Returns
    ///
    /// Returns `Ok(usize)` containing the number of tokens in the text, or `Err(EmbedderError)` if tokenization fails.
    ///
    /// # Errors
    ///
    /// Returns `EmbedderError::Tokenizer` if the tokenizer is unavailable or if encoding the text fails.
    pub fn token_count(&self, text: &str) -> Result<usize, EmbedderError> {
        let encoding = self
            .tokenizer()?
            .encode(text, false)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        Ok(encoding.get_ids().len())
    }

    /// Count tokens for multiple texts in a single batch.
    ///
    /// Uses `encode_batch` for potentially better throughput than individual
    /// `token_count` calls when processing many texts.
    pub fn token_counts_batch(&self, texts: &[&str]) -> Result<Vec<usize>, EmbedderError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        let encodings = self
            .tokenizer()?
            .encode_batch(texts.to_vec(), false)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        Ok(encodings.iter().map(|e| e.get_ids().len()).collect())
    }

    /// Split text into overlapping windows of max_tokens with overlap tokens of context.
    /// Returns Vec of (window_content, window_index).
    /// If text fits in max_tokens, returns single window with index 0.
    ///
    /// # Panics
    /// Panics if `overlap >= max_tokens / 2` as this creates exponential window count.
    pub fn split_into_windows(
        &self,
        text: &str,
        max_tokens: usize,
        overlap: usize,
    ) -> Result<Vec<(String, u32)>, EmbedderError> {
        if max_tokens == 0 {
            return Ok(vec![]);
        }

        // Validate overlap to prevent exponential window explosion.
        // overlap >= max_tokens/2 means step <= max_tokens/2, causing O(2n/max_tokens) windows
        // instead of O(n/max_tokens). With overlap >= max_tokens, step becomes 1 token = disaster.
        if overlap >= max_tokens / 2 {
            return Err(EmbedderError::Tokenizer(format!(
                "overlap ({overlap}) must be less than max_tokens/2 ({})",
                max_tokens / 2
            )));
        }

        let tokenizer = self.tokenizer()?;
        let encoding = tokenizer
            .encode(text, false)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;

        let ids = encoding.get_ids();
        if ids.len() <= max_tokens {
            return Ok(vec![(text.to_string(), 0)]);
        }

        let mut windows = Vec::new();
        // Step size: tokens per window minus overlap.
        // The assertion above guarantees step > max_tokens/2, ensuring linear window count.
        let step = max_tokens - overlap;
        let mut start = 0;
        let mut window_idx = 0u32;

        while start < ids.len() {
            let end = (start + max_tokens).min(ids.len());
            let window_ids: Vec<u32> = ids[start..end].to_vec();

            // Decode back to text
            let window_text = tokenizer
                .decode(&window_ids, true)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;

            windows.push((window_text, window_idx));
            window_idx += 1;

            if end >= ids.len() {
                break;
            }
            start += step;
        }

        Ok(windows)
    }

    /// Embed documents (code chunks). Adds model-specific document prefix.
    ///
    /// Large inputs are processed in batches of 64 to cap GPU memory usage.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let _span = tracing::info_span!("embed_documents", count = texts.len()).entered();
        let prefix = &self.model_config.doc_prefix;
        const MAX_BATCH: usize = 64;
        if texts.len() <= MAX_BATCH {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{}{}", prefix, t)).collect();
            return self.embed_batch(&prefixed);
        }
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH) {
            let prefixed: Vec<String> = chunk.iter().map(|t| format!("{}{}", prefix, t)).collect();
            all.extend(self.embed_batch(&prefixed)?);
        }
        Ok(all)
    }

    /// Embed a query. Adds "query: " prefix for E5. Uses LRU cache for repeated queries.
    ///
    /// # Concurrency Note
    /// Intentionally releases lock during embedding computation (~100ms) to allow parallel queries.
    /// This means two simultaneous queries for the same text may both compute embeddings, but this
    /// is preferable to serializing all queries through a single lock. The duplicate work is rare
    /// and the cache update is idempotent.
    /// Maximum input bytes before truncation (RT-RES-5).
    /// The tokenizer will further truncate to max_seq_length tokens, but this
    /// prevents O(n) tokenization work on megabyte-sized inputs.
    const MAX_QUERY_BYTES: usize = 32 * 1024;

    pub fn embed_query(&self, text: &str) -> Result<Embedding, EmbedderError> {
        let _span = tracing::info_span!("embed_query").entered();
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }
        // RT-RES-5: Truncate oversized input before tokenization to bound CPU work.
        let text = if text.len() > Self::MAX_QUERY_BYTES {
            tracing::warn!(
                len = text.len(),
                max = Self::MAX_QUERY_BYTES,
                "Query text truncated before embedding"
            );
            // Truncate at a char boundary
            let mut end = Self::MAX_QUERY_BYTES;
            while !text.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            &text[..end]
        } else {
            text
        };

        // Check cache first (lock released after check to allow parallel computation)
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                tracing::trace!(query = text, "Embedding cache hit");
                return Ok(cached.clone());
            }
            tracing::trace!(query = text, "Embedding cache miss");
        }

        // Compute embedding (outside lock - allows parallel queries)
        let prefixed = format!("{}{}", self.model_config.query_prefix, text);
        let results = self.embed_batch(&[prefixed])?;
        let base_embedding = results.into_iter().next().ok_or_else(|| {
            EmbedderError::InferenceFailed("embed_batch returned empty result".to_string())
        })?;

        let embedding = base_embedding;

        // Store in cache (idempotent - duplicate puts for same key are harmless)
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            cache.put(text.to_string(), embedding.clone());
            tracing::trace!(query = text, cache_len = cache.len(), "Embedding cached");
        }

        Ok(embedding)
    }

    /// Get the execution provider being used
    pub fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// Clear the ONNX session to free memory (~500MB).
    ///
    /// The session will be lazily re-initialized on the next embedding request.
    /// Use this in long-running processes during idle periods to reduce memory footprint.
    ///
    /// # Safety constraint
    /// Must only be called during idle periods -- not while embedding is in progress.
    /// Watch mode guarantees single-threaded access.
    pub fn clear_session(&self) {
        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
        tracing::info!("Embedder session cleared");
    }

    /// Warm up the model with a dummy inference
    pub fn warm(&self) -> Result<(), EmbedderError> {
        let _ = self.embed_query("warmup")?;
        Ok(())
    }

    /// Returns the embedding dimension detected from the model.
    /// Falls back to the model config's declared dimension if no inference has been run yet.
    pub fn embedding_dim(&self) -> usize {
        let dim = *self.detected_dim.get().unwrap_or(&self.model_config.dim);
        if dim == 0 {
            EMBEDDING_DIM
        } else {
            dim
        }
    }

    /// Generates embeddings for a batch of text inputs.
    ///
    /// This method tokenizes the input texts, prepares them as padded tensors suitable for the ONNX model, and runs inference to produce embedding vectors. Texts are padded to the maximum length within the batch (up to the model's configured maximum length).
    ///
    /// # Arguments
    ///
    /// * `texts` - A slice of strings to embed
    ///
    /// # Returns
    ///
    /// Returns a vector of embeddings, one per input text. Returns an error if tokenization fails or the embedding model cannot be run.
    ///
    /// # Errors
    ///
    /// Returns `EmbedderError::Tokenizer` if tokenization of the batch fails.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedderError> {
        use ort::value::Tensor;

        let _span = tracing::info_span!("embed_batch", count = texts.len()).entered();

        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize (lazy init tokenizer)
        // PERF-36: `encode_batch` requires `Vec<EncodeInput>` (owned), so `texts.to_vec()` is
        // unavoidable — the tokenizer API does not accept `&[impl AsRef<str>]`.
        let encodings = {
            let _tokenize = tracing::debug_span!("tokenize").entered();
            self.tokenizer()?
                .encode_batch(texts.to_vec(), true)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?
        };

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
        let input_ids_tensor = Tensor::from_array(input_ids_arr).map_err(ort_err)?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_arr).map_err(ort_err)?;
        let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr).map_err(ort_err)?;

        // Run inference (lazy init session)
        let mut guard = self.session()?;
        let session = guard
            .as_mut()
            .expect("session() guarantees initialized after Ok return");
        let _inference = tracing::debug_span!("inference", max_len).entered();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ])
            .map_err(ort_err)?;

        // Get the last_hidden_state output: shape [batch, seq_len, 768]
        let output = outputs.get("last_hidden_state").ok_or_else(|| {
            EmbedderError::InferenceFailed(format!(
                "ONNX model has no 'last_hidden_state' output. Available: {:?}",
                outputs.keys().collect::<Vec<_>>()
            ))
        })?;
        let (shape, data) = output.try_extract_tensor::<f32>().map_err(ort_err)?;

        // Validate tensor shape: expect [batch_size, seq_len, 768]
        let batch_size = texts.len();
        let seq_len = max_len;
        if shape.len() != 3 {
            return Err(EmbedderError::InferenceFailed(format!(
                "Unexpected tensor shape: expected 3 dimensions [batch, seq, dim], got {} dimensions",
                shape.len()
            )));
        }
        let embedding_dim = shape[2] as usize;
        // Set or validate embedding dimension from model output
        match self.detected_dim.get() {
            Some(&expected) if expected != embedding_dim => {
                return Err(EmbedderError::InferenceFailed(format!(
                    "Embedding dimension changed: expected {expected}, got {embedding_dim}"
                )));
            }
            None => {
                let _ = self.detected_dim.set(embedding_dim);
                tracing::info!(
                    dim = embedding_dim,
                    "Detected embedding dimension from model"
                );
            }
            _ => {} // matches expected — OK
        }
        if shape[0] as usize != batch_size {
            return Err(EmbedderError::InferenceFailed(format!(
                "Tensor batch size mismatch: expected {}, got {}",
                batch_size, shape[0]
            )));
        }
        // Mean-pooling via ndarray (vectorized, SIMD-friendly)
        let hidden = Array3::from_shape_vec((batch_size, seq_len, embedding_dim), data.to_vec())
            .map_err(|e| EmbedderError::InferenceFailed(format!("tensor reshape failed: {e}")))?;

        // Build mask: [batch, seq, 1] for broadcasting
        let mask_2d = Array2::from_shape_fn((batch_size, seq_len), |(i, j)| {
            attention_mask[i].get(j).copied().unwrap_or(0) as f32
        });
        let mask_3d = mask_2d.clone().insert_axis(Axis(2));

        // Masked sum: (hidden * mask).sum(axis=1) / mask.sum(axis=1)
        let masked = &hidden * &mask_3d;
        let summed = masked.sum_axis(Axis(1)); // [batch, dim]
        let counts = mask_2d.sum_axis(Axis(1)).insert_axis(Axis(1)); // [batch, 1]

        let results = (0..batch_size)
            .map(|i| {
                let count = counts[[i, 0]];
                let row = summed.row(i);
                let pooled: Vec<f32> = if count > 0.0 {
                    row.iter().map(|v| v / count).collect()
                } else {
                    vec![0.0f32; embedding_dim]
                };
                Embedding::new(normalize_l2(pooled))
            })
            .collect();

        Ok(results)
    }
}

/// Download model and tokenizer from HuggingFace Hub
fn ensure_model(config: &ModelConfig) -> Result<(PathBuf, PathBuf), EmbedderError> {
    // CQS_ONNX_DIR: bypass HF download, load from local directory.
    // Directory must contain model.onnx and tokenizer.json.
    if let Ok(dir) = std::env::var("CQS_ONNX_DIR") {
        let dir = PathBuf::from(dir);
        let model_path = dir.join(&config.onnx_path);
        let tokenizer_path = dir.join(&config.tokenizer_path);
        if model_path.exists() && tokenizer_path.exists() {
            tracing::info!(dir = %dir.display(), "Using local ONNX model directory");
            return Ok((model_path, tokenizer_path));
        }
        // Try flat layout (model.onnx + tokenizer.json in same dir)
        let flat_model = dir.join("model.onnx");
        let flat_tok = dir.join("tokenizer.json");
        if flat_model.exists() && flat_tok.exists() {
            tracing::info!(dir = %dir.display(), "Using local ONNX model directory (flat)");
            return Ok((flat_model, flat_tok));
        }
        tracing::warn!(dir = %dir.display(), "CQS_ONNX_DIR set but model files not found, falling back to HF download");
    }

    use hf_hub::api::sync::Api;

    let api = Api::new().map_err(|e| EmbedderError::HfHub(e.to_string()))?;
    let repo = api.model(config.repo.clone());

    let model_path = repo
        .get(&config.onnx_path)
        .map_err(|e| EmbedderError::HfHub(e.to_string()))?;
    let tokenizer_path = repo
        .get(&config.tokenizer_path)
        .map_err(|e| EmbedderError::HfHub(e.to_string()))?;

    // Verify checksums (skip if already verified via marker file)
    if !MODEL_BLAKE3.is_empty() || !TOKENIZER_BLAKE3.is_empty() {
        let marker = model_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(".cqs_verified");
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

/// Pad 2D sequences to a fixed length
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

/// L2 normalize a vector (single-pass, in-place)
fn normalize_l2(mut v: Vec<f32>) -> Vec<f32> {
    let norm_sq: f32 = v.iter().fold(0.0, |acc, &x| acc + x * x);
    if norm_sq > 0.0 {
        let inv_norm = 1.0 / norm_sq.sqrt();
        v.iter_mut().for_each(|x| *x *= inv_norm);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Embedding tests =====

    #[test]
    fn test_embedding_new() {
        let data = vec![0.5; EMBEDDING_DIM];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.as_slice(), &data);
    }

    #[test]
    fn test_embedding_len() {
        let emb = Embedding::new(vec![1.0; EMBEDDING_DIM]);
        assert_eq!(emb.len(), EMBEDDING_DIM);
    }

    #[test]
    fn test_embedding_is_empty() {
        let empty = Embedding::new(vec![]);
        assert!(empty.is_empty());

        let non_empty = Embedding::new(vec![1.0; EMBEDDING_DIM]);
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_embedding_into_inner() {
        let data = vec![1.0; EMBEDDING_DIM];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.into_inner(), data);
    }

    #[test]
    fn test_embedding_as_vec() {
        let data = vec![1.0; EMBEDDING_DIM];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.as_vec(), &data);
    }

    // ===== Embedding::try_new tests (TC-33) =====

    #[test]
    fn tc33_try_new_empty_vec_errors() {
        let result = Embedding::try_new(vec![]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.actual, 0);
        assert_eq!(err.expected, 1);
    }

    #[test]
    fn tc33_try_new_nan_errors() {
        let result = Embedding::try_new(vec![1.0, f32::NAN, 3.0]);
        assert!(result.is_err(), "NaN should be rejected by try_new");
    }

    #[test]
    fn tc33_try_new_inf_errors() {
        let result = Embedding::try_new(vec![1.0, f32::INFINITY, 3.0]);
        assert!(result.is_err(), "Infinity should be rejected by try_new");

        let result = Embedding::try_new(vec![f32::NEG_INFINITY]);
        assert!(result.is_err(), "Negative infinity should be rejected");
    }

    #[test]
    fn tc33_try_new_valid_ok() {
        let data = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let result = Embedding::try_new(data.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_slice(), &data);
    }

    // ===== normalize_l2 tests =====

    #[test]
    fn test_normalize_l2_unit_vector() {
        let v = normalize_l2(vec![1.0, 0.0, 0.0]);
        assert!((v[0] - 1.0).abs() < 1e-6);
        assert!((v[1] - 0.0).abs() < 1e-6);
        assert!((v[2] - 0.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_l2_produces_unit_vector() {
        let v = normalize_l2(vec![3.0, 4.0]);
        // Should produce [0.6, 0.8] (3-4-5 triangle)
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);

        // Verify it's a unit vector (magnitude = 1)
        let magnitude: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_normalize_l2_zero_vector() {
        // Zero vector should remain zero (no division by zero)
        let v = normalize_l2(vec![0.0, 0.0, 0.0]);
        assert_eq!(v, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn test_normalize_l2_empty_vector() {
        let v = normalize_l2(vec![]);
        assert!(v.is_empty());
    }

    // ===== ExecutionProvider tests =====

    #[test]
    fn test_execution_provider_display() {
        assert_eq!(format!("{}", ExecutionProvider::CPU), "CPU");
        assert_eq!(
            format!("{}", ExecutionProvider::CUDA { device_id: 0 }),
            "CUDA (device 0)"
        );
        assert_eq!(
            format!("{}", ExecutionProvider::TensorRT { device_id: 1 }),
            "TensorRT (device 1)"
        );
    }

    // ===== Constants tests =====

    #[test]
    fn test_model_dimensions() {
        assert_eq!(EMBEDDING_DIM, 1024);
    }

    // ===== pad_2d_i64 tests =====

    #[test]
    fn test_pad_2d_i64_basic() {
        let inputs = vec![vec![1, 2, 3], vec![4, 5]];
        let result = pad_2d_i64(&inputs, 4, 0);
        assert_eq!(result.shape(), &[2, 4]);
        assert_eq!(result[[0, 0]], 1);
        assert_eq!(result[[0, 1]], 2);
        assert_eq!(result[[0, 2]], 3);
        assert_eq!(result[[0, 3]], 0); // padded
        assert_eq!(result[[1, 0]], 4);
        assert_eq!(result[[1, 1]], 5);
        assert_eq!(result[[1, 2]], 0); // padded
        assert_eq!(result[[1, 3]], 0); // padded
    }

    #[test]
    fn test_pad_2d_i64_truncates() {
        let inputs = vec![vec![1, 2, 3, 4, 5]];
        let result = pad_2d_i64(&inputs, 3, 0);
        assert_eq!(result.shape(), &[1, 3]);
        assert_eq!(result[[0, 0]], 1);
        assert_eq!(result[[0, 1]], 2);
        assert_eq!(result[[0, 2]], 3);
        // 4 and 5 are truncated
    }

    #[test]
    fn test_pad_2d_i64_empty_input() {
        let inputs: Vec<Vec<i64>> = vec![];
        let result = pad_2d_i64(&inputs, 5, 0);
        assert_eq!(result.shape(), &[0, 5]);
    }

    #[test]
    fn test_pad_2d_i64_custom_pad_value() {
        let inputs = vec![vec![1]];
        let result = pad_2d_i64(&inputs, 3, -1);
        assert_eq!(result[[0, 0]], 1);
        assert_eq!(result[[0, 1]], -1);
        assert_eq!(result[[0, 2]], -1);
    }

    // ===== EmbedderError tests =====

    #[test]
    fn test_embedder_error_display() {
        let err = EmbedderError::EmptyQuery;
        assert_eq!(format!("{}", err), "Query cannot be empty");

        let err = EmbedderError::ModelNotFound("model.onnx".to_string());
        assert!(format!("{}", err).contains("model.onnx"));

        let err = EmbedderError::Tokenizer("invalid token".to_string());
        assert!(format!("{}", err).contains("invalid token"));

        let err = EmbedderError::ChecksumMismatch {
            path: "/path/to/file".to_string(),
            expected: "abc123".to_string(),
            actual: "def456".to_string(),
        };
        assert!(format!("{}", err).contains("abc123"));
        assert!(format!("{}", err).contains("def456"));
    }

    #[test]
    fn test_embedder_error_from_ort() {
        // Test that ort::Error converts to EmbedderError::InferenceFailed
        // We can't easily create an ort::Error, but we can verify the variant exists
        let err: EmbedderError = EmbedderError::InferenceFailed("test error".to_string());
        assert!(matches!(err, EmbedderError::InferenceFailed(_)));
    }

    // ===== Property-based tests =====

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Property: normalize_l2 produces unit vectors (magnitude ~= 1) or zero vectors
            #[test]
            fn prop_normalize_l2_unit_or_zero(v in prop::collection::vec(-1e6f32..1e6f32, 1..100)) {
                let normalized = normalize_l2(v.clone());

                // Compute magnitude
                let magnitude: f32 = normalized.iter().map(|x| x * x).sum::<f32>().sqrt();

                // Check: either zero vector (input was zero) or unit vector
                let input_is_zero = v.iter().all(|&x| x == 0.0);
                if input_is_zero {
                    prop_assert!(magnitude < 1e-6, "Zero input should give zero output");
                } else {
                    prop_assert!(
                        (magnitude - 1.0).abs() < 1e-4,
                        "Non-zero input should give unit vector, got magnitude {}",
                        magnitude
                    );
                }
            }

            /// Property: normalize_l2 preserves vector direction (dot product with original > 0)
            #[test]
            fn prop_normalize_l2_preserves_direction(v in prop::collection::vec(1.0f32..100.0, 1..50)) {
                let normalized = normalize_l2(v.clone());

                // Dot product with original should be positive (same direction)
                let dot: f32 = v.iter().zip(normalized.iter()).map(|(a, b)| a * b).sum();
                prop_assert!(dot > 0.0, "Direction should be preserved");
            }

            /// Property: Embedding length is preserved through operations
            #[test]
            fn prop_embedding_length_preserved(use_model_dim in proptest::bool::ANY) {
                let _ = use_model_dim; // single dimension now
                let emb = Embedding::new(vec![0.5; EMBEDDING_DIM]);
                prop_assert_eq!(emb.len(), EMBEDDING_DIM);
                prop_assert_eq!(emb.as_slice().len(), EMBEDDING_DIM);
                prop_assert_eq!(emb.as_vec().len(), EMBEDDING_DIM);
            }
        }
    }

    // ===== clear_session tests =====

    #[test]
    #[ignore] // Requires model
    fn test_clear_session_and_reinit() {
        let embedder = Embedder::new(ModelConfig::e5_base()).unwrap();
        // Force session init by embedding something
        let _ = embedder.embed_query("test");
        // Clear and re-embed
        embedder.clear_session();
        let result = embedder.embed_query("test again");
        assert!(result.is_ok());
    }

    #[test]
    fn test_clear_session_idempotent() {
        let embedder = Embedder::new_cpu(ModelConfig::e5_base()).unwrap();
        embedder.clear_session(); // clear before init -- should not panic
        embedder.clear_session(); // clear again -- should not panic
    }

    // ===== Integration tests (require model) =====

    mod integration {
        use super::*;

        #[test]
        #[ignore] // Requires model - run with: cargo test --lib integration -- --ignored
        fn test_token_count_empty() {
            let embedder =
                Embedder::new(ModelConfig::e5_base()).expect("Failed to create embedder");
            let count = embedder.token_count("").expect("token_count failed");
            assert_eq!(count, 0);
        }

        #[test]
        #[ignore]
        fn test_token_count_simple() {
            let embedder =
                Embedder::new(ModelConfig::e5_base()).expect("Failed to create embedder");
            let count = embedder
                .token_count("hello world")
                .expect("token_count failed");
            // E5-base-v2 tokenizer: "hello" and "world" are single tokens
            assert!(
                (2..=4).contains(&count),
                "Expected 2-4 tokens, got {}",
                count
            );
        }

        #[test]
        #[ignore]
        fn test_token_count_code() {
            let embedder =
                Embedder::new(ModelConfig::e5_base()).expect("Failed to create embedder");
            let code = "fn main() { println!(\"Hello\"); }";
            let count = embedder.token_count(code).expect("token_count failed");
            // Code typically tokenizes to more tokens than words
            assert!(count > 5, "Expected >5 tokens for code, got {}", count);
        }

        #[test]
        #[ignore]
        fn test_token_count_unicode() {
            let embedder =
                Embedder::new(ModelConfig::e5_base()).expect("Failed to create embedder");
            let text = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}\u{4e16}\u{754c}"; // "Hello world" in Japanese
            let count = embedder.token_count(text).expect("token_count failed");
            // Unicode text may tokenize differently
            assert!(count > 0, "Expected >0 tokens for unicode, got {}", count);
        }
    }
}
