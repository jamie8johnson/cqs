//! The stdio JSON-RPC loop: read framed stdin → parse → route → write framed
//! stdout. The bridge is a CLIENT of the cqs daemon (D4a) — every `tools/call`
//! relays the Lane 1 JSON-args frame over the daemon's unix socket.
//!
//! ## Framing — newline-delimited JSON (NDJSON)
//!
//! One JSON-RPC message per line on stdin, one per line on stdout (matching the
//! v0.10.0 stdio transport and the daemon socket, both JSONL). NDJSON has no
//! escaping, so the response MUST be single-line: `serde_json::to_string`
//! (never `to_string_pretty`) keeps an embedded `\n` out of the stream.
//!
//! ## stdout discipline
//!
//! stdout carries ONLY JSON-RPC. The bridge loads no GPU/hnsw/model, so there
//! is no library stdout noise to gag; all `tracing` routes to stderr (the
//! subscriber in `main.rs`). The 1 MiB request-line cap is reused from the old
//! transport.

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;

use super::lifecycle::{self, JsonRpcRequest, JsonRpcResponse};
use super::tools::{self, CallOutcome};

/// Maximum stdin line length (1 MiB) — reused from the v0.10.0 transport.
const MAX_LINE_LENGTH: usize = 1_048_576;

/// Run the `cqs mcp` stdio bridge against the daemon serving `cqs_dir`.
///
/// Reads JSON-RPC requests from stdin and writes responses to stdout until EOF.
/// Returns `Ok(())` on clean EOF; an `Err` only on an unrecoverable stdout I/O
/// failure (a broken pipe to the client).
pub fn serve_stdio(cqs_dir: &Path) -> Result<()> {
    let _span = tracing::info_span!("mcp_serve_stdio", cqs_dir = %cqs_dir.display()).entered();
    tracing::info!("cqs MCP bridge starting (stdio ↔ daemon socket)");

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                // A stdin read error ends the session — the client's pipe is
                // gone. Nothing to respond to.
                tracing::info!(error = %e, "stdin closed; MCP bridge exiting");
                break;
            }
        };

        // Oversized line → parse error, no id (we can't trust the contents).
        if line.len() > MAX_LINE_LENGTH {
            let resp = lifecycle::error(
                None,
                lifecycle::PARSE_ERROR,
                format!(
                    "request too large: {} bytes (max {MAX_LINE_LENGTH})",
                    line.len()
                ),
            );
            write_response(&mut stdout, &resp)?;
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let resp =
                    lifecycle::error(None, lifecycle::PARSE_ERROR, format!("parse error: {e}"));
                write_response(&mut stdout, &resp)?;
                continue;
            }
        };

        let response = route(cqs_dir, request);

        // Notifications (no id, null result) get NO response line.
        if response.id.is_none()
            && response.error.is_none()
            && response.result.as_ref().is_some_and(|v| v.is_null())
        {
            continue;
        }

        write_response(&mut stdout, &response)?;
    }

    tracing::info!("MCP bridge stdin EOF; exiting");
    Ok(())
}

/// Route one parsed request by method.
fn route(cqs_dir: &Path, request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id.clone();
    match request.method.as_str() {
        "initialize" => lifecycle::success(id, lifecycle::handle_initialize(request.params)),
        // The `initialized` notification (both spellings) — no reply.
        "notifications/initialized" | "initialized" => lifecycle::notification_handled(id),
        "tools/list" => lifecycle::success(id, tools::list()),
        "tools/call" => match tools::call(cqs_dir, request.params) {
            CallOutcome::Result(result) => lifecycle::success(id, result),
            CallOutcome::ProtocolError(code, message) => lifecycle::error(id, code, message),
        },
        // `ping` is a JSON-RPC utility method (empty result), distinct from the
        // `cqs ping` daemon command — clients may send it as a keepalive.
        "ping" => lifecycle::success(id, serde_json::json!({})),
        other => lifecycle::error(
            id,
            lifecycle::METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        ),
    }
}

/// Serialize one response as a single NDJSON line and flush. NEVER pretty-print
/// (an embedded newline corrupts the stream).
fn write_response(stdout: &mut impl Write, resp: &JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(resp)?;
    writeln!(stdout, "{json}")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn req(method: &str, id: u64, params: serde_json::Value) -> JsonRpcRequest {
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .unwrap()
    }

    #[test]
    fn route_initialize_returns_handshake() {
        let resp = route(
            &PathBuf::from("/x/.cqs"),
            req(
                "initialize",
                1,
                serde_json::json!({"protocolVersion": "2025-11-25"}),
            ),
        );
        let result = resp.result.expect("result");
        assert_eq!(
            result.get("protocolVersion").and_then(|v| v.as_str()),
            Some(lifecycle::MCP_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn route_tools_list_returns_tools() {
        let resp = route(
            &PathBuf::from("/x/.cqs"),
            req("tools/list", 2, serde_json::json!({})),
        );
        let result = resp.result.expect("result");
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .expect("tools");
        assert!(tools
            .iter()
            .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("cqs_search")));
        // context/explain withheld.
        assert!(!tools
            .iter()
            .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("cqs_context")));
    }

    #[test]
    fn route_unknown_method_is_method_not_found() {
        let resp = route(
            &PathBuf::from("/x/.cqs"),
            req("bogus/method", 3, serde_json::json!({})),
        );
        assert_eq!(
            resp.error.as_ref().map(|e| e.code),
            Some(lifecycle::METHOD_NOT_FOUND)
        );
    }

    #[test]
    fn route_initialized_is_notification() {
        // A notification has no id and a null result; the loop suppresses it.
        let mut r: JsonRpcRequest = serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .unwrap();
        r.id = None;
        let resp = route(&PathBuf::from("/x/.cqs"), r);
        assert!(resp.id.is_none());
        assert!(resp.result.as_ref().is_some_and(|v| v.is_null()));
    }
}
