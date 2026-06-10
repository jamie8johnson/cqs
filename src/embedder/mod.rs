//! Embedding generation with ort + tokenizers

mod core;
mod download;
pub mod models;
mod pooling;
mod provider;

pub use models::{EmbeddingConfig, InputNames, ModelConfig, ModelInfo, PoolingStrategy};

/// Default embedding dimension (compile-time mirror of `ModelConfig::DEFAULT_DIM`).
/// A `pub const` for `pub const EMBEDDING_DIM` in `lib.rs` and other `pub const`
/// consumers. Sourced from the `default = true` row in `define_embedder_presets!`.
pub const DEFAULT_DIM: usize = ModelConfig::DEFAULT_DIM;

/// Default model repo as a `&'static str` (compile-time mirror of
/// `ModelConfig::DEFAULT_REPO`). For store/metadata callers that want a
/// `&'static str` rather than `default_model().repo` (a `String`).
pub const DEFAULT_MODEL_REPO: &str = ModelConfig::DEFAULT_REPO;

pub(crate) use provider::{create_session, select_provider};

use thiserror::Error;

// blake3 checksums — empty to skip validation (configurable models have different checksums)
const MODEL_BLAKE3: &str = "";
const TOKENIZER_BLAKE3: &str = "";

pub use core::Embedder;
pub(crate) use download::ensure_model;
pub(crate) use pooling::{
    cls_pool, last_token_pool, mean_pool, normalize_l2, pad_2d_i64_from_encodings,
    truncate_at_char_boundary,
};
// Test-only: `pad_2d_i64` is exercised by unit tests but has no production
// caller (production uses `pad_2d_i64_from_encodings`). Gating the re-export
// keeps the non-test build warning-free.
#[cfg(test)]
pub(crate) use pooling::pad_2d_i64;

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
    /// HuggingFace Hub model download failure.
    ///
    /// Mirrors [`crate::reranker::RerankerError::ModelDownload`] — both wrap the
    /// same `hf_hub::ApiError` shape and emit the same display string, so a
    /// shared error handler can pattern-match on a single variant name across
    /// the embedder + reranker boundary.
    #[error("Model download failed: {0}")]
    ModelDownload(String),
}

/// Route a stringified ORT message into
/// [`InferenceFailed`](EmbedderError::InferenceFailed) so the shared
/// [`crate::ort_helpers::ort_err`] helper can hand back the right
/// variant for embedder call sites. Sealed trait, not `From<String>`,
/// so `.map_err(ort_err)` type inference isn't ambiguous against the
/// reflexive `From<T> for T` impl.
impl crate::ort_helpers::FromOrtMessage for EmbedderError {
    fn from_ort_message(msg: String) -> Self {
        Self::InferenceFailed(msg)
    }
}

/// An L2-normalized embedding vector.
///
/// Dimension depends on the configured model (e.g., 768 for EmbeddingGemma, 1024 for BGE-large).
/// Compared using cosine similarity (dot product for normalized vectors).
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

// NOTE: new() is the common path (internal, known-good data). try_new() is for untrusted input.
impl Embedding {
    /// Create a new embedding from raw vector data (unchecked).
    ///
    /// Accepts any dimension — the Embedder validates consistency via `detected_dim`.
    /// **Prefer `try_new()` for untrusted input** (external APIs, deserialized data).
    /// Use `new()` only when the data is known-good (e.g., fresh from ONNX inference).
    pub fn new(data: Vec<f32>) -> Self {
        Self(data)
    }

    /// Create a new embedding with validation.
    ///
    /// Returns `Err` if the vector is empty or contains non-finite values.
    /// Dimension is not validated here — the Embedder enforces consistency.
    ///
    /// # Example
    /// ```
    /// use cqs::embedder::Embedding;
    ///
    /// let valid = Embedding::try_new(vec![0.5; 1024]);
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
    /// Returns the number of dimensions (e.g., 1024 for BGE-large, 768 for E5-base).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Check if the embedding is empty
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Hardware execution provider for inference.
///
/// Variants for non-NVIDIA backends are gated behind the matching `ep-*`
/// cargo features so a build with no GPU support doesn't drag in unused enum
/// arms or downstream match-arm scaffolding. CUDA and TensorRT are
/// unconditional because the `ort` crate's `cuda` and `tensorrt` features are
/// always enabled on Linux/Windows (see
/// `[target.'cfg(not(target_os = "macos"))'.dependencies]` in `Cargo.toml`).
#[derive(Debug, Clone, Copy)]
pub enum ExecutionProvider {
    /// NVIDIA CUDA (requires CUDA toolkit)
    CUDA { device_id: i32 },
    /// NVIDIA TensorRT (faster than CUDA, requires TensorRT)
    TensorRT { device_id: i32 },
    /// Apple CoreML (Metal/Neural Engine on M-series). Requires
    /// `--features ep-coreml` and a macOS target.
    #[cfg(feature = "ep-coreml")]
    CoreML,
    /// AMD ROCm (HIP-based GPU compute). Requires `--features ep-rocm`
    /// and ROCm-enabled `ort` binaries.
    #[cfg(feature = "ep-rocm")]
    ROCm { device_id: i32 },
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
            #[cfg(feature = "ep-coreml")]
            ExecutionProvider::CoreML => write!(f, "CoreML"),
            #[cfg(feature = "ep-rocm")]
            ExecutionProvider::ROCm { device_id } => write!(f, "ROCm (device {})", device_id),
            ExecutionProvider::CPU => write!(f, "CPU"),
        }
    }
}
