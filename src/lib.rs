#![allow(clippy::doc_lazy_continuation)] // Bulk doc comment cleanup left some continuation formatting
//! # cqs - Code Intelligence and RAG for AI Agents
//!
//! Semantic search, call graph analysis, impact tracing, type dependencies, and smart
//! context assembly — all in single tool calls. Local ML embeddings, GPU-accelerated.
//!
//! ## Features
//!
//! - **Semantic search**: Hybrid RRF (keyword + vector) with configurable embedding models (BGE-large default, E5-base and v9-200k presets, custom ONNX). 90.9% Recall@1 on 296-query expanded eval.
//! - **Call graphs**: Callers, callees, transitive impact, shortest-path tracing between functions
//! - **Impact analysis**: What breaks if you change X? Callers + affected tests + risk scoring
//! - **Type dependencies**: Who uses this type? What types does this function use?
//! - **Smart context assembly**: `gather` (search + BFS expansion), `task` (scout + gather + impact + placement), `scout` (pre-investigation dashboard)
//! - **Diff review & CI**: Structured risk analysis, dead code detection in diffs, gating pipeline
//! - **Batch & chat modes**: Persistent session with pipeline syntax (`search "error" | callers | test-map`)
//! - **Notes with sentiment**: Unified memory system for AI collaborators
//! - **Multi-language**: 54 languages + L5X/L5K PLC exports, with multi-grammar injection (HTML→JS/CSS, Svelte, Vue, Razor, etc.)
//! - **Type-aware embeddings**: Full signatures appended to NL descriptions for richer type discrimination
//! - **Doc comment generation**: `--improve-docs` generates and writes doc comments to source files via LLM
//! - **HyDE query predictions**: `--hyde-queries` generates synthetic search queries per function for improved recall
//! - **Training data generation**: `train-data` command generates fine-tuning triplets from git history
//! - **GPU acceleration**: CUDA/TensorRT with CPU fallback
//! - **Document conversion**: PDF, HTML, CHM, Web Help → cleaned Markdown (optional `convert` feature)
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
//! use cqs::embedder::ModelConfig;
//! use cqs::store::SearchFilter;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Initialize components
//! let parser = Parser::new()?;
//! let embedder = Embedder::new(ModelConfig::resolve(None, None))?;
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
//! let filter = SearchFilter {
//!     enable_rrf: true,
//!     query_text: "parse configuration file".to_string(),
//!     ..Default::default()
//! };
//! let results = store.search_filtered(&query_embedding, &filter, 5, 0.3)?;
//! # Ok(())
//! # }
//! ```
//!
// Public library API modules
pub mod audit;
pub mod cache;
pub mod config;
pub mod convert;
pub mod embedder;
pub mod fs;
pub mod hnsw;
pub mod index;
pub mod language;
pub mod note;
pub mod parser;
pub mod reference;
pub mod splade;
pub mod store;
pub mod train_data;

pub mod ci;
pub mod health;
pub mod reranker;
pub mod suggest;

// Internal modules - not part of public library API
// These are pub(crate) to hide implementation details, but specific items are
// re-exported below for use by the binary crate (CLI) and integration tests.
pub(crate) mod diff;
pub(crate) mod diff_parse;
pub use diff_parse::{parse_unified_diff, DiffHunk};
pub mod drift;
pub use drift::{detect_drift, DriftEntry, DriftResult};
pub(crate) mod focused_read;
pub(crate) mod gather;
pub(crate) mod impact;
pub(crate) mod math;
pub(crate) mod nl;
pub(crate) mod onboard;
pub(crate) mod project;
pub(crate) mod related;
pub(crate) mod review;
pub use review::{review_diff, ReviewNoteEntry, ReviewResult, ReviewedFunction, RiskSummary};
#[cfg(feature = "llm-summaries")]
pub mod doc_writer;
#[cfg(feature = "llm-summaries")]
pub mod llm;
pub mod plan;
pub(crate) mod scout;
pub mod search;
pub(crate) mod structural;
pub(crate) mod task;
pub(crate) mod where_to_add;

#[cfg(test)]
pub mod test_helpers;

#[cfg(feature = "gpu-index")]
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
pub use reranker::Reranker;
pub use store::{HnswKind, ModelInfo, SearchFilter, Store};

// Re-exports for binary crate (CLI) - these are NOT part of the public library API
// but need to be accessible to src/cli/* and tests/.
// Wildcard re-exports: no external users, so name conflicts are compiler-caught.
pub use diff::*;
pub use focused_read::COMMON_TYPES;
pub use gather::*;
/// Cross-project call graph types and context.
pub mod cross_project {
    pub use crate::impact::cross_project::{
        analyze_impact_cross, trace_cross, CrossProjectHop, CrossProjectTraceResult,
    };
    pub use crate::store::calls::cross_project::{
        CrossProjectCallee, CrossProjectCaller, CrossProjectContext, CrossProjectTestChunk,
        NamedStore,
    };
}
pub use impact::*;
pub use nl::*;
pub use onboard::*;
pub use project::*;
pub use related::*;
pub use scout::*;
pub use search::*;
pub use structural::Pattern;
pub use task::*;
pub use where_to_add::*;

#[cfg(feature = "gpu-index")]
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
    Embedder(#[from] embedder::EmbedderError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("{phase} phase failed: {message}")]
    Phase {
        phase: &'static str,
        message: String,
    },
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

/// Default embedding dimension (1024, BGE-large-en-v1.5).
/// The actual dimension is detected at runtime from the model output.
/// Use `Embedder::embedding_dim()` for the runtime value.
/// Derived from `ModelConfig::default_model().dim`.
pub const EMBEDDING_DIM: usize = embedder::DEFAULT_DIM;

// # Batch Size Constants (#683)
//
// ~25 `const BATCH_SIZE` definitions across store/pipeline/search modules.
// Intentionally local — each is tuned for its SQL query shape:
//
// SQLite limit: max 999 bind parameters per statement. A query with N columns
// per row can batch `floor(999 / N)` rows.
//
// Common sizes:
//   500   — 1-2 param queries (chunks, embeddings, calls)
//   200   — 4-5 params per row (type edges, call graph)
//   132   — upsert_chunk (5 params, 132 × 5 = 660)
//   100   — staleness checks with path matching
//   20    — enrichment hash (many columns)
//
// Non-SQL:
//   EMBED_BATCH_SIZE = 64    — ONNX inference (CQS_EMBED_BATCH_SIZE)
//   FILE_BATCH_SIZE = 5000   — pipeline file processing (CQS_FILE_BATCH_SIZE)
//   HNSW_BATCH_SIZE = 10000  — HNSW insert
//   MAX_BATCH_SIZE = 10000   — Claude Batches API limit
//
// Do not centralize. If adding a batched SQL query: floor(999 / params_per_row).

/// Unified test-chunk detection heuristic.
///
/// Returns `true` if a chunk looks like a test based on its name or file path.
/// Used by scout, impact, and where_to_add to filter out tests from analysis.
///
/// **Not** used by `store::calls::find_dead_code`, which has its own SQL-based
/// detection (`TEST_NAME_PATTERNS`, `TEST_CONTENT_MARKERS`, `TEST_PATH_PATTERNS`)
/// that also checks content markers like `#[test]` and `@Test`.
pub fn is_test_chunk(name: &str, file: &str) -> bool {
    // Name-based patterns (language-agnostic).
    //
    // v1.22.0 audit AC-4: previously `name.starts_with("Test")` demoted
    // production types like TestRegistry, TestHarness, TestContext by 30%.
    // Tightened to require `Test` followed by underscore or end-of-name
    // (catches `test_foo`, `Test_bar`, but not `TestHarness`). The xUnit
    // `TestFoo` naming convention would still be caught but Go's and
    // Rust's `test_` prefix is the dominant pattern in practice.
    let name_match = name.starts_with("test_")
        || name.starts_with("Test_")
        || name == "Test"
        || name.starts_with("spec_")
        || name.ends_with("_test")
        || name.ends_with("_spec")
        || name.contains("_test_")
        || name.contains(".test");
    if name_match {
        return true;
    }
    // Path-based patterns from the language registry (all 52+ languages).
    // Patterns use SQL LIKE syntax: `%` = any chars, `\_` = literal underscore.
    // Normalize backslashes to forward slashes for cross-platform matching.
    let normalized = file.replace('\\', "/");
    for pattern in language::REGISTRY.all_test_path_patterns() {
        if sql_like_matches(&normalized, pattern) {
            return true;
        }
        // Registry patterns assume a prefix (e.g. `%/tests/%` needs something
        // before `/tests/`). Relative paths like `tests/foo.rs` lack that prefix,
        // so also try with a synthetic `/` prepended.
        if !normalized.starts_with('/') {
            let prefixed = format!("/{normalized}");
            if sql_like_matches(&prefixed, pattern) {
                return true;
            }
        }
    }
    false
}

/// Match a path against a SQL `LIKE` pattern.
///
/// Supports `%` (any sequence of characters) and `\_` (literal underscore).
/// All other characters are matched literally. Used by `is_test_chunk` to
/// evaluate the registry's `test_path_patterns`.
fn sql_like_matches(path: &str, pattern: &str) -> bool {
    // Convert SQL LIKE pattern to segments split on `%`.
    // `\_` is an escaped literal underscore in SQL LIKE — unescape it first.
    let unescaped = pattern.replace("\\_", "_");
    let parts: Vec<&str> = unescaped.split('%').collect();

    // Single segment (no wildcards) — exact match.
    if parts.len() == 1 {
        return path == parts[0];
    }

    let mut pos = 0;

    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            // First segment must be a prefix.
            if !path.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last segment must be a suffix, and must not overlap with what we've consumed.
            if !path[pos..].ends_with(part) {
                return false;
            }
        } else {
            // Interior segment — find it after `pos`.
            match path[pos..].find(part) {
                Some(offset) => pos += offset + part.len(),
                None => return false,
            }
        }
    }
    true
}

use std::path::Path;

/// Normalize a path to a string with forward slashes.
///
/// Converts `Path`/`PathBuf` to `String`, replacing backslashes with forward slashes
/// for cross-platform consistency (WSL, Windows paths in JSON output).
pub fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Normalize backslashes to forward slashes in a string path.
///
/// For already-stringified paths. Returns the input unchanged on Unix.
pub fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

/// Generate an unpredictable u64 suffix for temporary file names.
///
/// Uses [`std::collections::hash_map::RandomState`] (seeded by the OS on each
/// process start) to produce a value that is different every run and resists
/// symlink-based TOCTOU attacks on temp-file paths.
pub fn temp_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// Serde serializer for `PathBuf` fields: forward-slash normalized.
///
/// Use as `#[serde(serialize_with = "crate::serialize_path_normalized")]`
pub fn serialize_path_normalized<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&normalize_path(path))
}

/// Relativize a path against a root and normalize separators for display.
///
/// Strips `root` prefix if present, converts backslashes to forward slashes.
pub fn rel_display(path: &Path, root: &Path) -> String {
    normalize_path(path.strip_prefix(root).unwrap_or(path))
}

// ============ Note Indexing Helper ============

/// Index notes into the database (store without embeddings)
///
/// Shared logic used by CLI commands.
/// Stores notes in the database for mention-based lookup (SQ-9: note embeddings removed).
///
/// # Arguments
/// * `notes` - Notes to index
/// * `notes_path` - Path to notes file (for mtime tracking)
/// * `store` - Store for persisting notes
///
/// # Returns
/// Number of notes indexed
pub fn index_notes(
    notes: &[note::Note],
    notes_path: &Path,
    store: &Store<store::ReadWrite>,
) -> anyhow::Result<usize> {
    let _span =
        tracing::info_span!("index_notes", path = %notes_path.display(), count = notes.len())
            .entered();

    if notes.is_empty() {
        return Ok(0);
    }

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
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // Atomically replace notes (delete old + insert new in single transaction)
    store.replace_notes_for_file(notes, notes_path, file_mtime)?;

    Ok(notes.len())
}

// ============ File Enumeration ============

/// Default maximum file size to index (1MB). Generated code (`bindings.rs`
/// blobs, compiled TypeScript, migrations) can exceed this and is silently
/// skipped; tune via `CQS_MAX_FILE_SIZE` (bytes) when that happens.
const DEFAULT_MAX_FILE_SIZE: u64 = 1_048_576;

/// Resolve the per-file size cap, checking `CQS_MAX_FILE_SIZE` first. Zero
/// falls back to the default — disabling the cap entirely would OOM on
/// multi-GB artifacts.
fn max_file_size() -> u64 {
    std::env::var("CQS_MAX_FILE_SIZE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_FILE_SIZE)
}

/// Enumerate files to index in a project directory.
///
/// Respects .gitignore, skips hidden files and files larger than
/// `CQS_MAX_FILE_SIZE` bytes (default 1MB — generated code can exceed this).
/// Returns relative paths from the project root.
///
/// Shared file enumeration for consistent indexing.
pub fn enumerate_files(
    root: &Path,
    extensions: &[&str],
    no_ignore: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    let _span = tracing::debug_span!("enumerate_files", root = %root.display()).entered();
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
        .filter_entry(|entry| {
            // Skip nested git worktrees. A linked worktree's `.git` is a file
            // (not a directory) that contains a `gitdir: ...` pointer. Indexing
            // the worktree would duplicate the entire source tree under a
            // different prefix — this is the root cause of `.claude/worktrees/`
            // pollution in the index.
            if entry.file_type().is_some_and(|ft| ft.is_dir())
                && entry.path().join(".git").is_file()
            {
                return false;
            }
            true
        })
        .build();

    let size_cap = max_file_size();
    let files: Vec<PathBuf> = walker
        .filter_map(|e| {
            e.map_err(|err| {
                tracing::debug!(error = %err, "Failed to read directory entry during walk");
            })
            .ok()
        })
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter(|e| match e.metadata() {
            Ok(m) => {
                let len = m.len();
                if len > size_cap {
                    // SHL-V1.25-11: surface skipped oversize files at info
                    // so users debugging "why doesn't my symbol show up"
                    // discover CQS_MAX_FILE_SIZE instead of silent drop.
                    tracing::info!(
                        path = %e.path().display(),
                        size = len,
                        cap = size_cap,
                        "Skipping oversize file (CQS_MAX_FILE_SIZE)"
                    );
                    false
                } else {
                    true
                }
            }
            Err(_) => false,
        })
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    let lower = ext.to_ascii_lowercase();
                    extensions.contains(&lower.as_str())
                })
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
                    tracing::warn!(path = %e.path().display(), "Skipping path outside project");
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
        assert!(is_test_chunk("Test_suite", "src/lib.rs"));
        assert!(is_test_chunk("Test", "src/lib.rs")); // bare "Test" name
        assert!(is_test_chunk("foo_test", "src/lib.rs"));
        assert!(is_test_chunk("foo_test_bar", "src/lib.rs"));
        assert!(is_test_chunk("foo.test", "src/lib.rs"));
        // Negative: name-based
        // v1.22.0 audit AC-4: "TestSuite", "TestHarness", "TestRegistry" etc.
        // are NOT tests — they're test-framework API types that should not be
        // demoted 30% in search results. Only `test_` and `Test_` prefixes
        // (with underscore) are treated as test chunks by name.
        assert!(!is_test_chunk("TestSuite", "src/lib.rs"));
        assert!(!is_test_chunk("TestHarness", "src/lib.rs"));
        assert!(!is_test_chunk("TestRegistry", "src/lib.rs"));
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

    // ─── rel_display tests ──────────────────────────────────────────────────

    #[test]
    fn test_rel_display_relative_path_within_base() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/home/user/project/src/main.rs");
        assert_eq!(rel_display(path, root), "src/main.rs");
    }

    #[test]
    fn test_rel_display_path_outside_base() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/tmp/other/file.rs");
        // Path outside root — returns full path with normalized separators
        assert_eq!(rel_display(path, root), "/tmp/other/file.rs");
    }

    #[test]
    fn test_rel_display_exact_base_path() {
        let root = Path::new("/home/user/project");
        let path = Path::new("/home/user/project");
        // Exact match — strip_prefix returns ""
        assert_eq!(rel_display(path, root), "");
    }

    #[test]
    fn test_rel_display_backslash_normalization() {
        // Simulate a Windows-style path stored as a PathBuf
        let root = Path::new("/home/user/project");
        let path = PathBuf::from("/home/user/project/src\\cli\\mod.rs");
        assert_eq!(rel_display(&path, root), "src/cli/mod.rs");
    }

    #[test]
    fn test_rel_display_no_common_prefix() {
        let root = Path::new("/opt/tools");
        let path = Path::new("/var/log/app.log");
        assert_eq!(rel_display(path, root), "/var/log/app.log");
    }

    // ─── index_notes tests ──────────────────────────────────────────────────

    use crate::test_helpers::setup_store;

    /// Creates a notes file in the specified directory with the given content.
    ///
    /// # Arguments
    ///
    /// * `dir` - The directory path where the notes file will be created
    /// * `content` - The content to write to the notes.toml file
    ///
    /// # Returns
    ///
    /// Returns a `PathBuf` pointing to the created notes.toml file.
    ///
    /// # Panics
    ///
    /// Panics if the file write operation fails.
    fn make_notes_file(dir: &std::path::Path, content: &str) -> PathBuf {
        let path = dir.join("notes.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_index_notes_empty_returns_zero() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(dir.path(), "# empty notes file\n");
        let notes: Vec<note::Note> = Vec::new();

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 0);

        // Verify no notes in store
        let summaries = store.list_notes_summaries().unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn test_index_notes_stores_notes() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(
            dir.path(),
            r#"
[[note]]
text = "Always use RRF search, not raw embedding"
sentiment = -0.5
mentions = ["search.rs"]

[[note]]
text = "Batch queries are fast"
sentiment = 0.5
mentions = ["store.rs"]
"#,
        );

        let notes = vec![
            note::Note {
                id: "note:0".to_string(),
                text: "Always use RRF search, not raw embedding".to_string(),
                sentiment: -0.5,
                mentions: vec!["search.rs".to_string()],
            },
            note::Note {
                id: "note:1".to_string(),
                text: "Batch queries are fast".to_string(),
                sentiment: 0.5,
                mentions: vec!["store.rs".to_string()],
            },
        ];

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 2);

        // Verify notes are stored
        let summaries = store.list_notes_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(
            summaries[0].text,
            "Always use RRF search, not raw embedding"
        );
        assert!((summaries[0].sentiment - (-0.5)).abs() < f32::EPSILON);
        assert_eq!(summaries[1].text, "Batch queries are fast");
    }

    #[test]
    fn test_index_notes_stores_note_sentiment() {
        let (store, dir) = setup_store();
        let notes_path = make_notes_file(dir.path(), "");

        let notes = vec![note::Note {
            id: "note:0".to_string(),
            text: "Serious issue with error handling".to_string(),
            sentiment: -1.0,
            mentions: vec!["lib.rs".to_string()],
        }];

        let count = index_notes(&notes, &notes_path, &store).unwrap();
        assert_eq!(count, 1);

        // Verify the note is retrievable via list_notes_summaries
        let summaries = store.list_notes_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].text, "Serious issue with error handling");
        assert!((summaries[0].sentiment - (-1.0)).abs() < f32::EPSILON);
    }

    // ─── resolve_index_dir tests (TC-4) ──────────────────────────────────

    #[test]
    fn test_resolve_index_dir_only_legacy_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(LEGACY_INDEX_DIR);
        std::fs::create_dir(&legacy).unwrap();

        let result = resolve_index_dir(dir.path());

        // Legacy .cq/ should have been renamed to .cqs/
        assert!(
            !legacy.exists(),
            ".cq/ should no longer exist after migration"
        );
        assert_eq!(result, dir.path().join(INDEX_DIR));
        assert!(result.exists(), ".cqs/ should exist after migration");
    }

    #[test]
    fn test_resolve_index_dir_both_exist() {
        let dir = tempfile::TempDir::new().unwrap();
        let legacy = dir.path().join(LEGACY_INDEX_DIR);
        let new = dir.path().join(INDEX_DIR);
        std::fs::create_dir(&legacy).unwrap();
        std::fs::create_dir(&new).unwrap();

        let result = resolve_index_dir(dir.path());

        // Both exist: should return .cqs/ without renaming (legacy stays)
        assert_eq!(result, new);
        assert!(legacy.exists(), ".cq/ should still exist when both present");
        assert!(new.exists(), ".cqs/ should still exist");
    }

    #[test]
    fn test_resolve_index_dir_neither_exists() {
        let dir = tempfile::TempDir::new().unwrap();

        let result = resolve_index_dir(dir.path());

        // Neither exists: should return .cqs/ path (not created, just the path)
        assert_eq!(result, dir.path().join(INDEX_DIR));
        assert!(
            !result.exists(),
            ".cqs/ should not be created, only returned as path"
        );
    }

    // ─── enumerate_files tests (TC-9) ────────────────────────────────────

    #[test]
    fn test_enumerate_files_finds_supported_extensions() {
        let dir = tempfile::TempDir::new().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();

        // Create some Rust files
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(src.join("lib.rs"), "pub fn lib() {}").unwrap();
        // Create a non-Rust file (should be filtered out)
        std::fs::write(src.join("readme.txt"), "hello").unwrap();

        let files = enumerate_files(dir.path(), &["rs"], false).unwrap();

        assert_eq!(files.len(), 2, "Should find exactly 2 .rs files");
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"lib.rs".to_string()));
    }

    #[test]
    fn test_enumerate_files_empty_for_unsupported() {
        let dir = tempfile::TempDir::new().unwrap();

        // Create files with unsupported extensions only
        std::fs::write(dir.path().join("notes.txt"), "some text").unwrap();
        std::fs::write(dir.path().join("data.csv"), "a,b,c").unwrap();

        let files = enumerate_files(dir.path(), &["rs", "py"], false).unwrap();

        assert!(
            files.is_empty(),
            "Should return empty for directory with no supported files"
        );
    }
    /// Verifies that the `is_test_chunk` function correctly identifies test files based on filename patterns.
    ///
    /// # Arguments
    ///
    /// This function takes no arguments.
    ///
    /// # Returns
    ///
    /// Returns nothing. This is a test function that validates the behavior of `is_test_chunk` through assertions.
    ///
    /// # Panics
    ///
    /// Panics if any assertion fails, indicating that `is_test_chunk` does not correctly identify test files or non-test files according to expected patterns.

    #[test]
    fn is_test_chunk_spec_patterns() {
        assert!(is_test_chunk("spec_helper", "src/spec_helper.rb"));
        assert!(is_test_chunk("user_spec", "spec/user_spec.rb"));
        assert!(is_test_chunk("normal_fn", "tests/test_main.py"));
        assert!(!is_test_chunk("inspector", "src/inspect.rs"));
    }

    // TC-6: _tests.rs suffix and nested /tests/ path
    #[test]
    fn is_test_chunk_tests_suffix_and_nested_path() {
        // File with _test suffix
        assert!(is_test_chunk("normal_fn", "src/search_test.rs"));
        assert!(is_test_chunk("normal_fn", "src/search_test.py"));
        // Nested /tests/ directory
        assert!(is_test_chunk("normal_fn", "src/store/tests/calls_test.rs"));
        assert!(is_test_chunk("normal_fn", "tests/integration.rs"));
        // .test. suffix (JS/TS)
        assert!(is_test_chunk("normal_fn", "src/search.test.ts"));
        assert!(is_test_chunk("normal_fn", "src/search.test.js"));
        // _test.go suffix
        assert!(is_test_chunk("normal_fn", "pkg/search_test.go"));
        // Should NOT match
        assert!(!is_test_chunk("normal_fn", "src/testing_utils.rs"));
        assert!(!is_test_chunk("normal_fn", "src/attest.rs"));
    }
}
