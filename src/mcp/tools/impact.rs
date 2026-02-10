//! Impact tool — what breaks if you change a function

use anyhow::Result;
use serde_json::Value;

use crate::impact::{analyze_impact, impact_to_json, impact_to_mermaid};

use super::super::server::McpServer;
use super::resolve::resolve_target;

pub fn tool_impact(server: &McpServer, arguments: Value) -> Result<Value> {
    let name = arguments
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: name"))?;
    let depth = arguments
        .get("depth")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1)
        .clamp(1, 10);
    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    let (chunk, _) = resolve_target(&server.store, name)?;

    let result = analyze_impact(&server.store, &chunk.name, depth)?;

    if format == "mermaid" {
        let text = impact_to_mermaid(&result, &server.project_root);
        return Ok(serde_json::json!({"content": [{"type": "text", "text": text}]}));
    }

    let json = impact_to_json(&result, &server.project_root);
    Ok(
        serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&json)?}]}),
    )
}
