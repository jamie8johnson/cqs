//! Impact analysis core
//!
//! Provides BFS caller traversal, test discovery, snippet extraction,
//! transitive caller analysis, and mermaid diagram generation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::store::{CallGraph, CallerWithContext};
use crate::Store;

/// Direct caller with display-ready fields (call-site context + snippet).
///
/// Named `CallerDetail` to distinguish from `store::CallerInfo` which has
/// only basic fields (name, file, line). This struct adds `call_line` and
/// `snippet` for impact analysis display.
pub struct CallerDetail {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub call_line: u32,
    pub snippet: Option<String>,
}

/// Affected test with call depth
pub struct TestInfo {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub call_depth: usize,
}

/// Transitive caller at a given depth
pub struct TransitiveCaller {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub depth: usize,
}

/// Complete impact analysis result
pub struct ImpactResult {
    pub function_name: String,
    pub callers: Vec<CallerDetail>,
    pub tests: Vec<TestInfo>,
    pub transitive_callers: Vec<TransitiveCaller>,
}

/// Default maximum depth for test search BFS.
/// Exposed via `max_test_depth` parameters on analysis functions.
pub const DEFAULT_MAX_TEST_SEARCH_DEPTH: usize = 5;

/// Run impact analysis: find callers, affected tests, and transitive callers.
pub fn analyze_impact(
    store: &Store,
    target_name: &str,
    depth: usize,
) -> anyhow::Result<ImpactResult> {
    let _span = tracing::info_span!("analyze_impact", target = target_name, depth).entered();
    let callers = build_caller_info(store, target_name)?;
    let graph = store.get_call_graph()?;
    let tests = find_affected_tests(store, &graph, target_name)?;
    let transitive_callers = if depth > 1 {
        find_transitive_callers(store, &graph, target_name, depth)?
    } else {
        Vec::new()
    };

    Ok(ImpactResult {
        function_name: target_name.to_string(),
        callers,
        tests,
        transitive_callers,
    })
}

/// Lightweight caller + test coverage hints for a function.
pub struct FunctionHints {
    pub caller_count: usize,
    pub test_count: usize,
}

/// Core implementation — accepts pre-loaded graph and test chunks.
///
/// Use this when processing multiple functions to avoid loading the graph
/// N times (e.g., scout, which processes 10+ functions).
///
/// `max_test_depth` controls BFS depth for test discovery (default: [`DEFAULT_MAX_TEST_SEARCH_DEPTH`]).
pub fn compute_hints_with_graph(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> FunctionHints {
    compute_hints_with_graph_depth(
        graph,
        test_chunks,
        function_name,
        prefetched_caller_count,
        DEFAULT_MAX_TEST_SEARCH_DEPTH,
    )
}

/// Like [`compute_hints_with_graph`] but with configurable BFS depth.
pub fn compute_hints_with_graph_depth(
    graph: &CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    function_name: &str,
    prefetched_caller_count: Option<usize>,
    max_test_depth: usize,
) -> FunctionHints {
    // Note: prefetched_caller_count (from get_caller_counts_batch / get_callers_full)
    // counts DB rows which may include duplicate caller names from different files.
    // graph.reverse counts unique caller names per the in-memory graph. These can
    // diverge slightly. We prefer the prefetched count when available since it matches
    // what the caller already displayed, avoiding confusing mismatches.
    let caller_count = match prefetched_caller_count {
        Some(n) => n,
        None => graph
            .reverse
            .get(function_name)
            .map(|v| v.len())
            .unwrap_or(0),
    };
    let ancestors = reverse_bfs(graph, function_name, max_test_depth);
    let test_count = test_chunks
        .iter()
        .filter(|t| ancestors.get(&t.name).is_some_and(|&d| d > 0))
        .count();

    FunctionHints {
        caller_count,
        test_count,
    }
}

/// Compute caller count and test count for a single function.
///
/// Convenience wrapper that loads graph internally. Pass `prefetched_caller_count`
/// to avoid re-querying callers when the caller already has them (e.g., `explain`
/// fetches callers before this).
pub fn compute_hints(
    store: &Store,
    function_name: &str,
    prefetched_caller_count: Option<usize>,
) -> anyhow::Result<FunctionHints> {
    let caller_count = match prefetched_caller_count {
        Some(n) => n,
        None => store.get_callers_full(function_name)?.len(),
    };
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;
    Ok(compute_hints_with_graph(
        &graph,
        &test_chunks,
        function_name,
        Some(caller_count),
    ))
}

/// Build caller detail with call-site snippets.
///
/// Batch-fetches all caller chunks in a single query (via `search_by_names_batch`)
/// to avoid N+1 per-caller `search_by_name` calls.
fn build_caller_info(store: &Store, target_name: &str) -> anyhow::Result<Vec<CallerDetail>> {
    let callers_ctx = store.get_callers_with_context(target_name)?;

    // Batch-fetch chunk data for all unique caller names
    let unique_names: Vec<&str> = {
        let mut seen = HashSet::new();
        callers_ctx
            .iter()
            .filter(|c| seen.insert(c.name.as_str()))
            .map(|c| c.name.as_str())
            .collect()
    };
    let chunks_by_name = store
        .search_by_names_batch(&unique_names, 5)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch caller chunks for snippets");
            HashMap::new()
        });

    let mut callers = Vec::with_capacity(callers_ctx.len());
    for caller in &callers_ctx {
        let snippet = extract_call_snippet_from_cache(&chunks_by_name, caller);
        callers.push(CallerDetail {
            name: caller.name.clone(),
            file: caller.file.clone(),
            line: caller.line,
            call_line: caller.call_line,
            snippet,
        });
    }

    Ok(callers)
}

/// Extract a snippet around the call site using pre-fetched chunk data.
///
/// Prefers non-windowed chunks (correct line offsets) over windowed ones.
fn extract_call_snippet_from_cache(
    chunks_by_name: &HashMap<String, Vec<crate::store::SearchResult>>,
    caller: &CallerWithContext,
) -> Option<String> {
    let results = chunks_by_name.get(&caller.name)?;

    // Prefer non-windowed chunk (correct line offsets)
    let best = {
        let mut best = None;
        for r in results {
            if r.chunk.parent_id.is_none() {
                best = Some(r);
                break;
            }
            if best.is_none() {
                best = Some(r);
            }
        }
        best
    }?;

    let lines: Vec<&str> = best.chunk.content.lines().collect();
    let offset = caller.call_line.saturating_sub(best.chunk.line_start) as usize;
    if offset < lines.len() {
        let start = offset.saturating_sub(1);
        let end = (offset + 2).min(lines.len());
        Some(lines[start..end].join("\n"))
    } else {
        None
    }
}

/// Find tests that transitively call the target via reverse BFS
fn find_affected_tests(
    store: &Store,
    graph: &CallGraph,
    target_name: &str,
) -> anyhow::Result<Vec<TestInfo>> {
    let test_chunks = store.find_test_chunks()?;
    let ancestors = reverse_bfs(graph, target_name, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    let mut tests: Vec<TestInfo> = test_chunks
        .iter()
        .filter_map(|test| {
            ancestors.get(&test.name).and_then(|&d| {
                if d > 0 {
                    Some(TestInfo {
                        name: test.name.clone(),
                        file: test.file.clone(),
                        line: test.line_start,
                        call_depth: d,
                    })
                } else {
                    None
                }
            })
        })
        .collect();

    tests.sort_by_key(|t| t.call_depth);
    Ok(tests)
}

/// Find transitive callers up to the given depth
fn find_transitive_callers(
    store: &Store,
    graph: &CallGraph,
    target_name: &str,
    depth: usize,
) -> anyhow::Result<Vec<TransitiveCaller>> {
    let mut result = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(target_name.to_string());
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    queue.push_back((target_name.to_string(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller_name in callers {
                if visited.insert(caller_name.clone()) {
                    match store.search_by_name(caller_name, 1) {
                        Ok(results) => {
                            if let Some(r) = results.into_iter().next() {
                                result.push(TransitiveCaller {
                                    name: caller_name.clone(),
                                    file: r.chunk.file,
                                    line: r.chunk.line_start,
                                    depth: d + 1,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::warn!(caller = %caller_name, error = %e, "Failed to look up transitive caller");
                        }
                    }
                    queue.push_back((caller_name.clone(), d + 1));
                }
            }
        }
    }

    Ok(result)
}

/// Reverse BFS from a target node, returning all ancestors with their depths
pub(crate) fn reverse_bfs(
    graph: &CallGraph,
    target: &str,
    max_depth: usize,
) -> HashMap<String, usize> {
    let mut ancestors: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    ancestors.insert(target.to_string(), 0);
    queue.push_back((target.to_string(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                if !ancestors.contains_key(caller) {
                    ancestors.insert(caller.clone(), d + 1);
                    queue.push_back((caller.clone(), d + 1));
                }
            }
        }
    }

    ancestors
}

/// Multi-source reverse BFS from multiple target nodes simultaneously.
///
/// Instead of calling `reverse_bfs()` N times (one per changed function),
/// starts BFS from all targets at once. Each node gets the minimum depth
/// from any starting node. Returns ancestors with their minimum depths.
pub(crate) fn reverse_bfs_multi(
    graph: &CallGraph,
    targets: &[&str],
    max_depth: usize,
) -> HashMap<String, usize> {
    let mut ancestors: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    for &target in targets {
        ancestors.insert(target.to_string(), 0);
        queue.push_back((target.to_string(), 0));
    }

    while let Some((current, d)) = queue.pop_front() {
        if d >= max_depth {
            continue;
        }
        if let Some(callers) = graph.reverse.get(&current) {
            for caller in callers {
                match ancestors.entry(caller.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(d + 1);
                        queue.push_back((caller.clone(), d + 1));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Update if we found a shorter path
                        if d + 1 < *e.get() {
                            *e.get_mut() = d + 1;
                            queue.push_back((caller.clone(), d + 1));
                        }
                    }
                }
            }
        }
    }

    ancestors
}

// ============ JSON Serialization ============

/// Serialize impact result to JSON, relativizing paths against the project root
pub fn impact_to_json(result: &ImpactResult, root: &Path) -> serde_json::Value {
    let callers_json: Vec<_> = result
        .callers
        .iter()
        .map(|c| {
            let rel = crate::rel_display(&c.file, root);
            serde_json::json!({
                "name": c.name,
                "file": rel,
                "line": c.line,
                "call_line": c.call_line,
                "snippet": c.snippet,
            })
        })
        .collect();

    let tests_json: Vec<_> = result
        .tests
        .iter()
        .map(|t| {
            let rel = crate::rel_display(&t.file, root);
            serde_json::json!({
                "name": t.name,
                "file": rel,
                "line": t.line,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    let mut output = serde_json::json!({
        "function": result.function_name,
        "callers": callers_json,
        "tests": tests_json,
        "caller_count": callers_json.len(),
        "test_count": tests_json.len(),
    });

    if !result.transitive_callers.is_empty() {
        let trans_json: Vec<_> = result
            .transitive_callers
            .iter()
            .map(|c| {
                let rel = crate::rel_display(&c.file, root);
                serde_json::json!({
                    "name": c.name,
                    "file": rel,
                    "line": c.line,
                    "depth": c.depth,
                })
            })
            .collect();
        if let Some(obj) = output.as_object_mut() {
            obj.insert("transitive_callers".into(), serde_json::json!(trans_json));
        }
    }

    output
}

// ============ Mermaid Diagram ============

/// Generate a mermaid diagram from impact result
pub fn impact_to_mermaid(result: &ImpactResult, root: &Path) -> String {
    let mut lines = vec!["graph TD".to_string()];
    lines.push(format!(
        "    A[\"{}\"]\n    style A fill:#f96",
        mermaid_escape(&result.function_name)
    ));

    let mut idx = 1;
    for c in &result.callers {
        let rel = crate::rel_display(&c.file, root);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}[\"{} ({}:{})\"]",
            letter,
            mermaid_escape(&c.name),
            mermaid_escape(&rel),
            c.line
        ));
        lines.push(format!("    {} --> A", letter));
        idx += 1;
    }

    for t in &result.tests {
        let rel = crate::rel_display(&t.file, root);
        let letter = node_letter(idx);
        lines.push(format!(
            "    {}{{\"{}\\n{}\\ndepth: {}\"}}",
            letter,
            mermaid_escape(&t.name),
            mermaid_escape(&rel),
            t.call_depth
        ));
        lines.push(format!("    {} -.-> A", letter));
        idx += 1;
    }

    lines.join("\n")
}

// ============ Diff-Aware Impact ============

/// A function identified as changed by a diff
pub struct ChangedFunction {
    pub name: String,
    pub file: String,
    pub line_start: u32,
}

/// A test affected by diff changes, tracking which changed function leads to it
pub struct DiffTestInfo {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub via: String,
    pub call_depth: usize,
}

/// Summary counts for diff impact
pub struct DiffImpactSummary {
    pub changed_count: usize,
    pub caller_count: usize,
    pub test_count: usize,
}

/// Aggregated impact result from a diff
pub struct DiffImpactResult {
    pub changed_functions: Vec<ChangedFunction>,
    pub all_callers: Vec<CallerDetail>,
    pub all_tests: Vec<DiffTestInfo>,
    pub summary: DiffImpactSummary,
}

/// Map diff hunks to function names using the index.
///
/// For each hunk, finds chunks whose line range overlaps the hunk's range.
/// Returns deduplicated function names.
pub fn map_hunks_to_functions(
    store: &Store,
    hunks: &[crate::diff_parse::DiffHunk],
) -> Vec<ChangedFunction> {
    let _span = tracing::info_span!("map_hunks_to_functions", hunk_count = hunks.len()).entered();
    let mut seen = HashSet::new();
    let mut functions = Vec::new();

    // Group hunks by file
    let mut by_file: HashMap<&str, Vec<&crate::diff_parse::DiffHunk>> = HashMap::new();
    for hunk in hunks {
        by_file.entry(&hunk.file).or_default().push(hunk);
    }

    for (file, file_hunks) in &by_file {
        let normalized = file.replace('\\', "/");
        let chunks = match store.get_chunks_by_origin(&normalized) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %file, error = %e, "Failed to get chunks for file");
                continue;
            }
        };
        for hunk in file_hunks {
            // Skip zero-count hunks (insertion points with no changed lines)
            if hunk.count == 0 {
                continue;
            }
            let hunk_end = hunk.start.saturating_add(hunk.count); // exclusive
            for chunk in &chunks {
                // Overlap: hunk [start, start+count) vs chunk [line_start, line_end]
                if hunk.start <= chunk.line_end
                    && hunk_end > chunk.line_start
                    && seen.insert(chunk.name.clone())
                {
                    functions.push(ChangedFunction {
                        name: chunk.name.clone(),
                        file: file.to_string(),
                        line_start: chunk.line_start,
                    });
                }
            }
        }
    }

    functions
}

/// Run impact analysis across all changed functions from a diff.
///
/// Fetches call graph and test chunks once, then analyzes each function.
/// Results are deduplicated by name.
pub fn analyze_diff_impact(
    store: &Store,
    changed: Vec<ChangedFunction>,
) -> anyhow::Result<DiffImpactResult> {
    let _span = tracing::info_span!("analyze_diff_impact", changed_count = changed.len()).entered();
    if changed.is_empty() {
        return Ok(DiffImpactResult {
            changed_functions: Vec::new(),
            all_callers: Vec::new(),
            all_tests: Vec::new(),
            summary: DiffImpactSummary {
                changed_count: 0,
                caller_count: 0,
                test_count: 0,
            },
        });
    }

    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;

    let mut all_tests: Vec<DiffTestInfo> = Vec::new();
    let mut seen_callers = HashSet::new();
    let mut seen_tests: HashMap<String, usize> = HashMap::new();

    // Collect all unique callers across changed functions (first pass)
    let mut deduped_callers: Vec<CallerWithContext> = Vec::new();
    for func in &changed {
        match store.get_callers_with_context(&func.name) {
            Ok(callers_ctx) => {
                for caller in callers_ctx {
                    if seen_callers.insert(caller.name.clone()) {
                        deduped_callers.push(caller);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(function = %func.name, error = %e, "Failed to get callers");
            }
        }
    }

    // Batch-fetch chunk data for all caller names (single query)
    let unique_names: Vec<&str> = deduped_callers.iter().map(|c| c.name.as_str()).collect();
    let chunks_by_name = store.search_by_names_batch(&unique_names, 5).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to batch-fetch caller chunks for diff impact snippets");
        HashMap::new()
    });

    // Build CallerDetail with snippets from cache
    let all_callers: Vec<CallerDetail> = deduped_callers
        .iter()
        .map(|caller| {
            let snippet = extract_call_snippet_from_cache(&chunks_by_name, caller);
            CallerDetail {
                name: caller.name.clone(),
                file: caller.file.clone(),
                line: caller.line,
                call_line: caller.call_line,
                snippet,
            }
        })
        .collect();

    // Affected tests via multi-source reverse BFS — single traversal for all changed functions
    let start_names: Vec<&str> = changed.iter().map(|f| f.name.as_str()).collect();
    let ancestors = reverse_bfs_multi(&graph, &start_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);
    for test in &test_chunks {
        if let Some(&depth) = ancestors.get(&test.name) {
            if depth > 0 {
                // Find which changed function is closest to this test
                let via = changed
                    .iter()
                    .find(|f| ancestors.get(&f.name).is_some_and(|&d| d == 0))
                    .map(|f| f.name.clone())
                    .unwrap_or_default();

                match seen_tests.entry(test.name.clone()) {
                    std::collections::hash_map::Entry::Occupied(o) => {
                        let idx = *o.get();
                        if depth < all_tests[idx].call_depth {
                            all_tests[idx].via = via;
                            all_tests[idx].call_depth = depth;
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(all_tests.len());
                        all_tests.push(DiffTestInfo {
                            name: test.name.clone(),
                            file: test.file.clone(),
                            line: test.line_start,
                            via,
                            call_depth: depth,
                        });
                    }
                }
            }
        }
    }

    all_tests.sort_by_key(|t| t.call_depth);

    let summary = DiffImpactSummary {
        changed_count: changed.len(),
        caller_count: all_callers.len(),
        test_count: all_tests.len(),
    };

    Ok(DiffImpactResult {
        changed_functions: changed,
        all_callers,
        all_tests,
        summary,
    })
}

/// Serialize diff impact result to JSON
pub fn diff_impact_to_json(result: &DiffImpactResult, root: &Path) -> serde_json::Value {
    let changed_json: Vec<_> = result
        .changed_functions
        .iter()
        .map(|f| {
            serde_json::json!({
                "name": f.name,
                "file": f.file,
                "line_start": f.line_start,
            })
        })
        .collect();

    let callers_json: Vec<_> = result
        .all_callers
        .iter()
        .map(|c| {
            let rel = crate::rel_display(&c.file, root);
            serde_json::json!({
                "name": c.name,
                "file": rel,
                "line": c.line,
                "call_line": c.call_line,
            })
        })
        .collect();

    let tests_json: Vec<_> = result
        .all_tests
        .iter()
        .map(|t| {
            let rel = crate::rel_display(&t.file, root);
            serde_json::json!({
                "name": t.name,
                "file": rel,
                "line": t.line,
                "via": t.via,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    serde_json::json!({
        "changed_functions": changed_json,
        "callers": callers_json,
        "tests": tests_json,
        "summary": {
            "changed_count": result.summary.changed_count,
            "caller_count": result.summary.caller_count,
            "test_count": result.summary.test_count,
        }
    })
}

// ============ Test Suggestions ============

/// A suggested test for an untested caller
pub struct TestSuggestion {
    /// Suggested test function name
    pub test_name: String,
    /// Suggested file for the test
    pub suggested_file: String,
    /// The untested function this test would cover
    pub for_function: String,
    /// Where the naming pattern came from (empty if default)
    pub pattern_source: String,
    /// Whether to put the test inline (vs external test file)
    pub inline: bool,
}

/// Suggest tests for untested callers in an impact result.
///
/// Loads its own call graph and test chunks — only called when `--suggest-tests`
/// is set, so the normal path pays zero overhead.
pub fn suggest_tests(store: &Store, impact: &ImpactResult) -> Vec<TestSuggestion> {
    let graph = match store.get_call_graph() {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load call graph for test suggestions");
            return Vec::new();
        }
    };
    let test_chunks = match store.find_test_chunks() {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load test chunks for test suggestions");
            return Vec::new();
        }
    };

    let mut suggestions = Vec::new();

    for caller in &impact.callers {
        // Check if this caller is reached by ANY test (not just the target's tests).
        // Per-caller BFS is correct here because we need per-caller test status.
        // Multi-source BFS would merge all callers, losing which caller reaches which test.
        // Caller count is typically small (direct callers only), so this is fine.
        let ancestors = reverse_bfs(&graph, &caller.name, DEFAULT_MAX_TEST_SEARCH_DEPTH);
        let is_tested = test_chunks
            .iter()
            .any(|t| ancestors.get(&t.name).is_some_and(|&d| d > 0));

        if is_tested {
            continue;
        }

        // Fetch file chunks once for inline test check, pattern, and language
        let file_chunks = match store.get_chunks_by_origin(&caller.file.to_string_lossy()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %caller.file.display(), error = %e, "Failed to get file chunks");
                Vec::new()
            }
        };

        let chunk_is_test = |c: &crate::store::ChunkSummary| {
            crate::is_test_chunk(&c.name, &c.file.to_string_lossy())
        };

        let has_inline_tests = file_chunks.iter().any(chunk_is_test);

        let pattern_source = if has_inline_tests {
            file_chunks
                .iter()
                .find(|c| chunk_is_test(c))
                .map(|c| c.name.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };

        let language = file_chunks.first().map(|c| c.language);

        // Generate test name based on language
        let base_name = caller.name.trim_start_matches("self.");
        let test_name = match language {
            Some(crate::parser::Language::JavaScript | crate::parser::Language::TypeScript) => {
                format!("test('{base_name}', ...)")
            }
            Some(crate::parser::Language::Java) if !base_name.is_empty() => {
                // Java: camelCase testMethodName
                let mut chars = base_name.chars();
                let first = chars.next().unwrap().to_uppercase().to_string();
                let rest: String = chars.collect();
                format!("test{first}{rest}")
            }
            _ => {
                // Rust, Python, Go, C, SQL, Markdown — all use snake_case test_ prefix
                format!("test_{base_name}")
            }
        };

        // Suggest file location
        let caller_file_str = caller.file.to_string_lossy().replace('\\', "/");

        let suggested_file = if has_inline_tests {
            caller_file_str.to_string()
        } else {
            suggest_test_file(&caller_file_str)
        };

        suggestions.push(TestSuggestion {
            test_name,
            suggested_file,
            for_function: caller.name.clone(),
            pattern_source,
            inline: has_inline_tests,
        });
    }

    suggestions
}

/// Derive a test file path from a source file path.
fn suggest_test_file(source: &str) -> String {
    // Extract the filename stem and extension
    let path = std::path::Path::new(source);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("rs");

    // Find the nearest parent directory
    let parent = path
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("tests")
        .replace('\\', "/");

    match ext {
        "rs" => format!("{parent}/tests/{stem}_test.rs"),
        "py" => format!("{parent}/test_{stem}.py"),
        "ts" | "tsx" => format!("{parent}/{stem}.test.ts"),
        "js" | "jsx" => format!("{parent}/{stem}.test.js"),
        "go" => format!("{parent}/{stem}_test.go"),
        "java" => format!("{parent}/{stem}Test.java"),
        _ => format!("{parent}/tests/{stem}_test.{ext}"),
    }
}

// ============ Helpers ============

/// Convert index to spreadsheet-style column label: A..Z, AA..AZ, BA..BZ, ...
///
/// Unlike the previous `A1`, `B1` scheme, this produces valid mermaid node IDs
/// that are unambiguous for any number of nodes.
fn node_letter(mut i: usize) -> String {
    let mut result = String::new();
    loop {
        result.insert(0, (b'A' + (i % 26) as u8) as char);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    result
}

fn mermaid_escape(s: &str) -> String {
    s.replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== suggest_test_file tests =====

    #[test]
    fn test_suggest_test_file_rust() {
        assert_eq!(
            suggest_test_file("src/search.rs"),
            "src/tests/search_test.rs"
        );
    }

    #[test]
    fn test_suggest_test_file_python() {
        assert_eq!(suggest_test_file("src/search.py"), "src/test_search.py");
    }

    #[test]
    fn test_suggest_test_file_typescript() {
        assert_eq!(suggest_test_file("src/search.ts"), "src/search.test.ts");
    }

    #[test]
    fn test_suggest_test_file_javascript() {
        assert_eq!(suggest_test_file("src/search.js"), "src/search.test.js");
    }

    #[test]
    fn test_suggest_test_file_go() {
        assert_eq!(suggest_test_file("pkg/search.go"), "pkg/search_test.go");
    }

    #[test]
    fn test_suggest_test_file_java() {
        assert_eq!(suggest_test_file("src/Search.java"), "src/SearchTest.java");
    }

    // ===== reverse_bfs tests =====

    #[test]
    fn test_reverse_bfs_empty_graph() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
        let result = reverse_bfs(&graph, "target", 5);
        assert_eq!(result.len(), 1); // Just the target itself at depth 0
        assert_eq!(result["target"], 0);
    }

    #[test]
    fn test_reverse_bfs_chain() {
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let result = reverse_bfs(&graph, "C", 5);
        assert_eq!(result["C"], 0);
        assert_eq!(result["B"], 1);
        assert_eq!(result["A"], 2);
    }

    #[test]
    fn test_reverse_bfs_respects_depth() {
        let mut reverse = HashMap::new();
        reverse.insert("C".to_string(), vec!["B".to_string()]);
        reverse.insert("B".to_string(), vec!["A".to_string()]);
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let result = reverse_bfs(&graph, "C", 1);
        assert_eq!(result.len(), 2); // C at 0, B at 1
        assert!(!result.contains_key("A")); // Beyond depth limit
    }

    // ===== node_letter tests (P3-17) =====

    #[test]
    fn test_node_letter_single_char() {
        assert_eq!(node_letter(0), "A");
        assert_eq!(node_letter(1), "B");
        assert_eq!(node_letter(25), "Z");
    }

    #[test]
    fn test_node_letter_double_char() {
        // Spreadsheet-style: after Z comes AA, AB, ..., AZ, BA, ...
        assert_eq!(node_letter(26), "AA");
        assert_eq!(node_letter(27), "AB");
        assert_eq!(node_letter(51), "AZ");
        assert_eq!(node_letter(52), "BA");
    }

    #[test]
    fn test_node_letter_triple_char() {
        // 26 + 26*26 = 702 → AAA
        assert_eq!(node_letter(702), "AAA");
    }

    // ===== mermaid_escape tests (P3-17) =====

    #[test]
    fn test_mermaid_escape_quotes() {
        assert_eq!(mermaid_escape("hello \"world\""), "hello &quot;world&quot;");
    }

    #[test]
    fn test_mermaid_escape_angle_brackets() {
        assert_eq!(mermaid_escape("Vec<T>"), "Vec&lt;T&gt;");
    }

    #[test]
    fn test_mermaid_escape_no_special() {
        assert_eq!(mermaid_escape("plain_text"), "plain_text");
    }

    #[test]
    fn test_mermaid_escape_all_special() {
        assert_eq!(mermaid_escape("\"<>\""), "&quot;&lt;&gt;&quot;");
    }

    // ===== impact_to_json tests (P3-16) =====

    #[test]
    fn test_impact_to_json_structure() {
        let result = ImpactResult {
            function_name: "target_fn".to_string(),
            callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("/project/src/lib.rs"),
                line: 10,
                call_line: 15,
                snippet: Some("target_fn()".to_string()),
            }],
            tests: vec![TestInfo {
                name: "test_target".to_string(),
                file: PathBuf::from("/project/tests/test.rs"),
                line: 1,
                call_depth: 2,
            }],
            transitive_callers: Vec::new(),
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

        assert_eq!(json["function"], "target_fn");
        assert_eq!(json["caller_count"], 1);
        assert_eq!(json["test_count"], 1);

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers[0]["name"], "caller_a");
        assert_eq!(callers[0]["file"], "src/lib.rs");
        assert_eq!(callers[0]["line"], 10);
        assert_eq!(callers[0]["call_line"], 15);
        assert_eq!(callers[0]["snippet"], "target_fn()");

        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests[0]["name"], "test_target");
        assert_eq!(tests[0]["call_depth"], 2);
    }

    #[test]
    fn test_impact_to_json_with_transitive() {
        let result = ImpactResult {
            function_name: "target".to_string(),
            callers: Vec::new(),
            tests: Vec::new(),
            transitive_callers: vec![TransitiveCaller {
                name: "indirect".to_string(),
                file: PathBuf::from("/project/src/app.rs"),
                line: 5,
                depth: 2,
            }],
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

        assert!(json["transitive_callers"].is_array());
        let trans = json["transitive_callers"].as_array().unwrap();
        assert_eq!(trans.len(), 1);
        assert_eq!(trans[0]["name"], "indirect");
        assert_eq!(trans[0]["depth"], 2);
    }

    #[test]
    fn test_impact_to_json_empty() {
        let result = ImpactResult {
            function_name: "lonely".to_string(),
            callers: Vec::new(),
            tests: Vec::new(),
            transitive_callers: Vec::new(),
        };
        let root = Path::new("/project");
        let json = impact_to_json(&result, root);

        assert_eq!(json["function"], "lonely");
        assert_eq!(json["caller_count"], 0);
        assert_eq!(json["test_count"], 0);
        assert!(json.get("transitive_callers").is_none());
    }

    // ===== diff_impact_to_json tests (P3-16) =====

    #[test]
    fn test_diff_impact_to_json_structure() {
        let result = DiffImpactResult {
            changed_functions: vec![ChangedFunction {
                name: "changed_fn".to_string(),
                file: "src/lib.rs".to_string(),
                line_start: 10,
            }],
            all_callers: vec![CallerDetail {
                name: "caller_a".to_string(),
                file: PathBuf::from("/project/src/app.rs"),
                line: 20,
                call_line: 25,
                snippet: None,
            }],
            all_tests: vec![DiffTestInfo {
                name: "test_changed".to_string(),
                file: PathBuf::from("/project/tests/test.rs"),
                line: 1,
                via: "changed_fn".to_string(),
                call_depth: 1,
            }],
            summary: DiffImpactSummary {
                changed_count: 1,
                caller_count: 1,
                test_count: 1,
            },
        };
        let root = Path::new("/project");
        let json = diff_impact_to_json(&result, root);

        let changed = json["changed_functions"].as_array().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0]["name"], "changed_fn");

        let callers = json["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "caller_a");

        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["name"], "test_changed");
        assert_eq!(tests[0]["via"], "changed_fn");
        assert_eq!(tests[0]["call_depth"], 1);

        assert_eq!(json["summary"]["changed_count"], 1);
        assert_eq!(json["summary"]["caller_count"], 1);
        assert_eq!(json["summary"]["test_count"], 1);
    }

    #[test]
    fn test_diff_impact_to_json_empty() {
        let result = DiffImpactResult {
            changed_functions: Vec::new(),
            all_callers: Vec::new(),
            all_tests: Vec::new(),
            summary: DiffImpactSummary {
                changed_count: 0,
                caller_count: 0,
                test_count: 0,
            },
        };
        let root = Path::new("/project");
        let json = diff_impact_to_json(&result, root);

        assert_eq!(json["changed_functions"].as_array().unwrap().len(), 0);
        assert_eq!(json["callers"].as_array().unwrap().len(), 0);
        assert_eq!(json["tests"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["changed_count"], 0);
    }

    // ===== compute_hints_with_graph tests (P3-20: stale data edge case) =====

    #[test]
    fn test_compute_hints_with_graph_stale_callers() {
        // Graph references callers that don't exist as test chunks.
        // This should be handled gracefully — no panics, just zero test matches.
        let mut reverse = HashMap::new();
        reverse.insert(
            "target".to_string(),
            vec!["ghost_caller".to_string(), "another_ghost".to_string()],
        );
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        // No test chunks at all — stale graph referencing nonexistent functions
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        assert_eq!(hints.caller_count, 2, "Should count callers from graph");
        assert_eq!(hints.test_count, 0, "No test chunks means no tests");
    }

    #[test]
    fn test_compute_hints_with_graph_stale_test_ancestor() {
        // Test chunk exists but its ancestor chain in graph references nonexistent nodes.
        // The BFS should still find paths through valid edges.
        let mut reverse = HashMap::new();
        reverse.insert("target".to_string(), vec!["middle".to_string()]);
        // "middle" is called by "test_fn" but "test_fn" is not in the reverse graph for middle.
        // Graph is incomplete/stale — test_fn calls middle but only middle→target is in reverse.
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse,
        };
        let test_chunks = vec![crate::store::ChunkSummary {
            id: "test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("test.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "test_fn".to_string(),
            signature: "fn test_fn()".to_string(),
            content: "#[test] fn test_fn() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            parent_id: None,
        }];
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", None);
        // test_fn is not reachable from target via reverse BFS (it's not in graph.reverse)
        assert_eq!(hints.test_count, 0, "Unreachable test should not count");
        assert_eq!(hints.caller_count, 1, "middle is a caller");
    }

    #[test]
    fn test_compute_hints_with_graph_prefetched_caller_count() {
        let graph = CallGraph {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        };
        let test_chunks: Vec<crate::store::ChunkSummary> = Vec::new();
        let hints = compute_hints_with_graph(&graph, &test_chunks, "target", Some(99));
        assert_eq!(hints.caller_count, 99, "Should use prefetched value");
    }
}
