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
    ///
    /// Path resolution is delegated to [`cqs::splade::resolve_splade_model_dir`]
    /// — see that function's docs for env-var override and fallback rules.
    /// `SpladeEncoder::new` runs a vocab-mismatch probe at construction time,
    /// so a hot-swapped `model.onnx` with a stale `tokenizer.json` will fail
    /// fast here rather than silently producing garbage embeddings.
    ///
    /// Returns `None` when no usable model dir exists or the load fails —
    /// callers fall back to dense-only.
    pub fn splade_encoder(&self) -> Option<&cqs::splade::SpladeEncoder> {
        let opt = self.splade_encoder.get_or_init(|| {
            let _span = tracing::debug_span!("command_context_splade_encoder_init").entered();
            let model_dir = cqs::splade::resolve_splade_model_dir()?;
            match cqs::splade::SpladeEncoder::new(
                &model_dir,
                cqs::splade::SpladeEncoder::default_threshold(),
            ) {
                Ok(enc) => Some(enc),
                Err(e) => {
                    tracing::warn!(
                        path = %model_dir.display(),
                        error = %e,
                        "Failed to load SPLADE encoder"
                    );
                    None
                }
            }
        });
        opt.as_ref()
    }

    /// Get or lazily load the SPLADE inverted index.
    ///
    /// Tries the persisted on-disk index first (`splade.index.bin` next to
    /// the HNSW files). Falls back to building from SQLite and persisting
    /// the result if the file is absent, stale, corrupt, or version-mismatched.
    /// Returns `None` when the store contains no sparse vectors, or when the
    /// generation counter cannot be read at all (audit EH-3: substituting 0
    /// there would let a later successful `save()` write a gen-0 file whose
    /// header disagrees with whatever the DB actually holds, creating a
    /// self-perpetuating cache-poison loop).
    pub fn splade_index(&self) -> Option<&cqs::splade::index::SpladeIndex> {
        let opt = self.splade_index.get_or_init(|| {
            let _span = tracing::debug_span!("command_context_splade_index_init").entered();
            // Read the generation FIRST. If it fails, bail out — falling through
            // with generation=0 would let a later persist write a file labeled
            // gen-0 while the DB is at gen-N, and the next load would mismatch
            // and rebuild forever (audit EH-3).
            let generation = match self.store.splade_generation() {
                Ok(g) => g,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to read splade_generation — skipping SPLADE entirely for this \
                         invocation; search will fall back to dense-only"
                    );
                    return None;
                }
            };
            let splade_path = self.cqs_dir.join(cqs::splade::index::SPLADE_INDEX_FILENAME);
            // load_or_build returns an index unconditionally. It may be
            // None-worthy (no vectors in the store) — we detect that via a
            // zero-length id_map and skip.
            let store = &self.store;
            let (idx, rebuilt) =
                cqs::splade::index::SpladeIndex::load_or_build(&splade_path, generation, || {
                    match store.load_all_sparse_vectors() {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(error = %e, "Failed to load sparse vectors");
                            Vec::new()
                        }
                    }
                });
            if idx.is_empty() {
                tracing::debug!("No sparse vectors in store, SPLADE index unavailable");
                return None;
            }
            tracing::info!(
                chunks = idx.len(),
                tokens = idx.unique_tokens(),
                rebuilt,
                "SPLADE index ready"
            );
            Some(idx)
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
            // Issue #950: try the persisted index first. cuVS native
            // deserialize is fast (~sub-second even for tens of thousands of
            // vectors) compared to the ~30s rebuild on a mid-size repo, so
            // the daemon cold-start cost drops dramatically across
            // systemctl restarts / `cqs index` cycles. `load` validates
            // magic, dim, chunk_count, and blake3 before handing the blob
            // to cuVS, so a stale file falls through to rebuild rather
            // than corrupting results.
            let cagra_path = cqs_dir.join("index.cagra");
            if cqs::cagra::cagra_persist_enabled() && cagra_path.exists() {
                match cqs::cagra::CagraIndex::load(&cagra_path, store.dim(), chunk_count as usize) {
                    Ok(idx) => {
                        tracing::info!(
                            backend = "cagra",
                            source = "persisted",
                            vectors = idx.len(),
                            chunk_count,
                            cagra_threshold,
                            "Vector index backend selected"
                        );
                        return Ok(Some(Box::new(idx) as Box<dyn cqs::index::VectorIndex>));
                    }
                    Err(e) => {
                        // Sidecar mismatch / stale / corrupt — nuke both files
                        // so the next run doesn't pay the same load-then-fail
                        // cost and instead jumps straight to the rebuild path.
                        tracing::warn!(
                            error = %e,
                            path = %cagra_path.display(),
                            "CAGRA persisted load failed, rebuilding from store"
                        );
                        cqs::cagra::CagraIndex::delete_persisted(&cagra_path);
                    }
                }
            }

            match cqs::cagra::CagraIndex::build_from_store(store, store.dim()) {
                Ok(idx) => {
                    // OB-NEW-7: single structured log per backend selection so
                    // operators can grep a consistent `backend=` field instead
                    // of string-matching three distinct format messages.
                    tracing::info!(
                        backend = "cagra",
                        source = "rebuilt",
                        vectors = idx.len(),
                        chunk_count,
                        cagra_threshold,
                        "Vector index backend selected"
                    );

                    // Best-effort persist: a failed save is never fatal —
                    // we just rebuild on next startup. Keeping the warn at
                    // info level so operators can tell persistence is off
                    // without having to dig through debug logs.
                    if cqs::cagra::cagra_persist_enabled() {
                        if let Err(e) = idx.save_with_store(&cagra_path, store) {
                            tracing::warn!(
                                error = %e,
                                path = %cagra_path.display(),
                                "Failed to persist CAGRA index (will rebuild next restart)"
                            );
                        }
                    }

                    return Ok(Some(Box::new(idx) as Box<dyn cqs::index::VectorIndex>));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to build CAGRA index, falling back to HNSW");
                }
            }
        } else if chunk_count < cagra_threshold {
            // OB-NEW-7: promoted debug! → info! with structured fields so the
            // backend-selection decision is visible at the default log level.
            tracing::info!(
                backend = "hnsw",
                reason = "index_below_cagra_threshold",
                chunk_count,
                cagra_threshold,
                "Vector index backend selected"
            );
        } else {
            tracing::info!(
                backend = "hnsw",
                reason = "gpu_unavailable",
                chunk_count,
                cagra_threshold,
                "Vector index backend selected"
            );
        }
    }
    // Check for crash between SQLite commit and HNSW save (RT-DATA-6).
    // When dirty flag is set, verify the HNSW files pass their checksum before
    // falling back to brute-force. If checksum passes, the crash happened AFTER
    // the files were written — the dirty flag is a false positive, clear it
    // and proceed. If checksum fails, the files are genuinely stale.
    //
    // EH-16: surface metadata-read failures. Conservative fallback is still
    // "treat as dirty" but we emit a breadcrumb so mid-migration / corrupt
    // DB conditions don't get swallowed.
    // AC-V1.25-8: per-kind dirty flag (Enriched vs Base) so clearing one
    // does not silently mark the other clean.
    let dirty = match store.is_hnsw_dirty(cqs::HnswKind::Enriched) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                error = %e,
                hnsw_kind = "enriched",
                "Failed to read hnsw_dirty flag, treating as dirty"
            );
            true
        }
    };
    if dirty {
        match cqs::hnsw::verify_hnsw_checksums(cqs_dir, "index") {
            Ok(()) => {
                tracing::info!(
                    "HNSW dirty flag set but checksums pass — clearing flag (self-heal)"
                );
                if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Enriched, false) {
                    tracing::warn!(error = %e, "Failed to clear dirty flag");
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "HNSW index stale (checksum mismatch). \
                     Falling back to brute-force search. Run 'cqs index' to rebuild."
                );
                return Ok(None);
            }
        }
    }
    Ok(cqs::HnswIndex::try_load_with_ef(
        cqs_dir,
        ef_search,
        store.dim(),
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

    // Same self-heal logic as enriched: if checksums pass, clear the dirty
    // flag; otherwise fall back to enriched via the router.
    //
    // EH-16: surface metadata-read failures for the base index path too.
    let dirty = match store.is_hnsw_dirty(cqs::HnswKind::Base) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                error = %e,
                hnsw_kind = "base",
                "Failed to read hnsw_dirty flag, treating as dirty"
            );
            true
        }
    };
    if dirty {
        match cqs::hnsw::verify_hnsw_checksums(cqs_dir, "index_base") {
            Ok(()) => {
                tracing::info!(
                    "Base HNSW dirty flag set but checksums pass — clearing flag (self-heal)"
                );
                if let Err(e) = store.set_hnsw_dirty(cqs::HnswKind::Base, false) {
                    tracing::warn!(error = %e, "Failed to clear dirty flag");
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "Base HNSW index stale (checksum mismatch) — router falls back to enriched"
                );
                return Ok(None);
            }
        }
    }
    Ok(cqs::HnswIndex::try_load_base_with_ef(
        cqs_dir,
        None,
        store.dim(),
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
        store.set_hnsw_dirty(cqs::HnswKind::Base, false).unwrap();

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
        store.set_hnsw_dirty(cqs::HnswKind::Base, false).unwrap();

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
