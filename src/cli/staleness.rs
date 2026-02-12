//! Proactive staleness warnings for search results
//!
//! After query commands return results, checks if any result files have
//! changed since last index. Prints warning to stderr so JSON output
//! is not polluted.

use std::collections::HashSet;
use std::path::Path;

use colored::Colorize;

use cqs::Store;

/// Check result origins for staleness and print warning to stderr.
///
/// Returns the set of stale origins for callers that want to annotate results.
/// Errors are logged and swallowed — staleness check should never break a query.
pub fn warn_stale_results(store: &Store, origins: &[&str], root: &Path) -> HashSet<String> {
    match store.check_origins_stale(origins, root) {
        Ok(stale) => {
            if !stale.is_empty() {
                let count = stale.len();
                tracing::info!(count, "Stale result files detected");
                eprintln!(
                    "{} {} result file{} changed since last index. Run 'cqs index' to update.",
                    "warning:".yellow().bold(),
                    count,
                    if count == 1 { "" } else { "s" }
                );
                for file in &stale {
                    eprintln!("  {}", file.replace('\\', "/").dimmed());
                }
            }
            stale
        }
        Err(e) => {
            tracing::debug!(error = %e, "Failed to check staleness");
            HashSet::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_warn_stale_results_empty_origins() {
        // Create a temp store
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Empty origins should return empty set without error
        let result = warn_stale_results(&store, &[], dir.path());
        assert!(
            result.is_empty(),
            "Empty origins should produce empty stale set"
        );
    }

    #[test]
    fn test_warn_stale_results_nonexistent_origins() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Origins that don't exist in the index should not panic
        let result = warn_stale_results(&store, &["nonexistent.rs", "ghost.py"], dir.path());
        // Should return empty or the nonexistent files — depends on implementation.
        // Key: it must not panic.
        let _ = result;
    }
}
