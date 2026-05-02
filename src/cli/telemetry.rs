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

/// P3 #136: redact telemetry `query` strings by default. Search queries can
/// carry secrets and source snippets; logging them in plaintext at every
/// invocation is a privileged-journal harvest. Set
/// `CQS_TELEMETRY_REDACT_QUERY=0` to log the raw text (useful for offline
/// analysis on a single-user machine).
fn redact_query_str(query: &str) -> String {
    let redact = std::env::var("CQS_TELEMETRY_REDACT_QUERY")
        .ok()
        .as_deref()
        .map(|v| v != "0")
        .unwrap_or(true);
    if redact {
        // 8-char blake3 prefix is collision-resistant for telemetry buckets,
        // not reversible. Mirrors the SEC-V1.25-16 redaction shape used for
        // notes args in the daemon journal.
        let h = blake3::hash(query.as_bytes());
        h.to_hex().as_str()[..8].to_string()
    } else {
        query.to_string()
    }
}

/// Optional variant for `Option<&str>` fields.
fn redact_query_opt(query: Option<&str>) -> Option<String> {
    query.map(redact_query_str)
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
    // Active if env var is explicitly "1" OR (env unset AND telemetry file
    // already exists). RM-V1.25-25: when CQS_TELEMETRY is set to any
    // non-"1" value (including "0"), treat that as a hard opt-out so the
    // env var actually disables collection even when the file exists.
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

    // EH-V1.33-1: use `cqs::unix_secs_i64()` so a pre-epoch clock surfaces as
    // `ts: null` (serializing `Option<i64>::None`) and emits a one-shot
    // tracing::warn from the helper, instead of silently coercing to `ts: 0`.
    let timestamp = cqs::unix_secs_i64();

    // P3 #136: redact `query` by default to keep search strings out of the
    // telemetry log. `CQS_TELEMETRY_REDACT_QUERY=0` opts back in to raw text.
    let query_field = redact_query_opt(query);
    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": command,
        "query": query_field,
        "results": result_count,
    });

    // path already declared above for existence check
    // P3 #134: surface the closure result at debug. A telemetry write that
    // fails (lock contention, disk full, perms) should not be a hard error
    // — but `let _ = ...` made the failure invisible to the journal.
    let result: std::io::Result<()> = (|| -> std::io::Result<()> {
        // DS-V1.25-8: single-writer assumption — telemetry is per-process, but
        // multiple cqs invocations (CLI + agents + `cqs watch`) write to the
        // same `.cqs/telemetry.jsonl` concurrently. The advisory `flock` on
        // `telemetry.lock` enforces ordering *only if every writer takes the
        // lock* (classic advisory-lock caveat). Do not bypass it: skipping the
        // `try_lock` call will race with `cqs telemetry reset` (which takes
        // the blocking `lock`) and can either lose writes or corrupt a
        // half-rotated file.
        //
        // DS-NEW-2: advisory lock to prevent races with concurrent telemetry reset.
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

        // SHL-20: auto-archive if file exceeds 10 MB to prevent unbounded growth
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_TELEMETRY_BYTES {
                // EH-V1.33-1: archive filename falls back to `0` when the
                // clock is pre-epoch — uniqueness here is best-effort and
                // the JSON row above already records `ts: null` so the
                // bad-clock condition is preserved in the data, not just
                // a swept-under filename.
                let ts_for_filename = timestamp.unwrap_or(0);
                let archive_name = format!("telemetry_{ts_for_filename}.jsonl");
                let archive_path = cqs_dir.join(&archive_name);
                if let Err(e) = fs::rename(&path, &archive_path) {
                    tracing::warn!(
                        error = %e,
                        "Failed to auto-archive telemetry file"
                    );
                } else {
                    tracing::info!(
                        archived = %archive_name,
                        "Auto-archived telemetry file (exceeded 10 MB)"
                    );
                }
            }
        }
        // SEC-V1.25-5: set 0o600 at creation via OpenOptionsExt::mode to
        // close the umask race. The post-open set_permissions approach
        // left a window where the file was visible with default perms
        // (often 0o644).
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path)?;
        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry entry");
        }
        Ok(())
    })();
    if let Err(e) = result {
        tracing::debug!(error = %e, "Telemetry write skipped");
    }
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

    // EH-V1.33-1: see `log_command` above for the rationale.
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

    let result: std::io::Result<()> = (|| -> std::io::Result<()> {
        // Same single-writer + non-blocking flock + auto-archive contract
        // as `log_command`. See its inline doc for the rationale.
        let lock_path = cqs_dir.join("telemetry.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if lock_file.try_lock().is_err() {
            return Ok(());
        }

        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_TELEMETRY_BYTES {
                // EH-V1.33-1: archive filename falls back to `0` when the
                // clock is pre-epoch — uniqueness here is best-effort and
                // the JSON row above already records `ts: null` so the
                // bad-clock condition is preserved in the data, not just
                // a swept-under filename.
                let ts_for_filename = timestamp.unwrap_or(0);
                let archive_name = format!("telemetry_{ts_for_filename}.jsonl");
                let archive_path = cqs_dir.join(&archive_name);
                if let Err(e) = fs::rename(&path, &archive_path) {
                    tracing::warn!(error = %e, "Failed to auto-archive telemetry file");
                } else {
                    tracing::info!(archived = %archive_name, "Auto-archived telemetry file (exceeded 10 MB)");
                }
            }
        }

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path)?;
        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry completion entry");
        }
        Ok(())
    })();
    if let Err(e) = result {
        tracing::debug!(error = %e, "Telemetry completion write skipped");
    }
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
    // RM-V1.25-25: mirrors log_command — explicit non-"1" env opts out
    // even when the telemetry file is present.
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

    // EH-V1.33-1: see `log_command` above for the rationale.
    let timestamp = cqs::unix_secs_i64();

    // P3 #136: redact `query` by default — see `redact_query_str` doc.
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

    // P3 #134: same surface-at-debug treatment as `log_command`.
    let result: std::io::Result<()> = (|| -> std::io::Result<()> {
        // DS-V1.25-8: see the corresponding block in `log_command` above for the
        // full single-writer rationale. In short: telemetry is per-process but
        // many cqs invocations (CLI + agents + `cqs watch`) share the file, and
        // `flock` enforces ordering only when every writer takes it. Do not
        // bypass.
        //
        // Advisory lock to prevent races with concurrent telemetry reset.
        // Non-blocking try_lock — if reset holds it, skip this write silently.
        let lock_path = cqs_dir.join("telemetry.lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if lock_file.try_lock().is_err() {
            return Ok(());
        }

        // Auto-archive if file exceeds 10 MB to prevent unbounded growth
        if let Ok(meta) = fs::metadata(&path) {
            if meta.len() > MAX_TELEMETRY_BYTES {
                // EH-V1.33-1: archive filename falls back to `0` when the
                // clock is pre-epoch — uniqueness here is best-effort and
                // the JSON row above already records `ts: null` so the
                // bad-clock condition is preserved in the data, not just
                // a swept-under filename.
                let ts_for_filename = timestamp.unwrap_or(0);
                let archive_name = format!("telemetry_{ts_for_filename}.jsonl");
                let archive_path = cqs_dir.join(&archive_name);
                if let Err(e) = fs::rename(&path, &archive_path) {
                    tracing::warn!(
                        error = %e,
                        "Failed to auto-archive telemetry file"
                    );
                } else {
                    tracing::info!(
                        archived = %archive_name,
                        "Auto-archived telemetry file (exceeded 10 MB)"
                    );
                }
            }
        }

        // SEC-V1.25-5: set 0o600 at creation via OpenOptionsExt::mode to
        // close the umask race. The post-open set_permissions approach
        // left a window where the file was visible with default perms
        // (often 0o644).
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&path)?;
        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry entry");
        }
        Ok(())
    })();
    if let Err(e) = result {
        tracing::debug!(error = %e, "Telemetry write skipped");
    }
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
/// Pre-fix behavior: `cqs --json search "foo"` was logged as `cmd =
/// "--json"`. The archived 44k-record telemetry file shows this
/// happened to ~80% of all invocations. Post-fix it's recorded as
/// `cmd = "search"` with `query = "foo"` (or its blake3 prefix when
/// `CQS_TELEMETRY_REDACT_QUERY` is on).
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
        // The pre-fix bug: `cqs --json impact my_fn` was logged as
        // cmd="--json" because args[1] was returned verbatim. 80% of the
        // archived telemetry file hit this path. Fix walks past the flag
        // to the real subcommand.
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
}
