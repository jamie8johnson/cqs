//! JSON-RPC and MCP protocol types
//!
//! These types implement the MCP (Model Context Protocol) JSON-RPC interface.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

// MCP protocol types

/// MCP initialize request parameters.
///
/// These fields are required by the MCP protocol spec and must be deserialized,
/// but the server doesn't use them beyond validation - we accept any protocol version
/// and don't make decisions based on client capabilities or identity.
#[derive(Deserialize)]
pub(crate) struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    #[allow(dead_code)]
    pub protocol_version: String,
    #[allow(dead_code)]
    pub capabilities: Value,
    #[serde(rename = "clientInfo")]
    #[allow(dead_code)]
    pub client_info: ClientInfo,
}

/// MCP client info (part of initialize request).
/// Deserialized for protocol compliance but not used.
#[derive(Deserialize)]
pub(crate) struct ClientInfo {
    #[allow(dead_code)]
    pub name: String,
    #[allow(dead_code)]
    pub version: String,
}

#[derive(Serialize)]
pub(crate) struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

#[derive(Serialize)]
pub(crate) struct ServerCapabilities {
    pub tools: ToolsCapability,
}

#[derive(Serialize)]
pub(crate) struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Serialize)]
pub(crate) struct ServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Serialize)]
pub(crate) struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Serialize)]
pub(crate) struct ToolsListResult {
    pub tools: Vec<Tool>,
}

/// Search tool arguments
#[derive(Deserialize)]
pub(crate) struct SearchArgs {
    pub query: String,
    pub limit: Option<usize>,
    pub threshold: Option<f32>,
    pub language: Option<String>,
    pub path_pattern: Option<String>,
    pub name_boost: Option<f32>,
    /// Filter by chunk type (function, method, class, etc.)
    pub chunk_type: Option<String>,
    pub semantic_only: Option<bool>,
    /// Definition search mode - find by name only, no semantic matching.
    /// Use for "where is X defined?" queries. Much faster than semantic search.
    pub name_only: Option<bool>,
    /// Weight for note scores in results (0.0-1.0, default 1.0)
    /// Lower values make notes rank lower than code with similar semantic scores.
    pub note_weight: Option<f32>,
    /// Filter which indexes to search. Use "project" for primary, reference names for others.
    pub sources: Option<Vec<String>>,
    /// Filter by structural code pattern (builder, error_swallow, async, mutex, unsafe, recursion)
    pub pattern: Option<String>,
}

/// Audit mode arguments
#[derive(Deserialize)]
pub(crate) struct AuditModeArgs {
    pub enabled: Option<bool>,
    pub expires_in: Option<String>,
}
