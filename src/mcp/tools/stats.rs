//! Stats tool - index statistics

use anyhow::Result;
use serde_json::Value;

use crate::hnsw::HnswIndex;

use super::super::server::McpServer;

/// Get index statistics
pub fn tool_stats(server: &McpServer) -> Result<Value> {
    let stats = server.store.stats()?;

    let warning = if stats.total_chunks > 100_000 {
        Some(format!(
            "{} chunks is very large. Consider using --path to limit search scope.",
            stats.total_chunks
        ))
    } else {
        None
    };

    // Check HNSW index status (lightweight count, no full load)
    let cq_dir = server.project_root.join(".cq");
    let hnsw_status = if HnswIndex::exists(&cq_dir, "index") {
        match HnswIndex::count_vectors(&cq_dir, "index") {
            Some(count) => format!("{} vectors (O(log n) search)", count),
            None => "exists but failed to read".to_string(),
        }
    } else {
        "not built".to_string()
    };

    // Check active index type (HNSW or CAGRA)
    let active_index = {
        let guard = server.index.read().unwrap_or_else(|e| {
            tracing::debug!("Index RwLock poisoned (prior panic), recovering");
            e.into_inner()
        });
        match guard.as_ref() {
            Some(idx) => format!("{} ({} vectors)", idx.name(), idx.len()),
            None => "none loaded".to_string(),
        }
    };

    // Collect reference info
    let references: Vec<_> = server
        .references
        .iter()
        .map(|r| {
            let chunks = r.store.chunk_count().unwrap_or(0);
            let hnsw = r
                .index
                .as_ref()
                .map(|idx| format!("{} vectors", idx.len()))
                .unwrap_or_else(|| "not built".to_string());
            serde_json::json!({
                "name": r.name,
                "chunks": chunks,
                "hnsw": hnsw,
                "weight": r.weight,
            })
        })
        .collect();

    let mut result = serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "index_path": ".cq/index.db",
        "model": stats.model_name,
        "last_indexed": stats.updated_at,
        "schema_version": stats.schema_version,
        "hnsw_index": hnsw_status,
        "active_index": active_index,
        "warning": warning,
    });

    if !references.is_empty() {
        result["references"] = serde_json::json!(references);
    }

    // MCP tools/call requires content array format
    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
