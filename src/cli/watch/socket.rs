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

use std::cell::RefCell;
use std::io::Write;
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
/// SEC-V1.25-1: cap concurrent daemon client threads so a misbehaving
/// (or malicious) local client can't spawn unbounded handlers and exhaust
/// fds, threads, or stacks. The BatchContext mutex serialises dispatch
/// inside `handle_socket_client`; this cap only bounds parallel
/// read/parse/write I/O.
///
/// P3 #125: default lowered from 64 → 16 (matches typical agent fan-out)
/// and overridable via `CQS_MAX_DAEMON_CLIENTS`. At ~2 MB stack each, 16
/// caps worst-case at ~32 MB instead of ~128 MB.
const DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS: usize = 16;

/// Resolve the effective cap from `CQS_MAX_DAEMON_CLIENTS`, defaulting to
/// [`DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS`]. Called once at daemon
/// startup, so an env-var change requires `systemctl restart cqs-watch`.
pub(super) fn max_concurrent_daemon_clients() -> usize {
    std::env::var("CQS_MAX_DAEMON_CLIENTS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_CONCURRENT_DAEMON_CLIENTS)
}
/// Handle a single client connection on the daemon socket.
/// Reads one JSON-line request, dispatches via the shared BatchContext, writes response.
///
/// SEC-V1.25-1: `batch_ctx` is a shared `Arc<Mutex<BatchContext>>`; reads and
/// writes happen without the lock so concurrent clients can parse their
/// requests in parallel. Only the dispatch itself acquires the mutex, so a
/// slow/malicious client's 5 s read window no longer wedges the accept loop
/// or sibling handlers.
pub(super) fn handle_socket_client(
    mut stream: std::os::unix::net::UnixStream,
    batch_ctx: &Arc<Mutex<crate::cli::batch::BatchContext>>,
) {
    let span = tracing::info_span!("daemon_query", command = tracing::field::Empty);
    let _enter = span.enter();
    let start = std::time::Instant::now();

    // EH-14: explicit warn on timeout failures rather than silent `.ok()` —
    // without a timeout a wedged client would pin the handler thread forever.
    //
    // P2 #41 (post-v1.27.0 audit): both timeouts now come from the shared
    // `cqs::daemon_translate::resolve_daemon_timeout_ms` helper so a user
    // raising `CQS_DAEMON_TIMEOUT_MS` raises both sides symmetrically. The
    // previously-hardcoded 5s/30s values were the source of the
    // TODO(cross-coordination) note in dispatch.rs.
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

    // RM-V1.29-10 (#1116): per-thread scratch buffer for the request line.
    // `handle_socket_client` runs on a Tokio blocking-pool thread that
    // services many connections in succession. Allocating a fresh `String`
    // (and its grow-during-`read_line` churn) per accept is ~80% of the
    // allocator pressure on this path under high-QPS agent workloads. The
    // 8 KiB initial capacity covers typical JSON requests in one allocation.
    thread_local! {
        static REQ_LINE: RefCell<String> = RefCell::new(String::with_capacity(8192));
    }

    // Read request (max 1MB). Wrap reader in .take() so allocation is
    // bounded *before* we accept a giant line — the post-hoc size check
    // below still fires if a client sends exactly the cap worth of data.
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(1_048_577);

    enum ParseOutcome {
        Ok(serde_json::Value),
        Empty,
        TooLarge,
        IoError(String),
        JsonError(String),
    }
    let parse_outcome = REQ_LINE.with_borrow_mut(|line| {
        line.clear();
        match std::io::BufRead::read_line(&mut reader, line) {
            Ok(0) => ParseOutcome::Empty,
            Ok(n) if n > 1_048_576 => ParseOutcome::TooLarge,
            Err(e) => ParseOutcome::IoError(e.to_string()),
            Ok(_) => match serde_json::from_str(line.trim()) {
                Ok(v) => ParseOutcome::Ok(v),
                Err(e) => ParseOutcome::JsonError(e.to_string()),
            },
        }
    });
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
    // P3 #86: structurally validate args. The previous `filter_map` silently
    // dropped non-string elements (numbers, nested arrays, nulls); the
    // surviving args looked correct but the daemon then ran a half-formed
    // command with no diagnostic. Now: collect the indices of non-string
    // elements, warn on them, and reject the whole request as malformed
    // rather than execute on a truncated arg list.
    let raw_args = request.get("args").and_then(|v| v.as_array());
    // P3.43: fold validation + extraction into a single walk. The previous
    // shape iterated the array twice — once to collect non-string indices,
    // once to extract strings. Single pass collects both results into the
    // typed `Vec<String>` and a parallel `Vec<usize>` of bad indices.
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
    // SEC-V1.25-16: `notes add`/`update`/`remove` carry the note body
    // as the first arg, which may contain source snippets or secrets.
    // Log only `notes/<subcommand>` so operators see the shape of
    // activity without the body reaching the journal.
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

    // P3 #138 (supersedes SEC-V1.25-9 / SEC-V1.25-16 / P2 #51 preview path):
    // drop `args_preview` from `tracing::debug!` entirely. The 80-char preview
    // still leaked file path fragments and search-query snippets at the daemon
    // debug level — privileged-journal harvest. The command name is already on
    // the span; `args_len` is enough for traffic shaping. If a finer-grained
    // preview is ever needed, gate it behind `tracing::trace!` (off by default
    // in production loggers).
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

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // PF-V1.29-1: Pass pre-split tokens straight to `dispatch_via_view`
        // instead of joining them back into a shell string for
        // `dispatch_line` to immediately re-split via `shell_words::split`.
        // The round-trip is pure waste on every daemon query and a latent
        // correctness bug on tokens containing shell metacharacters —
        // `shell_words::join` quotes them, `shell_words::split` unquotes
        // them, but any asymmetry silently corrupts the token boundary.
        let mut output = Vec::new();
        // #1127: hold the BatchContext mutex only long enough to snapshot
        // a `BatchView` (microseconds — clones a few `Arc`s under one
        // critical section). The handler then runs against the view
        // outside any BatchContext lock, so two slow queries (gather,
        // task) overlap on wall-clock. Refresh — the only daemon-
        // dispatchable command that mutates BatchContext interior — is
        // re-locked briefly inside `dispatch_via_view` via the view's
        // `outer_lock` back-channel. Poisoned mutex → recover the inner
        // ctx (the catch_unwind around this closure handles dispatch
        // panics; an unrelated poisoning could still slip in).
        let view = crate::cli::batch::checkout_view_from_arc(batch_ctx);
        crate::cli::batch::dispatch_via_view(&view, command, &args, &mut output);
        // P2 #62 (post-v1.27.0 audit, partial): the audit's full fix routes
        // dispatch through a `dispatch_value` sibling in `batch/mod.rs` that
        // returns `Value` directly. That refactor is owned by another agent
        // and is deferred for this wave; see TODO below.
        //
        // Partial win applied here: parse the dispatch bytes into a JSON
        // `Value` and embed it as a real JSON field of the response envelope
        // instead of round-tripping through `String::from_utf8` and embedding
        // as a string-in-string. This eliminates the per-byte JSON-escape
        // inflation that doubled large search responses on the wire.
        //
        // TODO(P2 #62 full fix): replace this parse with a `dispatch_value`
        // call once `batch/mod.rs` exposes it. The bytes-then-parse shape
        // here keeps the wire compatible while removing the escape pass.
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
    }));

    let (status, delivered) = match result {
        Ok(Ok(output_value)) => {
            let resp = serde_json::json!({
                "status": "ok",
                "output": output_value,
            });
            let delivered = match writeln!(stream, "{}", resp) {
                Ok(()) => true,
                Err(e) => {
                    tracing::debug!(error = %e, "Failed to write daemon response");
                    false
                }
            };
            ("ok", delivered)
        }
        Ok(Err(e)) => {
            let delivered = write_daemon_error_tracked(&mut stream, &e);
            ("client_error", delivered)
        }
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&'static str>().copied())
                .unwrap_or("<non-string panic payload>");
            let delivered =
                write_daemon_error_tracked(&mut stream, "internal error (panic in dispatch)");
            tracing::error!(
                panic_msg = %msg,
                "Daemon query panicked — daemon continues"
            );
            ("panic", delivered)
        }
    };

    tracing::info!(
        status,
        delivered,
        latency_ms = start.elapsed().as_millis() as u64,
        "Daemon query complete"
    );
}
pub(super) fn write_daemon_error(
    stream: &mut std::os::unix::net::UnixStream,
    message: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    let resp = serde_json::json!({ "status": "error", "message": message });
    writeln!(stream, "{}", resp)
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
