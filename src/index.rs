//! Vector index trait for nearest neighbor search
//!
//! Abstracts over different index implementations (HNSW, CAGRA, etc.)
//! to enable runtime selection based on hardware availability.

use std::path::Path;

use crate::embedder::Embedding;
use crate::store::{ClearHnswDirty, Store, StoreError};

// Backend methods return `Result<_, StoreError>` directly: every
// non-fall-through error a backend produces is a store-level error, and
// every other failure mode is self-handled via `tracing::warn!` +
// `Ok(None)`. `anyhow::Result<_>` consumes it via `?` because
// `StoreError: std::error::Error`.

/// Distance metric a vector index is built and searched with.
///
/// The metric is an **index-time** choice: it is resolved from
/// `CQS_DISTANCE_METRIC` when an index is built, persisted alongside the
/// index (HNSW: `{basename}.hnsw.meta`; CAGRA: the `.cagra.meta` sidecar),
/// and the stored value wins on every subsequent load. Loading with
/// `CQS_DISTANCE_METRIC` explicitly set to a *different* metric is a typed
/// error ([`crate::hnsw::HnswError::MetricMismatch`]) — never a silent
/// reinterpretation. This mirrors how the embedding model is selected at
/// index time and pinned thereafter (`Store::stored_model_name()`).
///
/// ## Variants and backend support
///
/// - [`Cosine`](Self::Cosine) (default): HNSW uses `DistCosine`; CAGRA keeps
///   cuVS's default `L2Expanded`, which is rank-equivalent to cosine on the
///   unit-norm embeddings cqs produces (`d² = 2 − 2·cos`).
/// - [`DotProduct`](Self::DotProduct): HNSW uses `DistDot`
///   (`dist = 1 − a·b`); CAGRA sets cuVS `InnerProduct`. Note hnsw_rs's
///   `DistDot` asserts `a·b <= 1`, so un-normalized embedding models whose
///   pairwise dot products exceed 1 are not usable on the HNSW backend
///   without a scaling pass.
///
/// `L2` is deliberately absent: both score-conversion paths
/// (`1 − dist` for HNSW, `1 − d²/2` for CAGRA) are cosine-similarity-shaped
/// and an L2 variant needs its own score normalization design before it can
/// honor the documented 0..1 score contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    /// Cosine similarity (the cqs default since v0.1).
    #[default]
    Cosine,
    /// Inner product / dot product. For models whose reference
    /// implementations score with un-normalized dot product
    /// (e.g. CodeRankEmbed) or Matryoshka-truncated embeddings.
    DotProduct,
}

impl DistanceMetric {
    /// Stable on-disk / display name. The persisted index headers store this
    /// string; keep values stable across releases.
    pub fn as_str(&self) -> &'static str {
        match self {
            DistanceMetric::Cosine => "cosine",
            DistanceMetric::DotProduct => "dot",
        }
    }

    /// Read `CQS_DISTANCE_METRIC` if set. `Ok(None)` when unset; a set but
    /// unparseable value is a hard error — a typo must not silently build a
    /// cosine index when the operator asked for dot product.
    ///
    /// TEST DISCIPLINE: load paths call this, so a test that sets
    /// `CQS_DISTANCE_METRIC` to anything other than `"cosine"` makes every
    /// concurrent cosine load in the suite fail with a metric mismatch.
    /// Tests cover the parse/error behavior through the pure
    /// [`Self::parse_env_value`] instead, and only ever set the var to
    /// `"cosine"` (observationally identical to unset), under
    /// `HNSW_ENV_LOCK`.
    pub fn from_env() -> Result<Option<Self>, String> {
        let _span = tracing::info_span!("distance_metric_from_env").entered();
        match std::env::var("CQS_DISTANCE_METRIC") {
            Ok(raw) => Self::parse_env_value(&raw).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("CQS_DISTANCE_METRIC is not valid UTF-8".to_string())
            }
        }
    }

    /// Pure parse of a `CQS_DISTANCE_METRIC` value, factored out of
    /// [`Self::from_env`] so tests can pin the error mapping without
    /// mutating process-global env (see the test-discipline note above).
    fn parse_env_value(raw: &str) -> Result<Self, String> {
        raw.parse::<Self>().map_err(|e| {
            tracing::warn!(value = %raw, error = %e, "Invalid CQS_DISTANCE_METRIC");
            format!("Invalid CQS_DISTANCE_METRIC={raw:?}: {e}")
        })
    }

    /// Build-time resolution: `CQS_DISTANCE_METRIC` if set, else
    /// [`Cosine`](Self::Cosine). Load paths must NOT use this to pick the
    /// metric — the stored value wins there; they use [`Self::from_env`]
    /// only to detect an explicit conflict.
    pub fn resolve() -> Result<Self, String> {
        Ok(Self::from_env()?.unwrap_or_default())
    }
}

impl std::fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DistanceMetric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cosine" => Ok(DistanceMetric::Cosine),
            "dot" | "dotproduct" | "dot_product" | "dot-product" | "innerproduct"
            | "inner_product" | "ip" => Ok(DistanceMetric::DotProduct),
            other => Err(format!(
                "unknown distance metric {other:?} (supported: cosine, dot)"
            )),
        }
    }
}

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
        // Default: over-fetch unfiltered, post-filter by chunk_id.
        // saturating_mul guards against overflow when k is large.
        let results: Vec<IndexResult> = self
            .search(query, k.saturating_mul(3))
            .into_iter()
            .filter(|r| filter(&r.id))
            .take(k)
            .collect();
        // Warn when post-filter yields fewer results than requested.
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

    /// Has this index observed a panic / poisoned mutex mid-op?
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

    /// Maximum `k` this backend can serve in a single search call.
    ///
    /// Returns `None` when the backend has no upper bound (HNSW: bounded
    /// only by index size). Returns `Some(cap)` when the backend silently
    /// fails or returns empty for `k > cap` — CAGRA enforces
    /// `itopk_size >= k` and `itopk_size <= itopk_max(n_vectors)`, so
    /// `k > itopk_max` collapses to an empty Vec which the SPLADE-fusion
    /// path treats as "dense leg found nothing" rather than "backend
    /// refused". Dispatch sites should cap `k` at `max_k()` before the
    /// call so the index returns its actual capacity instead of nothing.
    fn max_k(&self) -> Option<usize> {
        None
    }

    /// Whether [`search`](Self::search)'s emitted `score` is bit-for-bit the
    /// cosine similarity the brute-force scoring path would recompute from the
    /// stored embedding BLOB.
    ///
    /// When `true`, callers may reuse the index-returned score as the dense
    /// base instead of re-fetching the embedding and recomputing cosine — the
    /// ranking is identical, the BLOB fetch + dot product are saved.
    ///
    /// The default is `false` (conservative): only the HNSW backend built with
    /// the `Cosine` metric returns `1 - DistCosine = cos`, exactly the
    /// brute-force value. CAGRA derives its cosine through cuVS `L2Expanded`
    /// (`1 - d/2`, floating-point-divergent and unit-norm-dependent) and its
    /// `DotProduct` metric returns a raw inner product on a different scale, so
    /// CAGRA leaves the default and the optimization is gated off there to
    /// avoid a silent ranking change.
    fn index_scores_are_cosine(&self) -> bool {
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
    /// Backend selection policy from `[index.policy]` in `.cqs.toml`.
    /// `None` when the project's config has no policy override (or no
    /// `[index]` table at all); each backend then falls through to the
    /// env > built-in default chain.
    pub policy: Option<&'a crate::config::IndexPolicy>,
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
    ) -> std::result::Result<Option<Box<dyn VectorIndex>>, StoreError>;
}

/// Declare the registered backends as a single table.
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
/// row each. The table keeps the cfg-permutation matrix collapsed: without
/// it, a USearch backend would need four `#[cfg]` arms (cuda+usearch, cuda
/// only, usearch only, neither) per `backends()` definition.
macro_rules! register_index_backends {
    (
        $(
            $backend:expr
            $(, cfg( $($cfg:tt)+ ))?
            ;
        )+
    ) => {
        // `vec_init_then_push` fires when only one cfg-arm is active for a
        // given build (no `cuda-index` → just HNSW, single `push` after
        // `Vec::new()`). The macro can't statically know how many backends
        // will be registered for a given build matrix, and `vec![...]`
        // doesn't compose cleanly with per-row `#[cfg]` attributes (the
        // closest equivalent — `vec![a, $(#[cfg] b,)?]` — trips on
        // trailing-comma quirks under cfg pruning). The allow is
        // load-bearing: removing it breaks `cargo clippy -- -D warnings` for
        // every backend permutation that ends up with one row active.
        #[allow(clippy::vec_init_then_push)]
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

    /// Default `max_k` returns `None` — backends that don't enforce a per-call
    /// upper bound (HNSW, brute force, MockIndex) leave it at the default.
    #[test]
    fn test_max_k_default_none() {
        let index = MockIndex::new(1000);
        assert_eq!(index.max_k(), None);
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

    // ===== DistanceMetric =====

    #[test]
    fn test_distance_metric_default_is_cosine() {
        assert_eq!(DistanceMetric::default(), DistanceMetric::Cosine);
    }

    #[test]
    fn test_distance_metric_parse_and_roundtrip() {
        for (raw, want) in [
            ("cosine", DistanceMetric::Cosine),
            ("Cosine", DistanceMetric::Cosine),
            ("dot", DistanceMetric::DotProduct),
            ("DotProduct", DistanceMetric::DotProduct),
            ("dot_product", DistanceMetric::DotProduct),
            ("dot-product", DistanceMetric::DotProduct),
            ("ip", DistanceMetric::DotProduct),
        ] {
            assert_eq!(raw.parse::<DistanceMetric>().unwrap(), want, "raw={raw}");
        }
        // as_str → parse roundtrip (the on-disk header contract).
        for m in [DistanceMetric::Cosine, DistanceMetric::DotProduct] {
            assert_eq!(m.as_str().parse::<DistanceMetric>().unwrap(), m);
        }
    }

    #[test]
    fn test_distance_metric_parse_rejects_unknown() {
        let err = "euclidean".parse::<DistanceMetric>().unwrap_err();
        assert!(
            err.contains("euclidean"),
            "error should name the value: {err}"
        );
        assert!("l2".parse::<DistanceMetric>().is_err());
        assert!("".parse::<DistanceMetric>().is_err());
    }

    /// Env-value handling, via the pure `parse_env_value` seam (no
    /// process-global env mutation — see the test-discipline note on
    /// `from_env`: a non-"cosine" value would fail concurrent loads).
    #[test]
    fn test_distance_metric_parse_env_value() {
        assert_eq!(
            DistanceMetric::parse_env_value("dot").unwrap(),
            DistanceMetric::DotProduct
        );
        assert_eq!(
            DistanceMetric::parse_env_value("cosine").unwrap(),
            DistanceMetric::Cosine
        );
        // Unparseable value is a hard error, not a silent cosine fallback,
        // and the error names the env var for the operator.
        let err = DistanceMetric::parse_env_value("manhattan").unwrap_err();
        assert!(
            err.contains("CQS_DISTANCE_METRIC") && err.contains("manhattan"),
            "error should name the var and value: {err}"
        );
    }

    /// `from_env`/`resolve` with the var unset (the suite-wide steady
    /// state): `None` / cosine. Held under the shared HNSW env lock so a
    /// concurrent "cosine"-setting test can't perturb the read.
    #[test]
    fn test_distance_metric_env_unset_resolves_cosine() {
        let _lock = crate::hnsw::HNSW_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::env::remove_var("CQS_DISTANCE_METRIC");
        assert_eq!(DistanceMetric::from_env().unwrap(), None);
        assert_eq!(DistanceMetric::resolve().unwrap(), DistanceMetric::Cosine);
    }
}
