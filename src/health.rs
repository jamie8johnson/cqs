//! Health check — codebase quality snapshot
//!
//! Composes existing primitives (stats, dead code, staleness, hotspots, notes)
//! into a single report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::impact::find_hotspots;
use crate::store::helpers::IndexStats;
use crate::{compute_risk_batch, HnswIndex, RiskLevel, Store};

/// Codebase health report.
#[derive(Debug)]
pub struct HealthReport {
    pub stats: IndexStats,
    pub stale_count: u64,
    pub missing_count: u64,
    pub dead_confident: usize,
    pub dead_possible: usize,
    /// Top most-called functions: (name, caller_count)
    pub hotspots: Vec<(String, usize)>,
    /// High-caller functions with zero tests: (name, caller_count)
    pub untested_hotspots: Vec<(String, usize)>,
    pub note_count: u64,
    pub note_warnings: u64,
    pub hnsw_vectors: Option<usize>,
    /// Non-fatal warnings from degraded sub-queries
    pub warnings: Vec<String>,
}

/// Run a comprehensive health check on the index.
///
/// Only `store.stats()` is fatal. All other sub-queries degrade gracefully,
/// populating defaults and adding warnings.
pub fn health_check(
    store: &Store,
    existing_files: &HashSet<PathBuf>,
    cqs_dir: &Path,
) -> Result<HealthReport> {
    let _span = tracing::info_span!("health_check").entered();

    // Fatal: can't report without basic stats
    let stats = store.stats()?;

    let mut warnings = Vec::new();

    // Staleness
    let (stale_count, missing_count) = match store.count_stale_files(existing_files) {
        Ok((s, m)) => (s, m),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count stale files");
            warnings.push(format!("Staleness check failed: {e}"));
            (0, 0)
        }
    };

    // Dead code
    let (dead_confident, dead_possible) = match store.find_dead_code(true) {
        Ok((confident, possible)) => (confident.len(), possible.len()),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to find dead code");
            warnings.push(format!("Dead code detection failed: {e}"));
            (0, 0)
        }
    };

    // Call graph → hotspots + untested hotspot detection
    let (hotspots, untested_hotspots) = match store.get_call_graph() {
        Ok(graph) => {
            let spots = find_hotspots(&graph, 5);

            // Find untested hotspots: functions with 5+ callers and 0 tests
            let untested = match store.find_test_chunks() {
                Ok(test_chunks) => {
                    let hotspot_names: Vec<&str> = spots.iter().map(|(n, _)| n.as_str()).collect();
                    let risks = compute_risk_batch(&hotspot_names, &graph, &test_chunks);
                    risks
                        .into_iter()
                        .zip(spots.iter())
                        .filter(|(r, _)| {
                            r.caller_count >= 5
                                && r.test_count == 0
                                && r.risk_level == RiskLevel::High
                        })
                        .map(|(_, (name, count))| (name.clone(), *count))
                        .collect()
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to find test chunks");
                    warnings.push(format!("Test coverage check failed: {e}"));
                    Vec::new()
                }
            };

            (spots, untested)
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get call graph");
            warnings.push(format!("Call graph analysis failed: {e}"));
            (Vec::new(), Vec::new())
        }
    };

    // Notes
    let (note_count, note_warnings) = match store.note_stats() {
        Ok(ns) => (ns.total, ns.warnings),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to get note stats");
            warnings.push(format!("Note stats failed: {e}"));
            (0, 0)
        }
    };

    // HNSW index
    let hnsw_vectors = HnswIndex::count_vectors(cqs_dir, "index");

    Ok(HealthReport {
        stats,
        stale_count,
        missing_count,
        dead_confident,
        dead_possible,
        hotspots,
        untested_hotspots,
        note_count,
        note_warnings,
        hnsw_vectors,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_health_check_empty_store() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();

        let files = HashSet::new();
        let report = health_check(&store, &files, dir.path()).unwrap();

        assert_eq!(report.stats.total_chunks, 0);
        assert_eq!(report.dead_confident, 0);
        assert_eq!(report.hotspots.len(), 0);
        assert!(report.warnings.is_empty());
    }
}
