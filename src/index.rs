//! Vector index trait for nearest neighbor search
//!
//! Abstracts over different index implementations (HNSW, CAGRA, etc.)
//! to enable runtime selection based on hardware availability.

use std::path::Path;

use thiserror::Error;

use crate::embedder::Embedding;
use crate::store::{ClearHnswDirty, Store, StoreError};

/// Errors produced by [`IndexBackend::try_open`].
///
/// EH-V1.33-7 / #1374: previously `try_open` returned `anyhow::Result`,
/// which leaked the `anyhow` dependency into a public lib-side trait
/// (cqs convention is `thiserror` for library APIs, `anyhow` only in
/// CLI). Most error paths in current backends — checksum mismatches,
/// failed loads, dirty-flag-reads — are already self-handled with
/// `tracing::warn!` + `Ok(None)` so the next backend can take over.
/// The variants below are reserved for the small set of cases where a
/// backend wants a hard failure to bubble up to the selector (currently
/// unused; future backends like USearch / Metal / ROCm can adopt as
/// needed). CLI sites consume this with `?` into `anyhow::Result` exactly
/// as today via the `From` impl.
#[derive(Debug, Error)]
pub enum IndexBackendError {
    /// A backend-internal store query failed in a way that can't be
    /// recovered by falling through to the next backend.
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    /// Persisted index file failed integrity check (blake3 mismatch,
    /// magic-bytes mismatch, dim mismatch, chunk-count mismatch).
    /// Distinct from `LoadFailed` because the operator-facing message
    /// differs: a checksum mismatch usually means the file is stale and
    /// safe to delete, while a load failure may indicate a deeper
    /// corruption.
    #[error("index integrity check failed: {0}")]
    ChecksumMismatch(String),

    /// Persisted index file deserialization failed for reasons other
    /// than a clean integrity mismatch (truncation, IO error during
    /// read, library deserialization error).
    #[error("index load failed: {0}")]
    LoadFailed(String),
}

/// Convenience for CLI consumers that want to fold backend errors into
/// `anyhow::Error` chains. Standard `thiserror`-derived error already
/// implements `std::error::Error`, so `anyhow::Result<...>` accepts it
/// via `?`.
pub type Result<T> = std::result::Result<T, IndexBackendError>;

/// Result from a vector index search
#[derive(Debug, Clone)]
pub struct IndexResult {
    /// Chunk ID (matches Store chunk IDs)
    pub id: String,
    /// Similarity score (0.0 to 1.0, higher is more similar)
    pub score: f32,
}

/// Trait for vector similarity search indexes
/// Implementations must be thread-safe (`Send + Sync`) for use in
/// async contexts like the sqlx store.
pub trait VectorIndex: Send + Sync {
    /// Search for nearest neighbors
    /// # Arguments
    /// * `query` - Query embedding vector (dimension depends on configured model)
    /// * `k` - Maximum number of results to return
    /// # Returns
    /// Results sorted by descending similarity score
    fn search(&self, query: &Embedding, k: usize) -> Vec<IndexResult>;

    /// Number of vectors in the index
    fn len(&self) -> usize;

    /// Check if the index is empty
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Index type name (e.g., "HNSW", "CAGRA")
    fn name(&self) -> &'static str;

    /// Embedding dimension of vectors in this index
    fn dim(&self) -> usize;

    /// Search with traversal-time filtering.
    ///
    /// The predicate receives a chunk_id and returns true to keep the candidate.
    /// HNSW overrides this with traversal-time filtering (skips non-matching nodes
    /// during graph walk). Default impl over-fetches and post-filters.
    fn search_with_filter(
        &self,
        query: &Embedding,
        k: usize,
        filter: &dyn Fn(&str) -> bool,
    ) -> Vec<IndexResult> {
        // Default: over-fetch unfiltered, post-filter by chunk_id
        let results: Vec<IndexResult> = self
            .search(query, k * 3)
            .into_iter()
            .filter(|r| filter(&r.id))
            .take(k)
            .collect();
        // AC-7: Warn when post-filter yields fewer results than requested.
        // This indicates the filter is too restrictive relative to the over-fetch
        // multiplier (3x), or the index is too small.
        if results.len() < k && self.len() >= k {
            tracing::warn!(
                returned = results.len(),
                requested = k,
                index_size = self.len(),
                "Filter-aware search under-returned"
            );
        }
        results
    }

    /// RM-V1.25-19: Has this index observed a panic / poisoned mutex mid-op?
    ///
    /// The default answer is `false`; only the CAGRA GPU backend currently
    /// tracks this. When `true`, the caller should discard the index and
    /// rebuild rather than continue searching against possibly-corrupted
    /// CUDA state (stream in a bad posture, buffer ownership ambiguous,
    /// etc.). HNSW / brute-force search don't manage any GPU context so
    /// they never surface poisoning.
    fn is_poisoned(&self) -> bool {
        false
    }
}

/// Inputs every [`IndexBackend`] needs to decide whether it can serve, and
/// to actually build / load its index when it can. Mode-generic so each
/// backend can call the [`ClearHnswDirty`] dispatch on the store typestate
/// without going through the binary's `cli/store.rs`.
pub struct BackendContext<'a, Mode: ClearHnswDirty> {
    /// The slot's `.cqs/slots/<name>/` directory — every persisted vector
    /// file lives under here.
    pub cqs_dir: &'a Path,
    /// Open store handle (typestate-erased to `Mode`).
    pub store: &'a Store<Mode>,
    /// Optional HNSW search-time `ef` knob — passed to [`crate::HnswIndex::try_load_with_ef`].
    /// Ignored by GPU backends that have their own runtime knobs.
    pub ef_search: Option<usize>,
}

/// Pluggable vector-index backend. Each backend (HNSW, CAGRA, future
/// USearch / SIMD brute-force / Metal / ROCm) declares its own priority
/// and runs its own open path. The selector picks the highest-priority
/// backend whose `try_open` returns `Some`; backends that aren't applicable
/// for this store (GPU unavailable, chunk count below threshold, dirty
/// flag with stale checksums, etc.) return `None` and the selector falls
/// through to the next candidate.
///
/// HNSW is the always-priority-zero fallback; new backends register at
/// higher priorities and only shadow HNSW when they pass their own gates.
pub trait IndexBackend<Mode: ClearHnswDirty>: Send + Sync {
    /// Stable identifier for structured logging (`backend = "cagra"` etc.).
    fn name(&self) -> &'static str;

    /// Higher wins. The slice helper sorts by descending priority; this
    /// only affects iteration order. Real eligibility (GPU available,
    /// chunk count above threshold, dirty flag) is decided inside
    /// `try_open`.
    fn priority(&self) -> i32;

    /// Try to provide a vector index for this store. Return `Ok(None)` to
    /// signal "not applicable, try the next backend" (GPU unavailable,
    /// below threshold, persisted file failed to load and rebuild also
    /// failed, dirty flag with stale checksums). Return `Ok(Some(idx))` on
    /// success. Return `Err(_)` only for true store-level errors that
    /// should abort selection entirely.
    fn try_open(
        &self,
        ctx: &BackendContext<'_, Mode>,
    ) -> std::result::Result<Option<Box<dyn VectorIndex>>, IndexBackendError>;
}

/// #1348 / EX-V1.33-2: declare the registered backends as a single table.
///
/// Each row is either an unconditional `name => path` or a feature-gated
/// `name => path, cfg(feature = "...")`. The macro emits a private const
/// `INDEX_BACKEND_REGISTRY: &[fn() -> &'static dyn IndexBackend<Mode>]`
/// (well — actually a `&[&dyn …]`) covering only the cfg-active rows, so
/// adding a backend is a single new row no matter how many cfg permutations
/// the build matrix has. `[`backends`]` reads the table, copies it, and
/// sorts.
///
/// Backends that ship in the future (USearch / Metal / ROCm / SIMD
/// brute-force) drop in as one extra `Backend, cfg(feature = "<flag>"),`
/// row each. The cfg-permutation matrix at the call site collapses; before
/// this refactor a USearch backend would have required four `#[cfg]` arms
/// (cuda+usearch, cuda only, usearch only, neither) per `backends()`
/// definition.
macro_rules! register_index_backends {
    (
        $(
            $backend:expr
            $(, cfg( $($cfg:tt)+ ))?
            ;
        )+
    ) => {
        pub fn backends<Mode: ClearHnswDirty>() -> Vec<&'static dyn IndexBackend<Mode>> {
            let mut v: Vec<&'static dyn IndexBackend<Mode>> = Vec::new();
            $(
                $( #[cfg( $($cfg)+ )] )?
                v.push(& $backend);
            )+
            v.sort_by_key(|b| std::cmp::Reverse(b.priority()));
            v
        }
    };
}

register_index_backends! {
    crate::hnsw::HnswBackend;
    crate::cagra::CagraBackend, cfg(feature = "cuda-index");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mock VectorIndex for testing trait behavior
    struct MockIndex {
        results: Vec<IndexResult>,
        size: usize,
        dim: usize,
    }

    impl MockIndex {
        /// Creates a new instance with an empty results vector and a specified size capacity.
        /// # Arguments
        /// * `size` - The maximum capacity or size limit for this instance
        /// # Returns
        /// A new `Self` instance with an empty results vector and the given size value
        fn new(size: usize) -> Self {
            Self {
                results: Vec::new(),
                size,
                dim: crate::EMBEDDING_DIM,
            }
        }

        /// Creates a new instance with the given index results.
        /// # Arguments
        /// * `results` - A vector of IndexResult items to store in this instance
        /// # Returns
        /// A new Self instance initialized with the provided results and their count.
        fn with_results(results: Vec<IndexResult>) -> Self {
            let size = results.len();
            Self {
                results,
                size,
                dim: crate::EMBEDDING_DIM,
            }
        }
    }

    impl VectorIndex for MockIndex {
        /// Retrieves the top k search results from the stored results.
        /// # Arguments
        /// * `_query` - An embedding query (unused in this implementation)
        /// * `k` - The number of top results to return
        /// # Returns
        /// A vector of up to k `IndexResult` items, cloned from the internal results storage.
        fn search(&self, _query: &Embedding, k: usize) -> Vec<IndexResult> {
            self.results.iter().take(k).cloned().collect()
        }

        /// Returns the number of elements currently stored in the collection.
        /// # Returns
        /// The total count of elements in the collection as a `usize`.
        fn len(&self) -> usize {
            self.size
        }

        /// Returns the name of this mock object.
        /// # Returns
        /// A static string slice containing the name "Mock".
        fn name(&self) -> &'static str {
            "Mock"
        }

        fn dim(&self) -> usize {
            self.dim
        }
    }

    #[test]
    fn test_index_result_fields() {
        let result = IndexResult {
            id: "chunk_1".to_string(),
            score: 0.95,
        };
        assert_eq!(result.id, "chunk_1");
        assert!((result.score - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn test_default_is_empty() {
        let empty = MockIndex::new(0);
        assert!(empty.is_empty());

        let nonempty = MockIndex::new(5);
        assert!(!nonempty.is_empty());
    }

    #[test]
    fn test_mock_search() {
        let index = MockIndex::with_results(vec![
            IndexResult {
                id: "a".into(),
                score: 0.9,
            },
            IndexResult {
                id: "b".into(),
                score: 0.8,
            },
            IndexResult {
                id: "c".into(),
                score: 0.7,
            },
        ]);
        let query = Embedding::new(vec![0.0; crate::EMBEDDING_DIM]);
        let results = index.search(&query, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "b");
    }

    #[test]
    fn test_trait_object_dispatch() {
        let index: Box<dyn VectorIndex> = Box::new(MockIndex::new(42));
        assert_eq!(index.len(), 42);
        assert!(!index.is_empty());
        assert_eq!(index.name(), "Mock");
    }

    /// HNSW always sits in the slice as the priority-0 fallback. With the
    /// `cuda-index` feature, CAGRA precedes it at priority 100. The slice
    /// is sorted highest-priority-first so callers iterate in the right
    /// order.
    #[test]
    fn test_backends_slice_ordering_readwrite() {
        use crate::store::ReadWrite;
        let backends = backends::<ReadWrite>();
        assert!(!backends.is_empty(), "backends slice must not be empty");
        // Hnsw is always present, always last (priority 0).
        let last = backends.last().unwrap();
        assert_eq!(last.name(), "hnsw");
        assert_eq!(last.priority(), 0);

        #[cfg(feature = "cuda-index")]
        {
            assert_eq!(backends.len(), 2);
            assert_eq!(backends[0].name(), "cagra");
            assert_eq!(backends[0].priority(), 100);
            // Sort puts higher priority first.
            assert!(backends[0].priority() > backends[1].priority());
        }

        #[cfg(not(feature = "cuda-index"))]
        {
            assert_eq!(backends.len(), 1);
        }
    }

    /// Read-only mode produces the same slice as read-write — the
    /// `ClearHnswDirty` typestate is the only thing that varies, and it
    /// only affects `try_open` behavior, not the slice composition.
    #[test]
    fn test_backends_slice_ordering_readonly() {
        use crate::store::ReadOnly;
        let backends = backends::<ReadOnly>();
        assert_eq!(backends.last().unwrap().name(), "hnsw");
        #[cfg(feature = "cuda-index")]
        assert_eq!(backends[0].name(), "cagra");
    }
}
