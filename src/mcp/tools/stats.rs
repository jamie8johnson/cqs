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

    // Check HNSW index status
    let cq_dir = server.project_root.join(".cq");
    let hnsw_status = if HnswIndex::exists(&cq_dir, "index") {
        match HnswIndex::load(&cq_dir, "index") {
            Ok(hnsw) => format!("{} vectors (O(log n) search)", hnsw.len()),
            Err(_) => "exists but failed to load".to_string(),
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

    let result = serde_json::json!({
        "total_chunks": stats.total_chunks,
        "total_files": stats.total_files,
        "by_language": stats.chunks_by_language.iter()
            .map(|(l, c)| (l.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "by_type": stats.chunks_by_type.iter()
            .map(|(t, c)| (t.to_string(), c))
            .collect::<std::collections::HashMap<_, _>>(),
        "index_path": server.project_root.join(".cq/index.db").to_string_lossy(),
        "model": stats.model_name,
        "last_indexed": stats.updated_at,
        "schema_version": stats.schema_version,
        "hnsw_index": hnsw_status,
        "active_index": active_index,
        "warning": warning,
    });

    // MCP tools/call requires content array format
    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
