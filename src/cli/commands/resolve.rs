//! Shared target resolution for CLI commands
//!
//! Delegates to `cqs::resolve_target` and `cqs::parse_target` in the library crate.

pub use cqs::parse_target;

use anyhow::Result;
use cqs::store::{ChunkSummary, SearchResult, Store};

/// Resolve a target string to a ChunkSummary (CLI wrapper).
///
/// Wraps the library's `resolve_target` with anyhow error conversion.
pub fn resolve_target(store: &Store, target: &str) -> Result<(ChunkSummary, Vec<SearchResult>)> {
    Ok(cqs::resolve_target(store, target)?)
}
