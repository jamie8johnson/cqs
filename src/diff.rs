//! Semantic diff between indexed snapshots
//!
//! Compares chunks by identity match + embedding similarity.
//! Reports added, removed, modified, and unchanged functions.

use std::collections::HashMap;

use crate::language::ChunkType;
use crate::math::full_cosine_similarity;
use crate::store::{ChunkIdentity, Store, StoreError};

/// A single diff entry
#[derive(Debug)]
pub struct DiffEntry {
    /// Function/class name
    pub name: String,
    /// Source file path
    pub file: String,
    /// Type of code element
    pub chunk_type: ChunkType,
    /// Embedding similarity (only for Modified)
    pub similarity: Option<f32>,
}

/// Result of a semantic diff
#[derive(Debug)]
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
    fn from(c: &ChunkIdentity) -> Self {
        ChunkKey {
            origin: c.origin.clone(),
            name: c.name.clone(),
            chunk_type: c.chunk_type.parse().unwrap_or(ChunkType::Function),
        }
    }
}

/// Run a semantic diff between two stores
pub fn semantic_diff(
    source_store: &Store,
    target_store: &Store,
    source_label: &str,
    target_label: &str,
    threshold: f32,
    language_filter: Option<&str>,
) -> Result<DiffResult, StoreError> {
    let _span = tracing::info_span!("semantic_diff").entered();

    // Load identities from both stores (push language filter into SQL when present)
    let source_ids = source_store.all_chunk_identities_filtered(language_filter)?;
    let target_ids = target_store.all_chunk_identities_filtered(language_filter)?;

    // Collapse windowed chunks: keep only window_idx=0 (or None)
    // When language filter is active, also exclude "unknown" chunk types
    let source_ids: Vec<_> = source_ids
        .into_iter()
        .filter(|c| {
            c.window_idx.is_none_or(|i| i == 0)
                && (language_filter.is_none() || c.chunk_type != "unknown")
        })
        .collect();
    let target_ids: Vec<_> = target_ids
        .into_iter()
        .filter(|c| {
            c.window_idx.is_none_or(|i| i == 0)
                && (language_filter.is_none() || c.chunk_type != "unknown")
        })
        .collect();

    tracing::debug!(
        source_count = source_ids.len(),
        target_count = target_ids.len(),
        "Loaded chunk identities"
    );

    // Build lookup maps: key → (id, identity)
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
                file: target_chunk.origin.clone(),
                chunk_type: target_chunk
                    .chunk_type
                    .parse()
                    .unwrap_or(ChunkType::Function),
                similarity: None,
            });
        }
    }

    // Find removed (in source but not target)
    for (key, source_chunk) in &source_map {
        if !target_map.contains_key(key) {
            removed.push(DiffEntry {
                name: source_chunk.name.clone(),
                file: source_chunk.origin.clone(),
                chunk_type: source_chunk
                    .chunk_type
                    .parse()
                    .unwrap_or(ChunkType::Function),
                similarity: None,
            });
        }
    }

    // Batch-fetch all embeddings upfront to avoid N+1 queries
    let source_ids: Vec<&str> = matched_pairs.iter().map(|(s, _)| s.id.as_str()).collect();
    let target_ids: Vec<&str> = matched_pairs.iter().map(|(_, t)| t.id.as_str()).collect();

    let source_embeddings = source_store.get_embeddings_by_ids(&source_ids)?;
    let target_embeddings = target_store.get_embeddings_by_ids(&target_ids)?;

    // Compare embeddings for matched pairs using pre-fetched data
    for (source_chunk, target_chunk) in &matched_pairs {
        let source_emb = source_embeddings.get(&source_chunk.id);
        let target_emb = target_embeddings.get(&target_chunk.id);

        match (source_emb, target_emb) {
            (Some(s_emb), Some(t_emb)) => {
                let sim = full_cosine_similarity(s_emb.as_slice(), t_emb.as_slice());
                if sim < threshold {
                    modified.push(DiffEntry {
                        name: target_chunk.name.clone(),
                        file: target_chunk.origin.clone(),
                        chunk_type: target_chunk
                            .chunk_type
                            .parse()
                            .unwrap_or(ChunkType::Function),
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
                    file: target_chunk.origin.clone(),
                    chunk_type: target_chunk
                        .chunk_type
                        .parse()
                        .unwrap_or(ChunkType::Function),
                    similarity: None,
                });
            }
        }
    }

    // Sort modified by similarity (most changed first)
    modified.sort_by(|a, b| {
        a.similarity
            .unwrap_or(0.0)
            .partial_cmp(&b.similarity.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
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

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((full_cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(full_cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((full_cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(full_cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(full_cosine_similarity(&a, &b), 0.0);
    }

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
