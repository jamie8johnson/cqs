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
    drifted.sort_by(|a, b| {
        b.drift
            .partial_cmp(&a.drift)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

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
        entries.sort_by(|a, b| {
            b.drift
                .partial_cmp(&a.drift)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
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
}
