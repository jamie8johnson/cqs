//! Co-occurrence analysis — find functions related by shared callers, callees, or types.

use std::path::PathBuf;

use crate::focused_read::extract_type_names;
use crate::store::helpers::StoreError;
use crate::store::Store;

/// A function related to the target with overlap count.
#[derive(Debug, Clone)]
pub struct RelatedFunction {
    pub name: String,
    pub file: PathBuf,
    pub line: u32,
    pub overlap_count: u32,
}

/// Result of co-occurrence analysis for a target function.
#[derive(Debug)]
pub struct RelatedResult {
    pub target: String,
    pub shared_callers: Vec<RelatedFunction>,
    pub shared_callees: Vec<RelatedFunction>,
    pub shared_types: Vec<RelatedFunction>,
}

/// Find functions related to `target_name` by co-occurrence.
///
/// Three dimensions:
/// 1. Shared callers — called by the same functions as target
/// 2. Shared callees — calls the same functions as target
/// 3. Shared types — uses the same custom types in their signature
pub fn find_related(
    store: &Store,
    target_name: &str,
    limit: usize,
) -> Result<RelatedResult, StoreError> {
    let _span = tracing::info_span!("find_related", target = target_name, limit).entered();
    // Resolve target to get its chunk (for signature/type extraction)
    let resolved = crate::resolve_target(store, target_name)?;
    let target_chunk = resolved.chunk;
    let target = target_chunk.name.clone();

    // 1. Shared callers
    let shared_caller_pairs = store.find_shared_callers(&target, limit)?;
    let shared_callers = resolve_to_related(store, &shared_caller_pairs);

    // 2. Shared callees
    let shared_callee_pairs = store.find_shared_callees(&target, limit)?;
    let shared_callees = resolve_to_related(store, &shared_callee_pairs);

    // 3. Shared types — extract type names from target signature, find other functions using them
    let type_names = extract_type_names(&target_chunk.signature);
    let shared_types = find_type_overlap(store, &target, &type_names, limit)?;

    Ok(RelatedResult {
        target,
        shared_callers,
        shared_callees,
        shared_types,
    })
}

/// Resolve (name, overlap_count) pairs to RelatedFunction by batch-looking up chunks.
///
/// Uses a single batch query instead of N individual `get_chunks_by_name` calls.
fn resolve_to_related(store: &Store, pairs: &[(String, u32)]) -> Vec<RelatedFunction> {
    if pairs.is_empty() {
        return Vec::new();
    }

    // Batch-fetch all names at once
    let names: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    let batch_results = match store.get_chunks_by_names_batch(&names) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to batch-resolve related functions");
            return Vec::new();
        }
    };

    pairs
        .iter()
        .filter_map(|(name, count)| {
            let chunks = batch_results.get(name.as_str())?;
            let chunk = chunks.first()?;
            Some(RelatedFunction {
                name: name.clone(),
                file: chunk.file.clone(),
                line: chunk.line_start,
                overlap_count: *count,
            })
        })
        .collect()
}

/// Find functions that share custom types with the target.
///
/// Uses a single batch query instead of N per-type LIKE scans.
fn find_type_overlap(
    store: &Store,
    target_name: &str,
    type_names: &[String],
    limit: usize,
) -> Result<Vec<RelatedFunction>, StoreError> {
    if type_names.is_empty() {
        return Ok(Vec::new());
    }

    // Single batch query for all type names at once
    let results = store.search_chunks_by_signatures_batch(type_names)?;

    let mut type_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut chunk_info: std::collections::HashMap<String, (PathBuf, u32)> =
        std::collections::HashMap::new();

    for (_type_name, chunk) in results {
        if chunk.name == target_name {
            continue;
        }
        if !matches!(
            chunk.chunk_type,
            crate::language::ChunkType::Function | crate::language::ChunkType::Method
        ) {
            continue;
        }
        *type_counts.entry(chunk.name.clone()).or_insert(0) += 1;
        chunk_info
            .entry(chunk.name.clone())
            .or_insert((chunk.file.clone(), chunk.line_start));
    }

    // Sort by overlap count descending
    let mut sorted: Vec<(String, u32)> = type_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted.truncate(limit);

    Ok(sorted
        .into_iter()
        .filter_map(|(name, count)| {
            let (file, line) = chunk_info.remove(&name)?;
            Some(RelatedFunction {
                name,
                file,
                line,
                overlap_count: count,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_related_function_fields() {
        let rf = RelatedFunction {
            name: "do_work".to_string(),
            file: PathBuf::from("src/worker.rs"),
            line: 42,
            overlap_count: 3,
        };
        assert_eq!(rf.name, "do_work");
        assert_eq!(rf.file, PathBuf::from("src/worker.rs"));
        assert_eq!(rf.line, 42);
        assert_eq!(rf.overlap_count, 3);
    }

    #[test]
    fn test_related_result_empty_dimensions() {
        let result = RelatedResult {
            target: "foo".to_string(),
            shared_callers: Vec::new(),
            shared_callees: Vec::new(),
            shared_types: Vec::new(),
        };
        assert_eq!(result.target, "foo");
        assert!(result.shared_callers.is_empty());
        assert!(result.shared_callees.is_empty());
        assert!(result.shared_types.is_empty());
    }

    #[test]
    fn test_related_result_populated() {
        let result = RelatedResult {
            target: "search".to_string(),
            shared_callers: vec![
                RelatedFunction {
                    name: "query".to_string(),
                    file: PathBuf::from("src/query.rs"),
                    line: 10,
                    overlap_count: 2,
                },
                RelatedFunction {
                    name: "filter".to_string(),
                    file: PathBuf::from("src/filter.rs"),
                    line: 20,
                    overlap_count: 1,
                },
            ],
            shared_callees: vec![RelatedFunction {
                name: "normalize".to_string(),
                file: PathBuf::from("src/utils.rs"),
                line: 5,
                overlap_count: 3,
            }],
            shared_types: Vec::new(),
        };
        assert_eq!(result.target, "search");
        assert_eq!(result.shared_callers.len(), 2);
        assert_eq!(result.shared_callees.len(), 1);
        assert_eq!(result.shared_callees[0].name, "normalize");
        assert_eq!(result.shared_callees[0].overlap_count, 3);
    }
}
