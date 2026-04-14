//! Optional usage telemetry for understanding how agents use cqs.
//!
//! Logs command invocations to `.cqs/telemetry.jsonl`. Each entry records:
//! timestamp, command name, query (if any), and result count.
//!
//! **Activation:** Telemetry is active when either:
//! - `CQS_TELEMETRY=1` env var is set, OR
//! - `.cqs/telemetry.jsonl` already exists (created by a previous `cqs telemetry reset`)
//!
//! This means: once you opt in (via env var or `cqs telemetry reset`), telemetry
//! stays on for all processes that use this project directory — including subagents
//! and non-interactive shells that may not inherit the env var.
//!
//! **Opt out:** Delete `.cqs/telemetry.jsonl` and unset `CQS_TELEMETRY`.
//!
//! Local file only. No network calls. Auto-archives at 10 MB.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

/// Maximum telemetry file size before auto-archiving (10 MB).
const MAX_TELEMETRY_BYTES: u64 = 10 * 1024 * 1024;

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
    // Active if env var is set OR telemetry file already exists (opt-in persists)
    let path = cqs_dir.join("telemetry.jsonl");
    if std::env::var("CQS_TELEMETRY").as_deref() != Ok("1") && !path.exists() {
        return;
    }

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": command,
        "query": query,
        "results": result_count,
    });

    // path already declared above for existence check
    let _ = (|| -> std::io::Result<()> {
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
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::debug!(path = %path.display(), error = %e, "Failed to set file permissions");
            }
        }
        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry entry");
        }
        Ok(())
    })();
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
    let path = cqs_dir.join("telemetry.jsonl");
    if std::env::var("CQS_TELEMETRY").as_deref() != Ok("1") && !path.exists() {
        return;
    }

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let entry = serde_json::json!({
        "ts": timestamp,
        "cmd": "search",
        "query": query,
        "category": category,
        "confidence": confidence,
        "strategy": strategy,
        "fallback": fallback,
        "results": result_count,
    });

    let _ = (|| -> std::io::Result<()> {
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

        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::debug!(path = %path.display(), error = %e, "Failed to set file permissions");
            }
        }
        if let Err(e) = writeln!(file, "{}", entry) {
            tracing::warn!(error = %e, "Failed to write telemetry entry");
        }
        Ok(())
    })();
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
