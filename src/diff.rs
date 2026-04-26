//! Semantic diff between indexed snapshots
//!
//! Compares chunks by identity match + embedding similarity.
//! Reports added, removed, modified, and unchanged functions.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::language::ChunkType;
use crate::math::full_cosine_similarity;
use crate::store::{ChunkIdentity, Store, StoreError};

/// A single diff entry
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffEntry {
    /// Function/class name
    pub name: String,
    /// Source file path
    pub file: PathBuf,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Embedding similarity (only for Modified)
    pub similarity: Option<f32>,
}

/// Result of a semantic diff
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiffResult {
    /// Source label (reference name)
    pub source: String,
    /// Target label ("project" or reference name)
    pub target: String,
    /// Functions in target but not source
    pub added: Vec<DiffEntry>,
    /// Functions in source but not target
    pub removed: Vec<DiffEntry>,
    /// Functions in both with embedding similarity < threshold
    pub modified: Vec<DiffEntry>,
    /// Count of unchanged functions
    pub unchanged_count: usize,
}

/// Composite key for matching chunks across stores
///
/// Uses (file, name, type) as semantic identity. Deliberately excludes `line_start`
/// so that moving a function to a different line (e.g., adding code above it) doesn't
/// cause a false removed+added pair.
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ChunkKey {
    origin: String,
    name: String,
    chunk_type: ChunkType,
}

impl From<&ChunkIdentity> for ChunkKey {
    /// Converts a ChunkIdentity reference into a ChunkKey.
    ///
    /// This method creates a new ChunkKey by extracting and transforming the relevant fields from a ChunkIdentity. The file path is converted to an owned string, while the name and chunk_type are cloned directly.
    ///
    /// # Arguments
    ///
    /// * `c` - A reference to the ChunkIdentity to convert
    ///
    /// # Returns
    ///
    /// A new ChunkKey containing the origin (file path), name, and chunk_type from the source ChunkIdentity.
    fn from(c: &ChunkIdentity) -> Self {
        ChunkKey {
            origin: c.file.to_string_lossy().into_owned(),
            name: c.name.clone(),
            chunk_type: c.chunk_type,
        }
    }
}

/// Embedding batch size for `semantic_diff`'s embedding-fetch loop.
///
/// Default 1000 keeps each batch ~12 MB at 1024-dim. Scales linearly with model dim;
/// override via `CQS_DIFF_EMBEDDING_BATCH_SIZE` for larger models or tight memory budgets.
fn embedding_batch_size() -> usize {
    std::env::var("CQS_DIFF_EMBEDDING_BATCH_SIZE")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1000)
}

/// Run a semantic diff between two stores.
///
/// # Memory
///
/// Loads `ChunkIdentity` (no content/embeddings) for all chunks in both stores.
/// At ~500 bytes per identity, a 100k-chunk codebase uses ~50 MB — well within
/// normal process memory. The `language_filter` param pushes filtering into SQL.
pub fn semantic_diff<Mode1, Mode2>(
    source_store: &Store<Mode1>,
    target_store: &Store<Mode2>,
    source_label: &str,
    target_label: &str,
    threshold: f32,
    language_filter: Option<&str>,
) -> Result<DiffResult, StoreError> {
    let _span =
        tracing::info_span!("semantic_diff", source_label, target_label, threshold).entered();

    // Load identities from both stores (push language filter into SQL when present)
    let source_ids = source_store.all_chunk_identities_filtered(language_filter)?;
    let target_ids = target_store.all_chunk_identities_filtered(language_filter)?;

    // Collapse windowed chunks: keep only window_idx=0 (or None)
    let source_ids: Vec<_> = source_ids
        .into_iter()
        .filter(|c| c.window_idx.is_none_or(|i| i == 0))
        .collect();
    let target_ids: Vec<_> = target_ids
        .into_iter()
        .filter(|c| c.window_idx.is_none_or(|i| i == 0))
        .collect();

    tracing::debug!(
        source_count = source_ids.len(),
        target_count = target_ids.len(),
        "Loaded chunk identities"
    );

    // Build lookup maps: key → identity.
    // AC-12: If duplicate ChunkKeys exist (e.g., same name+type+file, different line_start),
    // the last one wins. This is acceptable: window_idx>0 duplicates are already filtered
    // above, and remaining collisions (rare: same name in two impl blocks) are approximate.
    let source_map: HashMap<ChunkKey, &ChunkIdentity> =
        source_ids.iter().map(|c| (ChunkKey::from(c), c)).collect();
    let target_map: HashMap<ChunkKey, &ChunkIdentity> =
        target_ids.iter().map(|c| (ChunkKey::from(c), c)).collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let mut unchanged_count = 0usize;

    // Find added (in target but not source) and matched pairs
    let mut matched_pairs: Vec<(&ChunkIdentity, &ChunkIdentity)> = Vec::new();

    for (key, target_chunk) in &target_map {
        if let Some(source_chunk) = source_map.get(key) {
            matched_pairs.push((source_chunk, target_chunk));
        } else {
            added.push(DiffEntry {
                name: target_chunk.name.clone(),
                file: target_chunk.file.clone(),
                chunk_type: target_chunk.chunk_type,
                similarity: None,
            });
        }
    }

    // Find removed (in source but not target)
    for (key, source_chunk) in &source_map {
        if !target_map.contains_key(key) {
            removed.push(DiffEntry {
                name: source_chunk.name.clone(),
                file: source_chunk.file.clone(),
                chunk_type: source_chunk.chunk_type,
                similarity: None,
            });
        }
    }

    // Batch-fetch embeddings in groups (default 1000) to bound memory usage.
    // For 20k pairs at ~12 bytes/dim * model_dim, each batch is ~9-12 MB instead of ~240 MB total.
    // Default scales to ~12 MB at 1024-dim; override via CQS_DIFF_EMBEDDING_BATCH_SIZE for larger
    // models or memory-constrained hosts.
    let batch_size = embedding_batch_size();

    for batch in matched_pairs.chunks(batch_size) {
        let batch_source_ids: Vec<&str> = batch.iter().map(|(s, _)| s.id.as_str()).collect();
        let batch_target_ids: Vec<&str> = batch.iter().map(|(_, t)| t.id.as_str()).collect();

        let source_embeddings = source_store.get_embeddings_by_ids(&batch_source_ids)?;
        let target_embeddings = target_store.get_embeddings_by_ids(&batch_target_ids)?;

        for (source_chunk, target_chunk) in batch {
            let source_emb = source_embeddings.get(&source_chunk.id);
            let target_emb = target_embeddings.get(&target_chunk.id);

            match (source_emb, target_emb) {
                (Some(s_emb), Some(t_emb)) => {
                    let sim =
                        full_cosine_similarity(s_emb.as_slice(), t_emb.as_slice()).unwrap_or(0.0);
                    if sim < threshold {
                        modified.push(DiffEntry {
                            name: target_chunk.name.clone(),
                            file: target_chunk.file.clone(),
                            chunk_type: target_chunk.chunk_type,
                            similarity: Some(sim),
                        });
                    } else {
                        unchanged_count += 1;
                    }
                }
                _ => {
                    // Can't compare — treat as modified
                    modified.push(DiffEntry {
                        name: target_chunk.name.clone(),
                        file: target_chunk.file.clone(),
                        chunk_type: target_chunk.chunk_type,
                        similarity: None,
                    });
                }
            }
        }
    }

    // Sort modified by similarity (most changed first).
    // Entries with None similarity (missing embeddings) sort to the end
    // rather than being conflated with maximally-changed (similarity=0.0).
    //
    // AC-V1.29-1: cascade on (file, name, chunk_type) so identical
    // similarities (common when many chunks hit the exact threshold
    // boundary or all lose their embeddings) produce deterministic output.
    // `HashMap` iteration is process-seed-dependent and the previous sort
    // preserved that non-determinism into `cqs diff` / `cqs drift` JSON.
    modified.sort_by(|a, b| {
        match (a.similarity, b.similarity) {
            (Some(sa), Some(sb)) => sa.total_cmp(&sb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| a.file.cmp(&b.file))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| format!("{:?}", a.chunk_type).cmp(&format!("{:?}", b.chunk_type)))
    });

    Ok(DiffResult {
        source: source_label.to_string(),
        target: target_label.to_string(),
        added,
        removed,
        modified,
        unchanged_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // full_cosine_similarity tests are in math.rs (canonical location)

    #[test]
    fn test_chunk_key_equality() {
        let k1 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "bar".into(),
            chunk_type: ChunkType::Function,
        };
        let k2 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "bar".into(),
            chunk_type: ChunkType::Function,
        };
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_chunk_key_different_line_same_identity() {
        // Moving a function to a different line should NOT change its identity
        let k1 = ChunkKey {
            origin: "Foo.java".into(),
            name: "process".into(),
            chunk_type: ChunkType::Method,
        };
        let k2 = ChunkKey {
            origin: "Foo.java".into(),
            name: "process".into(),
            chunk_type: ChunkType::Method,
        };
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_chunk_key_different_type() {
        // Same name but different chunk type should NOT match
        let k1 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "Foo".into(),
            chunk_type: ChunkType::Struct,
        };
        let k2 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "Foo".into(),
            chunk_type: ChunkType::Function,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_diff_sort_none_similarity_at_end() {
        // Entries with None similarity should sort after entries with known similarity,
        // not be conflated with similarity=0.0 (maximally changed).
        let mut entries = [
            DiffEntry {
                name: "known_low".into(),
                file: "a.rs".into(),
                chunk_type: ChunkType::Function,
                similarity: Some(0.3),
            },
            DiffEntry {
                name: "unknown".into(),
                file: "b.rs".into(),
                chunk_type: ChunkType::Function,
                similarity: None,
            },
            DiffEntry {
                name: "known_high".into(),
                file: "c.rs".into(),
                chunk_type: ChunkType::Function,
                similarity: Some(0.8),
            },
        ];

        // Apply the same sort as semantic_diff
        entries.sort_by(|a, b| {
            match (a.similarity, b.similarity) {
                (Some(sa), Some(sb)) => sa.total_cmp(&sb),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| format!("{:?}", a.chunk_type).cmp(&format!("{:?}", b.chunk_type)))
        });

        // Most changed (lowest similarity) first, unknown at end
        assert_eq!(entries[0].name, "known_low");
        assert_eq!(entries[1].name, "known_high");
        assert_eq!(entries[2].name, "unknown");
    }

    #[test]
    fn test_diff_sort_tie_break_deterministic() {
        // AC-V1.29-1: entries with identical similarity must sort
        // deterministically by (file, name, chunk_type). Previously
        // HashMap iteration order bled into the output — shuffled inputs
        // produced different outputs across process invocations.
        let make_entry = |name: &str, file: &str, chunk_type: ChunkType| DiffEntry {
            name: name.into(),
            file: file.into(),
            chunk_type,
            similarity: Some(0.5),
        };

        let sort_them = |entries: &mut Vec<DiffEntry>| {
            entries.sort_by(|a, b| {
                match (a.similarity, b.similarity) {
                    (Some(sa), Some(sb)) => sa.total_cmp(&sb),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| format!("{:?}", a.chunk_type).cmp(&format!("{:?}", b.chunk_type)))
            });
        };

        let mut canonical = vec![
            make_entry("alpha", "src/a.rs", ChunkType::Function),
            make_entry("beta", "src/a.rs", ChunkType::Function),
            make_entry("gamma", "src/b.rs", ChunkType::Function),
            make_entry("delta", "src/a.rs", ChunkType::Struct),
        ];
        sort_them(&mut canonical);

        // Lexicographic file order: a.rs < b.rs; within a.rs name order:
        // alpha < beta < delta (deltaStruct vs Function is the chunk_type
        // cascade — "Function" < "Struct" lexicographically).
        assert_eq!(canonical[3].file.to_string_lossy(), "src/b.rs");
        assert_eq!(canonical[3].name, "gamma");

        // Deterministic across 100 permutations of the same input.
        for seed in 0..100u64 {
            let mut shuffled = vec![
                make_entry("alpha", "src/a.rs", ChunkType::Function),
                make_entry("beta", "src/a.rs", ChunkType::Function),
                make_entry("gamma", "src/b.rs", ChunkType::Function),
                make_entry("delta", "src/a.rs", ChunkType::Struct),
            ];
            // Deterministic pseudo-shuffle from `seed` — small linear
            // congruential scramble, reproducible and dependency-free.
            let mut state = seed.wrapping_mul(2862933555777941757).wrapping_add(1);
            for i in (1..shuffled.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (state as usize) % (i + 1);
                shuffled.swap(i, j);
            }
            sort_them(&mut shuffled);
            let canonical_names: Vec<&str> = canonical.iter().map(|e| e.name.as_str()).collect();
            let shuffled_names: Vec<&str> = shuffled.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(
                canonical_names, shuffled_names,
                "shuffle seed={seed} produced different sort order"
            );
        }
    }

    #[test]
    fn test_language_primary_extension() {
        use crate::parser::Language;
        assert_eq!(Language::Rust.primary_extension(), "rs");
        assert_eq!(Language::Python.primary_extension(), "py");
        assert_eq!(Language::TypeScript.primary_extension(), "ts");
        assert_eq!(Language::JavaScript.primary_extension(), "js");
        assert_eq!(Language::Go.primary_extension(), "go");
        assert_eq!(Language::C.primary_extension(), "c");
        assert_eq!(Language::Java.primary_extension(), "java");
        assert_eq!(Language::Markdown.primary_extension(), "md");
        // Unknown falls back to input string
        assert_eq!(
            "unknown"
                .parse::<Language>()
                .map(|l| l.primary_extension())
                .unwrap_or("unknown"),
            "unknown"
        );
    }
}
