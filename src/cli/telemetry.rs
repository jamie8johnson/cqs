//! Optional usage telemetry for understanding how agents use cqs.
//!
//! Logs command invocations to `.cqs/telemetry.jsonl`. Each entry records:
//! timestamp, command name, query (if any), and result count.
//!
//! **Activation:** Telemetry is active when either:
//! - `CQS_TELEMETRY=1` env var is set, OR
//! - `CQS_TELEMETRY` is unset AND `.cqs/telemetry.jsonl` already exists
//!   (created by a previous `cqs telemetry reset`)
//!
//! This means: once you opt in (via env var or `cqs telemetry reset`), telemetry
//! stays on for all processes that use this project directory — including subagents
//! and non-interactive shells that may not inherit the env var.
//!
//! **Opt out:** Set `CQS_TELEMETRY=0` (hard opt-out, overrides the existence
//! check), or delete `.cqs/telemetry.jsonl` and unset `CQS_TELEMETRY`.
//!
//! Local file only. No network calls. Auto-archives at 10 MB.

use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Maximum telemetry file size before auto-archiving (10 MB).
const MAX_TELEMETRY_BYTES: u64 = 10 * 1024 * 1024;

/// Decide whether query redaction is active from the raw env value.
///
/// Opt-OUT semantics: redaction is ON by default and only disabled when the
/// value is in the falsy alias set (`0`, `false`, `no`, `off`,
/// case-insensitive, surrounding whitespace trimmed). Unset (`None`) → redact.
/// Any other value (e.g. `1`, `yes`, `garbage`) → redact. Pure function, no
/// env read and no logging, so the decision logic is unit-testable without the
/// `OnceLock` cache or process-global env races.
fn redact_enabled_from(val: Option<&str>) -> bool {
    match val {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

/// 8-char blake3 prefix of the query — collision-resistant for telemetry
/// buckets, not reversible. Mirrors the redaction shape used for notes args in
/// the daemon journal. Deterministic: same input always yields the same digest.
fn redact_query_digest(query: &str) -> String {
    let h = blake3::hash(query.as_bytes());
    h.to_hex().as_str()[..8].to_string()
}

/// Redact telemetry `query` strings by default. Search queries can carry
/// secrets and source snippets; logging them in plaintext at every invocation
/// is a privileged-journal harvest. Set `CQS_TELEMETRY_REDACT_QUERY` to a
/// falsy value (`0`, `false`, `no`, `off`) to log the raw text (useful for
/// offline analysis on a single-user machine).
///
/// The env var is read once via `OnceLock`; the resolved posture is logged
/// once at `info` on first invocation so operators can confirm whether
/// plaintext queries are landing in the journal.
fn redact_query_str(query: &str) -> String {
    use std::sync::OnceLock;
    static REDACT: OnceLock<bool> = OnceLock::new();
    let redact = *REDACT.get_or_init(|| {
        let enabled =
            redact_enabled_from(std::env::var("CQS_TELEMETRY_REDACT_QUERY").ok().as_deref());
        tracing::info!(
            redact = enabled,
            source = "CQS_TELEMETRY_REDACT_QUERY",
            "Telemetry query-redaction posture"
        );
        enabled
    });
    if redact {
        redact_query_digest(query)
    } else {
        query.to_string()
    }
}

/// Optional variant for `Option<&str>` fields.
fn redact_query_opt(query: Option<&str>) -> Option<String> {
    query.map(redact_query_str)
}

/// Taxonomy codes that may prefix a daemon-path error string
/// (`"daemon error: <code>: <message>…"`). Mirrors
/// [`crate::cli::json_envelope::ErrorCode`]. Kept plaintext when the free-text
/// message is redacted, so a `complete` row stays categorizable by failure
/// class without echoing user input.
const TELEMETRY_ERROR_CODES: &[&str] = &[
    "not_found",
    "invalid_input",
    "parse_error",
    "io_error",
    "internal",
    "timeout",
];

/// Extract a leading taxonomy code from a daemon-path error string.
///
/// Client-side daemon errors are formatted `"daemon error: <code>: <msg>…"`,
/// where `<code>` is one of [`TELEMETRY_ERROR_CODES`]. Returns the recognized
/// code — borrowed from the static set, so it never carries user text — or
/// `None` for a protocol error with no code / a CLI-direct anyhow error. Only
/// an EXACT match against the known set is accepted, so a free-text message
/// that merely contains a colon is never mistaken for a code.
fn taxonomy_code_of(error: &str) -> Option<&'static str> {
    let rest = error.strip_prefix("daemon error: ")?;
    let candidate = rest.split(':').next()?.trim();
    TELEMETRY_ERROR_CODES
        .iter()
        .copied()
        .find(|c| *c == candidate)
}

/// Redact a `complete`-event error string under the query-redaction posture.
///
/// The error field can echo user input verbatim: a `not_found` on a symbol the
/// agent typed re-surfaces that symbol as `"daemon error: not_found: <symbol>…"`.
/// The query that produced it is already hashed in the paired invoke row, so
/// leaving the echoed value plaintext here is a redaction split-brain. When
/// redaction is on the free-text message is replaced by the same 8-char blake3
/// bucket queries get, keeping only the plaintext taxonomy code
/// (`not_found` / `invalid_input` / …) for failure-class analysis; when off the
/// plaintext is kept verbatim.
///
/// Pure: `redact` is passed in, so the on/off behaviour is unit-testable
/// without the process-global env cache.
fn redact_error_message(error: &str, redact: bool) -> String {
    if !redact {
        return error.to_string();
    }
    let digest = redact_query_digest(error);
    match taxonomy_code_of(error) {
        Some(code) => format!("{code}: {digest}"),
        None => digest,
    }
}

/// Append a telemetry entry to `.cqs/telemetry.jsonl`.
///
/// Centralizes the activation check, advisory-flock, 10-MB auto-archive,
/// and 0o600 file-mode contract shared by every `log_*` function. New event
/// flavors should construct a `serde_json::Value` and call this — they MUST
/// NOT re-implement the flock/archive/write dance.
///
/// Activation rules (mirror the module docstring):
///   - `CQS_TELEMETRY=1`                          → active
///   - any other `CQS_TELEMETRY` value (incl. `0`) → hard opt-out, returns immediately
///   - unset                                       → active iff `telemetry.jsonl` already exists
///
/// `timestamp` is the value already produced by `cqs::unix_secs_i64()` at
/// the call site (callers that include the timestamp in `entry` reuse the
/// same value here so the archive filename matches the event's `ts` field).
/// `None` triggers the bad-clock fallback path: archive filename uses
/// `0` as the suffix, matching the entry's `ts: null`.
///
/// Failures are logged at `debug` and dropped — telemetry must never
/// break the tool.
fn append_telemetry(cqs_dir: &Path, entry: &serde_json::Value, timestamp: Option<i64>) {
    // Active if env var is explicitly "1" OR (env unset AND telemetry file
    // already exists). When CQS_TELEMETRY is set to any non-"1" value
    // (including "0"), that's a hard opt-out so the env var disables
    // collection even when the file exists.
    let path = cqs_dir.join("telemetry.jsonl");
    match std::env::var("CQS_TELEMETRY") {
        Ok(v) if v == "1" => {}
        Ok(_) => return,
        Err(_) => {
            if !path.exists() {
                return;
            }
        }
    }

    let result: std::io::Result<()> = (|| -> std::io::Result<()> {
        // Single-writer assumption — telemetry is per-process, but multiple
        // cqs invocations (CLI + agents + `cqs watch`) write to the same
        // `.cqs/telemetry.jsonl` concurrently.
        // The advisory `flock` on `telemetry.lock` enforces ordering *only
        // if every writer takes the lock* (classic advisory-lock caveat). Do
        // not bypass it: skipping the `try_lock` call will race with
        // `cqs telemetry reset` (which takes the blocking `lock`) and can
        // either lose writes or corrupt a half-rotated file.
        //
        // Non-blocking try_lock — if reset holds it, skip this write silently.
        let lock_path = cqs_dir.join("telemetry.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if lock_file.try_lock().is_err() {
            // Reset in progress — skip this write rather than block
            return Ok(());
        }

        // Open the append handle once and reuse it for both the size check
        // and the write. The size gate uses `file.metadata()` (an fstat on
        // the already-open fd) instead of `fs::metadata(&path)` (a fresh
        // path-resolve + stat) — one fewer path lookup per write. The handle
        // is opened *before* the size check so the 10-MB rotation, when it
        // fires, drops this handle, renames, and reopens a fresh empty file
        // rather than appending the new row to the about-to-be-archived
        // inode.
        //
        // Set 0o600 at creation via OpenOptionsExt::mode to close the umask
        // race. A post-open set_permissions would leave a window where the
        // file is visible with default perms (often 0o644).
        let open_append = || -> std::io::Result<std::fs::File> {
            let mut opts = OpenOptions::new();
            opts.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            opts.open(&path)
        };
        let mut file = open_append()?;

        // Auto-archive if file exceeds 10 MB to prevent unbounded growth.
        // `file.metadata()` is an fstat on the open fd, so the rotation check
        // still triggers correctly even when the write handle is reused.
        let over_cap = file
            .metadata()
            .map(|m| m.len() > MAX_TELEMETRY_BYTES)
            .unwrap_or(false);
        if over_cap {
            // Archive filename falls back to `0` when the clock is
            // pre-epoch — uniqueness here is best-effort and
            // the JSON row above already records `ts: null` so the
            // bad-clock condition is preserved in the data, not just
            // a swept-under filename.
            let ts_for_filename = timestamp.unwrap_or(0);
            let archive_name = format!("telemetry_{ts_for_filename}.jsonl");
            let archive_path = cqs_dir.join(&archive_name);
            // Drop the handle on the soon-to-be-archived inode before the
            // rename so the post-rotation write lands in a fresh file.
            drop(file);
            match fs::rename(&path, &archive_path) {
                Ok(()) => {
                    tracing::info!(
                        archived = %archive_name,
                        "Auto-archived telemetry file (exceeded 10 MB)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "Failed to auto-archive telemetry file"
                    );
                }
            }
            // Reopen — the create flag makes a new empty file after a
            // successful rename, or reuses the existing one if the rename
            // failed (the row still gets written, just to the un-rotated file).
            file = open_append()?;
        }

        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry entry");
        }
        Ok(())
    })();
    if let Err(e) = result {
        tracing::debug!(error = %e, "Telemetry write skipped");
    }
}

/// The surface that dispatched a command, tagged onto every invoke event so
/// the dashboard can attribute (and later split) `commands[cmd]` by origin.
///
/// - `Cli`: ran inline in the CLI process (no daemon — `CQS_NO_DAEMON=1`, a
///   non-forwardable command, or a daemon-down fallback).
/// - `Daemon`: served by the `cqs watch --serve` daemon over its unix socket
///   via the argv-forward path (the CLI auto-connect an agent hits by default).
/// - `Mcp`: arrived over the daemon socket in the MCP JSON-args wire shape
///   (relayed by the `cqs mcp` stdio bridge).
///
/// Before this split only the CLI path wrote the invoke event, so daemon/MCP-
/// served traffic — everything an agent runs through auto-connect — was
/// invisible in `commands[cmd]` while fully visible in `kind_fallbacks[cmd]`.
/// The recorder writes from whichever process handles the command (the CLI for
/// `Cli`, the daemon for `Daemon`/`Mcp`), so the count now covers every surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandSurface {
    Cli,
    Daemon,
    Mcp,
}

impl CommandSurface {
    /// Lowercase wire token written to the telemetry `surface` field.
    pub fn as_str(self) -> &'static str {
        match self {
            CommandSurface::Cli => "cli",
            CommandSurface::Daemon => "daemon",
            CommandSurface::Mcp => "mcp",
        }
    }
}

/// Log a command invocation to the telemetry file.
///
/// Does nothing if `CQS_TELEMETRY` env var is not set to "1".
/// Silently ignores write failures — telemetry should never break the tool.
///
/// `surface` records which dispatch path handled the command (see
/// [`CommandSurface`]); it lands as the `surface` field so per-surface splits
/// are possible without a later schema change. The daemon/MCP surfaces call
/// this from the served project's process, so their events land in that
/// project's `.cqs/telemetry.jsonl` — the same file the CLI writes.
pub fn log_command(
    cqs_dir: &Path,
    command: &str,
    query: Option<&str>,
    result_count: Option<usize>,
    surface: CommandSurface,
) {
    // Use `cqs::unix_secs_i64()` so a pre-epoch clock surfaces as `ts: null`
    // (serializing `Option<i64>::None`) and emits a one-shot tracing::warn
    // from the helper, instead of silently coercing to `ts: 0`.
    let timestamp = cqs::unix_secs_i64();

    // Redact `query` by default to keep search strings out of the telemetry
    // log. `CQS_TELEMETRY_REDACT_QUERY=0` opts back in to raw text. Every
    // surface passes the raw query here so redaction is applied identically
    // regardless of dispatch path.
    let query_field = redact_query_opt(query);
    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": command,
        "query": query_field,
        "results": result_count,
        "surface": surface.as_str(),
    });

    append_telemetry(cqs_dir, &entry, timestamp);
}

/// Log the completion of a previously-invoked command — duration and
/// success/failure outcome.
///
/// Pairs with [`log_command`] as a two-event model. The invoke event lands
/// at dispatch entry so it survives mid-run crashes; the complete event
/// lands once the dispatch returns. Pair against the invoke by `(cmd, ts)`
/// proximity (within ~seconds for any single invocation).
///
/// Schema: `{event: "complete", cmd, ok, duration_ms, error?, ts}`. The
/// `error` field is only set when `ok = false`. It is redacted under the SAME
/// `CQS_TELEMETRY_REDACT_QUERY` posture as search queries, because the error
/// text CAN echo user input — a `not_found` on a symbol the agent typed
/// re-surfaces that symbol as `"daemon error: not_found: <symbol>…"`. With
/// redaction on (default) the free-text message becomes an 8-char blake3
/// bucket, keeping only the plaintext taxonomy code (`not_found` /
/// `invalid_input` / …) for failure-class analysis; with redaction off the
/// plaintext (truncated to 240 chars) is kept. The paired invoke row hashes
/// the query under the same posture, so invoke and complete never disagree.
///
/// Activation rules and write semantics mirror [`log_command`].
pub fn log_command_complete(
    cqs_dir: &Path,
    command: &str,
    duration_ms: u64,
    ok: bool,
    error: Option<&str>,
) {
    // See `log_command` above for the timestamp rationale.
    let timestamp = cqs::unix_secs_i64();

    // Resolve the redaction posture with a fresh env read (not the query
    // `OnceLock`): the env is process-constant in production so the two agree,
    // and reading fresh keeps this decision deterministic under test. When on,
    // the message is bucketed to a hash and only the taxonomy code stays
    // plaintext; when off, the raw text is kept and truncated below.
    let redact = redact_enabled_from(std::env::var("CQS_TELEMETRY_REDACT_QUERY").ok().as_deref());
    let error_field = error.map(|s| {
        let processed = redact_error_message(s, redact);
        if processed.len() > 240 {
            let mut out = processed.chars().take(240).collect::<String>();
            out.push('…');
            out
        } else {
            processed
        }
    });

    let entry = serde_json::json!({
        "ts": timestamp,
        "event": "complete",
        "cmd": command,
        "ok": ok,
        "duration_ms": duration_ms,
        "error": error_field,
    });

    append_telemetry(cqs_dir, &entry, timestamp);
}

/// Log a search command with adaptive routing classification.
///
/// Extends the standard telemetry entry with category, confidence, strategy,
/// and whether fallback was triggered.
pub fn log_routed(
    cqs_dir: &Path,
    query: &str,
    category: &str,
    confidence: &str,
    strategy: &str,
    fallback: bool,
    result_count: Option<usize>,
) {
    // See `log_command` above for the timestamp rationale.
    let timestamp = cqs::unix_secs_i64();

    // Redact `query` by default — see `redact_query_str` doc.
    let query_field = redact_query_str(query);
    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": "search",
        "query": query_field,
        "category": category,
        "confidence": confidence,
        "strategy": strategy,
        "fallback": fallback,
        "results": result_count,
    });

    append_telemetry(cqs_dir, &entry, timestamp);
}

/// The top-level command + served-project dir a kind-fallback should be
/// attributed to, captured at the dispatch boundary and read back when a
/// graph core fires a fallback deeper in the call stack.
///
/// Two fields, two bugs closed:
/// - `command` is the user-facing command the agent actually invoked
///   (`scout` / `gather` / `impact` / `callers` / …), known at dispatch and
///   the same name `log_command` records. Tagging the fallback with it makes
///   `kind_fallbacks[cmd]` comparable to `commands[cmd]` — the rate is now
///   per user-invocation. The graph core's own sub-operation label rides
///   alongside as `fallback_from`, so the sub-op detail survives without
///   becoming the (incomparable) bucket key.
/// - `cqs_dir` is the request's served project, carried from `BatchContext`
///   on the daemon path. The recorder used to re-derive it from the running
///   process cwd, which mis-attributes every fallback to the wrong project
///   when the daemon's cwd is not the served project.
pub(crate) struct FallbackOrigin {
    pub command: String,
    pub cqs_dir: PathBuf,
}

thread_local! {
    /// Per-thread current fallback origin. `None` outside a dispatch (or on a
    /// path that never set it — see the recorder's cwd fallback). Graph cores
    /// run synchronously on the dispatch thread, so the value a guard installs
    /// at the boundary is the one a fallback deeper in the same call reads.
    static FALLBACK_ORIGIN: RefCell<Option<FallbackOrigin>> = const { RefCell::new(None) };
}

/// RAII guard for the thread's [`FallbackOrigin`]. Install-if-absent
/// (outermost-wins): the FIRST guard on the thread installs the origin; a
/// nested `enter_fallback_origin` while one is already live returns a no-op
/// guard that leaves the thread-local untouched. Only the installing guard
/// clears the origin on drop; a no-op guard's drop does nothing, so the
/// outer origin survives the whole call.
///
/// Rationale: a kind-fallback must book against the top-level command the
/// agent invoked, not an internal sub-op. If a composite command's internal
/// dispatch re-enters this chokepoint with the sub-op name, the outer origin
/// must win at the recording site — that is the documented "`kind_fallbacks[cmd]`
/// comparable to `commands[cmd]`" contract. The prior restore-on-drop design
/// let the inner install SHADOW the outer for the span of the sub-dispatch, so
/// a fallback fired inside booked under the sub-op. Install-if-absent removes
/// that window: the outermost origin is the only one ever installed.
///
/// The installing guard always transitions the thread-local `None → Some →
/// None`, so clear-on-drop (not restore-on-drop) is correct — there is never a
/// prior origin to restore, because if one existed this guard would be a no-op.
/// That also keeps pooled daemon worker threads leak-free: after the outermost
/// guard drops, the origin is `None` again for the next request.
pub(crate) struct FallbackOriginGuard {
    /// Whether THIS guard installed the origin (the thread-local was absent).
    /// Only the installing guard clears on drop; a nested no-op guard leaves
    /// the outer origin in place.
    installed: bool,
}

impl Drop for FallbackOriginGuard {
    fn drop(&mut self) {
        if self.installed {
            FALLBACK_ORIGIN.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
    }
}

/// Install the top-level command + served-project dir for the current
/// dispatch, unless a guard is already live on this thread (install-if-absent
/// — the outermost origin wins). Hold the returned guard across the command's
/// execution; the graph cores read it when they fire a fallback.
pub(crate) fn enter_fallback_origin(command: &str, cqs_dir: &Path) -> FallbackOriginGuard {
    FALLBACK_ORIGIN.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_some() {
            // A guard is already installed on this thread. Outermost wins:
            // record nothing and return a no-op guard whose drop leaves the
            // outer origin untouched.
            FallbackOriginGuard { installed: false }
        } else {
            *slot = Some(FallbackOrigin {
                command: command.to_string(),
                cqs_dir: cqs_dir.to_path_buf(),
            });
            FallbackOriginGuard { installed: true }
        }
    })
}

/// Run `f` with the current thread's fallback origin (if any) borrowed.
pub(crate) fn with_fallback_origin<R>(f: impl FnOnce(Option<&FallbackOrigin>) -> R) -> R {
    FALLBACK_ORIGIN.with(|cell| f(cell.borrow().as_ref()))
}

/// Log a kind-fallback fire to the telemetry file.
///
/// A graph command (`callers`, `impact`, `deps`, `test-map`, `trace`, …)
/// queried a name that classified to a kind it can't process (a const has
/// no callers, a type has no call-graph impact, …) and redirected to the
/// kind-labeled fallback instead of running its normal flow. This is the
/// Phase-2 routing-prioritization signal: `cqs telemetry` aggregates these
/// into a per-command fallback rate so the standing question — do agents
/// still bounce between commands — has data behind it.
///
/// Schema: `{event: "kind_fallback", cmd, fallback_from, kind, name,
/// definitions, ts}`. `cmd` is the top-level user-facing command the agent
/// invoked (the [`FallbackOrigin`] captured at dispatch) so the count buckets
/// the same way `log_command` does; `fallback_from` is the graph core's own
/// sub-operation label (`callers` / `impact` / …) preserved for detail. An
/// aggregating command (`scout` / `gather` / …) that fans out to several
/// graph cores attributes every fallback it triggers to itself, not to the
/// internal sub-ops the agent never invoked directly. `kind` is the routing
/// label (`const` / `type` / `module` / `ambiguous`); `name` is redacted by
/// default (it can carry symbol names worth keeping out of the plaintext
/// journal — same `CQS_TELEMETRY_REDACT_QUERY` opt-out as search queries).
/// Both surfaces (CLI direct and daemon dispatch) route through the same
/// command cores, so a single call site here covers both.
///
/// Activation rules and write semantics mirror [`log_command`].
pub fn log_kind_fallback(
    cqs_dir: &Path,
    command: &str,
    fallback_from: &str,
    kind: &str,
    name: &str,
    definitions: usize,
) {
    // See `log_command` above for the timestamp rationale.
    let timestamp = cqs::unix_secs_i64();

    // Redact `name` by default — it can be a symbol the operator would
    // rather not see in plaintext; reuse the search-query redaction knob.
    let name_field = redact_query_str(name);
    let entry = serde_json::json!({
        "ts": timestamp,
        "event": "kind_fallback",
        "cmd": command,
        "fallback_from": fallback_from,
        "kind": kind,
        "name": name_field,
        "definitions": definitions,
    });

    append_telemetry(cqs_dir, &entry, timestamp);
}

/// Extract command name and query from CLI args for telemetry.
///
/// Walks past leading flags so global options (`--json`, `--slot <name>`,
/// `--model <id>`, `-q`, etc.) don't get recorded as the command name.
/// The first non-flag, non-flag-value token is matched against clap's
/// subcommand registry; if it's a known subcommand we record that, else
/// we treat it as a bare query (`cqs <query>` short form) and record
/// `cmd = "search"`.
///
/// `cqs --json search "foo"` is recorded as `cmd = "search"` with
/// `query = "foo"` (or its blake3 prefix when `CQS_TELEMETRY_REDACT_QUERY`
/// is on) — the leading `--json` flag is walked past, not treated as the
/// command.
///
/// Derives known subcommands from `Cli`'s clap definition at runtime
/// so new commands are recognized automatically without maintaining
/// a list. The set of known clap value-flags (those that take a
/// following value, like `--slot foo` or `--model bar`) is also derived
/// at runtime so we know which `--key value` pairs to skip past.
pub fn describe_command(args: &[String]) -> (String, Option<String>) {
    use clap::CommandFactory;

    let clap_app = super::definitions::Cli::command();

    // Collect long+short flag forms that consume a following value.
    // `--slot value`, `-q value`, etc. — we step past these as a pair so
    // the value isn't mistaken for a subcommand or query. Top-level only;
    // subcommand-local args are handled inside clap's parser, not here.
    let mut value_flags: std::collections::HashSet<String> = std::collections::HashSet::new();
    for arg in clap_app.get_arguments() {
        if matches!(
            arg.get_action(),
            clap::ArgAction::Set | clap::ArgAction::Append
        ) {
            if let Some(long) = arg.get_long() {
                value_flags.insert(format!("--{long}"));
            }
            if let Some(short) = arg.get_short() {
                value_flags.insert(format!("-{short}"));
            }
        }
    }

    let known_subcommands: std::collections::HashSet<String> = clap_app
        .get_subcommands()
        .map(|sc| sc.get_name().to_string())
        .collect();

    // args[0] is the binary name; start scanning at args[1].
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with('-') {
            // `--key=value` is a single token regardless of value-flag-ness.
            // `--key value` consumes the next arg only when `--key` is a
            // value-flag (else it's a boolean / count flag and the next
            // arg is unrelated). Short flags follow the same rule.
            if a.contains('=') {
                i += 1;
            } else if value_flags.contains(a) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-flag token: subcommand, or bare-query short form.
        if known_subcommands.contains(a) {
            // Look for the first non-flag arg after the subcommand as the query.
            let query = args[i + 1..].iter().find(|x| !x.starts_with('-')).cloned();
            return (a.clone(), query);
        }
        // Bare query — `cqs "find me a thing"` short form.
        return ("search".to_string(), Some(a.clone()));
    }

    // No bare token at all (e.g. `cqs --help`, `cqs --version`, or no args).
    ("unknown".to_string(), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Every test that reads or writes `CQS_TELEMETRY` / `CQS_TELEMETRY_REDACT_QUERY`
    // is `#[serial]`: they mutate process-global env, and `log_command_complete`
    // now reads the redaction posture fresh, so concurrent env flips would flake.
    use serial_test::serial;

    fn args(parts: &[&str]) -> Vec<String> {
        std::iter::once("cqs")
            .chain(parts.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn describe_command_skips_leading_global_flag_for_subcommand() {
        // `cqs --json impact my_fn` must walk past the leading flag to the
        // real subcommand rather than returning args[1] (`--json`) verbatim.
        let (cmd, query) = describe_command(&args(&["--json", "impact", "my_fn"]));
        assert_eq!(cmd, "impact");
        assert_eq!(query.as_deref(), Some("my_fn"));
    }

    #[test]
    fn describe_command_skips_leading_global_flag_for_bare_query() {
        // `cqs --json "find authentication"` — there is no `search`
        // subcommand; bare query is the search shorthand. We should still
        // skip past the flag.
        let (cmd, query) = describe_command(&args(&["--json", "find authentication"]));
        assert_eq!(cmd, "search");
        assert_eq!(query.as_deref(), Some("find authentication"));
    }

    #[test]
    fn describe_command_skips_value_flag_pair() {
        // `--slot <name>` is a value-flag; the value should not be mistaken
        // for the subcommand.
        let (cmd, query) = describe_command(&args(&["--slot", "default", "impact", "my_fn"]));
        assert_eq!(cmd, "impact");
        assert_eq!(query.as_deref(), Some("my_fn"));
    }

    #[test]
    fn describe_command_handles_eq_form_value_flag() {
        // `--slot=foo` is a single token; we don't double-skip after it.
        let (cmd, query) = describe_command(&args(&["--slot=default", "impact", "my_fn"]));
        assert_eq!(cmd, "impact");
        assert_eq!(query.as_deref(), Some("my_fn"));
    }

    #[test]
    fn describe_command_chained_global_flags() {
        // Multiple leading flags should all get skipped.
        let (cmd, query) =
            describe_command(&args(&["--json", "--slot", "alt", "scout", "do thing"]));
        assert_eq!(cmd, "scout");
        assert_eq!(query.as_deref(), Some("do thing"));
    }

    #[test]
    fn describe_command_bare_query_short_form() {
        // `cqs "find me a thing"` (no subcommand) is the search shorthand.
        let (cmd, query) = describe_command(&args(&["find me a thing"]));
        assert_eq!(cmd, "search");
        assert_eq!(query.as_deref(), Some("find me a thing"));
    }

    #[test]
    fn describe_command_no_subcommand_only_flags() {
        // `cqs --help` / `cqs --version` should land as "unknown" rather than
        // recording the flag itself as the command.
        let (cmd, query) = describe_command(&args(&["--help"]));
        assert_eq!(cmd, "unknown");
        assert!(query.is_none());

        let (cmd, query) = describe_command(&args(&["--version"]));
        assert_eq!(cmd, "unknown");
        assert!(query.is_none());
    }

    #[test]
    fn describe_command_subcommand_with_trailing_flags() {
        // Flags after the subcommand should not be picked up as the query.
        let (cmd, query) = describe_command(&args(&["impact", "some_fn", "--json"]));
        assert_eq!(cmd, "impact");
        assert_eq!(query.as_deref(), Some("some_fn"));
    }

    #[test]
    fn describe_command_empty_args_after_binary_name() {
        // `cqs` alone — no args after binary name.
        let (cmd, query) = describe_command(&args(&[]));
        assert_eq!(cmd, "unknown");
        assert!(query.is_none());
    }

    #[test]
    #[serial]
    fn log_command_complete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        // Activate telemetry by pre-creating the file.
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        // Redaction OFF so the plaintext error is preserved for the structural
        // assertion below; the redaction posture is exercised separately.
        std::env::set_var("CQS_TELEMETRY_REDACT_QUERY", "0");

        log_command_complete(cqs_dir, "impact", 42, true, None);
        log_command_complete(
            cqs_dir,
            "search",
            17,
            false,
            Some("Database error: table missing"),
        );

        std::env::remove_var("CQS_TELEMETRY");
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 completion lines");

        let r0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(r0["event"], "complete");
        assert_eq!(r0["cmd"], "impact");
        assert_eq!(r0["ok"], true);
        assert_eq!(r0["duration_ms"], 42);
        assert!(r0["error"].is_null());

        let r1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(r1["cmd"], "search");
        assert_eq!(r1["ok"], false);
        assert_eq!(r1["duration_ms"], 17);
        assert!(r1["error"].as_str().unwrap().contains("Database error"));
    }

    #[test]
    #[serial]
    fn log_kind_fallback_writes_event_with_redacted_name() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        // Default redaction on: the name must be hashed, not plaintext.
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        // Top-level command (`scout`) distinct from the firing sub-op
        // (`callers`): the event records both so the count buckets by the
        // user-facing command while preserving the sub-op detail.
        log_kind_fallback(cqs_dir, "scout", "callers", "const", "MAX_RETRIES", 1);

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(r["event"], "kind_fallback");
        assert_eq!(r["cmd"], "scout", "cmd is the top-level command");
        assert_eq!(
            r["fallback_from"], "callers",
            "fallback_from preserves the firing sub-op"
        );
        assert_eq!(r["kind"], "const");
        assert_eq!(r["definitions"], 1);
        // Redacted: the raw symbol name must not appear; the field is an
        // 8-char blake3 prefix.
        let name = r["name"].as_str().unwrap();
        assert_ne!(name, "MAX_RETRIES", "name must be redacted by default");
        assert_eq!(name.len(), 8, "redacted name is an 8-char hash prefix");
    }

    #[test]
    fn fallback_origin_guard_installs_and_clears() {
        // Outside any guard, no origin is installed.
        with_fallback_origin(|o| assert!(o.is_none()));

        {
            let _g = enter_fallback_origin("scout", Path::new("/tmp/proj/.cqs"));
            with_fallback_origin(|o| {
                let o = o.expect("origin installed");
                assert_eq!(o.command, "scout");
                assert_eq!(o.cqs_dir, Path::new("/tmp/proj/.cqs"));
            });

            // Install-if-absent (outermost-wins): a nested guard is a NO-OP —
            // the outer `scout` origin must NOT be shadowed by the inner
            // sub-op, even for the span of the inner scope. This is the
            // recording-site contract: a fallback fired inside the inner
            // dispatch books under the top-level command, not the sub-op.
            {
                let _g2 = enter_fallback_origin("callers", Path::new("/tmp/other/.cqs"));
                with_fallback_origin(|o| {
                    assert_eq!(
                        o.expect("origin still installed").command,
                        "scout",
                        "the inner install must be a no-op — the outermost origin wins"
                    );
                });
            }
            // After the inner (no-op) guard drops, the outer origin is intact:
            // a no-op guard's drop must not clear the thread-local.
            with_fallback_origin(|o| {
                assert_eq!(
                    o.expect("outer origin intact").command,
                    "scout",
                    "the inner no-op guard's drop must not clear the outer origin"
                );
            });
        }

        // After the outermost (installing) guard drops, the origin is cleared.
        with_fallback_origin(|o| assert!(o.is_none()));
    }

    /// The recording-site attribution contract, exercised end-to-end through
    /// [`log_kind_fallback`]: a top-level command (`impact`) whose internal
    /// sub-dispatch re-enters `enter_fallback_origin` with a sub-op name
    /// (`deps`) must STILL book the fallback under `impact`, with the sub-op
    /// preserved as `fallback_from`. This is the RED case the suite never had:
    /// under the pre-fix restore-on-drop guard the inner install shadowed the
    /// outer, so a fallback fired inside the sub-dispatch booked `cmd == "deps"`;
    /// install-if-absent makes the outermost origin win so `cmd == "impact"`.
    #[test]
    #[serial]
    fn nested_sub_dispatch_books_fallback_under_top_level_command() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");

        {
            // Outer dispatch boundary: the agent invoked `impact`.
            let _outer = enter_fallback_origin("impact", cqs_dir);
            // Inner sub-dispatch re-enters the chokepoint with the sub-op name.
            // Install-if-absent makes this a no-op guard.
            let _inner = enter_fallback_origin("deps", cqs_dir);
            // A graph core fires its fallback tagged with its own sub-op. It
            // reads `command` from the origin (impact) and passes `fallback_from`
            // (deps) at the fire site — the two are independent.
            with_fallback_origin(|origin| {
                let command = origin.map(|o| o.command.as_str()).unwrap_or("deps");
                let dir = origin.map(|o| o.cqs_dir.as_path()).unwrap_or(cqs_dir);
                log_kind_fallback(dir, command, "deps", "function", "helper", 1);
            });
        }

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(r["event"], "kind_fallback");
        assert_eq!(
            r["cmd"], "impact",
            "nested sub-dispatch must book under the top-level command the agent invoked"
        );
        assert_eq!(
            r["fallback_from"], "deps",
            "the firing sub-op rides alongside as fallback_from"
        );
    }

    /// Cross-request restore on a POOLED thread — the invariant the daemon
    /// relies on. A daemon worker thread is reused across requests; if a
    /// guard's drop failed to restore the thread-local, request B would read
    /// request A's leaked origin and mis-attribute its fallback. This pins the
    /// no-leak property by serving two requests on the SAME thread (the same
    /// `FALLBACK_ORIGIN` cell), asserting B never observes A's origin.
    ///
    /// Both "requests" run inline on the test thread — that is exactly a
    /// pooled worker serving two sequential dispatches, which a per-test
    /// thread (cargo's default) does NOT exercise on its own.
    #[test]
    fn fallback_origin_no_cross_request_leak_on_pooled_thread() {
        // Baseline before any request: clean.
        with_fallback_origin(|o| assert!(o.is_none(), "thread must start clean"));

        // ── Request A: sets its origin, runs, guard drops at scope end. ──
        {
            let _a = enter_fallback_origin("callers", Path::new("/tmp/projA/.cqs"));
            with_fallback_origin(|o| {
                let o = o.expect("request A origin installed");
                assert_eq!(o.command, "callers");
                assert_eq!(o.cqs_dir, Path::new("/tmp/projA/.cqs"));
            });
        }

        // Between requests on the reused thread: A's origin must be GONE.
        with_fallback_origin(|o| {
            assert!(
                o.is_none(),
                "request A's origin leaked to the next request on the pooled thread"
            );
        });

        // ── Request B: a DIFFERENT command/project on the same thread. ──
        {
            let _b = enter_fallback_origin("impact", Path::new("/tmp/projB/.cqs"));
            with_fallback_origin(|o| {
                let o = o.expect("request B origin installed");
                // B sees ITS OWN origin, never A's stale one.
                assert_eq!(
                    o.command, "impact",
                    "request B must see its own origin, not A's"
                );
                assert_eq!(o.cqs_dir, Path::new("/tmp/projB/.cqs"));
            });
        }

        // Thread returns to the pool clean for the next request.
        with_fallback_origin(|o| assert!(o.is_none(), "thread must return clean"));
    }

    #[test]
    #[serial]
    fn telemetry_rotates_at_10mb_with_reused_handle() {
        // The size gate now uses `file.metadata()` (fstat on the open append
        // handle) instead of a separate path stat. Pin that rotation still
        // fires: a telemetry file already past the 10-MB cap must be archived
        // on the next write, and the live file must come back as a small fresh
        // file holding only the new row.
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        let path = cqs_dir.join("telemetry.jsonl");

        // Seed an over-cap file (10 MB + 1 byte).
        let oversized = vec![b'x'; (MAX_TELEMETRY_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");

        log_command(
            cqs_dir,
            "impact",
            Some("my_fn"),
            Some(3),
            CommandSurface::Cli,
        );

        std::env::remove_var("CQS_TELEMETRY");

        // The live file should have been rotated: it now holds just the one
        // new JSON row, far under the cap.
        let live = std::fs::read_to_string(&path).unwrap();
        assert!(
            live.len() < (MAX_TELEMETRY_BYTES as usize),
            "live telemetry file should be small after rotation, got {} bytes",
            live.len()
        );
        let lines: Vec<&str> = live.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "rotated file should hold exactly the new row"
        );
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["cmd"], "impact");

        // An archive file `telemetry_*.jsonl` should now exist holding the
        // old oversized content.
        let archives: Vec<_> = std::fs::read_dir(cqs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("telemetry_") && n.ends_with(".jsonl")
            })
            .collect();
        assert_eq!(archives.len(), 1, "exactly one archive should be created");
        let archived_len = archives[0].metadata().unwrap().len();
        assert!(
            archived_len > MAX_TELEMETRY_BYTES,
            "archive should hold the old oversized content"
        );
    }

    #[test]
    #[serial]
    fn log_command_complete_truncates_long_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        // Truncation only applies to the plaintext branch — a redacted message
        // is already an 8-char bucket. Force redaction OFF to exercise the cap.
        std::env::set_var("CQS_TELEMETRY_REDACT_QUERY", "0");

        let long_err: String = "x".repeat(500);
        log_command_complete(cqs_dir, "search", 1, false, Some(&long_err));

        std::env::remove_var("CQS_TELEMETRY");
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let err_field = r["error"].as_str().unwrap();
        assert!(err_field.ends_with('…'));
        // 240 'x's plus the ellipsis.
        assert_eq!(err_field.chars().count(), 241);
    }

    #[test]
    fn taxonomy_code_of_recognizes_daemon_error_shape() {
        // The client-side daemon error carries "daemon error: <code>: <msg>".
        assert_eq!(
            taxonomy_code_of("daemon error: not_found: symbol SECRET not found\nhint: x"),
            Some("not_found")
        );
        assert_eq!(
            taxonomy_code_of("daemon error: invalid_input: bad arg"),
            Some("invalid_input")
        );
        // A protocol error with no code (busy reject, "missing 'command'"): the
        // free-text after the prefix is not a known code → None.
        assert_eq!(
            taxonomy_code_of("daemon error: daemon busy (too many concurrent clients)\nhint: x"),
            None
        );
        // A CLI-direct anyhow error has no "daemon error: " prefix.
        assert_eq!(taxonomy_code_of("no index found at .cqs/"), None);
        // A message whose free text merely contains a colon must not be
        // mistaken for a code.
        assert_eq!(
            taxonomy_code_of("daemon error: could not read: /etc/passwd"),
            None
        );
    }

    #[test]
    fn redact_error_message_keeps_code_drops_user_text_when_on() {
        // Redaction ON: the taxonomy code survives (categorization) but the
        // echoed symbol must NOT — it follows the query's hash treatment.
        let err = "daemon error: not_found: symbol SECRET_SYMBOL not found\nhint: set CQS_NO_DAEMON=1 to bypass the daemon";
        let out = redact_error_message(err, true);
        assert!(out.starts_with("not_found: "), "taxonomy code kept: {out}");
        assert!(
            !out.contains("SECRET_SYMBOL"),
            "user symbol must be redacted, got: {out}"
        );
        // The message segment is the 8-char blake3 bucket.
        let digest = out.strip_prefix("not_found: ").unwrap();
        assert_eq!(digest.len(), 8, "message segment is an 8-char hash prefix");
    }

    #[test]
    fn redact_error_message_hashes_whole_when_no_code_and_on() {
        // No recognizable taxonomy code → the whole thing becomes a bucket, so
        // a CLI-direct error that embeds a path/symbol can't leak.
        let err = "no index found at /home/secret/project/.cqs";
        let out = redact_error_message(err, true);
        assert!(!out.contains("secret"), "path must be redacted, got: {out}");
        assert_eq!(out.len(), 8, "redacted form is an 8-char hash prefix");
    }

    #[test]
    fn redact_error_message_plaintext_when_off() {
        // Redaction OFF: the raw error is preserved for offline analysis.
        let err = "daemon error: not_found: symbol SECRET_SYMBOL not found";
        assert_eq!(redact_error_message(err, false), err);
    }

    #[test]
    #[serial]
    fn log_command_complete_redacts_taxonomy_error_when_on() {
        // End-to-end guard for the redaction split-brain: a failing daemon-path
        // command whose error echoes a typed symbol must NOT land that symbol in
        // the complete row when redaction is on (the default). The taxonomy code
        // stays for failure-class analysis. RED before the fix — the row carried
        // the plaintext symbol.
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        // Default-on redaction (explicitly clear any inherited opt-out).
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        log_command_complete(
            cqs_dir,
            "impact",
            5,
            false,
            Some("daemon error: not_found: symbol SUPER_SECRET_TOKEN not found\nhint: set CQS_NO_DAEMON=1 to bypass the daemon"),
        );

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let err = r["error"].as_str().unwrap();
        assert!(
            !err.contains("SUPER_SECRET_TOKEN"),
            "user symbol must not leak into the complete row, got: {err}"
        );
        assert!(
            err.starts_with("not_found: "),
            "taxonomy code must be preserved, got: {err}"
        );
    }

    #[test]
    #[serial]
    fn log_command_complete_keeps_plaintext_when_off() {
        // Redaction OFF: the raw error text is preserved verbatim.
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        std::env::set_var("CQS_TELEMETRY_REDACT_QUERY", "0");

        log_command_complete(
            cqs_dir,
            "impact",
            5,
            false,
            Some("daemon error: not_found: symbol VISIBLE_SYMBOL not found"),
        );

        std::env::remove_var("CQS_TELEMETRY");
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let err = r["error"].as_str().unwrap();
        assert!(
            err.contains("VISIBLE_SYMBOL"),
            "plaintext must be kept when redaction is off, got: {err}"
        );
    }

    #[test]
    #[serial]
    fn invoke_and_complete_agree_on_redaction_posture() {
        // Cross-event posture parity: with redaction on, the invoke row hashes
        // the query AND the complete row hashes the error's user text — neither
        // surface leaks the typed value, so a reader can't reconstruct it by
        // joining the two rows.
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        log_command(
            cqs_dir,
            "impact",
            Some("SUPER_SECRET_TOKEN"),
            None,
            CommandSurface::Cli,
        );
        log_command_complete(
            cqs_dir,
            "impact",
            5,
            false,
            Some("daemon error: not_found: symbol SUPER_SECRET_TOKEN not found"),
        );

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        assert!(
            !body.contains("SUPER_SECRET_TOKEN"),
            "neither invoke nor complete row may carry the typed value, got: {body}"
        );
    }

    #[test]
    #[serial]
    fn log_command_writes_surface_and_redacts_query() {
        // The invoke event must carry the `surface` tag and redact the query
        // by default — the daemon/MCP path hands the raw query straight to
        // `log_command`, so this is the redaction-parity guard for every
        // surface.
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        log_command(
            cqs_dir,
            "search",
            Some("secret query"),
            Some(2),
            CommandSurface::Daemon,
        );

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(r["cmd"], "search");
        assert_eq!(r["surface"], "daemon", "surface tag must be recorded");
        // No `event` field: it's an invoke event, which the dashboard counts.
        assert!(r["event"].is_null());
        // Query redacted to an 8-char blake3 prefix, never the plaintext.
        let q = r["query"].as_str().unwrap();
        assert_ne!(q, "secret query", "query must be redacted by default");
        assert_eq!(q.len(), 8, "redacted query is an 8-char hash prefix");
    }

    #[test]
    fn command_surface_wire_tokens_are_stable() {
        assert_eq!(CommandSurface::Cli.as_str(), "cli");
        assert_eq!(CommandSurface::Daemon.as_str(), "daemon");
        assert_eq!(CommandSurface::Mcp.as_str(), "mcp");
    }

    #[test]
    fn redact_enabled_defaults_on_when_unset() {
        assert!(redact_enabled_from(None));
    }

    #[test]
    fn redact_enabled_falsy_aliases_opt_out() {
        // Opt-OUT set: only these disable redaction.
        assert!(!redact_enabled_from(Some("0")));
        assert!(!redact_enabled_from(Some("false")));
        assert!(!redact_enabled_from(Some("no")));
        assert!(!redact_enabled_from(Some("off")));
        // Case-insensitive + whitespace-trimmed.
        assert!(!redact_enabled_from(Some("OFF")));
        assert!(!redact_enabled_from(Some("False")));
        assert!(!redact_enabled_from(Some(" 0 ")));
    }

    #[test]
    fn redact_enabled_truthy_and_garbage_stay_on() {
        // Anything outside the falsy set keeps redaction ON (fail-safe).
        assert!(redact_enabled_from(Some("1")));
        assert!(redact_enabled_from(Some("yes")));
        assert!(redact_enabled_from(Some("on")));
        assert!(redact_enabled_from(Some("true")));
        assert!(redact_enabled_from(Some("")));
        assert!(redact_enabled_from(Some("garbage")));
    }

    #[test]
    fn redact_query_digest_is_deterministic_and_short() {
        let a = redact_query_digest("find authentication tokens");
        let b = redact_query_digest("find authentication tokens");
        assert_eq!(a, b, "same input must hash to same digest");
        assert_eq!(a.len(), 8, "digest is an 8-char blake3 prefix");
        // Different input → different bucket (overwhelmingly likely).
        assert_ne!(a, redact_query_digest("a totally different query"));
        // Digest must not leak the plaintext.
        assert!(!a.contains("authentication"));
    }
}
