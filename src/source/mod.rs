//! Source abstraction for indexable content
//!
//! This module provides the `Source` trait for abstracting different
//! sources of indexable code (filesystem, SQL Server, etc.).

mod filesystem;

pub use filesystem::FileSystemSource;

use crate::language::{LanguageDef, REGISTRY};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SourceError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Unsupported file type: {0}")]
    UnsupportedFileType(String),
    #[error("Source not available: {0}")]
    NotAvailable(String),
}

/// An item to be indexed from a source
#[derive(Clone)]
pub struct SourceItem {
    /// Unique origin identifier (e.g., "src/main.rs" for files)
    pub origin: String,
    /// Source type for filtering ("file", "mssql", etc.)
    pub source_type: &'static str,
    /// Raw content to parse and index
    pub content: String,
    /// Language definition for parsing
    pub language: &'static LanguageDef,
    /// Modification time if available (Unix timestamp)
    pub mtime: Option<i64>,
    /// Relative path for display (may differ from origin)
    pub display_path: PathBuf,
}

/// A source of indexable content
///
/// Implementations provide content from various sources like filesystems,
/// databases, or remote services.
pub trait Source: Send + Sync {
    /// Source type identifier ("file", "mssql", etc.)
    fn source_type(&self) -> &'static str;

    /// Enumerate all items from this source
    ///
    /// Returns items that should be indexed. For incremental indexing,
    /// callers should check `mtime` against stored values.
    fn enumerate(&self) -> Result<Vec<SourceItem>, SourceError>;

    /// Check if an item needs reindexing
    ///
    /// Returns the current mtime for the origin, or None if the source
    /// doesn't support mtime-based change detection.
    fn get_mtime(&self, origin: &str) -> Result<Option<i64>, SourceError>;
}

/// Helper to detect language from file extension
pub fn language_from_path(path: &std::path::Path) -> Option<&'static LanguageDef> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| REGISTRY.from_extension(ext))
}
