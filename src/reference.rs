//! Reference index support for multi-index search
//!
//! A reference index is a standard cqs index (SQLite DB + HNSW files) created
//! from an external codebase. References are read-only during search. Results
//! from references have their scores multiplied by a weight (default 0.8) to
//! rank them below equally-similar project results.

use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use tokio::runtime::Runtime;

use crate::config::ReferenceConfig;
use crate::hnsw::HnswIndex;
use crate::index::VectorIndex;
use crate::store::{
    DataVersionProbe, FileIdentity, ReadOnly, SearchFilter, SearchResult, Store, StoreError,
    UnifiedResult,
};
use crate::Embedding;

/// A loaded reference index ready for searching
///
/// Cannot derive `Debug` because `Box<dyn VectorIndex>` is not `Debug`.
pub struct ReferenceIndex {
    /// Display name
    pub name: String,
    /// The reference's store (separate DB + connection pool).
    ///
    /// Always `Store<ReadOnly>` — references are loaded from external
    /// codebases and only exposed through search/caller queries. The
    /// typestate turns any accidental write into a compile error.
    pub store: Store<ReadOnly>,
    /// Optional HNSW index for O(log n) search
    pub index: Option<Box<dyn VectorIndex>>,
    /// Score multiplier (0.0-1.0)
    pub weight: f32,
    /// Path to the reference's `index.db` — kept so staleness checks can
    /// re-stat the file and re-open the data_version probe without
    /// reconstructing the path from `name + path`.
    pub db_path: std::path::PathBuf,
    /// Freshness key of `index.db` at load time. Long-lived daemons that cache
    /// loaded references need to invalidate them when the reference's own
    /// `cqs ref update <name>` reindexes the DB. The primary project's identity
    /// change wouldn't catch this, so each cached reference tracks its own.
    pub loaded_identity: Option<FileIdentity>,
    /// Long-lived `PRAGMA data_version` probe — the second freshness
    /// discriminator. `cqs ref update` reindexes the DB *in place* (incremental
    /// pipeline, not rename-over), so WAL-mode commits can land before the
    /// closing checkpoint moves the file identity; the probe is the only
    /// discriminator that catches that window. Interior-mutable (`Mutex`)
    /// because [`Self::is_stale`] is `&self` — the index is shared across daemon
    /// threads via `Arc<ReferenceIndex>` in the references LRU. `None` when the
    /// probe couldn't be opened (warned, identity-only fallback); re-opened
    /// lazily on the next staleness check.
    data_version_probe: Mutex<Option<DataVersionProbe>>,
    /// Runtime handle (cloned from the store) that drives the probe's async
    /// sqlx queries, so the probe stays on the store's worker pool.
    runtime: Arc<Runtime>,
}

impl std::fmt::Debug for ReferenceIndex {
    /// Formats a ReferenceIndex for debugging output.
    ///
    /// # Arguments
    ///
    /// * `f` - The formatter to write the debug representation to
    ///
    /// # Returns
    ///
    /// A Result indicating whether formatting succeeded or failed
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReferenceIndex")
            .field("name", &self.name)
            .field("weight", &self.weight)
            .field("has_index", &self.index.is_some())
            .finish()
    }
}

/// A search result tagged with its source
#[derive(Debug)]
pub struct TaggedResult {
    /// The underlying search result
    pub result: UnifiedResult,
    /// Source: None = primary project, Some(name) = reference
    pub source: Option<String>,
}

/// Load a single reference index, returning None on failure.
fn load_single_reference(cfg: &ReferenceConfig) -> Option<ReferenceIndex> {
    let _span = tracing::info_span!("load_single_reference", name = %cfg.name).entered();
    if cfg
        .path
        .symlink_metadata()
        .map(|m| m.is_symlink())
        .unwrap_or(false)
    {
        tracing::warn!(
            name = cfg.name,
            path = %cfg.path.display(),
            "Skipping reference: path is a symlink (use the real path instead)"
        );
        return None;
    }

    // Warn if reference path is outside project and home directories.
    // Canonicalize to resolve any `..` segments, then check containment.
    // Use `dunce::canonicalize` and canonicalize both sides so Windows
    // verbatim (`\\?\C:\...`) vs non-verbatim mismatches don't defeat the
    // comparison.
    if let Ok(canonical) = dunce::canonicalize(&cfg.path) {
        let home = dirs::home_dir().and_then(|h| dunce::canonicalize(h).ok());
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|c| dunce::canonicalize(c).ok());
        let in_home = home.as_ref().is_some_and(|h| canonical.starts_with(h));
        let in_project = cwd.as_ref().is_some_and(|p| canonical.starts_with(p));
        let in_cqs_dir = canonical.components().any(|c| c.as_os_str() == ".cqs");
        if !in_home && !in_project && !in_cqs_dir {
            tracing::warn!(
                name = %cfg.name,
                path = %canonical.display(),
                "Reference path is outside project and home directories"
            );
        }
    }

    let db_path = cfg.path.join(crate::INDEX_DB_FILENAME);
    // Reference indexes hold a Store for as long as the LRU cache keeps them
    // resident. Use `open_readonly_small` (16MB mmap, 1MB cache) instead of
    // `open_readonly` (64MB mmap) so a 4-reference session reserves tens of MB
    // of mmap instead of hundreds. Reference queries are low-volume compared
    // to primary-index full-scan reads.
    let store = match Store::open_readonly_small(&db_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "Skipping reference '{}': failed to open {}: {}",
                cfg.name,
                db_path.display(),
                e
            );
            return None;
        }
    };

    // Pass the store's actual dim so the HNSW loader trusts the right byte
    // layout. A reference built with a 768-dim model (E5-base or v9-200k)
    // would otherwise load as `EMBEDDING_DIM`-dim garbage.
    let index = HnswIndex::try_load_with_ef(&cfg.path, None, store.dim());

    // `new_loaded` captures the freshness key (file identity + data_version
    // probe) at load time so cached references can be invalidated when the
    // reference itself is re-indexed.
    Some(ReferenceIndex::new_loaded(
        cfg.name.clone(),
        store,
        index,
        cfg.weight,
        db_path,
    ))
}

impl ReferenceIndex {
    /// Construct a `ReferenceIndex` around an already-opened store, deriving the
    /// freshness key (file identity + data_version probe) from `db_path`.
    ///
    /// The single construction path besides [`load_single_reference`], so the
    /// freshness-key fields stay private and a caller cannot hand-assemble a
    /// `ReferenceIndex` with an ad-hoc `(mtime, size)` key — the next hardening
    /// of [`FileIdentity`] propagates here by construction. The runtime is
    /// cloned from the store so the probe stays on the store's worker pool.
    pub fn new_loaded(
        name: String,
        store: Store<ReadOnly>,
        index: Option<Box<dyn VectorIndex>>,
        weight: f32,
        db_path: std::path::PathBuf,
    ) -> Self {
        let loaded_identity = FileIdentity::from_path(&db_path);
        let runtime = Arc::clone(store.runtime());
        let data_version_probe = Mutex::new(DataVersionProbe::open(&runtime, &db_path));
        Self {
            name,
            store,
            index,
            weight,
            db_path,
            loaded_identity,
            data_version_probe,
            runtime,
        }
    }

    /// Has the reference's `index.db` been rewritten since load?
    ///
    /// Two discriminators, OR-combined:
    /// 1. [`FileIdentity`] change — catches rename-over and checkpoint (the
    ///    `cqs ref update` close folds the WAL back, moving size/mtime/inode).
    /// 2. `PRAGMA data_version` movement on the long-lived probe — catches the
    ///    in-place WAL-incremental window before that checkpoint.
    ///
    /// If we couldn't read the file (or had no identity at load), returns
    /// `false` so the caller keeps using the cached index — transient
    /// NFS/permission glitches shouldn't thrash the LRU. A `true` result causes
    /// the references LRU to pop and reload this entry, so the probe baseline is
    /// only meaningful for the steady-state "not stale" path, where each query
    /// advances it to observe the next commit.
    pub fn is_stale(&self) -> bool {
        let Some(loaded) = self.loaded_identity else {
            return false;
        };
        let Some(current) = FileIdentity::from_path(&self.db_path) else {
            return false;
        };
        if current != loaded {
            return true;
        }
        // Identity unchanged — consult the probe for the WAL-incremental case.
        let mut slot = self
            .data_version_probe
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match slot.as_mut() {
            Some(probe) => match probe.changed(&self.runtime) {
                Ok(changed) => changed,
                Err(e) => {
                    tracing::warn!(
                        name = %self.name,
                        error = %e,
                        "data_version probe query failed — dropping probe; will re-open on next staleness check"
                    );
                    *slot = None;
                    false
                }
            },
            None => {
                // Earlier open failed (or the probe was dropped after a query
                // error) — retry. Freshly baselined, so nothing to compare.
                *slot = DataVersionProbe::open(&self.runtime, &self.db_path);
                false
            }
        }
    }
}

/// Load reference indexes from config, skipping any that fail to open.
///
/// References are loaded in parallel via rayon — each Store::open_readonly_small +
/// HnswIndex::try_load is independent I/O (10-50ms each). Both Store and
/// HnswIndex are Send + Sync.
pub fn load_references(configs: &[ReferenceConfig]) -> Vec<ReferenceIndex> {
    let _span = tracing::debug_span!("load_references", count = configs.len()).entered();
    // Cap concurrency — each ref loads Store (~16MB mmap via
    // open_readonly_small) + HNSW (~50-200MB). HNSW dominates the footprint.
    // Scale thread count with cores, capped at 8.
    let threads = std::env::var("CQS_RAYON_THREADS")
        .ok()
        .and_then(|v| {
            let parsed = v.parse();
            if parsed.is_err() {
                tracing::warn!(value = %v, "Invalid CQS_RAYON_THREADS, using default");
            }
            parsed.ok()
        })
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4)
        });
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(threads).build() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create reference loading thread pool, loading sequentially");
            // Fallback: load sequentially instead of panicking
            return configs.iter().filter_map(load_single_reference).collect();
        }
    };
    let refs: Vec<ReferenceIndex> = pool.install(|| {
        configs
            .par_iter()
            .filter_map(load_single_reference)
            .collect()
    });

    if !refs.is_empty() {
        tracing::info!(count = refs.len(), "Loaded reference indexes");
    }

    refs
}

/// Search a single reference index by embedding.
///
/// When `apply_weight` is true, multiplies scores by the reference weight and
/// re-filters against the threshold (used for multi-index merged search).
/// When false, returns raw scores (used for `--ref` scoped search).
pub fn search_reference(
    ref_idx: &ReferenceIndex,
    query_embedding: &Embedding,
    filter: &SearchFilter,
    limit: usize,
    threshold: f32,
    apply_weight: bool,
) -> Result<Vec<SearchResult>, StoreError> {
    let _span =
        tracing::info_span!("search_reference", name = %ref_idx.name, weight = ref_idx.weight, apply_weight)
            .entered();
    // When `apply_weight`, the underlying store search would otherwise
    // filter at the raw threshold AND cap at `limit` — both computed before
    // the post-weight retain step. That under-samples the
    // corpus when `weight < 1`: a candidate that scores `0.6 * weight` may
    // exceed the *post-weight* threshold yet get dropped by the *pre-weight*
    // limit cap. Relax the store's threshold and over-fetch headroom so
    // the post-weight retain + truncate sees the right pool.
    let raw_threshold = if apply_weight && ref_idx.weight > 0.0 {
        threshold / ref_idx.weight
    } else {
        threshold
    };
    let raw_limit = if apply_weight {
        // 2× over-fetch leaves headroom for the post-weight retain step.
        limit.saturating_mul(2).max(limit)
    } else {
        limit
    };
    let mut results = ref_idx.store.search_filtered_with_index(
        query_embedding,
        filter,
        raw_limit,
        raw_threshold,
        ref_idx.index.as_deref(),
    )?;
    if apply_weight {
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
        // Re-filter after weight against the *requested* threshold.
        results.retain(|r| r.score >= threshold);
        // Stable tie-break on chunk id so two refs with identical scores
        // produce deterministic merged ranking.
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk.id.cmp(&b.chunk.id))
        });
        results.truncate(limit);
    }
    Ok(results)
}

/// Search a reference by name.
///
/// When `apply_weight` is true, multiplies scores by the reference weight and
/// re-filters against the threshold (used for multi-index merged search).
/// When false, returns raw scores (used for `--ref` scoped search).
pub fn search_reference_by_name(
    ref_idx: &ReferenceIndex,
    name: &str,
    limit: usize,
    threshold: f32,
    apply_weight: bool,
) -> Result<Vec<SearchResult>, StoreError> {
    let _span =
        tracing::info_span!("search_reference_by_name", ref_name = %ref_idx.name, query = name, apply_weight)
            .entered();
    // Same shape as the embedding path — over-fetch from `search_by_name` so
    // the post-weight retain doesn't see a pre-truncated pool. The
    // `retain(|r| r.score * weight >= threshold)` boundary is correct; the
    // pre-weight limit cap is what needs the over-fetch headroom.
    let raw_limit = if apply_weight {
        limit.saturating_mul(2).max(limit)
    } else {
        limit
    };
    let mut results = ref_idx.store.search_by_name(name, raw_limit)?;
    if apply_weight {
        results.retain(|r| r.score * ref_idx.weight >= threshold);
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.chunk.id.cmp(&b.chunk.id))
        });
        results.truncate(limit);
    } else {
        results.retain(|r| r.score >= threshold);
    }
    Ok(results)
}

/// Merge primary results with reference results, sorted by score, truncated to limit.
///
/// Deduplicates code results with identical content across stores — keeps the
/// highest-scoring occurrence. Notes (project-local) are never deduplicated.
pub fn merge_results(
    primary: Vec<UnifiedResult>,
    refs: Vec<(String, Vec<SearchResult>)>,
    limit: usize,
) -> Vec<TaggedResult> {
    let mut tagged: Vec<TaggedResult> = Vec::new();

    // Add primary results
    for result in primary {
        tagged.push(TaggedResult {
            result,
            source: None,
        });
    }

    // Add reference results (code only — notes are project-local)
    for (name, results) in refs {
        for r in results {
            tagged.push(TaggedResult {
                result: UnifiedResult::Code(r),
                source: Some(name.clone()),
            });
        }
    }

    // Sort by score descending (highest first). Secondary sort on chunk id
    // keeps equal-score candidates deterministically ordered across process
    // invocations so the dedup + truncate below pick the same survivors.
    tagged.sort_by(|a, b| {
        b.result
            .score()
            .total_cmp(&a.result.score())
            .then(a.result.id().cmp(b.result.id()))
    });

    // Deduplicate code results by content hash (keeps highest-scoring occurrence).
    // Dedup must happen before truncation for correctness — otherwise duplicates
    // from different sources could occupy result slots, pushing out unique results.
    // Use stored content_hash when available instead of recomputing blake3.
    let mut seen_hashes = std::collections::HashSet::new();
    tagged.retain(|t| match &t.result {
        UnifiedResult::Code(r) => {
            if r.chunk.content_hash.is_empty() {
                // Fallback for chunks without a stored hash
                let hash = blake3::hash(r.chunk.content.as_bytes()).to_string();
                seen_hashes.insert(hash)
            } else {
                seen_hashes.insert(r.chunk.content_hash.clone())
            }
        }
    });

    tagged.truncate(limit);
    tagged
}

/// Build a rerank candidate pool from primary + reference legs, selecting
/// survivors by a **frame-neutral** key (each candidate's rank within its own
/// leg) rather than by the raw `score`.
///
/// This is the pool feeder for the merged-set cross-encoder rerank only — the
/// reranker rescores every survivor onto one comparable scale afterward, so the
/// order this function emits is irrelevant; only *which* candidates survive the
/// `pool_limit` truncation matters. The plain (no-rerank) merge stays on
/// [`merge_results`]' raw-`score` sort, untouched.
///
/// Why frame-neutral selection: the primary leg arrives in the project's score
/// frame (sigmoid `[0, 1]` once the project pool was reranked) while the
/// reference legs carry weighted cosine. A raw-`score` truncation across those
/// incomparable frames can drop a low-cosine / high-relevance reference or
/// overlay hit — exactly the candidate the reranker exists to surface — before
/// the cross-encoder ever scores it, and the bite worsens as `limit` grows past
/// the pool cap. Selecting by within-leg rank keeps the top of *each* leg in the
/// pool regardless of frame, so the reranker sees the full candidate set.
///
/// Legs are interleaved by rank (every leg's rank-0, then every leg's rank-1, …)
/// so a leg with few hits never crowds out the others. Dedup by content hash
/// happens during the interleave, matching [`merge_results`]' content-hash
/// dedup, so identical content across stores occupies a single pool slot: the
/// first candidate reached in the round-robin survives, which — because the
/// interleave visits lower ranks first and each leg is sorted within itself — is
/// the higher-ranked copy.
pub fn merge_results_for_rerank(
    primary: Vec<UnifiedResult>,
    refs: Vec<(String, Vec<SearchResult>)>,
    pool_limit: usize,
) -> Vec<TaggedResult> {
    // Build per-leg candidate queues, preserving each leg's own (already-sorted)
    // order — that order *is* the frame-neutral rank. `VecDeque` lets the
    // round-robin pop from the front without shifting the tail.
    let mut legs: Vec<std::collections::VecDeque<TaggedResult>> =
        Vec::with_capacity(1 + refs.len());

    legs.push(
        primary
            .into_iter()
            .map(|result| TaggedResult {
                result,
                source: None,
            })
            .collect(),
    );

    for (name, results) in refs {
        legs.push(
            results
                .into_iter()
                .map(|r| TaggedResult {
                    result: UnifiedResult::Code(r),
                    source: Some(name.clone()),
                })
                .collect(),
        );
    }

    // Round-robin interleave by within-leg rank: take rank-0 from every leg,
    // then rank-1 from every leg, and so on. Dedup by content hash as we go so
    // an identical chunk from a lower-ranked leg can't displace a distinct
    // candidate later. Truncation by `pool_limit` then keeps the top of each leg
    // rather than letting one frame's scores dominate the cut.
    let mut pool: Vec<TaggedResult> = Vec::new();
    let mut seen_hashes = std::collections::HashSet::new();

    'outer: while legs.iter().any(|leg| !leg.is_empty()) {
        for leg in legs.iter_mut() {
            let Some(candidate) = leg.pop_front() else {
                continue;
            };
            let fresh = match &candidate.result {
                UnifiedResult::Code(r) => {
                    if r.chunk.content_hash.is_empty() {
                        let hash = blake3::hash(r.chunk.content.as_bytes()).to_string();
                        seen_hashes.insert(hash)
                    } else {
                        seen_hashes.insert(r.chunk.content_hash.clone())
                    }
                }
            };
            if fresh {
                pool.push(candidate);
                if pool.len() >= pool_limit {
                    break 'outer;
                }
            }
        }
    }

    pool
}

/// Default storage directory for reference indexes
pub fn refs_dir() -> Option<std::path::PathBuf> {
    let dir = dirs::data_local_dir();
    if dir.is_none() {
        tracing::warn!("Could not determine local data directory for reference storage");
    }
    dir.map(|d| d.join("cqs/refs"))
}

/// Validate a reference name (no path separators or traversal)
pub fn validate_ref_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("Reference name cannot be empty");
    }
    if name.contains('\0') {
        return Err("Reference name cannot contain null bytes");
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("Reference name cannot contain '/', '\\', or '..'");
    }
    if name == "." {
        return Err("Reference name cannot be '.'");
    }
    if name.starts_with('.') {
        return Err("Reference name cannot start with '.'");
    }
    Ok(())
}

/// Get the storage path for a named reference
pub fn ref_path(name: &str) -> Option<std::path::PathBuf> {
    validate_ref_name(name).ok()?;
    refs_dir().map(|d| d.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ChunkSummary;

    /// Constructs a `SearchResult` for a Rust function code chunk with the given name and relevance score.
    ///
    /// # Arguments
    ///
    /// * `name` - The function name used to populate the chunk ID, file path, and function signature
    /// * `score` - The relevance score assigned to the search result
    ///
    /// # Returns
    ///
    /// A `SearchResult` containing a `ChunkSummary` representing a Rust function located at `src/{name}.rs` with minimal metadata and the provided score.
    fn make_code_result(name: &str, score: f32) -> SearchResult {
        SearchResult::new(
            ChunkSummary {
                id: format!("id-{}", name),
                file: std::path::PathBuf::from(format!("src/{}.rs", name)),
                language: crate::parser::Language::Rust,
                chunk_type: crate::parser::ChunkType::Function,
                name: name.to_string(),
                signature: String::new(),
                content: format!("fn {}() {{}}", name),
                doc: None,
                line_start: 1,
                line_end: 1,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            score,
        )
    }

    #[test]
    fn test_merge_results_empty_refs() {
        let primary = vec![UnifiedResult::Code(make_code_result("foo", 0.9))];
        let refs: Vec<(String, Vec<SearchResult>)> = vec![];

        let merged = merge_results(primary, refs, 10);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].source.is_none());
    }

    #[test]
    fn test_merge_results_only_refs() {
        let primary: Vec<UnifiedResult> = vec![];
        let refs = vec![("tokio".to_string(), vec![make_code_result("spawn", 0.8)])];

        let merged = merge_results(primary, refs, 10);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source.as_deref(), Some("tokio"));
    }

    #[test]
    fn test_merge_results_sorted_by_score() {
        let primary = vec![
            UnifiedResult::Code(make_code_result("primary_low", 0.5)),
            UnifiedResult::Code(make_code_result("primary_high", 0.95)),
        ];
        let refs = vec![(
            "tokio".to_string(),
            vec![
                make_code_result("ref_mid", 0.7),
                make_code_result("ref_high", 0.9),
            ],
        )];

        let merged = merge_results(primary, refs, 10);
        assert_eq!(merged.len(), 4);
        // Should be sorted: 0.95, 0.9, 0.7, 0.5
        assert!(merged[0].result.score() >= merged[1].result.score());
        assert!(merged[1].result.score() >= merged[2].result.score());
        assert!(merged[2].result.score() >= merged[3].result.score());
    }

    #[test]
    fn test_merge_results_truncates_to_limit() {
        let primary = vec![
            UnifiedResult::Code(make_code_result("a", 0.9)),
            UnifiedResult::Code(make_code_result("b", 0.8)),
            UnifiedResult::Code(make_code_result("c", 0.7)),
        ];
        let refs = vec![("tokio".to_string(), vec![make_code_result("d", 0.85)])];

        let merged = merge_results(primary, refs, 2);
        assert_eq!(merged.len(), 2);
        // Top 2 by score: 0.9, 0.85
        assert!(merged[0].result.score() > 0.85);
    }

    #[test]
    fn test_merge_results_weight_applied() {
        // Simulate weight already applied: ref result at 0.72 (was 0.9 * 0.8)
        let primary = vec![UnifiedResult::Code(make_code_result("project_fn", 0.8))];
        let refs = vec![(
            "tokio".to_string(),
            vec![make_code_result("ref_fn", 0.72)], // weight already applied
        )];

        let merged = merge_results(primary, refs, 10);
        assert_eq!(merged.len(), 2);
        // Primary (0.8) should rank above weighted ref (0.72)
        assert!(merged[0].source.is_none());
        assert_eq!(merged[1].source.as_deref(), Some("tokio"));
    }

    #[test]
    fn test_tagged_result_source_values() {
        let primary = vec![UnifiedResult::Code(make_code_result("a", 0.9))];
        let refs = vec![
            ("tokio".to_string(), vec![make_code_result("b", 0.8)]),
            ("serde".to_string(), vec![make_code_result("c", 0.7)]),
        ];

        let merged = merge_results(primary, refs, 10);
        assert!(merged[0].source.is_none()); // primary
        assert_eq!(merged[1].source.as_deref(), Some("tokio"));
        assert_eq!(merged[2].source.as_deref(), Some("serde"));
    }

    #[test]
    fn test_load_references_skips_missing_path() {
        let configs = vec![ReferenceConfig {
            name: "nonexistent".into(),
            path: "/tmp/cqs_test_nonexistent_ref_path_12345".into(),
            source: None,
            weight: 0.8,
        }];

        let refs = load_references(&configs);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_ref_path_helper() {
        if let Some(path) = ref_path("tokio") {
            assert!(path.ends_with("cqs/refs/tokio"));
        }
    }

    #[test]
    fn test_validate_ref_name_rejects_traversal() {
        assert!(validate_ref_name("../etc").is_err());
        assert!(validate_ref_name("foo/bar").is_err());
        assert!(validate_ref_name("foo\\bar").is_err());
        assert!(validate_ref_name("..").is_err());
        assert!(validate_ref_name(".").is_err());
        assert!(validate_ref_name("").is_err());
        assert!(validate_ref_name("foo\0bar").is_err());
    }

    #[test]
    fn test_validate_ref_name_accepts_valid() {
        assert!(validate_ref_name("tokio").is_ok());
        assert!(validate_ref_name("my-ref").is_ok());
        assert!(validate_ref_name("ref_v2").is_ok());
    }

    #[test]
    fn test_merge_deduplicates_by_content() {
        // Same content in primary and reference — keep highest score
        let primary = vec![UnifiedResult::Code(SearchResult::new(
            ChunkSummary {
                id: "primary-id".to_string(),
                file: std::path::PathBuf::from("src/foo.rs"),
                language: crate::parser::Language::Rust,
                chunk_type: crate::parser::ChunkType::Function,
                name: "foo".to_string(),
                signature: String::new(),
                content: "fn foo() {}".to_string(), // same content
                doc: None,
                line_start: 1,
                line_end: 1,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            0.9,
        ))];
        let refs = vec![(
            "ref1".to_string(),
            vec![SearchResult::new(
                ChunkSummary {
                    id: "ref-id".to_string(),
                    file: std::path::PathBuf::from("src/foo.rs"),
                    language: crate::parser::Language::Rust,
                    chunk_type: crate::parser::ChunkType::Function,
                    name: "foo".to_string(),
                    signature: String::new(),
                    content: "fn foo() {}".to_string(), // same content
                    doc: None,
                    line_start: 1,
                    line_end: 1,
                    parent_id: None,
                    parent_type_name: None,
                    content_hash: String::new(),
                    window_idx: None,
                    parser_version: 0,
                    vendored: false,
                },
                0.7,
            )],
        )];

        let merged = merge_results(primary, refs, 10);
        // Should have 1 result (deduped), not 2
        assert_eq!(merged.len(), 1);
        // Kept the highest-scoring one (primary, 0.9)
        assert!(merged[0].source.is_none());
        assert!((merged[0].result.score() - 0.9).abs() < 0.01);
    }

    // A high-relevance reference hit whose raw cosine is low (the frame-mismatch
    // case the merged-set rerank exists to recover) must survive into the rerank
    // pool at `--limit >= 10`, where the pool cap (20) is at or below the project
    // leg's over-fetch, so the cross-encoder can rescore it. The raw-
    // score merge (`merge_results`) sorts the project leg's high cosines to the
    // top and truncates the low-cosine target out before the reranker can see
    // it; the frame-neutral pool (`merge_results_for_rerank`) keeps the top of
    // each leg, so the target survives. Calibrated RED against `merge_results`,
    // GREEN against `merge_results_for_rerank`.
    #[test]
    fn test_rerank_pool_keeps_low_cosine_ref_at_large_limit() {
        // limit 10 -> rerank_pool_size = min(40, 20) = 20.
        let pool_limit = 20;

        // Project leg: 25 high-cosine hits (over-fetched well past the pool cap).
        // All score above the reference target's raw cosine.
        let primary: Vec<UnifiedResult> = (0..25)
            .map(|i| {
                UnifiedResult::Code(make_code_result(
                    &format!("project_{i:02}"),
                    0.95 - 0.001 * i as f32,
                ))
            })
            .collect();

        // Reference leg: one low-raw-cosine but high-relevance target (rank-0 in
        // its own frame).
        let refs = vec![(
            "refstore".to_string(),
            vec![make_code_result("ref_target", 0.10)],
        )];

        // Old raw-score path: the 20 highest cosines are all project hits, so the
        // 0.10 reference target is truncated out of the pool.
        let old_pool = merge_results(primary.clone(), refs.clone(), pool_limit);
        assert_eq!(old_pool.len(), pool_limit);
        assert!(
            !old_pool.iter().any(|t| t.result.id() == "id-ref_target"),
            "raw-score truncation drops the low-cosine reference target before rerank"
        );

        // New frame-neutral path: the reference leg's rank-0 candidate is
        // interleaved into the pool and survives the truncation.
        let new_pool = merge_results_for_rerank(primary, refs, pool_limit);
        assert_eq!(new_pool.len(), pool_limit);
        let target = new_pool
            .iter()
            .find(|t| t.result.id() == "id-ref_target")
            .expect("frame-neutral pool must keep the low-cosine reference target");
        assert_eq!(target.source.as_deref(), Some("refstore"));
    }

    // `merge_results_for_rerank` must still dedup identical content across legs:
    // a chunk present in both the project and a reference occupies one pool slot,
    // keeping the project (first-seen, higher within-leg rank) copy.
    #[test]
    fn test_rerank_pool_dedups_across_legs() {
        let primary = vec![UnifiedResult::Code(make_code_result("shared", 0.9))];
        let refs = vec![(
            "refstore".to_string(),
            vec![make_code_result("shared", 0.8)], // identical content
        )];

        let pool = merge_results_for_rerank(primary, refs, 20);
        assert_eq!(
            pool.len(),
            1,
            "identical content occupies a single pool slot"
        );
        assert!(
            pool[0].source.is_none(),
            "the project (first-seen) copy survives dedup"
        );
    }

    #[test]
    fn test_ref_path_rejects_traversal() {
        assert!(ref_path("../etc").is_none());
        assert!(ref_path("foo/bar").is_none());
    }

    /// Build a `ReferenceIndex` backed by a real on-disk `index.db` so the
    /// freshness-key tests can rewrite the file underneath it. Keeps the
    /// tempdir alive (`keep`) for the test duration. Returns the index and its
    /// `index.db` path.
    fn make_disk_reference() -> (ReferenceIndex, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let model_info = crate::store::ModelInfo::default();
        let store =
            Store::<ReadOnly>::open_readonly_after_init(&db_path, |s| s.init(&model_info)).unwrap();
        let _keep = dir.keep();
        let ref_idx =
            ReferenceIndex::new_loaded("disk-ref".to_string(), store, None, 1.0, db_path.clone());
        (ref_idx, db_path)
    }

    /// Sub-second, same-size, in-place replacement (rename-over) of the
    /// reference's `index.db` must flip `is_stale()` true via the file-identity
    /// inode bump — even when mtime and size are unchanged.
    ///
    /// Calibration: RED against the previous `(mtime, size)`-only key (a
    /// same-size rewrite inside one WSL mtime bucket reads as unchanged → stale
    /// serve from the LRU). GREEN with [`FileIdentity`], which mixes in the
    /// inode. Pins the inode-catchable replace case.
    #[cfg(unix)]
    #[test]
    fn test_is_stale_detects_same_size_rename_over() {
        use std::os::unix::fs::MetadataExt;

        let (ref_idx, db_path) = make_disk_reference();
        assert!(!ref_idx.is_stale(), "freshly loaded reference is not stale");

        let size_before = std::fs::metadata(&db_path).unwrap().len();
        let inode_before = std::fs::metadata(&db_path).unwrap().ino();
        let orig_mtime = std::fs::metadata(&db_path).unwrap().modified().unwrap();

        // Byte-identical copy → same size; rename-over → new inode. Force the
        // original mtime so only the inode discriminator can fire.
        let replacement = db_path.with_extension("db.replacement");
        std::fs::copy(&db_path, &replacement).unwrap();
        std::fs::File::open(&replacement)
            .unwrap()
            .set_modified(orig_mtime)
            .unwrap();
        std::fs::rename(&replacement, &db_path).unwrap();

        let md_after = std::fs::metadata(&db_path).unwrap();
        assert_eq!(md_after.len(), size_before, "precondition: size unchanged");
        assert_ne!(
            md_after.ino(),
            inode_before,
            "precondition: rename-over landed a new inode"
        );
        assert_eq!(
            md_after.modified().unwrap(),
            orig_mtime,
            "precondition: mtime forced equal — only the inode discriminator can fire"
        );

        assert!(
            ref_idx.is_stale(),
            "same-size rename-over (new inode, same mtime/size) must flip is_stale()"
        );
    }

    /// A WAL-mode commit with NO checkpoint leaves the reference's `index.db`
    /// identity (inode/size/mtime) unchanged, yet the cached index is stale.
    /// The `PRAGMA data_version` probe must catch it.
    ///
    /// Calibration: RED against EITHER `(mtime, size)`-only OR the
    /// `FileIdentity`-only key (neither moves on an uncheckpointed WAL commit).
    /// GREEN only with the long-lived data_version probe. Pins the
    /// WAL-incremental case — the real `cqs ref update` shape (in-place
    /// incremental reindex before the closing checkpoint).
    #[test]
    fn test_is_stale_detects_wal_commit_without_checkpoint() {
        use sqlx::{ConnectOptions, Connection};

        let (ref_idx, db_path) = make_disk_reference();
        // Baseline both discriminators.
        assert!(!ref_idx.is_stale(), "freshly loaded reference is not stale");

        let id_before = FileIdentity::from_path(&db_path).unwrap();

        // Second connection, WAL commit, NO checkpoint, kept open across the
        // assertions so closing the last writer can't auto-checkpoint and mask
        // the discriminator under test.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut writer = rt
            .block_on(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&db_path)
                    .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
                    .connect(),
            )
            .unwrap();
        rt.block_on(async {
            sqlx::query("CREATE TABLE IF NOT EXISTS wal_poke (x INTEGER)")
                .execute(&mut writer)
                .await?;
            sqlx::query("INSERT INTO wal_poke (x) VALUES (1)")
                .execute(&mut writer)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
        .unwrap();

        assert_eq!(
            FileIdentity::from_path(&db_path).unwrap(),
            id_before,
            "precondition: WAL commit must leave main-file identity unchanged"
        );

        assert!(
            ref_idx.is_stale(),
            "WAL commit with no checkpoint must flip is_stale() via data_version"
        );

        let _ = rt.block_on(writer.close());
    }
}
