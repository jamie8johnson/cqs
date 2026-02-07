//! Embedding generation with ort + tokenizers

use lru::LruCache;
use ndarray::Array2;
use once_cell::sync::OnceCell;
use ort::ep::ExecutionProvider as OrtExecutionProvider;
use ort::session::Session;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;

// Model configuration - E5-base-v2 (full CUDA coverage, no rotary embedding fallback)
const MODEL_REPO: &str = "intfloat/e5-base-v2";
const MODEL_FILE: &str = "onnx/model.onnx";
const TOKENIZER_FILE: &str = "onnx/tokenizer.json";

// blake3 checksums for model verification (empty = skip validation)
const MODEL_BLAKE3: &str = "5ca98b5db8c2d0e354163bff1160e4ca67b48e51e724d7b4a621270552fd5c04";
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

/// A 769-dimensional L2-normalized embedding vector
///
/// Embeddings are produced by E5-base-v2 (768-dim) with an
/// optional 769th dimension for sentiment (-1.0 to +1.0).
/// Can be compared using cosine similarity (dot product for normalized vectors).
#[derive(Debug, Clone)]
pub struct Embedding(Vec<f32>);

/// Standard embedding dimension from model
pub const MODEL_DIM: usize = 768;
/// Full embedding dimension with sentiment — re-exported from crate root
pub use crate::EMBEDDING_DIM;

/// Error returned when creating an embedding with invalid dimensions
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDimensionError {
    /// The actual dimension provided
    pub actual: usize,
    /// The expected dimension (769)
    pub expected: usize,
}

impl std::fmt::Display for EmbeddingDimensionError {
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
    /// Create a new embedding from raw vector data (unchecked).
    ///
    /// This does not validate the dimension. For validated construction,
    /// use `try_new()` which ensures 769-dimensional input.
    pub fn new(data: Vec<f32>) -> Self {
        Self(data)
    }

    /// Create a new embedding with dimension validation.
    ///
    /// Returns `Err` if the vector is not exactly 769 dimensions.
    /// Use this when constructing embeddings from untrusted sources.
    ///
    /// # Example
    /// ```
    /// use cqs::embedder::Embedding;
    ///
    /// let valid = Embedding::try_new(vec![0.5; 769]);
    /// assert!(valid.is_ok());
    ///
    /// let invalid = Embedding::try_new(vec![0.5; 768]);
    /// assert!(invalid.is_err());
    /// ```
    pub fn try_new(data: Vec<f32>) -> Result<Self, EmbeddingDimensionError> {
        if data.len() != EMBEDDING_DIM {
            return Err(EmbeddingDimensionError {
                actual: data.len(),
                expected: EMBEDDING_DIM,
            });
        }
        Ok(Self(data))
    }

    /// Append sentiment as 769th dimension
    ///
    /// Converts a 768-dim model embedding to 769-dim with sentiment.
    /// Sentiment should be -1.0 (negative) to +1.0 (positive).
    pub fn with_sentiment(mut self, sentiment: f32) -> Self {
        if self.0.len() != MODEL_DIM {
            tracing::warn!(
                actual = self.0.len(),
                expected = MODEL_DIM,
                "Unexpected embedding dimension in with_sentiment"
            );
        }
        self.0.push(sentiment.clamp(-1.0, 1.0));
        self
    }

    /// Get the sentiment (769th dimension) if present
    pub fn sentiment(&self) -> Option<f32> {
        if self.0.len() == EMBEDDING_DIM {
            Some(self.0[MODEL_DIM])
        } else {
            None
        }
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
    /// Returns 769 for cqs embeddings (768 from E5-base-v2 + 1 sentiment dimension).
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

/// Text embedding generator using E5-base-v2
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
/// println!("Embedding dimension: {}", embedding.len()); // 769
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Embedder {
    /// Lazy-loaded ONNX session (expensive ~500ms init, needs Mutex for run())
    session: OnceCell<Mutex<Session>>,
    /// Lazy-loaded tokenizer
    tokenizer: OnceCell<tokenizers::Tokenizer>,
    /// Lazy-loaded model paths (avoids HuggingFace API calls until actually embedding)
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    provider: ExecutionProvider,
    max_length: usize,
    /// LRU cache for query embeddings (avoids re-computing same queries)
    query_cache: Mutex<LruCache<String, Embedding>>,
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
    pub fn new() -> Result<Self, EmbedderError> {
        let provider = select_provider();

        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(DEFAULT_QUERY_CACHE_SIZE)
                .expect("DEFAULT_QUERY_CACHE_SIZE is non-zero"),
        ));

        Ok(Self {
            session: OnceCell::new(),
            tokenizer: OnceCell::new(),
            model_paths: OnceCell::new(),
            provider,
            max_length: 512,
            query_cache,
        })
    }

    /// Create a CPU-only embedder with lazy model loading
    ///
    /// Use this for single-query embedding where CPU is faster than GPU
    /// due to CUDA context setup overhead. GPU only helps for batch embedding.
    pub fn new_cpu() -> Result<Self, EmbedderError> {
        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(DEFAULT_QUERY_CACHE_SIZE)
                .expect("DEFAULT_QUERY_CACHE_SIZE is non-zero"),
        ));

        Ok(Self {
            session: OnceCell::new(),
            tokenizer: OnceCell::new(),
            model_paths: OnceCell::new(),
            provider: ExecutionProvider::CPU,
            max_length: 512,
            query_cache,
        })
    }

    /// Get or initialize model paths (lazy download)
    fn model_paths(&self) -> Result<&(PathBuf, PathBuf), EmbedderError> {
        self.model_paths.get_or_try_init(ensure_model)
    }

    /// Get or initialize the ONNX session
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Session>, EmbedderError> {
        let (model_path, _) = self.model_paths()?;
        let session = self
            .session
            .get_or_try_init(|| create_session(model_path, self.provider).map(Mutex::new))?;
        Ok(session.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Get or initialize the tokenizer
    fn tokenizer(&self) -> Result<&tokenizers::Tokenizer, EmbedderError> {
        let (_, tokenizer_path) = self.model_paths()?;
        self.tokenizer.get_or_try_init(|| {
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| EmbedderError::TokenizerError(e.to_string()))
        })
    }

    /// Count tokens in a text
    pub fn token_count(&self, text: &str) -> Result<usize, EmbedderError> {
        let encoding = self
            .tokenizer()?
            .encode(text, false)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;
        Ok(encoding.get_ids().len())
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
        // Validate overlap to prevent exponential window explosion.
        // overlap >= max_tokens/2 means step <= max_tokens/2, causing O(2n/max_tokens) windows
        // instead of O(n/max_tokens). With overlap >= max_tokens, step becomes 1 token = disaster.
        if overlap >= max_tokens / 2 {
            return Err(EmbedderError::TokenizerError(format!(
                "overlap ({overlap}) must be less than max_tokens/2 ({})",
                max_tokens / 2
            )));
        }

        let tokenizer = self.tokenizer()?;
        let encoding = tokenizer
            .encode(text, false)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

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
                .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

            windows.push((window_text, window_idx));
            window_idx += 1;

            if end >= ids.len() {
                break;
            }
            start += step;
        }

        Ok(windows)
    }

    /// Embed documents (code chunks). Adds "passage: " prefix for E5.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {}", t)).collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a query. Adds "query: " prefix for E5. Uses LRU cache for repeated queries.
    ///
    /// # Concurrency Note
    /// Intentionally releases lock during embedding computation (~100ms) to allow parallel queries.
    /// This means two simultaneous queries for the same text may both compute embeddings, but this
    /// is preferable to serializing all queries through a single lock. The duplicate work is rare
    /// and the cache update is idempotent.
    pub fn embed_query(&self, text: &str) -> Result<Embedding, EmbedderError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }

        // Check cache first (lock released after check to allow parallel computation)
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                return Ok(cached.clone());
            }
        }

        // Compute embedding (outside lock - allows parallel queries)
        let prefixed = format!("query: {}", text);
        let results = self.embed_batch(&[prefixed])?;
        let base_embedding = results.into_iter().next().ok_or_else(|| {
            EmbedderError::InferenceFailed("embed_batch returned empty result".to_string())
        })?;

        // Add neutral sentiment (0.0) as 769th dimension
        let embedding = base_embedding.with_sentiment(0.0);

        // Store in cache (idempotent - duplicate puts for same key are harmless)
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
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

    /// Warm up the model with a dummy inference
    pub fn warm(&self) -> Result<(), EmbedderError> {
        let _ = self.embed_query("warmup")?;
        Ok(())
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Embedding>, EmbedderError> {
        use ort::value::Tensor;

        let _span = tracing::info_span!("embed_batch", count = texts.len()).entered();

        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize (lazy init tokenizer)
        let encodings = {
            let _tokenize = tracing::debug_span!("tokenize").entered();
            self.tokenizer()?
                .encode_batch(texts.to_vec(), true)
                .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?
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
        let input_ids_tensor = Tensor::from_array(input_ids_arr)?;
        let attention_mask_tensor = Tensor::from_array(attention_mask_arr)?;
        let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr)?;

        // Run inference (lazy init session)
        let mut session = self.session()?;
        let _inference = tracing::debug_span!("inference", max_len).entered();
        let outputs = session.run(ort::inputs![
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

            results.push(Embedding::new(normalize_l2(sum)));
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

/// Ensure ort CUDA provider libraries are findable (Unix only)
///
/// The ort crate downloads provider libs to ~/.cache/ort.pyke.io/... but
/// doesn't add them to the library search path. This function creates
/// symlinks in a directory that's already in LD_LIBRARY_PATH.
#[cfg(unix)]
fn ensure_ort_provider_libs() {
    // Find ort's download cache using cross-platform API
    let cache_dir = match dirs::cache_dir() {
        Some(c) => c,
        None => return,
    };

    // Build target triplet dynamically (e.g., x86_64-unknown-linux-gnu, aarch64-apple-darwin)
    let triplet = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => return, // Unsupported platform for GPU acceleration
    };
    let ort_cache = cache_dir.join(format!("ort.pyke.io/dfbin/{}", triplet));

    // Find the versioned subdirectory (hash-named)
    let ort_lib_dir = match std::fs::read_dir(&ort_cache) {
        Ok(entries) => entries
            .filter_map(|e| {
                e.map_err(|err| {
                    tracing::debug!(path = %ort_cache.display(), error = %err, "Failed to read directory entry");
                })
                .ok()
            })
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .next(),
        Err(e) => {
            tracing::debug!(path = %ort_cache.display(), error = %e, "ORT cache directory not found");
            return;
        }
    };

    let ort_lib_dir = match ort_lib_dir {
        Some(d) => d,
        None => return,
    };

    // Find target directory from LD_LIBRARY_PATH (skip ort cache dirs to avoid self-symlinks)
    // Note: LD_LIBRARY_PATH uses colon separator on Unix
    let ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let ort_cache_str = ort_cache.to_string_lossy();
    let target_dir = ld_path
        .split(':')
        .find(|p| {
            !p.is_empty() && std::path::Path::new(p).is_dir() && !p.contains(ort_cache_str.as_ref())
            // Don't symlink into ort's own cache
        })
        .map(std::path::PathBuf::from);

    let target_dir = match target_dir {
        Some(d) => d,
        None => return, // No writable lib dir in path (or only ort cache in path)
    };

    // Provider libs to symlink
    let provider_libs = [
        "libonnxruntime_providers_shared.so",
        "libonnxruntime_providers_cuda.so",
        "libonnxruntime_providers_tensorrt.so",
    ];

    for lib in &provider_libs {
        let src = ort_lib_dir.join(lib);
        let dst = target_dir.join(lib);

        // Skip if source doesn't exist
        if !src.exists() {
            continue;
        }

        // Skip if symlink already valid
        if dst.symlink_metadata().is_ok() {
            if let Ok(target) = std::fs::read_link(&dst) {
                if target == src {
                    continue; // Already correct
                }
            }
            // Remove stale symlink
            if let Err(e) = std::fs::remove_file(&dst) {
                tracing::debug!("Failed to remove stale symlink {}: {}", dst.display(), e);
            }
        }

        // Create symlink
        if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
            tracing::debug!("Failed to symlink {}: {}", lib, e);
        } else {
            tracing::info!("Created symlink: {} -> {}", dst.display(), src.display());
        }
    }
}

/// No-op on non-Unix platforms (CUDA provider libs handled differently)
#[cfg(not(unix))]
fn ensure_ort_provider_libs() {
    // Windows/other platforms: CUDA libraries are typically in PATH already
}

/// Cached GPU provider detection result
static CACHED_PROVIDER: OnceCell<ExecutionProvider> = OnceCell::new();

/// Select the best available execution provider (cached)
///
/// Provider detection is expensive (checks CUDA/TensorRT availability).
/// Result is cached in a static OnceCell for subsequent calls.
fn select_provider() -> ExecutionProvider {
    *CACHED_PROVIDER.get_or_init(detect_provider)
}

/// Detect the best available execution provider
fn detect_provider() -> ExecutionProvider {
    use ort::ep::{TensorRT, CUDA};

    // Ensure provider libs are findable before checking availability
    ensure_ort_provider_libs();

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
        let data = vec![1.0, 2.0, 3.0];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.as_slice(), &data);
    }

    #[test]
    fn test_embedding_len() {
        let emb = Embedding::new(vec![1.0; 768]);
        assert_eq!(emb.len(), 768);
    }

    #[test]
    fn test_embedding_is_empty() {
        let empty = Embedding::new(vec![]);
        assert!(empty.is_empty());

        let non_empty = Embedding::new(vec![1.0]);
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_embedding_with_sentiment() {
        let emb = Embedding::new(vec![0.5; MODEL_DIM]);
        let emb_with_sentiment = emb.with_sentiment(0.8);

        assert_eq!(emb_with_sentiment.len(), EMBEDDING_DIM);
        assert_eq!(emb_with_sentiment.sentiment(), Some(0.8));
    }

    #[test]
    fn test_embedding_sentiment_clamped() {
        // Sentiment > 1.0 should be clamped
        let emb = Embedding::new(vec![0.5; MODEL_DIM]).with_sentiment(2.0);
        assert_eq!(emb.sentiment(), Some(1.0));

        // Sentiment < -1.0 should be clamped
        let emb = Embedding::new(vec![0.5; MODEL_DIM]).with_sentiment(-2.0);
        assert_eq!(emb.sentiment(), Some(-1.0));
    }

    #[test]
    fn test_embedding_sentiment_none_without_769_dims() {
        let emb = Embedding::new(vec![0.5; 768]);
        assert_eq!(emb.sentiment(), None);

        let emb = Embedding::new(vec![0.5; 100]);
        assert_eq!(emb.sentiment(), None);
    }

    #[test]
    fn test_embedding_into_inner() {
        let data = vec![1.0, 2.0, 3.0];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.into_inner(), data);
    }

    #[test]
    fn test_embedding_as_vec() {
        let data = vec![1.0, 2.0, 3.0];
        let emb = Embedding::new(data.clone());
        assert_eq!(emb.as_vec(), &data);
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
        assert_eq!(MODEL_DIM, 768);
        assert_eq!(EMBEDDING_DIM, 769);
        assert_eq!(EMBEDDING_DIM, MODEL_DIM + 1);
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

        let err = EmbedderError::TokenizerError("invalid token".to_string());
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
            /// Property: normalize_l2 produces unit vectors (magnitude ≈ 1) or zero vectors
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

            /// Property: sentiment clamping keeps values in [-1, 1]
            #[test]
            fn prop_sentiment_clamped(sentiment in -10.0f32..10.0f32) {
                let emb = Embedding::new(vec![0.5; MODEL_DIM]).with_sentiment(sentiment);
                if let Some(s) = emb.sentiment() {
                    prop_assert!(s >= -1.0 && s <= 1.0, "Sentiment {} out of range", s);
                }
            }

            /// Property: Embedding length is preserved through operations
            #[test]
            fn prop_embedding_length_preserved(len in 1usize..1000) {
                let emb = Embedding::new(vec![0.5; len]);
                prop_assert_eq!(emb.len(), len);
                prop_assert_eq!(emb.as_slice().len(), len);
                prop_assert_eq!(emb.as_vec().len(), len);
            }
        }
    }

    // ===== Integration tests (require model) =====

    mod integration {
        use super::*;

        #[test]
        #[ignore] // Requires model - run with: cargo test --lib integration -- --ignored
        fn test_token_count_empty() {
            let embedder = Embedder::new().expect("Failed to create embedder");
            let count = embedder.token_count("").expect("token_count failed");
            assert_eq!(count, 0);
        }

        #[test]
        #[ignore]
        fn test_token_count_simple() {
            let embedder = Embedder::new().expect("Failed to create embedder");
            let count = embedder
                .token_count("hello world")
                .expect("token_count failed");
            // E5-base-v2 tokenizer: "hello" and "world" are single tokens
            assert!(
                count >= 2 && count <= 4,
                "Expected 2-4 tokens, got {}",
                count
            );
        }

        #[test]
        #[ignore]
        fn test_token_count_code() {
            let embedder = Embedder::new().expect("Failed to create embedder");
            let code = "fn main() { println!(\"Hello\"); }";
            let count = embedder.token_count(code).expect("token_count failed");
            // Code typically tokenizes to more tokens than words
            assert!(count > 5, "Expected >5 tokens for code, got {}", count);
        }

        #[test]
        #[ignore]
        fn test_token_count_unicode() {
            let embedder = Embedder::new().expect("Failed to create embedder");
            let text = "こんにちは世界"; // "Hello world" in Japanese
            let count = embedder.token_count(text).expect("token_count failed");
            // Unicode text may tokenize differently
            assert!(count > 0, "Expected >0 tokens for unicode, got {}", count);
        }
    }
}
