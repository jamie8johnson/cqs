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
    // Resolve target to get its chunk (for signature/type extraction)
    let resolved = crate::resolve_target(store, target_name)?;
    let target_chunk = resolved.0;
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

/// Resolve (name, overlap_count) pairs to RelatedFunction by looking up chunks.
fn resolve_to_related(store: &Store, pairs: &[(String, u32)]) -> Vec<RelatedFunction> {
    pairs
        .iter()
        .filter_map(|(name, count)| {
            // Try to find the chunk for this function name
            let chunks = store.get_chunks_by_name(name).ok()?;
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
fn find_type_overlap(
    store: &Store,
    target_name: &str,
    type_names: &[String],
    limit: usize,
) -> Result<Vec<RelatedFunction>, StoreError> {
    if type_names.is_empty() {
        return Ok(Vec::new());
    }

    // For each type name, find chunks whose signature contains it
    let mut type_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut chunk_info: std::collections::HashMap<String, (PathBuf, u32)> =
        std::collections::HashMap::new();

    for type_name in type_names {
        // Search chunks by signature containing the type name
        let chunks = store.search_chunks_by_signature(type_name)?;
        for chunk in chunks {
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
    }

    // Sort by overlap count descending
    let mut results: Vec<(String, u32)> = type_counts.into_iter().collect();
    results.sort_by(|a, b| b.1.cmp(&a.1));
    results.truncate(limit);

    Ok(results
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
