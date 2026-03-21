//! Co-occurrence analysis — find functions related by shared callers, callees, or types.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::focused_read::COMMON_TYPES;
use crate::store::helpers::StoreError;
use crate::store::Store;

/// A function related to the target with overlap count.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RelatedFunction {
    pub name: String,
    #[serde(serialize_with = "crate::serialize_path_normalized")]
    pub file: PathBuf,
    pub line: u32,
    pub overlap_count: u32,
}

/// Result of co-occurrence analysis for a target function.
#[derive(Debug, serde::Serialize)]
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
/// 3. Shared types — uses the same types (via type_edges)
pub fn find_related(
    store: &Store,
    target_name: &str,
    limit: usize,
) -> Result<RelatedResult, StoreError> {
    let _span = tracing::info_span!("find_related", target = target_name, limit).entered();
    // Resolve target to get its chunk
    let resolved = crate::resolve_target(store, target_name)?;
    let target = resolved.chunk.name.clone();

    // 1. Shared callers
    let shared_caller_pairs = store.find_shared_callers(&target, limit)?;
    let shared_callers = resolve_to_related(store, &shared_caller_pairs);

    // 2. Shared callees
    let shared_callee_pairs = store.find_shared_callees(&target, limit)?;
    let shared_callees = resolve_to_related(store, &shared_callee_pairs);

    // 3. Shared types — query type_edges for target's types, find other functions using them
    let type_pairs = store.get_types_used_by(&target)?;
    let type_names: Vec<String> = type_pairs
        .into_iter()
        .map(|t| t.type_name)
        .filter(|name| !COMMON_TYPES.contains(name.as_str()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    tracing::debug!(
        type_count = type_names.len(),
        "Extracted type names for related"
    );
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

/// Find functions that share types with the target via type_edges.
///
/// Uses batch type-edge queries instead of LIKE-based signature scanning.
fn find_type_overlap(
    store: &Store,
    target_name: &str,
    type_names: &[String],
    limit: usize,
) -> Result<Vec<RelatedFunction>, StoreError> {
    if type_names.is_empty() {
        return Ok(Vec::new());
    }

    // Batch query: for each type name, get all chunks that reference it
    let refs: Vec<&str> = type_names.iter().map(|s| s.as_str()).collect();
    let results = store.get_type_users_batch(&refs)?;

    let mut type_counts: HashMap<String, u32> = HashMap::new();
    let mut chunk_info: HashMap<String, (PathBuf, u32)> = HashMap::new();

    for chunks in results.values() {
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

    tracing::debug!(
        candidates = type_counts.len(),
        "Type overlap candidates found"
    );

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
    use crate::language::{ChunkType, Language};
    use std::path::Path;

    use crate::test_helpers::{mock_embedding, setup_store};

    /// Creates a mock Rust function chunk for testing purposes.
    ///
    /// This function generates a `Chunk` struct representing a Rust function with placeholder content and a blake3 hash-based identifier. It's useful for creating test data without parsing actual source files.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the function
    /// * `file` - The file path where the function is located
    /// * `chunk_type` - The type classification of this chunk
    ///
    /// # Returns
    ///
    /// A `crate::parser::Chunk` struct with all fields populated, including a generated function signature, placeholder content, and a hash-based ID.
    fn make_chunk(name: &str, file: &str, chunk_type: ChunkType) -> crate::parser::Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        crate::parser::Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    // ===== Existing struct-construction tests =====

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

    // ===== find_type_overlap logic tests =====

    /// Empty type_names → fast-path returns empty without touching Store.
    #[test]
    fn test_find_type_overlap_empty_type_names_returns_empty() {
        let (store, _dir) = setup_store();
        let result = find_type_overlap(&store, "target_fn", &[], 10).unwrap();
        assert!(
            result.is_empty(),
            "empty type_names must produce empty result"
        );
    }

    /// find_type_overlap excludes the target function itself even when it uses the shared type.
    #[test]
    fn test_find_type_overlap_excludes_target_itself() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.5);

        let target_chunk = make_chunk("target_fn", "src/lib.rs", ChunkType::Function);
        let other_chunk = make_chunk("other_fn", "src/other.rs", ChunkType::Function);

        store
            .upsert_chunks_batch(
                &[
                    (target_chunk.clone(), emb.clone()),
                    (other_chunk.clone(), emb.clone()),
                ],
                None,
            )
            .unwrap();

        // Both target_fn and other_fn reference type "MyType"
        let type_refs = vec![crate::parser::TypeRef {
            type_name: "MyType".to_string(),
            kind: None,
            line_number: 2,
        }];
        store
            .upsert_type_edges(&target_chunk.id, &type_refs)
            .unwrap();
        store
            .upsert_type_edges(&other_chunk.id, &type_refs)
            .unwrap();

        let result = find_type_overlap(&store, "target_fn", &["MyType".to_string()], 10).unwrap();

        // target_fn must NOT appear in results
        assert!(
            result.iter().all(|r| r.name != "target_fn"),
            "target function must be excluded from type overlap results"
        );
        // other_fn shares the type and should appear
        assert!(
            result.iter().any(|r| r.name == "other_fn"),
            "other_fn shares MyType and should be in results"
        );
        assert_eq!(result[0].overlap_count, 1);
    }

    /// find_type_overlap filters out non-Function/Method chunk types (e.g. Struct).
    #[test]
    fn test_find_type_overlap_ignores_non_callable_chunks() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.3);

        let fn_chunk = make_chunk("real_fn", "src/lib.rs", ChunkType::Function);
        let struct_chunk = make_chunk("MyStruct", "src/lib.rs", ChunkType::Struct);

        store
            .upsert_chunks_batch(
                &[
                    (fn_chunk.clone(), emb.clone()),
                    (struct_chunk.clone(), emb.clone()),
                ],
                None,
            )
            .unwrap();

        let type_refs = vec![crate::parser::TypeRef {
            type_name: "SharedType".to_string(),
            kind: None,
            line_number: 3,
        }];
        store.upsert_type_edges(&fn_chunk.id, &type_refs).unwrap();
        store
            .upsert_type_edges(&struct_chunk.id, &type_refs)
            .unwrap();

        // Search from a different target so neither of the above is self-excluded
        let result =
            find_type_overlap(&store, "unrelated_target", &["SharedType".to_string()], 10).unwrap();

        assert!(
            result.iter().any(|r| r.name == "real_fn"),
            "Function chunk should appear in type overlap"
        );
        assert!(
            result.iter().all(|r| r.name != "MyStruct"),
            "Struct chunk must be filtered out from type overlap"
        );
    }

    /// find_type_overlap sorts results by overlap_count descending.
    #[test]
    fn test_find_type_overlap_sorted_by_overlap_count_descending() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.4);

        // fn_a uses 2 shared types, fn_b uses 1
        let fn_a = make_chunk("fn_a", "src/a.rs", ChunkType::Function);
        let fn_b = make_chunk("fn_b", "src/b.rs", ChunkType::Function);

        store
            .upsert_chunks_batch(
                &[(fn_a.clone(), emb.clone()), (fn_b.clone(), emb.clone())],
                None,
            )
            .unwrap();

        let refs_a = vec![
            crate::parser::TypeRef {
                type_name: "TypeX".to_string(),
                kind: None,
                line_number: 1,
            },
            crate::parser::TypeRef {
                type_name: "TypeY".to_string(),
                kind: None,
                line_number: 2,
            },
        ];
        let refs_b = vec![crate::parser::TypeRef {
            type_name: "TypeX".to_string(),
            kind: None,
            line_number: 1,
        }];

        store.upsert_type_edges(&fn_a.id, &refs_a).unwrap();
        store.upsert_type_edges(&fn_b.id, &refs_b).unwrap();

        let result = find_type_overlap(
            &store,
            "unrelated_target",
            &["TypeX".to_string(), "TypeY".to_string()],
            10,
        )
        .unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0].name, "fn_a",
            "fn_a with 2 shared types should rank first"
        );
        assert_eq!(result[0].overlap_count, 2);
        assert_eq!(result[1].name, "fn_b");
        assert_eq!(result[1].overlap_count, 1);
    }

    /// find_type_overlap respects the limit parameter.
    #[test]
    fn test_find_type_overlap_respects_limit() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.6);

        // Create 3 functions that all share the same type
        let chunks: Vec<_> = ["fn_1", "fn_2", "fn_3"]
            .iter()
            .enumerate()
            .map(|(i, &name)| make_chunk(name, &format!("src/{}.rs", i), ChunkType::Function))
            .collect();

        let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
        store.upsert_chunks_batch(&pairs, None).unwrap();

        let type_refs = vec![crate::parser::TypeRef {
            type_name: "CommonType".to_string(),
            kind: None,
            line_number: 1,
        }];
        for chunk in &chunks {
            store.upsert_type_edges(&chunk.id, &type_refs).unwrap();
        }

        let result =
            find_type_overlap(&store, "unrelated_target", &["CommonType".to_string()], 2).unwrap();

        assert_eq!(result.len(), 2, "limit=2 should cap results at 2");
    }

    // ===== resolve_to_related tests =====

    /// resolve_to_related with empty pairs returns empty immediately.
    #[test]
    fn test_resolve_to_related_empty_pairs() {
        let (store, _dir) = setup_store();
        let result = resolve_to_related(&store, &[]);
        assert!(result.is_empty());
    }

    /// resolve_to_related skips pairs whose names are not in the store.
    #[test]
    fn test_resolve_to_related_missing_chunks_skipped() {
        let (store, _dir) = setup_store();
        // Pairs reference names that don't exist in the store → should return empty (not panic)
        let pairs = vec![
            ("ghost_fn".to_string(), 3u32),
            ("phantom_fn".to_string(), 1u32),
        ];
        let result = resolve_to_related(&store, &pairs);
        assert!(
            result.is_empty(),
            "pairs without matching store chunks should be silently dropped"
        );
    }

    /// resolve_to_related with real chunks returns RelatedFunction with correct fields.
    #[test]
    fn test_resolve_to_related_with_real_chunks() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.7);

        let chunk = make_chunk("worker_fn", "src/worker.rs", ChunkType::Function);
        store
            .upsert_chunks_batch(&[(chunk.clone(), emb)], None)
            .unwrap();

        let pairs = vec![("worker_fn".to_string(), 5u32)];
        let result = resolve_to_related(&store, &pairs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "worker_fn");
        assert_eq!(result[0].overlap_count, 5);
    }

    // ===== Shared callers/callees via store =====

    /// find_related returns shared_callers when two functions are called by the same caller.
    ///
    /// Tests the call-graph dimension of find_related end-to-end with real Store data.
    /// Because find_related calls resolve_target (which needs a chunk), we insert chunks first.
    #[test]
    fn test_shared_callers_detected_via_function_calls_table() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.5);

        // Insert chunks for the functions we care about
        let target = make_chunk("target_fn", "src/target.rs", ChunkType::Function);
        let peer = make_chunk("peer_fn", "src/peer.rs", ChunkType::Function);
        let caller = make_chunk("shared_caller", "src/caller.rs", ChunkType::Function);

        store
            .upsert_chunks_batch(
                &[
                    (target.clone(), emb.clone()),
                    (peer.clone(), emb.clone()),
                    (caller.clone(), emb.clone()),
                ],
                None,
            )
            .unwrap();

        // shared_caller calls both target_fn and peer_fn
        let calls = vec![crate::parser::FunctionCalls {
            name: "shared_caller".to_string(),
            line_start: 1,
            calls: vec![
                crate::parser::CallSite {
                    callee_name: "target_fn".to_string(),
                    line_number: 2,
                },
                crate::parser::CallSite {
                    callee_name: "peer_fn".to_string(),
                    line_number: 3,
                },
            ],
        }];
        store
            .upsert_function_calls(Path::new("src/caller.rs"), &calls)
            .unwrap();

        let result = find_related(&store, "target_fn", 10).unwrap();

        assert_eq!(result.target, "target_fn");
        assert!(
            result.shared_callers.iter().any(|r| r.name == "peer_fn"),
            "peer_fn should appear in shared_callers (both called by shared_caller); got: {:?}",
            result.shared_callers
        );
        // shared_callers should have overlap_count = 1 (one shared caller)
        let peer_entry = result
            .shared_callers
            .iter()
            .find(|r| r.name == "peer_fn")
            .unwrap();
        assert_eq!(peer_entry.overlap_count, 1);
    }

    /// find_related returns shared_callees when two functions call the same function.
    #[test]
    fn test_shared_callees_detected_via_function_calls_table() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(0.5);

        let target = make_chunk("target_fn", "src/target.rs", ChunkType::Function);
        let peer = make_chunk("peer_fn", "src/peer.rs", ChunkType::Function);
        let shared_callee = make_chunk("common_helper", "src/helper.rs", ChunkType::Function);

        store
            .upsert_chunks_batch(
                &[
                    (target.clone(), emb.clone()),
                    (peer.clone(), emb.clone()),
                    (shared_callee.clone(), emb.clone()),
                ],
                None,
            )
            .unwrap();

        // Both target_fn and peer_fn call common_helper
        let calls = vec![
            crate::parser::FunctionCalls {
                name: "target_fn".to_string(),
                line_start: 1,
                calls: vec![crate::parser::CallSite {
                    callee_name: "common_helper".to_string(),
                    line_number: 2,
                }],
            },
            crate::parser::FunctionCalls {
                name: "peer_fn".to_string(),
                line_start: 10,
                calls: vec![crate::parser::CallSite {
                    callee_name: "common_helper".to_string(),
                    line_number: 11,
                }],
            },
        ];
        store
            .upsert_function_calls(Path::new("src/all.rs"), &calls)
            .unwrap();

        let result = find_related(&store, "target_fn", 10).unwrap();

        assert!(
            result.shared_callees.iter().any(|r| r.name == "peer_fn"),
            "peer_fn should appear in shared_callees (both call common_helper); got: {:?}",
            result.shared_callees
        );
        let peer_entry = result
            .shared_callees
            .iter()
            .find(|r| r.name == "peer_fn")
            .unwrap();
        assert_eq!(peer_entry.overlap_count, 1);
    }
}
