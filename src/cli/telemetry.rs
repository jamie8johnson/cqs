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
use std::time::SystemTime;

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

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

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
                let archive_name = format!("telemetry_{timestamp}.jsonl");
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

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

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
                let archive_name = format!("telemetry_{timestamp}.jsonl");
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
/// Derives known subcommands from `Cli`'s clap definition at runtime,
/// so new commands are recognized automatically without maintaining a list.
pub fn describe_command(args: &[String]) -> (String, Option<String>) {
    use clap::CommandFactory;

    // args[0] is the binary name
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("unknown");

    // If it's a bare query (no subcommand), it's a search
    if !cmd.starts_with('-') && !cmd.is_empty() {
        // Check if it's a known subcommand by querying clap's registry.
        // Also recognizes "help" which clap adds automatically.
        let clap_app = super::definitions::Cli::command();
        let is_subcommand = clap_app.get_subcommands().any(|sc| sc.get_name() == cmd);

        if is_subcommand {
            // It's a subcommand -- look for query in remaining args
            let query = args.iter().skip(2).find(|a| !a.starts_with('-')).cloned();
            return (cmd.to_string(), query);
        }

        // Bare query -- it's a search
        return ("search".to_string(), Some(cmd.to_string()));
    }

    (cmd.to_string(), None)
}
