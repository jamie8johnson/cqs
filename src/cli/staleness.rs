//! Proactive staleness warnings for search results
//!
//! After query commands return results, checks if any result files have
//! changed since last index. Prints warning to stderr so JSON output
//! is not polluted.

use std::collections::HashSet;
use std::path::Path;

use colored::Colorize;

use cqs::normalize_slashes;
use cqs::Store;

/// Print the canonical stale-results warning to stderr. No-op on an empty
/// slice.
///
/// This is the single formatting seam shared by the CLI-direct path
/// ([`warn_stale_results`]) and the daemon-client translation path
/// (`dispatch.rs` reading `_meta.stale_origins` from a daemon envelope) —
/// one formatter so the two surfaces emit byte-identical warnings and
/// can't drift.
pub fn print_stale_warning<S: AsRef<str>>(files: &[S]) {
    let count = files.len();
    if count == 0 {
        return;
    }
    eprintln!(
        "{} {} result file{} changed since last index. Run 'cqs index' to update.",
        "warning:".yellow().bold(),
        count,
        if count == 1 { "" } else { "s" }
    );
    for file in files {
        eprintln!("  {}", normalize_slashes(file.as_ref()).dimmed());
    }
}

/// Extract `stale_origins` from a daemon envelope `_meta` value. Returns an
/// empty vec when the meta is absent, has no `stale_origins` key, or the key
/// isn't an array of strings. Pure extraction — split from the printing so
/// the seam is unit-testable without capturing stderr.
pub fn stale_origins_from_meta(meta: Option<&serde_json::Value>) -> Vec<String> {
    meta.and_then(|m| m.get("stale_origins"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Read `_meta.stale_origins` from a daemon envelope and print the same
/// stderr warning the CLI-direct path emits. Called from the daemon-response
/// translation in `dispatch.rs` so daemon-served searches warn exactly like
/// CLI-direct ones.
pub fn print_stale_warning_from_meta(meta: Option<&serde_json::Value>) {
    let files = stale_origins_from_meta(meta);
    print_stale_warning(&files);
}

/// Check result origins for staleness and print warning to stderr.
/// Returns the set of stale origins for callers that want to annotate results.
/// Errors are logged and swallowed — staleness check should never break a query.
pub fn warn_stale_results<Mode>(
    store: &Store<Mode>,
    origins: &[&str],
    root: &Path,
) -> HashSet<String> {
    let _span = tracing::info_span!("warn_stale_results", count = origins.len()).entered();
    match store.check_origins_stale(origins, root) {
        Ok(stale) => {
            if !stale.is_empty() {
                tracing::info!(count = stale.len(), "Stale result files detected");
                // Sorted for deterministic output (HashSet iteration order
                // is arbitrary).
                let mut files: Vec<&String> = stale.iter().collect();
                files.sort();
                print_stale_warning(&files);
            }
            stale
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check staleness");
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
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
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
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();

        // Origins that don't exist in the index should not panic
        let result = warn_stale_results(&store, &["nonexistent.rs", "ghost.py"], dir.path());
        // Should return empty or the nonexistent files — depends on implementation.
        // Key: it must not panic.
        assert!(
            result.is_empty(),
            "Nonexistent origins should produce empty stale set"
        );
    }

    #[test]
    fn test_stale_origins_from_meta_extracts_strings() {
        let meta = serde_json::json!({"stale_origins": ["src/a.rs", "src/b.rs"]});
        let files = stale_origins_from_meta(Some(&meta));
        assert_eq!(files, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
    }

    #[test]
    fn test_stale_origins_from_meta_absent_meta_is_empty() {
        assert!(stale_origins_from_meta(None).is_empty());
    }

    #[test]
    fn test_stale_origins_from_meta_missing_key_is_empty() {
        let meta = serde_json::json!({"worktree_stale": true});
        assert!(stale_origins_from_meta(Some(&meta)).is_empty());
    }

    #[test]
    fn test_stale_origins_from_meta_non_array_is_empty() {
        // Defensive: a malformed daemon response must not panic or print.
        let meta = serde_json::json!({"stale_origins": "src/a.rs"});
        assert!(stale_origins_from_meta(Some(&meta)).is_empty());
    }

    #[test]
    fn test_stale_origins_from_meta_skips_non_string_elements() {
        let meta = serde_json::json!({"stale_origins": ["src/a.rs", 42, null]});
        assert_eq!(
            stale_origins_from_meta(Some(&meta)),
            vec!["src/a.rs".to_string()]
        );
    }
}
