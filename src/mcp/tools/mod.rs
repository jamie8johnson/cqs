//! MCP tool handlers
//!
//! Each tool provides a specific capability to MCP clients.

mod audit;
mod call_graph;
mod notes;
mod read;
mod search;
mod stats;

use anyhow::{bail, Result};
use serde_json::Value;

use super::server::McpServer;
use super::types::{Tool, ToolsListResult};

/// Handle tools/list request - return available tools
pub fn handle_tools_list() -> Result<Value> {
    let tools = vec![
        Tool {
            name: "cqs_search".into(),
            description: "Search code semantically. Find functions/methods by concept, not just name. Example: 'retry with exponential backoff' finds retry logic regardless of naming.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language description of what you're looking for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum results (default: 5, max: 20)",
                        "default": 5
                    },
                    "threshold": {
                        "type": "number",
                        "description": "Minimum similarity score 0.0-1.0 (default: 0.3)",
                        "default": 0.3
                    },
                    "language": {
                        "type": "string",
                        // Keep in sync with crate::parser::Language variants
                        "enum": ["rust", "python", "typescript", "javascript", "go", "c", "java"],
                        "description": "Filter by language (optional)"
                    },
                    "path_pattern": {
                        "type": "string",
                        "description": "Glob pattern to filter paths (e.g., 'src/api/**')"
                    },
                    "name_boost": {
                        "type": "number",
                        "description": "Weight for name matching 0.0-1.0 (default: 0.2)",
                        "default": 0.2
                    },
                    "semantic_only": {
                        "type": "boolean",
                        "description": "Disable RRF hybrid search, use pure semantic similarity (default: false)",
                        "default": false
                    },
                    "name_only": {
                        "type": "boolean",
                        "description": "Definition search: find by name only, skip semantic matching. Use for 'where is X defined?' queries. Much faster.",
                        "default": false
                    },
                    "note_weight": {
                        "type": "number",
                        "description": "Weight for note scores 0.0-1.0 (default: 1.0). Lower values make notes rank below code.",
                        "default": 1.0
                    },
                    "sources": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter which indexes to search. Use \"project\" for primary, reference names for others. Omit to search all."
                    }
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "cqs_stats".into(),
            description: "Get index statistics: chunk counts, languages, last update time.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "cqs_callers".into(),
            description: "Find functions that call a given function name. Useful for impact analysis and understanding code dependencies.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the function to find callers for"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "cqs_callees".into(),
            description: "Find functions called by a given function. Useful for understanding what a function depends on.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the function to find callees for"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "cqs_read".into(),
            description: "Read a file with relevant context (notes, observations) injected as comments. Use instead of raw file read to get contextual awareness.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to file (relative to project root)"
                    }
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "cqs_add_note".into(),
            description: "Add a note to project memory. Use for surprises - things that broke unexpectedly (negative sentiment) or patterns that worked well (positive sentiment). Notes are indexed and surface in future searches.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The note content - what happened, why it matters"
                    },
                    "sentiment": {
                        "type": "number",
                        "description": "-1.0 (pain/failure) to +1.0 (gain/success). Default 0.0 (neutral observation)",
                        "default": 0.0
                    },
                    "mentions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Code paths, files, or concepts this note relates to"
                    }
                },
                "required": ["text"]
            }),
        },
        Tool {
            name: "cqs_update_note".into(),
            description: "Update an existing note in project memory. Find by exact text match, then replace text, sentiment, or mentions.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Exact text of the note to update (used to find it)"
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text (optional — omit to keep current)"
                    },
                    "new_sentiment": {
                        "type": "number",
                        "description": "Replacement sentiment -1.0 to +1.0 (optional — omit to keep current)"
                    },
                    "new_mentions": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Replacement mentions (optional — omit to keep current)"
                    }
                },
                "required": ["text"]
            }),
        },
        Tool {
            name: "cqs_remove_note".into(),
            description: "Remove a note from project memory. Find by exact text match and delete from notes.toml.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Exact text of the note to remove"
                    }
                },
                "required": ["text"]
            }),
        },
        Tool {
            name: "cqs_audit_mode".into(),
            description: "Toggle audit mode to exclude notes from search and read results. Use before code audits or fresh-eyes reviews to prevent prior observations from influencing analysis.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "enabled": {
                        "type": "boolean",
                        "description": "Enable or disable audit mode. Omit to query current state."
                    },
                    "expires_in": {
                        "type": "string",
                        "description": "Duration until auto-expire (e.g., '30m', '1h'). Default: 30m",
                        "default": "30m"
                    }
                }
            }),
        },
    ];

    Ok(serde_json::to_value(ToolsListResult { tools })?)
}

/// Handle tools/call request - dispatch to appropriate tool handler
pub fn handle_tools_call(server: &McpServer, params: Option<Value>) -> Result<Value> {
    let params = params.ok_or_else(|| anyhow::anyhow!("Missing params"))?;

    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(Default::default()));

    let start = std::time::Instant::now();
    tracing::debug!(tool = name, "MCP tool call started");

    let result = match name {
        "cqs_search" => search::tool_search(server, arguments),
        "cqs_stats" => stats::tool_stats(server),
        "cqs_callers" => call_graph::tool_callers(server, arguments),
        "cqs_callees" => call_graph::tool_callees(server, arguments),
        "cqs_read" => read::tool_read(server, arguments),
        "cqs_add_note" => notes::tool_add_note(server, arguments),
        "cqs_update_note" => notes::tool_update_note(server, arguments),
        "cqs_remove_note" => notes::tool_remove_note(server, arguments),
        "cqs_audit_mode" => audit::tool_audit_mode(server, arguments),
        _ => bail!(
            "Unknown tool: '{}'. Available tools: cqs_search, cqs_stats, cqs_callers, cqs_callees, cqs_read, cqs_add_note, cqs_update_note, cqs_remove_note, cqs_audit_mode",
            name
        ),
    };

    let elapsed = start.elapsed();
    tracing::info!(
        tool = name,
        elapsed_ms = elapsed.as_millis() as u64,
        "MCP tool call completed"
    );
    result
}
