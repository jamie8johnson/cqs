//! Shared target resolution for CLI commands
//!
//! Extracts the duplicated parse_target + resolve logic from explain.rs and similar.rs.

use anyhow::{bail, Result};

use cqs::store::{ChunkSummary, SearchResult, Store};

/// Parse a target string into (optional_file_filter, function_name).
///
/// Supports formats:
/// - `"function_name"` -> (None, "function_name")
/// - `"path/to/file.rs:function_name"` -> (Some("path/to/file.rs"), "function_name")
pub fn parse_target(target: &str) -> (Option<&str>, &str) {
    if let Some(pos) = target.rfind(':') {
        let file = &target[..pos];
        let name = &target[pos + 1..];
        if !file.is_empty() && !name.is_empty() {
            return (Some(file), name);
        }
    }
    (None, target)
}

/// Resolve a target string to a ChunkSummary.
///
/// Uses search_by_name with optional file filtering.
/// Returns the best-matching chunk or an error if none found.
pub fn resolve_target(store: &Store, target: &str) -> Result<(ChunkSummary, Vec<SearchResult>)> {
    let (file_filter, name) = parse_target(target);
    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        bail!(
            "No function found matching '{}'. Check the name and try again.",
            name
        );
    }

    let matched = if let Some(file) = file_filter {
        results.iter().position(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let idx = matched.unwrap_or(0);
    let chunk = results[idx].chunk.clone();
    Ok((chunk, results))
}
