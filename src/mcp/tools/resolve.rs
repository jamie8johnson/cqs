//! Shared target resolution for MCP tools
//!
//! Delegates to `crate::search::resolve_target` and `crate::search::parse_target`
//! in the library crate.

pub use crate::search::parse_target;

use crate::store::{ChunkSummary, SearchResult, Store};
use anyhow::Result;

/// Resolve a target string to a ChunkSummary (MCP wrapper).
///
/// Wraps the library's `resolve_target` with anyhow error conversion.
pub fn resolve_target(store: &Store, target: &str) -> Result<(ChunkSummary, Vec<SearchResult>)> {
    Ok(crate::search::resolve_target(store, target)?)
}
