//! Task — one-shot implementation context for a task description.
//!
//! Combines scout + gather + impact + placement + notes into a single call,
//! loading shared resources (call graph, test chunks) once instead of per-phase.

use std::collections::HashMap;
use std::path::Path;

use crate::gather::{
    bfs_expand, fetch_and_assemble, sort_and_truncate, GatherDirection, GatherOptions,
    GatheredChunk,
};
use crate::impact::{compute_risk_and_tests, RiskLevel, RiskScore, TestInfo};
use crate::scout::{scout_core, ChunkRole, ScoutOptions, ScoutResult};
use crate::where_to_add::FileSuggestion;
use crate::{AnalysisError, Embedder, Store};

/// BFS expansion depth for gather phase (how many call-graph hops from modify targets).
const TASK_GATHER_DEPTH: usize = 2;

/// Maximum BFS-expanded nodes in gather phase (prevents blowup on hub functions).
const TASK_GATHER_MAX_NODES: usize = 100;

/// Multiplier applied to `limit` for gather phase truncation.
const TASK_GATHER_LIMIT_MULTIPLIER: usize = 3;

/// Complete task analysis result.
#[derive(Debug, Clone, serde::Serialize)]
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
#[derive(Debug, Clone, serde::Serialize)]
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
    let graph = store.get_call_graph()?;
    let test_chunks = match store.find_test_chunks() {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(error = %e, "Test chunk loading failed, continuing without tests");
            Vec::new()
        }
    };
    task_with_resources(
        store,
        embedder,
        description,
        root,
        limit,
        &graph,
        &test_chunks,
    )
}

/// Like [`task`] but accepts pre-loaded call graph and test chunks.
///
/// Use this in batch mode where `BatchContext` caches these resources across
/// commands, avoiding repeated loading per pipeline stage.
pub fn task_with_resources(
    store: &Store,
    embedder: &Embedder,
    description: &str,
    root: &Path,
    limit: usize,
    graph: &crate::store::CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> Result<TaskResult, AnalysisError> {
    let _span = tracing::info_span!("task", description_len = description.len(), limit).entered();

    // 1. Embed query
    let query_embedding = embedder.embed_query(description)?;

    // 2. Scout phase
    let scout = scout_core(
        store,
        &query_embedding,
        description,
        root,
        limit,
        &ScoutOptions::default(),
        graph,
        test_chunks,
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
            graph,
            &GatherOptions::default()
                .with_expand_depth(TASK_GATHER_DEPTH)
                .with_direction(GatherDirection::Both)
                .with_max_expanded_nodes(TASK_GATHER_MAX_NODES),
        );

        let (mut chunks, _degraded) = fetch_and_assemble(store, &name_scores, root);
        sort_and_truncate(&mut chunks, limit * TASK_GATHER_LIMIT_MULTIPLIER);
        chunks
    };
    tracing::debug!(
        targets = targets.len(),
        expanded = code.len(),
        "Gather complete"
    );

    // 5. Impact phase — risk scores + affected tests (single BFS per target)
    let (risk, tests) = if targets.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        let target_refs: Vec<&str> = targets.iter().map(|s| s.as_str()).collect();
        let (scores, tests) = compute_risk_and_tests(&target_refs, graph, test_chunks);
        let risk = target_refs
            .iter()
            .zip(scores)
            .map(|(&n, r)| (n.to_string(), r))
            .collect();
        (risk, tests)
    };
    tracing::debug!(risks = risk.len(), tests = tests.len(), "Impact complete");

    // 6. Placement phase — reuse query embedding to avoid redundant ONNX inference
    let placement = match crate::where_to_add::suggest_placement_with_embedding(
        store,
        &query_embedding,
        description,
        3,
    ) {
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
pub fn extract_modify_targets(scout: &ScoutResult) -> Vec<String> {
    scout
        .file_groups
        .iter()
        .flat_map(|g| &g.chunks)
        .filter(|c| c.role == ChunkRole::ModifyTarget)
        .map(|c| c.name.clone())
        .collect()
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

    let code_json: Vec<serde_json::Value> = result.code.iter().map(|c| c.to_json(root)).collect();
    let risk_json: Vec<serde_json::Value> = result.risk.iter().map(|(n, r)| r.to_json(n)).collect();
    let tests_json: Vec<serde_json::Value> = result.tests.iter().map(|t| t.to_json(root)).collect();
    let placement_json: Vec<serde_json::Value> =
        result.placement.iter().map(|s| s.to_json(root)).collect();

    serde_json::json!({
        "description": result.description,
        "scout": scout_json,
        "code": code_json,
        "risk": risk_json,
        "tests": tests_json,
        "placement": placement_json,
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
        // Notes are in scout.relevant_notes, no top-level "notes" key
        assert!(json["scout"]["relevant_notes"].is_array());
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
        assert_eq!(json["scout"]["relevant_notes"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["total_files"], 0);
    }

    // TC-3: task_to_json with populated code/risk/tests/placement
    #[test]
    fn test_task_to_json_populated_values() {
        use crate::gather::GatheredChunk;
        use crate::impact::TestInfo;
        use crate::language::{ChunkType, Language};
        use crate::where_to_add::{FileSuggestion, LocalPatterns};

        let scout = make_scout_result(vec![("fn_a", ChunkRole::ModifyTarget)]);
        let result = TaskResult {
            description: "add caching".to_string(),
            scout,
            code: vec![GatheredChunk {
                name: "fn_a".to_string(),
                file: PathBuf::from("/project/src/lib.rs"),
                line_start: 10,
                line_end: 20,
                language: Language::Rust,
                chunk_type: ChunkType::Function,
                signature: "fn fn_a()".to_string(),
                content: "fn fn_a() { }".to_string(),
                score: 0.9,
                depth: 0,
                source: None,
            }],
            risk: vec![(
                "fn_a".to_string(),
                RiskScore {
                    caller_count: 5,
                    test_count: 1,
                    coverage: 0.2,
                    risk_level: RiskLevel::High,
                    blast_radius: RiskLevel::Medium,
                    score: 4.0,
                },
            )],
            tests: vec![TestInfo {
                name: "test_fn_a".to_string(),
                file: PathBuf::from("/project/tests/a.rs"),
                line: 5,
                call_depth: 1,
            }],
            placement: vec![FileSuggestion {
                file: PathBuf::from("/project/src/lib.rs"),
                score: 0.85,
                insertion_line: 25,
                near_function: "fn_a".to_string(),
                reason: "same module".to_string(),
                patterns: LocalPatterns {
                    imports: vec!["use std::path::Path;".to_string()],
                    naming_convention: "snake_case".to_string(),
                    error_handling: "Result".to_string(),
                    visibility: "pub".to_string(),
                    has_inline_tests: true,
                },
            }],
            summary: TaskSummary {
                total_files: 1,
                total_functions: 1,
                modify_targets: 1,
                high_risk_count: 1,
                test_count: 1,
                stale_count: 0,
            },
        };

        let json = task_to_json(&result, Path::new("/project"));

        // Verify code section values
        let code = json["code"].as_array().unwrap();
        assert_eq!(code.len(), 1);
        assert_eq!(code[0]["name"], "fn_a");
        assert_eq!(code[0]["signature"], "fn fn_a()");

        // Verify risk section values
        let risk = json["risk"].as_array().unwrap();
        assert_eq!(risk.len(), 1);
        assert_eq!(risk[0]["name"], "fn_a");
        assert_eq!(risk[0]["risk_level"], "high");
        assert_eq!(risk[0]["caller_count"], 5);

        // Verify tests section values
        let tests = json["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["name"], "test_fn_a");
        assert_eq!(tests[0]["call_depth"], 1);

        // Verify placement section values
        let placement = json["placement"].as_array().unwrap();
        assert_eq!(placement.len(), 1);
        assert_eq!(placement[0]["near_function"], "fn_a");
        assert_eq!(placement[0]["reason"], "same module");
    }
}
