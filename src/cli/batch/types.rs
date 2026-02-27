//! Typed output structs for batch JSON responses.
//!
//! Replaces manual `serde_json::json!` assembly with `#[derive(Serialize)]` structs
//! for chunk-shaped output. Ensures consistent field names and path normalization.

use std::path::Path;

use serde::Serialize;

/// Normalize a path for JSON output: forward slashes, no backslashes.
pub(super) fn normalize_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Common chunk output shape used by search, similar, and other handlers.
///
/// Fields match the existing JSON output exactly — this is a refactor, not a schema change.
#[derive(Serialize)]
pub(super) struct ChunkOutput {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub language: String,
    pub chunk_type: String,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl ChunkOutput {
    /// Build from a SearchResult (search, name-only search).
    pub fn from_search_result(r: &cqs::store::SearchResult, include_content: bool) -> Self {
        Self {
            name: r.chunk.name.clone(),
            file: normalize_path(&r.chunk.file),
            line_start: r.chunk.line_start,
            line_end: r.chunk.line_end,
            language: r.chunk.language.to_string(),
            chunk_type: r.chunk.chunk_type.to_string(),
            score: r.score,
            signature: Some(r.chunk.signature.clone()),
            content: if include_content {
                Some(r.chunk.content.clone())
            } else {
                None
            },
        }
    }
}
