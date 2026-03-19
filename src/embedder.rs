//! Embedding generation with ort + tokenizers

use lru::LruCache;
use ndarray::{Array2, Array3, Axis};
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

/// Convert any ort error to [`EmbedderError::InferenceFailed`] via `.to_string()`.
///
/// Call sites use `.map_err(ort_err)` instead of `?` auto-conversion because
/// a blanket `From` impl would conflict with other error conversions.
fn ort_err<T>(e: ort::Error<T>) -> EmbedderError {
    EmbedderError::InferenceFailed(e.to_string())
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
    /// Create a new embedding from raw vector data.
    ///
    /// Logs a warning if the dimension doesn't match the expected 769.
    /// For strict validation, use `try_new()` which returns an error.
    pub fn new(data: Vec<f32>) -> Self {
        if data.len() != crate::EMBEDDING_DIM && data.len() != MODEL_DIM {
            tracing::warn!(
                expected = crate::EMBEDDING_DIM,
                actual = data.len(),
                "Embedding dimension mismatch — may cause incorrect similarity scores"
            );
        }
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
/// let embedder = Embedder::new()?;
/// let embedding = embedder.embed_query("parse configuration file")?;
/// println!("Embedding dimension: {}", embedding.len()); // 769
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
            session: Mutex::new(None),
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
            session: Mutex::new(None),
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
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;
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
    ///
    /// Large inputs are processed in batches of 64 to cap GPU memory usage.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let _span = tracing::info_span!("embed_documents", count = texts.len()).entered();
        const MAX_BATCH: usize = 64;
        if texts.len() <= MAX_BATCH {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {}", t)).collect();
            return self.embed_batch(&prefixed);
        }
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH) {
            let prefixed: Vec<String> = chunk.iter().map(|t| format!("passage: {}", t)).collect();
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
                tracing::trace!(query = text, "Embedding cache hit");
                return Ok(cached.clone());
            }
            tracing::trace!(query = text, "Embedding cache miss");
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
    /// Must only be called during idle periods — not while embedding is in progress.
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
        if embedding_dim != 768 {
            return Err(EmbedderError::InferenceFailed(format!(
                "Unexpected embedding dimension: expected 768, got {}",
                embedding_dim
            )));
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

/// Ensure ORT CUDA provider libraries are findable (Unix only)
///
/// ORT's C++ runtime resolves provider paths via `dladdr` → `argv[0]`.
/// With static linking and PATH invocation, `argv[0]` is the bare binary
/// name (e.g., "cqs"), so ORT constructs `absolute("cqs").remove_filename()`
/// = CWD. Providers must exist there for `dlopen` to succeed.
///
/// Strategy: compute the same directory ORT will search (from argv[0]),
/// and create symlinks from the ORT cache there. Symlinks are cleaned up
/// on process exit.
#[cfg(target_os = "linux")]
fn ensure_ort_provider_libs() {
    let ort_lib_dir = match find_ort_provider_dir() {
        Some(d) => d,
        None => return,
    };

    let provider_libs = [
        "libonnxruntime_providers_shared.so",
        "libonnxruntime_providers_cuda.so",
        "libonnxruntime_providers_tensorrt.so",
    ];

    // Compute the directory ORT's GetRuntimePath() will resolve to.
    // ORT does: dladdr() → dli_fname (= argv[0] on glibc) →
    //   std::filesystem::absolute(dli_fname).remove_filename()
    // For PATH invocation: argv[0]="cqs" → absolute = CWD/"cqs" → parent = CWD
    let ort_search_dir = match ort_runtime_search_dir() {
        Some(d) => d,
        None => return,
    };

    symlink_providers(&ort_lib_dir, &ort_search_dir, &provider_libs);

    // Collect all symlink paths for cleanup
    let mut cleanup_paths: Vec<PathBuf> = provider_libs
        .iter()
        .map(|lib| ort_search_dir.join(lib))
        .collect();

    // Also symlink into LD_LIBRARY_PATH for other search paths
    if let Some(ld_dir) = find_ld_library_dir(&ort_lib_dir) {
        symlink_providers(&ort_lib_dir, &ld_dir, &provider_libs);
        cleanup_paths.extend(provider_libs.iter().map(|lib| ld_dir.join(lib)));
    }

    // Register cleanup for ALL symlinked paths (both directories)
    register_provider_cleanup(cleanup_paths);
}

/// Compute the directory ORT's GetRuntimePath() will resolve to.
///
/// Reproduces ORT's logic: `dladdr` returns `dli_fname = argv[0]` (glibc),
/// then `std::filesystem::absolute(dli_fname).remove_filename()`.
#[cfg(target_os = "linux")]
fn ort_runtime_search_dir() -> Option<PathBuf> {
    // Read argv[0] the same way glibc's dladdr does
    let cmdline = std::fs::read("/proc/self/cmdline").ok()?;
    let argv0_end = cmdline.iter().position(|&b| b == 0)?;
    let argv0 = std::str::from_utf8(&cmdline[..argv0_end]).ok()?;

    // If argv[0] is already absolute, parent is the binary's directory
    let abs_path = if argv0.starts_with('/') {
        PathBuf::from(argv0)
    } else {
        // Relative: resolve against CWD (same as std::filesystem::absolute)
        std::env::current_dir().ok()?.join(argv0)
    };

    abs_path.parent().map(|p| p.to_path_buf())
}

/// Find the ORT provider library cache directory
#[cfg(target_os = "linux")]
fn find_ort_provider_dir() -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?;
    let triplet = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        _ => return None,
    };
    let ort_cache = cache_dir.join(format!("ort.pyke.io/dfbin/{triplet}"));

    match std::fs::read_dir(&ort_cache) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .next(),
        Err(e) => {
            tracing::debug!(path = %ort_cache.display(), error = %e, "ORT cache not found");
            None
        }
    }
}

/// Find a writable directory from LD_LIBRARY_PATH (excluding the ORT cache)
#[cfg(target_os = "linux")]
fn find_ld_library_dir(ort_lib_dir: &Path) -> Option<PathBuf> {
    let ld_path = std::env::var("LD_LIBRARY_PATH").unwrap_or_default();
    let ort_cache_str = ort_lib_dir.to_string_lossy();
    ld_path
        .split(':')
        .find(|p| !p.is_empty() && Path::new(p).is_dir() && !ort_cache_str.starts_with(p))
        .map(PathBuf::from)
}

/// Create symlinks for provider libraries in the target directory
#[cfg(target_os = "linux")]
fn symlink_providers(src_dir: &Path, target_dir: &Path, libs: &[&str]) {
    for lib in libs {
        let src = src_dir.join(lib);
        let dst = target_dir.join(lib);

        if !src.exists() {
            continue;
        }

        // Skip if symlink already points to the right place.
        // Canonicalize both paths so relative vs absolute and symlink chains
        // don't cause false mismatches (PB-10).
        if let Ok(existing) = std::fs::read_link(&dst) {
            let existing_canon = dunce::canonicalize(&existing).unwrap_or(existing);
            let src_canon = dunce::canonicalize(&src).unwrap_or_else(|_| src.clone());
            if existing_canon == src_canon {
                continue;
            }
            let _ = std::fs::remove_file(&dst);
        }

        if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
            tracing::debug!("Failed to symlink {}: {}", lib, e);
        }
    }
}

/// Register atexit cleanup for provider symlinks.
/// Uses Mutex to support paths from multiple directories.
#[cfg(target_os = "linux")]
fn register_provider_cleanup(paths: Vec<PathBuf>) {
    use std::sync::Mutex;

    static CLEANUP_PATHS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    if let Ok(mut guard) = CLEANUP_PATHS.lock() {
        guard.extend(paths);
    }

    // Register atexit handler only once
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        extern "C" fn cleanup() {
            // Note: remove_file may allocate. Acceptable for CLI tool that exits normally.
            if let Ok(paths) = CLEANUP_PATHS.lock() {
                for path in paths.iter() {
                    if path.symlink_metadata().is_ok() && std::fs::read_link(path).is_ok() {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        }
        unsafe { libc::atexit(cleanup) };
    });
}

/// No-op on non-Linux platforms (CUDA provider libs handled differently)
#[cfg(not(target_os = "linux"))]
fn ensure_ort_provider_libs() {
    // No-op: Windows and other platforms find CUDA/TensorRT provider libraries
    // via PATH, so no symlinking is needed. The Unix version symlinks .so files
    // into ort's search directory because LD_LIBRARY_PATH may not include them.
}

/// Cached GPU provider detection result
static CACHED_PROVIDER: OnceCell<ExecutionProvider> = OnceCell::new();

/// Select the best available execution provider (cached)
///
/// Provider detection is expensive (checks CUDA/TensorRT availability).
/// Result is cached in a static OnceCell for subsequent calls.
pub(crate) fn select_provider() -> ExecutionProvider {
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
pub(crate) fn create_session(
    model_path: &Path,
    provider: ExecutionProvider,
) -> Result<Session, EmbedderError> {
    use ort::ep::{TensorRT, CUDA};

    let mut builder = Session::builder().map_err(ort_err)?;

    let session = match provider {
        ExecutionProvider::CUDA { device_id } => builder
            .with_execution_providers([CUDA::default().with_device_id(device_id).build()])
            .map_err(ort_err)?
            .commit_from_file(model_path)
            .map_err(ort_err)?,
        ExecutionProvider::TensorRT { device_id } => {
            builder
                .with_execution_providers([
                    TensorRT::default().with_device_id(device_id).build(),
                    // Fallback to CUDA for unsupported ops
                    CUDA::default().with_device_id(device_id).build(),
                ])
                .map_err(ort_err)?
                .commit_from_file(model_path)
                .map_err(ort_err)?
        }
        ExecutionProvider::CPU => builder.commit_from_file(model_path).map_err(ort_err)?,
    };

    Ok(session)
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
        let emb = Embedding::new(vec![1.0; MODEL_DIM]);
        assert_eq!(emb.len(), MODEL_DIM);
    }

    #[test]
    fn test_embedding_is_empty() {
        let empty = Embedding::new(vec![]);
        assert!(empty.is_empty());

        let non_empty = Embedding::new(vec![1.0; EMBEDDING_DIM]);
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
        let emb = Embedding::new(vec![0.5; MODEL_DIM]);
        assert_eq!(emb.sentiment(), None);

        let emb = Embedding::new(vec![]);
        assert_eq!(emb.sentiment(), None);
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
                    prop_assert!((-1.0..=1.0).contains(&s), "Sentiment {} out of range", s);
                }
            }

            /// Property: Embedding length is preserved through operations
            #[test]
            fn prop_embedding_length_preserved(use_model_dim in proptest::bool::ANY) {
                let len = if use_model_dim { MODEL_DIM } else { EMBEDDING_DIM };
                let emb = Embedding::new(vec![0.5; len]);
                prop_assert_eq!(emb.len(), len);
                prop_assert_eq!(emb.as_slice().len(), len);
                prop_assert_eq!(emb.as_vec().len(), len);
            }
        }
    }

    // ===== clear_session tests =====

    #[test]
    #[ignore] // Requires model
    fn test_clear_session_and_reinit() {
        let embedder = Embedder::new().unwrap();
        // Force session init by embedding something
        let _ = embedder.embed_query("test");
        // Clear and re-embed
        embedder.clear_session();
        let result = embedder.embed_query("test again");
        assert!(result.is_ok());
    }

    #[test]
    fn test_clear_session_idempotent() {
        let embedder = Embedder::new_cpu().unwrap();
        embedder.clear_session(); // clear before init — should not panic
        embedder.clear_session(); // clear again — should not panic
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
                (2..=4).contains(&count),
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
