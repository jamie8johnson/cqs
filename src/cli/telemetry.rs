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

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

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

/// Log a command invocation to the telemetry file.
///
/// Does nothing if `CQS_TELEMETRY` env var is not set to "1".
/// Silently ignores write failures — telemetry should never break the tool.
pub fn log_command(
    cqs_dir: &Path,
    command: &str,
    query: Option<&str>,
    result_count: Option<usize>,
) {
    // Use `cqs::unix_secs_i64()` so a pre-epoch clock surfaces as `ts: null`
    // (serializing `Option<i64>::None`) and emits a one-shot tracing::warn
    // from the helper, instead of silently coercing to `ts: 0`.
    let timestamp = cqs::unix_secs_i64();

    // Redact `query` by default to keep search strings out of the telemetry
    // log. `CQS_TELEMETRY_REDACT_QUERY=0` opts back in to raw text.
    let query_field = redact_query_opt(query);
    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": command,
        "query": query_field,
        "results": result_count,
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
/// `error` field is only set when `ok = false`; the message is truncated
/// to 240 chars and never includes raw query text (anyhow `Display` already
/// gives the `Result::Err`'s `Display` rather than the search args).
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

    let error_field = error.map(|s| {
        if s.len() > 240 {
            let mut out = s.chars().take(240).collect::<String>();
            out.push('…');
            out
        } else {
            s.to_string()
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
/// Schema: `{event: "kind_fallback", cmd, kind, name, definitions, ts}`.
/// `cmd` is the firing command (the fallback's `fallback_from`); `kind` is
/// the routing label (`const` / `type` / `module` / `ambiguous`); `name`
/// is redacted by default (it can carry symbol names worth keeping out of
/// the plaintext journal — same `CQS_TELEMETRY_REDACT_QUERY` opt-out as
/// search queries). Both surfaces (CLI direct and daemon dispatch) route
/// through the same command cores, so a single call site here covers both.
///
/// Activation rules and write semantics mirror [`log_command`].
pub fn log_kind_fallback(
    cqs_dir: &Path,
    command: &str,
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
    fn log_command_complete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        // Activate telemetry by pre-creating the file.
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");

        log_command_complete(cqs_dir, "impact", 42, true, None);
        log_command_complete(
            cqs_dir,
            "search",
            17,
            false,
            Some("Database error: table missing"),
        );

        std::env::remove_var("CQS_TELEMETRY");

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
    fn log_kind_fallback_writes_event_with_redacted_name() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");
        // Default redaction on: the name must be hashed, not plaintext.
        std::env::remove_var("CQS_TELEMETRY_REDACT_QUERY");

        log_kind_fallback(cqs_dir, "callers", "const", "MAX_RETRIES", 1);

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(r["event"], "kind_fallback");
        assert_eq!(r["cmd"], "callers");
        assert_eq!(r["kind"], "const");
        assert_eq!(r["definitions"], 1);
        // Redacted: the raw symbol name must not appear; the field is an
        // 8-char blake3 prefix.
        let name = r["name"].as_str().unwrap();
        assert_ne!(name, "MAX_RETRIES", "name must be redacted by default");
        assert_eq!(name.len(), 8, "redacted name is an 8-char hash prefix");
    }

    #[test]
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

        log_command(cqs_dir, "impact", Some("my_fn"), Some(3));

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
    fn log_command_complete_truncates_long_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cqs_dir = tmp.path();
        std::fs::write(cqs_dir.join("telemetry.jsonl"), "").unwrap();
        std::env::set_var("CQS_TELEMETRY", "1");

        let long_err: String = "x".repeat(500);
        log_command_complete(cqs_dir, "search", 1, false, Some(&long_err));

        std::env::remove_var("CQS_TELEMETRY");

        let body = std::fs::read_to_string(cqs_dir.join("telemetry.jsonl")).unwrap();
        let r: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        let err_field = r["error"].as_str().unwrap();
        assert!(err_field.ends_with('…'));
        // 240 'x's plus the ellipsis.
        assert_eq!(err_field.chars().count(), 241);
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
