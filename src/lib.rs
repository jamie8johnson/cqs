//! # cqs - Semantic Code Search
//!
//! Local semantic search for code using ML embeddings.
//! Find functions by what they do, not just their names.
//!
//! ## Features
//!
//! - **Semantic search**: Uses E5-base-v2 embeddings (769-dim: 768 model + sentiment)
//! - **Notes with sentiment**: Unified memory system for AI collaborators
//! - **Multi-language**: Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown
//! - **GPU acceleration**: CUDA/TensorRT with CPU fallback
//! - **CLI tools**: Call graph, impact analysis, test mapping, dead code detection
//! - **Document conversion**: PDF, HTML, CHM → cleaned Markdown (optional `convert` feature)
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
//! use cqs::store::SearchFilter;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Initialize components
//! let parser = Parser::new()?;
//! let embedder = Embedder::new()?;
//! let store = Store::open(std::path::Path::new(".cqs/index.db"))?;
//!
//! // Parse and embed a file
//! let chunks = parser.parse_file(std::path::Path::new("src/main.rs"))?;
//! let embeddings = embedder.embed_documents(
//!     &chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>()
//! )?;
//!
//! // Search for similar code (hybrid RRF search)
//! let query_embedding = embedder.embed_query("parse configuration file")?;
//! let filter = SearchFilter { enable_rrf: true, ..Default::default() };
//! let results = store.search_filtered(&query_embedding, &filter, 5, 0.3)?;
//! # Ok(())
//! # }
//! ```
//!
// Public library API modules
pub mod audit;
pub mod config;
pub mod convert;
pub mod embedder;
pub mod hnsw;
pub mod index;
pub mod language;
pub mod note;
pub mod parser;
pub mod reference;
pub mod store;

// Internal modules - not part of public library API
// These are pub(crate) to hide implementation details, but specific items are
// re-exported below for use by the binary crate (CLI) and integration tests.
pub(crate) mod diff;
pub mod diff_parse;
pub(crate) mod focused_read;
pub(crate) mod gather;
pub(crate) mod impact;
pub(crate) mod math;
pub(crate) mod nl;
pub(crate) mod project;
pub(crate) mod related;
pub mod review;
pub(crate) mod scout;
pub(crate) mod search;
pub(crate) mod source;
pub(crate) mod structural;
pub(crate) mod where_to_add;

#[cfg(feature = "gpu-search")]
pub mod cagra;

pub use audit::parse_duration;
pub use embedder::{Embedder, Embedding};
pub use hnsw::HnswIndex;
pub use index::{IndexResult, VectorIndex};
pub use note::{
    parse_notes, path_matches_mention, rewrite_notes_file, NoteEntry, NoteError, NoteFile,
    NOTES_HEADER,
};
pub use parser::{Chunk, Parser};
pub use store::{ModelInfo, SearchFilter, Store};

// Re-exports for binary crate (CLI) - these are NOT part of the public library API
// but need to be accessible to src/cli/* and tests/
pub use diff::{semantic_diff, DiffResult};
pub use focused_read::extract_type_names;
pub use gather::{
    gather, gather_cross_index, GatherDirection, GatherOptions, DEFAULT_MAX_EXPANDED_NODES,
};
pub use impact::{
    analyze_diff_impact, analyze_impact, compute_hints, compute_hints_with_graph,
    compute_hints_with_graph_depth, compute_risk_batch, diff_impact_to_json, find_hotspots,
    impact_to_json, impact_to_mermaid, map_hunks_to_functions, suggest_tests, CallerDetail,
    ChangedFunction, DiffImpactResult, DiffImpactSummary, DiffTestInfo, FunctionHints,
    ImpactResult, RiskLevel, RiskScore, TestInfo, TestSuggestion, TransitiveCaller,
    DEFAULT_MAX_TEST_SEARCH_DEPTH,
};
pub use nl::{generate_nl_description, generate_nl_with_template, normalize_for_fts, NlTemplate};
pub use project::{search_across_projects, ProjectRegistry};
pub use related::{find_related, RelatedFunction, RelatedResult};
pub use scout::{
    scout, scout_to_json, scout_with_options, ChunkRole, FileGroup, ScoutChunk, ScoutOptions,
    ScoutResult, ScoutSummary, DEFAULT_MODIFY_TARGET_THRESHOLD, DEFAULT_SCOUT_SEARCH_LIMIT,
    DEFAULT_SCOUT_SEARCH_THRESHOLD,
};
pub use search::{parse_target, resolve_target, ResolvedTarget};
pub use structural::Pattern;
pub use where_to_add::{
    suggest_placement, suggest_placement_with_options, FileSuggestion, LocalPatterns,
    PlacementOptions, PlacementResult, DEFAULT_PLACEMENT_SEARCH_LIMIT,
    DEFAULT_PLACEMENT_SEARCH_THRESHOLD,
};

#[cfg(feature = "gpu-search")]
pub use cagra::CagraIndex;

use std::path::PathBuf;

/// Unified error type for analysis operations (scout, where-to-add, etc.)
///
/// Replaces the former `ScoutError` and `SuggestError` which were near-identical.
#[derive(Debug, thiserror::Error)]
pub enum AnalysisError {
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("embedding failed: {0}")]
    Embedder(String),
}

/// Name of the per-project index directory (created by `cqs init`).
pub const INDEX_DIR: &str = ".cqs";

/// Legacy index directory name (pre-v0.9.7). Used for auto-migration.
const LEGACY_INDEX_DIR: &str = ".cq";

/// Resolve the index directory for a project, migrating from `.cq/` to `.cqs/` if needed.
///
/// If the legacy `.cq/` exists and `.cqs/` does not, renames it automatically.
/// Falls back gracefully if the rename fails (e.g., permissions).
pub fn resolve_index_dir(project_root: &Path) -> PathBuf {
    let new_dir = project_root.join(INDEX_DIR);
    let old_dir = project_root.join(LEGACY_INDEX_DIR);

    if old_dir.exists() && !new_dir.exists() && std::fs::rename(&old_dir, &new_dir).is_ok() {
        tracing::info!("Migrated index directory from .cq/ to .cqs/");
    }

    if new_dir.exists() {
        new_dir
    } else if old_dir.exists() {
        old_dir
    } else {
        new_dir
    }
}

/// Embedding dimension: 768 from E5-base-v2 model + 1 sentiment dimension.
/// Single source of truth — all modules import this constant.
pub const EMBEDDING_DIM: usize = 769;

/// Unified test-chunk detection heuristic.
///
/// Returns `true` if a chunk looks like a test based on its name or file path.
/// Used by scout, impact, and where_to_add to filter out tests from analysis.
///
/// **Not** used by `store::calls::find_dead_code`, which has its own SQL-based
/// detection (`TEST_NAME_PATTERNS`, `TEST_CONTENT_MARKERS`, `TEST_PATH_PATTERNS`)
/// that also checks content markers like `#[test]` and `@Test`.
pub fn is_test_chunk(name: &str, file: &str) -> bool {
    // Name-based patterns (language-agnostic)
    let name_match = name.starts_with("test_")
        || name.starts_with("Test")
        || name.ends_with("_test")
        || name.contains("_test_")
        || name.contains(".test");
    if name_match {
        return true;
    }
    // Path-based patterns (mirrors TEST_PATH_PATTERNS in store/calls.rs)
    file.contains("/tests/")
        || file.starts_with("tests/")
        || file.contains("_test.")
        || file.contains(".test.")
        || file.contains(".spec.")
        || file.ends_with("_test.go")
        || file.ends_with("_test.py")
}

/// Relativize a path against a root and normalize separators for display.
///
/// Strips `root` prefix if present, converts backslashes to forward slashes.
pub fn rel_display(path: &std::path::Path, root: &std::path::Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ============ Note Indexing Helper ============

use std::path::Path;

/// Index notes into the database (embed and store)
///
/// Shared logic used by CLI commands.
/// Embeds notes using the provided embedder and stores them with sentiment.
///
/// # Arguments
/// * `notes` - Notes to index
/// * `notes_path` - Path to notes file (for mtime tracking)
/// * `embedder` - Embedder for creating embeddings
/// * `store` - Store for persisting notes
///
/// # Returns
/// Number of notes indexed
pub fn index_notes(
    notes: &[note::Note],
    notes_path: &Path,
    embedder: &Embedder,
    store: &Store,
) -> anyhow::Result<usize> {
    tracing::info!(path = %notes_path.display(), count = notes.len(), "Indexing notes");

    if notes.is_empty() {
        return Ok(0);
    }

    // Embed note content with sentiment prefix
    let texts: Vec<String> = notes.iter().map(|n| n.embedding_text()).collect();
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let base_embeddings = embedder.embed_documents(&text_refs)?;

    // Add sentiment as 769th dimension
    let embeddings_with_sentiment: Vec<embedder::Embedding> = base_embeddings
        .into_iter()
        .zip(notes.iter())
        .map(|(emb, note)| emb.with_sentiment(note.sentiment()))
        .collect();

    // Get file mtime
    let file_mtime = notes_path
        .metadata()
        .and_then(|m| m.modified())
        .map_err(|e| {
            tracing::trace!(path = %notes_path.display(), error = %e, "Failed to get file mtime");
            e
        })
        .ok()
        .and_then(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| {
                    tracing::trace!(path = %notes_path.display(), error = %e, "File mtime before Unix epoch");
                })
                .ok()
        })
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Atomically replace notes (delete old + insert new in single transaction)
    let note_embeddings: Vec<_> = notes
        .iter()
        .cloned()
        .zip(embeddings_with_sentiment)
        .collect();
    store.replace_notes_for_file(&note_embeddings, notes_path, file_mtime)?;

    Ok(notes.len())
}

// ============ File Enumeration ============

/// Maximum file size to index (1MB)
const MAX_FILE_SIZE: u64 = 1_048_576;

/// Enumerate files to index in a project directory.
///
/// Respects .gitignore, skips hidden files and large files (>1MB).
/// Returns relative paths from the project root.
///
/// Shared file enumeration for consistent indexing.
pub fn enumerate_files(
    root: &Path,
    parser: &Parser,
    no_ignore: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    use anyhow::Context;
    use ignore::WalkBuilder;

    let root = dunce::canonicalize(root).context("Failed to canonicalize root")?;

    let walker = WalkBuilder::new(&root)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .ignore(!no_ignore)
        .hidden(!no_ignore)
        .follow_links(false)
        .build();

    let files: Vec<PathBuf> = walker
        .filter_map(|e| {
            e.map_err(|err| {
                tracing::debug!(error = %err, "Failed to read directory entry during walk");
            })
            .ok()
        })
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| {
            e.metadata()
                .map(|m| m.len() <= MAX_FILE_SIZE)
                .unwrap_or(false)
        })
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| parser.supported_extensions().contains(&ext))
                .unwrap_or(false)
        })
        .filter_map({
            let failure_count = std::sync::atomic::AtomicUsize::new(0);
            move |e| {
                let path = match dunce::canonicalize(e.path()) {
                    Ok(p) => p,
                    Err(err) => {
                        let count =
                            failure_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if count < 3 {
                            tracing::warn!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        } else {
                            tracing::debug!(
                                path = %e.path().display(),
                                error = %err,
                                "Failed to canonicalize path, skipping"
                            );
                        }
                        return None;
                    }
                };
                if path.starts_with(&root) {
                    Some(path.strip_prefix(&root).unwrap_or(&path).to_path_buf())
                } else {
                    tracing::warn!("Skipping path outside project: {}", e.path().display());
                    None
                }
            }
        })
        .collect();

    tracing::info!(file_count = files.len(), "File enumeration complete");

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_test_chunk_name_patterns() {
        // Positive: name-based
        assert!(is_test_chunk("test_foo", "src/lib.rs"));
        assert!(is_test_chunk("TestSuite", "src/lib.rs"));
        assert!(is_test_chunk("foo_test", "src/lib.rs"));
        assert!(is_test_chunk("foo_test_bar", "src/lib.rs"));
        assert!(is_test_chunk("foo.test", "src/lib.rs"));
        // Negative: name-based
        assert!(!is_test_chunk("search_filtered", "src/lib.rs"));
        assert!(!is_test_chunk("testing_util", "src/lib.rs"));
    }

    #[test]
    fn test_is_test_chunk_path_patterns() {
        // Positive: path-based
        assert!(is_test_chunk("helper", "tests/helper.rs"));
        assert!(is_test_chunk("helper", "src/tests/helper.rs"));
        assert!(is_test_chunk("helper", "search_test.rs"));
        assert!(is_test_chunk("helper", "search.test.ts"));
        assert!(is_test_chunk("helper", "search.spec.js"));
        assert!(is_test_chunk("helper", "search_test.go"));
        assert!(is_test_chunk("helper", "search_test.py"));
        // Negative: path-based
        assert!(!is_test_chunk("helper", "src/lib.rs"));
        assert!(!is_test_chunk("helper", "src/search.rs"));
    }

    #[test]
    fn test_is_test_chunk_combined() {
        // Both name and path match
        assert!(is_test_chunk("test_helper", "tests/helper.rs"));
        // Name matches, path doesn't
        assert!(is_test_chunk("test_search", "src/search.rs"));
        // Path matches, name doesn't
        assert!(is_test_chunk("setup_fixtures", "tests/fixtures.rs"));
    }
}
