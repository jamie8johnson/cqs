//! Scout — pre-investigation dashboard for task planning
//!
//! Given a task description, searches for relevant code, groups by file,
//! and returns signatures + caller/test counts + staleness + relevant notes.
//! Optimized for planning, not reading.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::impact::compute_hints_with_graph;
use crate::store::{ChunkSummary, NoteSummary, SearchFilter};
use crate::{AnalysisError, Embedder, Store};

/// Role classification for chunks in scout results
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ChunkRole {
    /// High-relevance function likely needing modification (score >= 0.5)
    ModifyTarget,
    /// Test that may need updating
    TestToUpdate,
    /// Lower-relevance dependency
    Dependency,
}

impl ChunkRole {
    /// Stable string representation for JSON serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            ChunkRole::ModifyTarget => "modify_target",
            ChunkRole::TestToUpdate => "test_to_update",
            ChunkRole::Dependency => "dependency",
        }
    }
}

/// A chunk in the scout result with hints
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoutChunk {
    /// Function/class/etc. name
    pub name: String,
    /// Type of code element
    pub chunk_type: crate::language::ChunkType,
    /// Function signature
    pub signature: String,
    /// Starting line number
    pub line_start: u32,
    /// Role classification
    pub role: ChunkRole,
    /// Number of callers
    pub caller_count: usize,
    /// Number of tests reaching this function
    pub test_count: usize,
    /// Semantic search score (0.0-1.0)
    pub search_score: f32,
}

/// A file group in the scout result
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileGroup {
    /// File path
    pub file: PathBuf,
    /// Aggregate relevance score
    pub relevance_score: f32,
    /// Chunks in this file
    pub chunks: Vec<ScoutChunk>,
    /// Whether the file is stale (modified since last index)
    pub is_stale: bool,
}

/// Summary counts
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoutSummary {
    pub total_files: usize,
    pub total_functions: usize,
    pub untested_count: usize,
    pub stale_count: usize,
}

/// Complete scout result
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScoutResult {
    pub file_groups: Vec<FileGroup>,
    #[serde(skip)]
    pub relevant_notes: Vec<NoteSummary>,
    pub summary: ScoutSummary,
}

/// Minimum relative gap (%) between consecutive scores to split ModifyTarget
/// from Dependency. Below this, all non-test chunks are treated as a single
/// cluster and only the top result becomes a ModifyTarget.
const MIN_GAP_RATIO: f32 = 0.10;

/// Default number of search results for scout.
pub const DEFAULT_SCOUT_SEARCH_LIMIT: usize = 15;

/// Default minimum search score threshold for scout.
pub const DEFAULT_SCOUT_SEARCH_THRESHOLD: f32 = 0.2;

/// Options for customizing scout behavior.
#[derive(Debug, Clone)]
pub struct ScoutOptions {
    /// Number of search results to retrieve (default: 15)
    pub search_limit: usize,
    /// Minimum search score threshold (default: 0.2)
    pub search_threshold: f32,
    /// Minimum relative gap between consecutive scores to split ModifyTarget
    /// from Dependency (default: 0.10). Lower values create more ModifyTargets.
    pub min_gap_ratio: f32,
}

impl Default for ScoutOptions {
    fn default() -> Self {
        Self {
            search_limit: DEFAULT_SCOUT_SEARCH_LIMIT,
            search_threshold: DEFAULT_SCOUT_SEARCH_THRESHOLD,
            min_gap_ratio: MIN_GAP_RATIO,
        }
    }
}

/// Run scout analysis for a task description.
///
/// Uses default search parameters. For custom parameters, use [`scout_with_options`].
pub fn scout(
    store: &Store,
    embedder: &Embedder,
    task: &str,
    root: &Path,
    limit: usize,
) -> Result<ScoutResult, AnalysisError> {
    scout_with_options(store, embedder, task, root, limit, &ScoutOptions::default())
}

/// Run scout analysis with configurable search parameters.
pub fn scout_with_options(
    store: &Store,
    embedder: &Embedder,
    task: &str,
    root: &Path,
    limit: usize,
    opts: &ScoutOptions,
) -> Result<ScoutResult, AnalysisError> {
    let _span = tracing::info_span!("scout", task_len = task.len(), limit).entered();
    let query_embedding = embedder.embed_query(task)?;
    let graph = store.get_call_graph()?;
    let test_chunks = match store.find_test_chunks() {
        Ok(tc) => tc,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load test chunks, scout will skip test analysis");
            Vec::new()
        }
    };
    scout_core(
        store,
        &query_embedding,
        task,
        root,
        limit,
        opts,
        &graph,
        &test_chunks,
    )
}

/// Like [`scout_with_options`] but accepts pre-loaded call graph and test chunks.
///
/// Use when the caller already has these resources (e.g., batch mode where
/// `BatchContext` caches them across commands).
#[allow(clippy::too_many_arguments)]
pub fn scout_with_resources(
    store: &Store,
    embedder: &Embedder,
    task: &str,
    root: &Path,
    limit: usize,
    opts: &ScoutOptions,
    graph: &crate::store::CallGraph,
    test_chunks: &[crate::store::ChunkSummary],
) -> Result<ScoutResult, AnalysisError> {
    let _span = tracing::info_span!("scout_with_resources", task_len = task.len(), limit).entered();
    let query_embedding = embedder.embed_query(task)?;
    scout_core(
        store,
        &query_embedding,
        task,
        root,
        limit,
        opts,
        graph,
        test_chunks,
    )
}

/// Core scout implementation accepting pre-loaded resources.
///
/// Use this when you already have the call graph and test chunks loaded
/// (e.g., from `cqs task` which shares them across phases).
#[allow(clippy::too_many_arguments)]
pub(crate) fn scout_core(
    store: &Store,
    query_embedding: &crate::Embedding,
    task: &str,
    root: &Path,
    limit: usize,
    opts: &ScoutOptions,
    graph: &crate::store::CallGraph,
    test_chunks: &[ChunkSummary],
) -> Result<ScoutResult, AnalysisError> {
    // 1. Search
    let filter = SearchFilter {
        enable_rrf: true,
        query_text: task.to_string(),
        ..SearchFilter::default()
    };

    let results = store.search_filtered(
        query_embedding,
        &filter,
        opts.search_limit,
        opts.search_threshold,
    )?;

    tracing::debug!(search_results = results.len(), "Scout search complete");

    if results.is_empty() {
        return Ok(ScoutResult {
            file_groups: Vec::new(),
            relevant_notes: Vec::new(),
            summary: ScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
        });
    }

    // 2. Group by file
    let mut file_map: HashMap<PathBuf, Vec<(f32, &ChunkSummary)>> = HashMap::new();
    for r in &results {
        file_map
            .entry(r.chunk.file.clone())
            .or_default()
            .push((r.score, &r.chunk));
    }

    // 3. Batch caller/callee counts
    let all_names: Vec<&str> = results.iter().map(|r| r.chunk.name.as_str()).collect();
    let caller_counts = match store.get_caller_counts_batch(&all_names) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch caller counts");
            HashMap::new()
        }
    };

    // 5. Check staleness
    let origins: Vec<String> = file_map
        .keys()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let origin_refs: Vec<&str> = origins.iter().map(|s| s.as_str()).collect();
    let stale_set = match store.check_origins_stale(&origin_refs, root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check staleness");
            HashSet::new()
        }
    };

    // 6. Compute dynamic modify-target threshold via gap detection.
    // Scores naturally cluster: items matching both semantic + keyword rank
    // higher than keyword-only or semantic-only. Find the largest relative gap
    // in the sorted scores and split there. Scale-independent — works on
    // cosine (0-1), RRF (~0.01-0.03), or any future scoring.
    let modify_threshold = compute_modify_threshold(&results, opts.min_gap_ratio);
    tracing::debug!(modify_threshold, "Gap-based threshold computed");

    // 7. Build file groups
    let mut groups: Vec<FileGroup> = file_map
        .into_iter()
        .map(|(file, chunks)| {
            let relevance_score = chunks.iter().map(|(s, _)| s).sum::<f32>() / chunks.len() as f32;
            let is_stale = stale_set.contains(&file.to_string_lossy().to_string());

            let scout_chunks: Vec<ScoutChunk> = chunks
                .iter()
                .map(|(score, chunk)| {
                    let hints = compute_hints_with_graph(
                        graph,
                        test_chunks,
                        &chunk.name,
                        caller_counts.get(&chunk.name).map(|&c| c as usize),
                    );

                    let role = classify_role(
                        *score,
                        &chunk.name,
                        &chunk.file.to_string_lossy(),
                        modify_threshold,
                    );

                    ScoutChunk {
                        name: chunk.name.clone(),
                        chunk_type: chunk.chunk_type,
                        signature: chunk.signature.clone(),
                        line_start: chunk.line_start,
                        role,
                        caller_count: hints.caller_count,
                        test_count: hints.test_count,
                        search_score: *score,
                    }
                })
                .collect();

            FileGroup {
                file,
                relevance_score,
                chunks: scout_chunks,
                is_stale,
            }
        })
        .collect();

    // Sort by relevance, take top N
    groups.sort_by(|a, b| b.relevance_score.total_cmp(&a.relevance_score));
    groups.truncate(limit);

    // 7. Find relevant notes by mention overlap
    let result_files: HashSet<String> = groups
        .iter()
        .map(|g| crate::rel_display(&g.file, root))
        .collect();

    let relevant_notes = find_relevant_notes(store, &result_files);

    // 8. Build summary
    let total_functions: usize = groups.iter().map(|g| g.chunks.len()).sum();
    let untested_count: usize = groups
        .iter()
        .flat_map(|g| &g.chunks)
        .filter(|c| c.test_count == 0 && c.role != ChunkRole::TestToUpdate)
        .count();
    let stale_count = groups.iter().filter(|g| g.is_stale).count();

    Ok(ScoutResult {
        summary: ScoutSummary {
            total_files: groups.len(),
            total_functions,
            untested_count,
            stale_count,
        },
        file_groups: groups,
        relevant_notes,
    })
}

/// Find the natural score boundary between high-relevance (ModifyTarget) and
/// low-relevance (Dependency) chunks using gap detection.
///
/// Sorts non-test scores descending, finds the largest relative gap between
/// consecutive scores, and returns the score at the top of the gap (i.e. the
/// lowest score that still qualifies as a ModifyTarget).
/// Guarantees at least 1 ModifyTarget, at most half the non-test results.
/// If no clear gap exists (all gaps < 10%), only the top result qualifies.
/// Tied scores at the threshold are included as ModifyTargets.
fn compute_modify_threshold(results: &[crate::store::SearchResult], min_gap_ratio: f32) -> f32 {
    let mut scores: Vec<f32> = results
        .iter()
        .filter(|r| !crate::is_test_chunk(&r.chunk.name, &r.chunk.file.to_string_lossy()))
        .map(|r| r.score)
        .collect();
    scores.sort_by(|a, b| b.total_cmp(a));

    if scores.len() <= 1 {
        return scores.first().copied().unwrap_or(f32::MAX);
    }

    // Search the top half for the largest relative gap
    let max_targets = scores.len() / 2;
    let mut best_gap = 0.0f32;
    let mut split_at = 0; // index of last item in the top cluster

    for i in 0..max_targets.min(scores.len() - 1) {
        if scores[i] > 0.0 {
            let gap = (scores[i] - scores[i + 1]) / scores[i];
            if gap > best_gap {
                best_gap = gap;
                split_at = i;
            }
        }
    }

    // No clear gap → only top result is a ModifyTarget
    if best_gap < min_gap_ratio {
        return scores[0];
    }

    scores[split_at]
}

/// Classify a chunk's role based on score, name/file, and dynamic threshold.
fn classify_role(score: f32, name: &str, file: &str, modify_threshold: f32) -> ChunkRole {
    if crate::is_test_chunk(name, file) {
        ChunkRole::TestToUpdate
    } else if score >= modify_threshold {
        ChunkRole::ModifyTarget
    } else {
        ChunkRole::Dependency
    }
}

/// Find notes whose mentions overlap with result file paths.
///
/// Matches when a mention is a suffix of a result file path (e.g., mention "search.rs"
/// matches result "src/search.rs") at a path-component boundary.
/// This avoids false matches from short concept words like "audit" or "security".
fn find_relevant_notes(store: &Store, result_files: &HashSet<String>) -> Vec<NoteSummary> {
    let all_notes = match store.list_notes_summaries() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to list notes");
            return Vec::new();
        }
    };

    all_notes
        .into_iter()
        .filter(|note| {
            note.mentions
                .iter()
                .any(|m| result_files.iter().any(|f| note_mention_matches_file(m, f)))
        })
        .collect()
}

/// Check if a note mention matches a result file path.
///
/// Only file-like mentions (containing '.' or '/') are considered.
/// Match requires the file path to end with the mention at a path-component
/// boundary (preceded by '/' or at start of string).
fn note_mention_matches_file(mention: &str, file: &str) -> bool {
    let mention = mention.replace('\\', "/");
    let file = file.replace('\\', "/");
    if !mention.contains('.') && !mention.contains('/') {
        return false;
    }
    file.ends_with(&mention)
        && (file.len() == mention.len() || file.as_bytes()[file.len() - mention.len() - 1] == b'/')
}

/// Serialize scout result to JSON
pub fn scout_to_json(result: &ScoutResult, root: &Path) -> serde_json::Value {
    let groups_json: Vec<_> = result
        .file_groups
        .iter()
        .map(|g| {
            let rel = crate::rel_display(&g.file, root);

            let chunks_json: Vec<_> = g
                .chunks
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "chunk_type": c.chunk_type.to_string(),
                        "signature": c.signature,
                        "line_start": c.line_start,
                        "role": c.role.as_str(),
                        "caller_count": c.caller_count,
                        "test_count": c.test_count,
                        "search_score": c.search_score,
                    })
                })
                .collect();

            serde_json::json!({
                "file": rel,
                "relevance_score": g.relevance_score,
                "is_stale": g.is_stale,
                "chunks": chunks_json,
            })
        })
        .collect();

    let notes_json: Vec<_> = result
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
        "file_groups": groups_json,
        "relevant_notes": notes_json,
        "summary": {
            "total_files": result.summary.total_files,
            "total_functions": result.summary.total_functions,
            "untested_count": result.summary.untested_count,
            "stale_count": result.summary.stale_count,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_role_modify_target() {
        assert_eq!(
            classify_role(0.6, "search_filtered", "src/search.rs", 0.5),
            ChunkRole::ModifyTarget
        );
        assert_eq!(
            classify_role(0.5, "do_something", "src/lib.rs", 0.5),
            ChunkRole::ModifyTarget
        );
    }

    #[test]
    fn test_classify_role_dependency() {
        assert_eq!(
            classify_role(0.49, "helper_fn", "src/lib.rs", 0.5),
            ChunkRole::Dependency
        );
        assert_eq!(
            classify_role(0.3, "utility", "src/lib.rs", 0.5),
            ChunkRole::Dependency
        );
    }

    #[test]
    fn test_classify_role_test() {
        // Name-based test detection
        assert_eq!(
            classify_role(0.9, "test_search", "src/lib.rs", 0.5),
            ChunkRole::TestToUpdate
        );
        assert_eq!(
            classify_role(0.3, "test_helper", "src/lib.rs", 0.5),
            ChunkRole::TestToUpdate
        );
        assert_eq!(
            classify_role(0.8, "TestSuite", "src/lib.rs", 0.5),
            ChunkRole::TestToUpdate
        );
        // File-based test detection
        assert_eq!(
            classify_role(0.9, "helper_fn", "tests/integration.rs", 0.5),
            ChunkRole::TestToUpdate
        );
    }

    fn mock_result(name: &str, file: &str, score: f32) -> crate::store::SearchResult {
        crate::store::SearchResult {
            chunk: ChunkSummary {
                id: name.to_string(),
                file: std::path::PathBuf::from(file),
                language: crate::language::Language::Rust,
                chunk_type: crate::language::ChunkType::Function,
                name: name.to_string(),
                signature: String::new(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 10,
                parent_id: None,
            },
            score,
        }
    }

    #[test]
    fn test_compute_modify_threshold_clear_gap() {
        // RRF-like scores: top 3 in both semantic+FTS, bottom 3 in one only
        let results = vec![
            mock_result("a", "src/a.rs", 0.033),
            mock_result("b", "src/b.rs", 0.031),
            mock_result("c", "src/c.rs", 0.030),
            mock_result("d", "src/d.rs", 0.016), // big gap here
            mock_result("e", "src/e.rs", 0.015),
            mock_result("f", "src/f.rs", 0.014),
        ];
        let threshold = compute_modify_threshold(&results, MIN_GAP_RATIO);
        // Should split at the gap: 0.030 is the cutoff
        assert!(threshold >= 0.030);
        assert!(threshold <= 0.033);
    }

    #[test]
    fn test_compute_modify_threshold_no_gap() {
        // Nearly uniform scores — no clear gap
        let results = vec![
            mock_result("a", "src/a.rs", 0.020),
            mock_result("b", "src/b.rs", 0.019),
            mock_result("c", "src/c.rs", 0.018),
            mock_result("d", "src/d.rs", 0.017),
        ];
        let threshold = compute_modify_threshold(&results, MIN_GAP_RATIO);
        // All gaps < 10%, only top result qualifies
        assert!((threshold - 0.020).abs() < f32::EPSILON);
    }

    #[test]
    fn test_compute_modify_threshold_single() {
        let results = vec![mock_result("a", "src/a.rs", 0.05)];
        assert!((compute_modify_threshold(&results, MIN_GAP_RATIO) - 0.05).abs() < f32::EPSILON);
    }

    #[test]
    fn test_compute_modify_threshold_empty() {
        assert_eq!(compute_modify_threshold(&[], MIN_GAP_RATIO), f32::MAX);
    }

    #[test]
    fn test_compute_modify_threshold_skips_tests() {
        // test_foo is a test — should be excluded from threshold computation
        let results = vec![
            mock_result("test_foo", "src/a.rs", 0.050), // test, ignored
            mock_result("bar", "src/b.rs", 0.020),
            mock_result("baz", "src/c.rs", 0.010),
        ];
        let threshold = compute_modify_threshold(&results, MIN_GAP_RATIO);
        // Only bar and baz considered; gap between 0.020 and 0.010 is 50%
        assert!((threshold - 0.020).abs() < f32::EPSILON);
    }

    #[test]
    fn test_compute_modify_threshold_cosine_scale() {
        // Works on cosine 0-1 scale too — scale-independent
        let results = vec![
            mock_result("a", "src/a.rs", 0.95),
            mock_result("b", "src/b.rs", 0.90),
            mock_result("c", "src/c.rs", 0.50), // big gap
            mock_result("d", "src/d.rs", 0.45),
        ];
        let threshold = compute_modify_threshold(&results, MIN_GAP_RATIO);
        assert!(threshold >= 0.90);
    }

    #[test]
    fn test_note_mention_matches_file() {
        // Positive: suffix at path boundary
        assert!(note_mention_matches_file("search.rs", "src/search.rs"));
        assert!(note_mention_matches_file("src/search.rs", "src/search.rs"));
        assert!(note_mention_matches_file("cli/mod.rs", "src/cli/mod.rs"));
        assert!(note_mention_matches_file("mod.rs", "src/cli/mod.rs"));

        // Negative: not at path boundary (partial filename)
        assert!(!note_mention_matches_file("od.rs", "src/cli/mod.rs"));
        assert!(!note_mention_matches_file("earch.rs", "src/search.rs"));

        // Negative: not file-like (no '.' or '/')
        assert!(!note_mention_matches_file("audit", "src/audit.rs"));
        assert!(!note_mention_matches_file("search", "src/search.rs"));

        // Negative: mention longer than file
        assert!(!note_mention_matches_file(
            "extra/src/search.rs",
            "search.rs"
        ));

        // Edge: exact match
        assert!(note_mention_matches_file("src/scout.rs", "src/scout.rs"));

        // Edge: mention with '/' but no match
        assert!(!note_mention_matches_file(
            "other/search.rs",
            "src/search.rs"
        ));
    }

    #[test]
    fn test_note_mention_matches_file_backslash() {
        assert!(note_mention_matches_file("scout.rs", "src\\scout.rs"));
        assert!(note_mention_matches_file("cli\\mod.rs", "src\\cli\\mod.rs"));
        assert!(!note_mention_matches_file("od.rs", "src\\cli\\mod.rs"));
    }

    #[test]
    fn test_scout_summary_nonzero() {
        // Verify struct fields are stored and accessible (not tautologically zero)
        let summary = ScoutSummary {
            total_files: 3,
            total_functions: 15,
            untested_count: 4,
            stale_count: 2,
        };
        assert_eq!(summary.total_files, 3);
        assert_eq!(summary.total_functions, 15);
        assert_eq!(summary.untested_count, 4);
        assert_eq!(summary.stale_count, 2);
    }

    #[test]
    fn test_scout_to_json_empty() {
        let result = ScoutResult {
            file_groups: Vec::new(),
            relevant_notes: Vec::new(),
            summary: ScoutSummary {
                total_files: 0,
                total_functions: 0,
                untested_count: 0,
                stale_count: 0,
            },
        };
        let json = scout_to_json(&result, Path::new("/project"));
        assert_eq!(json["file_groups"].as_array().unwrap().len(), 0);
        assert_eq!(json["relevant_notes"].as_array().unwrap().len(), 0);
        assert_eq!(json["summary"]["total_files"], 0);
    }

    #[test]
    fn test_chunk_role_equality() {
        assert_eq!(ChunkRole::ModifyTarget, ChunkRole::ModifyTarget);
        assert_ne!(ChunkRole::ModifyTarget, ChunkRole::Dependency);
        assert_ne!(ChunkRole::TestToUpdate, ChunkRole::Dependency);
    }

    #[test]
    fn test_chunk_role_as_str() {
        assert_eq!(ChunkRole::ModifyTarget.as_str(), "modify_target");
        assert_eq!(ChunkRole::TestToUpdate.as_str(), "test_to_update");
        assert_eq!(ChunkRole::Dependency.as_str(), "dependency");
    }

    // TC-4: compute_modify_threshold with all-test-chunk inputs
    #[test]
    fn test_compute_modify_threshold_all_tests() {
        let results = vec![
            mock_result("test_a", "src/a.rs", 0.9),
            mock_result("test_b", "src/b.rs", 0.8),
            mock_result("test_c", "src/c.rs", 0.7),
        ];
        let threshold = compute_modify_threshold(&results, MIN_GAP_RATIO);
        // All chunks are tests → no non-test scores → should return f32::MAX
        assert_eq!(threshold, f32::MAX);
    }

    // TC-6: classify_role at exact threshold with test names
    #[test]
    fn test_classify_role_exact_threshold_test_name() {
        // Test name at exact threshold — test detection takes priority over score
        assert_eq!(
            classify_role(0.5, "test_foo", "src/lib.rs", 0.5),
            ChunkRole::TestToUpdate
        );
        // Non-test name at exact threshold — should be ModifyTarget
        assert_eq!(
            classify_role(0.5, "process_data", "src/lib.rs", 0.5),
            ChunkRole::ModifyTarget
        );
        // Test name below threshold — still TestToUpdate
        assert_eq!(
            classify_role(0.3, "test_bar", "src/lib.rs", 0.5),
            ChunkRole::TestToUpdate
        );
    }

    // TC-10: note_mention_matches_file with empty strings
    #[test]
    fn test_note_mention_matches_file_empty() {
        assert!(!note_mention_matches_file("", "src/lib.rs"));
        assert!(!note_mention_matches_file("lib.rs", ""));
        assert!(!note_mention_matches_file("", ""));
    }
}
