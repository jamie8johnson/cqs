//! Core impact analysis — caller discovery, test mapping, test suggestions

use std::collections::{HashMap, HashSet};

use crate::store::{CallerWithContext, SearchResult};
use crate::Store;

use super::bfs::reverse_bfs;
use super::types::{CallerDetail, ImpactResult, TestInfo, TestSuggestion, TransitiveCaller};
use super::DEFAULT_MAX_TEST_SEARCH_DEPTH;

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
pub(super) fn extract_call_snippet_from_cache(
    chunks_by_name: &HashMap<String, Vec<SearchResult>>,
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

    // Bounds check: call_line must fall within chunk's line range (windowed chunks
    // may not cover the call site)
    if caller.call_line < best.chunk.line_start || caller.call_line > best.chunk.line_end {
        return None;
    }

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
    graph: &crate::store::CallGraph,
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

/// Find transitive callers up to the given depth.
///
/// Uses `reverse_bfs` to discover all ancestor names in a single graph traversal,
/// then batch-fetches chunk locations with `search_by_names_batch` to avoid N+1 queries.
fn find_transitive_callers(
    store: &Store,
    graph: &crate::store::CallGraph,
    target_name: &str,
    depth: usize,
) -> anyhow::Result<Vec<TransitiveCaller>> {
    // Single graph traversal to collect all ancestors + depths
    let ancestors = reverse_bfs(graph, target_name, depth);

    // Filter out the target itself and depth-0 entries
    let caller_entries: Vec<(&str, usize)> = ancestors
        .iter()
        .filter(|(name, &d)| d > 0 && name.as_str() != target_name)
        .map(|(name, &d)| (name.as_str(), d))
        .collect();

    if caller_entries.is_empty() {
        return Ok(Vec::new());
    }

    // Batch-fetch all chunk locations in one query (exact name match, not FTS)
    let names: Vec<&str> = caller_entries.iter().map(|(n, _)| *n).collect();
    let chunks_by_name = store.get_chunks_by_names_batch(&names).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to batch-fetch transitive caller locations");
        HashMap::new()
    });

    // Build results from batch data
    let mut result = Vec::with_capacity(caller_entries.len());
    for (name, d) in &caller_entries {
        if let Some(chunks) = chunks_by_name.get(*name) {
            if let Some(c) = chunks.first() {
                result.push(TransitiveCaller {
                    name: name.to_string(),
                    file: c.file.clone(),
                    line: c.line_start,
                    depth: *d,
                });
            }
        }
    }

    Ok(result)
}

/// Suggest tests for untested callers in an impact result.
///
/// Loads its own call graph and test chunks — only called when `--suggest-tests`
/// is set, so the normal path pays zero overhead.
pub fn suggest_tests(store: &Store, impact: &ImpactResult) -> Vec<TestSuggestion> {
    let _span = tracing::info_span!("suggest_tests", function = %impact.function_name).entered();
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

    // Batch-fetch file chunks for all unique caller files upfront.
    // This avoids N+1 `get_chunks_by_origin` calls when processing untested callers.
    let unique_files: Vec<String> = {
        let mut seen = HashSet::new();
        impact
            .callers
            .iter()
            .filter_map(|c| {
                let f = c.file.to_string_lossy().to_string();
                if seen.insert(f.clone()) {
                    Some(f)
                } else {
                    None
                }
            })
            .collect()
    };
    let file_refs: Vec<&str> = unique_files.iter().map(|s| s.as_str()).collect();
    let chunks_by_file = store
        .get_chunks_by_origins_batch(&file_refs)
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to batch-fetch file chunks for test suggestions");
            HashMap::new()
        });

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

        // Use pre-fetched file chunks for inline test check, pattern, and language
        let caller_file_key = caller.file.to_string_lossy().to_string();
        let file_chunks = chunks_by_file
            .get(&caller_file_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

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
                match base_name.chars().next() {
                    Some(c) => {
                        let first = c.to_uppercase().to_string();
                        let rest = &base_name[c.len_utf8()..];
                        format!("test{first}{rest}")
                    }
                    None => format!("test_{base_name}"),
                }
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
///
/// EXT-16: These per-language conventions are hardcoded. A future improvement
/// would make them data-driven (e.g., `test_file_pattern` on language definitions),
/// but that requires restructuring across parser/store/impact modules.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
