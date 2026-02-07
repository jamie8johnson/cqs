//! Dead code detection MCP tool

use anyhow::Result;
use serde_json::Value;

use super::super::server::McpServer;

/// Execute dead code detection
pub fn tool_dead(server: &McpServer, arguments: Value) -> Result<Value> {
    let include_pub = arguments
        .get("include_pub")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (confident, possibly_pub) = server.store.find_dead_code(include_pub)?;

    let format_chunk = |chunk: &crate::store::ChunkSummary| {
        let rel = chunk
            .file
            .strip_prefix(&server.project_root)
            .unwrap_or(&chunk.file)
            .to_string_lossy()
            .replace('\\', "/");
        serde_json::json!({
            "name": chunk.name,
            "file": rel,
            "line_start": chunk.line_start,
            "line_end": chunk.line_end,
            "chunk_type": chunk.chunk_type.to_string(),
            "signature": chunk.signature,
            "language": chunk.language.to_string(),
        })
    };

    let result = serde_json::json!({
        "dead": confident.iter().map(&format_chunk).collect::<Vec<_>>(),
        "possibly_dead_pub": possibly_pub.iter().map(&format_chunk).collect::<Vec<_>>(),
        "total_dead": confident.len(),
        "total_possibly_dead_pub": possibly_pub.len(),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
