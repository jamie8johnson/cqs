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
/// Same as [`open_project_store`] but uses `Store::open_light()` which creates a
/// `current_thread` tokio runtime (1 OS thread) instead of `multi_thread` (4 OS threads).
/// Keeps full 256MB mmap and 16MB cache for search performance.
pub(crate) fn open_project_store_readonly() -> Result<(cqs::Store, PathBuf, PathBuf)> {
    open_store_with(cqs::Store::open_light)
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
        })
    }

    /// Get the resolved model config from the CLI.
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
    if store.is_hnsw_dirty().unwrap_or(false) {
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
