//! Health check — codebase quality snapshot
//!
//! Composes existing primitives (stats, dead code, staleness, hotspots, notes)
//! into a single report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::impact::find_hotspots;
use crate::store::helpers::IndexStats;
use crate::store::StoreError;
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
#[derive(Debug, Clone, serde::Serialize)]
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
/// Only `store.stats()` is fatal. All other sub-queries degrade gracefully,
/// populating defaults and adding warnings.
pub fn health_check<Mode>(
    store: &Store<Mode>,
    existing_files: &HashSet<PathBuf>,
    cqs_dir: &Path,
    root: &Path,
) -> Result<HealthReport, StoreError> {
    let _span = tracing::info_span!("health_check").entered();

    // Fatal: can't report without basic stats
    let stats = store.stats()?;

    let mut warnings = Vec::new();

    // Staleness
    let (stale_count, missing_count) = match store.count_stale_files(existing_files, root) {
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
    use crate::test_helpers::mock_embedding;
    use tempfile::TempDir;

    fn make_store() -> (Store, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join(crate::INDEX_DB_FILENAME);
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
            parser_version: 0,
        }
    }

    #[test]
    fn test_health_check_empty_store() {
        let (store, dir) = make_store();

        let files = HashSet::new();
        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

        assert_eq!(report.stats.total_chunks, 0);
        assert_eq!(report.dead_confident, 0);
        assert_eq!(report.hotspots.len(), 0);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn test_health_with_chunks() {
        let (store, dir) = make_store();

        // Insert 3 chunks with distinct files. Materialize each file on disk
        // under the project root so `list_stale_files`'s `metadata()` call
        // succeeds — P3 #135 changed the staleness check to treat metadata
        // failures as stale (with a `current_mtime = -1` sentinel) rather
        // than silently fresh, so synthetic happy-path tests must back the
        // chunks with real files.
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
            let file = format!("src/mod{}.rs", i);
            std::fs::write(dir.path().join(&file), format!("fn {}() {{ }}", name)).unwrap();
            let chunk = test_chunk(&file, name, 1, &format!("fn {}() {{ }}", name));
            // Stamp the stored mtime to a large future value so the post-#135
            // staleness check (now `current_mtime > stored_mtime`) correctly
            // sees these chunks as fresh — the file's real mtime from
            // `std::fs::write` above will be smaller.
            store
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(i64::MAX))
                .unwrap();
        }

        // existing_files matches all stored origins
        let files: HashSet<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("src/mod{}.rs", i)))
            .collect();

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

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
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(1000))
                .unwrap();
        }

        // Provide an empty existing_files set — all indexed files are "missing"
        let files: HashSet<PathBuf> = HashSet::new();

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

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
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(1000))
                .unwrap();
        }

        // No upsert_function_calls — nothing calls these functions
        let files: HashSet<PathBuf> = (0..3)
            .map(|i| PathBuf::from(format!("src/lib{}.rs", i)))
            .collect();

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

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
            .upsert_chunk(&target, &mock_embedding(0.0), Some(1000))
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
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(1000))
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

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

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

    /// TC-3: Verify untested_hotspots is populated when a high-caller function has no tests.
    /// The filter (health.rs lines 93-97) requires:
    ///   caller_count >= HOTSPOT_MIN_CALLERS (5)
    ///   test_count == 0
    ///   risk_level == High  (score = callers * 1.0 >= 5.0 with 0 tests)
    #[test]
    fn test_health_untested_hotspots() {
        let (store, dir) = make_store();

        // Insert the target function — will become the hotspot
        let target = test_chunk("src/core.rs", "untested_hot", 1, "fn untested_hot() { }");
        store
            .upsert_chunk(&target, &mock_embedding(0.0), Some(1000))
            .unwrap();

        // 6 callers, zero test functions → caller_count=6, test_count=0,
        // score=6.0 >= RISK_THRESHOLD_HIGH (5.0) → High risk → appears in untested_hotspots.
        for i in 0..6 {
            let caller_name = format!("caller_{}", i);
            let file = format!("src/user{}.rs", i);
            let chunk = test_chunk(
                &file,
                &caller_name,
                1,
                &format!("fn {}() {{ untested_hot() }}", caller_name),
            );
            store
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(1000))
                .unwrap();

            store
                .upsert_function_calls(
                    Path::new(&file),
                    &[FunctionCalls {
                        name: caller_name.clone(),
                        line_start: 1,
                        calls: vec![CallSite {
                            callee_name: "untested_hot".to_string(),
                            line_number: 2,
                        }],
                    }],
                )
                .unwrap();
        }

        let mut files: HashSet<PathBuf> = (0..6)
            .map(|i| PathBuf::from(format!("src/user{}.rs", i)))
            .collect();
        files.insert(PathBuf::from("src/core.rs"));

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

        // untested_hotspots must contain untested_hot
        let found = report
            .untested_hotspots
            .iter()
            .any(|h| h.name == "untested_hot");
        assert!(
            found,
            "Expected untested_hot in untested_hotspots (6 callers, 0 tests, score=6.0 → High). \
             Got: {:?}",
            report
                .untested_hotspots
                .iter()
                .map(|h| &h.name)
                .collect::<Vec<_>>()
        );

        // Sanity: the same function must also appear in hotspots
        let in_hotspots = report.hotspots.iter().any(|h| h.name == "untested_hot");
        assert!(
            in_hotspots,
            "untested_hot should also appear in hotspots, got: {:?}",
            report.hotspots.iter().map(|h| &h.name).collect::<Vec<_>>()
        );
    }

    /// TC-3b: Verify untested_hotspots is empty when a hotspot has test coverage.
    /// When a high-caller function has at least one test, it should not appear
    /// in untested_hotspots even if risk is otherwise High.
    #[test]
    fn test_health_untested_hotspots_excluded_when_tested() {
        let (store, dir) = make_store();

        // Insert the target function
        let target = test_chunk("src/core.rs", "tested_hot", 1, "fn tested_hot() { }");
        store
            .upsert_chunk(&target, &mock_embedding(0.0), Some(1000))
            .unwrap();

        // 6 callers
        for i in 0..6 {
            let caller_name = format!("caller_{}", i);
            let file = format!("src/caller{}.rs", i);
            let chunk = test_chunk(
                &file,
                &caller_name,
                1,
                &format!("fn {}() {{ tested_hot() }}", caller_name),
            );
            store
                .upsert_chunk(&chunk, &mock_embedding(0.0), Some(1000))
                .unwrap();

            store
                .upsert_function_calls(
                    Path::new(&file),
                    &[FunctionCalls {
                        name: caller_name.clone(),
                        line_start: 1,
                        calls: vec![CallSite {
                            callee_name: "tested_hot".to_string(),
                            line_number: 2,
                        }],
                    }],
                )
                .unwrap();
        }

        // Insert a test function that calls tested_hot — test_count becomes 1
        let test_name = "test_tested_hot";
        let test_file = "src/tests.rs";
        let test_content = format!("#[test] fn {}() {{ tested_hot() }}", test_name);
        let test_chunk_data = test_chunk(test_file, test_name, 50, &test_content);
        store
            .upsert_chunk(&test_chunk_data, &mock_embedding(0.0), Some(1000))
            .unwrap();
        store
            .upsert_function_calls(
                Path::new(test_file),
                &[FunctionCalls {
                    name: test_name.to_string(),
                    line_start: 50,
                    calls: vec![CallSite {
                        callee_name: "tested_hot".to_string(),
                        line_number: 51,
                    }],
                }],
            )
            .unwrap();

        let mut files: HashSet<PathBuf> = (0..6)
            .map(|i| PathBuf::from(format!("src/caller{}.rs", i)))
            .collect();
        files.insert(PathBuf::from("src/core.rs"));
        files.insert(PathBuf::from(test_file));

        let report = health_check(&store, &files, dir.path(), dir.path()).unwrap();

        let in_untested = report
            .untested_hotspots
            .iter()
            .any(|h| h.name == "tested_hot");
        assert!(
            !in_untested,
            "tested_hot should NOT appear in untested_hotspots because it has 1 test. \
             Got untested_hotspots: {:?}",
            report
                .untested_hotspots
                .iter()
                .map(|h| &h.name)
                .collect::<Vec<_>>()
        );
    }
}
