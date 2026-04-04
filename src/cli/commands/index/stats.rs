//! Stats command for cqs
//!
//! Displays index statistics.
//!
//! Core struct is [`StatsOutput`]; build with [`build_stats`].
//! CLI uses `print_stats_text()` for human output, batch serializes with `serde_json::to_value()`.

use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::{Context as _, Result};

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

/// Build the core stats shared between CLI and batch.
///
/// Contains: total_chunks, total_files, notes, call_graph, type_graph,
/// by_language, by_type, model, schema_version.
/// Callers add context-specific fields (stale_files, errors, etc.).
pub(crate) fn build_stats(store: &cqs::Store) -> Result<StatsOutput> {
    let _span = tracing::info_span!("build_stats").entered();
    let stats = store.stats().context("Failed to read index statistics")?;
    let note_count = store.note_count()?;
    let fc_stats = store.function_call_stats()?;
    let te_stats = store.type_edge_stats()?;

    Ok(StatsOutput {
        total_chunks: stats.total_chunks as usize,
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
pub(crate) fn cmd_stats(ctx: &crate::cli::CommandContext, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_stats").entered();
    let store = &ctx.store;
    let root = &ctx.root;
    let cqs_dir = &ctx.cqs_dir;

    // Check staleness by scanning filesystem
    let parser = Parser::new()?;
    let files = crate::cli::enumerate_files(root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();
    let (stale_count, missing_count) = store
        .count_stale_files(&file_set)
        .context("Failed to count stale files")?;

    // Use count_vectors to avoid loading full HNSW index just for stats
    let hnsw_vectors = HnswIndex::count_vectors(cqs_dir, "index");

    let stats = store.stats().context("Failed to read index statistics")?;

    let mut output = build_stats(store)?;
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
            schema_version: 16,
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
        // Verify None fields are omitted
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
            schema_version: 16,
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
        assert!(json.get("errors").is_none());
    }
}
