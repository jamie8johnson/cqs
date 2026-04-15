//! Cross-project impact analysis and trace.
//!
//! Extends single-project impact analysis and trace to work across
//! multiple project stores via `CrossProjectContext`.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::store::calls::cross_project::CrossProjectContext;
use crate::store::helpers::StoreError;

use super::types::{ImpactResult, TestInfo, TransitiveCaller};

/// A single hop in a cross-project trace path.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossProjectHop {
    /// Function name at this hop.
    pub name: String,
    /// Which project this function lives in.
    pub project: String,
}

/// Result of a cross-project trace between two functions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrossProjectTraceResult {
    pub source: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Vec<CrossProjectHop>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<usize>,
    pub found: bool,
}

/// Run cross-project impact analysis: BFS caller traversal across all stores.
///
/// At each BFS level, queries all projects for callers of the current frontier.
/// When a caller is found in a different project than its callee, a debug trace
/// is emitted for cross-boundary visibility.
///
/// Returns an `ImpactResult` with merged callers, tests, and transitive callers
/// from all projects.
pub fn analyze_impact_cross(
    ctx: &mut CrossProjectContext,
    name: &str,
    depth: usize,
    suggest_tests: bool,
    include_types: bool,
) -> Result<ImpactResult, StoreError> {
    let _span = tracing::info_span!(
        "analyze_impact_cross",
        target = name,
        depth,
        suggest_tests,
        include_types,
        projects = ctx.project_count()
    )
    .entered();

    // BFS: reverse traversal across all projects
    let mut visited: HashMap<String, (usize, String)> = HashMap::new(); // name -> (depth, project)
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(name.to_string(), (0, String::new()));
    queue.push_back((name.to_string(), 0));

    while let Some((current, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }

        let callers = ctx.get_callers_cross(&current)?;
        for caller in callers {
            if !visited.contains_key(&caller.caller.name) {
                // Detect cross-boundary hop
                if let Some((_, current_project)) = visited.get(&current) {
                    if !current_project.is_empty() && *current_project != caller.project {
                        tracing::debug!(
                            from_project = %current_project,
                            to_project = %caller.project,
                            callee = %current,
                            caller = %caller.caller.name,
                            "Cross-boundary hop in impact BFS"
                        );
                    }
                }

                visited.insert(caller.caller.name.clone(), (d + 1, caller.project.clone()));
                queue.push_back((caller.caller.name.clone(), d + 1));
            }
        }
    }

    // Build transitive callers (everything at depth > 0, excluding target)
    // TODO: resolve file/line from CallGraph (cross-project stores don't track source locations in edges yet)
    let caller_count = visited
        .iter()
        .filter(|(n, (d, _))| *d > 0 && n.as_str() != name)
        .count();
    if caller_count > 0 {
        tracing::warn!(
            count = caller_count,
            "Cross-project callers have empty file/line; resolve from CallGraph when edge metadata is available"
        );
    }
    let mut transitive_callers: Vec<TransitiveCaller> = visited
        .iter()
        .filter(|(n, (d, _))| *d > 0 && n.as_str() != name)
        .map(|(n, (d, _))| TransitiveCaller {
            name: n.clone(),
            file: std::path::PathBuf::new(),
            line: 0,
            depth: *d,
        })
        .collect();
    transitive_callers.sort_by_key(|tc| tc.depth);

    // Build direct callers (depth == 1)
    let callers = visited
        .iter()
        .filter(|(_, (d, _))| *d == 1)
        .map(|(n, _)| super::types::CallerDetail {
            name: n.clone(),
            file: std::path::PathBuf::new(),
            line: 0,
            call_line: 0,
            snippet: None,
        })
        .collect();

    // Find affected tests across all projects
    let tests = if suggest_tests {
        find_affected_tests_cross(ctx, &visited)?
    } else {
        Vec::new()
    };

    // Type impact is not supported cross-project (would need cross-store type edges)
    if include_types {
        tracing::warn!("--type-impact not supported in cross-project mode");
    }

    Ok(ImpactResult {
        function_name: name.to_string(),
        callers,
        tests,
        transitive_callers,
        type_impacted: Vec::new(),
        degraded: false,
    })
}

/// Find tests that exercise any of the visited functions across all projects.
fn find_affected_tests_cross(
    ctx: &mut CrossProjectContext,
    visited: &HashMap<String, (usize, String)>,
) -> Result<Vec<TestInfo>, StoreError> {
    let _span = tracing::info_span!("find_affected_tests_cross").entered();
    let test_chunks = ctx.find_test_chunks_cross()?;

    let visited_names: HashSet<&str> = visited.keys().map(|s| s.as_str()).collect();

    let tests: Vec<TestInfo> = test_chunks
        .iter()
        .filter(|tc| visited_names.contains(tc.chunk.name.as_str()))
        .map(|tc| {
            let depth = visited.get(&tc.chunk.name).map(|(d, _)| *d).unwrap_or(0);
            TestInfo {
                name: tc.chunk.name.clone(),
                file: tc.chunk.file.clone(),
                line: tc.chunk.line_start,
                call_depth: depth,
            }
        })
        .collect();

    Ok(tests)
}

/// Find shortest path between two functions across all projects via forward BFS.
///
/// At each BFS level, queries all projects for callees of the current frontier.
/// The result includes which project each hop belongs to. Returns `None` if
/// no path exists within `max_depth`.
pub fn trace_cross(
    ctx: &mut CrossProjectContext,
    source: &str,
    target: &str,
    max_depth: usize,
) -> Result<Option<Vec<CrossProjectHop>>, StoreError> {
    let _span = tracing::info_span!(
        "trace_cross",
        source,
        target,
        max_depth,
        projects = ctx.project_count()
    )
    .entered();

    if source == target {
        return Ok(Some(vec![CrossProjectHop {
            name: source.to_string(),
            project: String::new(),
        }]));
    }

    // BFS via forward edges across all projects
    // predecessor map: node -> (predecessor_name, node_project)
    let mut visited: HashMap<String, (String, String)> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();

    visited.insert(source.to_string(), (String::new(), String::new()));
    queue.push_back((source.to_string(), 0));

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        let callees = ctx.get_callees_cross(&current)?;
        for callee in callees {
            if callee.name == target {
                // Found! Reconstruct path.
                // Detect cross-boundary on final hop
                if let Some((_, current_project)) = visited.get(&current) {
                    if !current_project.is_empty() && *current_project != callee.project {
                        tracing::debug!(
                            from_project = %current_project,
                            to_project = %callee.project,
                            caller = %current,
                            callee = %callee.name,
                            "Cross-boundary hop in trace BFS"
                        );
                    }
                }

                let mut path = vec![CrossProjectHop {
                    name: callee.name.clone(),
                    project: callee.project.clone(),
                }];
                let mut node = current.clone();
                loop {
                    let (pred, proj) = visited.get(&node).cloned().unwrap_or_default();
                    path.push(CrossProjectHop {
                        name: node.clone(),
                        project: proj,
                    });
                    if pred.is_empty() {
                        break;
                    }
                    node = pred;
                }
                path.reverse();
                return Ok(Some(path));
            }

            if !visited.contains_key(&callee.name) {
                // Detect cross-boundary hop
                if let Some((_, current_project)) = visited.get(&current) {
                    if !current_project.is_empty() && *current_project != callee.project {
                        tracing::debug!(
                            from_project = %current_project,
                            to_project = %callee.project,
                            caller = %current,
                            callee = %callee.name,
                            "Cross-boundary hop in trace BFS"
                        );
                    }
                }

                visited.insert(
                    callee.name.clone(),
                    (current.clone(), callee.project.clone()),
                );
                queue.push_back((callee.name.clone(), depth + 1));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::calls::cross_project::NamedStore;
    use crate::Store;
    use std::collections::HashMap as StdMap;

    /// Helper: build a NamedStore backed by a temp Store with synthetic call edges.
    // NOTE: similar helper exists in store/calls/cross_project.rs
    fn make_named_store(name: &str, forward_edges: Vec<(&str, &str)>) -> NamedStore {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        let model_info = crate::store::helpers::ModelInfo::default();
        store.init(&model_info).unwrap();

        for (caller, callee) in &forward_edges {
            store
                .rt
                .block_on(async {
                    sqlx::query(
                        "INSERT OR IGNORE INTO function_calls (file, caller_name, callee_name, caller_line, call_line)
                         VALUES ('test.rs', ?1, ?2, 1, 1)",
                    )
                    .bind(caller)
                    .bind(callee)
                    .execute(&store.pool)
                    .await
                })
                .unwrap();
        }

        // Keep the tempdir alive so the db file survives for the test duration.
        // `into_path` disables automatic cleanup; tests are short-lived so this is fine.
        let _keep = dir.into_path();

        NamedStore {
            name: name.to_string(),
            store: store.into_readonly(),
        }
    }

    // ===== Cross-project impact tests =====

    #[test]
    fn test_cross_project_impact_cross_boundary() {
        // Project A: caller_a -> shared_fn
        // Project B: caller_b -> shared_fn
        // Impact of shared_fn should find callers in both projects.
        let store_a = make_named_store("proj_a", vec![("caller_a", "shared_fn")]);
        let store_b = make_named_store("proj_b", vec![("caller_b", "shared_fn")]);

        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let result = analyze_impact_cross(&mut ctx, "shared_fn", 3, false, false).unwrap();

        assert_eq!(result.function_name, "shared_fn");
        assert_eq!(
            result.callers.len(),
            2,
            "Should find callers from both projects"
        );

        let caller_names: HashSet<&str> = result.callers.iter().map(|c| c.name.as_str()).collect();
        assert!(caller_names.contains("caller_a"));
        assert!(caller_names.contains("caller_b"));
    }

    #[test]
    fn test_cross_project_impact_depth_limit() {
        // Project A: deep -> mid -> target
        // With depth=1, should only find mid, not deep.
        let store_a = make_named_store("proj_a", vec![("deep", "mid"), ("mid", "target")]);

        let mut ctx = CrossProjectContext::new(vec![store_a]);
        let result = analyze_impact_cross(&mut ctx, "target", 1, false, false).unwrap();

        let caller_names: HashSet<&str> = result.callers.iter().map(|c| c.name.as_str()).collect();
        assert!(caller_names.contains("mid"), "mid is at depth 1");
        assert!(
            !caller_names.contains("deep"),
            "deep is at depth 2, beyond limit"
        );
    }

    // ===== Cross-project trace tests =====

    #[test]
    fn test_cross_project_trace_found() {
        // Project A: source -> mid
        // Project B: mid -> target
        // Trace from source to target should cross the boundary.
        let store_a = make_named_store("proj_a", vec![("source", "mid")]);
        let store_b = make_named_store("proj_b", vec![("mid", "target")]);

        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let result = trace_cross(&mut ctx, "source", "target", 10).unwrap();

        assert!(result.is_some(), "Should find path across projects");
        let path = result.unwrap();
        assert_eq!(path.len(), 3); // source -> mid -> target

        let names: Vec<&str> = path.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["source", "mid", "target"]);
    }

    #[test]
    fn test_cross_project_trace_no_path() {
        // Project A: source -> mid (no edge to target)
        // Project B: unrelated -> target
        let store_a = make_named_store("proj_a", vec![("source", "mid")]);
        let store_b = make_named_store("proj_b", vec![("unrelated", "target")]);

        let mut ctx = CrossProjectContext::new(vec![store_a, store_b]);
        let result = trace_cross(&mut ctx, "source", "target", 10).unwrap();

        assert!(
            result.is_none(),
            "No path should exist between disconnected functions"
        );
    }
}
