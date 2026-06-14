//! Call graph storage and queries
//!
//! Split into submodules by concern:
//! - `crud` - upsert, delete, batch operations, basic stats
//! - `query` - callers, callees, call graph, context queries
//! - `dead_code` - dead code detection with confidence scoring
//! - `test_map` - test chunk discovery, pruning
//! - `related` - batch counts, shared callers/callees, co-occurrence

pub mod cross_project;
mod crud;
mod dead_code;
mod query;
mod related;
mod test_map;

// Re-export the in-tx function_calls writer so
// `store::chunks::crud::upsert_chunks_calls_and_prune` can fold the
// file-level call-graph write into the same per-file transaction.
pub(crate) use crud::write_function_calls_in_tx;

// Re-export the in-tx candidate_edges writer (the call-graph candidate
// side-table) so the same per-file fused write can fold it in alongside
// function_calls.
pub(crate) use crud::write_candidate_edges_in_tx;

// Re-export the doc-shaped-origin predicate so the worktree-overlay dead path
// (in the binary crate) can apply the same doc-path admissibility the
// dead-candidate SQL applies — single source for both views.
pub use dead_code::is_dead_doc_path;

// Re-export the worktree-overlay dead-set merge so both the `cqs dead` CLI core
// and the lib-level `cqs ci` analysis drive the same Direction A/B merge over the
// merged caller graph — single source, no surface drift.
pub use dead_code::apply_dead_overlay;

use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;

use super::helpers::ChunkSummary;
use crate::parser::{ChunkType, Language};

/// A dead function with confidence scoring.
/// Wraps a `ChunkSummary` with a confidence level indicating how likely
/// the function is truly dead (not just invisible to static analysis).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeadFunction {
    /// The code chunk (function/method metadata + content)
    pub chunk: ChunkSummary,
    /// How confident we are that this function is dead
    pub confidence: DeadConfidence,
    /// Set only by `apply_dead_overlay` Direction B: this entry was computed
    /// dead over the authoritative merged (parent+overlay) caller graph in this
    /// worktree, not read off the parent dead set. Verdict classification must
    /// not relabel it via the parent-truth `low_conf` heuristic-caller map — that
    /// map is parent-graph-derived and stale under the overlay, so a
    /// genuinely-worktree-dead function whose name collides with a parent
    /// candidate/heuristic name would otherwise be hidden from `--verdict dead`.
    /// Internal-only; entries reach user-facing JSON as `DeadFunctionEntry`.
    #[serde(skip)]
    pub overlay_dead: bool,
}

/// Confidence level for dead code detection.
/// Ordered from least to most confident, enabling `>=` filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, clap::ValueEnum)]
pub enum DeadConfidence {
    /// Likely a false positive (methods, functions in active files)
    Low,
    /// Possibly dead but uncertain (private functions in active files)
    Medium,
    /// Almost certainly dead (private, in files with no callers)
    High,
}

impl DeadConfidence {
    /// Stable string representation for display and JSON serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeadConfidence::Low => "low",
            DeadConfidence::Medium => "medium",
            DeadConfidence::High => "high",
        }
    }
}

/// Low-confidence-liveness evidence for a callee with NO trusted caller: the
/// heuristic-edge breakdown (from `function_calls`) AND the candidate-edge
/// breakdown (from the `candidate_edges` side-table, Lane 2). Either population
/// alone is enough to relabel a zero-trusted-caller function
/// `low-confidence-live` instead of `dead`; both feed the `cqs dead` verdict
/// reason so it can name the exact provenance and counts rather than a generic
/// "all callers are heuristic" claim. Produced by
/// [`Store::find_low_confidence_live_names`].
#[derive(Debug, Clone, Default)]
pub struct LowConfidenceLiveInfo {
    /// Total heuristic edges reaching this callee (no trusted edge exists).
    pub total: u64,
    /// `(edge_kind string, count)` pairs from `function_calls`, sorted for
    /// deterministic reasons.
    pub kind_counts: Vec<(String, u64)>,
    /// Total `candidate_edges` (Lane 2) references naming this callee (no trusted
    /// edge exists). A nonzero value with `total == 0` is a candidate-ONLY callee
    /// — zero `function_calls` edges, present only in the side-table.
    pub candidate_total: u64,
    /// `(candidate_kind string, count)` pairs from `candidate_edges`, sorted for
    /// deterministic reasons. Kinds are the Lane-2 provenance strings
    /// (`bare_arg_unresolved` / `macro_arg_unresolved` / `serde_container` /
    /// `serde_with_module`), reported transparently so a new candidate kind
    /// surfaces without a query change.
    pub candidate_counts: Vec<(String, u64)>,
}

impl std::fmt::Display for DeadConfidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Fallback entry point names — used when language definitions don't provide any.
/// Cross-language names that span multiple languages live here.
/// These are superseded by `LanguageDef::entry_point_names` via `build_entry_point_names()`.
const FALLBACK_ENTRY_POINT_NAMES: &[&str] = &["main", "new"];

/// Build unified entry point names from all enabled language definitions.
/// Falls back to `FALLBACK_ENTRY_POINT_NAMES` if no language provides any.
fn build_entry_point_names() -> Vec<&'static str> {
    let mut names = crate::language::REGISTRY.all_entry_point_names();
    // Always include cross-language fallbacks
    let mut seen: std::collections::HashSet<&str> = names.iter().copied().collect();
    for name in FALLBACK_ENTRY_POINT_NAMES {
        if seen.insert(name) {
            names.push(name);
        }
    }
    names
}

/// Lightweight chunk metadata for dead code analysis.
/// Used by `find_dead_code` Phase 1 to avoid loading full content/doc
/// until candidates pass name/test/path filters.
#[derive(Debug, Clone)]
pub(crate) struct LightChunk {
    pub id: String,
    pub file: PathBuf,
    pub language: Language,
    pub chunk_type: ChunkType,
    pub name: String,
    pub signature: String,
    pub line_start: u32,
    pub line_end: u32,
}

/// Statistics about call graph entries (chunk-level calls table)
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CallStats {
    /// Total number of call edges
    pub total_calls: u64,
    /// Number of distinct callee names
    pub unique_callees: u64,
}

/// Detailed function call statistics (function_calls table)
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct FunctionCallStats {
    /// Total number of call edges
    pub total_calls: u64,
    /// Number of distinct caller function names
    pub unique_callers: u64,
    /// Number of distinct callee function names
    pub unique_callees: u64,
}

/// Matches `impl SomeTrait for SomeType` patterns to detect trait implementations.
/// Used by `find_dead_code` to skip trait impl methods (invisible to static call graph).
static TRAIT_IMPL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"impl\s+\w+\s+for\s+").expect("hardcoded regex"));

/// Fallback test content markers — used when language definitions don't provide any.
/// These are superseded by `LanguageDef::test_markers` via `build_test_content_markers()`.
const FALLBACK_TEST_CONTENT_MARKERS: &[&str] = &["#[test]", "@Test"];

/// Fallback test path patterns — used when language definitions don't provide any.
/// These are superseded by `LanguageDef::test_path_patterns` via `build_test_path_patterns()`.
const FALLBACK_TEST_PATH_PATTERNS: &[&str] = &[
    "%/tests/%",
    "%\\_test.%",
    "%.test.%",
    "%.spec.%",
    "%_test.go",
    "%_test.py",
];

/// Build unified test content markers from all enabled language definitions.
/// Falls back to `FALLBACK_TEST_CONTENT_MARKERS` if no language provides any.
fn build_test_content_markers() -> Vec<&'static str> {
    let markers = crate::language::REGISTRY.all_test_markers();
    if markers.is_empty() {
        FALLBACK_TEST_CONTENT_MARKERS.to_vec()
    } else {
        markers
    }
}

/// Build unified test path patterns from all enabled language definitions.
/// Falls back to `FALLBACK_TEST_PATH_PATTERNS` if no language provides any.
fn build_test_path_patterns() -> Vec<&'static str> {
    let patterns = crate::language::REGISTRY.all_test_path_patterns();
    if patterns.is_empty() {
        FALLBACK_TEST_PATH_PATTERNS.to_vec()
    } else {
        patterns
    }
}

/// Fallback trait method names — cross-language constructor/builder patterns.
/// These are superseded by `LanguageDef::trait_method_names` via `build_trait_method_names()`.
const FALLBACK_TRAIT_METHOD_NAMES: &[&str] = &["new", "build", "builder"];

/// Build unified trait method names from all enabled language definitions.
/// Always includes cross-language fallbacks.
fn build_trait_method_names() -> Vec<&'static str> {
    let mut names = crate::language::REGISTRY.all_trait_method_names();
    let mut seen: std::collections::HashSet<&str> = names.iter().copied().collect();
    for name in FALLBACK_TRAIT_METHOD_NAMES {
        if seen.insert(name) {
            names.push(name);
        }
    }
    names
}

/// Build the shared SQL WHERE filter clause for test chunks.
/// Combines name patterns, content markers, and path patterns into a single
/// OR-joined clause string. Computed once at startup via LazyLock callers.
///
/// Name patterns flow from `language::REGISTRY.all_test_name_patterns()` —
/// the same source as `is_test_chunk` in lib.rs, so adding a Kotlin/Swift
/// convention is one line in the language module.
fn build_test_chunk_filter() -> String {
    let mut clauses: Vec<String> = Vec::new();
    for pat in crate::language::REGISTRY.all_test_name_patterns() {
        // Patterns are SQL-LIKE with `\_` escaping a literal underscore;
        // emit ESCAPE only for those that actually use the escape so SQL
        // parses cleanly when the pattern is wildcard-only (e.g. `Test`).
        if pat.contains("\\_") {
            clauses.push(format!("name LIKE '{pat}' ESCAPE '\\'"));
        } else {
            clauses.push(format!("name LIKE '{pat}'"));
        }
    }
    for marker in build_test_content_markers() {
        clauses.push(format!("content LIKE '%{marker}%'"));
    }
    for pat in build_test_path_patterns() {
        if pat.contains("\\_") {
            clauses.push(format!("origin LIKE '{pat}' ESCAPE '\\'"));
        } else {
            clauses.push(format!("origin LIKE '{pat}'"));
        }
    }
    clauses.join("\n                 OR ")
}

/// Cached SQL for `find_test_chunks_async` — built once at first use, reused on every call.
static TEST_CHUNKS_SQL: LazyLock<String> = LazyLock::new(|| {
    let filter = build_test_chunk_filter();
    let callable = ChunkType::callable_sql_list();
    format!(
        "SELECT id, origin, language, chunk_type, name, signature,
                    line_start, line_end, parent_id, parent_type_name
             FROM chunks
             WHERE chunk_type IN ({callable})
               AND (
                 {filter}
               )
             ORDER BY origin, line_start"
    )
});

/// Cached SQL for `find_test_chunk_names_async` — built once at first use, reused on every call.
static TEST_CHUNK_NAMES_SQL: LazyLock<String> = LazyLock::new(|| {
    let filter = build_test_chunk_filter();
    let callable = ChunkType::callable_sql_list();
    format!(
        "SELECT DISTINCT name
             FROM chunks
             WHERE chunk_type IN ({callable})
               AND (
                 {filter}
               )"
    )
});
