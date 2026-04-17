//! Stats command for cqs
//!
//! Displays index statistics.
//!
//! Core struct is [`StatsOutput`]; build with [`build_stats`].
//! CLI uses `print_stats_text()` for human output, batch serializes with `serde_json::to_value()`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context as _, Result};

use cqs::splade::index::SPLADE_INDEX_FILENAME;
use cqs::{HnswIndex, Parser};

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct CallGraphStats {
    pub total_calls: usize,
    pub unique_callers: usize,
    pub unique_callees: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TypeGraphStats {
    pub total_edges: usize,
    pub unique_types: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StatsOutput {
    pub total_chunks: usize,
    pub total_files: usize,
    pub notes: usize,
    pub call_graph: CallGraphStats,
    pub type_graph: TypeGraphStats,
    pub by_language: HashMap<String, usize>,
    pub by_type: HashMap<String, usize>,
    pub model: String,
    /// Embedding dimension for vectors in this index (read from `Store::dim()`).
    pub dim: usize,
    /// SPLADE model identifier from metadata, if recorded. `None` until a
    /// SPLADE-aware index pipeline writes the metadata key.
    pub splade_model: Option<String>,
    /// Size in bytes of `splade.index.bin`. `None` when the file does not
    /// exist (no SPLADE pass has run yet, or persistence is disabled).
    pub splade_index_size_bytes: Option<u64>,
    /// SPLADE coverage as a percentage of `total_chunks`. `None` when there
    /// are no chunks at all (avoids spurious 0/0 reporting on a fresh DB).
    pub splade_coverage_pct: Option<f64>,
    /// Size in bytes of `index.hnsw.data`. `None` when the file does not
    /// exist (HNSW not built yet — falls back to brute-force search).
    pub hnsw_data_bytes: Option<u64>,
    /// Size in bytes of `index.hnsw.graph`. `None` when the file does not
    /// exist (HNSW not built yet).
    pub hnsw_graph_bytes: Option<u64>,
    /// Size in bytes of `index.cagra` (cuVS GPU index). `None` when absent
    /// (no GPU available, sub-threshold corpus, or persistence disabled).
    pub cagra_size_bytes: Option<u64>,
    /// Total rows in the `llm_summaries` table across all `purpose` values.
    pub llm_summary_count: usize,
    pub schema_version: u32,
    // CLI-specific (batch omits these via Option)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_files: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hnsw_vectors: Option<usize>,
    // Batch-specific
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<usize>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Read a file's size in bytes, returning `None` when the file does not
/// exist or cannot be stat'd. Used for index-introspection fields where
/// "missing file" is a normal state (e.g. CAGRA on a CPU-only host).
fn file_size_bytes(path: &Path) -> Option<u64> {
    match std::fs::metadata(path) {
        Ok(m) => Some(m.len()),
        Err(e) => {
            tracing::debug!(
                path = %path.display(),
                error = %e,
                "Index file absent, omitting size from stats"
            );
            None
        }
    }
}

/// Build the core stats shared between CLI and batch.
///
/// Contains: total_chunks, total_files, notes, call_graph, type_graph,
/// by_language, by_type, model, schema_version, plus the index-introspection
/// fields (`dim`, `splade_*`, `hnsw_*`, `cagra_size_bytes`, `llm_summary_count`).
/// Callers add context-specific fields (stale_files, errors, etc.).
pub(crate) fn build_stats<Mode>(store: &cqs::Store<Mode>, cqs_dir: &Path) -> Result<StatsOutput> {
    let _span = tracing::info_span!("build_stats").entered();
    let stats = store.stats().context("Failed to read index statistics")?;
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let te_stats = store.type_edge_stats()?;

    // Index-introspection fields. SPLADE coverage collapses to None when
    // total_chunks == 0 so we don't show "0/0 = NaN%". The SPLADE file path
    // is `{cqs_dir}/splade.index.bin` and the HNSW pair is
    // `{cqs_dir}/index.hnsw.{data,graph}`. The CAGRA blob lives at
    // `{cqs_dir}/index.cagra`.
    let total_chunks = stats.total_chunks;
    let chunks_with_sparse = match store.chunks_with_sparse_count() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count chunks with sparse vectors");
            0
        }
    };
    let splade_coverage_pct = if total_chunks > 0 {
        Some((chunks_with_sparse as f64 / total_chunks as f64) * 100.0)
    } else {
        None
    };
    let llm_summary_count = match store.llm_summary_count() {
        Ok(n) => n as usize,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to count llm_summaries");
            0
        }
    };

    Ok(StatsOutput {
        total_chunks: total_chunks as usize,
        total_files: stats.total_files as usize,
        notes: note_count as usize,
        call_graph: CallGraphStats {
            total_calls: fc_stats.total_calls as usize,
            unique_callers: fc_stats.unique_callers as usize,
            unique_callees: fc_stats.unique_callees as usize,
        },
        type_graph: TypeGraphStats {
            total_edges: te_stats.total_edges as usize,
            unique_types: te_stats.unique_types as usize,
        },
        by_language: stats
            .chunks_by_language
            .iter()
            .map(|(l, c)| (l.to_string(), *c as usize))
            .collect(),
        by_type: stats
            .chunks_by_type
            .iter()
            .map(|(t, c)| (t.to_string(), *c as usize))
            .collect(),
        model: stats.model_name.clone(),
        dim: store.dim(),
        splade_model: store.stored_splade_model(),
        splade_index_size_bytes: file_size_bytes(&cqs_dir.join(SPLADE_INDEX_FILENAME)),
        splade_coverage_pct,
        hnsw_data_bytes: file_size_bytes(&cqs_dir.join("index.hnsw.data")),
        hnsw_graph_bytes: file_size_bytes(&cqs_dir.join("index.hnsw.graph")),
        cagra_size_bytes: file_size_bytes(&cqs_dir.join("index.cagra")),
        llm_summary_count,
        schema_version: stats.schema_version as u32,
        stale_files: None,
        missing_files: None,
        created_at: None,
        hnsw_vectors: None,
        errors: None,
    })
}

// ---------------------------------------------------------------------------
// Text output
// ---------------------------------------------------------------------------

fn print_stats_text(output: &StatsOutput) {
    println!("Index Statistics");
    println!("================");
    println!();
    println!("Total chunks: {}", output.total_chunks);
    println!("Total files:  {}", output.total_files);
    println!();
    println!("By language:");
    for (lang, count) in &output.by_language {
        println!("  {lang}: {count}");
    }
    println!();
    println!("By type:");
    for (chunk_type, count) in &output.by_type {
        println!("  {chunk_type}: {count}");
    }
    println!();
    println!("Model: {}", output.model);
    println!("Schema: v{}", output.schema_version);
    if let Some(ref created) = output.created_at {
        println!("Created: {created}");
    }
    println!();
    println!("Notes: {}", output.notes);
    println!(
        "Call graph: {} calls ({} callers, {} callees)",
        output.call_graph.total_calls,
        output.call_graph.unique_callers,
        output.call_graph.unique_callees,
    );
    println!(
        "Type graph: {} edges ({} types)",
        output.type_graph.total_edges, output.type_graph.unique_types,
    );

    // HNSW index status
    println!();
    match output.hnsw_vectors {
        Some(count) => {
            println!("HNSW index: {count} vectors (O(log n) search)");
        }
        None => {
            println!("HNSW index: not built (using brute-force O(n) search)");
            if output.total_chunks > 10_000 {
                println!("  Tip: Run 'cqs index' to build HNSW for faster search");
            }
        }
    }

    // Staleness warning
    let stale_count = output.stale_files.unwrap_or(0);
    let missing_count = output.missing_files.unwrap_or(0);
    if stale_count > 0 || missing_count > 0 {
        eprintln!();
        if stale_count > 0 {
            eprintln!(
                "Stale: {} file{} changed since last index",
                stale_count,
                if stale_count == 1 { "" } else { "s" }
            );
        }
        if missing_count > 0 {
            eprintln!(
                "Missing: {} file{} deleted since last index",
                missing_count,
                if missing_count == 1 { "" } else { "s" }
            );
        }
        eprintln!("  Run 'cqs index' to update, or 'cqs gc' to clean up deleted files");
    }

    // Warning for very large indexes
    if output.total_chunks > 50_000 {
        println!();
        println!(
            "Warning: {} chunks is a large index. Consider:",
            output.total_chunks
        );
        println!("  - Using --path to limit search scope");
        println!("  - Splitting into multiple projects");
    }
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

/// Display index statistics (chunk counts, languages, types)
pub(crate) fn cmd_stats(
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_stats").entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    // Check staleness by scanning filesystem
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();
    let (stale_count, missing_count) = store
        .count_stale_files(&file_set, root)
        .context("Failed to count stale files")?;

    // Use count_vectors to avoid loading full HNSW index just for stats
    let hnsw_vectors = HnswIndex::count_vectors(cqs_dir, "index");

    let stats = store.stats().context("Failed to read index statistics")?;

    let mut output = build_stats(store, cqs_dir)?;
    output.stale_files = Some(stale_count as usize);
    output.missing_files = Some(missing_count as usize);
    output.created_at = Some(stats.created_at.clone());
    output.hnsw_vectors = hnsw_vectors;

    if json || ctx.cli.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_stats_text(&output);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_output_serialization() {
        let output = StatsOutput {
            total_chunks: 100,
            total_files: 10,
            notes: 5,
            call_graph: CallGraphStats {
                total_calls: 50,
                unique_callers: 20,
                unique_callees: 30,
            },
            type_graph: TypeGraphStats {
                total_edges: 40,
                unique_types: 15,
            },
            by_language: [("rust".into(), 80), ("python".into(), 20)].into(),
            by_type: [("function".into(), 60), ("struct".into(), 40)].into(),
            model: "bge-large".into(),
            dim: 1024,
            splade_model: None,
            splade_index_size_bytes: None,
            splade_coverage_pct: None,
            hnsw_data_bytes: None,
            hnsw_graph_bytes: None,
            cagra_size_bytes: None,
            llm_summary_count: 0,
            schema_version: 17,
            stale_files: None,
            missing_files: None,
            created_at: None,
            hnsw_vectors: None,
            errors: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        // Verify normalized field names
        assert!(json.get("total_chunks").is_some());
        assert!(json.get("call_graph").is_some());
        assert!(json.get("by_language").is_some());
        // Verify the new index-introspection fields are always present
        // (Option fields serialize to null, not omitted).
        assert_eq!(json["dim"], 1024);
        assert!(json.get("splade_model").is_some());
        assert!(json.get("splade_index_size_bytes").is_some());
        assert!(json.get("hnsw_data_bytes").is_some());
        assert!(json.get("hnsw_graph_bytes").is_some());
        assert!(json.get("cagra_size_bytes").is_some());
        assert_eq!(json["llm_summary_count"], 0);
        // Verify None fields with skip_serializing_if are omitted
        assert!(json.get("stale_files").is_none());
        assert!(json.get("errors").is_none());
    }

    #[test]
    fn test_stats_output_with_optional_fields() {
        let output = StatsOutput {
            total_chunks: 50,
            total_files: 5,
            notes: 2,
            call_graph: CallGraphStats {
                total_calls: 10,
                unique_callers: 5,
                unique_callees: 8,
            },
            type_graph: TypeGraphStats {
                total_edges: 6,
                unique_types: 3,
            },
            by_language: HashMap::new(),
            by_type: HashMap::new(),
            model: "test".into(),
            dim: 768,
            splade_model: Some("naver/splade-cocondenser-ensembledistil".into()),
            splade_index_size_bytes: Some(67_108_864),
            splade_coverage_pct: Some(100.0),
            hnsw_data_bytes: Some(64_559_472),
            hnsw_graph_bytes: Some(8_084_767),
            cagra_size_bytes: Some(67_527_348),
            llm_summary_count: 12_345,
            schema_version: 17,
            stale_files: Some(3),
            missing_files: Some(1),
            created_at: Some("2026-01-01".into()),
            hnsw_vectors: Some(48),
            errors: None,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["stale_files"], 3);
        assert_eq!(json["missing_files"], 1);
        assert_eq!(json["hnsw_vectors"], 48);
        assert_eq!(json["dim"], 768);
        assert_eq!(
            json["splade_model"],
            "naver/splade-cocondenser-ensembledistil"
        );
        assert_eq!(json["splade_index_size_bytes"], 67_108_864);
        assert_eq!(json["splade_coverage_pct"], 100.0);
        assert_eq!(json["hnsw_data_bytes"], 64_559_472);
        assert_eq!(json["hnsw_graph_bytes"], 8_084_767);
        assert_eq!(json["cagra_size_bytes"], 67_527_348);
        assert_eq!(json["llm_summary_count"], 12_345);
        assert!(json.get("errors").is_none());
    }

    // ===== A2: index-introspection field tests =====

    /// Build an empty Store + tempdir for the A2 stats tests. Mirrors
    /// `cqs::test_helpers::setup_store` but inlined here because that helper
    /// is gated on `#[cfg(test)]` inside the `cqs` lib crate and isn't
    /// reachable from the `cqs` binary's test build.
    fn setup_stats_store() -> (cqs::Store, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
        let store = cqs::Store::open(&db_path).unwrap();
        store.init(&cqs::store::ModelInfo::default()).unwrap();
        (store, dir)
    }

    /// `cqs stats --json` exposes `dim` from `Store::dim()` so an agent can
    /// distinguish a 1024-dim BGE-large index from a 768-dim v9-200k one
    /// without a second metadata query.
    #[test]
    fn test_stats_json_includes_dim() {
        let (store, dir) = setup_stats_store();
        let output = build_stats(&store, dir.path()).unwrap();
        let json = serde_json::to_value(&output).unwrap();
        // setup_stats_store uses ModelInfo::default() which writes EMBEDDING_DIM
        assert_eq!(json["dim"], cqs::EMBEDDING_DIM);
    }

    /// On a fresh store with no CAGRA, HNSW, or SPLADE artifacts on disk,
    /// the file-size fields must serialize as JSON null — never as 0 (which
    /// would lie about whether the file exists) and never absent (the
    /// schema commitment is that the keys are always present).
    #[test]
    fn test_stats_json_handles_missing_optional_files() {
        let (store, dir) = setup_stats_store();
        let output = build_stats(&store, dir.path()).unwrap();
        let json = serde_json::to_value(&output).unwrap();
        assert!(
            json["cagra_size_bytes"].is_null(),
            "cagra_size_bytes should be null when index.cagra is absent, got {:?}",
            json["cagra_size_bytes"]
        );
        assert!(
            json["hnsw_data_bytes"].is_null(),
            "hnsw_data_bytes should be null when index.hnsw.data is absent"
        );
        assert!(
            json["hnsw_graph_bytes"].is_null(),
            "hnsw_graph_bytes should be null when index.hnsw.graph is absent"
        );
        assert!(
            json["splade_index_size_bytes"].is_null(),
            "splade_index_size_bytes should be null when splade.index.bin is absent"
        );
        assert!(
            json["splade_model"].is_null(),
            "splade_model should be null when no SPLADE metadata key is set"
        );
    }

    /// SPLADE coverage on a store with zero chunks must be `null`, not
    /// `0.0` and not `NaN` (which would be 0/0). The DB has no chunks and
    /// no sparse_vectors rows, so the percent is undefined.
    #[test]
    fn test_stats_json_splade_coverage_zero_when_no_sparse() {
        let (store, dir) = setup_stats_store();
        let output = build_stats(&store, dir.path()).unwrap();
        let json = serde_json::to_value(&output).unwrap();
        assert!(
            json["splade_coverage_pct"].is_null(),
            "splade_coverage_pct should be null on an empty index, got {:?}",
            json["splade_coverage_pct"]
        );
        assert_eq!(
            json["llm_summary_count"], 0,
            "llm_summary_count should be 0 on a fresh store"
        );
    }
}
