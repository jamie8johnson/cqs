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
        let cmd = first.clone();
        let mut tail = rest.to_vec();
        // `cqs notes list ...` → daemon `notes ...`. The CLI's `Notes`
        // command takes a NotesCommand subcommand enum (`list`/`add`/
        // `update`/`remove`) but the batch dispatcher only ever receives
        // `list` — `add`/`update`/`remove` route through `BatchSupport::Cli`
        // and never hit the daemon. The batch parser's `BatchCmd::Notes`
        // accepts `--warnings`/`--patterns` directly (no `list` token), so
        // we strip the redundant subcommand name here. Without this strip
        // every `cqs notes list` query through the daemon errors with
        // `unexpected argument 'list' found`.
        if cmd == "notes" && tail.first().map(|s| s.as_str()) == Some("list") {
            tail.remove(0);
        }
        (cmd, tail)
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
///
/// AC-V1.30.1-9: hashes via [`blake3`] rather than `std::collections::hash_map::DefaultHasher`.
/// `DefaultHasher` is Rust-version-dependent SipHash; a `cargo update` of
/// std could change socket names and break systemd `cqs-watch` units that
/// hardcode a specific path. BLAKE3 is stable across Rust versions and
/// truncating the digest to 8 bytes keeps the socket name short while
/// staying collision-safe (~1e-15 for 100 projects). Wire-format change:
/// operators upgrading from <v1.30.1 must `systemctl --user restart cqs-watch`
/// once so the daemon binds the new socket name; CLI auto-discovers via
/// `XDG_RUNTIME_DIR` thereafter.
#[cfg(unix)]
pub fn daemon_socket_path(cqs_dir: &std::path::Path) -> std::path::PathBuf {
    use std::path::PathBuf;

    // Thread-local override (test-only) takes precedence so test fixtures
    // can redirect the socket without `unsafe std::env::set_var`. None in
    // production. See SOCKET_DIR_OVERRIDE.
    let override_dir =
        SOCKET_DIR_OVERRIDE.with(|cell| cell.borrow().as_ref().map(|p| p.to_path_buf()));

    let sock_dir = match override_dir {
        Some(d) => d,
        None => match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => PathBuf::from(d),
            None => {
                // P3.38: silent fallback to /tmp (mode 1777) is fine on macOS
                // (`/var/folders/.../T/` is per-user) but a meaningful trust
                // drop on multi-user Linux. Surface it once so operators can
                // wire `XDG_RUNTIME_DIR=/run/user/$(id -u)` if they care.
                #[cfg(target_os = "linux")]
                {
                    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                    WARNED.get_or_init(|| {
                        tracing::info!(
                            "XDG_RUNTIME_DIR unset — daemon socket falls back to temp_dir; \
                             consider XDG_RUNTIME_DIR=/run/user/$(id -u)"
                        );
                    });
                }
                std::env::temp_dir()
            }
        },
    };
    // AC-V1.30.1-9: BLAKE3 is stable across Rust versions — important
    // because systemd unit files and operator scripts encode the socket
    // path. Truncate to 8 hex bytes (16 chars) — collision probability
    // for 100 projects is ~1e-15.
    let canonical_path_bytes = cqs_dir.as_os_str().as_encoded_bytes();
    let hash = blake3::hash(canonical_path_bytes);
    let truncated = &hash.as_bytes()[..8];
    let mut hex = String::with_capacity(16);
    for b in truncated {
        use std::fmt::Write as _;
        let _ = write!(hex, "{:02x}", b);
    }
    let sock_name = format!("cqs-{}.sock", hex);
    sock_dir.join(sock_name)
}

// Thread-local override for the socket directory. Production paths leave
// this `None` and `daemon_socket_path` reads `XDG_RUNTIME_DIR` as before;
// tests set it via `set_socket_dir_override_for_test` to redirect the
// socket under a per-test tempdir without touching process-wide env vars.
//
// Why thread-local rather than `unsafe std::env::set_var`: setenv races
// with concurrent env reads from any other thread, deadlocking libc's
// env mutex on parallel test runners (#1292). A thread-local override
// has no cross-thread state and lets the daemon round-trip tests run
// fully parallel.
#[cfg(unix)]
std::thread_local! {
    static SOCKET_DIR_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only hook to redirect the daemon socket directory on the current
/// thread. Pass `None` to clear. See [`SOCKET_DIR_OVERRIDE`] for the
/// rationale; replaces the previous `unsafe { std::env::set_var(
/// "XDG_RUNTIME_DIR", ...) }` pattern.
///
/// `#[doc(hidden)]` because it's strictly a test utility — the symbol is
/// `pub` so integration tests can reach it but it's not part of the
/// public API.
#[cfg(unix)]
#[doc(hidden)]
pub fn set_socket_dir_override_for_test(dir: Option<std::path::PathBuf>) {
    SOCKET_DIR_OVERRIDE.with(|cell| *cell.borrow_mut() = dir);
}

/// Typed error returned by [`daemon_ping`], [`daemon_status`], and
/// [`daemon_reconcile`].
///
/// API-V1.30.1-5: replaces the previous `Result<T, String>` shape so
/// callers can distinguish "daemon never ran" (socket file missing) from
/// "daemon crashed mid-call" (transport failure) from "daemon answered
/// with garbage" (envelope/JSON parse failure) from "daemon answered
/// with `status: \"err\"`" (handler-level error). The `wait_for_fresh`
/// hot path branches on the variant to produce different bail messages
/// per failure mode rather than collapsing every failure into "no
/// daemon — start one" advice.
#[cfg(unix)]
#[derive(Debug, Clone, thiserror::Error)]
pub enum DaemonRpcError {
    /// The daemon socket file does not exist — the daemon never started
    /// (or was stopped). Operator action: start `cqs watch --serve`.
    #[error("daemon socket missing: {0}")]
    SocketMissing(String),
    /// Connect / read / write / timeout / set_*_timeout failure. The
    /// socket file exists but the daemon isn't responding. Operator
    /// action: check `journalctl --user -u cqs-watch` and consider
    /// restarting the unit.
    #[error("daemon transport failure: {0}")]
    Transport(String),
    /// Envelope JSON parse, missing/non-string `status` field, or the
    /// dispatch payload deserialize failed. The daemon answered but the
    /// response is unparseable — most often a CLI/daemon version skew.
    /// Operator action: rebuild and restart `cqs-watch`.
    #[error("daemon returned malformed response: {0}")]
    BadResponse(String),
    /// The daemon returned `{"status": "err", "message": "..."}` — a
    /// handler-level error. The `message` is the daemon's own description.
    #[error("daemon error: {0}")]
    DaemonError(String),
}

#[cfg(unix)]
impl DaemonRpcError {
    /// Render the daemon error as a stable wire-format string for
    /// callers that still want to surface it as plain text (eg. the
    /// `cqs hook fire` fallback that touches `.cqs/.dirty`). Equivalent
    /// to the previous `Err(String)` payload.
    pub fn as_message(&self) -> String {
        self.to_string()
    }
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
    ///
    /// API-V1.30.1-4: accepts `"last_synced_at"` as an alias on
    /// deserialization so a consumer reading the
    /// [`crate::watch_status::WatchSnapshot`] field name in docs can
    /// hand the same JSON to `serde::from_value::<PingResponse>` and
    /// have it deserialize. Canonical name in serialization stays
    /// `last_indexed_at` — the alias is read-only.
    #[serde(alias = "last_synced_at")]
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
/// Task B2 helper: returns `Ok(PingResponse)` on success or a typed
/// [`DaemonRpcError`] on failure (socket missing, transport/connection
/// failure, malformed response, or daemon-side error).
///
/// Reusable from Task B1 (`cqs doctor --verbose`) and from any future tool
/// that wants to ask the daemon "are you alive and serving the right
/// model?". Stays in the library crate (not `cli::`) so non-binary callers
/// can use it.
///
/// Note: this function is unix-only because the daemon socket is unix-only.
/// On non-unix platforms the daemon never starts in the first place.
#[cfg(unix)]
pub fn daemon_ping(cqs_dir: &std::path::Path) -> Result<PingResponse, DaemonRpcError> {
    daemon_request(cqs_dir, "ping", serde_json::json!([]), "PingResponse")
}

/// Generic socket round-trip helper for daemon RPCs that follow the
/// `{"status":"ok","output":<dispatch envelope>}` wire shape. Centralises
/// the connect → set_timeouts → write → read → parse → unwrap_dispatch
/// pipeline so [`daemon_ping`], [`daemon_status`], [`daemon_reconcile`],
/// and any future RPC sit on the same well-tested transport (#1215).
///
/// Inputs:
/// - `command`: the wire-level command name (`"ping"`, `"status"`, etc.)
/// - `args`: JSON value used as the request's `args` field. `json!([])`
///   for arg-less RPCs; `json!(["--hook", "post-checkout", ...])` for
///   the `reconcile` arg-vector shape.
/// - `payload_label`: type name used in error messages and the
///   `unwrap_dispatch_payload` warn arm. Conventionally the deserialised
///   type's bare name (`"PingResponse"`, `"WatchSnapshot"`).
///
/// All connect / set_timeout / read / write failures emit a
/// `tracing::debug!(stage=…, command=…)` line so a multi-project agent
/// loop polling several daemons can correlate by `command` field. Parse
/// / non-ok-status failures emit `tracing::warn!` because they signal a
/// real wire-format break, not a transient socket condition. The 5s
/// timeout and 64 KiB read cap match what every individual RPC carried
/// before the extraction.
///
/// `T` must implement `serde::Deserialize` for the inner payload (the
/// `"data"` field of the dispatch envelope, or the bare value in the
/// legacy mock-test path — see [`unwrap_dispatch_payload`]).
#[cfg(unix)]
fn daemon_request<T: serde::de::DeserializeOwned>(
    cqs_dir: &std::path::Path,
    command: &str,
    args: serde_json::Value,
    payload_label: &str,
) -> Result<T, DaemonRpcError> {
    use std::io::{BufRead, Read as _, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let sock_path = daemon_socket_path(cqs_dir);
    // P3 #99 / #1215: single span for every daemon RPC, with the wire
    // `command` as a structured field so a multi-project loop can grep
    // by command without per-function span proliferation.
    let _span = tracing::info_span!(
        "daemon_request",
        command = command,
        path = %sock_path.display()
    )
    .entered();
    if !sock_path.exists() {
        return Err(DaemonRpcError::SocketMissing(format!(
            "no daemon running (socket {} does not exist)",
            sock_path.display()
        )));
    }

    let mut stream = UnixStream::connect(&sock_path).map_err(|e| {
        // OB-V1.30.1-8: connect failures are debug, not warn. The
        // `wait_for_fresh` polling loop hits this path repeatedly during
        // daemon startup; warn-level here would flood journalctl.
        // Final-decision warns live in `wait_for_fresh` / the eval gate.
        tracing::debug!(stage = "connect", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("connect to {} failed: {e}", sock_path.display()))
    })?;

    // 5s is generous: every daemon RPC's handler does at most a single
    // RwLock read + clone + a `metadata()` syscall. No real I/O on the
    // daemon side.
    let timeout = Duration::from_secs(5);
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        tracing::debug!(stage = "set_read_timeout", error = %e, command, "daemon request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_read_timeout failed: {e}"
        )));
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        tracing::debug!(stage = "set_write_timeout", error = %e, command, "daemon request failed");
        return Err(DaemonRpcError::Transport(format!(
            "set_write_timeout failed: {e}"
        )));
    }

    let request = serde_json::json!({"command": command, "args": args});
    writeln!(stream, "{}", request).map_err(|e| {
        tracing::debug!(stage = "write", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("write request failed: {e}"))
    })?;
    stream.flush().map_err(|e| {
        tracing::debug!(stage = "flush", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("flush failed: {e}"))
    })?;

    // 64 KiB matches the per-call cap each RPC used pre-extraction.
    // Responses are small (<1 KB for ping, ~few KB for status); the cap
    // is a defensive ceiling against a buggy daemon, not a sizing knob.
    let mut reader = std::io::BufReader::new(&stream).take(64 * 1024);
    let mut response_line = String::new();
    reader.read_line(&mut response_line).map_err(|e| {
        tracing::debug!(stage = "read", error = %e, command, "daemon request failed");
        DaemonRpcError::Transport(format!("read response failed: {e}"))
    })?;

    let envelope: serde_json::Value = serde_json::from_str(response_line.trim()).map_err(|e| {
        tracing::warn!(stage = "parse", error = %e, command, "daemon request failed");
        DaemonRpcError::BadResponse(format!("parse envelope failed: {e}"))
    })?;

    let status = envelope
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::warn!(
                stage = "parse",
                command,
                "daemon request failed: missing status field"
            );
            DaemonRpcError::BadResponse("missing 'status' field in daemon response".to_string())
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
            command,
            "daemon request failed: non-ok status"
        );
        return Err(DaemonRpcError::DaemonError(msg.to_string()));
    }

    let output = envelope.get("output").ok_or_else(|| {
        tracing::warn!(
            stage = "parse",
            command,
            "daemon request failed: missing output field"
        );
        DaemonRpcError::BadResponse("missing 'output' field in daemon response".to_string())
    })?;
    let payload =
        unwrap_dispatch_payload(output, payload_label).map_err(DaemonRpcError::BadResponse)?;
    serde_json::from_value::<T>(payload).map_err(|e| {
        DaemonRpcError::BadResponse(format!("{payload_label} deserialize failed: {e}"))
    })
}

/// Pull the inner handler payload out of a daemon dispatch response.
///
/// The wire chain looks like:
///
/// ```text
/// outer:    {"status":"ok","output":<inner>}        // socket layer
/// inner:    {"data":<payload>,"error":null,"version":N,"_meta":{...}}
///           // batch dispatch envelope (write_json_line)
/// ```
///
/// `<inner>` may arrive in two forms:
///
/// 1. **Object form** (production daemon socket): `output` parses to a
///    JSON object — the dispatch envelope. The handler payload is at
///    `output.data`.
/// 2. **String form** (some integration test mocks, and any caller that
///    re-wraps the dispatch bytes verbatim): `output` is a JSON string
///    that needs a second `from_str` to reach the inner shape.
///
/// We accept both forms transparently. If neither produces a `data`
/// field — i.e. the value is already the bare handler payload — return
/// it as-is so the legacy mock-test path keeps working.
///
/// Pre-existing bug fix: prior to #1182 this function lived inline in
/// `daemon_ping` without the `data` extraction, which silently broke
/// production `cqs ping` (envelope deserialized as PingResponse →
/// "missing field `model`"). Hoisting it lets the new
/// [`daemon_status`] helper reuse the same well-tested unwrap.
fn unwrap_dispatch_payload(
    output: &serde_json::Value,
    type_name: &str,
) -> Result<serde_json::Value, String> {
    let parsed: serde_json::Value = match output {
        serde_json::Value::String(s) => serde_json::from_str(s).map_err(|e| {
            tracing::warn!(stage = "parse", error = %e, type_name, "daemon dispatch output JSON parse failed");
            format!("parse {type_name} output JSON failed: {e}")
        })?,
        other => other.clone(),
    };
    // If the inner shape is the dispatch envelope, dig out `data`.
    // Otherwise pass through (legacy bare-payload mock form).
    match parsed.as_object().and_then(|m| m.get("data")) {
        Some(data) => Ok(data.clone()),
        None => Ok(parsed),
    }
}

/// #1182: connect to the running daemon and request a [`WatchSnapshot`].
///
/// Mirrors [`daemon_ping`] in shape (same envelope, same string-payload
/// transport) but issues the `status` command and deserializes a
/// [`WatchSnapshot`]. The daemon path is chosen because that's where the
/// watch loop publishes from — the CLI-only path returns a default
/// `unknown` snapshot, which would defeat the purpose.
///
/// Returns a typed [`DaemonRpcError`] on failure. Callers can fall back
/// or surface verbatim. The connect-stage `tracing::warn!` of the prior
/// implementation was demoted to `tracing::debug!` because [`wait_for_fresh`]
/// polls this in a tight loop during startup and the warn-cadence floods
/// the journal at info level (OB-V1.30.1-8). Final-decision warns live
/// in [`wait_for_fresh`] / the eval gate.
///
/// Unix-only: the daemon socket is unix-only.
///
/// [`WatchSnapshot`]: crate::watch_status::WatchSnapshot
#[cfg(unix)]
pub fn daemon_status(
    cqs_dir: &std::path::Path,
) -> Result<crate::watch_status::WatchSnapshot, DaemonRpcError> {
    daemon_request(cqs_dir, "status", serde_json::json!([]), "WatchSnapshot")
}

/// #1182 — Layer 1: response shape for [`daemon_reconcile`]. Mirrors the
/// JSON envelope `dispatch_reconcile` returns: a confirmation that the
/// signal was accepted plus the advisory hook metadata.
///
/// API-V1.30.1-6: the legacy `queued: bool` field was always `true`
/// (the dispatch handler sets it unconditionally) so it conveyed nothing
/// the `Ok(...)` envelope didn't already imply. Dropped; JSON consumers
/// who relied on the literal field should switch to "did `daemon_reconcile`
/// return Ok?" — the same signal, surfaced via Result.
#[cfg(unix)]
#[derive(Debug, Clone, serde::Deserialize)]
pub struct DaemonReconcileResponse {
    /// `true` if a previous request was still pending when this call
    /// arrived. Surfaces coalescing for hook-burst scenarios (rebase).
    pub was_pending: bool,
    /// Echoed advisory fields. Useful for tracing in the hook script's
    /// stderr.
    pub hook: Option<String>,
    pub args: Vec<String>,
}

/// #1182 — Layer 1: post a `reconcile` socket message to the running
/// daemon. Used by the `cqs hook fire` CLI surface.
///
/// Returns the parsed [`DaemonReconcileResponse`] on success, or a typed
/// [`DaemonRpcError`] on failure. The CLI surface downgrades the
/// `SocketMissing` variant to the `.cqs/.dirty` fallback; every other
/// variant is surfaced verbatim so hooks fail loudly.
#[cfg(unix)]
pub fn daemon_reconcile(
    cqs_dir: &std::path::Path,
    hook: Option<&str>,
    args: &[String],
) -> Result<DaemonReconcileResponse, DaemonRpcError> {
    // Build the batch-arg vector: ["--hook", "<name>", "--arg", "v1",
    // "--arg", "v2", ...]. The batch parser accepts `--arg` repeated
    // for `Vec<String>` fields.
    let mut batch_args: Vec<String> = Vec::with_capacity(2 + 2 * args.len());
    if let Some(name) = hook {
        batch_args.push("--hook".to_string());
        batch_args.push(name.to_string());
    }
    for a in args {
        batch_args.push("--arg".to_string());
        batch_args.push(a.clone());
    }
    daemon_request(
        cqs_dir,
        "reconcile",
        serde_json::json!(batch_args),
        "DaemonReconcileResponse",
    )
}

/// #1182 — Layer 4: outcome of [`wait_for_fresh`].
///
/// Five cases callers need to distinguish so the caller-side advice
/// matches the actual failure mode (EH-V1.30.1-2):
/// - `Fresh` — daemon reported `state == fresh` within the budget; safe to
///   proceed with the gated work.
/// - `Timeout` — daemon was reachable but never became fresh in time. The
///   final snapshot is attached so callers can surface counters
///   (`modified_files`, `pending_notes`) in their error message.
/// - `NoDaemon` — socket file does not exist. Operator action: start
///   `cqs watch --serve`.
/// - `Transport` — socket exists but the daemon isn't responding (connect /
///   read / write / timeout). Operator action: check the daemon log,
///   consider restarting the unit.
/// - `BadResponse` — daemon answered but the response was unparseable.
///   Most often a CLI/daemon version skew — restart `cqs-watch`.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub enum FreshnessWait {
    Fresh(crate::watch_status::WatchSnapshot),
    Timeout(crate::watch_status::WatchSnapshot),
    /// Socket file missing — the daemon never started.
    NoDaemon(String),
    /// Connect/read/write/timeout — daemon is gone or hung.
    Transport(String),
    /// Envelope/JSON/parse error — daemon answered but garbled.
    BadResponse(String),
}

/// #1182 — Layer 4: shared client-side polling for `state == fresh`.
///
/// Both `cqs status --watch-fresh --wait` and `cqs eval --require-fresh`
/// route through this. Polls the daemon with exponential backoff (initial
/// interval from [`crate::limits::freshness_poll_ms_initial`] doubling up
/// to a 2 s ceiling) within a deadline of `wait_secs`. RB-2 caps the
/// budget at 86,400 s (24 h) defensively so a misconfigured caller can't
/// pass a value that overflows `Instant + Duration::from_secs`.
///
/// The first successful poll that reports `fresh` returns immediately —
/// callers don't pay any latency on an already-fresh tree.
///
/// EH-V1.30.1-2: errors are surfaced per [`DaemonRpcError`] variant rather
/// than collapsed into a single `NoDaemon` so the caller-side advice can
/// distinguish "no daemon at all" (start one), "daemon hung" (restart),
/// and "daemon answered with garbage" (version skew → restart).
///
/// OB-V1.30.1-8: the connect-stage `tracing::warn!` was demoted to debug
/// at the [`daemon_status`] layer, so a 250 ms poll loop no longer floods
/// journalctl during startup. Final-decision warns are emitted here, one
/// per terminal outcome.
#[cfg(unix)]
pub fn wait_for_fresh(cqs_dir: &std::path::Path, wait_secs: u64) -> FreshnessWait {
    let _span = tracing::info_span!("wait_for_fresh", wait_secs).entered();
    let start = std::time::Instant::now();
    // RB-2: defensive cap so a `pub fn` can't panic on
    // `Instant + Duration::from_secs(u64::MAX)`. Caller should pass a
    // sane budget but we bound it here regardless.
    let bounded_secs = wait_secs.min(86_400);
    let deadline = start + std::time::Duration::from_secs(bounded_secs);

    let mut poll_interval =
        std::time::Duration::from_millis(crate::limits::freshness_poll_ms_initial());
    let max_interval = std::time::Duration::from_secs(2);

    loop {
        // RB-9: deadline-first so a slow daemon timeout can't push us
        // past the user's budget. If the deadline already passed before
        // we got our first response, return Timeout with the unknown
        // snapshot — the caller's bail message will explain.
        if std::time::Instant::now() >= deadline {
            tracing::info!(
                elapsed_ms = start.elapsed().as_millis() as u64,
                "wait_for_fresh: deadline reached before first fresh poll",
            );
            return FreshnessWait::Timeout(crate::watch_status::WatchSnapshot::unknown());
        }

        match daemon_status(cqs_dir) {
            Ok(snap) => {
                if snap.is_fresh() {
                    tracing::info!(
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        modified_files = snap.modified_files,
                        "wait_for_fresh: index reached Fresh",
                    );
                    return FreshnessWait::Fresh(snap);
                }
                if std::time::Instant::now() >= deadline {
                    tracing::info!(
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        modified_files = snap.modified_files,
                        pending_notes = snap.pending_notes,
                        rebuild_in_flight = snap.rebuild_in_flight,
                        "wait_for_fresh: timeout — index still stale",
                    );
                    return FreshnessWait::Timeout(snap);
                }
                std::thread::sleep(poll_interval);
                // Exponential backoff with a 2 s ceiling. Linearly
                // doubling beats fixed cadence on long waits because the
                // socket budget cost is per round-trip, not per second.
                poll_interval = (poll_interval * 2).min(max_interval);
            }
            Err(DaemonRpcError::SocketMissing(msg)) => {
                tracing::info!(error = %msg, "wait_for_fresh: daemon socket missing");
                return FreshnessWait::NoDaemon(msg);
            }
            Err(DaemonRpcError::Transport(msg)) => {
                tracing::info!(error = %msg, "wait_for_fresh: transport failure");
                return FreshnessWait::Transport(msg);
            }
            Err(DaemonRpcError::BadResponse(msg)) => {
                tracing::warn!(error = %msg, "wait_for_fresh: malformed daemon response");
                return FreshnessWait::BadResponse(msg);
            }
            Err(DaemonRpcError::DaemonError(msg)) => {
                // The daemon answered with `status: "err"`. From the
                // freshness-poll perspective this is the daemon refusing
                // to provide a snapshot — surface as transport-class so
                // the caller's message points at the daemon log.
                tracing::warn!(error = %msg, "wait_for_fresh: daemon-side error");
                return FreshnessWait::Transport(msg);
            }
        }
    }
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

    /// `cqs notes list` (the CLI form) must reach the daemon as `notes`,
    /// not `notes list` — the batch parser's `BatchCmd::Notes` accepts
    /// `--warnings`/`--patterns` directly without a `list` subcommand.
    /// Without the strip, every `cqs notes list` through the daemon errors
    /// `unexpected argument 'list' found`. Keep the explicit
    /// `--warnings`/`--patterns` flags through unchanged.
    #[test]
    fn notes_list_subcommand_stripped() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["notes", "list"]), true);
        assert_eq!(cmd, "notes");
        assert!(args.is_empty(), "got {args:?}");
    }

    #[test]
    fn notes_list_with_warnings_strips_only_list() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["notes", "list", "--warnings"]), true);
        assert_eq!(cmd, "notes");
        assert_eq!(args, v(&["--warnings"]));
    }

    /// Bare `cqs notes` (no `list` token) is unaffected.
    #[test]
    fn notes_without_list_unchanged() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["notes", "--patterns"]), true);
        assert_eq!(cmd, "notes");
        assert_eq!(args, v(&["--patterns"]));
    }

    /// Other commands with a `list` first-arg are NOT touched — only `notes`.
    #[test]
    fn list_arg_is_only_stripped_for_notes() {
        let (cmd, args) = translate_cli_args_to_batch(&v(&["impact", "list"]), true);
        assert_eq!(cmd, "impact");
        assert_eq!(args, v(&["list"]));
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
        // API-V1.30.1-5: typed variant — SocketMissing is the no-daemon path.
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// Task B2: `daemon_ping` must round-trip through a mock listener that
    /// speaks the same envelope as the real daemon. Asserts the field-by-
    /// field decoding so a future drift in either side surfaces here.
    ///
    /// `#[serial]` because this and `daemon_status_mock_round_trip` both
    /// mutate the global `XDG_RUNTIME_DIR` env var to redirect
    /// `daemon_socket_path` at a per-test tempdir. Running concurrently
    /// can deadlock the mock listener: test A binds at dir-A's path,
    /// test B then resets the env to dir-B before test A's
    /// `UnixStream::connect` resolves the socket — the spawned mock
    /// thread's `accept()` blocks forever, and `handle.join()` waits on
    /// it indefinitely. Pinning both tests to the same serialization key
    /// (`daemon_socket_xdg`) makes the env mutation safe.
    #[cfg(unix)]
    #[test]
    fn daemon_ping_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

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

        set_socket_dir_override_for_test(None);

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

    /// #1182: pre-existing `cqs ping` bug fix — production daemon serializes
    /// `output` as the dispatch envelope `{"data":<payload>,"error":null,...}`,
    /// not as a JSON-string of the bare payload like the mock above. The
    /// helper `unwrap_dispatch_payload` digs into `data` so `daemon_ping` /
    /// `daemon_status` deserialize the actual handler value. Pin both
    /// paths here.
    #[test]
    fn unwrap_dispatch_payload_extracts_data_from_envelope_object() {
        let envelope = serde_json::json!({
            "data": {"model": "x", "n": 1},
            "error": null,
            "version": 1,
            "_meta": {"handling_advice": "..."}
        });
        let inner = unwrap_dispatch_payload(&envelope, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x", "n": 1}));
    }

    #[test]
    fn unwrap_dispatch_payload_passes_through_bare_object() {
        // Legacy mock form: `output` is already the bare payload (no `data`
        // key). Helper must return it unchanged so existing test mocks keep
        // working.
        let bare = serde_json::json!({"model": "x", "n": 1});
        let inner = unwrap_dispatch_payload(&bare, "X").unwrap();
        assert_eq!(inner, bare);
    }

    #[test]
    fn unwrap_dispatch_payload_parses_string_then_extracts_data() {
        // String form wrapping an envelope — the helper parses the string,
        // then digs into `data`.
        let envelope_str = r#"{"data":{"model":"x"},"error":null,"version":1}"#;
        let value = serde_json::Value::String(envelope_str.to_string());
        let inner = unwrap_dispatch_payload(&value, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x"}));
    }

    #[test]
    fn unwrap_dispatch_payload_parses_string_bare_payload() {
        let bare_str = r#"{"model":"x","n":1}"#;
        let value = serde_json::Value::String(bare_str.to_string());
        let inner = unwrap_dispatch_payload(&value, "X").unwrap();
        assert_eq!(inner, serde_json::json!({"model": "x", "n": 1}));
    }

    /// #1182: `daemon_status` happy-path round-trip against a mock listener.
    /// Mirrors `daemon_ping_mock_round_trip` so a future drift in either
    /// the envelope shape or the WatchSnapshot fields surfaces here.
    ///
    /// `#[serial]` — see `daemon_ping_mock_round_trip` for the XDG race
    /// (also pins `daemon_reconcile_mock_round_trip` from Layer 1).
    #[cfg(unix)]
    #[test]
    fn daemon_status_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Mock the production envelope shape: `output` is the parsed
        // dispatch envelope object, not a JSON string.
        let snap = crate::watch_status::WatchSnapshot {
            state: crate::watch_status::FreshnessState::Stale,
            modified_files: 4,
            pending_notes: true,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 17,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_734_120_488,
            last_synced_at: Some(1_734_120_000),
            snapshot_at: Some(1_734_120_500),
            active_slot: None,
        };
        let inner_envelope = serde_json::json!({
            "data": serde_json::to_value(&snap).unwrap(),
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            assert!(
                request_line.contains("\"command\":\"status\""),
                "expected status request, got: {request_line}"
            );
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_status should succeed against mock");
        assert_eq!(resp.state, crate::watch_status::FreshnessState::Stale);
        assert_eq!(resp.modified_files, 4);
        assert!(resp.pending_notes);
        assert_eq!(resp.incremental_count, 17);
        assert_eq!(resp.last_synced_at, Some(1_734_120_000));
    }

    /// `daemon_status` surfaces a friendly error (not a panic / generic IO)
    /// when no daemon socket exists. Mirrors the `daemon_ping` parity test.
    #[cfg(unix)]
    #[test]
    fn daemon_status_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let result = daemon_status(&cqs_dir);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // API-V1.30.1-5: typed variant.
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// #1182 — Layer 1: `daemon_reconcile` happy-path round-trip
    /// against a mock listener. Asserts the wire shape of the request
    /// (command name + flag-style args) and the response deserialization.
    ///
    /// `#[serial]` for the same XDG-race reason as
    /// `daemon_status_mock_round_trip`.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_mock_round_trip() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Server response: dispatch envelope mirroring what
        // `dispatch_reconcile` would emit.
        // API-V1.30.1-6: `queued` field dropped from the wire shape;
        // Ok(...) implies queued.
        let inner_envelope = serde_json::json!({
            "data": {
                "was_pending": false,
                "hook": "post-checkout",
                "args": ["abc123", "def456", "1"],
            },
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            assert!(
                request_line.contains("\"command\":\"reconcile\""),
                "expected reconcile request, got: {request_line}"
            );
            // The hook + args ride along as flag-style batch args.
            assert!(
                request_line.contains("\"--hook\""),
                "expected --hook flag in args, got: {request_line}"
            );
            assert!(
                request_line.contains("\"post-checkout\""),
                "expected hook name in args, got: {request_line}"
            );
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let args: Vec<String> = ["abc123", "def456", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = daemon_reconcile(&cqs_dir, Some("post-checkout"), &args);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_reconcile should succeed against mock");
        // API-V1.30.1-6: `queued` field dropped; Ok(...) implies queued.
        assert!(!resp.was_pending);
        assert_eq!(resp.hook.as_deref(), Some("post-checkout"));
        assert_eq!(resp.args, vec!["abc123", "def456", "1"]);
    }

    /// TC-HAP-1.30.1-10: `daemon_reconcile` forwards UTF-8 hook args
    /// verbatim — emoji, accented characters, and zero-width characters
    /// must round-trip through the JSON envelope without mangling. Pin
    /// here because string handling that breaks on non-ASCII typically
    /// trips on multi-byte boundaries inside `BufRead::read_line`.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_forwards_unicode_args() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_for_thread = captured.clone();

        let inner_envelope = serde_json::json!({
            "data": {
                "was_pending": false,
                "hook": "post-merge",
                "args": ["mañana", "🚀", "café"],
            },
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({
            "status": "ok",
            "output": inner_envelope,
        });
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            *captured_for_thread.lock().unwrap() = Some(request_line.clone());
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let args: Vec<String> = vec!["mañana".to_string(), "🚀".to_string(), "café".to_string()];
        let result = daemon_reconcile(&cqs_dir, Some("post-merge"), &args);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        let resp = result.expect("daemon_reconcile should succeed against mock");
        assert_eq!(resp.hook.as_deref(), Some("post-merge"));
        assert_eq!(resp.args, vec!["mañana", "🚀", "café"]);

        // Captured request line preserves UTF-8 bytes verbatim. JSON
        // escaping may render emoji as `🚀` surrogate pairs;
        // accept either the raw or escaped form.
        let req = captured
            .lock()
            .unwrap()
            .clone()
            .expect("server thread should have captured the request");
        assert!(
            req.contains("mañana") || req.contains("ma\\u00f1ana"),
            "request must preserve accented characters, got: {req}"
        );
        assert!(
            req.contains("café") || req.contains("caf\\u00e9"),
            "request must preserve accented characters, got: {req}"
        );
        // The emoji 🚀 is U+1F680, encoded either raw (4 UTF-8 bytes) or
        // as a surrogate pair `🚀` in serde_json escape mode.
        assert!(
            req.contains('🚀') || req.contains("\\uD83D\\uDE80"),
            "request must preserve emoji, got: {req}"
        );
    }

    /// `daemon_reconcile` surfaces a friendly error when no daemon socket
    /// exists. The CLI surface treats this as a `.cqs/.dirty` fallback
    /// trigger — the error string must be matchable.
    #[cfg(unix)]
    #[test]
    fn daemon_reconcile_errors_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();
        let args: Vec<String> = Vec::new();
        let result = daemon_reconcile(&cqs_dir, Some("post-merge"), &args);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // API-V1.30.1-5: typed variant.
        assert!(
            matches!(err, DaemonRpcError::SocketMissing(_)),
            "expected SocketMissing variant, got: {err:?}"
        );
        let msg = err.as_message();
        assert!(
            msg.contains("no daemon running"),
            "expected friendly error, got: {msg}"
        );
    }

    /// PR 4 of #1182: `wait_for_fresh` returns `Fresh` immediately when the
    /// first poll already reports fresh. Pins the no-cost-when-fresh promise
    /// — agents that never have a stale tree don't pay 250 ms latency.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_fresh_on_first_poll() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let snap = crate::watch_status::WatchSnapshot {
            state: crate::watch_status::FreshnessState::Fresh,
            modified_files: 0,
            pending_notes: false,
            rebuild_in_flight: false,
            delta_saturated: false,
            incremental_count: 1234,
            dropped_this_cycle: 0,
            last_event_unix_secs: 1_734_120_470,
            last_synced_at: Some(1_734_120_000),
            snapshot_at: Some(1_734_120_500),
            active_slot: None,
        };
        let inner_envelope = serde_json::json!({
            "data": serde_json::to_value(&snap).unwrap(),
            "error": null,
            "version": 1,
        });
        let outer_envelope = serde_json::json!({"status": "ok", "output": inner_envelope});
        let outer_str = outer_envelope.to_string();

        let handle = std::thread::spawn(move || {
            // Single accept — fresh on first poll terminates the loop.
            let (mut stream, _) = listener.accept().unwrap();
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .unwrap();
            writeln!(stream, "{outer_str}").unwrap();
            stream.flush().unwrap();
        });

        let result = wait_for_fresh(&cqs_dir, 5);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Fresh(snap) => {
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Fresh);
                assert_eq!(snap.incremental_count, 1234);
            }
            other => panic!("expected Fresh, got: {other:?}"),
        }
    }

    /// PR 4 of #1182 (post-RB-9 refactor): `wait_for_fresh` reports
    /// `Timeout` when the deadline expires before any successful poll.
    /// With `wait_secs = 0` the deadline-first guard fires immediately,
    /// returning `Timeout(unknown)` without a wasted round-trip. The
    /// `unknown` snapshot signals "we never got a real status" so the
    /// caller's bail message can adapt.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_timeout_when_budget_expires() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        // A bound socket so the helper sees a daemon socket file. We never
        // actually accept — the deadline-first guard returns before the
        // first daemon_status call.
        let sock_path = daemon_socket_path(&cqs_dir);
        let _listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();

        let result = wait_for_fresh(&cqs_dir, 0);
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::Timeout(snap) => {
                // RB-9 deadline-first: budget=0 means we return without a
                // round-trip, so the snapshot is the synthetic "unknown".
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Unknown);
            }
            other => panic!("expected Timeout, got: {other:?}"),
        }
    }

    /// PR 4 of #1182: `wait_for_fresh` returns `NoDaemon` without a socket.
    /// No mock listener — the helper short-circuits on `daemon_status`'s
    /// pre-flight existence check. Must point XDG at a fresh tempdir so
    /// the helper sees no socket regardless of any host daemon.
    ///
    /// `#[serial]` for the same XDG-race reason as
    /// `daemon_status_mock_round_trip`.
    ///
    /// RB-9 reframe: post-deadline-first, we use `wait_secs = 5` so we
    /// reach `daemon_status` and get the SocketMissing → NoDaemon path.
    /// `wait_secs = 0` would short-circuit on the deadline-first guard
    /// and return `Timeout(unknown)` without ever asking the daemon.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_no_daemon_without_socket() {
        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let result = wait_for_fresh(&cqs_dir, 5);

        set_socket_dir_override_for_test(None);

        match result {
            FreshnessWait::NoDaemon(msg) => {
                assert!(
                    msg.contains("no daemon running"),
                    "expected friendly error, got: {msg}"
                );
            }
            other => panic!("expected NoDaemon, got: {other:?}"),
        }
    }

    /// TC-HAP-1.30.1-5: `wait_for_fresh` polls past stale snapshots and
    /// returns `Fresh` once the daemon flips. Listener accepts three
    /// connections: stale, stale, fresh. Pins both the polling loop AND
    /// the exponential backoff (elapsed must be at least the initial
    /// poll interval × 1 sleep — a fresh-on-first wouldn't sleep at all).
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_fresh_after_two_stale_polls() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
        // CQS_FRESHNESS_POLL_MS still uses set_var because freshness_poll_ms_initial
        // reads it directly (no thread-local hook); this is one isolated env mutation,
        // not a serialization-group dance, and the 25ms value is read once on test
        // entry. The structural fix for `freshness_poll_ms_initial` is the same shape
        // as #1292 (parameter / thread-local) but is out of scope for this PR.
        let prev_poll = std::env::var("CQS_FRESHNESS_POLL_MS").ok();
        // SAFETY: a single-set, single-restore env touch — racy in principle but the
        // mutator and reader are both in this test thread and the value is a poll
        // interval, not a path computed against contemporaneous state.
        unsafe {
            std::env::set_var("CQS_FRESHNESS_POLL_MS", "25");
        }

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        // Helper: build the outer envelope wrapping a snapshot.
        let envelope = |state: crate::watch_status::FreshnessState| -> String {
            let snap = crate::watch_status::WatchSnapshot {
                state,
                modified_files: if matches!(state, crate::watch_status::FreshnessState::Fresh) {
                    0
                } else {
                    3
                },
                pending_notes: false,
                rebuild_in_flight: false,
                delta_saturated: false,
                incremental_count: 0,
                dropped_this_cycle: 0,
                last_event_unix_secs: 1_734_120_488,
                last_synced_at: Some(1_734_120_000),
                snapshot_at: Some(1_734_120_500),
                active_slot: None,
            };
            let inner = serde_json::json!({
                "data": serde_json::to_value(&snap).unwrap(),
                "error": null,
                "version": 1,
            });
            serde_json::json!({"status": "ok", "output": inner}).to_string()
        };

        let stale1 = envelope(crate::watch_status::FreshnessState::Stale);
        let stale2 = envelope(crate::watch_status::FreshnessState::Stale);
        let fresh = envelope(crate::watch_status::FreshnessState::Fresh);

        let handle = std::thread::spawn(move || {
            for body in [stale1, stale2, fresh] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut req = String::new();
                BufReader::new(&stream).read_line(&mut req).unwrap();
                writeln!(stream, "{body}").unwrap();
                stream.flush().unwrap();
            }
        });

        let start = std::time::Instant::now();
        let result = wait_for_fresh(&cqs_dir, 5);
        let elapsed = start.elapsed();

        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);
        // SAFETY: matched single-set/restore with the entry block above.
        unsafe {
            match prev_poll {
                Some(v) => std::env::set_var("CQS_FRESHNESS_POLL_MS", v),
                None => std::env::remove_var("CQS_FRESHNESS_POLL_MS"),
            }
        }

        match result {
            FreshnessWait::Fresh(snap) => {
                assert_eq!(snap.state, crate::watch_status::FreshnessState::Fresh);
                assert_eq!(snap.modified_files, 0);
            }
            other => panic!("expected Fresh after stale polls, got: {other:?}"),
        }
        // We slept at least once (after first stale poll). At a 25ms
        // initial interval the test must take >= 25ms total. Use a loose
        // 20ms floor to allow scheduler jitter.
        assert!(
            elapsed.as_millis() >= 20,
            "expected at least one sleep cycle, got {}ms",
            elapsed.as_millis()
        );
    }

    /// TC-ADV-1.30.1-4: `wait_for_fresh` returns `Transport(_)` (not
    /// `NoDaemon`) when the daemon socket file exists but the daemon
    /// process has died — i.e. the listener was bound, accepted one
    /// connection, then dropped. The socket file persists (until we
    /// `remove_file`) but `UnixStream::connect` returns ECONNREFUSED
    /// because no listener is bound to it.
    #[cfg(unix)]
    #[test]
    fn wait_for_fresh_returns_transport_when_daemon_dies_mid_poll() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));
        let prev_poll = std::env::var("CQS_FRESHNESS_POLL_MS").ok();
        // SAFETY: see above.
        unsafe {
            std::env::set_var("CQS_FRESHNESS_POLL_MS", "25");
        }

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let stale_envelope = {
            let snap = crate::watch_status::WatchSnapshot {
                state: crate::watch_status::FreshnessState::Stale,
                modified_files: 5,
                pending_notes: false,
                rebuild_in_flight: false,
                delta_saturated: false,
                incremental_count: 0,
                dropped_this_cycle: 0,
                last_event_unix_secs: 1_734_120_488,
                last_synced_at: Some(1_734_120_000),
                snapshot_at: Some(1_734_120_500),
                active_slot: None,
            };
            let inner = serde_json::json!({
                "data": serde_json::to_value(&snap).unwrap(),
                "error": null,
                "version": 1,
            });
            serde_json::json!({"status": "ok", "output": inner}).to_string()
        };

        // Listener thread: accept once, send Stale, drop listener. The
        // socket file persists on disk so subsequent connects see the
        // file but hit ECONNREFUSED (no listener bound) — exercises
        // Transport, not SocketMissing.
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            writeln!(stream, "{stale_envelope}").unwrap();
            stream.flush().unwrap();
            drop(stream);
            drop(listener);
        });

        let result = wait_for_fresh(&cqs_dir, 5);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);
        // SAFETY: matched single-set/restore with the entry block above.
        unsafe {
            match prev_poll {
                Some(v) => std::env::set_var("CQS_FRESHNESS_POLL_MS", v),
                None => std::env::remove_var("CQS_FRESHNESS_POLL_MS"),
            }
        }

        // The socket file existed at every poll, but the daemon was
        // gone for the second one — must surface as Transport, not
        // NoDaemon, so the eval gate's advice points at journalctl
        // rather than at "start the daemon".
        match result {
            FreshnessWait::Transport(_) => { /* expected */ }
            other => panic!("expected Transport after daemon death, got: {other:?}"),
        }
    }

    /// TC-ADV-1.30.1-4 (sibling): `daemon_status` distinguishes a
    /// malformed envelope (`BadResponse`) from a missing socket
    /// (`SocketMissing`). The mock listener writes a truncated JSON line
    /// and closes — the helper must classify as BadResponse so callers
    /// like `wait_for_fresh` can route to "version skew" advice rather
    /// than "start a daemon".
    #[cfg(unix)]
    #[test]
    fn daemon_status_returns_bad_response_on_malformed_envelope() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Truncated envelope — opens an object then closes the line.
            writeln!(stream, r#"{{"status":"ok","output":"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::BadResponse(msg)) => {
                assert!(
                    msg.contains("parse")
                        || msg.contains("envelope")
                        || msg.contains("missing")
                        || msg.contains("EOF")
                        || msg.contains("trailing"),
                    "expected envelope/parse-class message, got: {msg}"
                );
            }
            other => panic!("expected BadResponse on malformed envelope, got: {other:?}"),
        }
    }

    /// AC-V1.30.1-9: `daemon_socket_path` is deterministic across calls
    /// (BLAKE3 of the cqs_dir bytes). Pin the exact hash for a known
    /// input so a future drift in the truncation length or hex
    /// formatting trips this test.
    #[cfg(unix)]
    #[test]
    fn daemon_socket_path_blake3_pinned() {
        use std::path::Path;
        // #1292: thread-local override; was previously unsafe set_var on XDG.
        set_socket_dir_override_for_test(Some(std::path::PathBuf::from("/tmp")));
        let p = daemon_socket_path(Path::new("/tmp/foo"));
        set_socket_dir_override_for_test(None);

        // Compute the expected hex independently so the test pins both
        // the algorithm choice (BLAKE3) and the truncation length (8 bytes
        // → 16 hex chars). If anyone swaps to SHA256 / changes the
        // truncation, this fails immediately.
        let expected_hash = blake3::hash(b"/tmp/foo");
        let expected_truncated = &expected_hash.as_bytes()[..8];
        let mut expected_hex = String::with_capacity(16);
        for b in expected_truncated {
            use std::fmt::Write as _;
            write!(expected_hex, "{:02x}", b).unwrap();
        }
        let expected = format!("/tmp/cqs-{expected_hex}.sock");
        assert_eq!(p.to_string_lossy(), expected);
        // Sanity: 16-char hex, fixed width — distinguishes from
        // DefaultHasher's variable-length unpadded output.
        assert_eq!(expected_hex.len(), 16, "BLAKE3 truncation must be 8 bytes");
    }

    // ===== TC-ADV-1.30.1: daemon-side adversarial pins =====
    //
    // Each test pins CURRENT behavior so a future fix produces a clear
    // inversion target. The two `daemon_status_handles_err_envelope_*`
    // tests pin the somewhat-ugly "daemon error: daemon error" doubled
    // string that today's fallback produces; future work should
    // surface the raw envelope in the error message.

    /// TC-ADV-1.30.1-8: `{"status":"err"}` with no `message` field. The
    /// fallback path uses the literal `"daemon error"` string for the
    /// missing message, then thiserror's `Display` prefixes with
    /// `"daemon error: "` — yielding the awkward doubled
    /// `"daemon error: daemon error"`. Pin so a future fix that
    /// surfaces the raw envelope (or a less-confusing fallback) trips
    /// this test.
    #[cfg(unix)]
    #[test]
    fn daemon_status_handles_err_envelope_with_no_message() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Err envelope with no `message` field at all.
            writeln!(stream, r#"{{"status":"err"}}"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::DaemonError(msg)) => {
                // CURRENT behavior: bare "daemon error" placeholder.
                assert_eq!(
                    msg, "daemon error",
                    "today's fallback uses the literal placeholder; future fix should \
                     surface the raw envelope or upgrade to a BadResponse variant",
                );
                // The full as_message() string today is the doubled form.
                let rendered = DaemonRpcError::DaemonError(msg).as_message();
                assert_eq!(
                    rendered, "daemon error: daemon error",
                    "today's wire-format string is the doubled fallback — \
                     future fix should produce a less-confusing rendering",
                );
            }
            other => panic!("expected DaemonError(\"daemon error\") today, got: {other:?}",),
        }
    }

    /// TC-ADV-1.30.1-9: `{"status":"err","message": 42}` (non-string
    /// message). `as_str()` returns None → falls back to the same
    /// `"daemon error"` placeholder as the no-message case. Pin both
    /// the variant and the fact that the integer payload is silently
    /// dropped, so future work that surfaces the type mismatch
    /// has a clear inversion target.
    #[cfg(unix)]
    #[test]
    fn daemon_status_handles_err_envelope_with_non_string_message() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).unwrap();

        // #1292: thread-local override avoids the unsafe set_var race that hangs CI.
        set_socket_dir_override_for_test(Some(dir.path().to_path_buf()));

        let sock_path = daemon_socket_path(&cqs_dir);
        let listener = UnixListener::bind(&sock_path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = String::new();
            BufReader::new(&stream).read_line(&mut req).unwrap();
            // Non-string message: integer 42.
            writeln!(stream, r#"{{"status":"err","message":42}}"#).unwrap();
            stream.flush().unwrap();
        });

        let result = daemon_status(&cqs_dir);
        handle.join().unwrap();
        let _ = std::fs::remove_file(&sock_path);

        set_socket_dir_override_for_test(None);

        match result {
            Err(DaemonRpcError::DaemonError(msg)) => {
                // CURRENT behavior: silent fallback. The 42 is dropped.
                assert_eq!(
                    msg, "daemon error",
                    "non-string message payload is silently dropped by `as_str()`; \
                     future fix should surface the shape mismatch as a BadResponse",
                );
            }
            other => panic!("expected DaemonError(\"daemon error\") today, got: {other:?}",),
        }
    }

    /// TC-ADV-1.30.1-10: `unwrap_dispatch_payload` does NOT distinguish
    /// envelope-with-`data:null`-and-error from a bare-payload null. The
    /// current code only checks for the *presence* of a `data` key and
    /// returns whatever it finds, so an envelope advertising
    /// `{"data": null, "error": "internal"}` round-trips as
    /// `Ok(Value::Null)` rather than surfacing the `error` field. Pin
    /// today's behavior — future work that propagates `error` from the
    /// envelope has a clear inversion target.
    #[test]
    fn unwrap_dispatch_payload_distinguishes_envelope_no_data_from_bare_form() {
        let v = serde_json::json!({"data": null, "error": "internal", "version": 1});
        let result = unwrap_dispatch_payload(&v, "TestType");
        // CURRENT behavior: returns Ok(Null), silently dropping `error`.
        // The future fix should surface `error` as `Err(_)` — when it
        // lands, this assertion inverts to `assert!(result.is_err())`.
        assert!(
            result.is_ok(),
            "today's helper passes through `data: null` even when an `error` field \
             is present alongside; future fix should surface that as Err",
        );
        assert_eq!(
            result.unwrap(),
            serde_json::Value::Null,
            "the data:null payload is returned verbatim; the error field is dropped",
        );
    }
}
