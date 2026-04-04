//! Neighbors command — brute-force cosine nearest neighbors for a function
//!
//! Unlike `similar` which uses HNSW index, this does a full brute-force scan
//! to return exact top-K neighbors with similarity scores.

use anyhow::{Context as _, Result};

use cqs::store::{ChunkSummary, Store};
use cqs::{rel_display, resolve_target};

// ─── Output types ──────────────────────────────────────────────────────────

/// A neighbor entry with similarity score.
#[derive(Debug, serde::Serialize)]
struct NeighborEntry {
    name: String,
    file: String,
    line_start: u32,
    chunk_type: String,
    score: f32,
}

/// Typed JSON output for the neighbors command.
#[derive(Debug, serde::Serialize)]
struct NeighborsOutput {
    target: String,
    neighbors: Vec<NeighborEntry>,
    count: usize,
}

// ─── Shared builder ────────────────────────────────────────────────────────

/// Build typed neighbors output — constructs entries from neighbor results.
fn build_neighbors_output(
    target_name: &str,
    neighbors: &[(ChunkSummary, f32)],
    root: &std::path::Path,
) -> NeighborsOutput {
    let _span = tracing::info_span!(
        "build_neighbors_output",
        target = target_name,
        count = neighbors.len()
    )
    .entered();

    let entries: Vec<NeighborEntry> = neighbors
        .iter()
        .map(|(chunk, sim)| NeighborEntry {
            name: chunk.name.clone(),
            file: rel_display(&chunk.file, root),
            line_start: chunk.line_start,
            chunk_type: chunk.chunk_type.to_string(),
            score: *sim,
        })
        .collect();

    let count = entries.len();
    NeighborsOutput {
        target: target_name.to_string(),
        neighbors: entries,
        count,
    }
}

// ─── Internal helpers ──────────────────────────────────────────────────────

/// Dot product for L2-normalized vectors (= cosine similarity).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Find top-K nearest neighbors by brute-force cosine similarity.
fn find_neighbors(
    store: &Store,
    target: &ChunkSummary,
    limit: usize,
) -> Result<Vec<(ChunkSummary, f32)>> {
    let _span = tracing::info_span!("find_neighbors", target = %target.name, limit).entered();

    // Get target embedding
    let (_chunk, target_embedding) =
        store
            .get_chunk_with_embedding(&target.id)?
            .with_context(|| {
                format!(
                    "Could not load embedding for '{}'. Index may be corrupt.",
                    target.name
                )
            })?;

    let target_slice = target_embedding.as_slice();

    // Brute-force scan all chunk embeddings via batched iterator
    let mut scored: Vec<(String, f32)> = Vec::new();
    for batch_result in store.embedding_batches(5000) {
        let batch = batch_result.context("Failed to read embedding batch")?;
        for (id, embedding) in batch {
            if id == target.id {
                continue; // exclude self
            }
            let sim = dot(target_slice, embedding.as_slice());
            scored.push((id, sim));
        }
    }

    // Sort descending by similarity
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(limit);

    if scored.is_empty() {
        return Ok(Vec::new());
    }

    // Fetch full chunk data for top results
    let ids: Vec<&str> = scored.iter().map(|(id, _)| id.as_str()).collect();
    let chunk_summaries = fetch_chunk_summaries(store, &ids)?;

    let results: Vec<(ChunkSummary, f32)> = scored
        .into_iter()
        .filter_map(|(id, sim)| chunk_summaries.get(&id).map(|chunk| (chunk.clone(), sim)))
        .collect();

    Ok(results)
}

/// Fetch ChunkSummary for a set of chunk IDs.
fn fetch_chunk_summaries(
    store: &Store,
    ids: &[&str],
) -> Result<std::collections::HashMap<String, ChunkSummary>> {
    // Group IDs by origin (file) and fetch per-file, then filter
    // Alternatively, since we have few IDs (limit is small), look up by name
    // Actually we can use search_by_name for each unique name. But that's N+1.
    // Better: use get_chunk_with_embedding (already loads ChunkSummary) for each.
    let mut map = std::collections::HashMap::new();
    for id in ids {
        if let Ok(Some((chunk, _emb))) = store.get_chunk_with_embedding(id) {
            map.insert(id.to_string(), chunk);
        }
    }
    Ok(map)
}

// ─── CLI command ───────────────────────────────────────────────────────────

pub(crate) fn cmd_neighbors(
    ctx: &crate::cli::CommandContext,
    name: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_neighbors", name, limit).entered();
    let store = &ctx.store;
    let root = &ctx.root;

    let resolved = resolve_target(store, name).context("Failed to resolve target")?;
    let target = &resolved.chunk;

    let neighbors = find_neighbors(store, target, limit)?;
    let output = build_neighbors_output(&target.name, &neighbors, root);

    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        use colored::Colorize;
        println!(
            "{} {} ({})",
            "Neighbors of".cyan(),
            target.name.bold(),
            rel_display(&target.file, root).dimmed()
        );
        if output.neighbors.is_empty() {
            println!("  No neighbors found.");
        } else {
            for e in &output.neighbors {
                println!(
                    "  {:.3}  {} [{}] ({}:{})",
                    e.score, e.name, e.chunk_type, e.file, e.line_start
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_product_identical_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((dot(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dot_product_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((dot(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn neighbor_entry_serializes() {
        let entry = NeighborEntry {
            name: "foo".to_string(),
            file: "src/lib.rs".to_string(),
            line_start: 5,
            chunk_type: "Function".to_string(),
            score: 0.95,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["name"], "foo");
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
        let sim = json["score"].as_f64().unwrap();
        assert!((sim - 0.95).abs() < 0.001);
    }

    #[test]
    fn neighbors_output_empty() {
        let output = build_neighbors_output("my_func", &[], std::path::Path::new("/project"));
        assert_eq!(output.target, "my_func");
        assert!(output.neighbors.is_empty());
        assert_eq!(output.count, 0);
    }

    #[test]
    fn neighbors_output_serializes() {
        let output = NeighborsOutput {
            target: "test_fn".to_string(),
            neighbors: vec![NeighborEntry {
                name: "similar_fn".to_string(),
                file: "src/lib.rs".to_string(),
                line_start: 10,
                chunk_type: "Function".to_string(),
                score: 0.85,
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["target"], "test_fn");
        assert_eq!(json["count"], 1);
        assert_eq!(json["neighbors"][0]["line_start"], 10);
        assert!(json["neighbors"][0].get("line").is_none());
    }
}
