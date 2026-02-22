//! Task — one-shot implementation context for a task description.
//!
//! Combines scout + gather + impact + placement + notes into a single call,
//! loading shared resources (call graph, test chunks) once instead of per-phase.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::gather::{
    bfs_expand, fetch_and_assemble, sort_and_truncate, GatherDirection, GatherOptions,
    GatheredChunk,
};
use crate::impact::compute_risk_batch;
use crate::impact::{find_affected_tests_with_chunks, RiskLevel, RiskScore, TestInfo};
use crate::scout::{scout_core, ChunkRole, ScoutOptions, ScoutResult};
use crate::where_to_add::FileSuggestion;
use crate::{AnalysisError, Embedder, Store};

/// Complete task analysis result.
pub struct TaskResult {
    /// Original task description.
    pub description: String,
    /// Scout phase: file groups, chunk roles, staleness, notes.
    pub scout: ScoutResult,
    /// Gather phase: BFS-expanded code with full content.
    pub code: Vec<GatheredChunk>,
    /// Impact phase: per-modify-target risk assessment.
    pub risk: Vec<(String, RiskScore)>,
    /// Impact phase: affected tests (deduped across targets).
    pub tests: Vec<TestInfo>,
    /// Placement phase: where to add new code.
    pub placement: Vec<FileSuggestion>,
    /// Aggregated summary counts.
    pub summary: TaskSummary,
}

/// Summary statistics for a task result.
pub struct TaskSummary {
    pub total_files: usize,
    pub total_functions: usize,
    pub modify_targets: usize,
    pub high_risk_count: usize,
    pub test_count: usize,
    pub stale_count: usize,
}

/// Produce complete implementation context for a task description.
///
/// Loads the call graph and test chunks once, then runs scout → gather → impact →
/// placement in sequence, sharing resources across phases.
pub fn task(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    root: &Path,
    limit: usize,
) -> Result<TaskResult, AnalysisError> {
    let _span = tracing::info_span!("task", description_len = description.len(), limit).entered();

    // 1. Embed query
    let query_embedding = embedder
        .embed_query(description)
        .map_err(|e| AnalysisError::Embedder(e.to_string()))?;

    // 2. Load shared resources ONCE
    let graph = store.get_call_graph()?;
    let test_chunks = match store.find_test_chunks() {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(error = %e, "Test chunk loading failed, continuing without tests");
            Vec::new()
        }
    };

    // 3. Scout phase
    let scout = scout_core(
        store,
        &query_embedding,
        description,
        root,
        limit,
        &ScoutOptions::default(),
        &graph,
        &test_chunks,
    )?;
    tracing::debug!(
        file_groups = scout.file_groups.len(),
        functions = scout.summary.total_functions,
        "Scout complete"
    );

    // 4. Gather phase — expand modify targets via BFS
    let targets = extract_modify_targets(&scout);
    let code = if targets.is_empty() {
        Vec::new()
    } else {
        let mut name_scores: HashMap<String, (f32, usize)> =
            targets.iter().map(|n| (n.to_string(), (1.0, 0))).collect();

        bfs_expand(
            &mut name_scores,
            &graph,
            &GatherOptions::default()
                .with_expand_depth(2)
                .with_direction(GatherDirection::Both)
                .with_max_expanded_nodes(100),
        );

        let (mut chunks, _degraded) = fetch_and_assemble(store, &name_scores, root);
        sort_and_truncate(&mut chunks, limit * 3);
        chunks
    };
    tracing::debug!(
        targets = targets.len(),
        expanded = code.len(),
        "Gather complete"
    );

    // 5. Impact phase — risk scores + affected tests
    let risk = if targets.is_empty() {
        Vec::new()
    } else {
        let target_refs: Vec<&str> = targets.iter().map(|s| s.as_str()).collect();
        let scores = compute_risk_batch(&target_refs, &graph, &test_chunks);
        target_refs
            .iter()
            .zip(scores)
            .map(|(&n, r)| (n.to_string(), r))
            .collect()
    };

    let tests = dedup_tests(&targets, &graph, &test_chunks);
    tracing::debug!(risks = risk.len(), tests = tests.len(), "Impact complete");

    // 6. Placement phase
    let placement = match crate::where_to_add::suggest_placement(store, embedder, description, 3) {
        Ok(result) => result.suggestions,
        Err(e) => {
            tracing::warn!(error = %e, "Placement suggestion failed, skipping");
            Vec::new()
        }
    };

    // 7. Assemble result
    let summary = compute_summary(&scout, &risk, &tests);
    tracing::info!(
        files = summary.total_files,
        functions = summary.total_functions,
        targets = summary.modify_targets,
        high_risk = summary.high_risk_count,
        tests = summary.test_count,
        "Task complete"
    );

    Ok(TaskResult {
        description: description.to_string(),
        scout,
        code,
        risk,
        tests,
        placement,
        summary,
    })
}

/// Extract modify target names from scout results.
pub(crate) fn extract_modify_targets(scout: &ScoutResult) -> Vec<String> {
    scout
        .file_groups
        .iter()
        .flat_map(|g| &g.chunks)
        .filter(|c| c.role == ChunkRole::ModifyTarget)
        .map(|c| c.name.clone())
        .collect()
}

/// Deduplicate tests across multiple targets.
fn dedup_tests(
    targets: &[String],
    graph: &crate::store::CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> Vec<TestInfo> {
    let mut all_tests = Vec::new();
    let mut seen = HashSet::new();
    for target in targets {
        for t in find_affected_tests_with_chunks(graph, test_chunks, target, 5) {
            if seen.insert(t.name.clone()) {
                all_tests.push(t);
            }
        }
    }
    all_tests
}

/// Compute summary statistics from task phases.
pub(crate) fn compute_summary(
    scout: &ScoutResult,
    risk: &[(String, RiskScore)],
    tests: &[TestInfo],
) -> TaskSummary {
    let modify_targets = scout
        .file_groups
        .iter()
        .flat_map(|g| &g.chunks)
        .filter(|c| c.role == ChunkRole::ModifyTarget)
        .count();

    let high_risk_count = risk
        .iter()
        .filter(|(_, r)| r.risk_level == RiskLevel::High)
        .count();

    TaskSummary {
        total_files: scout.summary.total_files,
        total_functions: scout.summary.total_functions,
        modify_targets,
        high_risk_count,
        test_count: tests.len(),
        stale_count: scout.summary.stale_count,
    }
}

/// Serialize task result to JSON.
///
/// Uses manual construction since ScoutResult doesn't derive Serialize.
/// Reuses `scout_to_json()` for the scout section.
pub fn task_to_json(result: &TaskResult, root: &Path) -> serde_json::Value {
    let scout_json = crate::scout::scout_to_json(&result.scout, root);

    let code_json: Vec<serde_json::Value> = result
        .code
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": crate::rel_display(&c.file, root),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "language": c.language.to_string(),
                "chunk_type": c.chunk_type.to_string(),
                "signature": c.signature,
                "content": c.content,
                "score": c.score,
                "depth": c.depth,
            })
        })
        .collect();

    let risk_json: Vec<serde_json::Value> = result
        .risk
        .iter()
        .map(|(name, r)| {
            serde_json::json!({
                "name": name,
                "risk_level": format!("{:?}", r.risk_level),
                "blast_radius": format!("{:?}", r.blast_radius),
                "score": r.score,
                "caller_count": r.caller_count,
                "test_count": r.test_count,
                "coverage": r.coverage,
            })
        })
        .collect();

    let tests_json: Vec<serde_json::Value> = result
        .tests
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "file": crate::rel_display(&t.file, root),
                "line": t.line,
                "call_depth": t.call_depth,
            })
        })
        .collect();

    let placement_json: Vec<serde_json::Value> = result
        .placement
        .iter()
        .map(|s| {
            serde_json::json!({
                "file": crate::rel_display(&s.file, root),
                "score": s.score,
                "insertion_line": s.insertion_line,
                "near_function": s.near_function,
                "reason": s.reason,
            })
        })
        .collect();

    let notes_json: Vec<serde_json::Value> = result
        .scout
        .relevant_notes
        .iter()
        .map(|n| {
            serde_json::json!({
                "text": n.text,
                "sentiment": n.sentiment,
                "mentions": n.mentions,
            })
        })
        .collect();

    serde_json::json!({
        "description": result.description,
        "scout": scout_json,
        "code": code_json,
        "risk": risk_json,
        "tests": tests_json,
        "placement": placement_json,
        "notes": notes_json,
        "summary": {
            "total_files": result.summary.total_files,
            "total_functions": result.summary.total_functions,
            "modify_targets": result.summary.modify_targets,
            "high_risk_count": result.summary.high_risk_count,
            "test_count": result.summary.test_count,
            "stale_count": result.summary.stale_count,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scout::{FileGroup, ScoutChunk, ScoutSummary};
    use crate::store::NoteSummary;
    use std::path::PathBuf;

    fn make_scout_chunk(name: &str, role: ChunkRole) -> ScoutChunk {
        ScoutChunk {
            name: name.to_string(),
            chunk_type: crate::language::ChunkType::Function,
            signature: format!("fn {name}()"),
            line_start: 1,
            role,
            caller_count: 3,
            test_count: 1,
            search_score: 0.8,
        }
    }

    fn make_scout_result(chunks: Vec<(&str, ChunkRole)>) -> ScoutResult {
        let scout_chunks: Vec<ScoutChunk> = chunks
            .iter()
            .map(|(name, role)| make_scout_chunk(name, role.clone()))
            .collect();
        let total_functions = scout_chunks.len();

        ScoutResult {
            file_groups: vec![FileGroup {
                file: PathBuf::from("src/lib.rs"),
                relevance_score: 0.7,
                chunks: scout_chunks,
                is_stale: false,
            }],
            relevant_notes: vec![NoteSummary {
                id: "1".to_string(),
                text: "test note".to_string(),
                sentiment: 0.5,
                mentions: vec!["src/lib.rs".to_string()],
            }],
            summary: ScoutSummary {
                total_files: 1,
                total_functions,
                untested_count: 0,
                stale_count: 0,
            },
        }
    }

    #[test]
    fn test_extract_modify_targets() {
        let scout = make_scout_result(vec![
            ("target_fn", ChunkRole::ModifyTarget),
            ("test_fn", ChunkRole::TestToUpdate),
            ("dep_fn", ChunkRole::Dependency),
            ("target2", ChunkRole::ModifyTarget),
        ]);
        let targets = extract_modify_targets(&scout);
        assert_eq!(targets, vec!["target_fn", "target2"]);
    }

    #[test]
    fn test_extract_modify_targets_empty() {
        let scout = make_scout_result(vec![
            ("test_fn", ChunkRole::TestToUpdate),
            ("dep_fn", ChunkRole::Dependency),
        ]);
        let targets = extract_modify_targets(&scout);
        assert!(targets.is_empty());
    }

    #[test]
    fn test_summary_computation() {
        let scout = make_scout_result(vec![
            ("a", ChunkRole::ModifyTarget),
            ("b", ChunkRole::ModifyTarget),
            ("c", ChunkRole::Dependency),
        ]);

        let risk = vec![
            (
                "a".to_string(),
                RiskScore {
                    caller_count: 5,
                    test_count: 0,
                    coverage: 0.0,
                    risk_level: RiskLevel::High,
                    blast_radius: RiskLevel::Medium,
                    score: 5.0,
                },
            ),
            (
                "b".to_string(),
                RiskScore {
                    caller_count: 2,
                    test_count: 2,
                    coverage: 1.0,
                    risk_level: RiskLevel::Low,
                    blast_radius: RiskLevel::Low,
                    score: 0.0,
                },
            ),
        ];

        let tests = vec![TestInfo {
            name: "test_a".to_string(),
            file: PathBuf::from("tests/a.rs"),
            line: 10,
            call_depth: 1,
        }];

        let summary = compute_summary(&scout, &risk, &tests);
        assert_eq!(summary.total_files, 1);
        assert_eq!(summary.total_functions, 3);
        assert_eq!(summary.modify_targets, 2);
        assert_eq!(summary.high_risk_count, 1);
        assert_eq!(summary.test_count, 1);
        assert_eq!(summary.stale_count, 0);
    }

    #[test]
    fn test_summary_empty() {
        let scout = ScoutResult {
            file_groups: Vec::new(),
            relevant_notes: Vec::new(),
            summary: ScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
        };
        let summary = compute_summary(&scout, &[], &[]);
        assert_eq!(summary.total_files, 0);
        assert_eq!(summary.total_functions, 0);
        assert_eq!(summary.modify_targets, 0);
        assert_eq!(summary.high_risk_count, 0);
        assert_eq!(summary.test_count, 0);
        assert_eq!(summary.stale_count, 0);
    }

    #[test]
    fn test_task_to_json_structure() {
        let scout = make_scout_result(vec![("fn_a", ChunkRole::ModifyTarget)]);
        let result = TaskResult {
            description: "test task".to_string(),
            scout,
            code: Vec::new(),
            risk: Vec::new(),
            tests: Vec::new(),
            placement: Vec::new(),
            summary: TaskSummary {
                total_files: 1,
                total_functions: 1,
                modify_targets: 1,
                high_risk_count: 0,
                test_count: 0,
                stale_count: 0,
            },
        };

        let json = task_to_json(&result, Path::new("/project"));
        assert_eq!(json["description"], "test task");
        assert!(json["scout"].is_object());
        assert!(json["code"].is_array());
        assert!(json["risk"].is_array());
        assert!(json["tests"].is_array());
        assert!(json["placement"].is_array());
        assert!(json["notes"].is_array());
        assert!(json["summary"].is_object());
        assert_eq!(json["summary"]["modify_targets"], 1);
    }

    #[test]
    fn test_task_to_json_empty() {
        let result = TaskResult {
            description: "empty".to_string(),
            scout: ScoutResult {
                file_groups: Vec::new(),
                relevant_notes: Vec::new(),
                summary: ScoutSummary {
                    total_files: 0,
                    total_functions: 0,
                    untested_count: 0,
                    stale_count: 0,
                },
            },
            code: Vec::new(),
            risk: Vec::new(),
            tests: Vec::new(),
            placement: Vec::new(),
            summary: TaskSummary {
                total_files: 0,
                total_functions: 0,
                modify_targets: 0,
                high_risk_count: 0,
                test_count: 0,
                stale_count: 0,
            },
        };

        let json = task_to_json(&result, Path::new("/project"));
        assert_eq!(json["code"].as_array().unwrap().len(), 0);
        assert_eq!(json["risk"].as_array().unwrap().len(), 0);
        assert_eq!(json["tests"].as_array().unwrap().len(), 0);
        assert_eq!(json["placement"].as_array().unwrap().len(), 0);
        assert_eq!(json["notes"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["total_files"], 0);
    }

    #[test]
    fn test_dedup_tests_removes_duplicates() {
        // Can't easily test dedup_tests without a real graph, but we can test
        // the HashSet logic directly
        let mut all_tests: Vec<TestInfo> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        let test_items = vec![
            TestInfo {
                name: "test_a".to_string(),
                file: PathBuf::from("tests/a.rs"),
                line: 1,
                call_depth: 1,
            },
            TestInfo {
                name: "test_a".to_string(), // duplicate
                file: PathBuf::from("tests/a.rs"),
                line: 1,
                call_depth: 2,
            },
            TestInfo {
                name: "test_b".to_string(),
                file: PathBuf::from("tests/b.rs"),
                line: 5,
                call_depth: 1,
            },
        ];

        for t in test_items {
            if seen.insert(t.name.clone()) {
                all_tests.push(t);
            }
        }

        assert_eq!(all_tests.len(), 2);
        assert_eq!(all_tests[0].name, "test_a");
        assert_eq!(all_tests[0].call_depth, 1); // keeps first occurrence
        assert_eq!(all_tests[1].name, "test_b");
    }
}
