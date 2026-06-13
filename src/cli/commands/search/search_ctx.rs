//! Surface-agnostic search context for [`super::query::query_core`].
//!
//! ## Why a trait (Phase 2b)
//!
//! `query_core` is the single search implementation. Two surfaces drive it:
//! the CLI ([`crate::cli::CommandContext`]) and the daemon
//! ([`crate::cli::batch::BatchView`]). They hold the same resources (store,
//! embedder, reranker, SPLADE encoder/index, vector index, audit state) but in
//! different shapes — `CommandContext` lazily builds a fresh `Box<dyn
//! VectorIndex>` per call while `BatchView` hands out a cached `Arc`; the
//! daemon's SPLADE index must be `ensure`d before borrow, the CLI's loads on
//! first access. [`SearchCtx`] is the lean common surface that erases those
//! differences so the core never branches on its caller.
//!
//! Each accessor returns an owned/`Arc` type rather than a borrow into the
//! concrete context, so the daemon's `Arc<Store>` / `Arc<SpladeIndex>` snapshot
//! pattern (which has no long-lived `&self` borrow to lend) composes the same
//! way the CLI's `&Store` does. The core keeps the returned `Arc`s alive in
//! locals and `as_deref()`s them into the retrieval primitives.
//!
//! ## The multi-store seam
//!
//! The project store is the single store the plain path searches, but the same
//! prepared query (one classification + embedding + filter + SPLADE resolution,
//! built once by [`super::query::prepare_query`]) also drives the `--ref` and
//! `--include-refs` paths. Those fan out over additional read-only stores
//! (loaded references). [`SearchCtx::references`] is the seam: it hands the
//! prepared-query consumer the reference stores to fan out over, returning
//! `Arc<ReferenceIndex>` on both surfaces (the daemon caches them in an LRU; the
//! CLI loads-then-wraps per call). The fan-out itself stays in
//! [`super::query`], consuming the prepared query — so there is exactly one
//! query-preparation path and a single seam where the multi-store retrieval
//! slots in.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use cqs::index::VectorIndex;
use cqs::reference::ReferenceIndex;
use cqs::splade::index::SpladeIndex;
use cqs::splade::SparseVector;
use cqs::store::{ReadOnly, Store};
use cqs::{Embedder, Reranker};

/// A SPLADE index handle that derefs to `&SpladeIndex` regardless of whether
/// the surface owns it behind an `Arc` (daemon snapshot) or lends a borrow out
/// of an in-process cache (CLI `OnceLock`).
///
/// `SpladeIndex` is deliberately not `Clone` (the inverted postings map is
/// large and build-once / read-many), so the CLI can't cheaply hand back an
/// `Arc`. This enum keeps the borrow zero-copy on both surfaces: the core asks
/// for the handle, `Deref`s it into the `&SpladeIndex` the retrieval primitive
/// wants, and the handle (and thus the borrow / `Arc`) stays alive across the
/// search call.
pub(crate) enum SpladeIndexRef<'a> {
    /// CLI: a borrow out of the `CommandContext` `OnceLock` cache.
    Borrowed(&'a SpladeIndex),
    /// Daemon: an `Arc` snapshot taken at view checkout.
    Owned(Arc<SpladeIndex>),
}

impl std::ops::Deref for SpladeIndexRef<'_> {
    type Target = SpladeIndex;
    fn deref(&self) -> &SpladeIndex {
        match self {
            SpladeIndexRef::Borrowed(idx) => idx,
            SpladeIndexRef::Owned(arc) => arc.as_ref(),
        }
    }
}

/// The exact resource surface [`super::query::query_core`] needs, independent
/// of whether it was invoked from the CLI or the daemon.
///
/// Accessors mirror [`crate::cli::CommandContext`] / `BatchView` names. The
/// vector-index accessor returns `Arc` (not a borrow) so the daemon's snapshot
/// model composes; the CLI implementation wraps its freshly-built `Box` into an
/// `Arc` to match. The SPLADE index is lent via [`SpladeIndexRef`] so neither
/// surface pays a clone.
pub(crate) trait SearchCtx {
    /// The read-only store the query runs against.
    fn store(&self) -> &Store<ReadOnly>;

    /// Slot dir holding `index.db`, `hnsw_*`, `splade.*` — the "where do my
    /// index files live" anchor for vector-index and SPLADE loads.
    fn cqs_dir(&self) -> &Path;

    /// Project root — used for parent-context source-file resolution.
    fn root(&self) -> &Path;

    /// Dense query embedder (lazy ONNX init on first call).
    fn embedder(&self) -> Result<&Embedder>;

    /// Cross-encoder reranker (lazy ONNX init).
    fn reranker(&self) -> Result<Arc<dyn Reranker>>;

    /// Encode the query into a SPLADE sparse vector, or `None` when no SPLADE
    /// model is available / encoding failed. Encapsulates the
    /// `splade_encoder().encode(query)` two-step (and the daemon's
    /// `ensure_splade_index` priming) so the core asks for the encoded vector,
    /// not the encoder.
    fn splade_encode(&self, query: &str) -> Option<SparseVector>;

    /// The SPLADE inverted index, primed if necessary. `None` when the store
    /// holds no sparse vectors or the index couldn't be read. Lent via
    /// [`SpladeIndexRef`] so the daemon's `Arc` snapshot and the CLI's cached
    /// borrow share one signature without a clone.
    fn splade_index(&self) -> Option<SpladeIndexRef<'_>>;

    /// Enriched vector index (CAGRA/HNSW/brute-force).
    fn vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>>;

    /// Base (non-enriched) vector index for adaptive routing's `DenseBase`
    /// strategy. `None` when the base index files are absent / disabled.
    fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>>;

    /// Current audit-mode state (forces the hybrid retrieval path when active).
    fn audit_state(&self) -> cqs::audit::AuditMode;

    /// The loaded reference stores the `--include-refs` fan-out searches
    /// alongside the project store. Empty when no references are configured.
    ///
    /// Returns `Arc<ReferenceIndex>` so the daemon's LRU-cached references and
    /// the CLI's per-call loads share one seam signature. The CLI wraps its
    /// freshly-loaded owned `ReferenceIndex` values into `Arc`; the daemon hands
    /// back its cached `Arc`s directly. Only the multi-store path calls this —
    /// the plain single-store path never loads references.
    fn references(&self) -> Result<Vec<Arc<ReferenceIndex>>>;

    /// Resolve a single named reference (the `--ref`-scoped path). Separate
    /// from [`references`](Self::references) because `--ref` searches exactly
    /// one named store with no project fan-out, and each surface resolves the
    /// name through its own path (the CLI re-reads config, the daemon hits its
    /// LRU).
    fn reference_by_name(&self, name: &str) -> Result<Arc<ReferenceIndex>>;

    /// The worktree search overlay to shadow the project store with, if any
    /// (result-trust §3). `Some` only when the surface both supports building
    /// an overlay AND the caller requested one for an eligible worktree.
    ///
    /// Default `None`: the plain single-store path never overlays, and the CLI
    /// surface returns `None` in phase 1 (overlays build only on the daemon
    /// path, which resolves+caches them per worktree root — see PR-3). The
    /// eligibility detection + CLI-direct degradation warn live in the
    /// `cmd_query` adapter, not here. `query.rs::apply_overlay` consumes this:
    /// it masks project hits whose origin is in the overlay's delta and merges
    /// the overlay store's hits in their place.
    fn overlay(&self) -> Option<Arc<cqs::worktree_overlay::WorktreeOverlay>> {
        None
    }
}

// ─── CLI adapter ────────────────────────────────────────────────────────────

impl SearchCtx for crate::cli::CommandContext<'_, ReadOnly> {
    fn store(&self) -> &Store<ReadOnly> {
        &self.store
    }

    fn cqs_dir(&self) -> &Path {
        &self.cqs_dir
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn embedder(&self) -> Result<&Embedder> {
        crate::cli::CommandContext::embedder(self)
    }

    fn reranker(&self) -> Result<Arc<dyn Reranker>> {
        crate::cli::CommandContext::reranker(self)
    }

    fn splade_encode(&self, query: &str) -> Option<SparseVector> {
        self.splade_encoder().and_then(|enc| match enc.encode(query) {
            Ok(sv) => Some(sv),
            Err(e) => {
                tracing::warn!(error = %e, "SPLADE query encoding failed, falling back to cosine-only");
                None
            }
        })
    }

    fn splade_index(&self) -> Option<SpladeIndexRef<'_>> {
        crate::cli::CommandContext::splade_index(self).map(SpladeIndexRef::Borrowed)
    }

    fn vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        let boxed = crate::cli::build_vector_index(&self.store, &self.cqs_dir)?;
        Ok(boxed.map(|b| -> Arc<dyn VectorIndex> { b.into() }))
    }

    fn base_vector_index(&self) -> Result<Option<Arc<dyn VectorIndex>>> {
        let boxed = crate::cli::build_base_vector_index(&self.store, &self.cqs_dir)?;
        Ok(boxed.map(|b| -> Arc<dyn VectorIndex> { b.into() }))
    }

    fn audit_state(&self) -> cqs::audit::AuditMode {
        // Audit-mode is project-scoped: the writer (`cmd_audit_mode`) and the
        // daemon both resolve `audit-mode.json` from the project `.cqs/`, not
        // the slot dir. Read it from `project_cqs_dir` so CLI-direct search
        // suppresses note-boost and forces the hybrid path identically to the
        // daemon. Reading `cqs_dir` (the slot dir) misses the file entirely on
        // slot-migrated projects, leaving audit-mode silently inert here.
        cqs::audit::load_audit_state(&self.project_cqs_dir)
    }

    fn references(&self) -> Result<Vec<Arc<ReferenceIndex>>> {
        // The CLI loads references fresh from config per call (no long-lived
        // cache on this surface); wrap each owned `ReferenceIndex` into an `Arc`
        // so the seam signature matches the daemon's LRU `Arc`s.
        let config = cqs::config::Config::load(&self.root);
        Ok(cqs::reference::load_references(&config.references)
            .into_iter()
            .map(Arc::new)
            .collect())
    }

    fn reference_by_name(&self, name: &str) -> Result<Arc<ReferenceIndex>> {
        crate::cli::commands::resolve::find_reference(&self.root, name).map(Arc::new)
    }
}

#[cfg(test)]
mod tests {
    use super::SearchCtx;
    use clap::Parser;
    use cqs::store::{ModelInfo, Store};

    /// Integration pin for the project-vs-slot audit-mode split.
    ///
    /// Builds a slot-migrated layout on disk — index.db under
    /// `.cqs/slots/work/`, `audit-mode.json` at the PROJECT level (`.cqs/`) —
    /// then constructs a real `CommandContext` whose slot `cqs_dir` is the slot
    /// dir and whose `project_cqs_dir` is the project `.cqs/`. The CLI-direct
    /// `audit_state()` must resolve the project file (active), matching the
    /// daemon. The earlier unit pins constructed bespoke `SearchCtx` mocks and
    /// missed this because they never exercised the slot/project dir split that
    /// a real `CommandContext` carries.
    #[test]
    fn cli_audit_state_resolves_from_project_dir_not_slot() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let project_cqs_dir = root.join(".cqs");
        let slot_dir = project_cqs_dir.join("slots").join("work");
        std::fs::create_dir_all(&slot_dir).unwrap();

        // Real store under the slot dir.
        let db = slot_dir.join(cqs::INDEX_DB_FILENAME);
        {
            let s = Store::open(&db).unwrap();
            s.init(&ModelInfo::default()).unwrap();
        }
        let store = Store::open_readonly(&db).unwrap();

        // Audit-mode ON, written at the PROJECT level (where `cmd_audit_mode`
        // and the daemon both put it) — NOT in the slot dir.
        let mode = cqs::audit::AuditMode {
            enabled: true,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::minutes(30)),
        };
        cqs::audit::save_audit_state(&project_cqs_dir, &mode).unwrap();

        // Sanity: the file lives at the project level, not the slot dir.
        assert!(project_cqs_dir.join("audit-mode.json").exists());
        assert!(!slot_dir.join("audit-mode.json").exists());
        // Negative control: reading the slot dir alone (the old, buggy anchor)
        // misses the file and reports inactive.
        assert!(
            !cqs::audit::load_audit_state(&slot_dir).is_active(),
            "slot dir holds no audit-mode.json — the old slot-anchored read would miss it"
        );

        let cli = crate::cli::Cli::try_parse_from(["cqs", "some query"]).unwrap();
        let ctx =
            crate::cli::CommandContext::new_for_test(&cli, store, root, slot_dir, project_cqs_dir);

        assert!(
            ctx.audit_state().is_active(),
            "CLI-direct audit_state() must resolve audit-mode.json from the project \
             .cqs/, matching the daemon — reading the slot dir would report inactive"
        );
    }
}
