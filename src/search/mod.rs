//! Search algorithms and name matching
//!
//! Implements search methods on Store for semantic, hybrid, and index-guided
//! search. See `math.rs` for similarity scoring.

mod mmr;
mod query;
pub mod router;
pub(crate) mod scoring;
pub(crate) mod synonyms;

use crate::store::helpers::{ChunkSummary, SearchResult};
use crate::store::{Store, StoreError};

/// Result of resolving a target name to a concrete chunk.
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
/// Uses search_by_name with optional file filtering.
/// Returns the best-matching chunk and alternatives, or an error if none found.
/// Generic over store typestate — pure read path.
pub fn resolve_target<Mode>(
    store: &Store<Mode>,
    target: &str,
) -> Result<ResolvedTarget, StoreError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Chunk;
    use crate::test_helpers::{mock_embedding, setup_store};
    use std::path::PathBuf;

    /// Insert a chunk into the store for testing resolve_target.
    fn insert_chunk(store: &Store, name: &str, file: &str) {
        let chunk = Chunk {
            id: format!("{}:1:abcd1234", file),
            file: PathBuf::from(file),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: format!("fn {}() {{}}", name),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: format!("hash_{}", name),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        let embedding = mock_embedding(1.0);
        store.upsert_chunk(&chunk, &embedding, None).unwrap();
    }

    // ===== TC-19: resolve_target tests =====

    #[test]
    fn resolve_target_prefers_non_test_chunk() {
        let (store, _dir) = setup_store();
        // Insert a test chunk and a non-test chunk with the same name
        insert_chunk(&store, "my_func", "src/lib.rs");
        insert_chunk(&store, "test_my_func", "src/tests/lib_test.rs");
        // Also insert a test-named variant
        let test_chunk = Chunk {
            id: "tests/test.rs:1:abcd1234".to_string(),
            file: PathBuf::from("tests/test.rs"),
            language: crate::parser::Language::Rust,
            chunk_type: crate::language::ChunkType::Function,
            name: "my_func".to_string(),
            signature: "fn my_func()".to_string(),
            content: "fn my_func() {}".to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: "hash_my_func_test".to_string(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        };
        store
            .upsert_chunk(&test_chunk, &mock_embedding(2.0), None)
            .unwrap();

        let result = resolve_target(&store, "my_func").unwrap();
        // Should prefer the non-test chunk (src/lib.rs, not tests/)
        let path = result.chunk.file.to_string_lossy().to_string();
        assert!(
            !path.contains("/tests/"),
            "Should prefer non-test file, got: {}",
            path
        );
        assert!(
            result.alternatives.len() >= 2,
            "Should have multiple alternatives"
        );
    }

    #[test]
    fn resolve_target_file_filter_narrows_result() {
        let (store, _dir) = setup_store();
        insert_chunk(&store, "parse", "src/parser/mod.rs");
        insert_chunk(&store, "parse", "src/search/mod.rs");

        let result = resolve_target(&store, "src/search/mod.rs:parse").unwrap();
        let path = result.chunk.file.to_string_lossy().to_string();
        assert!(
            path.contains("search"),
            "File filter should narrow to search module, got: {}",
            path
        );
    }

    #[test]
    fn resolve_target_not_found_on_missing_name() {
        let (store, _dir) = setup_store();
        // Empty store, nothing indexed
        let err = resolve_target(&store, "nonexistent_function").unwrap_err();
        match err {
            StoreError::NotFound(msg) => {
                assert!(
                    msg.contains("nonexistent_function"),
                    "Error should mention the missing name, got: {}",
                    msg
                );
            }
            other => panic!("Expected NotFound, got: {:?}", other),
        }
    }

    #[test]
    fn resolve_target_file_filter_not_found() {
        let (store, _dir) = setup_store();
        insert_chunk(&store, "my_func", "src/lib.rs");

        // File filter that doesn't match any result
        let err = resolve_target(&store, "src/nonexistent.rs:my_func").unwrap_err();
        match err {
            StoreError::NotFound(msg) => {
                assert!(
                    msg.contains("nonexistent.rs"),
                    "Error should mention the file filter, got: {}",
                    msg
                );
            }
            other => panic!("Expected NotFound, got: {:?}", other),
        }
    }
}
