//! Drift detection — find functions that changed semantically between snapshots
//!
//! Thin wrapper over `semantic_diff()` focused on the "modified" entries.
//! Sorts by drift magnitude (most changed first), supports min-drift filtering.

use crate::diff::semantic_diff;
use crate::store::{Store, StoreError};

/// A function that drifted between snapshots.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriftEntry {
    /// Function/class name
    pub name: String,
    /// Source file path
    pub file: String,
    /// Type of code element
    pub chunk_type: String,
    /// Cosine similarity (lower = more drift)
    pub similarity: f32,
    /// 1.0 - similarity (higher = more drift)
    pub drift: f32,
}

/// Result of drift detection.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DriftResult {
    /// Reference name compared against
    pub reference: String,
    /// Similarity threshold used
    pub threshold: f32,
    /// Minimum drift filter applied
    pub min_drift: f32,
    /// Functions that drifted, sorted by drift descending
    pub drifted: Vec<DriftEntry>,
    /// Total matched pairs compared (drifted + unchanged)
    pub total_compared: usize,
    /// Count of functions below threshold (not drifted)
    pub unchanged: usize,
}

/// Detect semantic drift between a reference and the project.
///
/// Uses `semantic_diff()` internally, filtering to only the "modified" entries
/// and presenting them as drift (1.0 - similarity).
pub fn detect_drift(
    ref_store: &Store,
    project_store: &Store,
    ref_name: &str,
    threshold: f32,
    min_drift: f32,
    language_filter: Option<&str>,
) -> Result<DriftResult, StoreError> {
    let _span =
        tracing::info_span!("detect_drift", reference = ref_name, threshold, min_drift).entered();

    let diff = semantic_diff(
        ref_store,
        project_store,
        ref_name,
        "project",
        threshold,
        language_filter,
    )?;

    let total_compared = diff.modified.len() + diff.unchanged_count;

    let mut drifted: Vec<DriftEntry> = diff
        .modified
        .into_iter()
        .filter_map(|entry| {
            let sim = entry.similarity?; // skip entries with unknown similarity
            let drift = 1.0 - sim;
            if drift >= min_drift {
                Some(DriftEntry {
                    name: entry.name,
                    file: entry.file,
                    chunk_type: entry.chunk_type.to_string(),
                    similarity: sim,
                    drift,
                })
            } else {
                None
            }
        })
        .collect();

    // Sort by drift desc (most changed first)
    drifted.sort_by(|a, b| b.drift.total_cmp(&a.drift));

    tracing::info!(
        drifted = drifted.len(),
        total_compared,
        "Drift detection complete"
    );

    Ok(DriftResult {
        reference: ref_name.to_string(),
        threshold,
        min_drift,
        drifted,
        total_compared,
        unchanged: diff.unchanged_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    #[test]
    fn test_drift_empty_stores() {
        let (ref_store, _d1) = make_store();
        let (proj_store, _d2) = make_store();

        let result = detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.0, None).unwrap();
        assert!(result.drifted.is_empty());
        assert_eq!(result.total_compared, 0);
        assert_eq!(result.unchanged, 0);
    }

    #[test]
    fn test_drift_entry_fields() {
        let entry = DriftEntry {
            name: "foo".into(),
            file: "src/foo.rs".into(),
            chunk_type: "Function".into(),
            similarity: 0.7,
            drift: 0.3,
        };
        assert!((entry.drift - (1.0 - entry.similarity)).abs() < f32::EPSILON);
    }

    #[test]
    fn test_drift_sort_order() {
        let mut entries = vec![
            DriftEntry {
                name: "a".into(),
                file: "a.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.9,
                drift: 0.1,
            },
            DriftEntry {
                name: "b".into(),
                file: "b.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.5,
                drift: 0.5,
            },
            DriftEntry {
                name: "c".into(),
                file: "c.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.7,
                drift: 0.3,
            },
        ];
        entries.sort_by(|a, b| b.drift.total_cmp(&a.drift));
        assert_eq!(entries[0].name, "b"); // most drift
        assert_eq!(entries[1].name, "c");
        assert_eq!(entries[2].name, "a"); // least drift
    }

    #[test]
    fn test_drift_min_filter() {
        // Verify that entries below min_drift are excluded
        let entries = vec![
            DriftEntry {
                name: "small".into(),
                file: "a.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.92,
                drift: 0.08,
            },
            DriftEntry {
                name: "big".into(),
                file: "b.rs".into(),
                chunk_type: "Function".into(),
                similarity: 0.5,
                drift: 0.5,
            },
        ];
        let min_drift = 0.1;
        let filtered: Vec<_> = entries
            .into_iter()
            .filter(|e| e.drift >= min_drift)
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "big");
    }

    // --- Helpers for populated-store tests ---

    use crate::embedder::Embedding;
    use crate::language::ChunkType;
    use crate::parser::types::{Chunk, Language};
    use std::path::PathBuf;

    fn make_chunk(name: &str, file: &str, lang: Language) -> Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: lang,
            chunk_type: ChunkType::Function,
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

    /// Create an embedding with a distinct direction based on `seed`.
    ///
    /// Uses `seed` as the index of a "hot" dimension (set to 1.0) while the
    /// rest are 0.0, ensuring different seeds produce orthogonal vectors
    /// (cosine similarity ≈ 0). The seed is taken modulo 768.
    fn make_emb(seed: f32) -> Embedding {
        let mut v = vec![0.0f32; 768];
        let idx = (seed.abs() as usize) % 768;
        v[idx] = 1.0;
        v.push(0.0); // sentiment dimension
        Embedding::new(v)
    }

    // --- Populated-store tests (TC-14) ---

    #[test]
    fn test_drift_with_matching_functions() {
        let (ref_store, _d1) = make_store();
        let (proj_store, _d2) = make_store();

        let chunk = make_chunk("process_data", "src/lib.rs", Language::Rust);
        let emb = make_emb(1.0);

        ref_store.upsert_chunk(&chunk, &emb, Some(100)).unwrap();
        proj_store.upsert_chunk(&chunk, &emb, Some(100)).unwrap();

        let result = detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.0, None).unwrap();

        assert_eq!(result.total_compared, 1);
        assert_eq!(result.unchanged, 1);
        assert!(
            result.drifted.is_empty(),
            "identical embeddings should produce no drift"
        );
    }

    #[test]
    fn test_drift_with_different_embeddings() {
        let (ref_store, _d1) = make_store();
        let (proj_store, _d2) = make_store();

        let chunk = make_chunk("process_data", "src/lib.rs", Language::Rust);
        let emb_ref = make_emb(1.0);
        let emb_proj = make_emb(2.0);

        ref_store.upsert_chunk(&chunk, &emb_ref, Some(100)).unwrap();
        proj_store
            .upsert_chunk(&chunk, &emb_proj, Some(100))
            .unwrap();

        let result = detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.0, None).unwrap();

        assert!(
            result.total_compared >= 1,
            "should compare at least one pair"
        );
        assert!(
            !result.drifted.is_empty(),
            "different embeddings should produce drift"
        );
        let entry = &result.drifted[0];
        assert_eq!(entry.name, "process_data");
        assert!(entry.drift > 0.0, "drift should be positive");
        assert!(
            entry.similarity < 0.95,
            "similarity should be below threshold"
        );
    }

    #[test]
    fn test_drift_min_drift_filter_with_stores() {
        let (ref_store, _d1) = make_store();
        let (proj_store, _d2) = make_store();

        // Use partially overlapping embeddings so drift is moderate (not 0 or 1).
        // Ref: hot at index 0. Project: equal weight at indices 0 and 1.
        // Cosine similarity = 1 * (1/sqrt(2)) / (1 * 1) = 0.707...
        // Drift = 1 - 0.707 ≈ 0.293
        let mut ref_v = vec![0.0f32; 768];
        ref_v[0] = 1.0;
        ref_v.push(0.0);
        let emb_ref = Embedding::new(ref_v);

        let mut proj_v = vec![0.0f32; 768];
        proj_v[0] = 1.0;
        proj_v[1] = 1.0;
        let norm = (2.0f32).sqrt();
        proj_v[0] /= norm;
        proj_v[1] /= norm;
        proj_v.push(0.0);
        let emb_proj = Embedding::new(proj_v);

        let chunk = make_chunk("process_data", "src/lib.rs", Language::Rust);

        ref_store.upsert_chunk(&chunk, &emb_ref, Some(100)).unwrap();
        proj_store
            .upsert_chunk(&chunk, &emb_proj, Some(100))
            .unwrap();

        // First, confirm drift exists with min_drift=0.0
        let baseline = detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.0, None).unwrap();
        assert!(
            !baseline.drifted.is_empty(),
            "precondition: drift should exist (drift ≈ 0.29)"
        );
        let actual_drift = baseline.drifted[0].drift;
        assert!(
            actual_drift > 0.2 && actual_drift < 0.4,
            "expected drift ≈ 0.29, got {actual_drift}"
        );

        // Now filter with min_drift above the actual drift — should exclude everything
        let result = detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.5, None).unwrap();

        assert!(
            result.drifted.is_empty(),
            "min_drift=0.5 should filter out drift of ≈0.29"
        );
        // total_compared still counts the pair (it was compared, just filtered from output)
        assert!(result.total_compared >= 1);
    }

    #[test]
    fn test_drift_language_filter() {
        let (ref_store, _d1) = make_store();
        let (proj_store, _d2) = make_store();

        // Insert a Rust function with different embeddings
        let rust_chunk = make_chunk("rust_func", "src/lib.rs", Language::Rust);
        let emb_a = make_emb(1.0);
        let emb_b = make_emb(2.0);

        ref_store
            .upsert_chunk(&rust_chunk, &emb_a, Some(100))
            .unwrap();
        proj_store
            .upsert_chunk(&rust_chunk, &emb_b, Some(100))
            .unwrap();

        // Insert a Python function with different embeddings
        let py_chunk = make_chunk("py_func", "src/lib.py", Language::Python);
        let emb_c = make_emb(3.0);
        let emb_d = make_emb(4.0);

        ref_store
            .upsert_chunk(&py_chunk, &emb_c, Some(100))
            .unwrap();
        proj_store
            .upsert_chunk(&py_chunk, &emb_d, Some(100))
            .unwrap();

        // Filter to Rust only
        let result =
            detect_drift(&ref_store, &proj_store, "test-ref", 0.95, 0.0, Some("rust")).unwrap();

        // Should only see Rust function
        let names: Vec<&str> = result.drifted.iter().map(|e| e.name.as_str()).collect();
        assert!(
            !names.contains(&"py_func"),
            "Python function should be excluded by language filter"
        );
        // Rust func should be present (different embeddings → drift)
        assert!(
            names.contains(&"rust_func"),
            "Rust function should appear in drift results"
        );
        // total_compared should only count the Rust pair
        assert_eq!(
            result.total_compared, 1,
            "only one pair (rust) should be compared"
        );
    }
}
