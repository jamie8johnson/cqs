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
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
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
//! // Search for similar code
//! let query_embedding = embedder.embed_query("parse configuration file")?;
//! let results = store.search(&query_embedding, 5, 0.3)?;
//! # Ok(())
//! # }
//! ```
//!
// Public library API modules
pub mod audit;
pub mod config;
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
pub use gather::{gather, GatherDirection, GatherOptions};
pub use impact::{
    analyze_diff_impact, analyze_impact, compute_hints, compute_hints_with_graph,
    diff_impact_to_json, impact_to_json, impact_to_mermaid, map_hunks_to_functions, suggest_tests,
    ChangedFunction, DiffImpactResult, FunctionHints, ImpactResult, TestSuggestion,
};
pub use nl::{generate_nl_description, generate_nl_with_template, normalize_for_fts, NlTemplate};
pub use project::{search_across_projects, ProjectRegistry};
pub use related::{find_related, RelatedFunction, RelatedResult};
pub use scout::{
    scout, scout_to_json, ChunkRole, FileGroup, ScoutChunk, ScoutResult, ScoutSummary,
};
pub use search::{parse_target, resolve_target};
pub use structural::Pattern;
pub use where_to_add::{suggest_placement, FileSuggestion, LocalPatterns, PlacementResult};

#[cfg(feature = "gpu-search")]
pub use cagra::CagraIndex;

use std::path::PathBuf;

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

/// Strip Windows UNC path prefix (\\?\) if present.
///
/// Windows `canonicalize()` returns UNC paths that can cause issues with
/// path comparison and display. This strips the prefix for consistency.
#[cfg(windows)]
pub fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// No-op on non-Windows platforms
#[cfg(not(windows))]
pub fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    path
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

    let root = strip_unc_prefix(root.canonicalize().context("Failed to canonicalize root")?);

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
                let path = match e.path().canonicalize() {
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
