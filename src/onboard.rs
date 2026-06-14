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
use crate::language::{ChunkType, Language};
use crate::parser::TypeEdgeKind;
use crate::store::Store;
use crate::{AnalysisError, Embedder};

/// Default callee BFS expansion depth.
pub const DEFAULT_ONBOARD_DEPTH: usize = 3;

/// Maximum callees to fetch content for. BFS may discover more, but we only
/// load content for the top entries by depth/score to cap memory usage.
/// Env override: `CQS_ONBOARD_CALLEE_FETCH`.
const MAX_CALLEE_FETCH_DEFAULT: usize = 30;

/// Maximum callers to fetch content for. Env override:
/// `CQS_ONBOARD_CALLER_FETCH`.
const MAX_CALLER_FETCH_DEFAULT: usize = 15;

/// Maximum key-type dependencies to render. The type-edge query plus the
/// `COMMON_TYPES` filter can still leave a long tail for a type-heavy entry
/// point; an unbounded list floods an agent's token budget. Env override:
/// `CQS_ONBOARD_KEY_TYPES`.
const MAX_KEY_TYPES_DEFAULT: usize = 50;

/// Ceiling on rows pulled from `get_types_used_by` for the key-types section.
/// The list is filtered by `COMMON_TYPES` after the fetch, so the SQL ceiling
/// is set above [`MAX_KEY_TYPES_DEFAULT`] to leave filtering headroom while
/// still bounding the query — a target touching thousands of type edges can't
/// pull them all.
const KEY_TYPES_FETCH_CEILING: usize = 200;

/// Resolve `CQS_ONBOARD_CALLEE_FETCH`, default 30. Parse/warn/default via
/// the shared `crate::limits::parse_env_usize` (warns on a malformed value).
fn max_callee_fetch() -> usize {
    crate::limits::parse_env_usize("CQS_ONBOARD_CALLEE_FETCH", MAX_CALLEE_FETCH_DEFAULT)
}

/// Resolve `CQS_ONBOARD_CALLER_FETCH`, default 15. Parse/warn/default via
/// the shared `crate::limits::parse_env_usize` (warns on a malformed value).
fn max_caller_fetch() -> usize {
    crate::limits::parse_env_usize("CQS_ONBOARD_CALLER_FETCH", MAX_CALLER_FETCH_DEFAULT)
}

/// Resolve `CQS_ONBOARD_KEY_TYPES`, default 50. Parse/warn/default via the
/// shared `crate::limits::parse_env_usize` (warns on a malformed value).
fn max_key_types() -> usize {
    crate::limits::parse_env_usize("CQS_ONBOARD_KEY_TYPES", MAX_KEY_TYPES_DEFAULT)
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
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line_start: u32,
    pub line_end: u32,
    pub language: Language,
    pub chunk_type: ChunkType,
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
    #[serde(serialize_with = "crate::serialize_path_normalized")]
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
    /// Callees discovered by BFS but dropped because they exceeded
    /// `CQS_ONBOARD_CALLEE_FETCH`. Zero when no truncation happened. Surfaces
    /// the cap to consumers so a user can lift it intentionally rather than
    /// silently wonder where their callees went.
    #[serde(default, skip_serializing_if = "crate::serde_helpers::is_zero_usize")]
    pub callees_truncated: usize,
    /// Callers truncated to `CQS_ONBOARD_CALLER_FETCH`. See
    /// `callees_truncated`.
    #[serde(default, skip_serializing_if = "crate::serde_helpers::is_zero_usize")]
    pub callers_truncated: usize,
    /// Key-type dependencies dropped because the filtered list exceeded
    /// `CQS_ONBOARD_KEY_TYPES`. Zero when no truncation happened. Pairs with
    /// the rendered `key_types` length so a consumer can recover the true
    /// total (`key_types.len() + key_types_truncated`) and never reads the
    /// capped list as complete.
    #[serde(default, skip_serializing_if = "crate::serde_helpers::is_zero_usize")]
    pub key_types_truncated: usize,
}

/// Produce a guided tour of a concept in the codebase.
///
/// Returns an ordered reading list: entry point → callees → callers → types → tests.
///
/// `direction` controls which side of the call graph gets the full BFS:
/// - `Callees` (default): follow what the entry point calls; callers walked at depth 1.
/// - `Callers`: follow who calls the entry point; callees walked at depth 1.
/// - `Both`: walk both sides at the requested `depth`.
pub fn onboard<Mode>(
    store: &Store<Mode>,
    embedder: &Embedder,
    concept: &str,
    root: &Path,
    depth: usize,
    direction: GatherDirection,
) -> Result<OnboardResult, AnalysisError> {
    let _span = tracing::info_span!("onboard", concept).entered();
    let depth = depth.min(10);
    // Per-side depths derived from the requested direction.
    // - Callees: full depth on callees, depth=1 on callers.
    // - Callers: depth=1 on callees, full depth on callers.
    // - Both: full depth on both sides.
    let (callee_depth, caller_depth) = match direction {
        GatherDirection::Callees => (depth, 1),
        GatherDirection::Callers => (1, depth),
        GatherDirection::Both => (depth, depth),
    };

    // 1. Search for relevant code (direct search, skip full scout overhead)
    let query_embedding = embedder.embed_query(concept)?;
    let filter = crate::store::SearchFilter {
        query_text: concept.to_string(),
        enable_rrf: false, // RRF off by default — pure cosine is faster + higher R@1 on expanded eval
        ..crate::store::SearchFilter::default()
    };
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
        .expect("results guaranteed non-empty by early return above");
    let entry_name = entry.chunk.name.clone();
    let entry_file = crate::relativize_or_warn(&entry.chunk.file, root);
    tracing::info!(entry_point = %entry_name, file = ?entry_file, "Selected entry point");

    // 3. Load shared resources
    let graph = store.get_call_graph()?;

    let test_chunks = match store.find_test_chunks() {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(error = %e, "Test chunk loading failed, skipping tests");
            std::sync::Arc::new(Vec::new())
        }
    };

    // 4. Callee BFS — follow what the entry point calls
    let mut callee_scores: HashMap<String, (f32, usize)> = HashMap::new();
    callee_scores.insert(entry_name.clone(), (1.0, 0));
    let callee_opts = GatherOptions::default()
        .with_expand_depth(callee_depth)
        .with_direction(GatherDirection::Callees)
        .with_decay_factor(0.7)
        .with_max_expanded_nodes(100);
    let _callee_capped = bfs_expand(&mut callee_scores, &graph, &callee_opts);

    // Remove entry point from callees (it's shown separately)
    callee_scores.remove(&entry_name);
    tracing::debug!(callee_count = callee_scores.len(), "Callee BFS complete");

    // 5. Caller BFS — who calls the entry point.
    // Depth is `caller_depth` (= 1 for direction=Callees,
    // = `depth` for Callers / Both).
    let mut caller_scores: HashMap<String, (f32, usize)> = HashMap::new();
    caller_scores.insert(entry_name.clone(), (1.0, 0));
    let caller_opts = GatherOptions::default()
        .with_expand_depth(caller_depth)
        .with_direction(GatherDirection::Callers)
        .with_decay_factor(0.8)
        .with_max_expanded_nodes(50);
    let _caller_capped = bfs_expand(&mut caller_scores, &graph, &caller_opts);

    // Remove entry point from callers
    caller_scores.remove(&entry_name);
    tracing::debug!(caller_count = caller_scores.len(), "Caller BFS complete");

    // 6. Cap score maps to avoid fetching content we'll discard.
    //    BFS may discover 100 callees, but we only load content for the top N.
    //    Env-overridable via CQS_ONBOARD_CALLEE_FETCH /
    //    CQS_ONBOARD_CALLER_FETCH. Track pre-cap counts so the caller can see
    //    truncation in `OnboardSummary`.
    let callee_fetch_cap = max_callee_fetch();
    let caller_fetch_cap = max_caller_fetch();
    let callees_truncated = callee_scores.len().saturating_sub(callee_fetch_cap);
    let callers_truncated = caller_scores.len().saturating_sub(caller_fetch_cap);
    if callees_truncated > 0 {
        tracing::warn!(
            dropped = callees_truncated,
            cap = callee_fetch_cap,
            "Onboard: callees truncated to CQS_ONBOARD_CALLEE_FETCH"
        );
    }
    if callers_truncated > 0 {
        tracing::warn!(
            dropped = callers_truncated,
            cap = caller_fetch_cap,
            "Onboard: callers truncated to CQS_ONBOARD_CALLER_FETCH"
        );
    }
    let callee_scores = cap_scores(callee_scores, callee_fetch_cap, |(_s, d)| *d);
    let caller_scores = cap_scores(caller_scores, caller_fetch_cap, |(score, _)| {
        // Keep highest scores: Reverse makes ascending sort = descending by score.
        let safe = if score.is_finite() && *score > 0.0 {
            *score
        } else {
            0.0
        };
        std::cmp::Reverse((safe * 1e6) as u64)
    });

    // 7. Fetch entry point — use search_by_name with exact match filter
    //    fetch_and_assemble's FTS can fuzzy-match "search" to "test_is_pipeable_search",
    //    so we do a direct lookup and prefer the exact name + file match from scout.
    let entry_point = fetch_entry_point(store, &entry_name, &entry_file, root)?;

    let (mut callee_chunks, _) = fetch_and_assemble(store, &callee_scores, root, None);
    // Sort by depth asc, then file/line within depth
    callee_chunks.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line_start.cmp(&b.line_start))
    });
    let call_chain: Vec<OnboardEntry> =
        callee_chunks.into_iter().map(gathered_to_onboard).collect();

    let (mut caller_chunks, _) = fetch_and_assemble(store, &caller_scores, root, None);
    // Sort callers by score desc. Secondary sort on (file, line_start, name)
    // keeps equal-score callers deterministically ordered across process
    // invocations.
    caller_chunks.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(a.file.cmp(&b.file))
            .then(a.line_start.cmp(&b.line_start))
            .then(a.name.cmp(&b.name))
    });
    let callers: Vec<OnboardEntry> = caller_chunks.into_iter().map(gathered_to_onboard).collect();

    // 7. Type dependencies — filter common types, then cap.
    // The SQL fetch is bounded by KEY_TYPES_FETCH_CEILING (the post-filter
    // `filter_common_types` discards most rows, so the ceiling sits above the
    // render cap for filtering headroom). The filtered list is then clipped to
    // `max_key_types()` so a type-heavy entry point can't flood the output;
    // `key_types_truncated` carries the dropped count to the summary.
    let key_types_cap = max_key_types();
    let key_types_all = match store.get_types_used_by(&entry_name, KEY_TYPES_FETCH_CEILING) {
        Ok(types) => filter_common_types(types),
        Err(e) => {
            tracing::warn!(error = %e, "Type dependency lookup failed, skipping key_types");
            Vec::new()
        }
    };
    let (key_types, key_types_truncated) = cap_key_types(key_types_all, key_types_cap);
    if key_types_truncated > 0 {
        tracing::warn!(
            dropped = key_types_truncated,
            cap = key_types_cap,
            "Onboard: key_types truncated to CQS_ONBOARD_KEY_TYPES"
        );
    }

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
        callees_truncated,
        callers_truncated,
        key_types_truncated,
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

// --- Internal helpers ---

/// Truncate a score map to `max` entries, keeping those with the lowest `key_fn` values.
fn cap_scores<F, K>(
    scores: HashMap<String, (f32, usize)>,
    max: usize,
    key_fn: F,
) -> HashMap<String, (f32, usize)>
where
    F: Fn(&(f32, usize)) -> K,
    K: Ord,
{
    if scores.len() <= max {
        return scores;
    }
    let mut entries: Vec<_> = scores.into_iter().collect();
    entries.sort_by_key(|a| key_fn(&a.1));
    entries.truncate(max);
    entries.into_iter().collect()
}

/// Clip the filtered key-types list to `cap`, returning the (possibly
/// truncated) list and the number of entries dropped. The fetch + filter
/// order is preserved (edge_kind, then type name) so the rendered window is
/// the deterministic front of the list. A `cap` of zero is treated as a clip
/// to zero with the full length reported as dropped.
fn cap_key_types(mut types: Vec<TypeInfo>, cap: usize) -> (Vec<TypeInfo>, usize) {
    let dropped = types.len().saturating_sub(cap);
    if dropped > 0 {
        types.truncate(cap);
    }
    (types, dropped)
}

/// Returns true for chunk types that have call graph connections.
fn is_callable_type(ct: ChunkType) -> bool {
    ct.is_callable()
}

/// Convert GatheredChunk to OnboardEntry.
fn gathered_to_onboard(c: GatheredChunk) -> OnboardEntry {
    OnboardEntry {
        name: c.name,
        file: c.file,
        line_start: c.line_start,
        line_end: c.line_end,
        language: c.language,
        chunk_type: c.chunk_type,
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
fn fetch_entry_point<Mode>(
    store: &Store<Mode>,
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
            a_file_match
                .cmp(&b_file_match)
                .then_with(|| a.score.total_cmp(&b.score))
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
            let rel_file = crate::relativize_or_warn(&r.chunk.file, root);
            Ok(OnboardEntry {
                name: r.chunk.name.clone(),
                file: rel_file,
                line_start: r.chunk.line_start,
                line_end: r.chunk.line_end,
                language: r.chunk.language,
                chunk_type: r.chunk.chunk_type,
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
        // filter_common_types uses the canonical HashSet from
        // focused_read.rs, which includes types like Error, Mutex, etc.
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

    /// Build `n` distinct `TypeInfo` rows for cap tests.
    fn make_key_types(n: usize) -> Vec<TypeInfo> {
        (0..n)
            .map(|i| TypeInfo {
                type_name: format!("Type{i}"),
                edge_kind: TypeEdgeKind::Param,
            })
            .collect()
    }

    /// An over-cap key-types list is clipped to the cap and reports the true
    /// dropped count (so the summary can recover total = cap + dropped).
    #[test]
    fn cap_key_types_clips_over_cap_and_reports_dropped() {
        let (clipped, dropped) = cap_key_types(make_key_types(120), 50);
        assert_eq!(clipped.len(), 50, "clipped to the cap");
        assert_eq!(dropped, 70, "true dropped count = 120 - 50");
        // Total is recoverable from the rendered length + dropped.
        assert_eq!(clipped.len() + dropped, 120);
        // The front of the deterministic order is preserved.
        assert_eq!(clipped[0].type_name, "Type0");
        assert_eq!(clipped[49].type_name, "Type49");
    }

    /// An under-cap list is returned unchanged with zero truncation signal.
    #[test]
    fn cap_key_types_leaves_under_cap_unchanged() {
        let (clipped, dropped) = cap_key_types(make_key_types(12), 50);
        assert_eq!(clipped.len(), 12, "no clip below the cap");
        assert_eq!(dropped, 0, "no truncation signal when under cap");
    }

    /// At exactly the cap there is no truncation — the boundary must not
    /// report a phantom drop of zero-but-truncated.
    #[test]
    fn cap_key_types_at_cap_boundary_no_truncation() {
        let (clipped, dropped) = cap_key_types(make_key_types(50), 50);
        assert_eq!(clipped.len(), 50);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn test_callee_ordering_by_depth() {
        use crate::parser::Language;

        // Verify that sort order is depth asc → file → line
        let mut chunks = [
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
                rank_signals: vec![],
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
                rank_signals: vec![],
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
