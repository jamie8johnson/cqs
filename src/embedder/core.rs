//! `Embedder`: session lifecycle, encoding, and batch paths.
//!
//! Split out of the former monolithic `embedder/mod.rs` (issue #1691). Error
//! and value types (`EmbedderError`, `Embedding`, `ExecutionProvider`),
//! download helpers, and pooling math live in sibling modules and are reached
//! via `use super::*`.

use super::*;
use crate::ort_helpers::ort_err;
use lru::LruCache;
use ndarray::{Array2, Array3};
use once_cell::sync::OnceCell;
use ort::session::Session;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Text embedding generator using a configurable model (default: EmbeddingGemma-300m).
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
    /// Stored as `Mutex<Option<Arc<Tokenizer>>>` so `clear_session` can drop the
    /// tokenizer alongside the ONNX session. Accessor `tokenizer()` hands back an
    /// `Arc<Tokenizer>` clone — `Tokenizer::encode` takes `&self`, so call sites
    /// using `arc.encode(...)` work via `Arc` deref without touching the mutex
    /// during inference.
    tokenizer: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    /// Lazy truncation-disabled clone of [`Self::tokenizer`].
    ///
    /// `split_into_windows`, `token_count`, and `token_counts_batch` all need
    /// full-sequence token counts, but some preset tokenizers ship
    /// `tokenizer.json` with `truncation: {max_length: 512}` baked in (see
    /// `split_into_windows` for the windowing-loss failure mode). Cloning the
    /// tokenizer per call to flip truncation off deep-copies the full vocab
    /// (~262k entries for EmbeddingGemma) — ~14k clones per reindex. Clone
    /// once here instead and hand out `Arc` clones.
    ///
    /// Same `Mutex<Option<Arc<..>>>` shape as `tokenizer` (not `OnceLock`) so
    /// [`Self::clear_session`] can drop it for a model swap — a stale
    /// no-trunc tokenizer from the previous model would silently mis-tokenize
    /// everything after the swap.
    tokenizer_no_trunc: Mutex<Option<Arc<tokenizers::Tokenizer>>>,
    /// Lazy-loaded model paths (avoids HuggingFace API calls until actually embedding)
    model_paths: OnceCell<(PathBuf, PathBuf)>,
    /// Lazy execution-provider resolution. `select_provider()` probes for CUDA
    /// and runs symlink ops, so deferring it via `OnceLock` keeps commands that
    /// never embed (notes list, slot list, cache stats, …) from paying that
    /// cost. `None` in the initial slot encodes "no provider was eagerly
    /// chosen"; a `Some` pre-populated by `new_with_provider(_, CPU)` keeps the
    /// explicit `Embedder::new_cpu` shortcut working.
    provider: std::sync::OnceLock<ExecutionProvider>,
    max_length: usize,
    /// LRU cache for query embeddings (avoids re-computing same queries)
    query_cache: Mutex<LruCache<String, Embedding>>,
    /// Disk-backed query cache (persists across CLI invocations).
    /// Best-effort: failures are logged and silently skipped.
    ///
    /// Lazily opened on first `embed_query` so commands that never touch query
    /// embeddings (`notes list`, `slot list`, `cache stats`, etc.) skip the WSL
    /// DrvFS 30-50ms cold-open + 7-day prune. The outer `OnceLock` is
    /// initialized empty; the inner `Option` is populated on first access —
    /// `Some` if the cache opened successfully, `None` if it failed.
    disk_query_cache: std::sync::OnceLock<Option<crate::cache::QueryCache>>,
    /// Detected embedding dimension from the model. Set on first inference.
    ///
    /// `Mutex<Option<usize>>` rather than `OnceLock<usize>` so
    /// [`Self::clear_session`] can reset the slot under `&self`. Without the
    /// reset, a model swap would read the first-loaded model's dim forever —
    /// silently feeding the wrong dim to `EmbeddingCache::read_batch`'s
    /// dimension filter (which then drops every cache hit on dim mismatch).
    /// Mutex contention is irrelevant: a single quick lock per
    /// `embedding_dim()` call.
    detected_dim: Mutex<Option<usize>>,
    /// Model configuration (repo, paths, prefixes, dimensions)
    model_config: ModelConfig,
    /// blake3 fingerprint of the ONNX model file, computed lazily on first access.
    /// Used as cache key to distinguish models with the same name but different weights.
    ///
    /// `Mutex<Option<String>>` rather than `OnceLock<String>` so
    /// [`Self::clear_session`] can reset the slot alongside the session drop.
    /// Without the reset, a model swap would read the first-loaded model's
    /// fingerprint — silently caching every new embedding under the wrong
    /// model_id key in the on-disk embedding cache.
    model_fingerprint: Mutex<Option<String>>,
    /// Pad token id resolved at tokenizer-init time.
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
/// of vector data plus the cache key; with the default embeddinggemma-300m
/// (768-dim) that is ~3 KB/entry, with bge-large (1024-dim) ~4 KB/entry,
/// and qwen3-embedding (2560/4096-dim) 10-16 KB/entry. Override with
/// `CQS_QUERY_CACHE_SIZE`.
///
/// 1024 entries: daemon-mode agent fleets routinely hit 30+ unique queries per
/// task (scout, gather, where, task), so a smaller cache is a coin toss for hit
/// rate. 1024 is ~3 MB at default, ~16 MB at qwen3-8B — trivial vs the model
/// footprint.
const DEFAULT_QUERY_CACHE_SIZE: usize = 1024;

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
    /// Provider selection (CUDA probe + ORT EP symlink ops) is also
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

    /// Build an embedder without resolving the execution provider.
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
        // Pre-populate the OnceLock so `provider()` returns this explicit
        // choice without ever calling `select_provider()`.
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

        // Disk-cache open + 7-day prune is deferred until first `embed_query`,
        // so the commands that never embed a query (notes/slot/cache/etc.)
        // don't pay 30-50ms on WSL DrvFS for a cache they never touch.

        Ok(Self {
            session: Mutex::new(None),
            tokenizer: Mutex::new(None),
            tokenizer_no_trunc: Mutex::new(None),
            model_paths: OnceCell::new(),
            // Lazy. Both `new_lazy_provider` and `new_with_provider`
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

    /// Seed the in-memory query cache so [`Self::embed_query`] returns `vec`
    /// for `text` without loading the ONNX model.
    ///
    /// A test affordance, not part of the inference path: it lets a unit test in
    /// the *binary* crate (where the library's `#[cfg(test)]` items aren't
    /// visible) drive a query path that needs an embedding — e.g. the search
    /// prelude — on a canned vector, with no model download or inference. Keying
    /// mirrors [`Self::embed_query`] (the input is `trim`med before lookup).
    pub fn seed_query_cache(&self, text: &str, vec: Embedding) {
        let key = text.trim().to_string();
        let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.put(key, vec);
    }

    /// Lazy provider accessor. Resolves on first call by running the CUDA
    /// probe, then memoises. Pre-populated by `new_with_provider` for the
    /// explicit-CPU path.
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
        // Stable fallback fingerprint — must NOT include any value that
        // changes across process restarts. Cross-slot embedding cache copy by
        // content_hash relies on the model fingerprint matching across runs, so
        // a per-restart Unix timestamp shape would fragment the cache and
        // orphan every fallback embedding.
        fn fallback_fingerprint(repo: &str, size: u64) -> String {
            format!("{}:fallback:size={}", repo, size)
        }
        // Lock-and-init pattern. Returns owned `String` (not `&str`) so the
        // value lives independently of the mutex guard. `clear_session` resets
        // the inner Option to None so a model swap re-fingerprints on next
        // access. Fast-path: lock, see Some, clone, return.
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
                            // >2GB models skip the streaming hash (would OOM on
                            // 32-bit / RAM-constrained boxes) and use a
                            // size-only fallback. Parity with the hash-failure
                            // path below — operators see the same shape
                            // regardless of which fallback fired.
                            let fp = fallback_fingerprint(&self.model_config.repo, meta.len());
                            tracing::info!(
                                size = meta.len(),
                                "Model >2GB, using stable size-based fingerprint"
                            );
                            fp
                        }
                        _ => {
                            // Streaming `update_reader` hashes the ONNX file in
                            // constant memory (same pattern as the HNSW
                            // checksum in hnsw/persist.rs), avoiding a ~1.3 GB
                            // heap load for BGE-large.
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
                                            // Stable size-based fallback, not a
                                            // timestamp — a transient hash
                                            // failure must not mint a new
                                            // fingerprint per restart and thrash
                                            // the cache. When metadata also
                                            // fails (the same FS hiccup that
                                            // broke the hash), distinguish by
                                            // failure mode instead of collapsing
                                            // every model under
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
                                    // Stable size-based fallback (see above).
                                    // When metadata also fails, emit a no-stat
                                    // sentinel so distinct models don't all
                                    // share `:fallback:size=0`.
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
                    // Model path resolution failed entirely — no path to stat —
                    // but `:fallback:no-path` is still deterministic (does not
                    // vary by wall-clock).
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
    /// Returns an `Arc<Tokenizer>` so callers can release the mutex immediately
    /// and let `clear_session` drop the inner tokenizer without racing against
    /// in-flight inference. `Tokenizer::encode` / `decode` take `&self`, so
    /// call sites using `arc.encode(...)` work via `Arc` deref.
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

    /// Get or initialize the truncation-disabled tokenizer.
    ///
    /// First call deep-clones the base tokenizer and flips truncation off;
    /// every subsequent call returns an `Arc` clone of that one copy. The
    /// result is immutable after creation (call sites only `encode`), so
    /// sharing is safe. See the `tokenizer_no_trunc` field doc for why the
    /// per-call clone this replaces was expensive.
    fn tokenizer_no_trunc(&self) -> Result<Arc<tokenizers::Tokenizer>, EmbedderError> {
        {
            let guard = self
                .tokenizer_no_trunc
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(t) = guard.as_ref() {
                return Ok(Arc::clone(t));
            }
        }
        // Build outside the lock — the clone of a 262k-entry vocab is the
        // expensive part and shouldn't serialize concurrent callers.
        let base = self.tokenizer()?;
        let mut no_trunc = (*base).clone();
        if no_trunc.get_truncation().is_some() {
            no_trunc
                .with_truncation(None)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        }
        let built = Arc::new(no_trunc);
        let mut guard = self
            .tokenizer_no_trunc
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Another thread may have initialized while we were cloning; prefer
        // the first winner so Arc identity is stable.
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        *guard = Some(Arc::clone(&built));
        Ok(built)
    }

    /// Resolve the pad token id once, caching on the embedder.
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
        let resolved: i64 = match tokenizer.get_padding() {
            Some(p) => p.pad_id as i64,
            None => {
                // Warn once when tokenizer.json has no [padding] section and we
                // fall back to model_config.pad_id. Most HF tokenizer.json
                // exports include padding; a missing section silently skews
                // attention masks for custom models.
                static WARN_ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                if WARN_ONCE.set(()).is_ok() {
                    tracing::warn!(
                        model = %self.model_config.name,
                        fallback_pad_id = self.model_config.pad_id,
                        "tokenizer.json has no padding section — using model_config.pad_id"
                    );
                }
                self.model_config.pad_id
            }
        };
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
        // Debug span — per-chunk during indexing, per-query during retrieval.
        // Slow indexing on large files is hard to attribute between token_count
        // vs the ONNX forward without per-call timing.
        let _span = tracing::debug_span!("token_count", text_len = text.len()).entered();
        // Same truncation-bypass as `split_into_windows`: count actual
        // tokens, not whatever the tokenizer's `truncation` cap returns.
        // bge-large-ft and v9-200k ship tokenizer.json with
        // truncation.max_length=512, which silently caps `token_count`
        // and breaks every downstream "is this chunk too long?" check.
        let tok = self.tokenizer_no_trunc()?;
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
        let _span = tracing::debug_span!("token_counts_batch", count = texts.len()).entered();
        // Same truncation-bypass as `token_count` — count actual tokens
        // for accurate windowing decisions.
        let tok = self.tokenizer_no_trunc()?;
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

        // Use the cached truncation-disabled tokenizer so we count the FULL
        // token sequence, not whatever the tokenizer's `truncation` field
        // caps it to. Some preset tokenizers (notably bge-large-ft and
        // v9-200k, which were exported via `optimum-cli` with default
        // truncation enabled) ship `tokenizer.json` with
        // `truncation: {max_length: 512}`. Without the override, encode()
        // silently caps long content at 512 tokens, the windowing loop
        // sees `ids.len() <= max_tokens` and returns a single window —
        // dropping ~90% of long markdown sections from the embedding.
        // Inference paths (embed_query/embed_batch) intentionally keep the
        // truncation behavior because they need to clamp input to
        // max_seq_length anyway, so we only override here.
        let tokenizer_no_trunc = self.tokenizer_no_trunc()?;
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
        // `ModelConfig::embed_batch_size` scales the inner loop with model
        // dim/seq. BGE-large stays at 64; nomic-coderank (768 dim × 2048 seq)
        // drops to 16 to avoid OOM on 8 GB GPUs.
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
        // Completion event with output dim/count/time. The entry span only
        // carries inputs; without this, operators have no signal that the call
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
    /// Maximum input bytes before truncation.
    /// The tokenizer further truncates to max_seq_length tokens, but this
    /// prevents O(n) tokenization work on megabyte-sized inputs.
    /// Configurable via `CQS_MAX_QUERY_BYTES` (default 32768).
    fn max_query_bytes() -> usize {
        crate::limits::parse_env_usize("CQS_MAX_QUERY_BYTES", 32 * 1024)
    }

    pub fn embed_query(&self, text: &str) -> Result<Embedding, EmbedderError> {
        let _span = tracing::info_span!("embed_query").entered();
        // Time end-to-end so cache-hit vs. miss latency is queryable on the
        // completion events — distinguishes "model is suddenly slow" from
        // "cache hit rate cratered".
        let start = std::time::Instant::now();
        let text = text.trim();
        if text.is_empty() {
            return Err(EmbedderError::EmptyQuery);
        }
        // Truncate oversized input before tokenization to bound CPU work.
        let max_query_bytes = Self::max_query_bytes();
        let text = truncate_at_char_boundary(text, max_query_bytes);

        // Check in-memory LRU first
        {
            let mut cache = self.query_cache.lock().unwrap_or_else(|poisoned| {
                tracing::warn!("Query cache lock poisoned (prior panic), recovering");
                poisoned.into_inner()
            });
            if let Some(cached) = cache.get(text) {
                tracing::trace!(query = text, "Query cache hit (memory)");
                // Cache-aware completion (no query text — it leaks at trace
                // level otherwise) so operators tracking hit-rate see hits at
                // debug without enabling trace journals.
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
                // Disk-hit completion mirrors memory_hit shape.
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

        // Completion event so embed_query has parity with the embed_documents
        // log line. Debug-level — embed_query runs once per search and the
        // entry span already covers timing. elapsed_ms + cache-tier let the
        // journal distinguish "model slow on miss" from "everything was a hit".
        tracing::debug!(
            dim = self.embedding_dim(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            cache = "miss",
            "embed_query complete"
        );
        Ok(embedding)
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
        // Also clear query cache -- stale embeddings from old session would be wrong
        // if model config changes before session is re-created.
        let mut cache = self.query_cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.clear();
        // Drop the tokenizer too (~10MB on BGE-large, ~20MB on larger BPE
        // vocabularies). The Arc holds a strong ref so in-flight inference that
        // grabbed an Arc clone before this call continues with its own copy;
        // the inner `Option` slot is cleared and lazy-reloads on the next
        // `tokenizer()` access.
        let mut tok = self.tokenizer.lock().unwrap_or_else(|p| p.into_inner());
        // Surface the doubled-memory window when in-flight inference is
        // mid-encode. `Arc::strong_count > 1` means a worker thread holds a
        // clone of the old tokenizer; the inner Option clears here, but the
        // cloned Arc keeps the old tokenizer alive until that thread releases
        // it. Peak memory transiently exceeds the documented ~500 MB by the
        // tokenizer size (~10–20 MB on BGE-large). Operators correlating memory
        // spikes need this signal; an RwLock-around-tokenizer alternative is
        // higher-risk because it extends the inference critical section.
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
        drop(tok);
        // Drop the cached truncation-disabled clone alongside the base
        // tokenizer — a model swap must not leave the old model's no-trunc
        // tokenizer serving `split_into_windows` / `token_count`.
        {
            let mut no_trunc = self
                .tokenizer_no_trunc
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            *no_trunc = None;
        }
        // Reset detected_dim and model_fingerprint so a model swap re-detects
        // dim and re-fingerprints on next inference. Without this reset, the
        // Mutex<Option<...>> slots would carry the first-loaded model's values
        // forever, silently feeding the wrong dim to
        // `EmbeddingCache::read_batch`'s dimension filter and the wrong
        // fingerprint to the disk cache key.
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
        // Operators investigating "daemon takes 4s to come up" need a "warm
        // started/completed" anchor. The dummy embed_query triggers ~250 MB+
        // ORT session + tokenizer load (1-3s on first GPU inference).
        let _span = tracing::info_span!("embedder_warm", model = %self.model_config.name).entered();
        let start = std::time::Instant::now();
        // Validate the warmup result has the declared dimension so a
        // misconfigured ONNX session that returns shape [1,0] surfaces here
        // instead of "warmed" + a confusing dim-mismatch error on the first
        // user query.
        let warm_vec = self.embed_query("warmup")?;
        if warm_vec.as_slice().len() != self.embedding_dim() {
            return Err(EmbedderError::InferenceFailed(format!(
                "warmup output dim {} != declared embedding_dim {}",
                warm_vec.as_slice().len(),
                self.embedding_dim()
            )));
        }
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
        // Read through the Mutex<Option<usize>> slot. Falls back to the model
        // config's declared dim when no inference has populated the slot yet
        // (or after `clear_session` reset it).
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

        // Tokenize (lazy init tokenizer).
        // `encode_batch` requires `Vec<EncodeInput>` (owned), so `texts.to_vec()` is
        // unavoidable — the tokenizer API does not accept `&[impl AsRef<str>]`.
        let encodings = {
            let _tokenize = tracing::debug_span!("tokenize").entered();
            self.tokenizer()?
                .encode_batch(texts.to_vec(), true)
                .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?
        };

        // Pad to max length in batch. max_len is computed directly from
        // encodings (no allocation); the Array2 is built from encodings in one
        // pass instead of through `Vec<Vec<i64>>`.
        let max_len = encodings
            .iter()
            .map(|e| e.get_ids().len())
            .max()
            .unwrap_or(0)
            .min(self.max_length);

        // Read the pad id from the tokenizer (cached on first call).
        // `input_ids` uses the model-declared pad token; the attention mask
        // always pads with `0` regardless — a `0` mask entry zeroes the padded
        // position at attention time, which is the whole point of the mask.
        let input_pad_id = self.pad_id()?;
        let input_ids_arr =
            pad_2d_i64_from_encodings(&encodings, |e| e.get_ids(), max_len, input_pad_id);
        let attention_mask_arr =
            pad_2d_i64_from_encodings(&encodings, |e| e.get_attention_mask(), max_len, 0);

        // Create tensors. Clone the mask Array2 for the tensor so the
        // post-inference pooling can still read the original — one i64-Array2
        // clone (memcpy).
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
            // Some third-party ONNX exports (Qwen3-Embedding-4B) require an
            // explicit `position_ids` input. With right-padding (BERT-style —
            // the same shape `pad_2d_i64_from_encodings` emits via the
            // tokenizer's default), positions are simply `[0, 1, ...,
            // max_len-1]` for every row. Padding tokens get positions too;
            // they're masked out by `attention_mask` at attention time, same as
            // for `input_ids`. Extend directly from the range iterator;
            // saturating_mul guards the with_capacity arg.
            let mut pos_data: Vec<i64> = Vec::with_capacity(texts.len().saturating_mul(max_len));
            for _ in 0..texts.len() {
                pos_data.extend(0..max_len as i64);
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
        // Dispatch on output dtype. Most ONNX exports emit `Tensor<f32>`, but
        // FP16 / bfloat16 quantized exports (e.g. Qwen3-Embedding-4B-ONNX) emit
        // `Tensor<f16>` or `Tensor<bf16>` and the f32 extract fails fast with
        // "Cannot extract Tensor<f32> from Tensor<f16>" — strict dtype check,
        // not a silent reinterpret. Try f32 first (zero-copy fast path), then
        // half-precision variants on mismatch and convert each element to f32
        // in software. The conversion cost (one map per inference) is
        // negligible next to the model forward pass.
        //
        // `shape` and `data` after this block are owned `Vec<i64>` /
        // `Vec<f32>`; the f32 fast path pays one extra `.to_vec()` (~few
        // MB/batch) for shape uniformity vs branching the rest of the function
        // body.
        //
        // The bf16-fallback error preserves the swallowed f32 / f16 errors so
        // the surfaced error reflects the real root cause — an actual ORT
        // failure ("session output index out of range", "tensor backing memory
        // invalid") on the f32 path would otherwise be invisible behind a bare
        // bf16 "wrong-dtype" error. Each non-bf16 error is also logged at debug
        // as the cascade walks past it, so `RUST_LOG=cqs=debug` shows the
        // dtype-probe progression. The happy f32 path returns immediately; only
        // fallback hits the logging + carry overhead.
        let (shape_vec, data_vec): (Vec<i64>, Vec<f32>) = match output.try_extract_tensor::<f32>() {
            Ok((s, d)) => (s.to_vec(), d.to_vec()),
            Err(e_f32) => {
                tracing::debug!(error = %e_f32, "f32 tensor extract failed, trying f16");
                match output.try_extract_tensor::<half::f16>() {
                    Ok((s, d)) => (s.to_vec(), d.iter().map(|h| h.to_f32()).collect()),
                    Err(e_f16) => {
                        tracing::debug!(error = %e_f16, "f16 tensor extract failed, trying bf16");
                        match output.try_extract_tensor::<half::bf16>() {
                            Ok((s, d)) => (s.to_vec(), d.iter().map(|h| h.to_f32()).collect()),
                            Err(e_bf16) => {
                                // Surface all three so the operator sees which dtype
                                // probe was the real failure, not just the last one.
                                return Err(EmbedderError::InferenceFailed(format!(
                                    "tensor extract failed for all dtypes — f32: {e_f32}; \
                                     f16: {e_f16}; bf16: {e_bf16}"
                                )));
                            }
                        }
                    }
                }
            }
        };
        let shape: &[i64] = &shape_vec;
        let data: &[f32] = &data_vec;

        let batch_size = texts.len();
        let seq_len = max_len;

        // PoolingStrategy::Identity: the ONNX output is already pooled to
        // `[batch, dim]`. Skip the 3D reshape + pool dispatch and emit
        // L2-normalized rows directly. Used by EmbeddingGemma's
        // `sentence_embedding` output.
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
                // Lock-and-set the Mutex<Option<usize>> slot.
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
        // Set or validate embedding dimension from model output.
        // Lock-and-set the Mutex<Option<usize>> slot.
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
            // Surface as a structured error rather than panic. Identity is
            // intercepted by the 2D shortcut above — reaching here implies the
            // ONNX model emitted 3D output AND configured Identity pooling, a
            // config-shape mismatch. Error cleanly rather than crash the daemon.
            PoolingStrategy::Identity => {
                return Err(EmbedderError::InferenceFailed(
                    "PoolingStrategy::Identity is not supported on 3D model outputs — \
                     the ONNX model must already produce a 2D [batch, dim] tensor \
                     for Identity pooling. Re-export with mean/cls/last-token pooling \
                     baked in, or change the model_config pooling value."
                        .to_string(),
                ));
            }
        };

        let results = pooled_batch
            .into_iter()
            .map(|v| Embedding::new(normalize_l2(v)))
            .collect();

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    // ===== truncate_at_char_boundary =====
    //
    // Pins (a) the truncate path firing at all, and (b) multi-byte UTF-8
    // boundary handling when the cap lands mid-codepoint. The free function
    // lets these run without an ONNX session.

    #[test]
    fn truncate_at_char_boundary_fits_under_cap() {
        let s = "hello world";
        let out = truncate_at_char_boundary(s, 100);
        assert_eq!(out, s, "input under cap returns unchanged");
        assert!(std::ptr::eq(out, s) || out == s); // structural identity
    }

    #[test]
    fn truncate_at_char_boundary_ascii_truncates_at_cap() {
        let s = "abcdefghij";
        let out = truncate_at_char_boundary(s, 5);
        assert_eq!(out, "abcde");
        assert!(out.len() <= 5);
    }

    #[test]
    fn truncate_at_char_boundary_snaps_back_on_emoji() {
        // 99 ASCII bytes + a 4-byte emoji = 103 bytes total.
        // Cap = 100 lands in the middle of the emoji (bytes 99..103).
        // The naive `&text[..100]` would panic with
        // `byte index 100 is not a char boundary`.
        let mut s = "a".repeat(99);
        s.push('🦀'); // 4-byte UTF-8
        assert_eq!(s.len(), 103);
        let out = truncate_at_char_boundary(&s, 100);
        // Must snap back to byte 99 (just before the emoji starts) so
        // the result is valid UTF-8.
        assert_eq!(out.len(), 99);
        assert!(out.chars().all(|c| c == 'a'));
        // Sanity: the slice IS valid UTF-8 (str-typed already proves
        // this; the assertion is the test's whole point).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn truncate_at_char_boundary_at_exactly_cap_is_no_op() {
        let s = "🦀🦀🦀"; // 12 bytes, 3 chars
        let out = truncate_at_char_boundary(s, 12);
        assert_eq!(out, s);
    }

    #[test]
    fn truncate_at_char_boundary_zero_cap_returns_empty() {
        let s = "🦀abc";
        let out = truncate_at_char_boundary(s, 0);
        assert_eq!(out, "");
    }

    /// Snap-back convergence is bounded by 3 (the longest valid
    /// UTF-8 sequence is 4 bytes). A 4-byte emoji at the cap boundary
    /// snaps back at most 3 bytes — pin this so a future "snap forward"
    /// regression that walks past `text.len()` would fail the test.
    #[test]
    fn truncate_at_char_boundary_walk_distance_bounded() {
        let mut s = String::new();
        for _ in 0..50 {
            s.push('🦀');
        }
        // 200 bytes, 50 chars. Try caps 1..=200, every truncation must
        // succeed without panic and produce valid UTF-8 (str type
        // already guarantees this; we're asserting the function never
        // walks off the start).
        for cap in 1..=s.len() {
            let out = truncate_at_char_boundary(&s, cap);
            assert!(
                out.len() <= cap,
                "cap={cap}: out.len()={} exceeded cap",
                out.len()
            );
            // Walk distance from cap to chosen end is at most 3 bytes.
            assert!(
                cap - out.len() <= 3,
                "cap={cap}: walked back {} bytes (max 3)",
                cap - out.len()
            );
        }
    }

    // ===== FP16 / BF16 conversion smoke tests =====
    //
    // The embed loop dispatches output extraction on dtype
    // (`try_extract_tensor::<f32>` → fall back to `f16` then `bf16`). The ORT
    // extraction needs a live session, but the half-crate conversion arithmetic
    // is pinned at the unit level so a future `half` crate bump (or a precision
    // regression) trips a fast local test instead of a 5-7 hour reindex.
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

    // ===== Embedding::try_new tests =====

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

    // normalize_l2 has no numeric validation. If the input
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

    // embed_batch does not validate ORT output before Embedding::new. The
    // load-bearing contract test that lands without a real ORT session is that
    // Embedding::new accepts non-finite values (NaN, Inf). Since embed_batch
    // passes pooled rows through Embedding::new, a NaN-poisoned ORT output
    // becomes a NaN-poisoned Embedding and propagates into search scoring.
    // Embedding::try_new rejects non-finite — but `embed_batch` calls the
    // infallible `Embedding::new` instead. This test pins that mismatch.

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

    /// Pooling consumes `&Array2<i64>`. Tests build the mask as a flat
    /// `Vec<Vec<i64>>` for readability and convert here.
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
        // in `define_embedder_presets!` (EmbeddingGemma-300m, 768-dim).
        assert_eq!(EMBEDDING_DIM, 768);
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
        assert_matches!(err, EmbedderError::InferenceFailed(_));
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

            /// Property (#1949 boundary characterization): the self-dot of an
            /// f32-`normalize_l2`'d vector is NOT guaranteed `<= 1.0`. It is
            /// `~1.0` but can overshoot by a tiny epsilon — `normalize` divides
            /// by `sqrt(sum(x^2))` accumulated in f32, and `sum(x^2) /
            /// sum(x^2)` rounds slightly above 1 on ~40% of realistic inputs
            /// (worst observed excess ≈ 1.7e-6 at dim 768). This is WHY
            /// `DistDotClamped` (`hnsw/mod.rs`) clamps `dot.min(1.0)` and why
            /// every downstream cosine consumer (MMR, name-blend, note-boost)
            /// clamps its score range. The guard pins the magnitude of the
            /// overshoot: it must stay tiny (a regression that let it grow —
            /// e.g. a buggy normalize — would surface here), but it must NOT
            /// be asserted away as exactly `<= 1.0`, because that is false.
            /// A hand-written example can pick one vector that happens to land
            /// at-or-below 1.0 and conclude (wrongly) the bound holds; only a
            /// generator over the input space exposes the over-unit cases.
            #[test]
            fn prop_normalize_l2_self_dot_overshoot_bounded(
                v in prop::collection::vec(-10.0f32..10.0f32, 64..=1024)
            ) {
                let norm_sq: f32 = v.iter().map(|x| x * x).sum();
                prop_assume!(norm_sq > 1e-6); // skip near-zero-norm (left unscaled)
                let n = normalize_l2(v);
                let self_dot: f32 = n.iter().map(|x| x * x).sum();
                // The overshoot, when present, is bounded by a tiny epsilon.
                // 1e-3 is generous headroom over the ~1.7e-6 observed worst
                // case — a real regression (wrong divisor, missing sqrt) would
                // blow past it.
                prop_assert!(
                    (1.0 - 1e-3..=1.0 + 1e-3).contains(&self_dot),
                    "self-dot of normalized vector must be ~1.0 (±1e-3), got {self_dot}"
                );
            }

            /// Property (idempotence): `normalize_l2(normalize_l2(v)) ==
            /// normalize_l2(v)` within f32 tolerance. An already-unit vector
            /// must survive a second pass unchanged — a divisor bug or an
            /// `if norm_sq != 1.0` short-circuit would break this. Example
            /// tests apply normalize once and never the second time.
            #[test]
            fn prop_normalize_l2_idempotent(
                v in prop::collection::vec(-10.0f32..10.0f32, 64..=1024)
            ) {
                let norm_sq: f32 = v.iter().map(|x| x * x).sum();
                prop_assume!(norm_sq > 1e-6);
                let n1 = normalize_l2(v);
                let n2 = normalize_l2(n1.clone());
                for (a, b) in n1.iter().zip(n2.iter()) {
                    prop_assert!(
                        (a - b).abs() < 1e-5,
                        "normalize_l2 not idempotent: {a} vs {b}"
                    );
                }
            }
        }
    }

    // ===== Pooling batch-vs-single equivalence (proptest) =====
    //
    // The core inference-correctness invariant the example suite never
    // expresses: a row's pooled vector must be invariant to which OTHER rows
    // share its batch. Real BERT/decoder inference with right-padding +
    // attention masking produces the same per-token hidden states regardless
    // of batch composition; the poolers must preserve that. The hand-written
    // pooling tests (`mean_pool_respects_mask`, `cls_pool_returns_first_token`,
    // `last_token_pool_picks_last_unmasked`) each use ONE fixed batch and never
    // compare a row against the same row pooled alone — so a pooling bug that
    // leaked a neighbor's padded positions into a row's mean would pass them.
    mod pooling_batch_eq_single {
        use super::*;
        use proptest::prelude::*;

        /// Build a `[batch, seq, dim]` hidden tensor + right-padded mask from
        /// per-row true lengths. The SAME (token-position, dim) coordinate
        /// gets the SAME hidden value across every batch composition, modeling
        /// batch-invariant inference; padded positions (`j >= true_len`) get
        /// distinct junk that correct masking must cancel.
        fn build_batch(
            lens: &[usize],
            seed: &[f32],
            seq_len: usize,
            dim: usize,
        ) -> (Array3<f32>, Array2<i64>) {
            let batch = lens.len();
            let mut hidden = Array3::<f32>::zeros((batch, seq_len, dim));
            let mut mask = Array2::<i64>::zeros((batch, seq_len));
            for (i, &true_len) in lens.iter().enumerate() {
                for j in 0..seq_len {
                    for d in 0..dim {
                        let base = seed[d % seed.len()];
                        hidden[[i, j, d]] = if j < true_len {
                            base + (j as f32) * 0.5 + (d as f32) * 0.01
                        } else {
                            999.0 + j as f32 // padded junk — must be masked
                        };
                    }
                    mask[[i, j]] = if j < true_len { 1 } else { 0 };
                }
            }
            (hidden, mask)
        }

        proptest! {
            /// mean_pool: a row's mean is invariant to batch padding.
            #[test]
            fn prop_mean_pool_batch_eq_single(
                lens in prop::collection::vec(1usize..=12, 1..=5),
                seed in prop::collection::vec(-3.0f32..3.0, 4..=8),
            ) {
                let dim = seed.len();
                let max_len = *lens.iter().max().unwrap();
                let (hb, mb) = build_batch(&lens, &seed, max_len, dim);
                let batched = mean_pool(&hb, &mb, dim);
                for (i, &l) in lens.iter().enumerate() {
                    let (hs, ms) = build_batch(&[l], &seed, l, dim);
                    let single = mean_pool(&hs, &ms, dim);
                    for d in 0..dim {
                        prop_assert!(
                            (batched[i][d] - single[0][d]).abs() < 1e-3,
                            "mean_pool batch != single: row {i} dim {d} (len {l}): {} vs {}",
                            batched[i][d], single[0][d]
                        );
                    }
                }
            }

            /// cls_pool: the first token is invariant to batch padding.
            #[test]
            fn prop_cls_pool_batch_eq_single(
                lens in prop::collection::vec(1usize..=12, 1..=5),
                seed in prop::collection::vec(-3.0f32..3.0, 4..=8),
            ) {
                let dim = seed.len();
                let max_len = *lens.iter().max().unwrap();
                let (hb, _mb) = build_batch(&lens, &seed, max_len, dim);
                let batched = cls_pool(&hb);
                for (i, &l) in lens.iter().enumerate() {
                    let (hs, _ms) = build_batch(&[l], &seed, l, dim);
                    let single = cls_pool(&hs);
                    for d in 0..dim {
                        prop_assert!((batched[i][d] - single[0][d]).abs() < 1e-3);
                    }
                }
            }

            /// last_token_pool: the last unmasked token is invariant to batch
            /// padding (padding lives to the RIGHT of the last real token, so
            /// a longer sibling in the batch must not shift the pick).
            #[test]
            fn prop_last_token_pool_batch_eq_single(
                lens in prop::collection::vec(1usize..=12, 1..=5),
                seed in prop::collection::vec(-3.0f32..3.0, 4..=8),
            ) {
                let dim = seed.len();
                let max_len = *lens.iter().max().unwrap();
                let (hb, mb) = build_batch(&lens, &seed, max_len, dim);
                let batched = last_token_pool(&hb, &mb);
                for (i, &l) in lens.iter().enumerate() {
                    let (hs, ms) = build_batch(&[l], &seed, l, dim);
                    let single = last_token_pool(&hs, &ms);
                    for d in 0..dim {
                        prop_assert!(
                            (batched[i][d] - single[0][d]).abs() < 1e-3,
                            "last_token_pool batch != single: row {i} dim {d} (len {l})"
                        );
                    }
                }
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

    /// `clear_session` must reset `detected_dim` and `model_fingerprint` so a
    /// model swap re-detects on the next inference. This test directly mutates
    /// the Mutex<Option<...>> slots to simulate post-inference state, calls
    /// clear_session, and verifies the slots are now None — no model load
    /// required.
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
            // condition.
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

        /// max_tokens == 0 short-circuits to empty Vec
        /// without touching the tokenizer.
        #[test]
        #[ignore]
        fn split_into_windows_max_tokens_zero_returns_empty() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(_) => return,
            };
            let windows = embedder.split_into_windows("any text", 0, 0).unwrap();
            assert!(windows.is_empty());
        }

        /// Two calls to `tokenizer_no_trunc()` must return the SAME cached
        /// tokenizer (Arc identity), not a fresh deep clone per call — the
        /// per-call vocab clone was ~14k clones per reindex.
        #[test]
        #[ignore]
        fn tokenizer_no_trunc_is_cached_across_calls() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("E5-base unavailable in test env: {err}; skipping (#1305)");
                    return;
                }
            };
            let first = match embedder.tokenizer_no_trunc() {
                Ok(t) => t,
                Err(err) => {
                    eprintln!(
                        "tokenizer load failed (likely corrupt tokenizer.json from a partial \
                         HF cache): {err}; skipping (#1305)"
                    );
                    return;
                }
            };
            let second = embedder.tokenizer_no_trunc().unwrap();
            assert!(
                std::sync::Arc::ptr_eq(&first, &second),
                "second call must reuse the cached no-trunc tokenizer"
            );
            // clear_session drops the cache; the next call rebuilds a NEW Arc.
            embedder.clear_session();
            let third = embedder.tokenizer_no_trunc().unwrap();
            assert!(
                !std::sync::Arc::ptr_eq(&first, &third),
                "clear_session must drop the cached no-trunc tokenizer"
            );
        }

        /// overlap >= max_tokens/2 must error out, not
        /// produce O(2n/max_tokens) windows.
        #[test]
        #[ignore]
        fn split_into_windows_overlap_too_large_errors() {
            let embedder = match Embedder::new(ModelConfig::e5_base()) {
                Ok(e) => e,
                Err(_) => return,
            };
            // overlap=64 with max_tokens=128 → overlap >= max_tokens/2.
            let res = embedder.split_into_windows("any text", 128, 64);
            assert!(res.is_err(), "overlap >= max_tokens/2 must error");
        }
    }

    // ===== ensure_model / CQS_ONNX_DIR path tests =====

    mod ensure_model_tests {
        use super::*;

        // Uses the crate-level shared `crate::ONNX_DIR_ENV_LOCK` so this test
        // mod serializes against `embedder_init_failure` (sibling) and the
        // `cli::commands::infra::doctor::tests` cohort on `CQS_ONNX_DIR`.
        // Separate Mutex instances would race under cargo's parallel runner —
        // a poisoned lock from a 401-on-CI HF download cascades into
        // PoisonError on the next tests.

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

    // ===== Embedder init failure path =====

    mod embedder_init_failure {
        use super::*;

        // Uses `crate::ONNX_DIR_ENV_LOCK` so this mod serializes against
        // `ensure_model_tests` and `doctor::tests`. See the comment in
        // `ensure_model_tests` above.

        #[test]
        fn embedder_with_bogus_onnx_path_returns_err_on_embed() {
            // Verify that an Embedder with a ModelConfig pointing to
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
            // Verify that after an Embedder returns Err on embed,
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
