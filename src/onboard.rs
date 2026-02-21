//! Guided codebase tour — given a concept, produce an ordered reading list.
//!
//! Algorithm:
//! 1. Scout for relevant code
//! 2. Pick entry point (highest-scored ModifyTarget)
//! 3. BFS expand callees + callers
//! 4. Fetch type dependencies
//! 5. Find tests via reverse BFS
//! 6. Assemble ordered reading list

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::gather::{
    bfs_expand, fetch_and_assemble, GatherDirection, GatherOptions, GatheredChunk,
};
use crate::impact::{find_affected_tests_with_chunks, TestInfo, DEFAULT_MAX_TEST_SEARCH_DEPTH};
use crate::language::ChunkType;
use crate::parser::TypeEdgeKind;
use crate::store::Store;
use crate::{AnalysisError, Embedder};

/// Default callee BFS expansion depth.
pub const DEFAULT_ONBOARD_DEPTH: usize = 3;

fn serialize_path_forward_slash<S>(path: &std::path::Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy().replace('\\', "/"))
}

// Uses crate::COMMON_TYPES (from focused_read.rs) for type filtering — single source of truth.

/// Result of an onboard analysis — ordered reading list for understanding a concept.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardResult {
    pub concept: String,
    pub entry_point: OnboardEntry,
    pub call_chain: Vec<OnboardEntry>,
    pub callers: Vec<OnboardEntry>,
    pub key_types: Vec<TypeInfo>,
    pub tests: Vec<TestEntry>,
    pub summary: OnboardSummary,
}

/// A code entry in the reading list.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardEntry {
    pub name: String,
    #[serde(serialize_with = "serialize_path_forward_slash")]
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub language: String,
    pub chunk_type: String,
    pub signature: String,
    pub content: String,
    pub depth: usize,
}

/// Type dependency of the entry point.
#[derive(Debug, Clone, Serialize)]
pub struct TypeInfo {
    pub type_name: String,
    pub edge_kind: TypeEdgeKind,
}

/// Test that exercises the entry point.
#[derive(Debug, Clone, Serialize)]
pub struct TestEntry {
    pub name: String,
    #[serde(serialize_with = "serialize_path_forward_slash")]
    pub file: PathBuf,
    pub line: u32,
    pub call_depth: usize,
}

/// Summary statistics for the onboard result.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardSummary {
    pub total_items: usize,
    pub files_covered: usize,
    pub callee_depth: usize,
    pub tests_found: usize,
}

/// Produce a guided tour of a concept in the codebase.
///
/// Returns an ordered reading list: entry point → callees → callers → types → tests.
pub fn onboard(
    store: &Store,
    embedder: &Embedder,
    concept: &str,
    root: &Path,
    depth: usize,
) -> Result<OnboardResult, AnalysisError> {
    let _span = tracing::info_span!("onboard", concept).entered();
    let depth = depth.min(10);

    // 1. Search for relevant code (direct search, skip full scout overhead)
    let query_embedding = embedder
        .embed_query(concept)
        .map_err(|e| AnalysisError::Embedder(e.to_string()))?;
    let filter = crate::store::SearchFilter::default();
    let results = store.search_filtered(&query_embedding, &filter, 10, 0.0)?;

    if results.is_empty() {
        return Err(AnalysisError::NotFound(format!(
            "No relevant code found for concept: {concept}"
        )));
    }

    // 2. Pick entry point — prefer callable types (Function/Method) for call graph connections
    let entry = results
        .iter()
        .find(|r| is_callable_type(r.chunk.chunk_type))
        .or(results.first())
        .unwrap();
    let entry_name = entry.chunk.name.clone();
    let entry_file = entry
        .chunk
        .file
        .strip_prefix(root)
        .unwrap_or(&entry.chunk.file)
        .to_path_buf();
    tracing::info!(entry_point = %entry_name, file = ?entry_file, "Selected entry point");

    // 3. Load shared resources
    let graph = store.get_call_graph()?;

    let test_chunks = match store.find_test_chunks() {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(error = %e, "Test chunk loading failed, skipping tests");
            Vec::new()
        }
    };

    // 4. Callee BFS — follow what the entry point calls
    let mut callee_scores: HashMap<String, (f32, usize)> = HashMap::new();
    callee_scores.insert(entry_name.clone(), (1.0, 0));
    let callee_opts = GatherOptions::default()
        .with_expand_depth(depth)
        .with_direction(GatherDirection::Callees)
        .with_decay_factor(0.7)
        .with_max_expanded_nodes(100);
    let _callee_capped = bfs_expand(&mut callee_scores, &graph, &callee_opts);

    // Remove entry point from callees (it's shown separately)
    callee_scores.remove(&entry_name);
    tracing::debug!(callee_count = callee_scores.len(), "Callee BFS complete");

    // 5. Caller BFS — who calls the entry point (shallow, 1 level)
    let mut caller_scores: HashMap<String, (f32, usize)> = HashMap::new();
    caller_scores.insert(entry_name.clone(), (1.0, 0));
    let caller_opts = GatherOptions::default()
        .with_expand_depth(1)
        .with_direction(GatherDirection::Callers)
        .with_decay_factor(0.8)
        .with_max_expanded_nodes(50);
    let _caller_capped = bfs_expand(&mut caller_scores, &graph, &caller_opts);

    // Remove entry point from callers
    caller_scores.remove(&entry_name);
    tracing::debug!(caller_count = caller_scores.len(), "Caller BFS complete");

    // 6. Fetch entry point — use search_by_name with exact match filter
    //    fetch_and_assemble's FTS can fuzzy-match "search" to "test_is_pipeable_search",
    //    so we do a direct lookup and prefer the exact name + file match from scout.
    let entry_point = fetch_entry_point(store, &entry_name, &entry_file, root)?;

    let (mut callee_chunks, _) = fetch_and_assemble(store, &callee_scores, root);
    // Sort by depth asc, then file/line within depth
    callee_chunks.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    let call_chain: Vec<OnboardEntry> =
        callee_chunks.into_iter().map(gathered_to_onboard).collect();

    let (mut caller_chunks, _) = fetch_and_assemble(store, &caller_scores, root);
    // Sort callers by score desc
    caller_chunks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let callers: Vec<OnboardEntry> = caller_chunks.into_iter().map(gathered_to_onboard).collect();

    // 7. Type dependencies — filter common types
    let key_types = match store.get_types_used_by(&entry_name) {
        Ok(types) => filter_common_types(types),
        Err(e) => {
            tracing::warn!(error = %e, "Type dependency lookup failed, skipping key_types");
            Vec::new()
        }
    };

    // 8. Tests via reverse BFS
    let tests: Vec<TestEntry> = find_affected_tests_with_chunks(
        &graph,
        &test_chunks,
        &entry_name,
        DEFAULT_MAX_TEST_SEARCH_DEPTH,
    )
    .into_iter()
    .map(test_info_to_entry)
    .collect();

    // 9. Build summary
    let mut all_files: std::collections::HashSet<&Path> = std::collections::HashSet::new();
    all_files.insert(&entry_point.file);
    for c in &call_chain {
        all_files.insert(&c.file);
    }
    for c in &callers {
        all_files.insert(&c.file);
    }

    let max_callee_depth = call_chain.iter().map(|c| c.depth).max().unwrap_or(0);

    let summary = OnboardSummary {
        total_items: 1 + call_chain.len() + callers.len() + key_types.len() + tests.len(),
        files_covered: all_files.len(),
        callee_depth: max_callee_depth,
        tests_found: tests.len(),
    };

    tracing::info!(
        callees = call_chain.len(),
        callers = callers.len(),
        types = key_types.len(),
        tests = tests.len(),
        "Onboard complete"
    );

    Ok(OnboardResult {
        concept: concept.to_string(),
        entry_point,
        call_chain,
        callers,
        key_types,
        tests,
        summary,
    })
}

/// Convert OnboardResult to JSON.
pub fn onboard_to_json(result: &OnboardResult) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::to_value(result)
}

// --- Internal helpers ---

/// Returns true for chunk types that have call graph connections (Function, Method).
fn is_callable_type(ct: ChunkType) -> bool {
    matches!(ct, ChunkType::Function | ChunkType::Method)
}

/// Convert GatheredChunk to OnboardEntry.
fn gathered_to_onboard(c: GatheredChunk) -> OnboardEntry {
    OnboardEntry {
        name: c.name,
        file: c.file,
        line_start: c.line_start,
        line_end: c.line_end,
        language: c.language.to_string(),
        chunk_type: c.chunk_type.to_string(),
        signature: c.signature,
        content: c.content,
        depth: c.depth,
    }
}

/// Fetch the entry point chunk with exact name matching.
///
/// `fetch_and_assemble` uses FTS which can fuzzy-match (e.g., "search" → "test_is_pipeable_search").
/// This function does a direct `search_by_name` with multiple results, then picks the one
/// with an exact name match, preferring the file from scout.
fn fetch_entry_point(
    store: &Store,
    entry_name: &str,
    entry_file: &Path,
    root: &Path,
) -> Result<OnboardEntry, AnalysisError> {
    let results = store.search_by_name(entry_name, 10)?;

    // Prefer exact name match from the expected file
    let best = results
        .iter()
        .filter(|r| r.chunk.name == entry_name)
        .max_by(|a, b| {
            // Prefer match from scout's file
            let a_file_match = a.chunk.file.ends_with(entry_file);
            let b_file_match = b.chunk.file.ends_with(entry_file);
            a_file_match.cmp(&b_file_match).then_with(|| {
                a.score
                    .partial_cmp(&b.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        })
        .or_else(|| {
            // Fallback: any result from the expected file
            results.iter().find(|r| r.chunk.file.ends_with(entry_file))
        })
        .or_else(|| {
            // Last resort: highest-scored result
            results.first()
        });

    match best {
        Some(r) => {
            let rel_file = r
                .chunk
                .file
                .strip_prefix(root)
                .unwrap_or(&r.chunk.file)
                .to_path_buf();
            Ok(OnboardEntry {
                name: r.chunk.name.clone(),
                file: rel_file,
                line_start: r.chunk.line_start,
                line_end: r.chunk.line_end,
                language: r.chunk.language.to_string(),
                chunk_type: r.chunk.chunk_type.to_string(),
                signature: r.chunk.signature.clone(),
                content: r.chunk.content.clone(),
                depth: 0,
            })
        }
        None => Err(AnalysisError::NotFound(format!(
            "Entry point '{entry_name}' not found in index"
        ))),
    }
}

/// Filter common types from type dependency results.
///
/// Uses `crate::COMMON_TYPES` (from focused_read.rs) — the canonical 44-entry HashSet.
fn filter_common_types(types: Vec<crate::store::TypeUsage>) -> Vec<TypeInfo> {
    types
        .into_iter()
        .filter(|t| !crate::COMMON_TYPES.contains(t.type_name.as_str()))
        .map(|t| TypeInfo {
            type_name: t.type_name,
            edge_kind: t
                .edge_kind
                .parse::<TypeEdgeKind>()
                .unwrap_or(TypeEdgeKind::Param),
        })
        .collect()
}

/// Convert impact TestInfo to onboard TestEntry.
fn test_info_to_entry(t: TestInfo) -> TestEntry {
    TestEntry {
        name: t.name,
        file: t.file,
        line: t.line,
        call_depth: t.call_depth,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_common_types_filtered() {
        use crate::store::TypeUsage;
        let types = vec![
            TypeUsage {
                type_name: "String".to_string(),
                edge_kind: "Param".to_string(),
            },
            TypeUsage {
                type_name: "Vec".to_string(),
                edge_kind: "Return".to_string(),
            },
            TypeUsage {
                type_name: "Store".to_string(),
                edge_kind: "Param".to_string(),
            },
            TypeUsage {
                type_name: "Option".to_string(),
                edge_kind: "Return".to_string(),
            },
        ];
        let filtered = filter_common_types(types);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].type_name, "Store");
    }

    #[test]
    fn test_common_types_canonical_set_filters_more() {
        use crate::store::TypeUsage;
        // Verify that filter_common_types now uses the canonical 44-entry HashSet
        // from focused_read.rs, which includes types like Error, Mutex, etc.
        // that the old 22-entry local array missed.
        let types = vec![
            TypeUsage {
                type_name: "Error".to_string(),
                edge_kind: "Return".to_string(),
            },
            TypeUsage {
                type_name: "Mutex".to_string(),
                edge_kind: "Field".to_string(),
            },
            TypeUsage {
                type_name: "Debug".to_string(),
                edge_kind: "Bound".to_string(),
            },
            TypeUsage {
                type_name: "Store".to_string(),
                edge_kind: "Param".to_string(),
            },
        ];
        let filtered = filter_common_types(types);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].type_name, "Store");
    }

    #[test]
    fn test_uncommon_types_kept() {
        use crate::store::TypeUsage;
        let types = vec![
            TypeUsage {
                type_name: "Embedder".to_string(),
                edge_kind: "Param".to_string(),
            },
            TypeUsage {
                type_name: "CallGraph".to_string(),
                edge_kind: "Field".to_string(),
            },
            TypeUsage {
                type_name: "SearchFilter".to_string(),
                edge_kind: "Param".to_string(),
            },
        ];
        let filtered = filter_common_types(types);
        assert_eq!(filtered.len(), 3);
    }

    #[test]
    fn test_callee_ordering_by_depth() {
        use crate::parser::Language;

        // Verify that sort order is depth asc → file → line
        let mut chunks = vec![
            GatheredChunk {
                name: "deep".into(),
                file: PathBuf::from("a.rs"),
                line_start: 1,
                line_end: 10,
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                signature: String::new(),
                content: String::new(),
                score: 0.5,
                depth: 2,
                source: None,
            },
            GatheredChunk {
                name: "shallow".into(),
                file: PathBuf::from("b.rs"),
                line_start: 1,
                line_end: 10,
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                signature: String::new(),
                content: String::new(),
                score: 0.3,
                depth: 1,
                source: None,
            },
        ];
        chunks.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line_start.cmp(&b.line_start))
        });
        assert_eq!(chunks[0].name, "shallow"); // depth 1 before depth 2
        assert_eq!(chunks[1].name, "deep");
    }

    #[test]
    fn test_entry_point_excluded_from_call_chain() {
        // Verify that removing entry point from callee_scores works
        let mut scores: HashMap<String, (f32, usize)> = HashMap::new();
        scores.insert("entry".into(), (1.0, 0));
        scores.insert("callee_a".into(), (0.7, 1));
        scores.insert("callee_b".into(), (0.5, 2));

        scores.remove("entry");
        assert_eq!(scores.len(), 2);
        assert!(!scores.contains_key("entry"));
    }

    #[test]
    fn test_test_info_to_entry() {
        let info = TestInfo {
            name: "test_foo".into(),
            file: PathBuf::from("tests/foo.rs"),
            line: 10,
            call_depth: 2,
        };
        let entry = test_info_to_entry(info);
        assert_eq!(entry.name, "test_foo");
        assert_eq!(entry.call_depth, 2);
    }
}
