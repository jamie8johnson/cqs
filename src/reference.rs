//! Reference index support for multi-index search
//!
//! A reference index is a standard cqs index (SQLite DB + HNSW files) created
//! from an external codebase. References are read-only during search. Results
//! from references have their scores multiplied by a weight (default 0.8) to
//! rank them below equally-similar project results.

use rayon::prelude::*;

use crate::config::ReferenceConfig;
use crate::hnsw::HnswIndex;
use crate::index::VectorIndex;
use crate::store::{ReadOnly, SearchFilter, SearchResult, Store, StoreError, UnifiedResult};
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
    /// typestate (#946) turns any accidental write into a compile error.
    pub store: Store<ReadOnly>,
    /// Optional HNSW index for O(log n) search
    pub index: Option<Box<dyn VectorIndex>>,
    /// Score multiplier (0.0-1.0)
    pub weight: f32,
    /// Path to the reference's `index.db` — kept so staleness checks can
    /// re-stat the file without reconstructing the path from `name + path`.
    pub db_path: std::path::PathBuf,
    /// RM-V1.25-7: (mtime, size) of `index.db` at load time. Long-lived
    /// daemons that cache loaded references need to invalidate them when
    /// the reference's own `cqs ref update <name>` rewrites the DB. The
    /// primary project's mtime change wouldn't catch this, so each
    /// cached reference tracks its own identity.
    pub loaded_identity: Option<(std::time::SystemTime, u64)>,
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

    // SEC-4: Warn if reference path is outside project and home directories.
    // Canonicalize to resolve any `..` segments, then check containment.
    if let Ok(canonical) = cfg.path.canonicalize() {
        let home = dirs::home_dir();
        let cwd = std::env::current_dir().ok();
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

    let db_path = cfg.path.join("index.db");
    let store = match Store::open_readonly(&db_path) {
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

    // v1.22.0 audit CQ-5: previously passed `dim=None` which defaulted to
    // `EMBEDDING_DIM` (1024 for BGE-large). A reference built with a 768-dim
    // model (E5-base or v9-200k) would load as 1024-dim garbage. Pass the
    // store's actual dim so the HNSW loader trusts the right byte layout.
    let index = HnswIndex::try_load_with_ef(&cfg.path, None, store.dim());

    // RM-V1.25-7: capture (mtime, size) at load time so cached references
    // can be invalidated when the reference itself is re-indexed. `None`
    // on stat failure is fine — `is_stale` treats that as "can't tell,
    // keep using it" rather than thrashing the cache on transient errors.
    let loaded_identity = stat_identity(&db_path);

    Some(ReferenceIndex {
        name: cfg.name.clone(),
        store,
        index,
        weight: cfg.weight,
        db_path,
        loaded_identity,
    })
}

/// Stat `path` and return `(mtime, size)` if readable. Used for RM-V1.25-7
/// reference staleness detection. Errors are logged at debug and treated as
/// "unknown" (caller falls back to keeping the cached value).
fn stat_identity(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    match std::fs::metadata(path) {
        Ok(md) => match md.modified() {
            Ok(t) => Some((t, md.len())),
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "metadata.modified() failed");
                None
            }
        },
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "stat failed");
            None
        }
    }
}

impl ReferenceIndex {
    /// RM-V1.25-7: Has the reference's `index.db` been rewritten since load?
    ///
    /// Returns `true` when the current (mtime, size) differs from the value
    /// captured at construction. If we couldn't read the file at all (or
    /// had no identity at load time), returns `false` so the caller keeps
    /// using the cached index — transient NFS/permission glitches shouldn't
    /// thrash the LRU.
    pub fn is_stale(&self) -> bool {
        let Some(loaded) = self.loaded_identity else {
            return false;
        };
        let Some(current) = stat_identity(&self.db_path) else {
            return false;
        };
        current != loaded
    }
}

/// Load reference indexes from config, skipping any that fail to open.
///
/// References are loaded in parallel via rayon — each Store::open_readonly +
/// HnswIndex::try_load is independent I/O (10-50ms each). Both Store and
/// HnswIndex are Send + Sync.
pub fn load_references(configs: &[ReferenceConfig]) -> Vec<ReferenceIndex> {
    let _span = tracing::debug_span!("load_references", count = configs.len()).entered();
    // RM-29: Cap concurrency — each ref loads Store (~64MB) + HNSW (~50-200MB)
    let threads = std::env::var("CQS_RAYON_THREADS")
        .ok()
        .and_then(|v| {
            let parsed = v.parse();
            if parsed.is_err() {
                tracing::warn!(value = %v, "Invalid CQS_RAYON_THREADS, using default");
            }
            parsed.ok()
        })
        .unwrap_or(4);
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
        tracing::info!("Loaded {} reference indexes", refs.len());
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
    let mut results = ref_idx.store.search_filtered_with_index(
        query_embedding,
        filter,
        limit,
        threshold,
        ref_idx.index.as_deref(),
    )?;
    if apply_weight {
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
        // Re-filter after weight: results that passed raw threshold may fall
        // below after weighting (consistent with name_only path)
        results.retain(|r| r.score >= threshold);
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
    let mut results = ref_idx.store.search_by_name(name, limit)?;
    if apply_weight {
        results.retain(|r| r.score * ref_idx.weight >= threshold);
        for r in &mut results {
            r.score *= ref_idx.weight;
        }
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
    // PF-4: Use stored content_hash when available instead of recomputing blake3.
    let mut seen_hashes = std::collections::HashSet::new();
    tagged.retain(|t| match &t.result {
        UnifiedResult::Code(r) => {
            if r.chunk.content_hash.is_empty() {
                // Fallback for test data or legacy chunks without stored hash
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
        SearchResult {
            chunk: ChunkSummary {
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
            },
            score,
        }
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
        let primary = vec![UnifiedResult::Code(SearchResult {
            chunk: ChunkSummary {
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
            },
            score: 0.9,
        })];
        let refs = vec![(
            "ref1".to_string(),
            vec![SearchResult {
                chunk: ChunkSummary {
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
                },
                score: 0.7,
            }],
        )];

        let merged = merge_results(primary, refs, 10);
        // Should have 1 result (deduped), not 2
        assert_eq!(merged.len(), 1);
        // Kept the highest-scoring one (primary, 0.9)
        assert!(merged[0].source.is_none());
        assert!((merged[0].result.score() - 0.9).abs() < 0.01);
    }

    #[test]
    fn test_ref_path_rejects_traversal() {
        assert!(ref_path("../etc").is_none());
        assert!(ref_path("foo/bar").is_none());
    }
}
