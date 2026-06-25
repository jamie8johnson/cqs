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

use std::io::{BufRead, Read, Write};
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
///
/// Fails fast on a non-unix target: the bridge's only transport is the daemon's
/// unix socket, so it cannot serve a single `tools/call` off-unix. Refusing up
/// front beats advertising a tool set whose every call returns INTERNAL_ERROR.
pub fn serve_stdio(cqs_dir: &Path) -> Result<()> {
    let _span = tracing::info_span!("mcp_serve_stdio", cqs_dir = %cqs_dir.display()).entered();

    #[cfg(not(unix))]
    {
        let _ = cqs_dir;
        anyhow::bail!(
            "the cqs MCP bridge requires a unix daemon socket and is not supported on this platform"
        );
    }

    #[cfg(unix)]
    {
        tracing::info!("cqs MCP bridge starting (stdio ↔ daemon socket)");

        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        let mut stdout = std::io::stdout();

        // Read each request with a per-line byte cap applied BEFORE the line is
        // fully buffered: wrap a fresh `.take(MAX_LINE_LENGTH + 1)` per iteration
        // (the budget resets each line; the underlying BufReader keeps its
        // buffered bytes) and `read_until(b'\n')` into a reused buffer. A
        // no-newline multi-GB line therefore stops at the cap+1 byte instead of
        // OOMing the long-lived bridge. The `> MAX` length check stays as a
        // backstop (mirrors the daemon socket reader).
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        loop {
            buf.clear();
            let n = match (&mut reader)
                .take(MAX_LINE_LENGTH as u64 + 1)
                .read_until(b'\n', &mut buf)
            {
                Ok(0) => break, // EOF.
                Ok(n) => n,
                Err(e) => {
                    // A stdin read error ends the session — the client's pipe is
                    // gone. Nothing to respond to.
                    tracing::info!(error = %e, "stdin closed; MCP bridge exiting");
                    break;
                }
            };

            // Oversized line → parse error, no id (we can't trust the contents).
            // The `.take` cap bounds memory; this length check classifies it. A
            // line that exactly fills the cap with no newline also trips here.
            if n > MAX_LINE_LENGTH || (buf.len() > MAX_LINE_LENGTH) {
                // Drain the rest of the oversized line so the NEXT iteration
                // starts at a fresh message boundary rather than mid-line.
                drain_to_newline(&mut reader);
                let resp = lifecycle::error(
                    None,
                    lifecycle::PARSE_ERROR,
                    format!("request too large: {n} bytes (max {MAX_LINE_LENGTH})"),
                );
                write_response(&mut stdout, &resp)?;
                continue;
            }

            // Decode as UTF-8; a non-UTF-8 line is a parse error.
            let line = match std::str::from_utf8(&buf) {
                Ok(s) => s,
                Err(e) => {
                    let resp = lifecycle::error(
                        None,
                        lifecycle::PARSE_ERROR,
                        format!("parse error: invalid UTF-8 ({e})"),
                    );
                    write_response(&mut stdout, &resp)?;
                    continue;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str(line) {
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
}

/// Discard up to one newline's worth of bytes after an oversized line, so the
/// next `read_until` resumes on a clean message boundary. Memory-bounded by a
/// `.take(MAX_LINE_LENGTH)` cap: a no-newline continuation longer than the cap
/// is left for the next loop iteration to re-classify as oversized — bytes are
/// never accumulated past the cap, so the long-lived bridge cannot OOM.
#[cfg(unix)]
fn drain_to_newline<R: BufRead>(reader: &mut R) {
    let mut sink = Vec::new();
    let _ = reader
        .take(MAX_LINE_LENGTH as u64)
        .read_until(b'\n', &mut sink);
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
