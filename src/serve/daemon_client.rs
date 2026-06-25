//! Synchronous client of the retrieval daemon socket for the `cqs serve` web UI.
//!
//! The serve process holds no embedder, vector index, or SPLADE index — it
//! opens the store read-only for the graph/chunk views. The SPLADE-leg
//! inspector (`/api/search_legs`) needs the live retrieval stack, so instead of
//! loading the models into the web process it forwards the query to the
//! retrieval daemon (`cqs watch --serve`) over the SAME Unix socket the CLI
//! client uses, and returns the daemon's three-leg response.
//!
//! The wire protocol mirrors the CLI client (`src/cli/dispatch.rs`): a single
//! request line `{"command": <verb>, "args": [<argv>...]}` followed by a single
//! response line `{"status": "ok"|..., "output": <value>}`.

use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::error::ServeError;

/// Forward a `search-legs` query to the retrieval daemon and return its parsed
/// `output` JSON (the [`crate::cli::commands`]-built legs payload).
///
/// Returns [`ServeError::ServiceUnavailable`] when the socket is absent or the
/// daemon is unresponsive — the caller renders that as a 503 with the
/// "mechanism mode requires `cqs watch --serve`" hint rather than silently
/// falling back to a degraded surface. A daemon error response (status != ok)
/// surfaces as [`ServeError::Internal`].
///
/// `args` are the argv tokens after the verb (e.g. `["my query", "--limit",
/// "5"]`). This function builds the request frame, runs the round-trip on the
/// CALLING thread (it does blocking socket I/O — call it from a blocking
/// context), and parses one response line.
pub(crate) fn query_search_legs(
    socket: &Path,
    args: &[String],
) -> Result<serde_json::Value, ServeError> {
    let _span = tracing::info_span!("serve_daemon_client", verb = "search-legs").entered();

    // Socket-absent is the normal "no daemon running" case — a clean 503, no
    // warn. The hint names the fix.
    if !socket.exists() {
        tracing::debug!(path = %socket.display(), "search-legs: daemon socket absent");
        return Err(daemon_down_error());
    }

    let stream = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e) => {
            // The socket file exists but connect failed — a wedged/crashed
            // daemon. Warn so the journal explains the 503.
            tracing::warn!(
                path = %socket.display(),
                error = %e,
                stage = "connect",
                "search-legs: daemon connect failed"
            );
            return Err(daemon_down_error());
        }
    };

    // Single shared timeout knob across CLI client and daemon.
    let timeout = crate::daemon_translate::resolve_daemon_timeout_ms();
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::warn!(error = %e, "search-legs: failed to set read timeout");
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::warn!(error = %e, "search-legs: failed to set write timeout");
    }

    let request = serde_json::json!({
        "command": "search-legs",
        "args": args,
    });

    let mut stream = stream;
    if let Err(e) = writeln!(stream, "{request}") {
        tracing::warn!(error = %e, stage = "write", "search-legs: daemon write failed");
        return Err(daemon_down_error());
    }
    if let Err(e) = stream.flush() {
        tracing::warn!(error = %e, stage = "flush", "search-legs: daemon flush failed");
        return Err(daemon_down_error());
    }

    // Bound the response so a rogue daemon can't force an unbounded allocation.
    let max_response = crate::limits::max_daemon_response_bytes();
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(max_response.saturating_add(1));
    let mut response_line = String::new();
    let bytes_read = match reader.read_line(&mut response_line) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, stage = "read", "search-legs: daemon read failed");
            return Err(daemon_down_error());
        }
    };
    if bytes_read as u64 > max_response {
        tracing::warn!(
            bytes = bytes_read,
            "search-legs: daemon response exceeded cap"
        );
        return Err(ServeError::Internal(
            "daemon response exceeded size cap".to_string(),
        ));
    }

    let resp: serde_json::Value = serde_json::from_str(response_line.trim())
        .map_err(|e| ServeError::Internal(format!("daemon response parse failed: {e}")))?;

    match resp.get("status").and_then(|v| v.as_str()) {
        Some("ok") => resp
            .get("output")
            .cloned()
            .ok_or_else(|| ServeError::Internal("daemon ok response missing 'output'".to_string())),
        Some(other) => {
            // The daemon ran but the command failed (bad filter flag, embedder
            // error, …). Surface the daemon's message when present.
            let detail = resp
                .get("message")
                .or_else(|| resp.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or(other)
                .to_string();
            tracing::warn!(status = other, detail = %detail, "search-legs: daemon error response");
            Err(ServeError::Internal(format!("daemon error: {detail}")))
        }
        None => Err(ServeError::Internal(
            "daemon response missing 'status'".to_string(),
        )),
    }
}

/// The canonical "mechanism mode needs the daemon" 503 error.
fn daemon_down_error() -> ServeError {
    ServeError::ServiceUnavailable(
        "mechanism mode requires the retrieval daemon — start it with `cqs watch --serve`"
            .to_string(),
    )
}
