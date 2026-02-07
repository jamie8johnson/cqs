//! GC tool - report stale/missing files (read-only)
//!
//! The actual GC operation (prune + rebuild) should be run via `cqs gc` CLI
//! to avoid conflicts with the running MCP server's loaded index.

use std::collections::HashSet;

use anyhow::Result;
use serde_json::Value;

use crate::Parser;

use super::super::server::McpServer;

/// Report what GC would clean (read-only — actual cleanup via `cqs gc` CLI)
pub fn tool_gc(server: &McpServer) -> Result<Value> {
    let parser = Parser::new()?;
    let files = crate::enumerate_files(&server.project_root, &parser, false)?;
    let file_set: HashSet<_> = files.into_iter().collect();

    let (stale_count, missing_count) = server.store.count_stale_files(&file_set).unwrap_or((0, 0));

    let result = if stale_count == 0 && missing_count == 0 {
        serde_json::json!({
            "status": "clean",
            "message": "Index is clean. Nothing to do."
        })
    } else {
        serde_json::json!({
            "status": "needs_gc",
            "stale_files": stale_count,
            "missing_files": missing_count,
            "message": format!(
                "{} stale, {} missing. Run 'cqs gc' from CLI to clean up.",
                stale_count, missing_count
            )
        })
    };

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&result)?
        }]
    }))
}
