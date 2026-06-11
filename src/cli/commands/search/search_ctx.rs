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
        cqs::audit::load_audit_state(&self.cqs_dir)
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
