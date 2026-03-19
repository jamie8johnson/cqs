//! Search algorithms and name matching
//!
//! Implements search methods on Store for semantic, hybrid, and index-guided
//! search. See `math.rs` for similarity scoring.

mod query;
pub(crate) mod scoring;

use crate::store::helpers::{ChunkSummary, SearchResult};
use crate::store::{Store, StoreError};

/// Result of resolving a target name to a concrete chunk.
///
/// Contains the best-matching chunk and any alternative matches
/// found during resolution (useful for disambiguation UIs).
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    /// The resolved chunk (best match for the target name)
    pub chunk: ChunkSummary,
    /// Other candidates found during resolution, ordered by match quality
    pub alternatives: Vec<SearchResult>,
}

// ============ Target Resolution ============

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
    (None, target.trim_end_matches(':'))
}

/// Resolve a target string to a [`ResolvedTarget`].
///
/// Uses search_by_name with optional file filtering.
/// Returns the best-matching chunk and alternatives, or an error if none found.
pub fn resolve_target(store: &Store, target: &str) -> Result<ResolvedTarget, StoreError> {
    let _span = tracing::info_span!("resolve_target", target).entered();
    let (file_filter, name) = parse_target(target);
    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        return Err(StoreError::NotFound(format!(
            "No function found matching '{}'. Check the name and try again.",
            name
        )));
    }

    let idx = if let Some(file) = file_filter {
        let matched = results.iter().position(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        });
        match matched {
            Some(i) => i,
            None => {
                let found_in: Vec<_> = results
                    .iter()
                    .take(3)
                    .map(|r| r.chunk.file.to_string_lossy().to_string())
                    .collect();
                return Err(StoreError::NotFound(format!(
                    "No function '{}' found in file matching '{}'. Found in: {}",
                    name,
                    file,
                    found_in.join(", ")
                )));
            }
        }
    } else {
        // Prefer non-test chunks when names are ambiguous
        results
            .iter()
            .position(|r| {
                let path = r.chunk.file.to_string_lossy();
                let name = &r.chunk.name;
                !name.starts_with("test_")
                    && !path.contains("/tests/")
                    && !path.ends_with("_test.rs")
            })
            .unwrap_or(0)
    };
    let chunk = results[idx].chunk.clone();
    Ok(ResolvedTarget {
        chunk,
        alternatives: results,
    })
}
