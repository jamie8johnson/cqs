//! Diff-aware impact analysis

use std::collections::{HashMap, HashSet};

use crate::store::CallerWithContext;
use crate::Store;

use super::analysis::extract_call_snippet_from_cache;
use super::bfs::{reverse_bfs, reverse_bfs_multi};
use super::types::{
    CallerDetail, ChangedFunction, DiffImpactResult, DiffImpactSummary, DiffTestInfo,
};
use super::DEFAULT_MAX_TEST_SEARCH_DEPTH;

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
    let chunks_by_name = store
        .search_by_names_batch(&unique_names, 5)
        .unwrap_or_else(|e| {
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

    // Affected tests via multi-source reverse BFS — single traversal for discovery
    let start_names: Vec<&str> = changed.iter().map(|f| f.name.as_str()).collect();
    let ancestors = reverse_bfs_multi(&graph, &start_names, DEFAULT_MAX_TEST_SEARCH_DEPTH);

    // Pre-compute per-function BFS for via attribution.
    // reverse_bfs_multi merges all sources, losing which changed function reaches which test.
    // Individual BFS per changed function lets us attribute each test to its closest source.
    let per_function_ancestors: Vec<HashMap<String, usize>> = changed
        .iter()
        .map(|f| reverse_bfs(&graph, &f.name, DEFAULT_MAX_TEST_SEARCH_DEPTH))
        .collect();

    for test in &test_chunks {
        if let Some(&depth) = ancestors.get(&test.name) {
            if depth > 0 {
                // Attribute test to the changed function that reaches it at minimum depth
                let mut best_via = None;
                let mut best_depth = usize::MAX;
                for (i, func_ancestors) in per_function_ancestors.iter().enumerate() {
                    if let Some(&d) = func_ancestors.get(&test.name) {
                        if d > 0 && d < best_depth {
                            best_depth = d;
                            best_via = Some(changed[i].name.clone());
                        }
                    }
                }
                let via = best_via.unwrap_or_else(|| {
                    tracing::debug!(test = %test.name, "BFS anomaly: test found but no changed function path");
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
