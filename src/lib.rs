//! # cqs - Semantic Code Search
//!
//! Local semantic search for code using ML embeddings.
//! Find functions by what they do, not just their names.
//!
//! ## Features
//!
//! - **Semantic search**: Uses E5-base-v2 embeddings (769-dim: 768 model + sentiment)
//! - **Notes with sentiment**: Unified memory system for AI collaborators
//! - **Multi-language**: Rust, Python, TypeScript, JavaScript, Go, C, Java
//! - **GPU acceleration**: CUDA/TensorRT with CPU fallback
//! - **MCP integration**: Works with Claude Code and other AI assistants
//!
//! ## Quick Start
//!
//! ```no_run
//! use cqs::{Embedder, Parser, Store};
//! use cqs::store::ModelInfo;
//!
//! # fn main() -> anyhow::Result<()> {
//! // Initialize components
//! let parser = Parser::new()?;
//! let mut embedder = Embedder::new()?;
//! let store = Store::open(std::path::Path::new(".cq/index.db"))?;
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
//! ## MCP Server
//!
//! Start the MCP server for AI assistant integration:
//!
//! ```no_run
//! # fn example() -> anyhow::Result<()> {
//! use std::path::PathBuf;
//!
//! // Stdio transport (for Claude Code)
//! cqs::serve_stdio(PathBuf::from("."), false)?;  // false = CPU, true = GPU
//!
//! // HTTP transport with GPU embedding (None = no auth)
//! // Note: serve_http blocks the current thread
//! cqs::serve_http(".", "127.0.0.1", 3000, None, true)?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod embedder;
pub mod hnsw;
pub mod index;
pub mod language;
pub mod math;
pub mod mcp;
pub mod nl;
pub mod note;
pub mod parser;
pub mod search;
pub mod source;
pub mod store;

#[cfg(feature = "gpu-search")]
pub mod cagra;

pub use embedder::{Embedder, Embedding};
pub use hnsw::HnswIndex;
pub use index::{IndexResult, VectorIndex};
pub use mcp::{serve_http, serve_stdio};
pub use note::parse_notes;
pub use parser::{Chunk, Parser};
pub use store::{ModelInfo, SearchFilter, Store};

#[cfg(feature = "gpu-search")]
pub use cagra::CagraIndex;

use std::path::PathBuf;

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
/// Shared logic used by both MCP server and CLI watch command.
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

    // Delete old notes and insert new
    store.delete_notes_by_file(notes_path)?;
    let note_embeddings: Vec<_> = notes
        .iter()
        .cloned()
        .zip(embeddings_with_sentiment)
        .collect();
    store.upsert_notes_batch(&note_embeddings, notes_path, file_mtime)?;

    Ok(notes.len())
}
