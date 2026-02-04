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
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

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
/// Full embedding dimension with sentiment
pub const EMBEDDING_DIM: usize = 769;

impl Embedding {
    /// Create a new embedding from raw vector data
    pub fn new(data: Vec<f32>) -> Self {
        Self(data)
    }

    /// Append sentiment as 769th dimension
    ///
    /// Converts a 768-dim model embedding to 769-dim with sentiment.
    /// Sentiment should be -1.0 (negative) to +1.0 (positive).
    pub fn with_sentiment(mut self, sentiment: f32) -> Self {
        debug_assert_eq!(self.0.len(), MODEL_DIM, "Expected 768-dim embedding");
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

    /// Get the dimension of the embedding
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
/// println!("Embedding dimension: {}", embedding.len()); // 768
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Embedder {
    /// Lazy-loaded ONNX session (expensive ~500ms init, needs Mutex for run())
    session: OnceCell<Mutex<Session>>,
    /// Lazy-loaded tokenizer
    tokenizer: OnceCell<tokenizers::Tokenizer>,
    /// Cached model paths
    model_path: PathBuf,
    tokenizer_path: PathBuf,
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
    ///
    /// Note: ONNX session is lazy-loaded on first embedding request (~500ms).
    pub fn new() -> Result<Self, EmbedderError> {
        let (model_path, tokenizer_path) = ensure_model()?;
        let provider = select_provider();

        let batch_size = match provider {
            ExecutionProvider::CPU => 4,
            _ => 16,
        };

        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(100).expect("100 is non-zero"),
        ));

        Ok(Self {
            session: OnceCell::new(),
            tokenizer: OnceCell::new(),
            model_path,
            tokenizer_path,
            provider,
            max_length: 512,
            batch_size,
            query_cache,
        })
    }

    /// Create a CPU-only embedder
    ///
    /// Use this for single-query embedding where CPU is faster than GPU
    /// due to CUDA context setup overhead. GPU only helps for batch embedding.
    pub fn new_cpu() -> Result<Self, EmbedderError> {
        let (model_path, tokenizer_path) = ensure_model()?;

        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(100).expect("100 is non-zero"),
        ));

        Ok(Self {
            session: OnceCell::new(),
            tokenizer: OnceCell::new(),
            model_path,
            tokenizer_path,
            provider: ExecutionProvider::CPU,
            max_length: 512,
            batch_size: 4,
            query_cache,
        })
    }

    /// Get or initialize the ONNX session
    fn session(&self) -> Result<std::sync::MutexGuard<'_, Session>, EmbedderError> {
        let session = self
            .session
            .get_or_try_init(|| create_session(&self.model_path, self.provider).map(Mutex::new))?;
        Ok(session.lock().unwrap_or_else(|p| p.into_inner()))
    }

    /// Get or initialize the tokenizer
    fn tokenizer(&self) -> Result<&tokenizers::Tokenizer, EmbedderError> {
        self.tokenizer.get_or_try_init(|| {
            tokenizers::Tokenizer::from_file(&self.tokenizer_path)
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
    pub fn split_into_windows(
        &self,
        text: &str,
        max_tokens: usize,
        overlap: usize,
    ) -> Result<Vec<(String, u32)>, EmbedderError> {
        let tokenizer = self.tokenizer()?;
        let encoding = tokenizer
            .encode(text, false)
            .map_err(|e| EmbedderError::TokenizerError(e.to_string()))?;

        let ids = encoding.get_ids();
        if ids.len() <= max_tokens {
            return Ok(vec![(text.to_string(), 0)]);
        }

        let mut windows = Vec::new();
        let step = max_tokens.saturating_sub(overlap).max(1); // Ensure step >= 1 to prevent infinite loop
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
    pub fn embed_documents(&mut self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("passage: {}", t)).collect();
        self.embed_batch(&prefixed)
    }

    /// Embed a query. Adds "query: " prefix for E5. Uses LRU cache for repeated queries.
    pub fn embed_query(&mut self, text: &str) -> Result<Embedding, EmbedderError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }

        // Check cache first
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::debug!("Query cache lock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                return Ok(cached.clone());
            }
        }

        // Compute embedding
        let prefixed = format!("query: {}", text);
        let results = self.embed_batch(&[prefixed])?;
        let base_embedding = results
            .into_iter()
            .next()
            .expect("embed_batch with single item always returns one result");

        // Add neutral sentiment (0.0) as 769th dimension
        let embedding = base_embedding.with_sentiment(0.0);

        // Store in cache
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::debug!("Query cache lock poisoned, recovering");
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

        let _span = tracing::info_span!("embed_batch", count = texts.len()).entered();

        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize (lazy init tokenizer)
        let encodings = self
            .tokenizer()?
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

        // Run inference (lazy init session)
        let mut session = self.session()?;
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

/// Ensure ort CUDA provider libraries are findable
///
/// The ort crate downloads provider libs to ~/.cache/ort.pyke.io/... but
/// doesn't add them to the library search path. This function creates
/// symlinks in a directory that's already in LD_LIBRARY_PATH.
fn ensure_ort_provider_libs() {
    // Find ort's download cache
    let home = match std::env::var("HOME") {
        Ok(h) => std::path::PathBuf::from(h),
        Err(_) => return,
    };
    let ort_cache = home.join(".cache/ort.pyke.io/dfbin/x86_64-unknown-linux-gnu");

    // Find the versioned subdirectory (hash-named)
    let ort_lib_dir = match std::fs::read_dir(&ort_cache) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .next(),
        Err(_) => return,
    };

    let ort_lib_dir = match ort_lib_dir {
        Some(d) => d,
        None => return,
    };

    // Find target directory from LD_LIBRARY_PATH (skip ort cache dirs to avoid self-symlinks)
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
            let _ = std::fs::remove_file(&dst);
        }

        // Create symlink
        if let Err(e) = std::os::unix::fs::symlink(&src, &dst) {
            tracing::debug!("Failed to symlink {}: {}", lib, e);
        } else {
            tracing::info!("Created symlink: {} -> {}", dst.display(), src.display());
        }
    }
}

/// Select the best available execution provider
fn select_provider() -> ExecutionProvider {
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
