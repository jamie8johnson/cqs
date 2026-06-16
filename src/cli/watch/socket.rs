//! Daemon Unix-socket client handler.
//!
//! Per-connection state machine: read the JSON-line request, snapshot
//! the shared `BatchContext` into a `BatchView`, dispatch against the
//! view, then write the response. Carved out of `watch.rs` so the
//! parent module focuses on the watch loop and not its socket protocol.
//!
//! The accept loop and concurrency cap live in `cmd_watch` (parent
//! module) — only the per-client work lives here.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// RAII guard that removes the Unix socket file on drop.
pub(super) struct SocketCleanupGuard(pub(super) PathBuf);

impl Drop for SocketCleanupGuard {
    fn drop(&mut self) {
        if self.0.exists() {
            if let Err(e) = std::fs::remove_file(&self.0) {
                tracing::warn!(path = %self.0.display(), error = %e, "Failed to remove socket file");
            } else {
                tracing::info!(path = %self.0.display(), "Daemon socket removed");
            }
        }
    }
}
/// Cap concurrent daemon client threads so a misbehaving (or malicious)
/// local client can't spawn unbounded handlers and exhaust fds, threads, or
/// stacks. The BatchContext mutex serialises dispatch inside
/// `handle_socket_client`; this cap only bounds parallel read/parse/write I/O.
///
/// Default 16 (matches typical agent fan-out), overridable via
/// `CQS_MAX_DAEMON_CLIENTS`. At ~2 MB stack each, 16 caps worst-case at
/// ~32 MB.
const DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS: usize = 16;

/// Resolve the effective cap from `CQS_MAX_DAEMON_CLIENTS`, defaulting to
/// [`DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS`] scaled by host parallelism.
/// Called once at daemon startup, so an env-var change requires
/// `systemctl restart cqs-watch`.
///
/// Scales with cores within `[16, 64]` so a 64-core box running a large
/// Tasks fan-out doesn't queue requests serially past 16 clients. Stack
/// memory (16 × 2 MB = 32 MB) isn't the binding constraint on modern 64-bit
/// hosts.
pub(super) fn max_concurrent_daemon_clients() -> usize {
    let scaled_default = || {
        std::thread::available_parallelism()
            .map(|n| n.get().clamp(DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS, 64))
            .unwrap_or(DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS)
    };
    match std::env::var("CQS_MAX_DAEMON_CLIENTS") {
        Ok(val) => match val.parse::<usize>() {
            Ok(n) if n > 0 => {
                tracing::info!(
                    cap = n,
                    "Daemon client cap overridden via CQS_MAX_DAEMON_CLIENTS"
                );
                n
            }
            _ => {
                tracing::warn!(
                    val = %val,
                    "CQS_MAX_DAEMON_CLIENTS invalid — using parallelism-scaled default"
                );
                scaled_default()
            }
        },
        Err(_) => scaled_default(),
    }
}
/// Handle a single client connection on the daemon socket.
/// Reads one JSON-line request, dispatches via the shared BatchContext, writes response.
///
/// `batch_ctx` is a shared `Arc<Mutex<BatchContext>>`; reads and writes
/// happen without the lock so concurrent clients can parse their requests in
/// parallel. Only the dispatch itself acquires the mutex, so a slow/malicious
/// client's read window can't wedge the accept loop or sibling handlers.
pub(super) fn handle_socket_client(
    mut stream: std::os::unix::net::UnixStream,
    batch_ctx: &Arc<Mutex<crate::cli::batch::BatchContext>>,
) {
    let span = tracing::info_span!("daemon_query", command = tracing::field::Empty);
    let _enter = span.enter();
    let start = std::time::Instant::now();

    // Explicit warn on timeout failures rather than silent `.ok()` —
    // without a timeout a wedged client would pin the handler thread forever.
    //
    // Both timeouts come from the shared
    // `cqs::daemon_translate::resolve_daemon_timeout_ms` helper so a user
    // raising `CQS_DAEMON_TIMEOUT_MS` raises both sides symmetrically.
    let timeout = cqs::daemon_translate::resolve_daemon_timeout_ms();
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::warn!(
            error = %e,
            "Failed to set read timeout on daemon stream — slow client could pin handler"
        );
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::warn!(
            error = %e,
            "Failed to set write timeout on daemon stream — slow client could pin handler"
        );
    }

    // A plain stack-local `String::with_capacity(8192)`. The accept loop in
    // `daemon.rs` spawns a fresh `cqs-daemon-client` thread per connection,
    // not a Tokio blocking-pool worker, so a thread_local buffer would never
    // be reused across calls — the stack-local has the same single-allocation
    // cost.
    let mut line = String::with_capacity(8192);

    // Align with the CLI's `MAX_DIFF_BYTES` so a multi-MB diff
    // (`cqs review --stdin`, `cqs impact --diff`) routed through the daemon
    // doesn't fail with `TooLarge` while the same diff via direct CLI
    // succeeds. +4 KB JSON-envelope headroom for the daemon protocol wrapping.
    use std::io::Read as _;
    let max_request_bytes = crate::cli::limits::max_diff_bytes().saturating_add(4 * 1024);
    let take_cap = (max_request_bytes as u64).saturating_add(1);
    let mut reader = std::io::BufReader::new(&stream).take(take_cap);

    enum ParseOutcome {
        Ok(serde_json::Value),
        Empty,
        TooLarge,
        IoError(String),
        JsonError(String),
    }
    let parse_outcome = match std::io::BufRead::read_line(&mut reader, &mut line) {
        Ok(0) => ParseOutcome::Empty,
        Ok(n) if n > max_request_bytes => ParseOutcome::TooLarge,
        Err(e) => ParseOutcome::IoError(e.to_string()),
        Ok(_) => match serde_json::from_str(line.trim()) {
            Ok(v) => ParseOutcome::Ok(v),
            Err(e) => ParseOutcome::JsonError(e.to_string()),
        },
    };
    let request: serde_json::Value = match parse_outcome {
        ParseOutcome::Ok(v) => v,
        ParseOutcome::Empty => return,
        ParseOutcome::TooLarge => {
            let delivered = write_daemon_error_tracked(&mut stream, "request too large");
            tracing::info!(
                status = "client_error",
                delivered,
                latency_ms = start.elapsed().as_millis() as u64,
                "Daemon query complete"
            );
            return;
        }
        ParseOutcome::IoError(msg) => {
            tracing::debug!(error = %msg, "Socket read failed");
            return;
        }
        ParseOutcome::JsonError(msg) => {
            let delivered =
                write_daemon_error_tracked(&mut stream, &format!("invalid JSON: {msg}"));
            tracing::info!(
                status = "parse_error",
                delivered,
                latency_ms = start.elapsed().as_millis() as u64,
                "Daemon query complete"
            );
            return;
        }
    };

    let command = request
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Structurally validate args. Silently dropping non-string elements
    // (numbers, nested arrays, nulls) would leave surviving args that look
    // correct while the daemon runs a half-formed command. Instead: collect
    // the indices of non-string elements, warn on them, and reject the whole
    // request as malformed rather than execute on a truncated arg list.
    let raw_args = request.get("args").and_then(|v| v.as_array());
    // Fold validation + extraction into a single walk: one pass collects
    // both the typed `Vec<String>` and a parallel `Vec<usize>` of bad
    // indices.
    let (args, bad_arg_indices): (Vec<String>, Vec<usize>) = match raw_args {
        Some(arr) => {
            let mut args = Vec::with_capacity(arr.len());
            let mut bad = Vec::new();
            for (i, v) in arr.iter().enumerate() {
                match v.as_str() {
                    Some(s) => args.push(s.to_string()),
                    None => bad.push(i),
                }
            }
            (args, bad)
        }
        None => (Vec::new(), Vec::new()),
    };
    if !bad_arg_indices.is_empty() {
        tracing::warn!(
            command,
            ?bad_arg_indices,
            "Daemon rejected request: args contains non-string elements"
        );
        let delivered =
            write_daemon_error_tracked(&mut stream, "args contains non-string elements");
        tracing::info!(
            status = "bad_args",
            delivered,
            latency_ms = start.elapsed().as_millis() as u64,
            "Daemon query complete"
        );
        return;
    }

    // Record command on the span so every event inside this handler is
    // enriched with it, without needing to repeat `command` on each log.
    //
    // `notes add`/`update`/`remove` carry the note body as the first arg,
    // which may contain source snippets or secrets. Log only
    // `notes/<subcommand>` so operators see the shape of activity without the
    // body reaching the journal.
    let command_for_log: String = if command == "notes" {
        let sub = args.first().map(String::as_str).unwrap_or("<unknown>");
        // Only pass the subcommand itself through, never args beyond it.
        match sub {
            "add" | "update" | "remove" | "list" => format!("notes/{sub}"),
            _ => "notes/<unknown>".to_string(),
        }
    } else {
        command.to_string()
    };
    span.record("command", command_for_log.as_str());

    // No `args_preview` in `tracing::debug!`: a preview would leak file path
    // fragments and search-query snippets at the daemon debug level. The
    // command name is already on the span; `args_len` is enough for traffic
    // shaping. If a finer-grained preview is ever needed, gate it behind
    // `tracing::trace!` (off by default in production loggers).
    tracing::debug!(
        command = %command_for_log,
        args_len = args.len(),
        "Daemon request"
    );

    if command.is_empty() {
        let delivered = write_daemon_error_tracked(&mut stream, "missing 'command' field");
        tracing::info!(
            status = "client_error",
            delivered,
            latency_ms = start.elapsed().as_millis() as u64,
            "Daemon query complete"
        );
        return;
    }

    // No panic firewall around dispatch. The release profile sets `panic =
    // "abort"` (Cargo.toml), so a `catch_unwind` here would catch nothing in
    // the shipped binary — a panic in dispatch aborts the daemon and systemd
    // restarts it (cold caches). An earlier `catch_unwind` logged "daemon
    // continues", which only ever held in dev/test (unwind) builds; it was
    // removed so the code stops claiming a guarantee release does not provide.
    // Real per-request panic isolation that holds in release would require
    // `panic = "unwind"` (a deliberate binary-size/perf tradeoff that also
    // needs the in-flight slot counter made RAII) — deferred, not done here.
    let result: Result<serde_json::Value, String> = {
        // Pass pre-split tokens straight to `dispatch_via_view` instead of
        // joining them back into a shell string for `dispatch_line` to
        // immediately re-split via `shell_words::split`. The round-trip is
        // pure waste on every daemon query and a latent correctness bug on
        // tokens containing shell metacharacters — `shell_words::join` quotes
        // them, `shell_words::split` unquotes them, but any asymmetry
        // silently corrupts the token boundary.
        let mut output = Vec::new();
        // Hold the BatchContext mutex only long enough to snapshot
        // a `BatchView` (microseconds — clones a few `Arc`s under one
        // critical section). The handler then runs against the view
        // outside any BatchContext lock, so two slow queries (gather,
        // task) overlap on wall-clock. Refresh — the only daemon-
        // dispatchable command that mutates BatchContext interior — is
        // re-locked briefly inside `dispatch_via_view` via the view's
        // `outer_lock` back-channel.
        let view = crate::cli::batch::checkout_view_from_arc(batch_ctx);
        crate::cli::batch::dispatch_via_view(&view, command, &args, &mut output);
        // Parse the dispatch bytes into a JSON `Value` and embed it as a real
        // JSON field of the response envelope instead of round-tripping
        // through `String::from_utf8` and embedding as a string-in-string.
        // This eliminates the per-byte JSON-escape inflation that would
        // double large search responses on the wire.
        //
        // The parse also canonicalizes key order (serde_json `Value` is
        // BTreeMap-backed without `preserve_order`); the response is then
        // serialized exactly once into a byte buffer and written with a
        // single `write_all` (see `write_daemon_ok`), rather than
        // re-stringified through `Display` / `writeln!` per JSON fragment.
        let trimmed = trim_trailing_newline(&output);
        match serde_json::from_slice::<serde_json::Value>(trimmed) {
            Ok(v) => Ok(v),
            Err(parse_err) => {
                // Non-JSON dispatch output (e.g. a plaintext handler) falls
                // back to the legacy string-in-string envelope so the client
                // still receives the bytes. UTF-8 validation here flags the
                // rare case where the handler produced binary garbage.
                String::from_utf8(trimmed.to_vec())
                    .map(serde_json::Value::String)
                    .map_err(|utf_err| {
                        format!(
                            "dispatch output is neither valid JSON ({parse_err}) nor valid UTF-8 ({utf_err})"
                        )
                    })
            }
        }
    };

    let (status, delivered) = match result {
        Ok(output_value) => {
            let delivered = write_daemon_ok(&mut stream, output_value);
            ("ok", delivered)
        }
        Err(e) => {
            let delivered = write_daemon_error_tracked(&mut stream, &e);
            ("client_error", delivered)
        }
    };

    tracing::info!(
        status,
        delivered,
        latency_ms = start.elapsed().as_millis() as u64,
        "Daemon query complete"
    );
}
/// Serialize a daemon response `Value` as one JSONL frame
/// (`<json>\n`) into a single buffer and emit it with one `write_all`.
///
/// `UnixStream` is unbuffered: writing through `writeln!("{}", value)`
/// drives `Value`'s `Display` impl, whose `write_fmt` adapter issues a
/// `write_all` per JSON fragment (every key, string, and delimiter). A
/// multi-KB search response becomes hundreds of write syscalls — material
/// against the daemon's single-digit-millisecond budget, and worse on WSL
/// where syscalls are slow. Serializing into a `Vec<u8>` first collapses
/// the whole frame to one syscall, and `to_writer` serializes the value
/// exactly once (no intermediate `String`).
///
/// Returns whether the frame reached the client. Serialization of a
/// `serde_json::Value` cannot fail except on a downstream `io::Write`
/// error, and the buffer is in-memory, so the only failure mode is the
/// final socket write.
fn write_response_frame(
    stream: &mut impl std::io::Write,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    // Infallible for an in-memory `Vec<u8>` sink with a `Value` payload.
    serde_json::to_writer(&mut buf, value).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    stream.write_all(&buf)
}

/// Write the success envelope `{"status":"ok","output":<value>}` as one
/// frame. The wrapped `Value` carries the canonicalized dispatch output
/// (already parsed in `handle_socket_client`), so this is the sole
/// serialization of the response on the success path. Returns whether the
/// frame was delivered.
fn write_daemon_ok(stream: &mut impl std::io::Write, output: serde_json::Value) -> bool {
    let resp = serde_json::json!({
        "status": "ok",
        "output": output,
    });
    match write_response_frame(stream, &resp) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to write daemon response");
            false
        }
    }
}

pub(super) fn write_daemon_error(
    stream: &mut std::os::unix::net::UnixStream,
    message: &str,
) -> std::io::Result<()> {
    let resp = serde_json::json!({ "status": "error", "message": message });
    write_response_frame(stream, &resp)
}

/// Trim a trailing `\n` (and optional `\r`) from `buf` and return the
/// resulting slice. Mirrors `str::trim_end_matches` for newline cases
/// without forcing a UTF-8 validation step on the way in.
///
/// Used by `handle_socket_client` to strip the trailing newline that
/// `write_json_line` always emits before parsing the dispatch output as
/// JSON.
pub(super) fn trim_trailing_newline(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
        if end > 0 && buf[end - 1] == b'\r' {
            end -= 1;
        }
    }
    &buf[..end]
}

/// Like `write_daemon_error`, but logs on failure and returns whether
/// the write reached the client. Used by `handle_socket_client` to
/// populate the `delivered` telemetry field instead of silently
/// swallowing write errors with `let _ = ...`.
fn write_daemon_error_tracked(stream: &mut std::os::unix::net::UnixStream, message: &str) -> bool {
    match write_daemon_error(stream, message) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "Failed to write daemon error response");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `io::Write` sink that records every `write` / `write_all` call so a
    /// test can assert the response frame reaches the socket in exactly one
    /// syscall (`UnixStream` is unbuffered — fragmented writes there are
    /// fragmented syscalls).
    #[derive(Default)]
    struct CountingWriter {
        buf: Vec<u8>,
        writes: usize,
    }

    impl Write for CountingWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // The response frame is emitted with a single `write_all` of one
    // pre-serialized buffer. `write_all` on a `Vec`-extending sink that
    // never short-writes calls `write` exactly once — so one logical write,
    // one syscall against a real unbuffered `UnixStream`. Pins the
    // PERF-fix: no per-JSON-fragment Display write storm.
    #[test]
    fn response_frame_is_one_write() {
        let mut w = CountingWriter::default();
        let resp = serde_json::json!({
            "status": "ok",
            "output": {"data": {"a": 1, "b": [2, 3]}},
        });
        write_response_frame(&mut w, &resp).expect("frame writes");
        assert_eq!(
            w.writes, 1,
            "response must reach the socket in a single write; got {}",
            w.writes
        );
        assert!(w.buf.ends_with(b"\n"), "frame must end with a newline");
    }

    // `write_daemon_ok` wraps the dispatch output in
    // `{"status":"ok","output":<value>}` and serializes once. Pin the exact
    // wire bytes of a representative envelope so the single-serialization
    // path stays byte-identical to the prior parse → wrap → serialize shape:
    // serde_json `Value` is BTreeMap-backed (no `preserve_order`), so keys
    // emit alphabetically — `output` before `status` on the frame, and
    // `data` before everything in the slim envelope.
    #[test]
    fn daemon_ok_frame_exact_bytes() {
        let mut w = CountingWriter::default();
        // The dispatch output value, already parsed/canonicalized by the
        // handler before it reaches `write_daemon_ok`.
        let output = serde_json::json!({"data": {"zebra": 1, "alpha": 2}});
        let delivered = write_daemon_ok(&mut w, output);
        assert!(delivered, "frame must be delivered");
        let bytes = String::from_utf8(w.buf).expect("utf-8 frame");
        assert_eq!(
            bytes, "{\"output\":{\"data\":{\"alpha\":2,\"zebra\":1}},\"status\":\"ok\"}\n",
            "exact wire bytes must stay alphabetized + single-line + newline-terminated"
        );
    }

    // The error frame shares the single-serialization path and the same
    // byte contract: `{"message":...,"status":"error"}` alphabetized.
    #[test]
    fn daemon_error_frame_exact_bytes() {
        let mut w = CountingWriter::default();
        let resp = serde_json::json!({ "status": "error", "message": "boom" });
        write_response_frame(&mut w, &resp).expect("frame writes");
        let bytes = String::from_utf8(w.buf).expect("utf-8 frame");
        assert_eq!(bytes, "{\"message\":\"boom\",\"status\":\"error\"}\n");
    }
}
