//! Store opening, command context, and vector index building.
//!
//! Extracted from `mod.rs` to keep the CLI module hub lean.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use cqs::store::ClearHnswDirty;

use super::config::find_project_root;
use super::definitions;

/// Bundle of paths produced by [`resolve_slot_paths`] — slot-local index +
/// project-level metadata in one struct so call sites don't have to re-derive
/// either.
#[derive(Debug, Clone)]
pub(crate) struct SlotPaths {
    /// Project root (`<root>` — directory holding `.cqs/`).
    pub root: PathBuf,
    /// Project-level `.cqs/` (telemetry, daemon socket, embeddings_cache.db,
    /// active_slot pointer, slots/).
    pub project_cqs_dir: PathBuf,
    /// Slot dir `.cqs/slots/<name>/` (holds index.db, hnsw_*, splade.*).
    pub slot_dir: PathBuf,
    /// Slot name (validated, post-resolution).
    pub slot_name: String,
}

impl SlotPaths {
    pub fn index_path(&self) -> PathBuf {
        self.slot_dir.join(cqs::INDEX_DB_FILENAME)
    }
}

/// Resolve `--slot` / `CQS_SLOT` / `.cqs/active_slot` / "default" into the
/// concrete slot dir, falling back to a legacy `.cqs/index.db` layout when
/// `slots/` doesn't yet exist (pre-migration / never-indexed projects).
pub(crate) fn resolve_slot_paths(slot_flag: Option<&str>) -> Result<SlotPaths> {
    let _span = tracing::debug_span!("resolve_slot_paths", slot_flag).entered();
    let root = find_project_root();
    let project_cqs_dir = cqs::resolve_index_dir(&root);

    // Pre-slots layout (legacy): `.cqs/index.db` directly under project_cqs_dir.
    // Path is returned as a fake slot of name "default" so downstream code
    // that joins INDEX_DB_FILENAME against `slot_dir` finds the existing file.
    let resolved = cqs::slot::resolve_slot_name(slot_flag, &project_cqs_dir)
        .map_err(|e| anyhow::anyhow!(e))?;
    let slot_dir = cqs::resolve_slot_dir(&project_cqs_dir, &resolved.name);
    let slots_root = cqs::slot::slots_root(&project_cqs_dir);

    // If neither the slot dir nor the legacy `.cqs/index.db` is present, we
    // still return the slot_dir form so `open_project_store` can produce a
    // clean "Index not found" error pointing at the modern path.
    if !slots_root.exists() && project_cqs_dir.join(cqs::INDEX_DB_FILENAME).exists() {
        // Pre-slots layout — index.db sits directly in `.cqs/`. Treat
        // `.cqs/` as the slot dir for this read.
        return Ok(SlotPaths {
            root,
            project_cqs_dir: project_cqs_dir.clone(),
            slot_dir: project_cqs_dir,
            slot_name: cqs::slot::DEFAULT_SLOT.to_string(),
        });
    }
    Ok(SlotPaths {
        root,
        project_cqs_dir,
        slot_dir,
        slot_name: resolved.name,
    })
}

/// Shared helper: locate project root and index, open store with the given opener.
///
/// Generic over the typestate returned by `opener`, so both `Store::open`
/// (→ `Store<ReadWrite>`) and `Store::open_readonly_pooled`
/// (→ `Store<ReadOnly>`) compose through the same helper.
fn open_store_with<Mode>(
    opener: fn(&Path) -> std::result::Result<cqs::Store<Mode>, cqs::store::StoreError>,
    slot_flag: Option<&str>,
) -> Result<(cqs::Store<Mode>, SlotPaths)> {
    // P3 #131: span on the shared opener so both `open_project_store` and
    // `open_project_store_readonly` (which fan into here) get consistent
    // tracing identity covering the index existence check + open.
    let _span = tracing::info_span!("open_project_store").entered();
    let paths = resolve_slot_paths(slot_flag)?;
    let index_path = paths.index_path();

    if !index_path.exists() {
        anyhow::bail!(
            "Index not found at {}. Run `cqs init && cqs index` (or `cqs index --slot {}` if the slot exists but is empty).",
            index_path.display(),
            paths.slot_name,
        );
    }

    let store = opener(&index_path)
        .map_err(|e| anyhow::anyhow!("Failed to open index at {}: {}", index_path.display(), e))?;
    Ok((store, paths))
}

/// Open the project store, returning the store, project root, and slot dir.
/// Bails with a user-friendly message if no index exists.
///
/// Kept for legacy in-tree callers that don't (yet) flow through
/// `CommandContext`. New code should prefer
/// [`open_project_store_for_slot`] which honors the `--slot` flag.
#[allow(dead_code)]
pub(crate) fn open_project_store() -> Result<(cqs::Store, PathBuf, PathBuf)> {
    let (store, paths) = open_store_with(cqs::Store::open, None)?;
    Ok((store, paths.root, paths.slot_dir))
}

/// Slot-aware variant of [`open_project_store`]. Honors the resolved slot flag
/// from CLI / env / file.
pub(crate) fn open_project_store_for_slot(
    slot_flag: Option<&str>,
) -> Result<(cqs::Store, SlotPaths)> {
    open_store_with(cqs::Store::open, slot_flag)
}

/// Open the project store with a single-threaded runtime for read-only commands.
/// Same as [`open_project_store`] but uses `Store::open_readonly_pooled()` which creates a
/// `current_thread` tokio runtime (1 OS thread) instead of `multi_thread` (4 OS threads).
/// Keeps full 256MB mmap and 16MB cache for search performance.
pub(crate) fn open_project_store_readonly(
) -> Result<(cqs::Store<cqs::store::ReadOnly>, PathBuf, PathBuf)> {
    let (store, paths) = open_store_with(cqs::Store::open_readonly_pooled, None)?;
    Ok((store, paths.root, paths.slot_dir))
}

/// Slot-aware read-only open. Honors `--slot`/env/file resolution for query
/// commands flowing through [`CommandContext`].
pub(crate) fn open_project_store_readonly_for_slot(
    slot_flag: Option<&str>,
) -> Result<(cqs::Store<cqs::store::ReadOnly>, SlotPaths)> {
    open_store_with(cqs::Store::open_readonly_pooled, slot_flag)
}

/// Shared context for CLI commands that need an open store.
/// Created once in dispatch, passed to all store-using handlers.
/// Eliminates per-handler `open_project_store_readonly()` calls.
///
/// The `Mode` type parameter records whether the store was opened read-only
/// or read-write. Commands that only read (search, explain, etc.) take
/// `&CommandContext<'_, ReadOnly>`; commands that mutate (gc, suggest
/// --apply, notes add) take `&CommandContext<'_, ReadWrite>`. This makes
/// GitHub #946 structurally impossible: a read-only command cannot
/// accidentally call a write method at compile time.
///
/// `Mode` defaults to `ReadWrite` so pre-typestate call sites keep
/// compiling. New code that only needs reads should prefer
/// `CommandContext<'_, ReadOnly>`.
pub(crate) struct CommandContext<'a, Mode = cqs::store::ReadWrite> {
    pub cli: &'a definitions::Cli,
    pub store: cqs::Store<Mode>,
    pub root: PathBuf,
    /// Slot dir — `.cqs/slots/<active>/`. Holds index.db, hnsw_*, splade.*.
    /// Most call sites use this as the "where do my index files live" anchor.
    pub cqs_dir: PathBuf,
    /// Project-level `.cqs/` dir (parent of `slots/`). Holds the
    /// embeddings_cache.db, the active_slot pointer, telemetry, daemon
    /// socket. Pre-slots projects have `cqs_dir == project_cqs_dir`.
    ///
    /// Surfaced as `pub` for handlers that need to open the project-scoped
    /// embeddings cache, write the active_slot pointer, or interact with
    /// the daemon socket — all project-level concerns rather than
    /// slot-local ones.
    #[allow(dead_code)] // wired into doctor + handlers progressively; spec §Architecture
    pub project_cqs_dir: PathBuf,
    /// Slot name resolved from `--slot` / `CQS_SLOT` / `.cqs/active_slot` /
    /// fallback "default". Available so handlers can include the slot in
    /// their tracing fields and `--json` envelopes without re-resolving.
    #[allow(dead_code)] // wired into doctor + handlers progressively
    pub slot_name: String,
    reranker: OnceLock<cqs::Reranker>,
    embedder: OnceLock<cqs::Embedder>,
    splade_encoder: OnceLock<Option<cqs::splade::SpladeEncoder>>,
    splade_index: OnceLock<Option<cqs::splade::index::SpladeIndex>>,
    /// Index-aware ModelConfig override: if `Store::stored_model_name()` is
    /// a known preset, that wins over CLI/env/config. Computed lazily on
    /// first `model_config()` call. See
    /// [`cqs::embedder::ModelConfig::resolve_for_query`].
    index_aware_model: OnceLock<cqs::embedder::ModelConfig>,
}

impl<'a> CommandContext<'a, cqs::store::ReadOnly> {
    /// Open the project store in read-only mode and build a command context.
    pub fn open_readonly(cli: &'a definitions::Cli) -> Result<Self> {
        let (store, paths) = open_project_store_readonly_for_slot(cli.slot.as_deref())?;
        Ok(Self {
            cli,
            store,
            root: paths.root,
            cqs_dir: paths.slot_dir,
            project_cqs_dir: paths.project_cqs_dir,
            slot_name: paths.slot_name,
            reranker: OnceLock::new(),
            embedder: OnceLock::new(),
            splade_encoder: OnceLock::new(),
            splade_index: OnceLock::new(),
            index_aware_model: OnceLock::new(),
        })
    }
}

impl<'a> CommandContext<'a, cqs::store::ReadWrite> {
    /// Open the project store in read-write mode and build a command context.
    ///
    /// Used by write commands (gc, etc.) that need the lazy embedder/reranker
    /// from `CommandContext` but also need a writable store.
    pub fn open_readwrite(cli: &'a definitions::Cli) -> Result<Self> {
        let _span = tracing::info_span!("CommandContext::open_readwrite").entered();
        let (store, paths) = open_project_store_for_slot(cli.slot.as_deref())?;
        Ok(Self {
            cli,
            store,
            root: paths.root,
            cqs_dir: paths.slot_dir,
            project_cqs_dir: paths.project_cqs_dir,
            slot_name: paths.slot_name,
            reranker: OnceLock::new(),
            embedder: OnceLock::new(),
            splade_encoder: OnceLock::new(),
            splade_index: OnceLock::new(),
            index_aware_model: OnceLock::new(),
        })
    }
}

impl<'a, Mode> CommandContext<'a, Mode> {
    /// Get the resolved model config, preferring the model recorded in the
    /// open store's metadata over CLI flag / env var / config / default.
    ///
    /// Index-time callers (`cqs index --force`) MUST NOT use this — they
    /// should consult [`cqs::embedder::ModelConfig::resolve`] directly via
    /// `cli.try_model_config()`. At index time the user's intent is to
    /// install a new embedder; honouring the stored name would refuse to
    /// switch models.
    ///
    /// Query-time commands (search, scout, gather, impact, ...) flow through
    /// `CommandContext` and pick up the index-aware override here. This
    /// closes the long-standing footgun where `CQS_EMBEDDING_MODEL=foo` set
    /// in a shell would silently search a `bar`-model index with a wrong-dim
    /// embedder and return zero results.
    pub fn model_config(&self) -> &cqs::embedder::ModelConfig {
        self.index_aware_model.get_or_init(|| {
            let stored = self.store.stored_model_name();
            // Build a fresh resolution chain matching dispatch.rs logic.
            // We re-load config because we don't carry it on the Cli.
            let config = cqs::config::Config::load(&self.root);
            let resolved = cqs::embedder::ModelConfig::resolve_for_query(
                stored.as_deref(),
                self.cli.model.as_deref(),
                config.embedding.as_ref(),
            )
            .apply_env_overrides();
            tracing::debug!(
                stored_model = stored.as_deref().unwrap_or("<none>"),
                resolved_model = %resolved.name,
                resolved_dim = resolved.dim,
                "CommandContext resolved index-aware model config"
            );
            resolved
        })
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
        // P1.7: thread the `[reranker]` config section so .cqs.toml preset/
        // model_path is honoured instead of silently defaulting to ms-marco.
        let config = cqs::config::Config::load(&self.root);
        let r = cqs::Reranker::with_section(config.reranker.clone())
            .map_err(|e| anyhow::anyhow!("Reranker init failed: {e}"))?;
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
pub(crate) fn build_vector_index<Mode: ClearHnswDirty>(
    store: &cqs::Store<Mode>,
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
///
/// Generic over the store's typestate. The self-heal write (clearing the
/// `hnsw_dirty` flag after a successful checksum verify) is gated to
/// `Store<ReadWrite>` via [`cqs::store::ClearHnswDirty::try_clear_hnsw_dirty`];
/// a daemon with a `Store<ReadOnly>` will still observe the verify result
/// but cannot persist the cleared flag. That's intentional — the daemon
/// never mutates the DB, and the next writable open (`cqs index`,
/// `cqs gc`) re-runs this path and performs the clear.
///
/// Backend selection is delegated to [`cqs::index::backends`]: each
/// backend declares its own priority and runs its own open path
/// (CAGRA gates on GPU + threshold + persistence; HNSW handles dirty-flag
/// self-heal). The first backend whose `try_open` returns `Some` wins.
pub(crate) fn build_vector_index_with_config<Mode: ClearHnswDirty>(
    store: &cqs::Store<Mode>,
    cqs_dir: &Path,
    ef_search: Option<usize>,
) -> Result<Option<Box<dyn cqs::index::VectorIndex>>> {
    let _span = tracing::info_span!("build_vector_index_with_config").entered();
    let ctx = cqs::index::BackendContext {
        cqs_dir,
        store,
        ef_search,
    };
    for backend in cqs::index::backends::<Mode>() {
        if let Some(idx) = backend.try_open(&ctx)? {
            return Ok(Some(idx));
        }
    }
    Ok(None)
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
pub(crate) fn build_base_vector_index<Mode: ClearHnswDirty>(
    store: &cqs::Store<Mode>,
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
                Mode::try_clear_hnsw_dirty(store, cqs::HnswKind::Base);
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
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
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
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
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

    /// Issue #971: after a successful checksum verify, the self-heal path
    /// must clear `hnsw_dirty` so the next run skips the expensive verify
    /// step. This pins the invariant documented at
    /// `build_base_vector_index` lines ~499-515: dirty flag + checksum OK
    /// → `try_clear_hnsw_dirty` runs and the flag ends up `false`.
    #[test]
    fn test_build_base_vector_index_clears_dirty_after_successful_rebuild() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Belt-and-braces: make sure no prior test left the bypass set —
        // the module-level ENV_LOCK serializes but doesn't reset state.
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..5)
            .map(|i| (format!("v{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        index.save(dir.path(), "index_base").unwrap();

        // Simulate the post-crash state: sidecar files on disk + checksums
        // intact (because we just wrote them) + dirty flag set (as if the
        // process died between the SQLite commit and `set_hnsw_dirty(false)`).
        store.set_hnsw_dirty(cqs::HnswKind::Base, true).unwrap();
        assert!(
            store.is_hnsw_dirty(cqs::HnswKind::Base).unwrap(),
            "precondition: flag must be dirty before the call"
        );

        let loaded = build_base_vector_index(&store, dir.path()).unwrap();
        assert!(
            loaded.is_some(),
            "checksum passes → base index should load rather than fall back to None"
        );

        assert!(
            !store.is_hnsw_dirty(cqs::HnswKind::Base).unwrap(),
            "self-heal must clear the Base dirty flag after successful verify"
        );
    }

    /// Issue #971 (negative half): if the sidecar files are corrupted,
    /// the self-heal must NOT clear the dirty flag and the function must
    /// return `Ok(None)` so the router falls back to enriched. Truncating
    /// `index_base.hnsw.graph` to zero bytes gives us a deterministic
    /// checksum mismatch without needing to hand-craft the blake3 output.
    #[test]
    fn test_build_base_vector_index_keeps_dirty_on_checksum_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..5)
            .map(|i| (format!("v{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        index.save(dir.path(), "index_base").unwrap();

        // Corrupt one of the checksummed files — truncating to zero bytes
        // is enough to flip the blake3 result and flunk verify_hnsw_checksums.
        let graph_path = dir.path().join("index_base.hnsw.graph");
        assert!(
            graph_path.exists(),
            "fixture invariant: .hnsw.graph must exist after save() so we can corrupt it"
        );
        std::fs::File::create(&graph_path)
            .unwrap()
            .set_len(0)
            .unwrap();

        store.set_hnsw_dirty(cqs::HnswKind::Base, true).unwrap();

        let result = build_base_vector_index(&store, dir.path()).unwrap();
        assert!(
            result.is_none(),
            "checksum mismatch → build_base_vector_index must return Ok(None) \
             (caller falls back to enriched index)"
        );

        assert!(
            store.is_hnsw_dirty(cqs::HnswKind::Base).unwrap(),
            "Base dirty flag must remain set when checksums fail — clearing it \
             would silently mask a genuine staleness condition on the next run"
        );
    }

    /// Issue #971 (enriched mirror): the enriched HNSW path lives in
    /// `build_vector_index_with_config` — there is no separate
    /// `build_enriched_vector_index` function. Same self-heal contract
    /// applies: dirty flag + verify OK → clear flag + return the index.
    ///
    /// NOTE: with the `gpu-index` feature the function first checks the
    /// CAGRA threshold via `store.chunk_count()`. We seed no chunks, so
    /// `chunk_count = 0 < 5000` and CAGRA is skipped, guaranteeing we
    /// hit the HNSW self-heal branch we care about.
    #[test]
    fn test_build_enriched_vector_index_clears_dirty_after_successful_rebuild() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..5)
            .map(|i| (format!("v{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        // Enriched basename is "index" (see `try_load_with_ef` /
        // `verify_hnsw_checksums(cqs_dir, "index")`).
        index.save(dir.path(), "index").unwrap();

        store.set_hnsw_dirty(cqs::HnswKind::Enriched, true).unwrap();
        assert!(
            store.is_hnsw_dirty(cqs::HnswKind::Enriched).unwrap(),
            "precondition: Enriched flag must be dirty before the call"
        );

        let loaded = build_vector_index_with_config(&store, dir.path(), None).unwrap();
        assert!(
            loaded.is_some(),
            "checksum passes → enriched index should load rather than fall back to None"
        );

        assert!(
            !store.is_hnsw_dirty(cqs::HnswKind::Enriched).unwrap(),
            "self-heal must clear the Enriched dirty flag after successful verify"
        );
    }

    /// Issue #971 (enriched mirror, negative half): a corrupted enriched
    /// sidecar file must NOT clear the dirty flag. The function must
    /// return `Ok(None)` so the search path falls back to brute-force.
    #[test]
    fn test_build_enriched_vector_index_keeps_dirty_on_checksum_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_DISABLE_BASE_INDEX");

        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        let dim = store.dim();
        let embeddings: Vec<(String, cqs::embedder::Embedding)> = (0..5)
            .map(|i| (format!("v{i}"), make_embedding(i as f32 + 0.1, dim)))
            .collect();
        let index = cqs::HnswIndex::build_with_dim(embeddings, dim).unwrap();
        index.save(dir.path(), "index").unwrap();

        // Truncate the enriched graph file — enriched basename is "index".
        let graph_path = dir.path().join("index.hnsw.graph");
        assert!(
            graph_path.exists(),
            "fixture invariant: index.hnsw.graph must exist after save() so we can corrupt it"
        );
        std::fs::File::create(&graph_path)
            .unwrap()
            .set_len(0)
            .unwrap();

        store.set_hnsw_dirty(cqs::HnswKind::Enriched, true).unwrap();

        let result = build_vector_index_with_config(&store, dir.path(), None).unwrap();
        assert!(
            result.is_none(),
            "checksum mismatch → build_vector_index_with_config must return Ok(None) \
             (caller falls back to brute-force search)"
        );

        assert!(
            store.is_hnsw_dirty(cqs::HnswKind::Enriched).unwrap(),
            "Enriched dirty flag must remain set when checksums fail — clearing it \
             would silently mask a genuine staleness condition on the next run"
        );
    }
}
