//! Semantic diff between indexed snapshots
//!
//! Compares chunks by identity match + embedding similarity.
//! Reports added, removed, modified, and unchanged functions.

use std::collections::HashMap;

use crate::store::{ChunkIdentity, Store, StoreError};

/// A single diff entry
#[derive(Debug)]
pub struct DiffEntry {
    /// Function/class name
    pub name: String,
    /// Source file path
    pub file: String,
    /// Type of code element
    pub chunk_type: String,
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
#[derive(Hash, Eq, PartialEq, Clone, Debug)]
struct ChunkKey {
    origin: String,
    name: String,
    chunk_type: String,
    line_start: u32,
}

impl From<&ChunkIdentity> for ChunkKey {
    fn from(c: &ChunkIdentity) -> Self {
        ChunkKey {
            origin: c.origin.clone(),
            name: c.name.clone(),
            chunk_type: c.chunk_type.clone(),
            line_start: c.line_start,
        }
    }
}

/// Compute cosine similarity between two embeddings
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
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
    // Load identities from both stores
    let source_ids = source_store.all_chunk_identities()?;
    let target_ids = target_store.all_chunk_identities()?;

    // Collapse windowed chunks: keep only window_idx=0 (or None)
    let source_ids: Vec<_> = source_ids
        .into_iter()
        .filter(|c| c.window_idx.is_none_or(|i| i == 0))
        .collect();
    let target_ids: Vec<_> = target_ids
        .into_iter()
        .filter(|c| c.window_idx.is_none_or(|i| i == 0))
        .collect();

    // Apply language filter
    let source_ids: Vec<_> = if let Some(lang) = language_filter {
        source_ids
            .into_iter()
            .filter(|c| {
                c.chunk_type != "unknown"
                    && c.origin.ends_with(&format!(".{}", lang_extension(lang)))
            })
            .collect()
    } else {
        source_ids
    };

    let target_ids: Vec<_> = if let Some(lang) = language_filter {
        target_ids
            .into_iter()
            .filter(|c| {
                c.chunk_type != "unknown"
                    && c.origin.ends_with(&format!(".{}", lang_extension(lang)))
            })
            .collect()
    } else {
        target_ids
    };

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
                chunk_type: target_chunk.chunk_type.clone(),
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
                chunk_type: source_chunk.chunk_type.clone(),
                similarity: None,
            });
        }
    }

    // Compare embeddings for matched pairs
    for (source_chunk, target_chunk) in &matched_pairs {
        let source_emb = source_store.get_chunk_with_embedding(&source_chunk.id)?;
        let target_emb = target_store.get_chunk_with_embedding(&target_chunk.id)?;

        match (source_emb, target_emb) {
            (Some((_, s_emb)), Some((_, t_emb))) => {
                let sim = cosine_similarity(s_emb.as_slice(), t_emb.as_slice());
                if sim < threshold {
                    modified.push(DiffEntry {
                        name: target_chunk.name.clone(),
                        file: target_chunk.origin.clone(),
                        chunk_type: target_chunk.chunk_type.clone(),
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
                    chunk_type: target_chunk.chunk_type.clone(),
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

/// Map language name to file extension for filtering
fn lang_extension(lang: &str) -> &str {
    match lang {
        "rust" => "rs",
        "python" => "py",
        "typescript" => "ts",
        "javascript" => "js",
        "go" => "go",
        "c" => "c",
        "java" => "java",
        _ => lang,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_chunk_key_equality() {
        let k1 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "bar".into(),
            chunk_type: "function".into(),
            line_start: 10,
        };
        let k2 = ChunkKey {
            origin: "src/foo.rs".into(),
            name: "bar".into(),
            chunk_type: "function".into(),
            line_start: 10,
        };
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_chunk_key_different_line() {
        // Java overloads: same name, different line
        let k1 = ChunkKey {
            origin: "Foo.java".into(),
            name: "process".into(),
            chunk_type: "method".into(),
            line_start: 10,
        };
        let k2 = ChunkKey {
            origin: "Foo.java".into(),
            name: "process".into(),
            chunk_type: "method".into(),
            line_start: 25,
        };
        assert_ne!(k1, k2);
    }

    #[test]
    fn test_lang_extension() {
        assert_eq!(lang_extension("rust"), "rs");
        assert_eq!(lang_extension("python"), "py");
        assert_eq!(lang_extension("typescript"), "ts");
        assert_eq!(lang_extension("javascript"), "js");
        assert_eq!(lang_extension("go"), "go");
        assert_eq!(lang_extension("c"), "c");
        assert_eq!(lang_extension("java"), "java");
        assert_eq!(lang_extension("unknown"), "unknown");
    }
}
