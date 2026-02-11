//! Scout — pre-investigation dashboard for task planning
//!
//! Given a task description, searches for relevant code, groups by file,
//! and returns signatures + caller/test counts + staleness + relevant notes.
//! Optimized for planning, not reading.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::impact::compute_hints_with_graph;
use crate::store::{ChunkSummary, NoteSummary, SearchFilter, StoreError};
use crate::{Embedder, Store};

/// Role classification for chunks in scout results
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkRole {
    /// High-relevance function likely needing modification (score >= 0.5)
    ModifyTarget,
    /// Test that may need updating
    TestToUpdate,
    /// Lower-relevance dependency
    Dependency,
}

/// A chunk in the scout result with hints
pub struct ScoutChunk {
    /// Function/class/etc. name
    pub name: String,
    /// Type of code element
    pub chunk_type: String,
    /// Function signature
    pub signature: String,
    /// Starting line number
    pub line_start: u32,
    /// Role classification
    pub role: ChunkRole,
    /// Number of callers
    pub caller_count: u64,
    /// Number of tests reaching this function
    pub test_count: u64,
    /// Semantic search score (0.0-1.0)
    pub search_score: f32,
}

/// A file group in the scout result
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
pub struct ScoutSummary {
    pub total_files: usize,
    pub total_functions: usize,
    pub untested_count: usize,
    pub stale_count: usize,
}

/// Complete scout result
pub struct ScoutResult {
    pub file_groups: Vec<FileGroup>,
    pub relevant_notes: Vec<NoteSummary>,
    pub summary: ScoutSummary,
}

/// Minimum search score to classify as ModifyTarget
const MODIFY_TARGET_THRESHOLD: f32 = 0.5;

/// Test name patterns (subset of store/calls.rs patterns)
fn is_test_name(name: &str) -> bool {
    name.starts_with("test_")
        || name.starts_with("Test")
        || name.ends_with("_test")
        || name.contains("_test_")
        || name.contains(".test")
}

/// Run scout analysis for a task description.
pub fn scout(
    store: &Store,
    embedder: &Embedder,
    task: &str,
    root: &Path,
    limit: usize,
) -> Result<ScoutResult, ScoutError> {
    // 1. Embed and search
    let query_embedding = embedder
        .embed_query(task)
        .map_err(|e| ScoutError::Embedder(e.to_string()))?;

    let filter = SearchFilter {
        enable_rrf: true,
        query_text: task.to_string(),
        ..SearchFilter::default()
    };

    let results = store
        .search_filtered(&query_embedding, &filter, 15, 0.2)
        .map_err(ScoutError::Store)?;

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

    // 3. Load call graph + test chunks ONCE
    let graph = store.get_call_graph().map_err(ScoutError::Store)?;
    let test_chunks = store.find_test_chunks().map_err(ScoutError::Store)?;

    // 4. Batch caller/callee counts
    let all_names: Vec<&str> = results.iter().map(|r| r.chunk.name.as_str()).collect();
    let caller_counts = match store.get_caller_counts_batch(&all_names) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch caller counts");
            HashMap::new()
        }
    };

    // 5. Check staleness
    let origins: Vec<&str> = file_map.keys().map(|p| p.to_str().unwrap_or("")).collect();
    let stale_set = match store.check_origins_stale(&origins, root) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check staleness");
            HashSet::new()
        }
    };

    // 6. Build file groups
    let mut groups: Vec<FileGroup> = file_map
        .into_iter()
        .map(|(file, chunks)| {
            let relevance_score = chunks.iter().map(|(s, _)| s).sum::<f32>() / chunks.len() as f32;
            let is_stale = stale_set.contains(&file.to_string_lossy().to_string());

            let scout_chunks: Vec<ScoutChunk> = chunks
                .iter()
                .map(|(score, chunk)| {
                    let hints = compute_hints_with_graph(
                        &graph,
                        &test_chunks,
                        &chunk.name,
                        caller_counts.get(&chunk.name).map(|&c| c as usize),
                    );

                    let role = classify_role(*score, &chunk.name);

                    ScoutChunk {
                        name: chunk.name.clone(),
                        chunk_type: chunk.chunk_type.to_string(),
                        signature: chunk.signature.clone(),
                        line_start: chunk.line_start,
                        role,
                        caller_count: hints.caller_count as u64,
                        test_count: hints.test_count as u64,
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
    groups.sort_by(|a, b| {
        b.relevance_score
            .partial_cmp(&a.relevance_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    groups.truncate(limit);

    // 7. Find relevant notes by mention overlap
    let result_files: HashSet<String> = groups
        .iter()
        .map(|g| {
            g.file
                .strip_prefix(root)
                .unwrap_or(&g.file)
                .to_string_lossy()
                .replace('\\', "/")
        })
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

/// Classify a chunk's role based on score and name
fn classify_role(score: f32, name: &str) -> ChunkRole {
    if is_test_name(name) {
        ChunkRole::TestToUpdate
    } else if score >= MODIFY_TARGET_THRESHOLD {
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
        Err(_) => return Vec::new(),
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
    if !mention.contains('.') && !mention.contains('/') {
        return false;
    }
    file.ends_with(mention)
        && (file.len() == mention.len() || file.as_bytes()[file.len() - mention.len() - 1] == b'/')
}

/// Serialize scout result to JSON
pub fn scout_to_json(result: &ScoutResult, root: &Path) -> serde_json::Value {
    let groups_json: Vec<_> = result
        .file_groups
        .iter()
        .map(|g| {
            let rel = g
                .file
                .strip_prefix(root)
                .unwrap_or(&g.file)
                .to_string_lossy()
                .replace('\\', "/");

            let chunks_json: Vec<_> = g
                .chunks
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "chunk_type": c.chunk_type,
                        "signature": c.signature,
                        "line_start": c.line_start,
                        "role": match c.role {
                            ChunkRole::ModifyTarget => "modify_target",
                            ChunkRole::TestToUpdate => "test_to_update",
                            ChunkRole::Dependency => "dependency",
                        },
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

/// Scout-specific error type
#[derive(Debug)]
pub enum ScoutError {
    Store(StoreError),
    Embedder(String),
}

impl std::fmt::Display for ScoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScoutError::Store(e) => write!(f, "{e}"),
            ScoutError::Embedder(e) => write!(f, "Embedder error: {e}"),
        }
    }
}

impl std::error::Error for ScoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ScoutError::Store(e) => Some(e),
            ScoutError::Embedder(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_role_modify_target() {
        assert_eq!(
            classify_role(0.6, "search_filtered"),
            ChunkRole::ModifyTarget
        );
        assert_eq!(classify_role(0.5, "do_something"), ChunkRole::ModifyTarget);
    }

    #[test]
    fn test_classify_role_dependency() {
        assert_eq!(classify_role(0.49, "helper_fn"), ChunkRole::Dependency);
        assert_eq!(classify_role(0.3, "utility"), ChunkRole::Dependency);
    }

    #[test]
    fn test_classify_role_test() {
        assert_eq!(classify_role(0.9, "test_search"), ChunkRole::TestToUpdate);
        assert_eq!(classify_role(0.3, "test_helper"), ChunkRole::TestToUpdate);
        assert_eq!(classify_role(0.8, "TestSuite"), ChunkRole::TestToUpdate);
    }

    #[test]
    fn test_is_test_name() {
        assert!(is_test_name("test_foo"));
        assert!(is_test_name("TestSuite"));
        assert!(is_test_name("foo_test"));
        assert!(is_test_name("foo.test"));
        assert!(!is_test_name("search_filtered"));
        assert!(!is_test_name("testing_util")); // "testing" starts with test but not test_/Test
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
    fn test_scout_summary_zero() {
        let summary = ScoutSummary {
            total_files: 0,
            total_functions: 0,
            untested_count: 0,
            stale_count: 0,
        };
        assert_eq!(summary.total_files, 0);
        assert_eq!(summary.stale_count, 0);
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
}
