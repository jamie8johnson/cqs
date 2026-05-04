//! Embedding generation with ort + tokenizers

pub mod models;
mod provider;

pub use models::{EmbeddingConfig, InputNames, ModelConfig, ModelInfo, PoolingStrategy};

/// Default embedding dimension (compile-time mirror of `ModelConfig::DEFAULT_DIM`).
/// Kept as a `pub const` for `pub const EMBEDDING_DIM` in `lib.rs` and other
/// `pub const` consumers. Sourced from the `default = true` row in
/// `define_embedder_presets!`.
pub const DEFAULT_DIM: usize = ModelConfig::DEFAULT_DIM;

/// Default model repo as a `&'static str` (compile-time mirror of
/// `ModelConfig::DEFAULT_REPO`). Kept for store/metadata callers that
/// want a `&'static str` rather than `default_model().repo` (a `String`).
pub const DEFAULT_MODEL_REPO: &str = ModelConfig::DEFAULT_REPO;

use crate::ort_helpers::ort_err;
pub(crate) use provider::{create_session, select_provider};

use lru::LruCache;
use ndarray::{Array2, Array3, Axis};
use once_cell::sync::OnceCell;
use ort::session::Session;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

// CQ-V1.33.0-3: `model_repo()` was removed. It silently dropped any CLI/env
// override by calling `ModelConfig::resolve(None, None)`, which made
// `cqs doctor --model X` lie about the runtime repo (always reporting the
// compile-time default). Callers now use `model_config.repo` directly.

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

/// CQ-V1.30.1-5 (P3-CQ-2): route a stringified ORT message into
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
/// Dimension depends on the configured model (e.g., 1024 for BGE-large, 768 for E5-base).
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
    /// Dimension is no longer validated here — the Embedder enforces consistency.
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
/// Issue #956: variants for non-NVIDIA backends are gated behind the
/// matching `ep-*` cargo features so a build with no GPU support doesn't
/// drag in unused enum arms or downstream match-arm scaffolding. CUDA
/// and TensorRT are unconditional today because the `ort` crate's
/// `cuda` and `tensorrt` features are always enabled on Linux/Windows
/// (see `[target.'cfg(not(target_os = "macos"))'.dependencies]` in
/// `Cargo.toml`); a future scope split could move them behind their
/// own cargo features too.
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

/// Text embedding generator using a configurable model (default since v1.35.0: EmbeddingGemma-300m)
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
/// println!("Embedding dimension: {}", embedding.len()); // 1024 for BGE-large
/// # Ok::<(), anyhow::Error>(())
/// ```
pub struct Embedder {
    /// Lazy-loaded ONNX session (expensive ~500ms init, needs Mutex for run()).
    ///
    /// Persists for the lifetime of the Embedder. In long-running processes,
    /// this holds ~500MB of GPU/CPU memory. To release, call [`clear_session`]
    /// or drop the Embedder instance and create a new one when needed.
    session: Mutex<Option<Session>>,
    /// Lazy-loaded tokenizer.
    ///
    /// RM-V1.25-15: Stored as `Mutex<Option<Arc<Tokenizer>>>` (instead of the
    /// previous `OnceCell<Tokenizer>`) so `clear_session` can drop the
    /// tokenizer alongside the ONNX session. Accessor `tokenizer()` hands
    /// back an `Arc<Tokenizer>` clone — `Tokenizer::encode` takes `&self`,
    /// so call sites using `arc.encode(...)` still work via `Arc` deref
    /// without needing to touch the mutex during inference.
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    /// Lazy-loaded model paths (avoids HuggingFace API calls until actually embedding)
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    /// P2.75: lazy execution-provider resolution. Was a precomputed
    /// `ExecutionProvider` populated in `Embedder::new` via
    /// `select_provider()` — that function probes for CUDA, runs symlink
    /// ops, and is invoked on every CLI process even for commands that
    /// never embed (notes list, slot list, cache stats, …). The
    /// `OnceLock` defers the probe to first inference. `None` in the
    /// initial slot encodes "no provider was eagerly chosen"; a `Some`
    /// pre-populated by `new_with_provider(_, CPU)` keeps the explicit
    /// `Embedder::new_cpu` shortcut working.
    provider: std::sync::OnceLock<ExecutionProvider>,
    max_length: usize,
    /// LRU cache for query embeddings (avoids re-computing same queries)
    query_cache: Mutex<LruCache<String, Embedding>>,
    /// Disk-backed query cache (persists across CLI invocations).
    /// Best-effort: failures are logged and silently skipped.
    ///
    /// P2.92: lazily opened on first `embed_query` so commands that never
    /// touch query embeddings (`notes list`, `slot list`, `cache stats`,
    /// etc.) skip the WSL DrvFS 30-50ms cold-open + 7-day prune. The
    /// outer `OnceLock` is initialized empty in `Embedder::new`; the inner
    /// `Option` is populated on first access — `Some` if the cache opened
    /// successfully, `None` if it failed (best-effort fallback).
    disk_query_cache: std::sync::OnceLock<Option<crate::cache::QueryCache>>,
    /// Detected embedding dimension from the model. Set on first inference.
    ///
    /// DS-V1.33-7: switched from `OnceLock<usize>` to `Mutex<Option<usize>>`
    /// so [`Self::clear_session`] can reset the slot. `OnceLock` cannot be
    /// reset under `&self`, which would have left a model swap reading the
    /// first-loaded model's dim forever — silently feeding the wrong dim to
    /// `EmbeddingCache::read_batch`'s dimension filter (which then drops
    /// every cache hit on dim mismatch). Mutex contention is irrelevant: a
    /// single quick lock per `embedding_dim()` call.
    detected_dim: Mutex<Option<usize>>,
    /// Model configuration (repo, paths, prefixes, dimensions)
    model_config: ModelConfig,
    /// blake3 fingerprint of the ONNX model file, computed lazily on first access.
    /// Used as cache key to distinguish models with the same name but different weights.
    ///
    /// DS-V1.33-7: switched from `OnceLock<String>` to `Mutex<Option<String>>`
    /// so [`Self::clear_session`] can reset the slot alongside the session
    /// drop. `OnceLock` cannot be reset under `&self`, which would have
    /// left a model swap reading the first-loaded model's fingerprint —
    /// silently caching every new embedding under the wrong model_id key
    /// in the on-disk embedding cache.
    model_fingerprint: Mutex<Option<String>>,
    /// SHL-V1.29-1: Pad token id resolved at tokenizer-init time.
    ///
    /// Cache set once per embedder lifetime on first call to [`Self::pad_id`].
    /// Read order:
    ///   1. `tokenizer.get_padding().map(|p| p.pad_id)` — the tokenizer's
    ///      own declared pad id when `tokenizer.json` carries a padding
    ///      section.
    ///   2. `model_config.pad_id` — preset-declared fallback (`0` for every
    ///      shipped model).
    ///
    /// Stored as `OnceLock<i64>` so every inference call after the first
    /// pays the cheap load; the lookup goes through the tokenizer mutex
    /// once and the result sticks.
    pad_id: std::sync::OnceLock<i64>,
}

/// Default query cache size (entries). Each entry is roughly `4 * dim` bytes
/// of vector data plus the cache key; with the default BGE-large (1024-dim) that
/// is ~4 KB/entry, with E5-base / v9-200k (768-dim) it is ~3 KB/entry, and scales
/// accordingly for custom models. Override with `CQS_QUERY_CACHE_SIZE`.
const DEFAULT_QUERY_CACHE_SIZE: usize = 128;

impl Embedder {
    /// Create a new embedder with lazy model loading.
    ///
    /// When `force_cpu` is false, automatically detects GPU and uses CUDA/TensorRT
    /// when available, falling back to CPU if no GPU is found.
    /// When `force_cpu` is true, always uses CPU -- use this for single-query
    /// embedding where CPU is faster than GPU due to CUDA context setup overhead.
    ///
    /// Note: Model download and ONNX session are lazy-loaded on first
    /// embedding request. This avoids HuggingFace API calls for commands
    /// that don't need embeddings.
    ///
    /// P2.75: provider selection (CUDA probe + ORT EP symlink ops) is also
    /// deferred — see [`Self::provider`].
    pub fn new(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        Self::new_lazy_provider(model_config)
    }

    /// Create a CPU-only embedder with lazy model loading.
    ///
    /// Convenience wrapper for `new()` — use this for single-query embedding
    /// where CPU is faster than GPU due to CUDA context setup overhead.
    pub fn new_cpu(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        Self::new_with_provider(model_config, ExecutionProvider::CPU)
    }

    /// P2.75: build an embedder without resolving the execution provider.
    /// The probe runs on first inference via [`Self::provider`].
    fn new_lazy_provider(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        let mut emb = Self::new_inner(model_config)?;
        emb.provider = std::sync::OnceLock::new();
        Ok(emb)
    }

    /// Shared constructor for both GPU-auto and CPU-only embedders.
    fn new_with_provider(
        model_config: ModelConfig,
        provider: ExecutionProvider,
    ) -> Result<Self, EmbedderError> {
        let emb = Self::new_inner(model_config)?;
        // P2.75: pre-populate the OnceLock so `provider()` returns this
        // explicit choice without ever calling `select_provider()`.
        let _ = emb.provider.set(provider);
        Ok(emb)
    }

    fn new_inner(model_config: ModelConfig) -> Result<Self, EmbedderError> {
        let max_length = model_config.max_seq_length;

        let cache_size = match std::env::var("CQS_QUERY_CACHE_SIZE") {
            Ok(val) => match val.parse::<usize>() {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        size = n,
                        "Query cache size override from CQS_QUERY_CACHE_SIZE"
                    );
                    n
                }
                _ => {
                    tracing::warn!(
                        value = %val,
                        "Invalid CQS_QUERY_CACHE_SIZE (must be positive integer), using default {DEFAULT_QUERY_CACHE_SIZE}"
                    );
                    DEFAULT_QUERY_CACHE_SIZE
                }
            },
            Err(_) => DEFAULT_QUERY_CACHE_SIZE,
        };
        let query_cache = Mutex::new(LruCache::new(
            NonZeroUsize::new(cache_size).expect("cache_size is non-zero"),
        ));

        // P2.92: defer disk-cache open + 7-day prune until first `embed_query`.
        // The 16+ commands that never embed a query (notes/slot/cache/etc.) used
        // to pay 30-50ms on WSL DrvFS for a cache they never touched.

        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            model_paths: OnceCell::new(),
            // P2.75: lazy. Both `new_lazy_provider` and `new_with_provider`
            // overwrite this slot before returning.
            provider: std::sync::OnceLock::new(),
            max_length,
            query_cache,
            disk_query_cache: std::sync::OnceLock::new(),
            detected_dim: Mutex::new(None),
            model_config,
            model_fingerprint: Mutex::new(None),
            pad_id: std::sync::OnceLock::new(),
        })
    }

    /// P2.75: lazy provider accessor. Resolves on first call by running the
    /// CUDA probe, then memoises. Pre-populated by `new_with_provider` for
    /// the explicit-CPU path. Replaces the eagerly-resolved `provider`
    /// field; matches the public visibility of the previous accessor so
    /// out-of-crate callers compile unchanged.
    pub fn provider(&self) -> ExecutionProvider {
        *self
            .provider
            .get_or_init(crate::embedder::provider::select_provider)
    }

    /// Lazy accessor for the on-disk query embedding cache. Opens (and runs
    /// the 7-day prune) on first call; subsequent calls return the cached
    /// `Option<&QueryCache>`. Failure to open is non-fatal — caller treats
    /// `None` as "no disk cache available" and proceeds.
    fn disk_query_cache(&self) -> Option<&crate::cache::QueryCache> {
        self.disk_query_cache
            .get_or_init(|| {
                match crate::cache::QueryCache::open(&crate::cache::QueryCache::default_path()) {
                    Ok(c) => {
                        let _ = c.prune_older_than(7);
                        Some(c)
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "Disk query cache unavailable (non-fatal)");
                        None
                    }
                }
            })
            .as_ref()
    }

    /// Get the model configuration
    pub fn model_config(&self) -> &ModelConfig {
        &self.model_config
    }

    /// Get or compute the model fingerprint (blake3 hash of ONNX file).
    ///
    /// Computed lazily on first access. Used as cache key to distinguish
    /// models with the same name but different weights (fine-tuned, different
    /// HF revision, different ONNX export).
    pub fn model_fingerprint(&self) -> String {
        // P2.63: stable fallback fingerprint — must NOT include any value
        // that changes across process restarts. Cross-slot embedding cache
        // copy by content_hash relies on the model fingerprint matching
        // across runs, so a per-restart Unix timestamp shape would fragment
        // the cache and orphan every fallback embedding.
        fn fallback_fingerprint(repo: &str, size: u64) -> String {
            format!("{}:fallback:size={}", repo, size)
        }
        // DS-V1.33-7: lock-and-init pattern replaces the previous
        // `OnceLock::get_or_init`. Returns owned `String` (not `&str`) so
        // the value lives independently of the mutex guard. `clear_session`
        // resets the inner Option to None so a model swap re-fingerprints
        // on next access. Fast-path: lock, see Some, clone, return.
        {
            let guard = self
                .model_fingerprint
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(fp) = guard.as_ref() {
                return fp.clone();
            }
        }
        let computed: String = {
            let _span = tracing::info_span!("compute_model_fingerprint").entered();
            match self.model_paths() {
                Ok((model_path, _)) => {
                    match std::fs::metadata(model_path) {
                        Ok(meta) if meta.len() > 2 * 1024 * 1024 * 1024 => {
                            // P2.63: >2GB models skip the streaming hash (would
                            // OOM on 32-bit / RAM-constrained boxes), but the
                            // previous `repo_size_mtime` shape used wall-clock
                            // mtime — `touch model.onnx` after every download
                            // would mint a new fingerprint and orphan the cache.
                            // mtime IS stable across restarts (filesystem
                            // metadata, not wall clock at fingerprint time), so
                            // it's safe in principle, but we prefer the
                            // size-only fallback for parity with the
                            // hash-failure path below — operators see the same
                            // shape regardless of which fallback fired.
                            let fp = fallback_fingerprint(&self.model_config.repo, meta.len());
                            tracing::info!(
                                size = meta.len(),
                                "Model >2GB, using stable size-based fingerprint"
                            );
                            fp
                        }
                        _ => {
                            // v1.22.0 audit RM-1: previously `std::fs::read`
                            // loaded the entire ONNX into heap (~1.3 GB for
                            // BGE-large) just to hash it. Use streaming
                            // `update_reader` (same pattern as HNSW checksum
                            // at hnsw/persist.rs:298-306) — constant memory.
                            match std::fs::File::open(model_path) {
                                Ok(file) => {
                                    let mut hasher = blake3::Hasher::new();
                                    match hasher.update_reader(file) {
                                        Ok(_) => {
                                            let hash = hasher.finalize().to_hex().to_string();
                                            tracing::info!(
                                                hash = &hash[..16],
                                                "Model fingerprint computed (streaming)"
                                            );
                                            hash
                                        }
                                        Err(e) => {
                                            // P1.8 / P2.63: stable size-based
                                            // fallback, not timestamp — every
                                            // restart with a transient hash
                                            // failure used to mint a NEW
                                            // fingerprint and thrash the cache.
                                            // EH-V1.33-6: when metadata also
                                            // fails (the same FS hiccup that
                                            // broke the hash), distinguish by
                                            // failure mode instead of
                                            // collapsing every model under
                                            // `:fallback:size=0`.
                                            tracing::warn!(
                                                error = %e,
                                                "Failed to stream-hash model, using repo+size fallback (cache may miss until next successful hash)"
                                            );
                                            match std::fs::metadata(model_path) {
                                                Ok(m) => fallback_fingerprint(
                                                    &self.model_config.repo,
                                                    m.len(),
                                                ),
                                                Err(meta_err) => {
                                                    tracing::warn!(
                                                        error = %meta_err,
                                                        "Failed to stat model for size fallback after hash failure; using no-stat fingerprint"
                                                    );
                                                    format!(
                                                        "{}:fallback:no-stat",
                                                        self.model_config.repo
                                                    )
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    // P1.8 / P2.63: stable size-based fallback (see above).
                                    // EH-V1.33-6: when metadata also fails,
                                    // emit a no-stat sentinel so distinct
                                    // models don't all share `:fallback:size=0`.
                                    tracing::warn!(
                                        error = %e,
                                        "Failed to open model for fingerprint, using repo+size fallback"
                                    );
                                    match std::fs::metadata(model_path) {
                                        Ok(m) => {
                                            fallback_fingerprint(&self.model_config.repo, m.len())
                                        }
                                        Err(meta_err) => {
                                            tracing::warn!(
                                                error = %meta_err,
                                                "Failed to stat model for size fallback after open failure; using no-stat fingerprint"
                                            );
                                            format!("{}:fallback:no-stat", self.model_config.repo)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    // P1.8: model path resolution failed entirely — no path to
                    // stat — but `:fallback:no-path` is still deterministic
                    // (does not vary by wall-clock).
                    tracing::warn!(
                        error = %e,
                        "Failed to get model paths for fingerprint, using repo-only fallback"
                    );
                    format!("{}:fallback:no-path", self.model_config.repo)
                }
            }
        };
        // Race: a parallel caller could have populated the slot between our
        // initial check and the compute. The first writer wins; subsequent
        // readers (including this caller's clone return) see the same value.
        let mut guard = self
            .model_fingerprint
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(existing) => existing.clone(),
            None => {
                *guard = Some(computed.clone());
                computed
            }
        }
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
            *guard = Some(create_session(model_path, self.provider())?);
            tracing::info!("Embedder session initialized");
        }
        Ok(guard)
    }

    /// Get or initialize the tokenizer.
    ///
    /// RM-V1.25-15: Returns an `Arc<Tokenizer>` so callers can release the
    /// mutex immediately and let `clear_session` drop the inner tokenizer
    /// without racing against in-flight inference. `Tokenizer::encode` /
    /// `decode` take `&self`, so call sites using `arc.encode(...)` work
    /// via `Arc` deref.
    fn tokenizer(&self) -> Result<Arc<tokenizers::Tokenizer>, EmbedderError> {
        {
            let guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = guard.as_ref() {
                return Ok(Arc::clone(t));
            }
        }
        let (_, tokenizer_path) = self.model_paths()?;
        let loaded = Arc::new(
            tokenizers::Tokenizer::from_file(tokenizer_path)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?,
        );
        let mut guard = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        // Another thread may have initialized while we were loading; prefer
        // the first winner so Arc identity is stable.
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        *guard = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    /// SHL-V1.29-1: Resolve the pad token id once, caching on the embedder.
    ///
    /// Returns the id used to fill `input_ids` below `max_length` during
    /// batched inference. Priority:
    ///   1. `tokenizer.get_padding().map(|p| p.pad_id)` — the tokenizer's
    ///      declared pad id from `tokenizer.json` when a padding section
    ///      is present.
    ///   2. `model_config.pad_id` — preset-declared fallback.
    ///
    /// Every call after the first short-circuits on the cached `OnceLock`
    /// value so `embed_batch` pays tokenizer-mutex cost exactly once.
    fn pad_id(&self) -> Result<i64, EmbedderError> {
        if let Some(&cached) = self.pad_id.get() {
            return Ok(cached);
        }
        let tokenizer = self.tokenizer()?;
        let resolved: i64 = tokenizer
            .get_padding()
            .map(|p| p.pad_id as i64)
            .unwrap_or(self.model_config.pad_id);
        // Last-writer wins is acceptable — get_padding() is deterministic
        // for the tokenizer, and `model_config.pad_id` is immutable, so
        // every racer computes the same value.
        let _ = self.pad_id.set(resolved);
        Ok(resolved)
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
        // Same truncation-bypass as `split_into_windows`: count actual
        // tokens, not whatever the tokenizer's `truncation` cap returns.
        // bge-large-ft and v9-200k ship tokenizer.json with
        // truncation.max_length=512, which silently caps `token_count`
        // and breaks every downstream "is this chunk too long?" check.
        let tokenizer_arc = self.tokenizer()?;
        let mut tok = (*tokenizer_arc).clone();
        if tok.get_truncation().is_some() {
            tok.with_truncation(None)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        }
        let encoding = tok
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
        // Same truncation-bypass as `token_count` — count actual tokens
        // for accurate windowing decisions.
        let tokenizer_arc = self.tokenizer()?;
        let mut tok = (*tokenizer_arc).clone();
        if tok.get_truncation().is_some() {
            tok.with_truncation(None)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        }
        let encodings = tok
            .encode_batch(texts.to_vec(), false)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        Ok(encodings.iter().map(|e| e.get_ids().len()).collect())
    }

    /// Count tokens consumed by the document prefix (e.g. "passage: " for E5,
    /// "Represent this query for searching relevant code: " for nomic).
    ///
    /// Used by windowing to size each window so that `prefix + window + special
    /// tokens` fits within `max_seq_length`. Falls back to a conservative 16 if
    /// tokenizer load fails — long-prefix models will silently truncate but the
    /// process still makes progress.
    pub fn doc_prefix_token_count(&self) -> usize {
        let prefix = &self.model_config.doc_prefix;
        if prefix.is_empty() {
            return 0;
        }
        match self.tokenizer() {
            Ok(t) => match t.encode(prefix.as_str(), false) {
                Ok(enc) => enc.get_ids().len(),
                Err(_) => 16,
            },
            Err(_) => 16,
        }
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

        // Clone the tokenizer and disable truncation so we count the FULL
        // token sequence, not whatever the tokenizer's `truncation` field
        // caps it to. Some preset tokenizers (notably bge-large-ft and
        // v9-200k, which were exported via `optimum-cli` with default
        // truncation enabled) ship `tokenizer.json` with
        // `truncation: {max_length: 512}`. Without this clone, encode()
        // silently caps long content at 512 tokens, the windowing loop
        // sees `ids.len() <= max_tokens` and returns a single window —
        // dropping ~90% of long markdown sections from the embedding.
        // Inference paths (embed_query/embed_batch) intentionally keep the
        // truncation behavior because they need to clamp input to
        // max_seq_length anyway, so we only override here.
        let tokenizer_arc = self.tokenizer()?;
        let mut tokenizer_no_trunc = (*tokenizer_arc).clone();
        if tokenizer_no_trunc.get_truncation().is_some() {
            tokenizer_no_trunc
                .with_truncation(None)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        }
        let encoding = tokenizer_no_trunc
            .encode(text, false)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;

        let ids = encoding.get_ids();
        if ids.len() <= max_tokens {
            return Ok(vec![(text.to_string(), 0)]);
        }

        // Slice the original `text` by each window's character offsets rather
        // than decoding token IDs. Decoding a WordPiece tokenizer (BGE) is
        // lossy — it lowercases, drops original whitespace, and inserts a
        // space between every subword — so stored chunk content would be
        // unreadable ("pub fn save ( & self, path : & path )") and useless
        // for cross-encoder reranking, result display, and NL generation.
        // `encoding.get_offsets()` maps each token to (start_char, end_char)
        // in the original input, which lets us return exact source slices.
        let offsets = encoding.get_offsets();

        let mut windows = Vec::new();
        // Step size: tokens per window minus overlap.
        // The assertion above guarantees step > max_tokens/2, ensuring linear window count.
        let step = max_tokens - overlap;
        let mut start = 0;
        let mut window_idx = 0u32;

        while start < ids.len() {
            let end = (start + max_tokens).min(ids.len());
            let char_start = offsets[start].0;
            let char_end = offsets[end - 1].1;
            // Some tokens (added special tokens, BOS/EOS with add_special_tokens=false
            // unset, padding) have offsets (0, 0) which would collapse the slice.
            // Fall back to the previous known-good offset in that case.
            let window_text = if char_end <= char_start {
                text.to_string()
            } else {
                text[char_start..char_end].to_string()
            };

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
    /// Large inputs are processed in batches to cap GPU memory usage.
    /// Batch size scales with the model's dim & seq via
    /// [`ModelConfig::embed_batch_size`]; override with `CQS_EMBED_BATCH_SIZE`.
    pub fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbedderError> {
        let _span = tracing::info_span!("embed_documents", count = texts.len()).entered();
        let prefix = &self.model_config.doc_prefix;
        // CQ-V1.33.0-2: route through `ModelConfig::embed_batch_size` so the
        // inner loop scales with model dim/seq instead of hardcoding 64.
        // BGE-large stays at 64; nomic-coderank (768 dim × 2048 seq) drops
        // to 16 to avoid OOM on 8 GB GPUs.
        let max_batch: usize = self.model_config.embed_batch_size();
        let started = std::time::Instant::now();
        let result = if texts.len() <= max_batch {
            let prefixed: Vec<String> = texts.iter().map(|t| format!("{}{}", prefix, t)).collect();
            self.embed_batch(&prefixed)
        } else {
            let mut all = Vec::with_capacity(texts.len());
            for chunk in texts.chunks(max_batch) {
                let prefixed: Vec<String> =
                    chunk.iter().map(|t| format!("{}{}", prefix, t)).collect();
                all.extend(self.embed_batch(&prefixed)?);
            }
            Ok(all)
        };
        // P3.10: completion event with output dim/count/time. Entry span only
        // carries inputs; without this operators have no signal that the call
        // actually produced what was asked for.
        if let Ok(ref embeddings) = result {
            tracing::info!(
                total = embeddings.len(),
                dim = self.embedding_dim(),
                input_count = texts.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "embed_documents complete"
            );
        }
        result
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
    /// Configurable via `CQS_MAX_QUERY_BYTES` (default 32768).
    fn max_query_bytes() -> usize {
        // P2.4: route through shared `parse_env_usize` helper.
        crate::limits::parse_env_usize("CQS_MAX_QUERY_BYTES", 32 * 1024)
    }

    pub fn embed_query(&self, text: &str) -> Result<Embedding, EmbedderError> {
        let _span = tracing::info_span!("embed_query").entered();
        // P3-3 (audit v1.33.0): time end-to-end so cache-hit vs. miss
        // latency is queryable on the completion events. Without this
        // operators can't tell "model is suddenly slow" from "cache hit
        // rate cratered".
        let start = std::time::Instant::now();
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }
        // RT-RES-5: Truncate oversized input before tokenization to bound CPU work.
        let max_query_bytes = Self::max_query_bytes();
        let text = if text.len() > max_query_bytes {
            tracing::warn!(
                len = text.len(),
                max = max_query_bytes,
                "Query text truncated before embedding"
            );
            // Truncate at a char boundary
            let mut end = max_query_bytes;
            while !text.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            &text[..end]
        } else {
            text
        };

        // Check in-memory LRU first
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                tracing::trace!(query = text, "Query cache hit (memory)");
                // P3-3: emit cache-aware completion (no query text — leaks at
                // trace level otherwise) so operators tracking hit-rate can
                // see hits at debug without enabling trace journals.
                tracing::debug!(
                    dim = self.embedding_dim(),
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    cache = "memory_hit",
                    "embed_query complete"
                );
                return Ok(cached.clone());
            }
        }

        // Check disk cache (survives across CLI invocations)
        let model_fp = self.model_fingerprint();
        if let Some(disk) = self.disk_query_cache() {
            if let Some(cached) = disk.get(text, &model_fp) {
                tracing::trace!(query = text, "Query cache hit (disk)");
                // Populate in-memory LRU for fast subsequent hits
                let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
                cache.put(text.to_string(), cached.clone());
                // P3-3: disk-hit completion mirrors memory_hit shape.
                tracing::debug!(
                    dim = self.embedding_dim(),
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    cache = "disk_hit",
                    "embed_query complete"
                );
                return Ok(cached);
            }
        }

        tracing::trace!(query = text, "Query cache miss");

        // Compute embedding (outside lock - allows parallel queries)
        let prefixed = format!("{}{}", self.model_config.query_prefix, text);
        let results = self.embed_batch(&[prefixed])?;
        let base_embedding = results.into_iter().next().ok_or_else(|| {
            EmbedderError::InferenceFailed("embed_batch returned empty result".to_string())
        })?;

        let embedding = base_embedding;

        // Store in memory LRU + disk cache (write-through)
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            cache.put(text.to_string(), embedding.clone());
        }
        if let Some(disk) = self.disk_query_cache() {
            disk.put(text, &model_fp, &embedding);
        }

        // P3.10: completion event so embed_query has parity with the
        // embed_documents log line. Debug-level — embed_query runs once per
        // search and the entry span already covers timing.
        // P3-3 (audit v1.33.0): add elapsed_ms + cache-tier so the journal
        // distinguishes "model slow on miss" from "everything was a hit".
        tracing::debug!(
            dim = self.embedding_dim(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            cache = "miss",
            "embed_query complete"
        );
        Ok(embedding)
    }

    // P2.75: previously `pub fn provider(&self) -> ExecutionProvider`
    // returned the eagerly-resolved field. Now superseded by the lazy
    // accessor defined above (`pub(crate) fn provider`). External callers
    // expecting the public symbol fall through to the lazy accessor's
    // `pub(crate)` visibility — switch to that name.

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
        // Also clear query cache -- stale embeddings from old session would be wrong
        // if model config changes before session is re-created.
        let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.clear();
        // RM-V1.25-15: Drop the tokenizer too (~10MB on BGE-large, ~20MB on
        // larger BPE vocabularies). The Arc holds a strong ref so in-flight
        // inference that grabbed an Arc clone before this call continues
        // with its own copy; the inner `Option` slot is cleared and will
        // lazy-reload on the next `tokenizer()` access.
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        // P2.77: surface the doubled-memory window when in-flight inference
        // is mid-encode. `Arc::strong_count > 1` means a worker thread
        // holds a clone of the old tokenizer; the inner Option clears here,
        // but the cloned Arc keeps the old tokenizer alive until that
        // thread releases it. Peak memory transiently exceeds the
        // documented ~500 MB by the tokenizer size (~10–20 MB on BGE-large).
        // Operators correlating memory spikes need this signal — option (a)
        // (RwLock around tokenizer + clear takes write lock) is higher-risk
        // because it extends the inference critical section.
        if let Some(t) = tok.as_ref() {
            let strong = std::sync::Arc::strong_count(t);
            if strong > 1 {
                tracing::info!(
                    strong_count = strong,
                    stage = "clear_during_inference",
                    "tokenizer Arc still referenced by in-flight inference; \
                     transient doubled-memory window during reload"
                );
            }
        }
        *tok = None;
        // DS-V1.33-7: also reset detected_dim and model_fingerprint so a
        // model swap re-detects dim and re-fingerprints on next inference.
        // Without this reset, the OnceLock-replaced Mutex<Option<...>>
        // slots would carry the first-loaded model's values forever,
        // silently feeding the wrong dim to `EmbeddingCache::read_batch`'s
        // dimension filter and the wrong fingerprint to the disk cache key.
        {
            let mut dim_guard = self.detected_dim.lock().unwrap_or_else(|p| p.into_inner());
            *dim_guard = None;
        }
        {
            let mut fp_guard = self
                .model_fingerprint
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *fp_guard = None;
        }
        tracing::info!(
            "Embedder session, query cache, tokenizer, detected_dim, and model_fingerprint cleared"
        );
    }

    /// Warm up the model with a dummy inference
    pub fn warm(&self) -> Result<(), EmbedderError> {
        // P3-7 (audit v1.33.0): operators investigating "daemon takes 4s
        // to come up" need a "warm started/completed" anchor. The dummy
        // embed_query triggers ~250 MB+ ORT session + tokenizer load (1-3s
        // on first GPU inference) and previously emitted nothing of its own.
        let _span = tracing::info_span!("embedder_warm", model = %self.model_config.name).entered();
        let start = std::time::Instant::now();
        let _ = self.embed_query("warmup")?;
        tracing::info!(
            elapsed_ms = start.elapsed().as_millis() as u64,
            model = %self.model_config.name,
            "embedder warmed"
        );
        Ok(())
    }

    /// Returns the embedding dimension detected from the model.
    /// Falls back to the model config's declared dimension if no inference has been run yet.
    pub fn embedding_dim(&self) -> usize {
        // DS-V1.33-7: read through the Mutex<Option<usize>> slot. Falls
        // back to the model config's declared dim when no inference has
        // populated the slot yet (or after `clear_session` reset it).
        let detected = self
            .detected_dim
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .copied();
        let dim = detected.unwrap_or(self.model_config.dim);
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
        use ort::session::SessionInputValue;
        use ort::value::Tensor;
        use std::borrow::Cow;

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

        // Pad to max length in batch. PERF-V1.33-3 / #1377: compute max_len
        // directly from encodings (no allocation), then build Array2 from
        // encodings in one pass instead of through `Vec<Vec<i64>>`.
        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);

        // SHL-V1.29-1: Read the pad id from the tokenizer (cached on first
        // call). `input_ids` uses the model-declared pad token; the attention
        // mask always pads with `0` regardless — a `0` mask entry zeroes the
        // padded position at attention time, which is the whole point of the
        // mask.
        let input_pad_id = self.pad_id()?;
        let input_ids_arr =
            pad_2d_i64_from_encodings(&encodings, |e| e.get_ids(), max_len, input_pad_id);
        let attention_mask_arr =
            pad_2d_i64_from_encodings(&encodings, |e| e.get_attention_mask(), max_len, 0);

        // Create tensors. PERF-V1.33-3 / #1377: clone the mask Array2 for
        // the tensor so the post-inference pooling can still read the
        // original. Net: 1 i64-Array2 clone (memcpy) replaces the prior
        // `Vec<Vec<i64>>` + per-element u32→i64 conversion in mean_pool.
        let input_ids_tensor = Tensor::from_array(input_ids_arr).map_err(ort_err)?;
        let attention_mask_tensor =
            Tensor::from_array(attention_mask_arr.clone()).map_err(ort_err)?;

        // Build the named input map. Tensor names come from `ModelConfig::input_names`
        // so non-BERT models (different naming) and distilled variants (no
        // token_type_ids) are supported without touching encoder code.
        let names = &self.model_config.input_names;
        let mut inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = Vec::with_capacity(3);
        inputs.push((
            Cow::Borrowed(names.ids.as_str()),
            SessionInputValue::from(input_ids_tensor),
        ));
        inputs.push((
            Cow::Borrowed(names.mask.as_str()),
            SessionInputValue::from(attention_mask_tensor),
        ));
        if let Some(ref tt_name) = names.token_types {
            // token_type_ids: all zeros, same shape as input_ids.
            // Only added when the model wants it.
            let token_type_ids_arr = Array2::<i64>::zeros((texts.len(), max_len));
            let token_type_ids_tensor = Tensor::from_array(token_type_ids_arr).map_err(ort_err)?;
            inputs.push((
                Cow::Borrowed(tt_name.as_str()),
                SessionInputValue::from(token_type_ids_tensor),
            ));
        }
        if let Some(ref pos_name) = names.position_ids {
            // #1442: third-party Qwen3-Embedding-4B ONNX exports require
            // an explicit `position_ids` input. We use right-padding
            // (BERT-style — the same shape `pad_2d_i64_from_encodings`
            // emits via the tokenizer's default), so positions are simply
            // `[0, 1, ..., max_len-1]` for every row. Padding tokens get
            // positions too; they're masked out by `attention_mask` at
            // attention time, same as for `input_ids`.
            let mut pos_data: Vec<i64> = Vec::with_capacity(texts.len() * max_len);
            for _ in 0..texts.len() {
                pos_data.extend((0..max_len as i64).collect::<Vec<i64>>());
            }
            let position_ids_arr = Array2::<i64>::from_shape_vec((texts.len(), max_len), pos_data)
                .map_err(|e| {
                    EmbedderError::InferenceFailed(format!("position_ids shape failed: {e}"))
                })?;
            let position_ids_tensor = Tensor::from_array(position_ids_arr).map_err(ort_err)?;
            inputs.push((
                Cow::Borrowed(pos_name.as_str()),
                SessionInputValue::from(position_ids_tensor),
            ));
        }

        // Run inference (lazy init session)
        let mut guard = self.session()?;
        let session = guard
            .as_mut()
            .expect("session() guarantees initialized after Ok return");
        let _inference = tracing::debug_span!("inference", max_len).entered();
        let outputs = session.run(inputs).map_err(ort_err)?;

        // Get the configured output tensor: shape [batch, seq_len, dim]
        let output_name = self.model_config.output_name.as_str();
        let output = outputs.get(output_name).ok_or_else(|| {
            EmbedderError::InferenceFailed(format!(
                "ONNX model has no '{}' output. Available: {:?}",
                output_name,
                outputs.keys().collect::<Vec<_>>()
            ))
        })?;
        // #1442 follow-up: dispatch on output dtype. Most ONNX exports
        // emit `Tensor<f32>`, but FP16 / bfloat16 quantized exports
        // (e.g. zhiqing/Qwen3-Embedding-4B-ONNX) emit `Tensor<f16>` or
        // `Tensor<bf16>` and the f32 extract fails fast with
        // "Cannot extract Tensor<f32> from Tensor<f16>" — strict
        // dtype check, not a silent reinterpret. Try f32 first
        // (zero-copy fast path), then half-precision variants on
        // mismatch and convert each element to f32 in software.
        // The conversion cost (one map per inference) is negligible
        // next to the model forward pass.
        //
        // `shape` and `data` after this block are owned `Vec<i64>` /
        // `Vec<f32>`; the f32 fast path pays one extra `.to_vec()`
        // (~few MB/batch) for shape uniformity vs branching the rest
        // of the function body.
        let (shape_vec, data_vec): (Vec<i64>, Vec<f32>) = match output.try_extract_tensor::<f32>() {
            Ok((s, d)) => (s.to_vec(), d.to_vec()),
            Err(_) => match output.try_extract_tensor::<half::f16>() {
                Ok((s, d)) => (s.to_vec(), d.iter().map(|h| h.to_f32()).collect()),
                Err(_) => {
                    let (s, d) = output.try_extract_tensor::<half::bf16>().map_err(ort_err)?;
                    (s.to_vec(), d.iter().map(|h| h.to_f32()).collect())
                }
            },
        };
        let shape: &[i64] = &shape_vec;
        let data: &[f32] = &data_vec;

        let batch_size = texts.len();
        let seq_len = max_len;

        // PoolingStrategy::Identity: the ONNX output is already pooled to
        // `[batch, dim]`. Skip the 3D reshape + pool dispatch and emit
        // L2-normalized rows directly. Used by EmbeddingGemma's
        // `sentence_embedding` output (#1220 follow-up).
        if self.model_config.pooling == PoolingStrategy::Identity {
            if shape.len() != 2 {
                return Err(EmbedderError::InferenceFailed(format!(
                    "PoolingStrategy::Identity expects 2D [batch, dim] output; got {} dimensions",
                    shape.len()
                )));
            }
            if shape[0] as usize != batch_size {
                return Err(EmbedderError::InferenceFailed(format!(
                    "Tensor batch size mismatch: expected {}, got {}",
                    batch_size, shape[0]
                )));
            }
            let embedding_dim = shape[1] as usize;
            {
                // DS-V1.33-7: lock-and-set the Mutex<Option<usize>> slot.
                let mut guard = self.detected_dim.lock().unwrap_or_else(|p| p.into_inner());
                match *guard {
                    Some(expected) if expected != embedding_dim => {
                        return Err(EmbedderError::InferenceFailed(format!(
                            "Embedding dimension changed: expected {expected}, got {embedding_dim}"
                        )));
                    }
                    None => {
                        *guard = Some(embedding_dim);
                        tracing::info!(
                            dim = embedding_dim,
                            "Detected embedding dimension from model (Identity pooling)"
                        );
                    }
                    _ => {}
                }
            }
            let results: Vec<Embedding> = (0..batch_size)
                .map(|b| {
                    let start = b * embedding_dim;
                    let v = data[start..start + embedding_dim].to_vec();
                    Embedding::new(normalize_l2(v))
                })
                .collect();
            return Ok(results);
        }

        // Validate tensor shape: expect [batch_size, seq_len, dim]
        if shape.len() != 3 {
            return Err(EmbedderError::InferenceFailed(format!(
                "Unexpected tensor shape: expected 3 dimensions [batch, seq, dim], got {} dimensions",
                shape.len()
            )));
        }
        let embedding_dim = shape[2] as usize;
        // Set or validate embedding dimension from model output
        // DS-V1.33-7: lock-and-set the Mutex<Option<usize>> slot.
        {
            let mut guard = self.detected_dim.lock().unwrap_or_else(|p| p.into_inner());
            match *guard {
                Some(expected) if expected != embedding_dim => {
                    return Err(EmbedderError::InferenceFailed(format!(
                        "Embedding dimension changed: expected {expected}, got {embedding_dim}"
                    )));
                }
                None => {
                    *guard = Some(embedding_dim);
                    tracing::info!(
                        dim = embedding_dim,
                        "Detected embedding dimension from model"
                    );
                }
                _ => {} // matches expected — OK
            }
        }
        if shape[0] as usize != batch_size {
            return Err(EmbedderError::InferenceFailed(format!(
                "Tensor batch size mismatch: expected {}, got {}",
                batch_size, shape[0]
            )));
        }
        // Reshape flat output into [batch, seq, dim] for pooling dispatch.
        let hidden = Array3::from_shape_vec((batch_size, seq_len, embedding_dim), data.to_vec())
            .map_err(|e| EmbedderError::InferenceFailed(format!("tensor reshape failed: {e}")))?;

        // Dispatch on the configured pooling strategy. Each pooler returns
        // an unnormalized per-batch vector; L2 normalization is applied
        // uniformly after to keep the contract (unit-length embeddings)
        // invariant across strategies.
        let pooled_batch: Vec<Vec<f32>> = match self.model_config.pooling {
            PoolingStrategy::Mean => mean_pool(&hidden, &attention_mask_arr, embedding_dim),
            PoolingStrategy::Cls => cls_pool(&hidden),
            PoolingStrategy::LastToken => last_token_pool(&hidden, &attention_mask_arr),
            // Already handled by the early-return above; this arm is only
            // reachable if the model output had 3 dims AND pooling = Identity,
            // which is a config error (Identity expects 2D).
            PoolingStrategy::Identity => unreachable!(
                "PoolingStrategy::Identity should be handled before the 3D pool dispatch"
            ),
        };

        let results = pooled_batch
            .into_iter()
            .map(|v| Embedding::new(normalize_l2(v)))
            .collect();

        Ok(results)
    }
}

/// Download model and tokenizer from HuggingFace Hub
fn ensure_model(config: &ModelConfig) -> Result<(PathBuf, PathBuf), EmbedderError> {
    // CQS_ONNX_DIR: bypass HF download, load from local directory.
    // Directory must contain model.onnx and tokenizer.json.
    if let Ok(dir) = std::env::var("CQS_ONNX_DIR") {
        let dir = dunce::canonicalize(PathBuf::from(&dir)).unwrap_or_else(|_| PathBuf::from(dir));
        let model_path = dir.join(&config.onnx_path);
        let tokenizer_path = dir.join(&config.tokenizer_path);
        // SEC-3: Verify joined paths stay inside CQS_ONNX_DIR (symlink/traversal defense)
        for (label, path) in [("model", &model_path), ("tokenizer", &tokenizer_path)] {
            if let Ok(canonical) = dunce::canonicalize(path) {
                if !canonical.starts_with(&dir) {
                    return Err(EmbedderError::ModelNotFound(format!(
                        "SEC-3: {} path escapes CQS_ONNX_DIR: {} resolves to {}",
                        label,
                        path.display(),
                        canonical.display()
                    )));
                }
            }
        }
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

    use hf_hub::api::sync::ApiBuilder;

    // hf-hub defaults to max_retries=0 — a single transient ureq error
    // (TLS handshake glitch, HTTP/2 reset, throttling) aborts the download
    // with no retry. The Python `huggingface-cli` retries internally and
    // succeeds, so the same file from the same server can fail in cqs and
    // succeed in Python. Bump retries to 5 to match human expectations
    // and to make >2 GB external-data downloads (Gemma, Qwen3) reliable.
    // Discovered 2026-05-03 during the Qwen3-Embedding-8B ceiling-probe
    // attempt: a `model.onnx_data` fetch silently failed with no retry,
    // ORT then panicked at session init with "cannot get file size".
    let api = ApiBuilder::from_env()
        .with_retries(5)
        .build()
        .map_err(|e| EmbedderError::HfHub(e.to_string()))?;
    let repo = api.model(config.repo.clone());

    let model_path = repo
        .get(&config.onnx_path)
        .map_err(|e| EmbedderError::HfHub(e.to_string()))?;
    let tokenizer_path = repo
        .get(&config.tokenizer_path)
        .map_err(|e| EmbedderError::HfHub(e.to_string()))?;

    // Fetch the ONNX external-data sidecar for models that exceed the 2GB
    // protobuf limit. The Rust ONNX Runtime expects the .onnx_data file to
    // sit next to model.onnx; without it, session init fails with
    // "filesystem error: cannot get file size" when the graph references
    // external tensors. Most presets (BGE, E5, etc.) ship a single
    // self-contained model.onnx and do not have this file — the sidecar
    // attempt returns a 404 wrapped in `ApiError::RequestError`. That's
    // expected for self-contained models and gets silenced at debug level.
    //
    // Anything other than a clean 404 (network error, IoError,
    // LockAcquisition, etc.) is unexpected: either the operator is on
    // an external-data preset and the file genuinely couldn't be fetched
    // (broken setup), or there's a transient that even retries didn't
    // recover from. Either way the operator needs to see it — log at
    // warn so it surfaces at the default RUST_LOG level. The pipeline
    // continues; if ORT later panics with "cannot get file size", the
    // warn line is the breadcrumb.
    let external_data_path = format!("{}_data", config.onnx_path);
    match repo.get(&external_data_path) {
        Ok(_) => {} // Sidecar fetched (or already cached) — common path for Gemma / Qwen3.
        Err(e) if is_likely_not_found(&e) => {
            tracing::debug!(
                file = %external_data_path,
                error = %e,
                "ONNX external-data sidecar not present in repo (expected for self-contained models)"
            );
        }
        Err(e) => {
            tracing::warn!(
                file = %external_data_path,
                error = %e,
                "ONNX external-data sidecar fetch failed unexpectedly. \
                 If the model exceeds 2GB, ORT will panic with 'cannot get file size' \
                 at session init. Check network, HF auth, or run \
                 `hf download <repo> --include='{external_data_path}'` manually."
            );
        }
    }

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

/// Heuristic — was an `hf_hub::api::sync::ApiError` likely a 404 / "file not
/// in repo" rather than a real failure?
///
/// `hf_hub` wraps the HTTP layer behind ureq and exposes most errors as
/// `RequestError(Box<ureq::Error>)`. Distinguishing 404 cleanly would
/// require pattern-matching the inner ureq error, which is version-specific
/// and exposes the dependency surface. Instead we string-match the
/// rendered error: `ureq::Error::Status(404, _)` formats with "404" or
/// "status code 404". Good-enough for the warn-vs-debug split — if a 404
/// ever stops formatting that way, we just get a warn for a self-contained
/// preset, which is annoying but not wrong. False-positive direction
/// (treating a real failure as 404) is the more dangerous one but
/// requires the error message to literally contain "404", which a network
/// failure normally won't.
fn is_likely_not_found(err: &hf_hub::api::sync::ApiError) -> bool {
    let s = err.to_string();
    s.contains("404") || s.contains("Not Found") || s.contains("not found")
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

/// Pad 2D sequences to a fixed length.
///
/// PERF-V1.33-3 / #1377: production embed/rerank now uses
/// [`pad_2d_i64_from_encodings`] which fills the `Array2` directly from
/// tokenizer encodings. This helper stays for tests + future callers
/// that have a `&[Vec<i64>]` already in hand.
#[allow(dead_code)]
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

/// PERF-V1.33-3 / #1377: build the padded `Array2<i64>` directly from
/// tokenizer encodings, skipping the per-batch `Vec<Vec<i64>>` intermediate
/// that [`pad_2d_i64`] previously consumed.
///
/// `extract` selects which encoding field to pull (`get_ids`,
/// `get_attention_mask`, `get_type_ids`); the function is generic over `&[u32]`
/// vs `&[u32]` so the same helper covers all three fields. The cast from
/// `u32` → `i64` happens in the inner loop alongside the array write —
/// no intermediate `Vec<i64>` is allocated.
///
/// At `CQS_EMBED_BATCH_SIZE=64` and `seq_len=512`, the prior
/// `Vec<Vec<i64>>` shape allocated ~98K i64 conversions + 192 inner Vecs
/// per batch (3 fields × 64 batch × ~512 seq + 3 outer Vecs). This helper
/// drops to zero auxiliary heap allocations beyond the final `Array2`.
pub(crate) fn pad_2d_i64_from_encodings<F>(
    encodings: &[tokenizers::Encoding],
    extract: F,
    max_len: usize,
    pad_value: i64,
) -> Array2<i64>
where
    F: Fn(&tokenizers::Encoding) -> &[u32],
{
    let batch_size = encodings.len();
    let mut arr = Array2::from_elem((batch_size, max_len), pad_value);
    for (i, enc) in encodings.iter().enumerate() {
        for (j, &val) in extract(enc).iter().take(max_len).enumerate() {
            arr[[i, j]] = val as i64;
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

// ---------------------------------------------------------------------------
// Pooling strategies
// ---------------------------------------------------------------------------
//
// Each pooler takes the `[batch, seq, dim]` hidden-state tensor and returns
// one `Vec<f32>` per batch item (unnormalized). The caller normalizes.
//
// Mean pooling is the BGE / E5 / v9-200k path. CLS and LastToken are present
// for future non-BERT models (tested with synthetic fixtures today; wiring
// for a real model is handled via `ModelConfig::pooling`).

/// Mean-pool the masked token positions.
///
/// Builds the attention mask as a `[batch, seq, 1]` broadcast tensor, multiplies
/// in-place against hidden states, sums along the sequence axis, and divides
/// by the mask sum. Matches BGE reference / sentence-transformers mean pooling.
///
/// Batches whose attention mask is all zero return a zero vector and log a
/// warning — this preserves pre-refactor behavior.
fn mean_pool(
    hidden: &Array3<f32>,
    attention_mask: &Array2<i64>,
    embedding_dim: usize,
) -> Vec<Vec<f32>> {
    // PERF-V1.33-3 / #1377: takes the already-built `Array2<i64>` directly
    // (was `&[Vec<i64>]`) so the embed pipeline doesn't have to keep a
    // parallel `Vec<Vec<i64>>` of the mask alongside the tensor.
    let (batch_size, seq_len, _) = hidden.dim();
    let mask_2d = Array2::from_shape_fn((batch_size, seq_len), |(i, j)| {
        attention_mask.get([i, j]).copied().unwrap_or(0) as f32
    });
    let mask_3d = mask_2d.clone().insert_axis(Axis(2));

    let masked = hidden * &mask_3d;
    let summed = masked.sum_axis(Axis(1)); // [batch, dim]
    let counts = mask_2d.sum_axis(Axis(1)).insert_axis(Axis(1)); // [batch, 1]

    (0..batch_size)
        .map(|i| {
            let count = counts[[i, 0]];
            let row = summed.row(i);
            if count > 0.0 {
                row.iter().map(|v| v / count).collect()
            } else {
                tracing::warn!(batch_idx = i, "Zero attention mask — producing zero vector");
                vec![0.0f32; embedding_dim]
            }
        })
        .collect()
}

/// CLS-pool: return the hidden state of the first token for each batch item.
///
/// Used by some DistilBERT-derived embedders trained specifically for CLS
/// pooling. On those models, using mean pooling degrades quality silently
/// (no error; just worse retrieval) — hence the configurable dispatch.
fn cls_pool(hidden: &Array3<f32>) -> Vec<Vec<f32>> {
    let (batch_size, _, _) = hidden.dim();
    (0..batch_size)
        .map(|i| hidden.slice(ndarray::s![i, 0usize, ..]).to_vec())
        .collect()
}

/// Last-token pool: return the hidden state of the last non-padding token,
/// located via the attention mask (rightmost `1`).
///
/// Used by autoregressive / decoder-only embedders (Qwen3-Embedding,
/// some Mistral-based embedders) where the final token's hidden state is the
/// trained embedding location.
///
/// If the mask is all zero (pathological) the function falls back to the
/// first token and logs a warning. If a batch item's mask has no `1`s we
/// use index 0.
fn last_token_pool(hidden: &Array3<f32>, attention_mask: &Array2<i64>) -> Vec<Vec<f32>> {
    // PERF-V1.33-3 / #1377: takes the `Array2<i64>` directly — see
    // `mean_pool` above for the rationale.
    let (batch_size, seq_len, _) = hidden.dim();
    let (mask_batch, mask_seq) = attention_mask.dim();
    (0..batch_size)
        .map(|i| {
            // Find the last position where the mask is set. `i` may be
            // beyond the mask Array2's first dim only if a caller passes a
            // mismatched shape — fall back to index 0 and warn, mirroring
            // the prior `attention_mask.get(i)` defensive read.
            let last_idx = if i < mask_batch {
                let bound = seq_len.min(mask_seq);
                let mut found = None;
                for j in (0..bound).rev() {
                    if attention_mask[[i, j]] != 0 {
                        found = Some(j);
                        break;
                    }
                }
                found.unwrap_or_else(|| {
                    tracing::warn!(
                        batch_idx = i,
                        "last_token_pool: zero attention mask — using index 0"
                    );
                    0
                })
            } else {
                tracing::warn!(
                    batch_idx = i,
                    "last_token_pool: mask shorter than batch — using index 0"
                );
                0
            };
            hidden.slice(ndarray::s![i, last_idx, ..]).to_vec()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== FP16 / BF16 conversion smoke tests =====
    //
    // #1442 follow-up: the embed loop dispatches output extraction on
    // dtype (`try_extract_tensor::<f32>` → fall back to `f16` then
    // `bf16`). The actual ORT extraction needs a live session, but
    // the half-crate conversion arithmetic that we depend on is
    // worth pinning at the unit level so a future `half` crate bump
    // (or a precision regression in the representation) trips a fast
    // local test instead of a 5-7 hour reindex.
    #[test]
    fn f16_round_trip_preserves_chunk_embedding_range() {
        // Embedding values are normalized to [-1, 1] (cosine-comparable
        // unit vectors); pin a few representative points across that
        // range. f16 has ~3 decimal digits of precision; tolerance 1e-3.
        for v in [-1.0_f32, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0] {
            let h = half::f16::from_f32(v);
            let back = h.to_f32();
            assert!(
                (back - v).abs() < 1e-3,
                "f16 round-trip lost precision: {v} → {h:?} → {back}"
            );
        }
    }

    #[test]
    fn bf16_round_trip_preserves_chunk_embedding_range() {
        // bf16 has ~2-3 decimal digits — coarser mantissa than f16 but
        // wider exponent range. Same value sweep, looser tolerance.
        for v in [-1.0_f32, -0.5, -0.1, 0.0, 0.1, 0.5, 1.0] {
            let h = half::bf16::from_f32(v);
            let back = h.to_f32();
            assert!(
                (back - v).abs() < 1e-2,
                "bf16 round-trip lost precision: {v} → {h:?} → {back}"
            );
        }
    }

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

    // TC-ADV-1.29-1: normalize_l2 has no numeric validation. If the input
    // contains NaN, norm_sq becomes NaN, `norm_sq > 0.0` is false, and the
    // NaN passes through unchanged. If the input contains Inf, norm_sq is
    // Inf, `inv_norm = 1/Inf = 0`, and every Inf × 0.0 = NaN.
    //
    // Pin the current behaviour so a future finite-check refactor is
    // deliberate.

    #[test]
    fn test_normalize_l2_passes_nan_through() {
        let v = normalize_l2(vec![1.0, f32::NAN, 3.0]);
        assert_eq!(v.len(), 3);
        // norm_sq = 1 + NaN + 9 = NaN, `NaN > 0.0` = false, fall through
        // branch leaves v untouched.
        assert_eq!(
            v[0], 1.0,
            "AUDIT-FOLLOWUP (TC-ADV-1.29-1): NaN in input short-circuits \
             normalization — values passed through verbatim"
        );
        assert!(v[1].is_nan());
        assert_eq!(v[2], 3.0);
    }

    #[test]
    fn test_normalize_l2_pure_nan_input() {
        let v = normalize_l2(vec![f32::NAN; 4]);
        assert_eq!(v.len(), 4);
        assert!(
            v.iter().all(|x| x.is_nan()),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-1): all-NaN input stays all-NaN — \
             no error, no sanitization"
        );
    }

    #[test]
    fn test_normalize_l2_inf_input_collapses_to_nan() {
        // norm_sq = Inf + Inf + Inf = Inf, inv_norm = 1/Inf = 0, every
        // multiply-by-zero on an Inf gives NaN (not 0). Pin that behaviour.
        let v = normalize_l2(vec![f32::INFINITY, f32::INFINITY, f32::INFINITY]);
        assert_eq!(v.len(), 3);
        assert!(
            v.iter().all(|x| x.is_nan()),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-1): Inf input × (1/Inf=0) = NaN — \
             the output is corrupted silently, got {:?}",
            v
        );
    }

    #[test]
    fn test_normalize_l2_neg_inf_input_collapses_to_nan() {
        let v = normalize_l2(vec![f32::NEG_INFINITY, 0.0, 0.0]);
        // norm_sq = Inf (squaring NEG_INFINITY), same short-circuit as above.
        assert!(
            v[0].is_nan(),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-1): -Inf in input yields NaN after \
             normalization — got {}",
            v[0]
        );
    }

    // TC-ADV-1.29-2: embed_batch does not validate ORT output before
    // Embedding::new. The load-bearing contract test we can land without
    // a real ORT session is that Embedding::new accepts non-finite values
    // (NaN, Inf). Since embed_batch eventually passes pooled rows through
    // Embedding::new, a NaN-poisoned ORT output would become a NaN-poisoned
    // Embedding and propagate into search scoring. Embedding::try_new DOES
    // reject non-finite — but `embed_batch` calls `Embedding::new` (the
    // infallible path) instead. This test pins that mismatch.

    #[test]
    fn test_embedding_new_accepts_nan_unlike_try_new() {
        // Embedding::new is the path embed_batch uses — no validation.
        let v = vec![f32::NAN; EMBEDDING_DIM];
        let emb = Embedding::new(v);
        assert_eq!(emb.len(), EMBEDDING_DIM);
        // The resulting Embedding carries NaN — anything that downstream
        // consumer uses for scoring will be corrupted.
        assert!(
            emb.as_slice().iter().all(|x| x.is_nan()),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-2): Embedding::new accepts NaN \
             (unlike try_new) — embed_batch uses this path, so a NaN-poisoned \
             ORT output silently propagates"
        );
        // Contrast with try_new (already tested at `tc33_try_new_nan_errors`)
        // which would reject the same input.
        let rejected = Embedding::try_new(vec![f32::NAN; EMBEDDING_DIM]);
        assert!(
            rejected.is_err(),
            "try_new rejects NaN — embed_batch should switch to this path \
             to catch poisoned ORT outputs"
        );
    }

    #[test]
    fn test_embedding_new_accepts_inf_unlike_try_new() {
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        v[0] = f32::INFINITY;
        v[1] = f32::NEG_INFINITY;
        let emb = Embedding::new(v);
        assert!(emb.as_slice()[0].is_infinite());
        assert!(emb.as_slice()[1].is_infinite());

        // try_new would reject this Inf-laden vector.
        let mut v2 = vec![0.0f32; EMBEDDING_DIM];
        v2[0] = f32::INFINITY;
        assert!(
            Embedding::try_new(v2).is_err(),
            "try_new rejects +Inf — embed_batch should use it"
        );
    }

    // ===== Pooling strategy tests =====
    //
    // These exercise mean_pool / cls_pool / last_token_pool with synthetic
    // [batch, seq, dim] tensors. No model file is needed — we're testing
    // the reducer, not the whole encode path.

    fn make_hidden(values: Vec<Vec<Vec<f32>>>) -> Array3<f32> {
        let batch = values.len();
        let seq = values[0].len();
        let dim = values[0][0].len();
        let flat: Vec<f32> = values.into_iter().flatten().flatten().collect();
        Array3::from_shape_vec((batch, seq, dim), flat).expect("synthetic shape mismatch")
    }

    /// PERF-V1.33-3 / #1377: pooling now consumes `&Array2<i64>` instead of
    /// `&[Vec<i64>]`. Tests build the mask as a flat `Vec<Vec<i64>>` for
    /// readability and convert here.
    fn mask_2d(rows: Vec<Vec<i64>>) -> Array2<i64> {
        let batch = rows.len();
        let seq = rows.first().map(|r| r.len()).unwrap_or(0);
        let flat: Vec<i64> = rows.into_iter().flatten().collect();
        Array2::from_shape_vec((batch, seq), flat).expect("test mask shape mismatch")
    }

    #[test]
    fn mean_pool_respects_mask() {
        // 1 batch, 3 tokens, 2-dim hidden state. Mask: [1, 1, 0] — last
        // position is padding, so it must be excluded.
        let hidden = make_hidden(vec![vec![
            vec![1.0, 2.0],
            vec![3.0, 4.0],
            vec![100.0, 200.0], // should be ignored
        ]]);
        let mask = mask_2d(vec![vec![1i64, 1, 0]]);
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled.len(), 1, "one batch item");
        // Mean of [1,2] and [3,4] = [2,3]
        assert!((pooled[0][0] - 2.0).abs() < 1e-6);
        assert!((pooled[0][1] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_zero_mask_returns_zero_vector() {
        let hidden = make_hidden(vec![vec![vec![5.0, 5.0], vec![6.0, 6.0]]]);
        let mask = mask_2d(vec![vec![0i64, 0]]);
        let pooled = mean_pool(&hidden, &mask, 2);
        assert_eq!(pooled[0], vec![0.0, 0.0]);
    }

    #[test]
    fn cls_pool_returns_first_token() {
        // CLS pooling must return the [0]-th token regardless of mask.
        let hidden = make_hidden(vec![
            vec![vec![1.0, 2.0], vec![9.9, 9.9]],
            vec![vec![3.0, 4.0], vec![7.7, 7.7]],
        ]);
        let pooled = cls_pool(&hidden);
        assert_eq!(pooled.len(), 2);
        assert_eq!(pooled[0], vec![1.0, 2.0]);
        assert_eq!(pooled[1], vec![3.0, 4.0]);
    }

    #[test]
    fn last_token_pool_picks_last_unmasked() {
        // Mask: [1, 1, 1, 0] — last real token is index 2.
        // Mask: [1, 0, 0, 0] — last real token is index 0.
        let hidden = make_hidden(vec![
            vec![
                vec![0.0, 0.0],
                vec![0.0, 0.0],
                vec![42.0, 43.0], // <- expected
                vec![9.0, 9.0],
            ],
            vec![
                vec![11.0, 12.0], // <- expected
                vec![0.0, 0.0],
                vec![0.0, 0.0],
                vec![0.0, 0.0],
            ],
        ]);
        let mask = mask_2d(vec![vec![1i64, 1, 1, 0], vec![1i64, 0, 0, 0]]);
        let pooled = last_token_pool(&hidden, &mask);
        assert_eq!(pooled[0], vec![42.0, 43.0]);
        assert_eq!(pooled[1], vec![11.0, 12.0]);
    }

    #[test]
    fn last_token_pool_zero_mask_falls_back_to_index_0() {
        let hidden = make_hidden(vec![vec![vec![7.0, 8.0], vec![9.0, 10.0]]]);
        let mask = mask_2d(vec![vec![0i64, 0]]);
        let pooled = last_token_pool(&hidden, &mask);
        assert_eq!(pooled[0], vec![7.0, 8.0]);
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
        // EMBEDDING_DIM derives from the preset row marked `default = true`
        // in `define_embedder_presets!`. EmbeddingGemma-300m since v1.35.0.
        assert_eq!(EMBEDDING_DIM, 768);
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
        let embedder = match Embedder::new(ModelConfig::e5_base()) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                return;
            }
        };
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

    /// DS-V1.33-7: `clear_session` must reset `detected_dim` and
    /// `model_fingerprint` so a model swap re-detects on the next inference.
    /// Pre-fix, both fields were `OnceLock` and could not be cleared under
    /// `&self`, leaving the first-loaded model's values in place forever.
    /// This test directly mutates the Mutex<Option<...>> slots to simulate
    /// post-inference state, calls clear_session, and verifies the slots
    /// are now None — no model load required.
    #[test]
    fn clear_session_resets_detected_dim_and_model_fingerprint() {
        let embedder = Embedder::new_cpu(ModelConfig::e5_base()).unwrap();
        // Simulate post-inference state: detected_dim populated.
        {
            let mut g = embedder.detected_dim.lock().unwrap();
            *g = Some(1024);
        }
        // Simulate post-fingerprint state: model_fingerprint populated.
        {
            let mut g = embedder.model_fingerprint.lock().unwrap();
            *g = Some("stale-model-hash-from-old-load".to_string());
        }
        // Sanity: read-through via the public APIs returns the staged values.
        assert_eq!(
            embedder.embedding_dim(),
            1024,
            "embedding_dim must read the staged detected_dim"
        );
        assert_eq!(
            embedder.model_fingerprint(),
            "stale-model-hash-from-old-load",
            "model_fingerprint must read the staged value"
        );
        // Clear and verify both slots reset to None.
        embedder.clear_session();
        assert!(
            embedder.detected_dim.lock().unwrap().is_none(),
            "detected_dim must be None after clear_session"
        );
        assert!(
            embedder.model_fingerprint.lock().unwrap().is_none(),
            "model_fingerprint must be None after clear_session"
        );
        // Public API now falls back to model_config.dim (model never loaded).
        assert_eq!(
            embedder.embedding_dim(),
            ModelConfig::e5_base().dim,
            "embedding_dim falls back to config dim when detected_dim is None"
        );
    }

    // ===== Integration tests (require model) =====

    mod integration {
        use super::*;

        #[test]
        #[ignore] // Requires model - run with: cargo test --lib integration -- --ignored
        fn test_token_count_empty() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            let count = match embedder.token_count("") {
                Ok(c) => c,
                Err(err) => {
                    eprintln!(
                        "token_count failed (likely corrupt tokenizer): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
            assert_eq!(count, 0);
        }

        #[test]
        #[ignore]
        fn test_token_count_simple() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            let count = match embedder.token_count("hello world") {
                Ok(c) => c,
                Err(err) => {
                    eprintln!(
                        "token_count failed (likely corrupt tokenizer): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
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
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            let code = "fn main() { println!(\"Hello\"); }";
            let count = match embedder.token_count(code) {
                Ok(c) => c,
                Err(err) => {
                    eprintln!(
                        "token_count failed (likely corrupt tokenizer): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
            // Code typically tokenizes to more tokens than words
            assert!(count > 5, "Expected >5 tokens for code, got {}", count);
        }

        #[test]
        #[ignore]
        fn test_token_count_unicode() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            let text = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}\u{4e16}\u{754c}"; // "Hello world" in Japanese
            let count = match embedder.token_count(text) {
                Ok(c) => c,
                Err(err) => {
                    eprintln!(
                        "token_count failed (likely corrupt tokenizer): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
            // Unicode text may tokenize differently
            assert!(count > 0, "Expected >0 tokens for unicode, got {}", count);
        }

        /// Windowing must preserve raw source formatting — decoding token IDs
        /// back to text is lossy on WordPiece tokenizers (lowercases, inserts
        /// spaces between subwords), which would corrupt stored chunk content.
        /// Regression check for the 2026-04-20 windowing fix.
        #[test]
        #[ignore]
        fn split_into_windows_preserves_original_text() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            // Mix of casing, punctuation, multi-space indentation — WordPiece
            // decode would collapse `pub fn` to `pub fn`, strip mixed-case
            // identifiers like `CagraError`, and pad every punctuation char
            // with spaces. Raw slicing keeps all of it.
            let source = "pub fn save(&self, path: &Path) -> Result<(), CagraError> {\n"
                .to_string()
                + &"    let _span = tracing::info_span!(\"cagra_save\").entered();\n".repeat(200);
            // The Embedder::new soft-skip catches the case where the model
            // is fully missing. But on the GitHub-hosted runner we've also
            // seen `Embedder::new` succeed against a half-populated cache
            // (ONNX present, tokenizer.json got a HTML error page from a
            // prior failed download), and the actual tokenize-on-demand call
            // fails inside split_into_windows with `Tokenizer("expected
            // ident at line 1 column 3")`. Treat that as the same skip
            // condition. (#1305)
            let windows = match embedder.split_into_windows(&source, 128, 16) {
                Ok(w) => w,
                Err(err) => {
                    eprintln!(
                        "split_into_windows failed (likely corrupt tokenizer.json from a partial \
                         HF cache): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
            assert!(windows.len() > 1, "text must be long enough to window");

            // Each window should be a substring of the original text (modulo
            // whitespace boundaries where the tokenizer split mid-character-class).
            for (w, idx) in &windows {
                assert!(
                    source.contains(w.trim()),
                    "window {idx} is not a substring of the source — tokenizer decode leaked"
                );
                // WordPiece decode inserts ' ( ' with surrounding spaces. Raw
                // slicing keeps the exact `(` without spaces.
                if w.contains('(') {
                    assert!(
                        !w.contains(" ( "),
                        "window {idx} shows WordPiece-decoded punctuation: {w:?}"
                    );
                }
                // WordPiece decode lowercases — raw slicing preserves `CagraError`.
                // We only check the CagraError part appears in at least one window.
            }
            let any_has_camel = windows.iter().any(|(w, _)| w.contains("CagraError"));
            assert!(
                any_has_camel,
                "no window contains `CagraError` — decoding lowercased the text"
            );
        }
    }

    // ===== TC-45: ensure_model / CQS_ONNX_DIR path tests =====

    mod ensure_model_tests {
        use super::*;

        // ONNX_DIR_MUTEX hoisted to the crate-level shared
        // `crate::ONNX_DIR_ENV_LOCK` (#1305) so this test mod serializes
        // against `embedder_init_failure` (sibling) and the
        // `cli::commands::infra::doctor::tests` cohort. Pre-fix all three
        // had separate Mutex instances and raced on `CQS_ONNX_DIR` under
        // cargo's parallel runner — a poisoned lock from a 401-on-CI HF
        // download cascaded into PoisonError on the next two tests.

        fn test_model_config() -> ModelConfig {
            ModelConfig {
                name: "test".to_string(),
                repo: "test/model".to_string(),
                onnx_path: "onnx/model.onnx".to_string(),
                tokenizer_path: "tokenizer.json".to_string(),
                dim: 768,
                max_seq_length: 512,
                query_prefix: String::new(),
                doc_prefix: String::new(),
                input_names: crate::embedder::models::InputNames::bert(),
                output_name: "last_hidden_state".to_string(),
                pooling: crate::embedder::models::PoolingStrategy::Mean,
                approx_download_bytes: None,
                pad_id: 0,
            }
        }

        #[test]
        fn cqs_onnx_dir_structured_layout() {
            let _lock = crate::ONNX_DIR_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::TempDir::new().unwrap();
            let onnx_dir = dir.path().join("onnx");
            std::fs::create_dir_all(&onnx_dir).unwrap();
            std::fs::write(onnx_dir.join("model.onnx"), b"fake").unwrap();
            std::fs::write(dir.path().join("tokenizer.json"), b"fake").unwrap();

            std::env::set_var("CQS_ONNX_DIR", dir.path().to_str().unwrap());
            let result = ensure_model(&test_model_config());
            std::env::remove_var("CQS_ONNX_DIR");

            let (model, tok) = result.unwrap();
            assert!(
                model.to_string_lossy().ends_with("model.onnx"),
                "Expected model path ending in model.onnx, got {:?}",
                model
            );
            assert!(
                tok.to_string_lossy().ends_with("tokenizer.json"),
                "Expected tokenizer path ending in tokenizer.json, got {:?}",
                tok
            );
        }

        #[test]
        fn cqs_onnx_dir_flat_layout() {
            let _lock = crate::ONNX_DIR_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::TempDir::new().unwrap();
            std::fs::write(dir.path().join("model.onnx"), b"fake").unwrap();
            std::fs::write(dir.path().join("tokenizer.json"), b"fake").unwrap();

            std::env::set_var("CQS_ONNX_DIR", dir.path().to_str().unwrap());
            let result = ensure_model(&test_model_config());
            std::env::remove_var("CQS_ONNX_DIR");

            let (model, tok) = result.unwrap();
            assert!(
                model.to_string_lossy().ends_with("model.onnx"),
                "Expected model path ending in model.onnx, got {:?}",
                model
            );
            assert!(
                tok.to_string_lossy().ends_with("tokenizer.json"),
                "Expected tokenizer path ending in tokenizer.json, got {:?}",
                tok
            );
        }

        #[test]
        fn cqs_onnx_dir_missing_files_falls_through() {
            let _lock = crate::ONNX_DIR_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::TempDir::new().unwrap();
            // Empty dir -- neither structured nor flat layout

            std::env::set_var("CQS_ONNX_DIR", dir.path().to_str().unwrap());
            let result = ensure_model(&test_model_config());
            std::env::remove_var("CQS_ONNX_DIR");

            // Falls through to HF download -- which will fail in test env,
            // but the point is it didn't return the CQS_ONNX_DIR path
            assert!(
                result.is_err() || !result.as_ref().unwrap().0.starts_with(dir.path()),
                "Should not return paths from empty CQS_ONNX_DIR"
            );
        }
    }

    // ===== TC-11: Embedder init failure path =====

    mod embedder_init_failure {
        use super::*;

        // ONNX_DIR_MUTEX hoisted to `crate::ONNX_DIR_ENV_LOCK` (#1305) so
        // this mod serializes against `ensure_model_tests` and
        // `doctor::tests`. See the comment in `ensure_model_tests` above.

        #[test]
        fn embedder_with_bogus_onnx_path_returns_err_on_embed() {
            // TC-11: Verify that an Embedder with a ModelConfig pointing to
            // a nonexistent ONNX path returns Err (not panic) when embed is called.
            let _lock = crate::ONNX_DIR_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            let dir = tempfile::TempDir::new().unwrap();
            // Create only the tokenizer file, leave ONNX model missing
            std::fs::write(dir.path().join("tokenizer.json"), b"{}").unwrap();
            std::fs::create_dir_all(dir.path().join("onnx")).unwrap();
            // Deliberately do NOT create onnx/model.onnx

            let config = ModelConfig {
                name: "bogus".to_string(),
                repo: "nonexistent/model".to_string(),
                onnx_path: "onnx/model.onnx".to_string(),
                tokenizer_path: "tokenizer.json".to_string(),
                dim: 768,
                max_seq_length: 512,
                query_prefix: String::new(),
                doc_prefix: String::new(),
                input_names: crate::embedder::models::InputNames::bert(),
                output_name: "last_hidden_state".to_string(),
                pooling: crate::embedder::models::PoolingStrategy::Mean,
                approx_download_bytes: None,
                pad_id: 0,
            };

            // Point CQS_ONNX_DIR at our incomplete dir (has tokenizer but no model)
            // With CQS_ONNX_DIR set but model missing, ensure_model falls through
            // to HF download which fails in test env.
            std::env::set_var("CQS_ONNX_DIR", dir.path().to_str().unwrap());
            let embedder = Embedder::new_cpu(config);
            std::env::remove_var("CQS_ONNX_DIR");

            // Embedder::new() itself may succeed (lazy) or fail (ensure_model fallthrough)
            // Either way, we should get a clean error, not a panic
            match embedder {
                Ok(emb) => {
                    // Lazy init: the session isn't created until embed is called.
                    // Calling embed_query should fail because the model file doesn't exist.
                    let result = emb.embed_query("test query");
                    assert!(
                        result.is_err(),
                        "embed_query should return Err with missing model, got Ok"
                    );
                }
                Err(_e) => {
                    // Early failure at construction time is also acceptable --
                    // the key is that it's an Err, not a panic.
                }
            }
        }

        #[test]
        fn embedder_init_failure_is_not_cached() {
            // TC-11: Verify that after an Embedder returns Err on embed,
            // calling embed again also returns Err (no cached bad state).
            let _lock = crate::ONNX_DIR_ENV_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());

            let dir = tempfile::TempDir::new().unwrap();
            // Create empty dir -- no model files at all
            std::env::set_var("CQS_ONNX_DIR", dir.path().to_str().unwrap());
            let embedder = Embedder::new_cpu(ModelConfig {
                name: "bogus".to_string(),
                repo: "nonexistent/model".to_string(),
                onnx_path: "model.onnx".to_string(),
                tokenizer_path: "tokenizer.json".to_string(),
                dim: 768,
                max_seq_length: 512,
                query_prefix: String::new(),
                doc_prefix: String::new(),
                input_names: crate::embedder::models::InputNames::bert(),
                output_name: "last_hidden_state".to_string(),
                pooling: crate::embedder::models::PoolingStrategy::Mean,
                approx_download_bytes: None,
                pad_id: 0,
            });
            std::env::remove_var("CQS_ONNX_DIR");

            match embedder {
                Ok(emb) => {
                    let first = emb.embed_query("test");
                    let second = emb.embed_query("test again");
                    assert!(first.is_err(), "First embed should fail");
                    assert!(
                        second.is_err(),
                        "Second embed should also fail (not cached bad state)"
                    );
                }
                Err(_) => {
                    // Early failure is fine -- both calls would fail anyway
                }
            }
        }
    }
}
