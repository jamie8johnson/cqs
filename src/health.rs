//! Health check — codebase quality snapshot
//!
//! Composes existing primitives (stats, dead code, staleness, hotspots, notes)
//! into a single report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::impact::find_hotspots;
use crate::store::helpers::IndexStats;
use crate::suggest::HOTSPOT_MIN_CALLERS;
use crate::{compute_risk_batch, HnswIndex, RiskLevel, Store};

/// Number of top hotspots to include in the health report.
const HEALTH_HOTSPOT_COUNT: usize = 5;

/// A function hotspot: high caller count indicates central importance.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Hotspot {
    pub name: String,
    pub caller_count: usize,
}

/// Codebase health report.
#[derive(Debug, serde::Serialize)]
pub struct HealthReport {
    pub stats: IndexStats,
    pub stale_count: u64,
    pub missing_count: u64,
    pub dead_confident: usize,
    pub dead_possible: usize,
    /// Top most-called functions
    pub hotspots: Vec<Hotspot>,
    /// High-caller functions with zero tests
    pub untested_hotspots: Vec<Hotspot>,
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
            let spots = find_hotspots(&graph, HEALTH_HOTSPOT_COUNT);

            // Find untested hotspots: high-caller functions (≥HOTSPOT_MIN_CALLERS) with 0 tests
            let untested = match store.find_test_chunks() {
                Ok(test_chunks) => {
                    let hotspot_names: Vec<&str> = spots.iter().map(|h| h.name.as_str()).collect();
                    let risks = compute_risk_batch(&hotspot_names, &graph, &test_chunks);
                    risks
                        .into_iter()
                        .zip(spots.iter())
                        .filter(|(r, _)| {
                            r.caller_count >= HOTSPOT_MIN_CALLERS
                                && r.test_count == 0
                                && r.risk_level == RiskLevel::High
                        })
                        .map(|(_, h)| h.clone())
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
    use crate::language::{ChunkType, Language};
    use crate::parser::{CallSite, Chunk, FunctionCalls};
    use crate::Embedding;
    use tempfile::TempDir;

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("index.db");
        let store = Store::open(&db_path).unwrap();
        store.init(&crate::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    fn test_chunk(file: &str, name: &str, line_start: u32, content: &str) -> Chunk {
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: format!("{}:{}:{}", file, line_start, &hash[..8]),
            file: PathBuf::from(file),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content: content.to_string(),
            doc: None,
            line_start,
            line_end: line_start + 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    fn mock_embedding() -> Embedding {
        Embedding::new(vec![0.0; 769])
    }

    #[test]
    fn test_health_check_empty_store() {
        let (store, dir) = make_store();

        let files = HashSet::new();
        let report = health_check(&store, &files, dir.path()).unwrap();

        assert_eq!(report.stats.total_chunks, 0);
        assert_eq!(report.dead_confident, 0);
        assert_eq!(report.hotspots.len(), 0);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn test_health_with_chunks() {
        let (store, dir) = make_store();

        // Insert 3 chunks with distinct files
        for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
            let file = format!("src/mod{}.rs", i);
            let chunk = test_chunk(&file, name, 1, &format!("fn {}() {{ }}", name));
            store
                .upsert_chunk(&chunk, &mock_embedding(), Some(1000))
                .unwrap();
        }

        // existing_files matches all stored origins
        let files: HashSet<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("src/mod{}.rs", i)))
            .collect();

        let report = health_check(&store, &files, dir.path()).unwrap();

        assert!(
            report.stats.total_chunks >= 3,
            "Expected at least 3 chunks, got {}",
            report.stats.total_chunks
        );
        assert_eq!(report.stale_count, 0);
        assert_eq!(report.missing_count, 0);
    }

    #[test]
    fn test_health_stale_files() {
        let (store, dir) = make_store();

        // Insert chunks for files that won't be in existing_files
        for (i, name) in ["foo", "bar"].iter().enumerate() {
            let file = format!("src/gone{}.rs", i);
            let chunk = test_chunk(&file, name, 1, &format!("fn {}() {{ }}", name));
            store
                .upsert_chunk(&chunk, &mock_embedding(), Some(1000))
                .unwrap();
        }

        // Provide an empty existing_files set — all indexed files are "missing"
        let files: HashSet<PathBuf> = HashSet::new();

        let report = health_check(&store, &files, dir.path()).unwrap();

        assert!(
            report.missing_count > 0,
            "Expected missing_count > 0 when existing_files is empty, got {}",
            report.missing_count
        );
    }

    #[test]
    fn test_health_dead_code() {
        let (store, dir) = make_store();

        // Insert functions with NO call graph edges pointing to them.
        // Use names that won't be filtered out by entry-point or test heuristics.
        for (i, name) in ["compute_value", "process_data", "transform_input"]
            .iter()
            .enumerate()
        {
            let file = format!("src/lib{}.rs", i);
            let chunk = test_chunk(&file, name, 10, &format!("fn {}() {{ todo!() }}", name));
            store
                .upsert_chunk(&chunk, &mock_embedding(), Some(1000))
                .unwrap();
        }

        // No upsert_function_calls — nothing calls these functions
        let files: HashSet<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("src/lib{}.rs", i)))
            .collect();

        let report = health_check(&store, &files, dir.path()).unwrap();

        assert!(
            report.dead_confident > 0,
            "Expected dead_confident > 0 for uncalled functions, got {}",
            report.dead_confident
        );
    }

    #[test]
    fn test_health_hotspots() {
        let (store, dir) = make_store();

        // Insert a "target" function that will be the hotspot
        let target = test_chunk("src/core.rs", "hot_target", 1, "fn hot_target() { }");
        store
            .upsert_chunk(&target, &mock_embedding(), Some(1000))
            .unwrap();

        // Insert 6 caller functions that each call hot_target
        let mut all_calls = Vec::new();
        for i in 0..6 {
            let caller_name = format!("caller_{}", i);
            let file = format!("src/caller{}.rs", i);
            let chunk = test_chunk(
                &file,
                &caller_name,
                1,
                &format!("fn {}() {{ hot_target() }}", caller_name),
            );
            store
                .upsert_chunk(&chunk, &mock_embedding(), Some(1000))
                .unwrap();

            // Record call graph: caller_i -> hot_target
            store
                .upsert_function_calls(
                    Path::new(&file),
                    &[FunctionCalls {
                        name: caller_name.clone(),
                        line_start: 1,
                        calls: vec![CallSite {
                            callee_name: "hot_target".to_string(),
                            line_number: 2,
                        }],
                    }],
                )
                .unwrap();

            all_calls.push(caller_name);
        }

        let mut files: HashSet<PathBuf> = (0..6)
            .map(|i| PathBuf::from(format!("src/caller{}.rs", i)))
            .collect();
        files.insert(PathBuf::from("src/core.rs"));

        let report = health_check(&store, &files, dir.path()).unwrap();

        assert!(
            !report.hotspots.is_empty(),
            "Expected at least one hotspot for a function called by 6 callers"
        );
        // The hotspot should be hot_target
        let top = &report.hotspots[0];
        assert_eq!(top.name, "hot_target");
        assert!(
            top.caller_count >= 5,
            "Expected hot_target caller count >= 5, got {}",
            top.caller_count
        );
    }
}
