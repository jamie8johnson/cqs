//! Shared target resolution for CLI commands
//!
//! Delegates to `cqs::resolve_target` and `cqs::parse_target` in the library crate.

pub use cqs::parse_target;

use std::path::Path;

use anyhow::Result;
use cqs::config::Config;
use cqs::reference::{self, ReferenceIndex};
use cqs::store::Store;
use cqs::ResolvedTarget;

/// Resolve a target string to a [`ResolvedTarget`] (CLI wrapper).
///
/// Wraps the library's `resolve_target` with anyhow error conversion.
pub fn resolve_target(store: &Store, target: &str) -> Result<ResolvedTarget> {
    Ok(cqs::resolve_target(store, target)?)
}

/// Find a reference index by name from the project config.
///
/// Loads config, loads all references, finds the one matching `name`.
/// Returns an error with a user-friendly message if not found.
pub(crate) fn find_reference(root: &Path, name: &str) -> Result<ReferenceIndex> {
    let config = Config::load(root);
    let references = reference::load_references(&config.references);
    references
        .into_iter()
        .find(|r| r.name == name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Reference '{}' not found. Run 'cqs ref list' to see available references.",
                name
            )
        })
}
