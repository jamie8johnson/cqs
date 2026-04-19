//! CLI-argv to batch-request translation and daemon-socket path helpers.
//!
//! Pure arg-shaping logic extracted from `cli::dispatch::try_daemon_query` so
//! integration tests (`tests/daemon_forward_test.rs`) can exercise it without
//! reaching into the binary-only `cli` module tree. See issue #972.
//!
//! Also exposes `daemon_socket_path` (the real function used by the CLI) so
//! tests can compute the exact socket path for a given `cqs_dir` and bind a
//! mock `UnixListener` there.
//!
//! The daemon speaks the same syntax as `cqs batch`: one JSON object per line
//! with `{"command": "<sub>", "args": [...]}`. The CLI args that reach
//! `try_daemon_query` are the raw argv post-`cqs` (i.e. `std::env::args()
//! .skip(1)`) — still mixed with global flags the top-level `Cli` struct
//! consumes (`--json`, `-q`, `--model VAL`) and sub-command flags that the
//! batch parser consumes on the subcommand side.
//!
//! Responsibilities:
//!
//! - Strip global boolean flags (`--json`, `-q`, `--quiet`) — they live on
//!   `Cli` not on the subcommand, and the batch handler always emits JSON.
//! - Strip global key/value flags (`--model VAL`). The daemon runs a single
//!   loaded model; reporting the stripped value is the caller's concern
//!   (see `stripped_model_value`).
//! - Remap `-n <N>` / `--limit <N>` to the canonical `--limit` form the
//!   batch parser expects on the subcommand. Also handles `-n=N` /
//!   `--limit=N` attached-value forms.
//! - Auto-prepend `search` for bare-query invocations (no subcommand, e.g.
//!   `cqs "hello world"`) so the batch parser can route them.
//!
//! Keeping the translation pure (no I/O, no tracing) makes the whole surface
//! unit-testable. The caller owns side effects (warning on stripped
//! `--model`, socket I/O, response parsing).

/// Translate raw CLI argv into a `(subcommand, args)` pair for the batch
/// handler, stripping global flags and normalising `-n`/`--limit`.
///
/// `raw` is the argv after `cqs` (i.e. what `std::env::args().skip(1)` yields).
/// `has_subcommand` is `true` iff `Cli::command` parsed a subcommand — i.e.
/// the first post-strip token is the subcommand name. When `false`, the input
/// is a bare query (e.g. `cqs "find something"`) and `search` is prepended.
///
/// Behaviour is pinned by `tests/daemon_forward_test.rs`. See the module
/// docs for the full stripping/remapping rules.
pub fn translate_cli_args_to_batch(raw: &[String], has_subcommand: bool) -> (String, Vec<String>) {
    // Boolean global flags: drop entirely (no value to track).
    const GLOBAL_FLAGS: &[&str] = &["--json", "-q", "--quiet"];
    // Key/value global flags (space-separated form): drop the flag AND its
    // value. `-n` and `--limit` are intercepted here to rewrite to the
    // canonical `--limit`.
    const GLOBAL_WITH_VALUE: &[&str] = &["--model", "-n", "--limit"];

    let mut args: Vec<String> = Vec::with_capacity(raw.len());
    let mut skip_next = false;
    for arg in raw.iter() {
        if skip_next {
            skip_next = false;
            continue;
        }
        // `--json` / `-q` / `--quiet` → drop.
        if GLOBAL_FLAGS.contains(&arg.as_str()) {
            continue;
        }
        // Attached-value forms (`-n=5`, `--limit=5`, `--model=foo`). Handle
        // here so we don't emit them verbatim and rely on the batch parser
        // to tolerate them. The spaced forms fall through to the next branch.
        if let Some((key, value)) = arg.split_once('=') {
            if key == "-n" || key == "--limit" {
                args.push(format!("--limit={}", value));
                continue;
            }
            if key == "--model" {
                // Strip `--model=VAL` entirely; caller handles the warning.
                continue;
            }
            if GLOBAL_FLAGS.contains(&key) {
                // Edge case: `--json=true` etc. — drop.
                continue;
            }
            // Non-global attached-value flag: pass through verbatim.
        }
        // Spaced-form global key/value flags.
        if GLOBAL_WITH_VALUE.contains(&arg.as_str()) {
            if arg == "-n" || arg == "--limit" {
                // Remap `-n 5` → `--limit 5`. The value is the next token and
                // is forwarded verbatim on the next iteration (skip_next
                // stays false). This mirrors the pre-extraction inline block.
                args.push("--limit".to_string());
                continue;
            }
            // `--model VAL` → strip both tokens. Caller uses
            // `stripped_model_value` to recover VAL for its warning.
            skip_next = true;
            continue;
        }
        args.push(arg.clone());
    }

    if !has_subcommand {
        // Bare query: `cqs "hello world"` → ("search", ["hello world"]).
        return ("search".to_string(), args);
    }
    // With a subcommand: the first surviving token is the subcommand name.
    if let Some((first, rest)) = args.split_first() {
        (first.clone(), rest.to_vec())
    } else {
        // Unreachable in practice: if clap parsed a subcommand the argv
        // contained its name. Defensive empty fallback.
        (String::new(), Vec::new())
    }
}

/// Resolve the daemon socket read/write timeout used on both the CLI client
/// side (`cli::dispatch::try_daemon_query`) and the daemon server side
/// (`cli::watch::handle_socket_client`).
///
/// SHL-V1.25-1 / SHL-V1.25-2 / P2 #41 (post-v1.27.0 audit): a single env knob
/// keeps the two surfaces symmetric. Previously the daemon hardcoded 5 s read
/// / 30 s write while the CLI honored `CQS_DAEMON_TIMEOUT_MS` — a user who
/// raised the CLI cap to allow a slow rerank would still hit the daemon's 30 s
/// write cap, getting a truncated/parse-error JSON line.
///
/// Honors `CQS_DAEMON_TIMEOUT_MS` (millisecond integer). Defaults to
/// 30 s; floors to 1 s so a misconfigured `=500` doesn't collapse to
/// `Duration::from_secs(0)` (which is unusable on UnixStream timeouts).
pub fn resolve_daemon_timeout_ms() -> std::time::Duration {
    std::time::Duration::from_millis(
        std::env::var("CQS_DAEMON_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(|ms| ms.max(1_000))
            .unwrap_or(30_000),
    )
}

/// Extract the value of `--model` from the raw argv, if present. Used by the
/// caller to emit a "daemon ignores your --model" warning without duplicating
/// the arg-scanning logic here. Supports both `--model VAL` and `--model=VAL`.
pub fn stripped_model_value(raw: &[String]) -> Option<String> {
    let mut it = raw.iter();
    while let Some(arg) = it.next() {
        if arg == "--model" {
            return it.next().cloned();
        }
        if let Some(rest) = arg.strip_prefix("--model=") {
            return Some(rest.to_string());
        }
    }
    None
}

/// Derive the daemon socket path for a given `cqs_dir`.
///
/// Mirrors `cli::files::daemon_socket_path` exactly. Exposed here so
/// integration tests can compute the path the CLI will try to connect to and
/// bind a mock listener there. The hash is collision-avoidance only (per-
/// project naming) — not a security property; access control relies on the
/// filesystem permissions the real daemon sets (0o600).
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::path::PathBuf;

    let sock_dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let sock_name = format!("cqs-{:x}.sock", {
        let mut h = DefaultHasher::new();
        cqs_dir.hash(&mut h);
        h.finish()
    });
    sock_dir.join(sock_name)
}

/// Daemon healthcheck response — the payload returned by `cqs ping`.
///
/// Task B2: makes the daemon's runtime state observable from the CLI without
/// having to grep `journalctl` for the right span. Exposed in the library
/// crate (rather than `cli::`) so the in-tree `tests/daemon_forward_test.rs`
/// integration tests and any sibling tooling (e.g. Task B1's
/// `cqs doctor --verbose`) can deserialize the same shape the daemon writes.
///
/// Wire format: serialized as the `output` field of the existing
/// `{"status":"ok","output":<json>}` daemon envelope. The daemon special-cases
/// `command == "ping"` to emit a JSON object matching this struct rather than
/// going through the batch dispatcher's free-form value path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PingResponse {
    /// Embedding model the daemon currently has loaded (e.g.
    /// `"BAAI/bge-large-en-v1.5"`). Comes from `BatchContext::model_config.name`.
    pub model: String,
    /// Embedding dimensionality the daemon reports for that model. Sourced
    /// from the resolved `ModelConfig::dim` so it reflects any
    /// `CQS_EMBEDDING_DIM` override at daemon startup.
    pub dim: u32,
    /// Seconds since the daemon's `BatchContext` was created. Approximates
    /// "how long has the daemon been serving" — process uptime if the daemon
    /// has been up since boot, otherwise time since last restart.
    pub uptime_secs: u64,
    /// Unix timestamp (UTC seconds) of the last write to `index.db`, or
    /// `None` if the file is missing/unreadable. Best-effort proxy for
    /// "when did the index last change" — reflects both `cqs index` and
    /// incremental `cqs watch` updates because both touch the DB file.
    pub last_indexed_at: Option<i64>,
    /// Cumulative count of dispatch errors observed by the daemon since
    /// the `BatchContext` was created. Includes parse failures and handler
    /// errors; transport-level failures (closed sockets) are not counted.
    pub error_count: u64,
    /// Cumulative count of socket queries the daemon has dispatched since
    /// the `BatchContext` was created. Counts every line that reached
    /// `dispatch_line` whether it succeeded or errored.
    pub total_queries: u64,
    /// Whether the SPLADE encoder ONNX session is currently resident.
    /// Lazy-loaded on first sparse-needing query; once loaded it stays
    /// until idle eviction. `false` means the daemon hasn't run a query
    /// that needed sparse retrieval yet, or the SPLADE model isn't
    /// configured at all.
    pub splade_loaded: bool,
    /// Whether the cross-encoder reranker ONNX session is currently
    /// resident. Same lazy-load semantics as `splade_loaded`.
    pub reranker_loaded: bool,
}

/// Connect to the running daemon and request a `PingResponse`.
///
/// Task B2 helper: returns `Ok(PingResponse)` on success, `Err` if the
/// socket is missing, the connection fails, the daemon returns a non-`ok`
/// status, or the response payload doesn't deserialize. Errors are explicit
/// strings the caller can present verbatim.
///
/// Reusable from Task B1 (`cqs doctor --verbose`) and from any future tool
/// that wants to ask the daemon "are you alive and serving the right
/// model?". Stays in the library crate (not `cli::`) so non-binary callers
/// can use it.
///
/// Note: this function is unix-only because the daemon socket is unix-only.
/// On non-unix platforms the daemon never starts in the first place.
#[cfg(unix)]
pub fn daemon_ping(cqs_dir: &std::path::Path) -> Result<PingResponse, String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock_path = daemon_socket_path(cqs_dir);
    // P3 #99: span ties every warn below to the same socket path so a
    // multi-project agent loop can disambiguate which daemon failed.
    let _span = tracing::info_span!("daemon_ping", path = %sock_path.display()).entered();
    if !sock_path.exists() {
        return Err(format!(
            "no daemon running (socket {} does not exist)",
            sock_path.display()
        ));
    }

    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        tracing::warn!(stage = "connect", error = %e, "daemon_ping failed");
        format!("connect to {} failed: {e}", sock_path.display())
    })?;

    // 5s is generous: the ping handler does no I/O — just snapshot reads
    // off atomic counters and a single `metadata()` on `index.db`.
    let timeout = Duration::from_secs(5);
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::warn!(stage = "set_read_timeout", error = %e, "daemon_ping failed");
        return Err(format!("set_read_timeout failed: {e}"));
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::warn!(stage = "set_write_timeout", error = %e, "daemon_ping failed");
        return Err(format!("set_write_timeout failed: {e}"));
    }

    let request = serde_json::json!({"command": "ping", "args": []});
    writeln!(stream, "{}", request).map_err(|e| {
        tracing::warn!(stage = "write", error = %e, "daemon_ping failed");
        format!("write request failed: {e}")
    })?;
    stream.flush().map_err(|e| {
        tracing::warn!(stage = "flush", error = %e, "daemon_ping failed");
        format!("flush failed: {e}")
    })?;

    // PingResponse is small (<1KB). Cap the read at 64KB to bound memory
    // even if a buggy daemon ever writes a huge response — same defensive
    // posture as the main `try_daemon_query` 16 MiB cap.
    use std::io::Read as _;
    let mut reader = std::io::BufReader::new(&stream).take(64 * 1024);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).map_err(|e| {
        tracing::warn!(stage = "read", error = %e, "daemon_ping failed");
        format!("read response failed: {e}")
    })?;

    let envelope: serde_json::Value = serde_json::from_str(response_line.trim()).map_err(|e| {
        tracing::warn!(stage = "parse", error = %e, "daemon_ping failed");
        format!("parse envelope failed: {e}")
    })?;

    let status = envelope
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::warn!(stage = "parse", "daemon_ping failed: missing status field");
            "missing 'status' field in daemon response".to_string()
        })?;
    if status != "ok" {
        let msg = envelope
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("daemon error");
        tracing::warn!(
            stage = "parse",
            status,
            msg,
            "daemon_ping failed: non-ok status"
        );
        return Err(format!("daemon error: {msg}"));
    }

    let output = envelope.get("output").ok_or_else(|| {
        tracing::warn!(stage = "parse", "daemon_ping failed: missing output field");
        "missing 'output' field in daemon response".to_string()
    })?;
    // The daemon writes the PingResponse as a JSON-string-encoded payload
    // (because the existing envelope is `{status, output: string}`). When
    // `output` is a string, parse it; when it's already an object (future-
    // proofing if the envelope is ever changed), accept that too.
    let payload: serde_json::Value = match output {
        serde_json::Value::String(s) => {
            serde_json::from_str(s).map_err(|e| format!("parse output JSON failed: {e}"))?
        }
        other => other.clone(),
    };
    serde_json::from_value(payload).map_err(|e| format!("PingResponse deserialize failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|s| s.to_string()).collect()
    }

    // P2 #41: env-var-driven timeout helper used by both daemon and CLI sides.
    // Test only the unset path; mutating env vars in a parallel test runner
    // is fragile, and the floor / honor logic is small enough to inspect.
    #[test]
    fn resolve_daemon_timeout_default_is_30s() {
        if std::env::var("CQS_DAEMON_TIMEOUT_MS").is_ok() {
            return;
        }
        assert_eq!(
            resolve_daemon_timeout_ms(),
            std::time::Duration::from_secs(30)
        );
    }

    // Sanity parity with the inline block that was removed from
    // `try_daemon_query`. The full black-box suite lives in
    // `tests/daemon_forward_test.rs`.

    #[test]
    fn strips_json_bare_query() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["--json", "search me"]), false);
        assert_eq!(cmd, "search");
        assert_eq!(args, v(&["search me"]));
    }

    #[test]
    fn remaps_dash_n_spaced() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["impact", "foo", "-n", "5"]), true);
        assert_eq!(cmd, "impact");
        assert_eq!(args, v(&["foo", "--limit", "5"]));
    }

    #[test]
    fn stripped_model_value_spaced_form() {
        assert_eq!(
            stripped_model_value(&v(&["search", "q", "--model", "bge-large"])),
            Some("bge-large".to_string())
        );
    }

    #[test]
    fn stripped_model_value_equals_form() {
        assert_eq!(
            stripped_model_value(&v(&["search", "q", "--model=bge-large"])),
            Some("bge-large".to_string())
        );
    }

    /// Task B2: smoke-test PingResponse round-trips through serde without
    /// schema drift. Pins the wire shape so a future field rename here
    /// doesn't silently break the CLI<->daemon contract.
    #[test]
    fn ping_response_roundtrip_serde() {
        let original = PingResponse {
            model: "BAAI/bge-large-en-v1.5".to_string(),
            dim: 1024,
            uptime_secs: 9_375,
            last_indexed_at: Some(1_734_120_000),
            error_count: 3,
            total_queries: 12_453,
            splade_loaded: true,
            reranker_loaded: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        // Spot-check field names: a typo here would be a wire-break.
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"dim\""));
        assert!(json.contains("\"uptime_secs\""));
        assert!(json.contains("\"last_indexed_at\""));
        assert!(json.contains("\"error_count\""));
        assert!(json.contains("\"total_queries\""));
        assert!(json.contains("\"splade_loaded\""));
        assert!(json.contains("\"reranker_loaded\""));
        let parsed: PingResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    /// Task B2: PingResponse with no last-indexed timestamp serializes as
    /// `"last_indexed_at": null` and round-trips. Pins the Option<i64>
    /// behaviour so the CLI doesn't have to special-case missing-field
    /// vs. null vs. -1 sentinel.
    #[test]
    fn ping_response_null_last_indexed() {
        let original = PingResponse {
            model: "test".into(),
            dim: 1,
            uptime_secs: 0,
            last_indexed_at: None,
            error_count: 0,
            total_queries: 0,
            splade_loaded: false,
            reranker_loaded: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        assert!(
            json.contains("\"last_indexed_at\":null"),
            "Option::None must serialize as JSON null, got {json}"
        );
        let parsed: PingResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.last_indexed_at, None);
    }

    /// Task B2: `daemon_ping` must surface a friendly error (not a panic
    /// or generic IO message) when the socket file is absent. This is the
    /// primary failure mode the CLI hits when no daemon is running.
    #[cfg(unix)]
    #[test]
    fn daemon_ping_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        // No XDG_RUNTIME_DIR override here — the helper hashes the cqs_dir
        // path, so even if there's a real daemon socket somewhere the
        // hashed name for this temp dir won't collide.
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let result = daemon_ping(&cqs_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("no daemon running"),
            "expected friendly error, got: {err}"
        );
    }

    /// Task B2: `daemon_ping` must round-trip through a mock listener that
    /// speaks the same envelope as the real daemon. Asserts the field-by-
    /// field decoding so a future drift in either side surfaces here.
    #[cfg(unix)]
    #[test]
    fn daemon_ping_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // Override XDG_RUNTIME_DIR so the socket lives in our temp dir.
        // Snapshot the prior value to restore at the end (other tests
        // may rely on the inherited environment).
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: tests run sequentially within a process; this is only
        // racy against parallel test workers, but the worst case is a
        // sibling test computing a different socket path — not data
        // corruption. The temp_dir itself is unique per test.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        }

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Mock daemon thread: accept one connection, drain the request
        // line (just to confirm we got the expected JSON), reply with a
        // canned `{"status":"ok","output":"<PingResponse JSON string>"}`.
        let response_payload = serde_json::json!({
            "model": "BAAI/bge-large-en-v1.5",
            "dim": 1024,
            "uptime_secs": 60,
            "last_indexed_at": 1_734_120_000_i64,
            "error_count": 2,
            "total_queries": 100,
            "splade_loaded": true,
            "reranker_loaded": false,
        })
        .to_string();
        let envelope = serde_json::json!({
            "status": "ok",
            "output": response_payload,
        });
        let envelope_str = envelope.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            // Sanity: confirm the helper actually sent a ping request.
            assert!(
                request_line.contains("\"command\":\"ping\""),
                "expected ping request, got: {request_line}"
            );
            writeln!(stream, "{envelope_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_ping(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        // Restore prior XDG_RUNTIME_DIR before the assertion so a
        // failure doesn't leak the temp-dir override into subsequent
        // tests in the same process.
        // SAFETY: see above.
        unsafe {
            match prev_xdg {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }

        let resp = result.expect("daemon_ping should succeed against mock");
        assert_eq!(resp.model, "BAAI/bge-large-en-v1.5");
        assert_eq!(resp.dim, 1024);
        assert_eq!(resp.uptime_secs, 60);
        assert_eq!(resp.last_indexed_at, Some(1_734_120_000));
        assert_eq!(resp.error_count, 2);
        assert_eq!(resp.total_queries, 100);
        assert!(resp.splade_loaded);
        assert!(!resp.reranker_loaded);
    }
}
