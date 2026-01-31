//! MCP (Model Context Protocol) server implementation

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::embedder::Embedder;
use crate::parser::Language;
use crate::store::{SearchFilter, Store};

/// JSON-RPC request
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

/// JSON-RPC response
#[derive(Serialize)]
pub struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

/// JSON-RPC error
#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

// MCP types

#[derive(Deserialize)]
#[allow(dead_code)]
struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: Value,
    #[serde(rename = "clientInfo")]
    client_info: ClientInfo,
}

#[derive(Deserialize)]
struct ClientInfo {
    name: String,
    version: String,
}

#[derive(Serialize)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
}

#[derive(Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Serialize)]
struct ToolsListResult {
    tools: Vec<Tool>,
}

/// Search tool arguments
#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
    threshold: Option<f32>,
    language: Option<String>,
    path_pattern: Option<String>,
}

/// MCP Server
pub struct McpServer {
    store: Store,
    embedder: Option<Embedder>,
    project_root: PathBuf,
}

impl McpServer {
    /// Create a new MCP server for the given project
    pub fn new(project_root: PathBuf) -> Result<Self> {
        let index_path = project_root.join(".cq/index.db");

        if !index_path.exists() {
            bail!("Index not found. Run 'cq init && cq index' first.");
        }

        let store = Store::open(&index_path)
            .context("Failed to open index")?;

        Ok(Self {
            store,
            embedder: None,
            project_root,
        })
    }

    /// Ensure embedder is loaded (lazy initialization)
    fn ensure_embedder(&mut self) -> Result<&mut Embedder> {
        if self.embedder.is_none() {
            self.embedder = Some(Embedder::new()?);
        }
        Ok(self.embedder.as_mut().unwrap())
    }

    /// Handle a JSON-RPC request
    pub fn handle_request(&mut self, request: JsonRpcRequest) -> JsonRpcResponse {
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params),
            "initialized" => Ok(Value::Null), // Notification, no response needed
            "tools/list" => self.handle_tools_list(),
            "tools/call" => self.handle_tools_call(request.params),
            _ => Err(anyhow::anyhow!("Unknown method: {}", request.method)),
        };

        match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: Some(value),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e.to_string(),
                    data: None,
                }),
            },
        }
    }

    fn handle_initialize(&self, params: Option<Value>) -> Result<Value> {
        let _params: InitializeParams = params
            .map(|p| serde_json::from_value(p))
            .transpose()?
            .unwrap_or(InitializeParams {
                protocol_version: "2024-11-05".into(),
                capabilities: Value::Object(Default::default()),
                client_info: ClientInfo {
                    name: "unknown".into(),
                    version: "0.0.0".into(),
                },
            });

        let result = InitializeResult {
            protocol_version: "2024-11-05".into(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability { list_changed: false },
            },
            server_info: ServerInfo {
                name: "cqs".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_tools_list(&self) -> Result<Value> {
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
                            "enum": ["rust", "python", "typescript", "javascript", "go"],
                            "description": "Filter by language (optional)"
                        },
                        "path_pattern": {
                            "type": "string",
                            "description": "Glob pattern to filter paths (e.g., 'src/api/**')"
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
        ];

        Ok(serde_json::to_value(ToolsListResult { tools })?)
    }

    fn handle_tools_call(&mut self, params: Option<Value>) -> Result<Value> {
        let params = params.ok_or_else(|| anyhow::anyhow!("Missing params"))?;

        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing tool name"))?;

        let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));

        match name {
            "cqs_search" => self.tool_search(arguments),
            "cqs_stats" => self.tool_stats(),
            _ => bail!("Unknown tool: {}", name),
        }
    }

    fn tool_search(&mut self, arguments: Value) -> Result<Value> {
        let args: SearchArgs = serde_json::from_value(arguments)?;

        let embedder = self.ensure_embedder()?;
        let query_embedding = embedder.embed_query(&args.query)?;

        let filter = SearchFilter {
            languages: args.language.map(|l| {
                vec![l.parse().unwrap_or(Language::Rust)]
            }),
            path_pattern: args.path_pattern,
        };

        let limit = args.limit.unwrap_or(5).min(20);
        let threshold = args.threshold.unwrap_or(0.3);

        let results = self.store.search_filtered(&query_embedding, &filter, limit, threshold)?;

        let json_results: Vec<_> = results
            .iter()
            .map(|r| {
                // Paths are stored relative; strip_prefix handles legacy absolute paths
                serde_json::json!({
                    "file": r.chunk.file.strip_prefix(&self.project_root)
                        .unwrap_or(&r.chunk.file)
                        .to_string_lossy(),
                    "line_start": r.chunk.line_start,
                    "line_end": r.chunk.line_end,
                    "name": r.chunk.name,
                    "signature": r.chunk.signature,
                    "language": r.chunk.language.to_string(),
                    "chunk_type": r.chunk.chunk_type.to_string(),
                    "score": r.score,
                    "content": r.chunk.content,
                })
            })
            .collect();

        let result = serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": results.len(),
        });

        // MCP tools/call requires content array format
        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    }

    fn tool_stats(&self) -> Result<Value> {
        let stats = self.store.stats()?;

        let result = serde_json::json!({
            "total_chunks": stats.total_chunks,
            "total_files": stats.total_files,
            "by_language": stats.chunks_by_language.iter()
                .map(|(l, c)| (l.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "by_type": stats.chunks_by_type.iter()
                .map(|(t, c)| (t.to_string(), c))
                .collect::<std::collections::HashMap<_, _>>(),
            "index_path": self.project_root.join(".cq/index.db").to_string_lossy(),
            "model": stats.model_name,
            "last_indexed": stats.updated_at,
            "schema_version": stats.schema_version,
        });

        // MCP tools/call requires content array format
        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    }
}

/// Run the MCP server with stdio transport
pub fn serve_stdio(project_root: PathBuf) -> Result<()> {
    let mut server = McpServer::new(project_root)?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let error_response = JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                        data: None,
                    }),
                };
                let response_json = serde_json::to_string(&error_response)?;
                writeln!(stdout, "{}", response_json)?;
                stdout.flush()?;
                continue;
            }
        };

        let response = server.handle_request(request);

        // Skip response for notifications (no id)
        if response.id.is_none() && response.result.as_ref().map(|v| v.is_null()).unwrap_or(false) {
            continue;
        }

        let response_json = serde_json::to_string(&response)?;
        writeln!(stdout, "{}", response_json)?;
        stdout.flush()?;
    }

    Ok(())
}
