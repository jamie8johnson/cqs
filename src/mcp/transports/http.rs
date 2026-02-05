//! HTTP transport for MCP server
//!
//! Implements the MCP Streamable HTTP transport (MCP spec 2025-11-25).

use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
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
use serde_json::Value;
use subtle::ConstantTimeEq;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use super::super::server::{McpServer, MCP_PROTOCOL_VERSION};
use super::super::types::JsonRpcRequest;

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
///
/// # Design Note: Separate bind and port parameters
///
/// We use separate `bind` and `port` instead of a combined `SocketAddr` for CLI ergonomics:
/// - `--bind 127.0.0.1 --port 3000` is clearer than `--addr 127.0.0.1:3000`
/// - Allows independent defaults (bind defaults to localhost, port to 3000)
/// - Matches conventions of similar tools (uvicorn, gunicorn, etc.)
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

    // CORS layer allows any origin for preflight requests (OPTIONS).
    // Actual origin validation happens in validate_origin_header() which rejects
    // non-localhost origins with 403 Forbidden. This two-layer approach is intentional:
    // - CORS preflight must succeed for browsers to send the actual request
    // - Application-level validation then enforces localhost-only access
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
///
/// # Security Note: Version Exposure
///
/// This endpoint exposes the service version. This is intentional for:
/// - Operational monitoring and debugging
/// - Client compatibility checks
/// - Standard health check patterns (Kubernetes, load balancers)
///
/// For a localhost-only service with origin validation, version exposure is
/// acceptable. If exposed publicly, consider removing version or requiring auth.
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
}
