//! Store opening, command context, and vector index building.
//!
//! Extracted from `mod.rs` to keep the CLI module hub lean.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;

use super::config::find_project_root;
use super::definitions;

/// Shared helper: locate project root and index, open store with the given opener.
fn open_store_with(
    opener: fn(&Path) -> std::result::Result<cqs::Store, cqs::store::StoreError>,
) -> Result<(cqs::Store, PathBuf, PathBuf)> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = opener(&index_path)
        .map_err(|e| anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e))?;
    Ok((store, root, cqs_dir))
}

/// Open the project store, returning the store, project root, and index directory.
/// Bails with a user-friendly message if no index exists.
pub(crate) fn open_project_store() -> Result<(cqs::Store, PathBuf, PathBuf)> {
    open_store_with(cqs::Store::open)
}

/// Open the project store with a single-threaded runtime for read-only commands.
/// Same as [`open_project_store`] but uses `Store::open_readonly_pooled()` which creates a
/// `current_thread` tokio runtime (1 OS thread) instead of `multi_thread` (4 OS threads).
/// Keeps full 256MB mmap and 16MB cache for search performance.
pub(crate) fn open_project_store_readonly() -> Result<(cqs::Store, PathBuf, PathBuf)> {
    open_store_with(cqs::Store::open_readonly_pooled)
}

/// Shared context for CLI commands that need an open store.
/// Created once in dispatch, passed to all store-using handlers.
/// Eliminates per-handler `open_project_store_readonly()` calls.
pub(crate) struct CommandContext<'a> {
    pub cli: &'a definitions::Cli,
    pub store: cqs::Store,
    pub root: PathBuf,
    pub cqs_dir: PathBuf,
    reranker: OnceLock<cqs::Reranker>,
    embedder: OnceLock<cqs::Embedder>,
    splade_encoder: OnceLock<Option<cqs::splade::SpladeEncoder>>,
    splade_index: OnceLock<Option<cqs::splade::index::SpladeIndex>>,
}

impl<'a> CommandContext<'a> {
    /// Open the project store in read-only mode and build a command context.
    pub fn open_readonly(cli: &'a definitions::Cli) -> Result<Self> {
        let (store, root, cqs_dir) = open_project_store_readonly()?;
        Ok(Self {
            cli,
            store,
            root,
            cqs_dir,
            reranker: OnceLock::new(),
            embedder: OnceLock::new(),
            splade_encoder: OnceLock::new(),
            splade_index: OnceLock::new(),
        })
    }

    /// Open the project store in read-write mode and build a command context.
    ///
    /// Used by write commands (gc, etc.) that need the lazy embedder/reranker
    /// from `CommandContext` but also need a writable store.
    pub fn open_readwrite(cli: &'a definitions::Cli) -> Result<Self> {
        let _span = tracing::info_span!("CommandContext::open_readwrite").entered();
        let (store, root, cqs_dir) = open_project_store()?;
        Ok(Self {
            cli,
            store,
            root,
            cqs_dir,
            reranker: OnceLock::new(),
            embedder: OnceLock::new(),
            splade_encoder: OnceLock::new(),
            splade_index: OnceLock::new(),
        })
    }

    /// Get the resolved model config from the CLI.
    #[allow(deprecated)]
    pub fn model_config(&self) -> &cqs::embedder::ModelConfig {
        self.cli.model_config()
    }

    /// Get or lazily create the cross-encoder reranker.
    ///
    /// The ONNX session (~91MB) is created on first call and reused for
    /// all subsequent reranking within this CLI invocation.
    pub fn reranker(&self) -> Result<&cqs::Reranker> {
        if let Some(r) = self.reranker.get() {
            return Ok(r);
        }
        let _span = tracing::info_span!("command_context_reranker_init").entered();
        let r = cqs::Reranker::new().map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
        let _ = self.reranker.set(r);
        Ok(self
            .reranker
            .get()
            .expect("reranker OnceLock populated by set() above"))
    }

    /// Get or lazily create the embedder.
    ///
    /// The ONNX session is created on first call and reused for
    /// all subsequent embedding within this CLI invocation.
    pub fn embedder(&self) -> Result<&cqs::Embedder> {
        if let Some(e) = self.embedder.get() {
            return Ok(e);
        }
        let _span = tracing::info_span!("command_context_embedder_init").entered();
        let e = cqs::Embedder::new(self.model_config().clone())
            .map_err(|e| anyhow::anyhow!("Embedder init failed: {e}"))?;
        let _ = self.embedder.set(e);
        Ok(self
            .embedder
            .get()
            .expect("embedder OnceLock populated by set() above"))
    }

    /// Get or lazily load the SPLADE encoder.
    /// Returns None if the SPLADE model is not available.
    pub fn splade_encoder(&self) -> Option<&cqs::splade::SpladeEncoder> {
        let opt = self.splade_encoder.get_or_init(|| {
            let _span = tracing::debug_span!("command_context_splade_encoder_init").entered();
            let model_dir = dirs::home_dir()
                .map(|h| h.join(".cache/huggingface/splade-onnx"))
                .unwrap_or_default();
            if !model_dir.join("model.onnx").exists() {
                tracing::warn!("SPLADE model not found, hybrid search unavailable");
                return None;
            }
            match cqs::splade::SpladeEncoder::new(
                &model_dir,
                cqs::splade::SpladeEncoder::default_threshold(),
            ) {
                Ok(enc) => Some(enc),
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load SPLADE encoder");
                    None
                }
            }
        });
        opt.as_ref()
    }

    /// Get or lazily load the SPLADE inverted index from SQLite.
    /// Returns None if no sparse vectors are stored.
    pub fn splade_index(&self) -> Option<&cqs::splade::index::SpladeIndex> {
        let opt = self.splade_index.get_or_init(|| {
            let _span = tracing::debug_span!("command_context_splade_index_init").entered();
            match self.store.load_all_sparse_vectors() {
                Ok(vectors) if !vectors.is_empty() => {
                    let idx = cqs::splade::index::SpladeIndex::build(vectors);
                    tracing::info!(chunks = idx.len(), "SPLADE index loaded");
                    Some(idx)
                }
                Ok(_) => {
                    tracing::debug!("No sparse vectors in store, SPLADE index unavailable");
                    None
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load sparse vectors");
                    None
                }
            }
        });
        opt.as_ref()
    }
}

/// Build the best available vector index for the store.
/// Priority: CAGRA (GPU, large indexes) > HNSW (CPU) > brute-force (None).
/// CAGRA rebuilds index each CLI invocation (~1s for 474 vectors).
/// Only worth it when search time savings exceed rebuild cost.
/// Threshold: 5000 vectors (where CAGRA search is ~10x faster than HNSW).
pub(crate) fn build_vector_index(
    store: &cqs::Store,
    cqs_dir: &Path,
) -> Result<Option<Box<dyn cqs::index::VectorIndex>>> {
    build_vector_index_with_config(store, cqs_dir, None)
}

/// Builds a vector index for the store with the specified configuration.
/// Attempts to build a GPU-accelerated CAGRA index if the store contains enough vectors and GPU support is available. Falls back to HNSW index otherwise. If the HNSW index is detected to be stale due to an interrupted write, returns None to fall back to brute-force search.
/// # Arguments
/// * `store` - Reference to the data store containing vectors to index
/// * `cqs_dir` - Path to the CQS directory
/// * `ef_search` - Optional search parameter to configure index behavior
/// # Returns
/// Returns `Ok(Some(index))` with a boxed vector index implementation if indexing succeeds, or `Ok(None)` if the index is stale or unavailable.
/// # Errors
/// Returns an error if the HNSW index building fails or store operations encounter errors.
pub(crate) fn build_vector_index_with_config(
    store: &cqs::Store,
    cqs_dir: &Path,
    ef_search: Option<usize>,
) -> Result<Option<Box<dyn cqs::index::VectorIndex>>> {
    let _span = tracing::info_span!("build_vector_index_with_config").entered();
    let _ = store; // Used only with gpu-index feature
    #[cfg(feature = "gpu-index")]
    {
        let cagra_threshold: u64 = std::env::var("CQS_CAGRA_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);
        let chunk_count = store.chunk_count().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to get chunk count for CAGRA threshold check");
            0
        });
        if chunk_count >= cagra_threshold && cqs::cagra::CagraIndex::gpu_available() {
            match cqs::cagra::CagraIndex::build_from_store(store, store.dim()) {
                Ok(idx) => {
                    tracing::info!("Using CAGRA GPU index ({} vectors)", idx.len());
                    return Ok(Some(Box::new(idx) as Box<dyn cqs::index::VectorIndex>));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to build CAGRA index, falling back to HNSW");
                }
            }
        } else if chunk_count < cagra_threshold {
            tracing::debug!(
                "Index too small for CAGRA ({} < {}), using HNSW",
                chunk_count,
                cagra_threshold
            );
        } else {
            tracing::debug!("GPU not available, using HNSW");
        }
    }
    // Check for crash between SQLite commit and HNSW save (RT-DATA-6)
    if store.is_hnsw_dirty().unwrap_or(true) {
        tracing::warn!(
            "HNSW index may be stale (interrupted write detected). \
             Falling back to brute-force search. Run 'cqs index' to rebuild."
        );
        return Ok(None);
    }
    Ok(cqs::HnswIndex::try_load_with_ef(
        cqs_dir,
        ef_search,
        Some(store.dim()),
    ))
}

/// Phase 5: load the base (non-enriched) HNSW index for adaptive routing.
///
/// Returns `Ok(None)` when:
/// - The `index_base.hnsw.*` files don't exist (e.g. fresh v17→v18 migration)
/// - The store is flagged `hnsw_dirty` (interrupted write)
/// - `CQS_DISABLE_BASE_INDEX=1` is set in the environment (eval A/B testing)
/// - CAGRA is preferred for the enriched index; we never build CAGRA for the
///   base — the base path is a narrow router decision, not a hot path, so
///   plain HNSW is sufficient
///
/// The router falls back to the enriched index when this returns `None`.
pub(crate) fn build_base_vector_index(
    store: &cqs::Store,
    cqs_dir: &Path,
) -> Result<Option<Box<dyn cqs::index::VectorIndex>>> {
    let _span = tracing::info_span!("build_base_vector_index").entered();

    // Eval A/B bypass: forces fallback to enriched even when index_base exists.
    // Lets us measure the marginal contribution of routing on the same corpus
    // without rebuilding the index.
    if std::env::var("CQS_DISABLE_BASE_INDEX").as_deref() == Ok("1") {
        tracing::info!("CQS_DISABLE_BASE_INDEX=1 — base index bypass active");
        return Ok(None);
    }

    if store.is_hnsw_dirty().unwrap_or(true) {
        tracing::warn!(
            "Base HNSW index may be stale (dirty flag set) — router falls back to enriched"
        );
        return Ok(None);
    }
    Ok(cqs::HnswIndex::try_load_base_with_ef(
        cqs_dir,
        None,
        Some(store.dim()),
    ))
}

#[cfg(test)]
mod base_index_tests {
    use super::*;
    use std::sync::Mutex;

    /// Process-wide lock — env-touching tests must serialize so they don't
    /// race against each other (env::set_var/remove_var are global state).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Build a deterministic L2-normalized embedding from a seed value.
    /// Inlined here because cqs::test_helpers is `#[cfg(test)]`-gated in the
    /// library crate and bin-crate test code can't reach it.
    fn make_embedding(seed: f32, dim: usize) -> cqs::embedder::Embedding {
        let mut v = vec![seed; dim];
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        cqs::embedder::Embedding::new(v)
    }

    /// Phase 5 invariant: `CQS_DISABLE_BASE_INDEX=1` short-circuits
    /// `build_base_vector_index` to return `None` even when the
    /// `index_base.hnsw.*` files exist on disk and the store is clean.
    /// This is the load-bearing behavior for same-corpus A/B eval.
    #[test]
    fn test_disable_base_index_env_short_circuits_with_files_present() {
        let _guard = ENV_LOCK.lock().unwrap();

        // Set up a real Store + a real index_base.hnsw.* fixture so we
        // exercise the actual file-load path, not just the early return.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        // Mark the store as clean so we don't get filtered out by the
        // hnsw_dirty branch — that branch fires before the file load but
        // AFTER the env-var check, so we still test the early return.
        store.set_hnsw_dirty(false).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..10)
            .map(|i| (format!("vec{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        index.save(dir.path(), "index_base").unwrap();

        // ── Sanity: without the bypass, the function loads the base index ──
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");
        let loaded = build_base_vector_index(&store, dir.path()).unwrap();
        assert!(
            loaded.is_some(),
            "without bypass, base files present + store clean → should load"
        );
        assert_eq!(loaded.unwrap().len(), 10);

        // ── With the bypass, the function returns None despite files existing ──
        std::env::set_var("CQS_DISABLE_BASE_INDEX", "1");
        let bypassed = build_base_vector_index(&store, dir.path()).unwrap();
        assert!(
            bypassed.is_none(),
            "with CQS_DISABLE_BASE_INDEX=1, base files exist + store clean \
             → must return None (this is the load-bearing A/B-eval behavior)"
        );
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");

        // ── And that the bypass is reset cleanly: removing it brings the
        //    function back to its normal load behavior ──
        let after_unset = build_base_vector_index(&store, dir.path()).unwrap();
        assert!(
            after_unset.is_some(),
            "after env var unset, normal load path should resume"
        );
    }

    /// `CQS_DISABLE_BASE_INDEX` only triggers for the literal value "1".
    /// Any other value (including "true", "yes", "0", empty) must NOT activate
    /// the bypass — we don't want a stray export accidentally suppressing
    /// the base index.
    #[test]
    fn test_disable_base_index_env_strict_value_match() {
        let _guard = ENV_LOCK.lock().unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        store.set_hnsw_dirty(false).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..5)
            .map(|i| (format!("v{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        index.save(dir.path(), "index_base").unwrap();

        for non_one in ["", "0", "true", "yes", "on", "TRUE", " 1", "1 ", "false"] {
            std::env::set_var("CQS_DISABLE_BASE_INDEX", non_one);
            let result = build_base_vector_index(&store, dir.path()).unwrap();
            assert!(
                result.is_some(),
                "CQS_DISABLE_BASE_INDEX={non_one:?} must NOT activate bypass"
            );
        }
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");
    }
}
