//! MCP (Model Context Protocol) server implementation

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context, Result};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::convert::Infallible;
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::embedder::Embedder;
use crate::parser::Language;
use crate::store::{SearchFilter, Store};

/// JSON-RPC request
#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// JSON-RPC response
#[derive(Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// JSON-RPC error
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
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
#[allow(dead_code)]
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
    name_boost: Option<f32>,
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

        let store = Store::open(&index_path).context("Failed to open index")?;

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
        let _params: InitializeParams =
            params
                .map(serde_json::from_value)
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
            protocol_version: MCP_PROTOCOL_VERSION.into(),
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
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
                        },
                        "name_boost": {
                            "type": "number",
                            "description": "Weight for name matching 0.0-1.0 (default: 0.2)",
                            "default": 0.2
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

        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        match name {
            "cqs_search" => self.tool_search(arguments),
            "cqs_stats" => self.tool_stats(),
            _ => bail!("Unknown tool: {}", name),
        }
    }

    fn tool_search(&mut self, arguments: Value) -> Result<Value> {
        let args: SearchArgs = serde_json::from_value(arguments)?;

        // Validate query length (prevent excessive embedding computation)
        const MAX_QUERY_LENGTH: usize = 8192;
        if args.query.len() > MAX_QUERY_LENGTH {
            bail!(
                "Query too long: {} bytes (max {})",
                args.query.len(),
                MAX_QUERY_LENGTH
            );
        }

        let embedder = self.ensure_embedder()?;
        let query_embedding = embedder.embed_query(&args.query)?;

        let filter = SearchFilter {
            languages: args
                .language
                .map(|l| vec![l.parse().unwrap_or(Language::Rust)]),
            path_pattern: args.path_pattern,
            name_boost: args.name_boost.unwrap_or(0.2),
            query_text: args.query.clone(),
        };

        let limit = args.limit.unwrap_or(5).min(20);
        let threshold = args.threshold.unwrap_or(0.3);

        let results = self
            .store
            .search_filtered(&query_embedding, &filter, limit, threshold)?;

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

        let warning = if stats.total_chunks > 50_000 {
            Some(format!(
                "{} chunks. Search uses brute-force O(n). Consider splitting projects.",
                stats.total_chunks
            ))
        } else {
            None
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
            "index_path": self.project_root.join(".cq/index.db").to_string_lossy(),
            "model": stats.model_name,
            "last_indexed": stats.updated_at,
            "schema_version": stats.schema_version,
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
        if response.id.is_none()
            && response
                .result
                .as_ref()
                .map(|v| v.is_null())
                .unwrap_or(false)
        {
            continue;
        }

        let response_json = serde_json::to_string(&response)?;
        writeln!(stdout, "{}", response_json)?;
        stdout.flush()?;
    }

    Ok(())
}

// === HTTP Transport (Streamable HTTP per MCP spec 2025-11-25) ===

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Shared state for HTTP server
struct HttpState {
    server: RwLock<McpServer>,
}

/// Run the MCP server with HTTP transport
pub fn serve_http(project_root: PathBuf, port: u16) -> Result<()> {
    let server = McpServer::new(project_root)?;
    let state = Arc::new(HttpState {
        server: RwLock::new(server),
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Rate limiting: 1MB body limit, reasonable for MCP JSON-RPC
    let middleware = ServiceBuilder::new()
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(cors);

    let app = Router::new()
        .route("/mcp", post(handle_mcp_post).get(handle_mcp_sse))
        .route("/health", get(handle_health))
        .layer(middleware)
        .with_state(state);

    let addr = format!("127.0.0.1:{}", port);
    eprintln!("MCP HTTP server listening on http://{}", addr);
    eprintln!("MCP Protocol Version: {}", MCP_PROTOCOL_VERSION);

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        let shutdown = async {
            tokio::signal::ctrl_c().await.ok();
            eprintln!("\nShutting down HTTP server...");
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}

/// Handle POST /mcp - JSON-RPC requests (MCP 2025-11-25 compliant)
async fn handle_mcp_post(
    State(state): State<Arc<HttpState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    // Validate Origin header to prevent DNS rebinding attacks (MCP 2025-11-25 security requirement)
    if let Some(origin) = headers.get("origin") {
        let origin_str = origin.to_str().unwrap_or("");
        // Allow localhost origins only
        if !origin_str.is_empty()
            && !origin_str.starts_with("http://localhost")
            && !origin_str.starts_with("http://127.0.0.1")
            && !origin_str.starts_with("https://localhost")
            && !origin_str.starts_with("https://127.0.0.1")
        {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32600,
                        "message": "Invalid origin"
                    }
                })),
            );
        }
    }

    // Check MCP-Protocol-Version header (optional, default to 2025-03-26 per spec)
    if let Some(version) = headers.get("mcp-protocol-version") {
        let version_str = version.to_str().unwrap_or("");
        if !version_str.is_empty()
            && version_str != MCP_PROTOCOL_VERSION
            && version_str != "2025-03-26"
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32600,
                        "message": format!("Unsupported protocol version: {}. Supported: {}", version_str, MCP_PROTOCOL_VERSION)
                    }
                })),
            );
        }
    }

    let response = {
        let mut server = match state.server.write() {
            Ok(s) => s,
            Err(poisoned) => {
                tracing::warn!("Server lock poisoned, recovering");
                poisoned.into_inner()
            }
        };
        server.handle_request(request)
    };

    // Return 202 Accepted for notifications (no response needed)
    if response.id.is_none()
        && response
            .result
            .as_ref()
            .map(|v| v.is_null())
            .unwrap_or(false)
    {
        return (StatusCode::ACCEPTED, Json(serde_json::json!(null)));
    }

    (
        StatusCode::OK,
        Json(serde_json::to_value(&response).unwrap_or_default()),
    )
}

/// Handle GET /health
async fn handle_health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "cqs",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

/// Handle GET /mcp - SSE stream for server-to-client messages (MCP 2025-11-25)
async fn handle_mcp_sse(
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    // Validate Accept header includes text/event-stream
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !accept.contains("text/event-stream") {
        return Err((
            StatusCode::NOT_ACCEPTABLE,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {
                    "code": -32600,
                    "message": "Accept header must include text/event-stream"
                }
            })),
        ));
    }

    // Create SSE stream with priming event per MCP 2025-11-25 spec:
    // "The server SHOULD immediately send an SSE event consisting of an event
    // ID and an empty data field in order to prime the client to reconnect"
    let event_id = uuid_simple();

    let stream = stream::once(async move {
        // Send priming event with ID and empty data
        Ok(Event::default().id(event_id).data(""))
    });

    // Note: For a full implementation, this stream would be kept alive and
    // server-initiated messages (notifications, requests) would be pushed here.
    // Since cqs doesn't have server-initiated messages yet, we just send the
    // priming event and keep the connection alive.

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Generate a simple unique ID for SSE events
fn uuid_simple() -> String {
    use rand::Rng;
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let random: u32 = rand::thread_rng().gen();
    format!("{:x}-{:08x}", nanos, random)
}
