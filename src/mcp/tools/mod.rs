//! MCP tool handlers
//!
//! Each tool provides a specific capability to MCP clients.

mod audit;
mod batch;
mod call_graph;
mod context;
mod dead;
mod diff;
mod explain;
mod gc;
mod impact;
mod notes;
mod read;
pub(crate) mod resolve;
mod search;
mod similar;
mod stats;
mod test_map;
mod trace;

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
                    "chunk_type": {
                        "type": "string",
                        "enum": ["function", "method", "class", "struct", "enum", "trait", "interface", "constant"],
                        "description": "Filter by code element type (optional). Can specify multiple comma-separated values."
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
                    },
                    "focus": {
                        "type": "string",
                        "description": "Function name or file:function. Returns only the target function + its type dependencies instead of the whole file. Cuts tokens by 50-80% in large files."
                    }
                }
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
            name: "cqs_diff".into(),
            description: "Semantic diff between indexed snapshots. Compare project vs a reference, or two references.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Reference name to compare from"
                    },
                    "target": {
                        "type": "string",
                        "description": "Reference name or 'project' (default: project)"
                    },
                    "threshold": {
                        "type": "number",
                        "description": "Similarity threshold for 'modified' (default: 0.95)",
                        "default": 0.95
                    },
                    "language": {
                        "type": "string",
                        "enum": ["rust", "python", "typescript", "javascript", "go", "c", "java"],
                        "description": "Filter by language (optional)"
                    }
                },
                "required": ["source"]
            }),
        },
        Tool {
            name: "cqs_explain".into(),
            description: "Generate a function card: signature, docs, callers, callees, similar functions.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Function name or file:function"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "cqs_similar".into(),
            description: "Find code similar to a given function. Search by example.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Function name or file:function (e.g., 'search_filtered' or 'src/search.rs:search_filtered')"
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
                        "enum": ["rust", "python", "typescript", "javascript", "go", "c", "java"],
                        "description": "Filter by language (optional)"
                    }
                },
                "required": ["target"]
            }),
        },
        Tool {
            name: "cqs_impact".into(),
            description: "Impact analysis: what breaks if you change a function. Returns callers with usage context and tests that reference the function.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Function name or file:function to analyze"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Caller depth (1=direct only, 2+=transitive). Default: 1",
                        "default": 1
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "cqs_trace".into(),
            description: "Follow a call chain between two functions. Returns the shortest path through the call graph.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Source function name or file:function"
                    },
                    "target": {
                        "type": "string",
                        "description": "Target function name or file:function"
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum search depth (default: 10)",
                        "default": 10
                    }
                },
                "required": ["source", "target"]
            }),
        },
        Tool {
            name: "cqs_test_map".into(),
            description: "Map functions to tests that exercise them. Find what tests cover a given function.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Function name or file:function to find tests for"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Max call chain depth to search (default: 5)",
                        "default": 5
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "cqs_batch".into(),
            description: "Execute multiple queries in one tool call. Eliminates round-trip overhead for independent lookups.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": {"type": "string", "enum": ["search", "callers", "callees", "explain", "similar", "stats"], "description": "Which tool to invoke"},
                                "arguments": {"type": "object", "description": "Arguments for the tool"}
                            },
                            "required": ["tool"]
                        },
                        "maxItems": 10,
                        "description": "Array of queries to execute"
                    }
                },
                "required": ["queries"]
            }),
        },
        Tool {
            name: "cqs_context".into(),
            description: "What do I need to know to work on this file? Returns all chunks (signatures), external callers, external callees, dependent files, and related notes.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path relative to project root (e.g., 'src/search.rs')"}
                },
                "required": ["path"]
            }),
        },
        Tool {
            name: "cqs_dead".into(),
            description: "Find functions with no callers (dead code detection). Returns confident dead code and optionally public API functions that may be unused.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "include_pub": {
                        "type": "boolean",
                        "description": "Include public API functions in the main dead code list (default: false, shown separately)",
                        "default": false
                    }
                }
            }),
        },
        Tool {
            name: "cqs_gc".into(),
            description: "Check index staleness and report what needs cleanup. Returns stale/missing file counts. Run 'cqs gc' from CLI to actually clean up.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
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
        "cqs_dead" => dead::tool_dead(server, arguments),
        "cqs_gc" => gc::tool_gc(server),
        "cqs_audit_mode" => audit::tool_audit_mode(server, arguments),
        "cqs_diff" => diff::tool_diff(server, arguments),
        "cqs_explain" => explain::tool_explain(server, arguments),
        "cqs_similar" => similar::tool_similar(server, arguments),
        "cqs_impact" => impact::tool_impact(server, arguments),
        "cqs_trace" => trace::tool_trace(server, arguments),
        "cqs_test_map" => test_map::tool_test_map(server, arguments),
        "cqs_batch" => batch::tool_batch(server, arguments),
        "cqs_context" => context::tool_context(server, arguments),
        _ => bail!(
            "Unknown tool: '{}'. Available tools: cqs_search, cqs_stats, cqs_callers, cqs_callees, cqs_read, cqs_add_note, cqs_update_note, cqs_remove_note, cqs_dead, cqs_gc, cqs_audit_mode, cqs_diff, cqs_explain, cqs_similar, cqs_impact, cqs_trace, cqs_test_map, cqs_batch, cqs_context",
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
