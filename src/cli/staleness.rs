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
