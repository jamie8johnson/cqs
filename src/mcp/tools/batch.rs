//! Batch tool — execute multiple queries in one tool call

use anyhow::{bail, Result};
use serde_json::Value;

use super::super::server::McpServer;
use super::{
    audit, call_graph, context, dead, diff, explain, gather, gc, impact, notes, read, search,
    similar, stats, test_map, trace,
};

pub fn tool_batch(server: &McpServer, arguments: Value) -> Result<Value> {
    let queries = arguments
        .get("queries")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Missing 'queries' array"))?;

    if queries.len() > 10 {
        bail!("Batch size exceeds maximum of 10 queries");
    }

    let mut results = Vec::new();
    for query in queries {
        let tool = query.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        let args = query
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let result = match tool {
            "search" => search::tool_search(server, args),
            "callers" => call_graph::tool_callers(server, args),
            "callees" => call_graph::tool_callees(server, args),
            "explain" => explain::tool_explain(server, args),
            "similar" => similar::tool_similar(server, args),
            "stats" => stats::tool_stats(server),
            "gather" => gather::tool_gather(server, args),
            "impact" => impact::tool_impact(server, args),
            "trace" => trace::tool_trace(server, args),
            "test_map" => test_map::tool_test_map(server, args),
            "context" => context::tool_context(server, args),
            "dead" => dead::tool_dead(server, args),
            "read" => read::tool_read(server, args),
            "diff" => diff::tool_diff(server, args),
            "gc" => gc::tool_gc(server),
            "audit_mode" => audit::tool_audit_mode(server, args),
            "add_note" => notes::tool_add_note(server, args),
            "update_note" => notes::tool_update_note(server, args),
            "remove_note" => notes::tool_remove_note(server, args),
            _ => Err(anyhow::anyhow!(
                "Unknown batch tool: '{}'. Valid: search, callers, callees, explain, similar, stats, \
                 gather, impact, trace, test_map, context, dead, read, diff, gc, audit_mode, \
                 add_note, update_note, remove_note",
                tool
            )),
        };

        let entry = match result {
            Ok(val) => {
                // Extract inner content text to avoid double-encoding
                let inner = val
                    .get("content")
                    .and_then(|c| c.as_array())
                    .and_then(|a| a.first())
                    .and_then(|item| item.get("text"))
                    .and_then(|t| t.as_str())
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .unwrap_or(val.clone());
                serde_json::json!({"tool": tool, "result": inner})
            }
            Err(e) => {
                tracing::warn!(tool = %tool, error = %e, "Batch tool execution failed");
                serde_json::json!({"tool": tool, "error": e.to_string()})
            }
        };
        results.push(entry);
    }

    Ok(
        serde_json::json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&results)?}]}),
    )
}
