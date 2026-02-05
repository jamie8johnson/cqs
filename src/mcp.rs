//! MCP (Model Context Protocol) server implementation
//!
//! # Security
//!
//! JSON deserialization from untrusted input is bounded by:
//! - HTTP transport: 1MB request body limit (RequestBodyLimitLayer)
//! - Stdio transport: trusted client (Claude Code) with reasonable message sizes

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;

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
use crate::hnsw::HnswIndex;
use crate::index::VectorIndex;
use crate::note::parse_notes;
use crate::parser::Language;
use crate::store::{SearchFilter, Store, UnifiedResult};

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
    semantic_only: Option<bool>,
    /// Definition search mode - find by name only, no semantic matching.
    /// Use for "where is X defined?" queries. Much faster than semantic search.
    name_only: Option<bool>,
}

/// Audit mode arguments
#[derive(Deserialize)]
struct AuditModeArgs {
    enabled: Option<bool>,
    expires_in: Option<String>,
}

/// Audit mode state - excludes notes from search/read during audits
#[derive(Default)]
struct AuditMode {
    enabled: bool,
    expires_at: Option<DateTime<Utc>>,
}

impl AuditMode {
    /// Check if audit mode is currently active (enabled and not expired)
    fn is_active(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.expires_at {
            Some(expires) => Utc::now() < expires,
            None => true,
        }
    }

    /// Get remaining time as human-readable string, or None if expired/disabled
    fn remaining(&self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        let expires = self.expires_at?;
        let remaining = expires - Utc::now();
        let minutes = remaining.num_minutes();
        if minutes <= 0 {
            None
        } else if minutes < 60 {
            Some(format!("{}m", minutes))
        } else {
            Some(format!("{}h {}m", minutes / 60, minutes % 60))
        }
    }

    /// Format status line for inclusion in responses
    fn status_line(&self) -> Option<String> {
        let remaining = self.remaining()?;
        Some(format!(
            "(audit mode: notes excluded, {} remaining)",
            remaining
        ))
    }
}

/// MCP Server
///
/// Uses interior mutability (OnceLock, Mutex) to allow concurrent read access
/// via RwLock read locks. This enables parallel request handling.
pub struct McpServer {
    store: Store,
    /// Lazily initialized embedder (thread-safe via OnceLock)
    embedder: OnceLock<Embedder>,
    project_root: PathBuf,
    /// Vector index for O(log n) search (CAGRA or HNSW)
    /// Wrapped in Arc<RwLock> to allow background CAGRA upgrade
    index: Arc<RwLock<Option<Box<dyn VectorIndex>>>>,
    /// Use GPU for query embedding
    use_gpu: bool,
    /// Audit mode state (interior mutability for concurrent access)
    audit_mode: Mutex<AuditMode>,
}

impl McpServer {
    /// Create a new MCP server for the given project
    ///
    /// Loads HNSW index immediately for fast startup, then spawns background
    /// thread to build CAGRA GPU index. Queries use HNSW until CAGRA is ready.
    pub fn new(project_root: impl AsRef<Path>, use_gpu: bool) -> Result<Self> {
        let project_root = project_root.as_ref();
        let index_path = project_root.join(".cq/index.db");
        let cq_dir = project_root.join(".cq");

        if !index_path.exists() {
            bail!("Index not found. Run 'cq init && cq index' first.");
        }

        let store = Store::open(&index_path).context("Failed to open index")?;

        // Load HNSW first (fast) - wrap in Arc<RwLock> for background upgrade
        let hnsw = Self::load_hnsw_index(&cq_dir);
        let index = Arc::new(RwLock::new(hnsw));

        // Spawn background CAGRA build if GPU available.
        // Thread is intentionally detached - it holds only an Arc reference and will
        // complete gracefully when the main process exits. Joining on Drop would block
        // shutdown unnecessarily for potentially long GPU operations.
        #[cfg(feature = "gpu-search")]
        if crate::cagra::CagraIndex::gpu_available() {
            let index_clone = Arc::clone(&index);
            let index_path_clone = index_path.clone();
            std::thread::spawn(move || {
                Self::build_cagra_background(index_clone, &index_path_clone);
            });
        }

        Ok(Self {
            store,
            embedder: OnceLock::new(),
            project_root: project_root.to_path_buf(),
            index,
            use_gpu,
            audit_mode: Mutex::new(AuditMode::default()),
        })
    }

    /// Build CAGRA index in background and swap it in when ready
    #[cfg(feature = "gpu-search")]
    fn build_cagra_background(
        index: Arc<RwLock<Option<Box<dyn VectorIndex>>>>,
        index_path: &std::path::Path,
    ) {
        tracing::info!("MCP: Building CAGRA GPU index in background...");

        // Open a separate store connection for the background thread
        let store = match Store::open(index_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("MCP: Failed to open store for CAGRA build: {}", e);
                return;
            }
        };

        match crate::cagra::CagraIndex::build_from_store(&store) {
            Ok(cagra) => {
                let len = cagra.len();
                let mut guard = index.write().unwrap_or_else(|e| e.into_inner());
                *guard = Some(Box::new(cagra) as Box<dyn VectorIndex>);
                tracing::info!("MCP: Upgraded to CAGRA GPU index ({} vectors)", len);
            }
            Err(e) => {
                tracing::warn!("MCP: CAGRA build failed, keeping HNSW: {}", e);
            }
        }
    }

    /// Load HNSW index if available
    fn load_hnsw_index(cq_dir: &std::path::Path) -> Option<Box<dyn VectorIndex>> {
        if HnswIndex::exists(cq_dir, "index") {
            match HnswIndex::load(cq_dir, "index") {
                Ok(index) => {
                    tracing::info!("MCP: Loaded HNSW index ({} vectors)", index.len());
                    Some(Box::new(index))
                }
                Err(e) => {
                    tracing::warn!("MCP: Failed to load HNSW index: {}", e);
                    None
                }
            }
        } else {
            None
        }
    }

    /// Ensure embedder is loaded (lazy initialization)
    ///
    /// Uses OnceLock for thread-safe lazy initialization without requiring &mut self.
    /// This allows concurrent read access via RwLock read locks.
    fn ensure_embedder(&self) -> Result<&Embedder> {
        // Fast path: already initialized
        if let Some(embedder) = self.embedder.get() {
            return Ok(embedder);
        }

        // Slow path: initialize
        let new_embedder = if self.use_gpu {
            Embedder::new()?
        } else {
            Embedder::new_cpu()?
        };

        // Try to set (another thread might have raced us, that's OK)
        let _ = self.embedder.set(new_embedder);

        // Return reference (definitely initialized now, either by us or another thread)
        Ok(self.embedder.get().expect("embedder just initialized"))
    }

    /// Handle a JSON-RPC request
    ///
    /// Takes &self (not &mut self) to allow concurrent request handling via read locks.
    pub fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
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
        // SAFETY: Allocation bounded by 1MB request body limit (HTTP) or trusted client (stdio)
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
                            // Keep in sync with crate::parser::Language variants
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

    fn handle_tools_call(&self, params: Option<Value>) -> Result<Value> {
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
            "cqs_search" => self.tool_search(arguments),
            "cqs_stats" => self.tool_stats(),
            "cqs_callers" => self.tool_callers(arguments),
            "cqs_callees" => self.tool_callees(arguments),
            "cqs_read" => self.tool_read(arguments),
            "cqs_add_note" => self.tool_add_note(arguments),
            "cqs_audit_mode" => self.tool_audit_mode(arguments),
            _ => bail!(
                "Unknown tool: '{}'. Available tools: cqs_search, cqs_stats, cqs_callers, cqs_callees, cqs_read, cqs_add_note, cqs_audit_mode",
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

    fn tool_search(&self, arguments: Value) -> Result<Value> {
        // SAFETY: Allocation bounded by 1MB request body limit (HTTP) or trusted client (stdio)
        let args: SearchArgs = serde_json::from_value(arguments)?;
        validate_query_length(&args.query)?;

        let limit = args.limit.unwrap_or(5).min(20);
        let threshold = args.threshold.unwrap_or(0.3);

        // Definition search mode - find by name only, skip embedding
        if args.name_only.unwrap_or(false) {
            let results = self.store.search_by_name(&args.query, limit)?;
            let json_results: Vec<_> = results
                .iter()
                .filter(|r| r.score >= threshold)
                .map(|r| {
                    serde_json::json!({
                        "type": "code",
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

            return Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&json_results)?
                }]
            }));
        }

        // Semantic search mode (default)
        let embedder = self.ensure_embedder()?;
        let query_embedding = embedder.embed_query(&args.query)?;

        let filter = SearchFilter {
            languages: args
                .language
                .map(|l| vec![l.parse().unwrap_or(Language::Rust)]),
            path_pattern: args.path_pattern,
            name_boost: args.name_boost.unwrap_or(0.2),
            query_text: args.query.clone(),
            enable_rrf: !args.semantic_only.unwrap_or(false), // RRF on by default, disable with semantic_only
        };

        // Read-lock the index (allows background CAGRA build to upgrade it)
        let index_guard = self.index.read().unwrap_or_else(|e| e.into_inner());

        // Check audit mode - if active, use code-only search
        let audit_active = self
            .audit_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_active();
        let results: Vec<UnifiedResult> = if audit_active {
            // Code-only search when audit mode is active
            let code_results = self.store.search_filtered_with_index(
                &query_embedding,
                &filter,
                limit,
                threshold,
                index_guard.as_deref(),
            )?;
            code_results.into_iter().map(UnifiedResult::Code).collect()
        } else {
            // Unified search including notes
            self.store.search_unified_with_index(
                &query_embedding,
                &filter,
                limit,
                threshold,
                index_guard.as_deref(),
            )?
        };

        let json_results: Vec<_> = results
            .iter()
            .map(|r| match r {
                UnifiedResult::Code(r) => {
                    serde_json::json!({
                        "type": "code",
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
                }
                UnifiedResult::Note(r) => {
                    serde_json::json!({
                        "type": "note",
                        "text": r.note.text,
                        "sentiment": r.note.sentiment,
                        "mentions": r.note.mentions,
                        "score": r.score,
                    })
                }
            })
            .collect();

        let mut result = serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": results.len(),
        });

        // Add audit mode status if active
        if let Some(status) = self
            .audit_mode
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status_line()
        {
            result["audit_mode"] = serde_json::json!(status);
        }

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

        let warning = if stats.total_chunks > 100_000 {
            Some(format!(
                "{} chunks is very large. Consider using --path to limit search scope.",
                stats.total_chunks
            ))
        } else {
            None
        };

        // Check HNSW index status
        let cq_dir = self.project_root.join(".cq");
        let hnsw_status = if HnswIndex::exists(&cq_dir, "index") {
            match HnswIndex::load(&cq_dir, "index") {
                Ok(hnsw) => format!("{} vectors (O(log n) search)", hnsw.len()),
                Err(_) => "exists but failed to load".to_string(),
            }
        } else {
            "not built".to_string()
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
            "hnsw_index": hnsw_status,
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

    fn tool_callers(&self, arguments: Value) -> Result<Value> {
        let name = arguments
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;

        // Use full call graph (includes large functions)
        let callers = self.store.get_callers_full(name)?;

        let result = if callers.is_empty() {
            serde_json::json!({
                "callers": [],
                "message": format!("No callers found for '{}'", name)
            })
        } else {
            serde_json::json!({
                "callers": callers.iter().map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "file": c.file.to_string_lossy(),
                        "line": c.line,
                    })
                }).collect::<Vec<_>>(),
                "count": callers.len(),
            })
        };

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    }

    fn tool_callees(&self, arguments: Value) -> Result<Value> {
        let name = arguments
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'name' argument"))?;

        // Use full call graph (includes large functions)
        let callees = self.store.get_callees_full(name)?;

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

    fn tool_read(&self, arguments: Value) -> Result<Value> {
        let path = arguments
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' argument"))?;

        let file_path = self.project_root.join(path);
        if !file_path.exists() {
            bail!("File not found: {}", path);
        }

        // Path traversal protection
        let canonical = file_path
            .canonicalize()
            .context("Failed to canonicalize path")?;
        let project_canonical = self
            .project_root
            .canonicalize()
            .context("Failed to canonicalize project root")?;
        if !canonical.starts_with(&project_canonical) {
            bail!("Path traversal not allowed: {}", path);
        }

        // Read file content
        let content = std::fs::read_to_string(&file_path)
            .context(format!("Failed to read file: {}", path))?;

        // Check audit mode - if active, skip note injection
        let audit_guard = self.audit_mode.lock().unwrap_or_else(|e| e.into_inner());
        let audit_active = audit_guard.is_active();
        let mut context_header = String::new();

        // Add audit mode status line if active
        if let Some(status) = audit_guard.status_line() {
            context_header.push_str(&format!("// {}\n//\n", status));
        }
        drop(audit_guard); // Release lock before file I/O

        // Find relevant notes by searching for this file path (skip if audit mode active)
        if !audit_active {
            let notes_path = self.project_root.join("docs/notes.toml");

            if notes_path.exists() {
                if let Ok(notes) = parse_notes(&notes_path) {
                    // Find notes that mention this file
                    let file_name = file_path.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    let relevant: Vec<_> = notes
                        .iter()
                        .filter(|n| {
                            n.mentions
                                .iter()
                                .any(|m| m == file_name || m == path || path.contains(m))
                        })
                        .collect();

                    if !relevant.is_empty() {
                        context_header.push_str(
                            "// ┌─────────────────────────────────────────────────────────────┐\n",
                        );
                        context_header.push_str(
                            "// │ [cqs] Context from notes.toml                              │\n",
                        );
                        context_header.push_str(
                            "// └─────────────────────────────────────────────────────────────┘\n",
                        );

                        for n in relevant {
                            let sentiment_label = if n.sentiment() < -0.3 {
                                "WARNING"
                            } else if n.sentiment() > 0.3 {
                                "PATTERN"
                            } else {
                                "NOTE"
                            };
                            // First line of text only
                            if let Some(first_line) = n.text.lines().next() {
                                context_header.push_str(&format!(
                                    "// [{}] {}\n",
                                    sentiment_label,
                                    first_line.trim()
                                ));
                            }
                        }
                        context_header.push_str("//\n");
                    }
                }
            }
        }

        let enriched_content = if context_header.is_empty() {
            content
        } else {
            format!("{}{}", context_header, content)
        };

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": enriched_content
            }]
        }))
    }

    fn tool_add_note(&self, arguments: Value) -> Result<Value> {
        let text = arguments
            .get("text")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'text' argument"))?;

        // Validate text length
        if text.is_empty() {
            bail!("Note text cannot be empty");
        }
        if text.len() > 2000 {
            bail!("Note text too long: {} bytes (max 2000)", text.len());
        }

        let sentiment: f32 = arguments
            .get("sentiment")
            .and_then(|s| s.as_f64())
            .map(|s| (s as f32).clamp(-1.0, 1.0))
            .unwrap_or(0.0);

        let mentions: Vec<String> = arguments
            .get("mentions")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Build TOML entry - escape all strings properly
        let mentions_toml = if mentions.is_empty() {
            String::new()
        } else {
            format!(
                "\nmentions = [{}]",
                mentions
                    .iter()
                    .map(|m| {
                        format!(
                            "\"{}\"",
                            m.replace('\\', "\\\\")
                                .replace('\"', "\\\"")
                                .replace('\n', "\\n")
                                .replace('\r', "\\r")
                                .replace('\t', "\\t")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        // Escape text for TOML - use single-line strings with escape sequences
        // (avoids triple-quote edge cases)
        let text_toml = format!(
            "\"{}\"",
            text.replace('\\', "\\\\")
                .replace('\"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        );

        let entry = format!(
            "\n[[note]]\nsentiment = {:.1}\ntext = {}{}\n",
            sentiment, text_toml, mentions_toml
        );

        // Append to notes.toml
        let notes_path = self.project_root.join("docs/notes.toml");

        // Create docs dir if needed
        if let Some(parent) = notes_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create docs directory")?;
        }

        // Create file with header if it doesn't exist
        if !notes_path.exists() {
            std::fs::write(
                &notes_path,
                "# Notes - unified memory for AI collaborators\n# sentiment: -1.0 (pain) to +1.0 (gain)\n",
            )
            .context("Failed to create notes.toml")?;
        }

        // Append entry
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&notes_path)
                .context("Failed to open notes.toml")?;
            file.write_all(entry.as_bytes())
                .context("Failed to write note")?;
            file.sync_all().context("Failed to sync note to disk")?;
        }

        // Re-parse and re-index all notes so the new one is immediately searchable
        let indexed = match parse_notes(&notes_path) {
            Ok(notes) if !notes.is_empty() => match self.index_notes(&notes, &notes_path) {
                Ok(count) => count,
                Err(e) => {
                    tracing::warn!("Failed to index notes: {}", e);
                    0
                }
            },
            Ok(_) => 0,
            Err(e) => {
                tracing::warn!("Failed to parse notes after adding: {}", e);
                0
            }
        };

        let sentiment_label = if sentiment < -0.3 {
            "warning"
        } else if sentiment > 0.3 {
            "pattern"
        } else {
            "observation"
        };

        let result = serde_json::json!({
            "status": "added",
            "type": sentiment_label,
            "sentiment": sentiment,
            "text_preview": text.char_indices().nth(100).map(|(i, _)| format!("{}...", &text[..i])).unwrap_or_else(|| text.to_string()),
            "file": "docs/notes.toml",
            "indexed": indexed > 0,
            "total_notes": indexed
        });

        Ok(serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string_pretty(&result)?
            }]
        }))
    }

    fn tool_audit_mode(&self, arguments: Value) -> Result<Value> {
        let args: AuditModeArgs = serde_json::from_value(arguments)?;
        let mut audit_mode = self.audit_mode.lock().unwrap_or_else(|e| e.into_inner());

        // If no enabled argument, just query current state
        if args.enabled.is_none() {
            let result = if audit_mode.is_active() {
                serde_json::json!({
                    "audit_mode": true,
                    "remaining": audit_mode.remaining(),
                    "expires_at": audit_mode.expires_at.map(|t| t.to_rfc3339()),
                })
            } else {
                serde_json::json!({
                    "audit_mode": false,
                })
            };

            return Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&result)?
                }]
            }));
        }

        let enabled = args.enabled.unwrap();

        if enabled {
            // Parse expires_in duration (default 30m)
            let expires_in = args.expires_in.as_deref().unwrap_or("30m");
            let duration = parse_duration(expires_in)?;
            let expires_at = Utc::now() + duration;

            audit_mode.enabled = true;
            audit_mode.expires_at = Some(expires_at);

            let result = serde_json::json!({
                "audit_mode": true,
                "message": "Audit mode enabled. Notes excluded from search and read.",
                "remaining": audit_mode.remaining(),
                "expires_at": expires_at.to_rfc3339(),
            });

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&result)?
                }]
            }))
        } else {
            audit_mode.enabled = false;
            audit_mode.expires_at = None;

            let result = serde_json::json!({
                "audit_mode": false,
                "message": "Audit mode disabled. Notes included in search and read.",
            });

            Ok(serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string_pretty(&result)?
                }]
            }))
        }
    }

    /// Index notes into the database (embed and store)
    fn index_notes(
        &self,
        notes: &[crate::note::Note],
        notes_path: &std::path::Path,
    ) -> Result<usize> {
        use crate::embedder::Embedding;

        let embedder = self.ensure_embedder()?;

        // Embed note content with sentiment prefix
        let texts: Vec<String> = notes.iter().map(|n| n.embedding_text()).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let base_embeddings = embedder.embed_documents(&text_refs)?;

        // Add sentiment as 769th dimension
        let embeddings_with_sentiment: Vec<Embedding> = base_embeddings
            .into_iter()
            .zip(notes.iter())
            .map(|(emb, note)| emb.with_sentiment(note.sentiment()))
            .collect();

        // Get file mtime
        let file_mtime = notes_path
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Delete old notes and insert new
        self.store.delete_notes_by_file(notes_path)?;
        let note_embeddings: Vec<_> = notes
            .iter()
            .cloned()
            .zip(embeddings_with_sentiment)
            .collect();
        self.store
            .upsert_notes_batch(&note_embeddings, notes_path, file_mtime)?;

        Ok(notes.len())
    }
}

/// Run the MCP server with stdio transport
///
/// Reads JSON-RPC requests from stdin and writes responses to stdout.
/// Used by Claude Code for direct integration.
///
/// # Arguments
/// * `project_root` - Root directory of the project to index
/// * `use_gpu` - Whether to use GPU acceleration for embeddings
pub fn serve_stdio(project_root: impl AsRef<Path>, use_gpu: bool) -> Result<()> {
    let server = McpServer::new(project_root, use_gpu)?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;

        if line.trim().is_empty() {
            continue;
        }

        // SAFETY: Stdio transport is from trusted client (Claude Code)
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
///
/// McpServer uses interior mutability (Arc<RwLock> for index, Mutex for audit_mode),
/// so no outer lock is needed. All McpServer methods take &self.
struct HttpState {
    server: McpServer,
    /// API key for authentication (None = no auth required)
    api_key: Option<String>,
}

/// Run the MCP server with HTTP transport
///
/// Listens for JSON-RPC requests on the specified address.
/// Supports CORS and optional API key authentication.
///
/// # Arguments
/// * `project_root` - Root directory of the project to index
/// * `bind` - Address to bind to (e.g., "127.0.0.1")
/// * `port` - Port to listen on (e.g., 3000)
/// * `use_gpu` - Whether to use GPU acceleration for embeddings
/// * `api_key` - Optional API key; if set, requests need `Authorization: Bearer <key>`
pub fn serve_http(
    project_root: impl AsRef<Path>,
    bind: &str,
    port: u16,
    use_gpu: bool,
    api_key: Option<String>,
) -> Result<()> {
    // Capture api_key presence before moving it into state
    let has_api_key = api_key.is_some();

    let server = McpServer::new(project_root, use_gpu)?;
    let state = Arc::new(HttpState { server, api_key });

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

    let addr = format!("{}:{}", bind, port);

    // Warn if binding to non-localhost
    let is_localhost = bind == "127.0.0.1" || bind == "localhost" || bind == "::1";
    if !is_localhost {
        if has_api_key {
            eprintln!("WARNING: Binding to {} with API key authentication.", bind);
        } else {
            eprintln!("WARNING: Binding to {} WITHOUT authentication!", bind);
        }
    }

    eprintln!("MCP HTTP server listening on http://{}", addr);
    eprintln!("MCP Protocol Version: {}", MCP_PROTOCOL_VERSION);
    if has_api_key {
        eprintln!("Authentication: API key required (Authorization: Bearer <key>)");
    }

    // Note: Creates separate runtime from Store's internal runtime.
    // Sharing would require exposing Store.rt or restructuring the API.
    // Two runtimes is acceptable - one for SQLx ops, one for HTTP serving.
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

// ============================================================================
// Validation helpers
// ============================================================================

/// Maximum query length to prevent excessive embedding computation
const MAX_QUERY_LENGTH: usize = 8192;

/// Validate query length to prevent excessive embedding computation.
fn validate_query_length(query: &str) -> Result<()> {
    if query.len() > MAX_QUERY_LENGTH {
        bail!(
            "Query too long: {} bytes (max {})",
            query.len(),
            MAX_QUERY_LENGTH
        );
    }
    Ok(())
}

/// Parse duration string like "30m", "1h", "2h30m" into chrono::Duration
fn parse_duration(s: &str) -> Result<chrono::Duration> {
    let s = s.trim().to_lowercase();
    let mut total_minutes: i64 = 0;
    let mut current_num = String::new();

    for c in s.chars() {
        if c.is_ascii_digit() {
            current_num.push(c);
        } else if c == 'h' {
            if current_num.is_empty() {
                bail!("Invalid duration '{}': missing number before 'h'", s);
            }
            let hours: i64 = current_num.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Invalid duration '{}': '{}' is not a valid number",
                    s,
                    current_num
                )
            })?;
            total_minutes += hours * 60;
            current_num.clear();
        } else if c == 'm' {
            if current_num.is_empty() {
                bail!("Invalid duration '{}': missing number before 'm'", s);
            }
            let mins: i64 = current_num.parse().map_err(|_| {
                anyhow::anyhow!(
                    "Invalid duration '{}': '{}' is not a valid number",
                    s,
                    current_num
                )
            })?;
            total_minutes += mins;
            current_num.clear();
        } else if !c.is_whitespace() {
            bail!(
                "Invalid duration '{}': unexpected character '{}'. Use format like '30m', '1h', '2h30m'",
                s, c
            );
        }
    }

    // Handle bare number (assume minutes)
    if !current_num.is_empty() {
        let mins: i64 = current_num.parse().map_err(|_| {
            anyhow::anyhow!(
                "Invalid duration '{}': '{}' is not a valid number",
                s,
                current_num
            )
        })?;
        total_minutes += mins;
    }

    if total_minutes <= 0 {
        bail!(
            "Invalid duration: '{}'. Use format like '30m', '1h', '2h30m'",
            s
        );
    }

    Ok(chrono::Duration::minutes(total_minutes))
}

/// Error type for HTTP validation failures (JSON-RPC error response)
type ValidationError = (StatusCode, Json<Value>);

/// Validate Bearer token using constant-time comparison.
///
/// Returns Ok(()) if valid or no key configured, Err with 401 response otherwise.
fn validate_api_key(
    headers: &HeaderMap,
    expected_key: Option<&str>,
) -> Result<(), ValidationError> {
    let Some(expected) = expected_key else {
        return Ok(());
    };

    let auth_header = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let provided = auth_header.strip_prefix("Bearer ").unwrap_or("");

    // Constant-time comparison to prevent timing attacks
    // Note: Length comparison still leaks length, but this is acceptable for API keys
    // since attackers can't exploit length timing without knowing the key format.
    let valid = provided.len() == expected.len()
        && bool::from(provided.as_bytes().ct_eq(expected.as_bytes()));

    if valid {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": "Invalid or missing API key"}
            })),
        ))
    }
}

/// Validate Origin header for DNS rebinding protection (MCP 2025-11-25 spec).
///
/// Allows localhost origins only. Empty/missing Origin is allowed.
fn validate_origin_header(headers: &HeaderMap) -> Result<(), ValidationError> {
    if let Some(origin) = headers.get("origin") {
        let origin_str = origin.to_str().unwrap_or("");
        if !origin_str.is_empty() && !is_localhost_origin(origin_str) {
            return Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32600, "message": "Invalid origin"}
                })),
            ));
        }
    }
    Ok(())
}

/// Check if origin is a valid localhost origin.
/// Prevents bypass via subdomains like localhost.evil.com
fn is_localhost_origin(origin: &str) -> bool {
    // Check each allowed prefix and ensure it's followed by end, port, or path
    let prefixes = [
        "http://localhost",
        "http://127.0.0.1",
        "https://localhost",
        "https://127.0.0.1",
        "http://[::1]",
        "https://[::1]",
    ];

    for prefix in prefixes {
        if let Some(rest) = origin.strip_prefix(prefix) {
            // After prefix, must be empty, start with ':', or start with '/'
            if rest.is_empty() || rest.starts_with(':') || rest.starts_with('/') {
                return true;
            }
        }
    }
    false
}

/// Require Accept header includes text/event-stream for SSE endpoints.
fn require_accept_event_stream(headers: &HeaderMap) -> Result<(), ValidationError> {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !accept.contains("text/event-stream") {
        return Err((
            StatusCode::NOT_ACCEPTABLE,
            Json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -32600, "message": "Accept header must include text/event-stream"}
            })),
        ));
    }
    Ok(())
}

/// Handle POST /mcp - JSON-RPC requests (MCP 2025-11-25 compliant)
///
/// # Security
///
/// ## API Key Authentication
/// If an API key is configured, requests must include `Authorization: Bearer <key>` header.
///
/// ## Origin Validation
/// Validates the Origin header to prevent DNS rebinding attacks per MCP 2025-11-25 spec:
/// - Localhost origins (http://localhost:*, http://127.0.0.1:*) are allowed
/// - Empty/missing Origin header is allowed (some MCP clients don't send it)
/// - All other origins are rejected with 403 Forbidden
///
/// This behavior is intentional for a localhost-only service. If exposing via proxy,
/// additional origin restrictions should be configured at the proxy layer.
async fn handle_mcp_post(
    State(state): State<Arc<HttpState>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    if let Err(e) = validate_api_key(&headers, state.api_key.as_deref()) {
        return e;
    }
    if let Err(e) = validate_origin_header(&headers) {
        return e;
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

    let response = state.server.handle_request(request);

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
    State(state): State<Arc<HttpState>>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    validate_api_key(&headers, state.api_key.as_deref())?;
    require_accept_event_stream(&headers)?;

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
    let random: u32 = rand::rng().random();
    format!("{:x}-{:08x}", nanos, random)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== validate_api_key tests =====

    #[test]
    fn test_api_key_no_key_configured() {
        let headers = HeaderMap::new();
        assert!(validate_api_key(&headers, None).is_ok());
    }

    #[test]
    fn test_api_key_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer secret123".parse().unwrap());
        assert!(validate_api_key(&headers, Some("secret123")).is_ok());
    }

    #[test]
    fn test_api_key_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer wrong".parse().unwrap());
        let result = validate_api_key(&headers, Some("secret123"));
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_api_key_missing_header() {
        let headers = HeaderMap::new();
        let result = validate_api_key(&headers, Some("secret123"));
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_api_key_empty_provided() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer ".parse().unwrap());
        let result = validate_api_key(&headers, Some("secret123"));
        assert!(result.is_err());
    }

    #[test]
    fn test_api_key_wrong_prefix() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Basic secret123".parse().unwrap());
        let result = validate_api_key(&headers, Some("secret123"));
        assert!(result.is_err());
    }

    #[test]
    fn test_api_key_case_sensitive() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer SECRET123".parse().unwrap());
        let result = validate_api_key(&headers, Some("secret123"));
        assert!(result.is_err());
    }

    // ===== validate_origin_header tests =====

    #[test]
    fn test_origin_missing() {
        let headers = HeaderMap::new();
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_localhost_http() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://localhost".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_localhost_with_port() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://localhost:3000".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_127_0_0_1() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://127.0.0.1".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_localhost_https() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://localhost".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_127_0_0_1_https() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://127.0.0.1:8443".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_external_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://evil.com".parse().unwrap());
        let result = validate_origin_header(&headers);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_origin_localhost_in_subdomain_rejected() {
        // localhost.evil.com must be rejected - DNS rebinding attack vector
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://localhost.evil.com".parse().unwrap());
        let result = validate_origin_header(&headers);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn test_origin_localhost_with_path() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://localhost/api".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_ipv6_localhost() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://[::1]".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_ipv6_localhost_with_port() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "http://[::1]:3000".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    #[test]
    fn test_origin_ipv6_localhost_https() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://[::1]:8443".parse().unwrap());
        assert!(validate_origin_header(&headers).is_ok());
    }

    // ===== require_accept_event_stream tests =====

    #[test]
    fn test_accept_event_stream_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("accept", "text/event-stream".parse().unwrap());
        assert!(require_accept_event_stream(&headers).is_ok());
    }

    #[test]
    fn test_accept_event_stream_with_other_types() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "accept",
            "application/json, text/event-stream".parse().unwrap(),
        );
        assert!(require_accept_event_stream(&headers).is_ok());
    }

    #[test]
    fn test_accept_missing() {
        let headers = HeaderMap::new();
        let result = require_accept_event_stream(&headers);
        assert!(result.is_err());
        let (status, _) = result.unwrap_err();
        assert_eq!(status, StatusCode::NOT_ACCEPTABLE);
    }

    #[test]
    fn test_accept_wrong_type() {
        let mut headers = HeaderMap::new();
        headers.insert("accept", "application/json".parse().unwrap());
        let result = require_accept_event_stream(&headers);
        assert!(result.is_err());
    }

    // ===== AuditMode tests =====

    #[test]
    fn test_audit_mode_default_inactive() {
        let mode = AuditMode::default();
        assert!(!mode.is_active());
    }

    #[test]
    fn test_audit_mode_enabled_active() {
        let mode = AuditMode {
            enabled: true,
            expires_at: None,
        };
        assert!(mode.is_active());
    }

    #[test]
    fn test_audit_mode_expired_inactive() {
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        };
        assert!(!mode.is_active());
    }

    #[test]
    fn test_audit_mode_not_expired_active() {
        let mode = AuditMode {
            enabled: true,
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
        };
        assert!(mode.is_active());
    }

    // ===== parse_duration tests =====

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(
            parse_duration("30m").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(parse_duration("1m").unwrap(), chrono::Duration::minutes(1));
        assert_eq!(
            parse_duration("120m").unwrap(),
            chrono::Duration::minutes(120)
        );
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h").unwrap(), chrono::Duration::minutes(60));
        assert_eq!(
            parse_duration("2h").unwrap(),
            chrono::Duration::minutes(120)
        );
    }

    #[test]
    fn test_parse_duration_combined() {
        assert_eq!(
            parse_duration("1h30m").unwrap(),
            chrono::Duration::minutes(90)
        );
        assert_eq!(
            parse_duration("2h15m").unwrap(),
            chrono::Duration::minutes(135)
        );
    }

    #[test]
    fn test_parse_duration_bare_number() {
        // Bare number = minutes
        assert_eq!(parse_duration("30").unwrap(), chrono::Duration::minutes(30));
    }

    #[test]
    fn test_parse_duration_whitespace() {
        assert_eq!(
            parse_duration("  30m  ").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(
            parse_duration("1h 30m").unwrap(),
            chrono::Duration::minutes(90)
        );
    }

    #[test]
    fn test_parse_duration_case_insensitive() {
        assert_eq!(
            parse_duration("30M").unwrap(),
            chrono::Duration::minutes(30)
        );
        assert_eq!(parse_duration("1H").unwrap(), chrono::Duration::minutes(60));
    }

    #[test]
    fn test_parse_duration_invalid_character() {
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn test_parse_duration_zero() {
        assert!(parse_duration("0m").is_err());
        assert!(parse_duration("0").is_err());
    }

    #[test]
    fn test_parse_duration_empty() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("   ").is_err());
    }

    #[test]
    fn test_parse_duration_missing_number() {
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("h").is_err());
        assert!(parse_duration("hm").is_err());
    }

    // ===== Fuzz tests =====

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: JsonRpcRequest parsing should never panic
            #[test]
            fn fuzz_jsonrpc_parse_no_panic(input in "\\PC{0,1000}") {
                let _ = serde_json::from_str::<JsonRpcRequest>(&input);
            }

            /// Fuzz: JsonRpcRequest with JSON-like structure
            #[test]
            fn fuzz_jsonrpc_structured(
                jsonrpc in "(1\\.0|2\\.0|[0-9]\\.[0-9])",
                id in prop::option::of(0i64..1000),
                method in "[a-z/_]{1,30}",
            ) {
                let json = match id {
                    Some(id) => format!(
                        r#"{{"jsonrpc":"{}","id":{},"method":"{}"}}"#,
                        jsonrpc, id, method
                    ),
                    None => format!(
                        r#"{{"jsonrpc":"{}","method":"{}"}}"#,
                        jsonrpc, method
                    ),
                };
                let _ = serde_json::from_str::<JsonRpcRequest>(&json);
            }

            /// Fuzz: is_localhost_origin should never panic
            #[test]
            fn fuzz_is_localhost_origin_no_panic(input in "\\PC{0,200}") {
                let _ = is_localhost_origin(&input);
            }

            /// Fuzz: origin validation with URL-like strings
            #[test]
            fn fuzz_origin_url_like(
                scheme in "(http|https|ftp|ws)",
                host in "[a-z0-9.-]{1,50}",
                port in prop::option::of(1u16..65535),
            ) {
                let origin = match port {
                    Some(p) => format!("{}://{}:{}", scheme, host, p),
                    None => format!("{}://{}", scheme, host),
                };
                let _ = is_localhost_origin(&origin);
            }
        }
    }
}
