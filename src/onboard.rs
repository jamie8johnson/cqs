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
use crate::scout::{ChunkRole, ScoutResult};
use crate::store::Store;
use crate::{scout, AnalysisError, Embedder};

/// Default callee BFS expansion depth.
pub const DEFAULT_ONBOARD_DEPTH: usize = 3;

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
    pub edge_kind: String,
}

/// Test that exercises the entry point.
#[derive(Debug, Clone, Serialize)]
pub struct TestEntry {
    pub name: String,
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

    // 1. Scout for relevant code
    let scout_result = scout(store, embedder, concept, root, 10)?;
    tracing::debug!(
        file_groups = scout_result.file_groups.len(),
        "Scout completed"
    );

    if scout_result.file_groups.is_empty() {
        return Err(AnalysisError::NotFound(format!(
            "No relevant code found for concept: {concept}"
        )));
    }

    // 2. Pick entry point — first ModifyTarget, fallback to highest-scored chunk
    let (entry_name, entry_file) = pick_entry_point(&scout_result);
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
        total_items: 1 + call_chain.len() + callers.len() + tests.len(),
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
pub fn onboard_to_json(result: &OnboardResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_default()
}

// --- Internal helpers ---

/// Pick the best entry point from scout results.
///
/// Prefers the first ModifyTarget across all file groups (sorted by relevance).
/// Falls back to the highest-scored callable chunk (Function/Method), then any chunk.
fn pick_entry_point(scout_result: &ScoutResult) -> (String, PathBuf) {
    // Try ModifyTarget first — prefer callable types even here
    let mut best_modify_callable: Option<(f32, String, PathBuf)> = None;
    let mut best_modify_any: Option<(f32, String, PathBuf)> = None;
    for group in &scout_result.file_groups {
        for chunk in &group.chunks {
            if chunk.role == ChunkRole::ModifyTarget {
                let entry = (chunk.search_score, chunk.name.clone(), group.file.clone());
                if is_callable_type(chunk.chunk_type) {
                    if best_modify_callable
                        .as_ref()
                        .is_none_or(|(s, _, _)| *s < chunk.search_score)
                    {
                        best_modify_callable = Some(entry);
                    }
                } else if best_modify_any
                    .as_ref()
                    .is_none_or(|(s, _, _)| *s < chunk.search_score)
                {
                    best_modify_any = Some(entry);
                }
            }
        }
    }
    if let Some((_, name, file)) = best_modify_callable.or(best_modify_any) {
        return (name, file);
    }

    // Fallback: prefer callable types (Function/Method) — they have call graph connections
    tracing::warn!("No ModifyTarget found, using highest-scored chunk as entry point");
    let mut best_callable: Option<(f32, String, PathBuf)> = None;
    let mut best_any: Option<(f32, String, PathBuf)> = None;
    for group in &scout_result.file_groups {
        for chunk in &group.chunks {
            if chunk.role == ChunkRole::TestToUpdate {
                continue; // skip tests as entry points
            }
            let entry = (chunk.search_score, chunk.name.clone(), group.file.clone());
            if is_callable_type(chunk.chunk_type) {
                if best_callable
                    .as_ref()
                    .is_none_or(|(s, _, _)| *s < chunk.search_score)
                {
                    best_callable = Some(entry);
                }
            } else if best_any
                .as_ref()
                .is_none_or(|(s, _, _)| *s < chunk.search_score)
            {
                best_any = Some(entry);
            }
        }
    }

    best_callable
        .or(best_any)
        .map(|(_, name, file)| (name, file))
        .unwrap_or_else(|| ("unknown".to_string(), PathBuf::new()))
}

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
        chunk_type: format!("{:?}", c.chunk_type),
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
                chunk_type: format!("{:?}", r.chunk.chunk_type),
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
fn filter_common_types(types: Vec<(String, String)>) -> Vec<TypeInfo> {
    types
        .into_iter()
        .filter(|(name, _)| !crate::COMMON_TYPES.contains(name.as_str()))
        .map(|(type_name, edge_kind)| TypeInfo {
            type_name,
            edge_kind,
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
    use crate::scout::{ChunkRole, FileGroup, ScoutChunk, ScoutResult, ScoutSummary};
    use std::path::PathBuf;

    fn make_scout_chunk(name: &str, role: ChunkRole, score: f32) -> ScoutChunk {
        make_scout_chunk_typed(name, role, score, ChunkType::Function)
    }

    fn make_scout_chunk_typed(
        name: &str,
        role: ChunkRole,
        score: f32,
        chunk_type: ChunkType,
    ) -> ScoutChunk {
        ScoutChunk {
            name: name.to_string(),
            chunk_type,
            signature: format!("fn {name}()"),
            line_start: 1,
            role,
            caller_count: 0,
            test_count: 0,
            search_score: score,
        }
    }

    fn make_file_group(file: &str, chunks: Vec<ScoutChunk>) -> FileGroup {
        let relevance = chunks.iter().map(|c| c.search_score).sum::<f32>() / chunks.len() as f32;
        FileGroup {
            file: PathBuf::from(file),
            relevance_score: relevance,
            chunks,
            is_stale: false,
        }
    }

    fn make_scout_result(file_groups: Vec<FileGroup>) -> ScoutResult {
        let total_functions = file_groups.iter().map(|g| g.chunks.len()).sum();
        ScoutResult {
            file_groups,
            relevant_notes: Vec::new(),
            summary: ScoutSummary {
                total_files: 1,
                total_functions,
                untested_count: 0,
                stale_count: 0,
            },
        }
    }

    #[test]
    fn test_entry_point_prefers_modify_target() {
        let result = make_scout_result(vec![make_file_group(
            "src/foo.rs",
            vec![
                make_scout_chunk("dependency_fn", ChunkRole::Dependency, 0.8),
                make_scout_chunk("target_fn", ChunkRole::ModifyTarget, 0.6),
            ],
        )]);
        let (name, _) = pick_entry_point(&result);
        assert_eq!(name, "target_fn"); // ModifyTarget wins despite lower score
    }

    #[test]
    fn test_entry_point_fallback_no_modify_target() {
        let result = make_scout_result(vec![make_file_group(
            "src/bar.rs",
            vec![
                make_scout_chunk("low_fn", ChunkRole::Dependency, 0.3),
                make_scout_chunk("high_fn", ChunkRole::Dependency, 0.9),
            ],
        )]);
        let (name, _) = pick_entry_point(&result);
        assert_eq!(name, "high_fn"); // Highest score wins as fallback
    }

    #[test]
    fn test_entry_point_from_multiple_files() {
        let result = make_scout_result(vec![
            make_file_group(
                "src/first.rs",
                vec![make_scout_chunk("dep", ChunkRole::Dependency, 0.9)],
            ),
            make_file_group(
                "src/second.rs",
                vec![make_scout_chunk("target", ChunkRole::ModifyTarget, 0.5)],
            ),
        ]);
        let (name, file) = pick_entry_point(&result);
        assert_eq!(name, "target"); // ModifyTarget from second file picked
        assert_eq!(file, PathBuf::from("src/second.rs"));
    }

    #[test]
    fn test_entry_point_prefers_callable_over_struct() {
        // Struct has higher score but Function should be preferred (call graph connections)
        let result = make_scout_result(vec![make_file_group(
            "src/search.rs",
            vec![
                make_scout_chunk_typed("MyStruct", ChunkRole::Dependency, 0.4, ChunkType::Struct),
                make_scout_chunk_typed(
                    "search_fn",
                    ChunkRole::Dependency,
                    0.3,
                    ChunkType::Function,
                ),
            ],
        )]);
        let (name, _) = pick_entry_point(&result);
        assert_eq!(name, "search_fn"); // Function preferred over struct
    }

    #[test]
    fn test_entry_point_fallback_to_struct_when_no_callable() {
        // When no Function/Method exists, fall back to struct
        let result = make_scout_result(vec![make_file_group(
            "src/types.rs",
            vec![
                make_scout_chunk_typed("MyEnum", ChunkRole::Dependency, 0.3, ChunkType::Enum),
                make_scout_chunk_typed("MyStruct", ChunkRole::Dependency, 0.4, ChunkType::Struct),
            ],
        )]);
        let (name, _) = pick_entry_point(&result);
        assert_eq!(name, "MyStruct"); // Highest-scored non-callable
    }

    #[test]
    fn test_entry_point_skips_tests() {
        // Tests should not be chosen as entry points
        let result = make_scout_result(vec![make_file_group(
            "src/lib.rs",
            vec![
                make_scout_chunk("test_something", ChunkRole::TestToUpdate, 0.9),
                make_scout_chunk("real_fn", ChunkRole::Dependency, 0.2),
            ],
        )]);
        let (name, _) = pick_entry_point(&result);
        assert_eq!(name, "real_fn"); // Test skipped, real function chosen
    }

    #[test]
    fn test_common_types_filtered() {
        let types = vec![
            ("String".to_string(), "Param".to_string()),
            ("Vec".to_string(), "Return".to_string()),
            ("Store".to_string(), "Param".to_string()),
            ("Option".to_string(), "Return".to_string()),
        ];
        let filtered = filter_common_types(types);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].type_name, "Store");
    }

    #[test]
    fn test_common_types_canonical_set_filters_more() {
        // Verify that filter_common_types now uses the canonical 44-entry HashSet
        // from focused_read.rs, which includes types like Error, Mutex, etc.
        // that the old 22-entry local array missed.
        let types = vec![
            ("Error".to_string(), "Return".to_string()),
            ("Mutex".to_string(), "Field".to_string()),
            ("Debug".to_string(), "Bound".to_string()),
            ("Store".to_string(), "Param".to_string()),
        ];
        let filtered = filter_common_types(types);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].type_name, "Store");
    }

    #[test]
    fn test_uncommon_types_kept() {
        let types = vec![
            ("Embedder".to_string(), "Param".to_string()),
            ("CallGraph".to_string(), "Field".to_string()),
            ("SearchFilter".to_string(), "Param".to_string()),
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
