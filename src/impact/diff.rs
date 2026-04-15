//! Diff-aware impact analysis

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::store::CallerWithContext;
use crate::AnalysisError;
use crate::Store;

use super::analysis::extract_call_snippet_from_cache;
use super::bfs::reverse_bfs_multi_attributed;
use super::types::{
    CallerDetail, ChangedFunction, DiffImpactResult, DiffImpactSummary, DiffTestInfo,
};
use super::DEFAULT_MAX_TEST_SEARCH_DEPTH;

use crate::normalize_slashes;

/// Maximum number of changed functions to process in diff impact analysis.
/// Beyond this cap, results are truncated and a warning is emitted.
const DEFAULT_MAX_CHANGED_FUNCTIONS: usize = 500;

/// Resolve the effective cap, checking `CQS_IMPACT_MAX_CHANGED_FUNCTIONS`
/// first. Zero or unparseable values fall back to the default. Read at
/// every call because impact analysis is infrequent and tuning the cap
/// mid-session should not require restart.
fn max_changed_functions() -> usize {
    std::env::var("CQS_IMPACT_MAX_CHANGED_FUNCTIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_CHANGED_FUNCTIONS)
}

/// Map diff hunks to function names using the index.
/// For each hunk, finds chunks whose line range overlaps the hunk's range.
/// Returns deduplicated function names.
pub fn map_hunks_to_functions<Mode>(
    store: &Store<Mode>,
    hunks: &[crate::diff_parse::DiffHunk],
) -> Vec<ChangedFunction> {
    let _span = tracing::info_span!("map_hunks_to_functions", hunk_count = hunks.len()).entered();
    let mut seen = HashSet::new();
    let mut functions = Vec::new();

    // Group hunks by file
    let mut by_file: HashMap<&Path, Vec<&crate::diff_parse::DiffHunk>> = HashMap::new();
    for hunk in hunks {
        by_file.entry(&hunk.file).or_default().push(hunk);
    }

    // PF-1: Batch-fetch all file chunks in a single query instead of N queries
    let normalized_paths: Vec<String> = by_file
        .keys()
        .map(|f| normalize_slashes(&f.to_string_lossy()))
        .collect();
    let origin_refs: Vec<&str> = normalized_paths.iter().map(|s| s.as_str()).collect();
    let chunks_by_origin = match store.get_chunks_by_origins_batch(&origin_refs) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-fetch chunks for diff hunks");
            return functions;
        }
    };

    for (file, file_hunks) in &by_file {
        let normalized = normalize_slashes(&file.to_string_lossy());
        let chunks = match chunks_by_origin.get(&normalized) {
            Some(c) => c,
            None => continue,
        };
        for hunk in file_hunks {
            // Skip zero-count hunks (insertion points with no changed lines)
            if hunk.count == 0 {
                continue;
            }
            // AC-14: Skip malformed hunks where start + count overflows u32
            let hunk_end = match hunk.start.checked_add(hunk.count) {
                Some(end) => end,
                None => {
                    tracing::warn!(
                        start = hunk.start,
                        count = hunk.count,
                        "Malformed hunk: overflow"
                    );
                    continue;
                }
            };
            for chunk in chunks {
                // Overlap: hunk [start, start+count) vs chunk [line_start, line_end]
                if hunk.start <= chunk.line_end
                    && hunk_end > chunk.line_start
                    && seen.insert(chunk.name.clone())
                {
                    functions.push(ChangedFunction {
                        name: chunk.name.clone(),
                        file: file.to_path_buf(),
                        line_start: chunk.line_start,
                    });
                }
            }
        }
    }

    functions
}

/// Run impact analysis across all changed functions from a diff.
/// Fetches call graph and test chunks once, then analyzes each function.
/// Results are deduplicated by name.
pub fn analyze_diff_impact<Mode>(
    store: &Store<Mode>,
    changed: Vec<ChangedFunction>,
    root: &Path,
) -> Result<DiffImpactResult, AnalysisError> {
    let graph = store.get_call_graph()?;
    let test_chunks = store.find_test_chunks()?;
    analyze_diff_impact_with_graph(store, changed, &graph, &test_chunks, root)
}

/// Like [`analyze_diff_impact`] but accepts pre-loaded graph and test chunks.
/// Paths in the returned result are relative to `root`.
/// Use when the caller already has the graph/test_chunks (e.g., `review_diff`
/// which also needs them for risk scoring).
pub fn analyze_diff_impact_with_graph<Mode>(
    store: &Store<Mode>,
    changed: Vec<ChangedFunction>,
    graph: &crate::store::CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
    root: &Path,
) -> Result<DiffImpactResult, AnalysisError> {
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
                truncated: false,
                truncated_functions: 0,
            },
        });
    }

    // RT-RES-9 / SHL-V1.25-7: cap changed functions to prevent unbounded
    // processing on massive diffs. The cap is overridable via
    // CQS_IMPACT_MAX_CHANGED_FUNCTIONS. `truncated_functions` surfaces the
    // dropped count in the summary so JSON consumers can detect silent
    // truncation without scraping stderr.
    let cap = max_changed_functions();
    let total = changed.len();
    let truncated = total > cap;
    let truncated_functions = if truncated { total - cap } else { 0 };
    let changed = if truncated {
        tracing::warn!(
            total,
            cap,
            dropped = truncated_functions,
            "Changed functions exceed cap, truncating"
        );
        changed.into_iter().take(cap).collect()
    } else {
        changed
    };

    let mut all_tests: Vec<DiffTestInfo> = Vec::new();
    let mut seen_callers = HashSet::new();
    let mut seen_tests: HashMap<String, usize> = HashMap::new();

    // Batch-fetch callers for all changed functions in a single query
    let callee_names: Vec<&str> = changed.iter().map(|f| f.name.as_str()).collect();
    let callers_by_callee = store
        .get_callers_with_context_batch(&callee_names)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch callers for diff impact");
            HashMap::new()
        });

    // Deduplicate callers across all changed functions
    let mut deduped_callers: Vec<CallerWithContext> = Vec::new();
    for func in &changed {
        if let Some(callers_ctx) = callers_by_callee.get(&func.name) {
            for caller in callers_ctx {
                if seen_callers.insert(caller.name.clone()) {
                    deduped_callers.push(caller.clone());
                }
            }
        }
    }

    // Batch-fetch chunk data for all caller names (single query)
    let unique_names: Vec<&str> = deduped_callers.iter().map(|c| c.name.as_str()).collect();
    let chunks_by_name = store
        .search_by_names_batch(&unique_names, 5)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch caller chunks for diff impact snippets");
            HashMap::new()
        });

    // Build CallerDetail with snippets from cache (paths relative to root)
    let all_callers: Vec<CallerDetail> = deduped_callers
        .iter()
        .map(|caller| {
            let snippet = extract_call_snippet_from_cache(&chunks_by_name, caller);
            CallerDetail {
                name: caller.name.clone(),
                file: caller
                    .file
                    .strip_prefix(root)
                    .unwrap_or(&caller.file)
                    .to_path_buf(),
                line: caller.line,
                call_line: caller.call_line,
                snippet,
            }
        })
        .collect();

    // Single attributed BFS: discovers all reachable ancestors AND tracks which
    // changed function (by index) produced the shortest path to each node.
    // Replaces both reverse_bfs_multi (discovery) and N×reverse_bfs (attribution).
    let start_names: Vec<&str> = changed.iter().map(|f| f.name.as_str()).collect();
    let attributed =
        reverse_bfs_multi_attributed(graph, &start_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    for test in test_chunks {
        if let Some(&(depth, source_idx)) = attributed.get(&test.name) {
            if depth > 0 {
                let via = changed
                    .get(source_idx)
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| {
                        tracing::debug!(test = %test.name, source_idx, "BFS anomaly: invalid source index");
                        "(unknown)".to_string()
                    });

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
                            file: test
                                .file
                                .strip_prefix(root)
                                .unwrap_or(&test.file)
                                .to_path_buf(),
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
        truncated,
        truncated_functions,
    };

    Ok(DiffImpactResult {
        changed_functions: changed,
        all_callers,
        all_tests,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::language::{ChunkType, Language};
    use crate::store::{CallGraph, ChunkSummary};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// Creates a test Store instance with a temporary database directory.
    /// This function initializes a new Store with a temporary directory and default ModelInfo configuration. The temporary directory is returned alongside the Store to ensure the database persists for the duration of the test.
    /// # Returns
    /// A tuple containing:
    /// - A newly initialized Store instance
    /// - The TempDir holding the temporary database file
    /// # Panics
    /// Panics if temporary directory creation fails, if opening the Store fails, or if Store initialization fails.
    fn make_test_store() -> (crate::Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
        let store = crate::Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    /// Creates a ChunkSummary for a Rust function with basic initialization.
    /// Constructs a ChunkSummary struct with default values for a Rust function chunk. The function sets up metadata like the function name, file path, and line information, while leaving content and documentation fields empty for later population.
    /// # Arguments
    /// * `name` - The name of the function, used for both the id and name fields
    /// * `file` - The file path where the function is located
    /// * `line_start` - The starting line number of the function
    /// # Returns
    /// A new ChunkSummary struct with language set to Rust, chunk_type set to Function, line_end calculated as line_start + 5, and other fields initialized to empty or default values.
    fn make_chunk_summary(name: &str, file: &str, line_start: u32) -> ChunkSummary {
        ChunkSummary {
            id: name.to_string(),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: String::new(),
            doc: None,
            line_start,
            line_end: line_start + 5,
            parent_id: None,
            parent_type_name: None,
            content_hash: String::new(),
            window_idx: None,
        }
    }

    /// Creates a `ChangedFunction` struct from the provided function metadata.
    /// # Arguments
    /// * `name` - The name of the function
    /// * `file` - The file path where the function is defined
    /// * `line_start` - The starting line number of the function in the file
    /// # Returns
    /// A `ChangedFunction` struct containing the provided function name, file path, and line number information.
    fn make_changed(name: &str, file: &str, line_start: u32) -> ChangedFunction {
        ChangedFunction {
            name: name.to_string(),
            file: PathBuf::from(file),
            line_start,
        }
    }

    /// Constructs an empty call graph with no edges or nodes.
    /// # Returns
    /// A new `CallGraph` instance with empty forward and reverse adjacency maps, ready to have nodes and edges added to it.
    fn make_empty_graph() -> CallGraph {
        CallGraph::from_string_maps(HashMap::new(), HashMap::new())
    }

    /// Depth-0 exclusion: if a test chunk has the same name as a changed function,
    /// BFS returns depth 0 for it and it must NOT appear in all_tests.
    #[test]
    fn test_depth0_changed_function_excluded_from_tests() {
        let (store, _dir) = make_test_store();

        let changed = vec![make_changed("my_func", "src/lib.rs", 10)];

        // The changed function is itself a test chunk (depth 0 in BFS).
        let test_chunks = vec![make_chunk_summary("my_func", "src/lib.rs", 10)];

        let graph = make_empty_graph();

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert!(
            result.all_tests.is_empty(),
            "depth-0 test chunk (same as changed function) must be excluded; got {:?}",
            result.all_tests
        );
        assert_eq!(result.summary.test_count, 0);
    }

    /// Depth-1 test is included, but the changed function itself (depth 0) is not.
    #[test]
    fn test_depth1_test_is_included_depth0_is_not() {
        let (store, _dir) = make_test_store();

        // Graph: test_fn calls changed_fn (so changed_fn is called by test_fn)
        // Reverse: changed_fn <- test_fn
        let mut reverse = HashMap::new();
        reverse.insert("changed_fn".to_string(), vec!["test_fn".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let changed = vec![make_changed("changed_fn", "src/lib.rs", 10)];

        // Two test chunks: changed_fn at depth 0 (excluded), test_fn at depth 1 (included)
        let test_chunks = vec![
            make_chunk_summary("changed_fn", "src/lib.rs", 10),
            make_chunk_summary("test_fn", "tests/lib_test.rs", 50),
        ];

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert_eq!(
            result.all_tests.len(),
            1,
            "only depth-1 test should be included"
        );
        assert_eq!(result.all_tests[0].name, "test_fn");
        assert_eq!(result.all_tests[0].call_depth, 1);
        assert_eq!(result.all_tests[0].via, "changed_fn");
    }

    /// Empty changed list returns empty result immediately (early return path).
    #[test]
    fn test_empty_changed_returns_empty_result() {
        let (store, _dir) = make_test_store();
        let graph = make_empty_graph();
        let test_chunks = vec![make_chunk_summary("some_test", "tests/foo.rs", 1)];

        let result = analyze_diff_impact_with_graph(
            &store,
            vec![],
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert!(result.changed_functions.is_empty());
        assert!(result.all_callers.is_empty());
        assert!(result.all_tests.is_empty());
        assert_eq!(result.summary.changed_count, 0);
        assert_eq!(result.summary.test_count, 0);
    }

    /// BFS anomaly: multi-BFS finds the test at depth > 0, but the only per-function
    /// path to it is at depth 0 (the test IS the changed function in that function's BFS),
    /// so no `d > 0` match exists → falls back to "(unknown)" via.
    /// Setup: two changed functions A and B.
    /// - A's individual BFS finds test_T at depth 0 (A == T — same node, which never happens
    ///   in practice, but we model it via the graph rather than name identity).
    /// - Instead, we model the anomaly using depth-only checks: multi-BFS reaches test_T
    ///   at depth 1 (via B→test_T), but A's BFS reaches test_T at depth 0 (A calls test_T
    ///   directly with A == test_T is impossible; instead we make A's reverse graph have
    ///   test_T as itself).
    /// Actually the most natural anomaly: test_T appears at depth 1 in multi-BFS
    /// (from changed_B), but in changed_A's per-function BFS test_T appears only at depth 0
    /// because test_T == changed_A. The via for test_T then comes from changed_B, not "(unknown)".
    /// To get "(unknown)" we need: multi-BFS finds test_T but BOTH per-function BFS results
    /// only have test_T at depth 0.  This happens when test_T == changed_A AND test_T == changed_B
    /// — impossible. The real "(unknown)" path requires a graph topology bug.
    /// We test the logged anomaly branch via the depth-0 skip: when multi-BFS returns depth 1
    /// for a test, but the per-function BFS has it at depth 0 only, best_via stays None.
    /// Simulate: changed = [T], test_chunks = [T]. Multi-BFS has T at 0 (excluded by `depth > 0`
    /// on the outer if). So actually with a single changed function that IS the test,
    /// there's no way to trigger the inner anomaly without separate multi/single BFS disagreement.
    /// Instead, test the closest approximation: multi-source with two functions where one
    /// reaches the test normally.
    #[test]
    fn test_bfs_anomaly_via_attribution_uses_closest_function() {
        let (store, _dir) = make_test_store();

        // Call graph (forward direction): test_t calls func_a and test_t calls func_b.
        // Reverse edges (callee → callers):
        //   func_a is called by test_t  →  reverse["func_a"] = ["test_t"]
        //   func_b is called by test_t  →  reverse["func_b"] = ["test_t"]
        //
        // BFS from func_a: traverses reverse["func_a"] = [test_t] → test_t at depth 1.
        // BFS from func_b: traverses reverse["func_b"] = [test_t] → test_t at depth 1.
        // Multi-BFS from [func_a, func_b]: test_t at depth 1.
        // Per-function: both find test_t at depth 1, so best_via is one of them (not "(unknown)").
        let mut reverse = HashMap::new();
        reverse.insert("func_a".to_string(), vec!["test_t".to_string()]);
        reverse.insert("func_b".to_string(), vec!["test_t".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let changed = vec![
            make_changed("func_a", "src/a.rs", 1),
            make_changed("func_b", "src/b.rs", 1),
        ];
        let test_chunks = vec![make_chunk_summary("test_t", "tests/t.rs", 5)];

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert_eq!(result.all_tests.len(), 1);
        let test = &result.all_tests[0];
        assert_eq!(test.name, "test_t");
        assert_eq!(test.call_depth, 1);
        // via must be one of the two changed functions, not "(unknown)"
        assert!(
            test.via == "func_a" || test.via == "func_b",
            "via should be a known changed function, got {:?}",
            test.via
        );
    }

    /// Tests are sorted by call_depth ascending.
    #[test]
    fn test_results_sorted_by_call_depth() {
        let (store, _dir) = make_test_store();

        // Chain: changed_fn <- mid <- deep_test
        //        changed_fn <- near_test
        let mut reverse = HashMap::new();
        reverse.insert(
            "changed_fn".to_string(),
            vec!["mid".to_string(), "near_test".to_string()],
        );
        reverse.insert("mid".to_string(), vec!["deep_test".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let changed = vec![make_changed("changed_fn", "src/lib.rs", 1)];
        let test_chunks = vec![
            make_chunk_summary("deep_test", "tests/deep.rs", 1),
            make_chunk_summary("near_test", "tests/near.rs", 1),
        ];

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert_eq!(result.all_tests.len(), 2);
        assert!(
            result.all_tests[0].call_depth <= result.all_tests[1].call_depth,
            "tests must be sorted by call_depth ascending"
        );
        assert_eq!(result.all_tests[0].name, "near_test");
        assert_eq!(result.all_tests[0].call_depth, 1);
        assert_eq!(result.all_tests[1].name, "deep_test");
        assert_eq!(result.all_tests[1].call_depth, 2);
    }

    /// Deduplication: same test reachable from two changed functions gets the shallower via.
    #[test]
    fn test_dedup_keeps_shallower_depth() {
        let (store, _dir) = make_test_store();

        // Call graph (forward): test_t calls func_a (direct), and test_t calls mid which calls func_b.
        // Reverse edges (callee → callers):
        //   func_a is called by test_t → reverse["func_a"] = ["test_t"]  (depth 1 from func_a)
        //   func_b is called by mid    → reverse["func_b"] = ["mid"]
        //   mid is called by test_t   → reverse["mid"]    = ["test_t"]  (depth 1 from mid)
        //
        // BFS from func_a: test_t at depth 1.
        // BFS from func_b: mid at depth 1, test_t at depth 2.
        // Multi-BFS from [func_a, func_b]: test_t at depth 1 (min).
        // test_t should appear once with call_depth=1, via=func_a.
        let mut reverse = HashMap::new();
        reverse.insert("func_a".to_string(), vec!["test_t".to_string()]);
        reverse.insert("func_b".to_string(), vec!["mid".to_string()]);
        reverse.insert("mid".to_string(), vec!["test_t".to_string()]);
        let graph = CallGraph::from_string_maps(HashMap::new(), reverse);

        let changed = vec![
            make_changed("func_a", "src/a.rs", 1),
            make_changed("func_b", "src/b.rs", 1),
        ];
        let test_chunks = vec![make_chunk_summary("test_t", "tests/t.rs", 5)];

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert_eq!(result.all_tests.len(), 1, "test_t deduped to one entry");
        // Multi-BFS minimum depth: test_t is at depth 1 (from func_a).
        assert_eq!(result.all_tests[0].call_depth, 1);
        assert_eq!(result.all_tests[0].via, "func_a");
    }

    /// RT-RES-9: When changed functions exceed the cap, results are truncated
    /// and summary.truncated is true.
    #[test]
    fn test_changed_functions_truncated_when_exceeding_cap() {
        let (store, _dir) = make_test_store();
        let graph = make_empty_graph();
        let test_chunks = vec![];

        // Generate more changed functions than DEFAULT_MAX_CHANGED_FUNCTIONS (500)
        let changed: Vec<ChangedFunction> = (0..600)
            .map(|i| make_changed(&format!("func_{i}"), "src/lib.rs", i))
            .collect();

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert!(result.summary.truncated, "summary must indicate truncation");
        assert_eq!(
            result.summary.changed_count, 500,
            "should be capped at DEFAULT_MAX_CHANGED_FUNCTIONS"
        );
        assert_eq!(result.changed_functions.len(), 500);
        assert_eq!(
            result.summary.truncated_functions, 100,
            "SHL-V1.25-7: truncated_functions counts the dropped excess"
        );
    }

    /// When changed functions are within the cap, truncated is false.
    #[test]
    fn test_changed_functions_not_truncated_within_cap() {
        let (store, _dir) = make_test_store();
        let graph = make_empty_graph();
        let test_chunks = vec![];

        let changed: Vec<ChangedFunction> = (0..10)
            .map(|i| make_changed(&format!("func_{i}"), "src/lib.rs", i))
            .collect();

        let result = analyze_diff_impact_with_graph(
            &store,
            changed,
            &graph,
            &test_chunks,
            std::path::Path::new(""),
        )
        .unwrap();

        assert!(!result.summary.truncated);
        assert_eq!(result.summary.changed_count, 10);
        assert_eq!(result.changed_functions.len(), 10);
        assert_eq!(result.summary.truncated_functions, 0);
    }
}
