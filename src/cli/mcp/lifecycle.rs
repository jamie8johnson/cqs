//! MCP lifecycle: JSON-RPC envelope types + `initialize` / `initialized`.
//!
//! The JSON-RPC types are ported verbatim from the v0.10.0 MCP server
//! (`git show 291ec6b0^:src/mcp/types.rs`) — they were correct, and reusing
//! them keeps the wire shape identical to the surface clients already knew.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP protocol version this bridge advertises (D4d).
///
/// We advertise `2025-11-25` and echo it back regardless of the client's
/// requested version, but ACCEPT any client whose `protocolVersion` is
/// `>= 2025-06-18` (see [`handle_initialize`]) rather than hard-rejecting.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Minimum client protocol version the bridge will negotiate with (D4d).
const MIN_ACCEPTED_PROTOCOL_VERSION: &str = "2025-06-18";

// ─── JSON-RPC envelope (ported from 291ec6b0^:src/mcp/types.rs) ──────────────

/// A JSON-RPC 2.0 request (or notification, when `id` is absent).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 response. Exactly one of `result` / `error` is `Some`.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ─── JSON-RPC error codes ────────────────────────────────────────────────────
//
// Protocol / framing failures map to these `-32xxx` codes; handler-semantic
// failures do NOT (they ride as `isError:true` inside a successful
// `tools/call` result — see `tools::call`).

/// Parse error: a stdin line that is not valid JSON, or exceeds the size cap.
pub const PARSE_ERROR: i32 = -32700;
/// Method not found: an unknown JSON-RPC method, or an unknown `tools/call`
/// tool name.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid params: `tools/call` arguments that fail to deserialize into the
/// tool's core struct.
pub const INVALID_PARAMS: i32 = -32602;
/// Internal error: a transport failure talking to the daemon (socket missing,
/// connect/read/write failure, malformed daemon response).
pub const INTERNAL_ERROR: i32 = -32603;

// ─── Constructors ────────────────────────────────────────────────────────────

/// Build a success response carrying `result`.
pub fn success(id: Option<Value>, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

/// Build an error response. `id` is `None` for errors raised before an `id`
/// could be parsed (e.g. a parse error).
pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

/// The notification sentinel: a `Value::Null` result with no `id`. The bridge
/// loop suppresses writing a response line for this shape (notifications get no
/// reply).
pub fn notification_handled(id: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(Value::Null),
        error: None,
    }
}

// ─── initialize ──────────────────────────────────────────────────────────────

/// Handle the `initialize` request (D4d).
///
/// Does not negotiate beyond a floor check: if the client requests a
/// `protocolVersion` older than [`MIN_ACCEPTED_PROTOCOL_VERSION`], reply with
/// our [`MCP_PROTOCOL_VERSION`] anyway (lexicographic date strings compare
/// correctly), but log it. We advertise:
/// - `protocolVersion`: [`MCP_PROTOCOL_VERSION`].
/// - `capabilities.tools.listChanged: false` (the tool set is static).
/// - `serverInfo`: `{name, version, title}`.
/// - `instructions`: a short static usage string (new in 2025-11-25).
pub fn handle_initialize(params: Option<Value>) -> Value {
    let _span = tracing::info_span!("mcp_initialize").entered();

    // The client's requested version + identity are read for compliance /
    // logging only; we do not branch behavior on them (beyond the floor log).
    let requested = params
        .as_ref()
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let client_name = params
        .as_ref()
        .and_then(|p| p.get("clientInfo"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if !requested.is_empty() && requested < MIN_ACCEPTED_PROTOCOL_VERSION {
        tracing::warn!(
            requested,
            min = MIN_ACCEPTED_PROTOCOL_VERSION,
            advertised = MCP_PROTOCOL_VERSION,
            client = client_name,
            "MCP client requested a protocol version below the accepted floor; \
             responding with the advertised version anyway"
        );
    } else {
        tracing::info!(requested, client = client_name, "MCP client connected");
    }

    serde_json::json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "cqs",
            "version": env!("CARGO_PKG_VERSION"),
            "title": "cqs"
        },
        "instructions":
            "cqs semantic code search and call-graph navigation, exposed as \
             read-only MCP tools. Each tool rides the corresponding `cqs` \
             command over a warm daemon. Use cqs_search for concept queries, \
             cqs_callers / cqs_callees / cqs_impact for the call graph, and \
             cqs_scout / cqs_gather to assemble context."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_protocol_and_serverinfo() {
        let result = handle_initialize(Some(serde_json::json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0"}
        })));
        assert_eq!(
            result.get("protocolVersion").and_then(|v| v.as_str()),
            Some(MCP_PROTOCOL_VERSION)
        );
        assert_eq!(
            result
                .get("capabilities")
                .and_then(|c| c.get("tools"))
                .and_then(|t| t.get("listChanged"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            result
                .get("serverInfo")
                .and_then(|s| s.get("name"))
                .and_then(|v| v.as_str()),
            Some("cqs")
        );
        assert!(result
            .get("serverInfo")
            .and_then(|s| s.get("title"))
            .is_some());
        assert!(result.get("instructions").is_some());
    }

    /// An old-but-accepted client version (>= floor) still gets our advertised
    /// version echoed; an empty/absent version also works.
    #[test]
    fn initialize_accepts_floor_version_and_missing_params() {
        let r1 = handle_initialize(Some(serde_json::json!({
            "protocolVersion": "2025-06-18"
        })));
        assert_eq!(
            r1.get("protocolVersion").and_then(|v| v.as_str()),
            Some(MCP_PROTOCOL_VERSION)
        );
        let r2 = handle_initialize(None);
        assert_eq!(
            r2.get("protocolVersion").and_then(|v| v.as_str()),
            Some(MCP_PROTOCOL_VERSION)
        );
    }
}
