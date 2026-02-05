//! MCP Server core implementation
//!
//! The McpServer handles JSON-RPC requests and coordinates tool execution.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::embedder::Embedder;
use crate::hnsw::HnswIndex;
use crate::index::VectorIndex;
use crate::store::Store;

use super::audit_mode::AuditMode;
use super::tools;
use super::types::{
    ClientInfo, InitializeParams, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    ServerCapabilities, ServerInfo, ToolsCapability,
};

/// MCP protocol version
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// MCP Server
///
/// Uses interior mutability (OnceLock, Mutex) to allow concurrent read access
/// via RwLock read locks. This enables parallel request handling.
pub struct McpServer {
    pub(crate) store: Store,
    /// Lazily initialized embedder (thread-safe via OnceLock)
    pub(crate) embedder: OnceLock<Embedder>,
    pub(crate) project_root: PathBuf,
    /// Vector index for O(log n) search (CAGRA or HNSW)
    /// Wrapped in Arc<RwLock> to allow background CAGRA upgrade
    pub(crate) index: Arc<RwLock<Option<Box<dyn VectorIndex>>>>,
    /// Use GPU for query embedding
    pub(crate) use_gpu: bool,
    /// Audit mode state (interior mutability for concurrent access)
    pub(crate) audit_mode: Mutex<AuditMode>,
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

        let store = Store::open(&index_path)
            .with_context(|| format!("Failed to open index at {}", index_path.display()))?;

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
    pub(crate) fn ensure_embedder(&self) -> Result<&Embedder> {
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
        self.embedder.get().ok_or_else(|| {
            anyhow::anyhow!("embedder initialization failed: OnceLock empty after set")
        })
    }

    /// Handle a JSON-RPC request
    ///
    /// Takes &self (not &mut self) to allow concurrent request handling via read locks.
    pub fn handle_request(&self, request: JsonRpcRequest) -> JsonRpcResponse {
        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(request.params),
            "initialized" => Ok(Value::Null), // Notification, no response needed
            "tools/list" => tools::handle_tools_list(),
            "tools/call" => tools::handle_tools_call(self, request.params),
            _ => Err(anyhow::anyhow!("Unknown method: {}", request.method)),
        };

        match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: request.id,
                result: Some(value),
                error: None,
            },
            Err(e) => {
                // Sanitize error message to avoid exposing internal paths.
                // Log the full error for debugging, return sanitized version to client.
                let full_error = e.to_string();
                tracing::debug!(error = %full_error, "Request error");
                let sanitized = self.sanitize_error_message(&full_error);
                JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: request.id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32000,
                        message: sanitized,
                        data: None,
                    }),
                }
            }
        }
    }

    /// Sanitize error messages to avoid exposing internal filesystem paths.
    ///
    /// Replaces absolute paths (starting with / or drive letter) with relative paths
    /// or generic descriptions to prevent information leakage to clients.
    fn sanitize_error_message(&self, error: &str) -> String {
        // Strip the project root path if present
        let project_str = self.project_root.to_string_lossy();
        let sanitized = error.replace(project_str.as_ref(), "<project>");

        // Also strip common absolute path patterns that might leak from dependencies
        // Match Unix paths: /home/user/... /tmp/... etc.
        // Match Windows paths: C:\Users\... D:\...
        // Note: Using [^\s:] instead of [^\s:"'] to avoid raw string escaping issues
        let re_unix = regex::Regex::new(r"/(?:home|Users|tmp|var|usr|opt|etc)/[^\s:]+").ok();
        let re_windows =
            regex::Regex::new(r"[A-Za-z]:\\(?:Users|Windows|Program Files)[^\s:]*").ok();

        let mut result = sanitized;
        if let Some(re) = re_unix {
            result = re.replace_all(&result, "<path>").to_string();
        }
        if let Some(re) = re_windows {
            result = re.replace_all(&result, "<path>").to_string();
        }
        result
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

    /// Index notes into the database (embed and store)
    pub(crate) fn index_notes(
        &self,
        notes: &[crate::note::Note],
        notes_path: &std::path::Path,
    ) -> Result<usize> {
        let embedder = self.ensure_embedder()?;
        crate::index_notes(notes, notes_path, embedder, &self.store)
    }
}
