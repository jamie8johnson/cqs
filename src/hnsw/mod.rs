//! HNSW (Hierarchical Navigable Small World) index for fast vector search
//!
//! Provides O(log n) approximate nearest neighbor search, scaling to >50k chunks.
//!
//! ## Security
//!
//! The underlying hnsw_rs library uses bincode for serialization, which is
//! unmaintained (RUSTSEC-2025-0141). To mitigate deserialization risks, we
//! compute and verify blake3 checksums on save/load.
//!
//! ## Memory Management
//!
//! When loading an index from disk, hnsw_rs returns `Hnsw<'a>` borrowing from
//! `HnswIo`. We use `self_cell` to safely manage this self-referential pattern:
//! - HnswIo is wrapped in `HnswIoCell` (UnsafeCell for one-time &mut access)
//! - self_cell ties the lifetimes together and ensures correct drop order
//! - No transmute or raw pointers needed
//!
//! ## hnsw_rs Version Dependency
//!
//! If upgrading hnsw_rs: Run `cargo test safety_tests` and verify behavior.
//! The `HnswIo::load_hnsw()` API must still return `Hnsw<'a>` borrowing from
//! `&'a mut HnswIo`. Current tested version: hnsw_rs 0.3.x

mod build;
mod persist;
mod safety;
mod search;

pub use persist::{
    verify_hnsw_checksums, verify_hnsw_current, SaveOutcome, StoreStamp, HNSW_ALL_EXTENSIONS,
};

use std::cell::UnsafeCell;

use hnsw_rs::anndists::dist::distances::{DistCosine, DistDot};
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::{Hnsw, Neighbour};
use hnsw_rs::hnswio::HnswIo;
use self_cell::self_cell;
use thiserror::Error;

use crate::embedder::Embedding;
use crate::index::{DistanceMetric, IndexResult, VectorIndex};

// HNSW tuning parameters
//
// Defaults are corpus-size-aware: small projects pay less build cost, large
// monorepos get the recall headroom they need at search time.
//
// | corpus       | M  | ef_construction | ef_search |
// |--------------|----|-----------------|-----------|
// | < 5k         | 16 |             100 |        50 |
// | 5k–100k      | 24 |             200 |       100 |
// | ≥ 100k       | 32 |             400 |       200 |
//
// Env vars (`CQS_HNSW_M`, `CQS_HNSW_EF_CONSTRUCTION`, `CQS_HNSW_EF_SEARCH`)
// win — set explicitly, the override is taken verbatim. The
// `chunk_count`-aware path is used by the build (`hnsw/build.rs`) where
// the corpus size is in scope; the zero-arg `max_nb_connection`
// / `ef_construction` / `ef_search` helpers serve callers that don't
// know the corpus size yet (CLI knob parsing, ref-only paths) and use
// the middle tier (`MID_*`) as their default.
//
// CAGRA already scales analogously via `cagra_itopk_max_default(n_vectors)`
// (`src/cagra.rs`).

pub(crate) const MAX_LAYER: usize = 16; // Maximum layers in the graph

/// Mid-tier default M. Used when the no-corpus-size path is the only
/// available context.
const MID_M: usize = 24;
/// Mid-tier default ef_construction.
const MID_EF_CONSTRUCTION: usize = 200;
/// Mid-tier default ef_search.
const MID_EF_SEARCH: usize = 100;

/// Alias for tests + any caller that pins exact values. Prefer
/// [`hnsw_tier_defaults`] with a corpus size when one is available.
const DEFAULT_M: usize = MID_M;
const DEFAULT_EF_CONSTRUCTION: usize = MID_EF_CONSTRUCTION;
const DEFAULT_EF_SEARCH: usize = MID_EF_SEARCH;

/// Pick `(M, ef_construction, ef_search)` for the given corpus size. Pure
/// function — no env reads, no I/O. Callers layer env overrides on top via
/// [`max_nb_connection_for`] / [`ef_construction_for`] / [`ef_search_for`].
pub(crate) fn hnsw_tier_defaults(chunk_count: usize) -> (usize, usize, usize) {
    if chunk_count < 5_000 {
        (16, 100, 50)
    } else if chunk_count < 100_000 {
        (MID_M, MID_EF_CONSTRUCTION, MID_EF_SEARCH)
    } else {
        (32, 400, 200)
    }
}

/// `M` for `chunk_count`. Env `CQS_HNSW_M` wins.
pub(crate) fn max_nb_connection_for(chunk_count: usize) -> usize {
    let (m, _, _) = hnsw_tier_defaults(chunk_count);
    let resolved = parse_hnsw_env_knob("CQS_HNSW_M", m);
    if std::env::var_os("CQS_HNSW_M").is_some() && resolved != m {
        tracing::info!(
            chunk_count,
            tier_default = m,
            override_value = resolved,
            "CQS_HNSW_M override active"
        );
    }
    resolved
}

/// `ef_construction` for `chunk_count`. Env `CQS_HNSW_EF_CONSTRUCTION` wins.
pub(crate) fn ef_construction_for(chunk_count: usize) -> usize {
    let (_, ef, _) = hnsw_tier_defaults(chunk_count);
    let resolved = parse_hnsw_env_knob("CQS_HNSW_EF_CONSTRUCTION", ef);
    if std::env::var_os("CQS_HNSW_EF_CONSTRUCTION").is_some() && resolved != ef {
        tracing::info!(
            chunk_count,
            tier_default = ef,
            override_value = resolved,
            "CQS_HNSW_EF_CONSTRUCTION override active"
        );
    }
    resolved
}

/// `ef_search` for `chunk_count`. Env `CQS_HNSW_EF_SEARCH` wins.
pub(crate) fn ef_search_for(chunk_count: usize) -> usize {
    let (_, _, ef) = hnsw_tier_defaults(chunk_count);
    let resolved = parse_hnsw_env_knob("CQS_HNSW_EF_SEARCH", ef);
    if std::env::var_os("CQS_HNSW_EF_SEARCH").is_some() && resolved != ef {
        tracing::info!(
            chunk_count,
            tier_default = ef,
            override_value = resolved,
            "CQS_HNSW_EF_SEARCH override active"
        );
    }
    resolved
}

/// Parse an env-var-overridable HNSW knob, validating that the value is `>= 1`.
/// A value of `0` (or unparseable garbage) produces a degenerate graph; warn
/// and fall back to the default.
fn parse_hnsw_env_knob(env_name: &str, default: usize) -> usize {
    match std::env::var(env_name) {
        Ok(raw) => match raw.parse::<usize>() {
            Ok(n) if n >= 1 => n,
            Ok(n) => {
                tracing::warn!(
                    env = env_name,
                    raw = %raw,
                    parsed = n,
                    fallback = default,
                    "HNSW env knob must be >= 1 — using default"
                );
                default
            }
            Err(e) => {
                tracing::warn!(
                    env = env_name,
                    raw = %raw,
                    error = %e,
                    fallback = default,
                    "HNSW env knob not parseable as usize — using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// Zero-arg HNSW knob helpers. Production build sites call the
// corpus-size-aware `*_for(chunk_count)` variants above; these zero-arg
// entry points back the test cohort that exercises env-override + default
// behaviour against the mid-tier static defaults. `pub(crate)` (rather than
// `#[cfg(test)]`) keeps the cohort grep-discoverable for a future test that
// reaches into them.

/// M parameter — connections per node. Override with `CQS_HNSW_M`.
/// Defaults to the mid-tier static `DEFAULT_M`; production code uses
/// [`max_nb_connection_for`] which scales by corpus size.
#[allow(dead_code)]
pub(crate) fn max_nb_connection() -> usize {
    let m = parse_hnsw_env_knob("CQS_HNSW_M", DEFAULT_M);
    if m != DEFAULT_M {
        tracing::info!(m, "CQS_HNSW_M override active");
    }
    m
}

/// Construction-time search width. Override with `CQS_HNSW_EF_CONSTRUCTION`.
/// See [`ef_construction_for`] for the corpus-size-aware variant.
#[allow(dead_code)]
pub(crate) fn ef_construction() -> usize {
    let ef = parse_hnsw_env_knob("CQS_HNSW_EF_CONSTRUCTION", DEFAULT_EF_CONSTRUCTION);
    if ef != DEFAULT_EF_CONSTRUCTION {
        tracing::info!(ef, "CQS_HNSW_EF_CONSTRUCTION override active");
    }
    ef
}

/// Search width for queries (higher = more accurate but slower).
/// Override with `CQS_HNSW_EF_SEARCH`. See [`ef_search_for`] for the
/// corpus-size-aware variant used by the build path.
pub(crate) fn ef_search() -> usize {
    let ef = parse_hnsw_env_knob("CQS_HNSW_EF_SEARCH", DEFAULT_EF_SEARCH);
    if ef != DEFAULT_EF_SEARCH {
        tracing::info!(ef, "CQS_HNSW_EF_SEARCH override active");
    }
    ef
}

#[derive(Error, Debug)]
pub enum HnswError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HNSW index not found at {0}")]
    NotFound(String),
    #[error("Dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },
    #[error("Build error: {0}")]
    Build(String),
    #[error("HNSW error: {0}")]
    Internal(String),
    #[error(
        "Checksum mismatch for {file}: expected {expected}, got {actual}. Index may be corrupted."
    )]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// The index on disk was built with one distance metric, but
    /// `CQS_DISTANCE_METRIC` explicitly requests a different one. The stored
    /// metric always wins for an existing index; a conflicting explicit
    /// request is surfaced as this typed error rather than silently
    /// reinterpreting distances. Mirrors `Store::stored_model_name()`.
    #[error(
        "Distance metric mismatch: index was built with '{stored}' but CQS_DISTANCE_METRIC \
         requests '{requested}'. Unset CQS_DISTANCE_METRIC to use the stored metric, or run \
         'cqs index --force' to rebuild with '{requested}'."
    )]
    MetricMismatch {
        stored: DistanceMetric,
        requested: DistanceMetric,
    },
}

// Note: Uses crate::index::IndexResult instead of a separate HnswResult type
// since they have identical structure (id: String, score: f32)

/// Metric-dispatching wrapper over the concrete `Hnsw<f32, D>` types.
///
/// hnsw_rs bakes the distance into the type parameter (`Hnsw<'a, f32, D>`),
/// and its API doesn't lend itself to `dyn Distance` erasure, so metric
/// polymorphism is an enum over the concrete dist types with forwarding
/// methods. One variant per [`DistanceMetric`] — both must stay
/// buildable AND loadable, since the load path instantiates the variant
/// from the persisted `{basename}.hnsw.meta` header.
///
/// Also the dependent type of the `self_cell!` below, which is why it must
/// carry exactly one lifetime parameter.
pub(crate) enum HnswGraph<'a> {
    /// `DistCosine` — the default metric.
    Cosine(Hnsw<'a, f32, DistCosine>),
    /// `DistDot` (`dist = 1 − a·b`). NOTE: anndists asserts `a·b <= 1`, so
    /// this variant expects unit-norm (or sub-unit-dot) embeddings.
    Dot(Hnsw<'a, f32, DistDot>),
}

/// Dispatch a method body across every [`HnswGraph`] variant. Local macro
/// (not a visitor trait): the body is monomorphized per concrete dist type,
/// which is exactly what hnsw_rs's generic API requires.
macro_rules! with_graph {
    ($graph:expr, $h:ident => $body:expr) => {
        match $graph {
            HnswGraph::Cosine($h) => $body,
            HnswGraph::Dot($h) => $body,
        }
    };
}

impl<'a> HnswGraph<'a> {
    /// Construct an empty graph for `metric` with the given hnsw_rs
    /// parameters (mirrors `Hnsw::new`'s argument order).
    pub(crate) fn new(
        metric: DistanceMetric,
        max_nb_connection: usize,
        nb_elem: usize,
        max_layer: usize,
        ef_construction: usize,
    ) -> Self {
        match metric {
            DistanceMetric::Cosine => HnswGraph::Cosine(Hnsw::new(
                max_nb_connection,
                nb_elem,
                max_layer,
                ef_construction,
                DistCosine,
            )),
            DistanceMetric::DotProduct => HnswGraph::Dot(Hnsw::new(
                max_nb_connection,
                nb_elem,
                max_layer,
                ef_construction,
                DistDot,
            )),
        }
    }

    /// Load a graph from `io`, instantiating the dist type recorded in the
    /// persisted meta header. hnsw_rs independently verifies the distance
    /// name embedded in the `.hnsw.graph` file against the requested type,
    /// so a meta/graph disagreement fails loudly here.
    pub(crate) fn load(io: &'a mut HnswIo, metric: DistanceMetric) -> Result<Self, HnswError> {
        match metric {
            DistanceMetric::Cosine => io.load_hnsw::<f32, DistCosine>().map(HnswGraph::Cosine),
            DistanceMetric::DotProduct => io.load_hnsw::<f32, DistDot>().map(HnswGraph::Dot),
        }
        .map_err(|e| HnswError::Internal(format!("Failed to load HNSW: {}", e)))
    }

    /// The metric this graph was built with.
    pub(crate) fn metric(&self) -> DistanceMetric {
        match self {
            HnswGraph::Cosine(_) => DistanceMetric::Cosine,
            HnswGraph::Dot(_) => DistanceMetric::DotProduct,
        }
    }

    /// Number of points in the graph.
    pub(crate) fn get_nb_point(&self) -> usize {
        with_graph!(self, h => h.get_nb_point())
    }

    /// Dump graph + data files via `AnnT::file_dump`.
    pub(crate) fn file_dump(
        &self,
        dir: &std::path::Path,
        basename: &str,
    ) -> Result<String, String> {
        with_graph!(self, h => h.file_dump(dir, basename).map_err(|e| e.to_string()))
    }

    /// Parallel insert (used by both the build and incremental paths).
    pub(crate) fn parallel_insert_data(&mut self, data: &[(&Vec<f32>, usize)]) {
        with_graph!(self, h => h.parallel_insert_data(data))
    }

    /// Unfiltered k-NN search.
    pub(crate) fn search_neighbours(&self, query: &[f32], k: usize, ef: usize) -> Vec<Neighbour> {
        with_graph!(self, h => h.search_neighbours(query, k, ef))
    }

    /// Traversal-time filtered k-NN search.
    pub(crate) fn search_filter(
        &self,
        query: &[f32],
        k: usize,
        ef: usize,
        filter: Option<&dyn hnsw_rs::filter::FilterT>,
    ) -> Vec<Neighbour> {
        with_graph!(self, h => h.search_filter(query, k, ef, filter))
    }
}

/// Wrapper to allow `&mut HnswIo` access from self_cell's `&Owner` builder.
///
/// # Safety
///
/// `UnsafeCell` is accessed mutably only once, during `self_cell` construction.
/// After construction, the HnswIo data is only accessed immutably through
/// the dependent `Hnsw` (which holds shared references into it).
pub(crate) struct HnswIoCell(pub(crate) UnsafeCell<HnswIo>);

// SAFETY: HnswIoCell contains data buffers and file paths from HnswIo.
// The UnsafeCell is only mutated during construction (single-threaded, exclusive).
// After construction, only the Hnsw reads from it (immutably via shared refs).
unsafe impl Send for HnswIoCell {}
unsafe impl Sync for HnswIoCell {}

self_cell!(
    /// Self-referential wrapper for a loaded HNSW index.
    ///
    /// HnswIo owns the raw data buffers. Hnsw borrows from them.
    /// `self_cell` guarantees correct drop order (Hnsw before HnswIo)
    /// and sound lifetime management without transmute.
    pub(crate) struct LoadedHnsw {
        owner: Box<HnswIoCell>,
        #[not_covariant]
        dependent: HnswGraph,
    }
);

// SAFETY: LoadedHnsw is Send+Sync because:
// - HnswIoCell wraps HnswIo (file paths + data buffers)
// - HnswGraph wraps Hnsw<f32, D> (read-only graph data; the dist types
//   DistCosine/DistDot are stateless unit structs)
// - After construction, only immutable search access occurs
unsafe impl Send for LoadedHnsw {}
unsafe impl Sync for LoadedHnsw {}

/// HNSW index wrapper for semantic code search
///
/// This wraps the hnsw_rs library, handling:
/// - Building indexes from embeddings
/// - Searching for nearest neighbors
/// - Saving/loading to disk
/// - Mapping between internal IDs and chunk IDs
pub struct HnswIndex {
    /// Internal state - either built in memory or loaded from disk
    pub(crate) inner: HnswInner,
    /// Mapping from internal index to chunk ID.
    ///
    /// `Box<str>` saves the 8-byte `cap` field per entry vs `String` without
    /// changing heap layout — ~8 MB at 1M chunks. `Arc<str>` would ADD 16
    /// bytes (ArcInner header) per entry and buy no dedup, since every
    /// chunk_id is unique by construction. An mmap'd id_map alongside the
    /// HNSW graph file is the only path to constant-RAM regardless of corpus
    /// size; that remains a separate item.
    pub(crate) id_map: Vec<Box<str>>,
    /// Configurable search width (defaults to ef_search())
    pub(crate) ef_search: usize,
    /// Embedding dimension of vectors in this index
    pub(crate) dim: usize,
    /// Vestigial. Always `None`.
    ///
    /// Pre-fix this held a shared `flock(2)` for the lifetime of a
    /// disk-loaded index, intended to block concurrent `save()` from
    /// overwriting files while this index was in use. In practice the
    /// lifetime-held shared lock self-deadlocked the daemon's rebuild
    /// thread on its next `save()` (Linux flock's exclusive lock waits
    /// for *all* shared holders, including ones held by the same
    /// process via a different open description). The lock is now
    /// released at the end of `load_with_dim` after the data is in
    /// memory; this field is kept on the struct so existing call sites
    /// (in-memory-built indexes via `build.rs`) continue to compile
    /// without churn, and so a future per-process-aware locking
    /// strategy can repopulate it without re-introducing the field.
    pub(crate) _lock_file: Option<std::fs::File>,
}

/// Internal HNSW state
pub(crate) enum HnswInner {
    /// Built in memory - owns its data with 'static lifetime
    Owned(HnswGraph<'static>),
    /// Loaded from disk - self-referential via self_cell
    Loaded(LoadedHnsw),
}

impl HnswInner {
    /// Access the underlying HNSW graph regardless of variant.
    ///
    /// Uses a closure because `Hnsw` is invariant over its lifetime parameter,
    /// so `self_cell` cannot provide a direct reference accessor. The closure
    /// receives the metric-dispatching [`HnswGraph`] wrapper; call its
    /// forwarding methods rather than matching on variants.
    pub(crate) fn with_hnsw<R>(&self, f: impl FnOnce(&HnswGraph<'_>) -> R) -> R {
        match self {
            HnswInner::Owned(hnsw) => f(hnsw),
            HnswInner::Loaded(loaded) => loaded.with_dependent(|_, hnsw| f(hnsw)),
        }
    }
}

impl HnswIndex {
    /// Override the ef_search parameter (from config)
    pub fn set_ef_search(&mut self, ef: usize) {
        self.ef_search = ef;
    }

    /// The distance metric this index was built (or loaded) with. Single
    /// source of truth is the graph variant itself — there is no separate
    /// field that could drift.
    pub fn metric(&self) -> DistanceMetric {
        self.inner.with_hnsw(|g| g.metric())
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.id_map.len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.id_map.is_empty()
    }

    /// View of the chunk IDs currently indexed, in insertion order. Used by
    /// the `cqs watch` background-rebuild swap path to dedup an external delta
    /// against what the rebuild thread already snapshot-ingested before
    /// replaying.
    ///
    /// Returns the id_map as `&[Box<str>]`. Callers comparing against `&str`
    /// literals must deref both sides (`&**id == "delta_a"`); `Box<str>`
    /// doesn't implement `PartialEq<str>` directly.
    pub fn ids(&self) -> &[Box<str>] {
        &self.id_map
    }

    /// Incrementally insert vectors into an Owned HNSW index.
    ///
    /// Returns the number of items inserted, or an error if called on a Loaded
    /// index (which is immutable — use a full rebuild instead).
    ///
    /// This enables watch mode to add new embeddings without rebuilding the
    /// entire graph from scratch.
    pub fn insert_batch(&mut self, items: &[(String, &[f32])]) -> Result<usize, HnswError> {
        let _span = tracing::info_span!("hnsw_insert_batch", count = items.len()).entered();
        if items.is_empty() {
            return Ok(0);
        }

        // Only works on Owned variant — Loaded is immutable (self_cell)
        let hnsw = match &mut self.inner {
            HnswInner::Owned(h) => h,
            HnswInner::Loaded(_) => {
                return Err(HnswError::Internal(
                    "Cannot incrementally insert into loaded index; rebuild required".into(),
                ));
            }
        };

        // Validate dimensions
        for (id, emb) in items {
            if emb.len() != self.dim {
                return Err(HnswError::DimensionMismatch {
                    expected: self.dim,
                    actual: emb.len(),
                });
            }
            tracing::trace!("Inserting {} into HNSW index", id);
        }

        // Assign sequential IDs starting from current id_map length.
        // Convert &[f32] → Vec<f32> so we can pass &Vec<f32> to hnsw_rs
        // (which expects T: Sized + Send + Sync for parallel insert).
        //
        // Claim the id_map slots BEFORE calling into the hnsw_rs graph.
        // `parallel_insert_data` returns `()` but can panic from the worker
        // pool; if it does, we want the next insert to
        // advance `base_idx` past the potentially-corrupted positions
        // rather than reuse them (hnsw_rs has no dedup — reusing the same
        // `(vec, id)` position is undefined behaviour). Claiming the
        // id_map up-front guarantees `base_idx` monotonically advances
        // even if unwinding aborts the rest of the method — worst case is
        // orphan id_map entries pointing at partially-inserted graph
        // positions, which the SQLite post-filter already tolerates.
        let base_idx = self.id_map.len();
        for (id, _) in items {
            // `Box::from(id.as_str())` clones the bytes once into a tight
            // `Box<str>` (no `cap` field).
            self.id_map.push(Box::from(id.as_str()));
        }

        let owned_vecs: Vec<Vec<f32>> = items.iter().map(|(_, emb)| emb.to_vec()).collect();
        let data_for_insert: Vec<(&Vec<f32>, usize)> = owned_vecs
            .iter()
            .enumerate()
            .map(|(i, v)| (v, base_idx + i))
            .collect();

        hnsw.parallel_insert_data(&data_for_insert);

        tracing::info!(
            inserted = items.len(),
            total = self.id_map.len(),
            "HNSW batch insert complete"
        );
        Ok(items.len())
    }
}

/// Prepare embeddings for vector index construction.
///
/// Validates all dimensions match `expected_dim`, flattens into contiguous f32 buffer,
/// and returns the ID map for index<->chunk_id mapping.
///
/// PERF-39: This uses two passes (validate then build). Merging into one pass would
/// require partial rollback on mid-iteration dimension errors, complicating the code
/// for no real gain — this is only called from test/build paths with small N.
#[allow(clippy::type_complexity)]
pub(crate) fn prepare_index_data(
    embeddings: Vec<(String, crate::Embedding)>,
    expected_dim: usize,
) -> Result<(Vec<Box<str>>, Vec<f32>, usize), HnswError> {
    let n = embeddings.len();
    if n == 0 {
        return Err(HnswError::Build("No embeddings to index".into()));
    }

    // Validate dimensions
    for (id, emb) in &embeddings {
        if emb.len() != expected_dim {
            return Err(HnswError::Build(format!(
                "Embedding dimension mismatch for {}: got {}, expected {}",
                id,
                emb.len(),
                expected_dim
            )));
        }
    }

    // Build ID map and flat data vector
    let mut id_map: Vec<Box<str>> = Vec::with_capacity(n);
    let cap = n
        .checked_mul(expected_dim)
        .ok_or_else(|| HnswError::Build("embedding count * dimension would overflow".into()))?;
    let mut data = Vec::with_capacity(cap);
    for (chunk_id, embedding) in embeddings {
        // `String::into_boxed_str()` is zero-copy — shrinks the existing heap
        // allocation in place and drops the `cap` field. ~8 bytes per entry.
        id_map.push(chunk_id.into_boxed_str());
        data.extend(embedding.into_inner());
    }

    Ok((id_map, data, n))
}

impl VectorIndex for HnswIndex {
    /// Searches the index for the k nearest neighbors to the given query embedding.
    ///
    /// # Arguments
    ///
    /// * `query` - The query embedding to search for
    /// * `k` - The number of nearest neighbors to return
    ///
    /// # Returns
    ///
    /// A vector of `IndexResult` items containing the k nearest neighbors, ordered by similarity (closest first).
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult> {
        self.search(query, k)
    }

    fn search_with_filter(
        &self,
        query: &Embedding,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        self.search_filtered(query, k, filter)
    }

    /// Returns the number of elements in the collection.
    ///
    /// # Returns
    ///
    /// The count of elements currently stored in this collection.
    fn len(&self) -> usize {
        self.len()
    }

    /// Checks whether this collection is empty.
    ///
    /// # Returns
    ///
    /// `true` if the collection contains no elements, `false` otherwise.
    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    /// Returns the name of this index type.
    ///
    /// # Returns
    ///
    /// A static string slice containing the name "HNSW" (Hierarchical Navigable Small World).
    fn name(&self) -> &'static str {
        "HNSW"
    }

    fn dim(&self) -> usize {
        self.dim
    }

    /// HNSW with the `Cosine` metric returns `1 - DistCosine`, which on full
    /// vectors is exactly the cosine similarity the brute-force path
    /// recomputes — so its scores are reusable. The `DotProduct` metric
    /// returns `1 - DistDot = a·b`, a different scale from cosine, so it leaves
    /// the default `false`.
    fn index_scores_are_cosine(&self) -> bool {
        self.metric() == DistanceMetric::Cosine
    }
}

/// Always-available CPU vector index. Priority 0 (lowest). The selector
/// uses HNSW as the unconditional fallback when no GPU backend (CAGRA,
/// future Metal/ROCm/USearch) is eligible.
///
/// Handles the per-kind `hnsw_dirty` self-heal via `verify_hnsw_current`:
/// the dirty flag may only be cleared when the sidecars are intact AND their
/// store-state stamp matches the live store — that combination proves the
/// crash happened *after* the save landed but *before* the flag cleared (a
/// false positive). Checksums alone cannot prove that: the manifest is
/// written by the same save it describes, so a complete previous-generation
/// set always passes — exactly the crash-between-chunk-commit-and-save case
/// the flag exists to catch. On stamp mismatch (or a stamp-less legacy
/// sidecar) we return `None` so the caller falls back to brute-force search
/// until a rebuild.
pub struct HnswBackend;

impl<Mode: crate::store::ClearHnswDirty> crate::index::IndexBackend<Mode> for HnswBackend {
    fn name(&self) -> &'static str {
        "hnsw"
    }

    fn priority(&self) -> i32 {
        0
    }

    fn try_open(
        &self,
        ctx: &crate::index::BackendContext<'_, Mode>,
    ) -> std::result::Result<Option<Box<dyn VectorIndex>>, crate::store::StoreError> {
        let dirty = match ctx.store.is_hnsw_dirty(crate::HnswKind::Enriched) {
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
            match verify_hnsw_current(ctx.cqs_dir, "index", ctx.store) {
                Ok(()) => {
                    tracing::info!(
                        "HNSW dirty flag set but sidecars verify and match the live store \
                         — clearing flag (self-heal)"
                    );
                    Mode::try_clear_hnsw_dirty(ctx.store, crate::HnswKind::Enriched);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "HNSW index stale (failed verification or predates the last chunk \
                         write). Falling back to brute-force search. Run 'cqs index' to rebuild."
                    );
                    return Ok(None);
                }
            }
        }
        Ok(HnswIndex::try_load_with_ef(
            ctx.cqs_dir,
            ctx.ef_search,
            ctx.store.dim(),
        ))
    }
}

/// Shared lock serializing tests that read or mutate the process-global
/// `CQS_HNSW_*` env vars. A single static (rather than one per test module) is
/// required: `max_nb_connection_for` / `ef_construction_for` / `ef_search_for`
/// and `build_with_dim` all read these vars, so an env-override test in one
/// module can otherwise race a tier-default or build test in another.
///
/// Holders: the env-mutating tests (`env_override_tests`,
/// `test_hnsw_for_helpers_pick_tier`) hold it across their set/read/remove
/// sequence; the hardened recall tests hold it via the shared
/// `assert_self_match_reachable` helper (below), which takes it around each
/// `build()` closure, and the lifecycle test in `safety.rs` holds it just
/// around its `build_with_dim` call — so graph params cannot be perturbed
/// mid-build, while search phases stay parallel. Other build sites tolerate a
/// transient env override (graph params change, soundness does not).
///
/// All acquisitions are poison-safe
/// (`.lock().unwrap_or_else(PoisonError::into_inner)`): the lock guards
/// process-global env vars with no data invariant, and a single test failure
/// while holding it must not cascade PoisonError panics into every later
/// HNSW test.
#[cfg(test)]
pub(crate) static HNSW_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard pairing `set_var` with `remove_var` on drop, so an assertion
/// failure mid-test cannot leak a process-global env var into later tests.
/// Use together with [`HNSW_ENV_LOCK`] for any var the HNSW build/load
/// paths read (`CQS_HNSW_*`, `CQS_DISTANCE_METRIC`).
#[cfg(test)]
pub(crate) struct EnvVarGuard(&'static str);

#[cfg(test)]
impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        std::env::set_var(key, value);
        Self(key)
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

/// Shared test helper: create a deterministic normalized embedding from a seed.
/// Uses sin-based values for reproducible but varied vectors.
#[cfg(test)]
pub(crate) fn make_test_embedding(seed: u32) -> Embedding {
    let mut v = vec![0.0f32; crate::EMBEDDING_DIM];
    for (i, val) in v.iter_mut().enumerate() {
        *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut v {
            *val /= norm;
        }
    }
    Embedding::new(v)
}

/// Shared test helper: assert that the exact-match vector `want` is reachable
/// in the top-`k` results of an index produced by `build`, retrying the build
/// to absorb the rare degenerate concurrent-build graph. `parallel_insert_data`
/// under CPU contention yields a self-unreachable node on ~1-2% of builds
/// (measured 52/3000 under 16-core load vs 0/3000 sequential — an hnsw_rs
/// concurrent-build characteristic, filed upstream as hnswlib-rs#32, not a cqs
/// bug); at 8 retries a transient miss is ~2.5e-14 while a systematic recall
/// bug (miss on every build) still fails deterministically.
///
/// The `build` closure returns the searchable index, so save/load roundtrips
/// (and any per-build invariant asserts) live inside the closure and are
/// re-exercised on every retry. Returns the matched result's score so callers
/// can additionally pin the self-match score.
///
/// Each `build()` call runs under `HNSW_ENV_LOCK` so a concurrent env-override
/// test cannot perturb the CQS_HNSW_* graph params mid-build; the search phase
/// runs unlocked. (Only `build_with_dim` reads CQS_HNSW_* vars, so holding the
/// lock across an in-closure save/load roundtrip is harmless.)
#[cfg(test)]
pub(crate) fn assert_self_match_reachable(
    build: impl Fn() -> HnswIndex,
    query: &Embedding,
    want: &str,
    k: usize,
) -> f32 {
    let mut last_ids = Vec::new();
    for _ in 0..8 {
        let index = {
            let _env = HNSW_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            build()
        };
        let results = index.search(query, k);
        assert!(!results.is_empty(), "search returned no results");
        last_ids = results.iter().map(|r| r.id.clone()).collect();
        if let Some(r) = results.iter().find(|r| r.id == want) {
            return r.score;
        }
    }
    panic!("{want:?} unreachable across 8 builds (real recall bug, not noise); last top-{k} = {last_ids:?}");
}

#[cfg(test)]
mod send_sync_tests {
    use super::*;

    /// Asserts at compile time that the generic type `T` implements the `Send` trait.
    ///
    /// This function is a compile-time assertion utility that verifies a type is safe to send across thread boundaries. It produces no runtime code and will fail to compile if `T` does not implement `Send`.
    ///
    /// # Arguments
    ///
    /// * `T` - The type to check for `Send` trait implementation
    ///
    /// # Panics
    ///
    /// This function does not panic. If the type does not implement `Send`, compilation will fail with a trait bound error.
    fn assert_send<T: Send>() {}
    /// Asserts that a type `T` implements the `Sync` trait at compile time.
    ///
    /// This function is used for compile-time verification that a type is thread-safe for sharing across threads. It performs no runtime work and is typically called within tests or compile-time assertions to ensure type safety properties.
    ///
    /// # Arguments
    ///
    /// * `T` - A generic type parameter that must implement the `Sync` trait
    ///
    /// # Compile-time Behavior
    ///
    /// This function will fail to compile if `T` does not implement `Sync`, providing immediate feedback about thread-safety violations.
    fn assert_sync<T: Sync>() {}

    #[test]
    fn test_hnsw_index_is_send_sync() {
        assert_send::<HnswIndex>();
        assert_sync::<HnswIndex>();
    }

    #[test]
    fn test_loaded_hnsw_is_send_sync() {
        assert_send::<LoadedHnsw>();
        assert_sync::<LoadedHnsw>();
    }

    /// Tier table — pure function, no env reads.
    #[test]
    fn test_hnsw_tier_defaults_small_corpus() {
        // Pre-5k tier — tighter graph, faster build.
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(0);
        assert_eq!((m, ef_c, ef_s), (16, 100, 50));
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(4_999);
        assert_eq!((m, ef_c, ef_s), (16, 100, 50));
    }

    #[test]
    fn test_hnsw_tier_defaults_medium_corpus() {
        // 5k–100k mid-tier.
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(5_000);
        assert_eq!((m, ef_c, ef_s), (24, 200, 100));
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(50_000);
        assert_eq!((m, ef_c, ef_s), (24, 200, 100));
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(99_999);
        assert_eq!((m, ef_c, ef_s), (24, 200, 100));
    }

    #[test]
    fn test_hnsw_tier_defaults_large_corpus() {
        // ≥100k — bumps M and ef for monorepo-scale recall.
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(100_000);
        assert_eq!((m, ef_c, ef_s), (32, 400, 200));
        let (m, ef_c, ef_s) = super::hnsw_tier_defaults(1_000_000);
        assert_eq!((m, ef_c, ef_s), (32, 400, 200));
    }

    #[test]
    fn test_hnsw_for_helpers_pick_tier() {
        // Serialize against env_override_tests — these vars are process-global
        // and read by max_nb_connection_for / ef_*_for.
        let _lock = super::HNSW_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // No env override: helper returns the tier value verbatim.
        std::env::remove_var("CQS_HNSW_M");
        std::env::remove_var("CQS_HNSW_EF_CONSTRUCTION");
        std::env::remove_var("CQS_HNSW_EF_SEARCH");
        assert_eq!(super::max_nb_connection_for(2_000), 16);
        assert_eq!(super::ef_construction_for(2_000), 100);
        assert_eq!(super::ef_search_for(2_000), 50);
        assert_eq!(super::max_nb_connection_for(20_000), 24);
        assert_eq!(super::max_nb_connection_for(500_000), 32);
        assert_eq!(super::ef_search_for(500_000), 200);
    }
}

#[cfg(test)]
mod insert_batch_tests {
    use super::*;

    use crate::hnsw::make_test_embedding;
    use crate::EMBEDDING_DIM;

    #[test]
    fn test_insert_batch_on_owned() {
        // Both the initial build and insert_batch go through
        // parallel_insert_data, so the recall assert (chunk_6 reachable) uses
        // the shared retry helper rather than a single un-retried build. The
        // insert-count invariants are asserted on every build.
        let build = || {
            // Build a small Owned HNSW index
            let embeddings: Vec<(String, Embedding)> = (0..5)
                .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
                .collect();

            let mut index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
            let initial_len = index.len();
            assert_eq!(initial_len, 5);

            // Insert new items
            let new_embeddings: Vec<(String, Embedding)> = (5..8)
                .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
                .collect();
            let refs: Vec<(String, &[f32])> = new_embeddings
                .iter()
                .map(|(id, emb)| (id.clone(), emb.as_slice()))
                .collect();

            let inserted = index.insert_batch(&refs).unwrap();
            assert_eq!(inserted, 3);
            assert_eq!(index.len(), initial_len + 3);
            index
        };

        // Search should find the newly inserted chunk_6 in top results.
        assert_self_match_reachable(build, &make_test_embedding(6), "chunk_6", 3);
    }

    #[test]
    fn test_insert_batch_empty() {
        let embeddings: Vec<(String, Embedding)> = (0..3)
            .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
            .collect();

        let mut index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        let initial_len = index.len();

        let inserted = index.insert_batch(&[]).unwrap();
        assert_eq!(inserted, 0);
        assert_eq!(index.len(), initial_len);
    }

    #[test]
    fn test_insert_batch_on_loaded_fails() {
        // Build, save, load, then try insert_batch — should fail
        let embeddings: Vec<(String, Embedding)> = (0..3)
            .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
            .collect();

        let index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();

        // Save to temp dir
        let dir = tempfile::tempdir().unwrap();
        index.save(dir.path(), "test").unwrap();

        // Load back (creates a Loaded variant)
        let mut loaded =
            HnswIndex::load_with_dim(dir.path(), "test", crate::EMBEDDING_DIM).unwrap();

        let new_emb = make_test_embedding(10);
        let items = vec![("new_chunk".to_string(), new_emb.as_slice())];
        let result = loaded.insert_batch(&items);

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Cannot incrementally insert"),
            "Expected 'Cannot incrementally insert' error, got: {}",
            err
        );
    }

    #[test]
    fn test_insert_batch_dimension_mismatch() {
        let embeddings: Vec<(String, Embedding)> = (0..3)
            .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
            .collect();

        let mut index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();

        // Try to insert with wrong dimension
        let bad_vec = vec![1.0f32; 10]; // wrong dimension
        let items = vec![("bad".to_string(), bad_vec.as_slice())];
        let result = index.insert_batch(&items);

        assert!(result.is_err());
        match result.unwrap_err() {
            HnswError::DimensionMismatch { expected, actual } => {
                assert_eq!(expected, EMBEDDING_DIM);
                assert_eq!(actual, 10);
            }
            other => panic!("Expected DimensionMismatch, got: {}", other),
        }
    }

    /// A failed insert (early-return before the graph is touched) must not
    /// leave id_map partially populated.
    #[test]
    fn test_insert_batch_dim_mismatch_leaves_id_map_untouched() {
        let embeddings: Vec<(String, Embedding)> = (0..3)
            .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
            .collect();

        let mut index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        let before = index.len();

        let bad_vec = vec![1.0f32; 10];
        let items = vec![("bad".to_string(), bad_vec.as_slice())];
        let _ = index.insert_batch(&items);

        assert_eq!(
            index.len(),
            before,
            "id_map must not grow when insert fails validation"
        );
    }

    /// id_map slots are claimed before calling into the graph so successful
    /// inserts monotonically advance `base_idx`. Two consecutive inserts must
    /// land at disjoint positions.
    #[test]
    fn test_insert_batch_monotonic_base_idx() {
        let embeddings: Vec<(String, Embedding)> = (0..3)
            .map(|i| (format!("chunk_{}", i), make_test_embedding(i)))
            .collect();

        let mut index = HnswIndex::build_with_dim(embeddings, crate::EMBEDDING_DIM).unwrap();
        let after_build = index.len();

        let batch_a: Vec<(String, Embedding)> = (3..5)
            .map(|i| (format!("a{}", i), make_test_embedding(i)))
            .collect();
        let refs_a: Vec<(String, &[f32])> = batch_a
            .iter()
            .map(|(id, emb)| (id.clone(), emb.as_slice()))
            .collect();
        index.insert_batch(&refs_a).unwrap();
        let after_a = index.len();
        assert_eq!(after_a, after_build + 2);

        let batch_b: Vec<(String, Embedding)> = (5..8)
            .map(|i| (format!("b{}", i), make_test_embedding(i)))
            .collect();
        let refs_b: Vec<(String, &[f32])> = batch_b
            .iter()
            .map(|(id, emb)| (id.clone(), emb.as_slice()))
            .collect();
        index.insert_batch(&refs_b).unwrap();
        let after_b = index.len();
        assert_eq!(after_b, after_a + 3);

        // Both inserts must be findable — confirms id_map entries align
        // with the graph positions they claim.
        let q = make_test_embedding(4);
        let r = index.search(&q, 5);
        assert!(r.iter().any(|n| n.id == "a4"), "a4 should be findable");
        let q = make_test_embedding(6);
        let r = index.search(&q, 5);
        assert!(r.iter().any(|n| n.id == "b6"), "b6 should be findable");
    }
}

#[cfg(test)]
mod env_override_tests {
    /// Serialize tests that manipulate CQS_HNSW_* env vars. Uses the shared
    /// module-level lock so tier-default / build tests in other modules also
    /// serialize against these process-global mutations. Acquisition is
    /// poison-safe (`PoisonError::into_inner`): the lock guards a process
    /// global with no invariant of its own, and a poisoned static would
    /// otherwise cascade panics into every later HNSW test.
    use super::HNSW_ENV_LOCK as ENV_MUTEX;

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// RAII guard pairing `set_var` with `remove_var` on drop — shared
    /// definition in `hnsw/mod.rs` (these tests assert while holding the
    /// env lock).
    use super::EnvVarGuard;

    #[test]
    fn test_m_default() {
        let _lock = lock();
        std::env::remove_var("CQS_HNSW_M");
        assert_eq!(super::max_nb_connection(), 24);
    }

    #[test]
    fn test_m_override() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_M", "32");
        assert_eq!(super::max_nb_connection(), 32);
    }

    #[test]
    fn test_m_invalid_falls_back() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_M", "not_a_number");
        assert_eq!(super::max_nb_connection(), 24);
    }

    #[test]
    fn test_ef_construction_default() {
        let _lock = lock();
        std::env::remove_var("CQS_HNSW_EF_CONSTRUCTION");
        assert_eq!(super::ef_construction(), 200);
    }

    #[test]
    fn test_ef_construction_override() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_EF_CONSTRUCTION", "400");
        assert_eq!(super::ef_construction(), 400);
    }

    #[test]
    fn test_ef_construction_invalid_falls_back() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_EF_CONSTRUCTION", "xyz");
        assert_eq!(super::ef_construction(), 200);
    }

    #[test]
    fn test_ef_search_default() {
        let _lock = lock();
        std::env::remove_var("CQS_HNSW_EF_SEARCH");
        assert_eq!(super::ef_search(), 100);
    }

    #[test]
    fn test_ef_search_override() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_EF_SEARCH", "250");
        assert_eq!(super::ef_search(), 250);
    }

    #[test]
    fn test_ef_search_invalid_falls_back() {
        let _lock = lock();
        let _var = EnvVarGuard::set("CQS_HNSW_EF_SEARCH", "");
        assert_eq!(super::ef_search(), 100);
    }

    /// Unset env → cosine default on the env-resolving build wrapper.
    ///
    /// NOTE (#1351 test discipline): no test sets `CQS_DISTANCE_METRIC` to a
    /// non-"cosine" value — concurrent unlocked loads would observe it and
    /// fail with a metric mismatch. Non-default metrics are exercised
    /// through the explicit `*_and_metric` builders; the env *parse* logic
    /// is covered by the pure `parse_env_value` tests in `index.rs`.
    #[test]
    fn test_distance_metric_default_build_is_cosine() {
        let _lock = lock();
        std::env::remove_var("CQS_DISTANCE_METRIC");
        let idx = super::HnswIndex::build_with_dim(
            vec![("a".to_string(), crate::hnsw::make_test_embedding(1))],
            crate::EMBEDDING_DIM,
        )
        .unwrap();
        assert_eq!(idx.metric(), crate::index::DistanceMetric::Cosine);
    }
}
