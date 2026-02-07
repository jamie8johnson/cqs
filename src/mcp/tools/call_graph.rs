//! Call graph tools - callers and callees

use anyhow::Result;
use serde_json::Value;

use super::super::server::McpServer;

/// Find functions that call the specified function
pub fn tool_callers(server: &McpServer, arguments: Value) -> Result<Value> {
    let name = arguments
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;

    // Use full call graph (includes large functions)
    let callers = server.store.get_callers_full(name)?;

    let result = serde_json::json!({
        "function": name,
        "callers": callers.iter().map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": c.file.to_string_lossy().replace('\\', "/"),
                "line": c.line,
            })
        }).collect::<Vec<_>>(),
        "count": callers.len(),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}

/// Find functions called by the specified function
pub fn tool_callees(server: &McpServer, arguments: Value) -> Result<Value> {
    let name = arguments
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;

    // Use full call graph (includes large functions)
    // No file context available from MCP tool input — pass None
    let callees = server.store.get_callees_full(name, None)?;

    let result = serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
