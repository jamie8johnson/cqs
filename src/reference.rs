//! Reference index support for multi-index search
//!
//! A reference index is a standard cqs index (SQLite DB + HNSW files) created
//! from an external codebase. References are read-only during search. Results
//! from references have their scores multiplied by a weight (default 0.8) to
//! rank them below equally-similar project results.

use crate::config::ReferenceConfig;
use crate::hnsw::HnswIndex;
use crate::index::VectorIndex;
use crate::store::{SearchFilter, SearchResult, Store, UnifiedResult};
use crate::Embedding;

/// A loaded reference index ready for searching
pub struct ReferenceIndex {
    /// Display name
    pub name: String,
    /// The reference's store (separate DB + connection pool)
    pub store: Store,
    /// Optional HNSW index for O(log n) search
    pub index: Option<Box<dyn VectorIndex>>,
    /// Score multiplier (0.0-1.0)
    pub weight: f32,
}

/// A search result tagged with its source
#[derive(Debug)]
pub struct TaggedResult {
    /// The underlying search result
    pub result: UnifiedResult,
    /// Source: None = primary project, Some(name) = reference
    pub source: Option<String>,
}

/// Load reference indexes from config, skipping any that fail to open
pub fn load_references(configs: &[ReferenceConfig]) -> Vec<ReferenceIndex> {
    let mut refs = Vec::with_capacity(configs.len());

    for cfg in configs {
        // Warn if reference path is a symlink (trust boundary)
        if cfg
            .path
            .symlink_metadata()
            .map(|m| m.is_symlink())
            .unwrap_or(false)
        {
            tracing::warn!(
                name = cfg.name,
                path = %cfg.path.display(),
                "Reference path is a symlink — verify it points to a trusted location"
            );
        }

        let db_path = cfg.path.join("index.db");
        let store = match Store::open(&db_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Skipping reference '{}': failed to open {}: {}",
                    cfg.name,
                    db_path.display(),
                    e
                );
                continue;
            }
        };

        let index = HnswIndex::try_load(&cfg.path);

        refs.push(ReferenceIndex {
            name: cfg.name.clone(),
            store,
            index,
            weight: cfg.weight,
        });
    }

    if !refs.is_empty() {
        tracing::info!("Loaded {} reference indexes", refs.len());
    }

    refs
}

/// Search a single reference index, applying weight multiplier to scores
pub fn search_reference(
    ref_idx: &ReferenceIndex,
    query_embedding: &Embedding,
    filter: &SearchFilter,
    limit: usize,
    threshold: f32,
) -> anyhow::Result<Vec<SearchResult>> {
    let _span =
        tracing::info_span!("search_reference", name = %ref_idx.name, weight = ref_idx.weight)
            .entered();
    let mut results = ref_idx.store.search_filtered_with_index(
        query_embedding,
        filter,
        limit,
        threshold,
        ref_idx.index.as_deref(),
    )?;
    for r in &mut results {
        r.score *= ref_idx.weight;
    }
    // Re-filter after weight: results that passed raw threshold may fall
    // below after weighting (consistent with name_only path)
    results.retain(|r| r.score >= threshold);
    Ok(results)
}

/// Search a reference by name (for name_only mode), applying weight
pub fn search_reference_by_name(
    ref_idx: &ReferenceIndex,
    name: &str,
    limit: usize,
    threshold: f32,
) -> anyhow::Result<Vec<SearchResult>> {
    let _span =
        tracing::info_span!("search_reference_by_name", ref_name = %ref_idx.name, query = name)
            .entered();
    let mut results = ref_idx.store.search_by_name(name, limit)?;
    results.retain(|r| r.score * ref_idx.weight >= threshold);
    for r in &mut results {
        r.score *= ref_idx.weight;
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

    // Sort by score descending (highest first)
    tagged.sort_by(|a, b| {
        b.result
            .score()
            .partial_cmp(&a.result.score())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Deduplicate code results by content hash (keeps highest-scoring occurrence).
    // Notes are never deduplicated — they're project-local and unique.
    let mut seen_hashes = std::collections::HashSet::new();
    tagged.retain(|t| match &t.result {
        UnifiedResult::Code(r) => {
            let hash = blake3::hash(r.chunk.content.as_bytes());
            seen_hashes.insert(hash)
        }
        UnifiedResult::Note(_) => true,
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
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err("Reference name cannot contain '/', '\\', or '..'");
    }
    if name == "." {
        return Err("Reference name cannot be '.'");
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
    use crate::store::{ChunkSummary, NoteSearchResult, NoteSummary};

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
            },
            score,
        }
    }

    fn make_note_result(text: &str, score: f32) -> NoteSearchResult {
        NoteSearchResult {
            note: NoteSummary {
                id: String::new(),
                text: text.to_string(),
                sentiment: 0.0,
                mentions: vec![],
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
    fn test_merge_results_with_notes() {
        let primary = vec![
            UnifiedResult::Code(make_code_result("fn_a", 0.9)),
            UnifiedResult::Note(make_note_result("a note", 0.85)),
        ];
        let refs = vec![("tokio".to_string(), vec![make_code_result("spawn", 0.88)])];

        let merged = merge_results(primary, refs, 10);
        assert_eq!(merged.len(), 3);
        // Sorted: 0.9 (code), 0.88 (ref), 0.85 (note)
        assert!(merged[0].result.score() >= merged[1].result.score());
        assert!(merged[1].result.score() >= merged[2].result.score());
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
    fn test_merge_dedup_keeps_notes() {
        // Notes should never be deduplicated, even with same score
        let primary = vec![
            UnifiedResult::Note(make_note_result("a note", 0.85)),
            UnifiedResult::Note(make_note_result("a note", 0.85)), // duplicate note text
        ];
        let refs: Vec<(String, Vec<SearchResult>)> = vec![];

        let merged = merge_results(primary, refs, 10);
        // Both notes should be kept (notes are never deduped)
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn test_ref_path_rejects_traversal() {
        assert!(ref_path("../etc").is_none());
        assert!(ref_path("foo/bar").is_none());
    }
}
