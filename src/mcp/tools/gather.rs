//! Gather tool — smart context assembly

use anyhow::Result;
use serde_json::Value;

use crate::gather::{gather, GatherDirection, GatherOptions};

use super::super::server::McpServer;

pub fn tool_gather(server: &McpServer, arguments: Value) -> Result<Value> {
    let query = arguments
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

    let expand = arguments
        .get("expand")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1)
        .clamp(0, 5);

    let direction: GatherDirection = arguments
        .get("direction")
        .and_then(|v| v.as_str())
        .unwrap_or("both")
        .parse()?;

    let limit = arguments
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(10)
        .clamp(1, 50);

    let embedder = server.ensure_embedder()?;
    let query_embedding = embedder.embed_query(query)?;

    let opts = GatherOptions {
        expand_depth: expand,
        direction,
        limit,
    };

    let result = gather(&server.store, &query_embedding, &opts, &server.project_root)?;

    let json_chunks: Vec<_> = result
        .chunks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line_start": c.line_start,
                "line_end": c.line_end,
                "signature": c.signature,
                "score": c.score,
                "depth": c.depth,
                "content": c.content,
            })
        })
        .collect();

    let output = serde_json::json!({
        "query": query,
        "chunks": json_chunks,
        "expansion_capped": result.expansion_capped,
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&output)?
        }]
    }))
}
