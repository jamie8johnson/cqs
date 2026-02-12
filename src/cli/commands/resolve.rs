//! Shared target resolution for CLI commands
//!
//! Delegates to `cqs::resolve_target` and `cqs::parse_target` in the library crate.

pub use cqs::parse_target;

use anyhow::Result;
use cqs::store::Store;
use cqs::ResolvedTarget;

/// Resolve a target string to a [`ResolvedTarget`] (CLI wrapper).
///
/// Wraps the library's `resolve_target` with anyhow error conversion.
pub fn resolve_target(store: &Store, target: &str) -> Result<ResolvedTarget> {
    Ok(cqs::resolve_target(store, target)?)
}
