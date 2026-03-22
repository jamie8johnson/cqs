//! Optional usage telemetry for understanding how agents use cqs.
//!
//! Logs command invocations to `.cqs/telemetry.jsonl` when `CQS_TELEMETRY=1`.
//! Each entry records: timestamp, command name, query (if any), and result count.
//!
//! Off by default. No network calls — local file only.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::SystemTime;

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
    if std::env::var("CQS_TELEMETRY").as_deref() != Ok("1") {
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

    let path = cqs_dir.join("telemetry.jsonl");
    let _ = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{}", entry)?;
        Ok(())
    })();
}

/// Extract command name and query from CLI args for telemetry.
pub fn describe_command(args: &[String]) -> (String, Option<String>) {
    // args[0] is the binary name
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("unknown");

    // If it's a bare query (no subcommand), it's a search
    if !cmd.starts_with('-') && !cmd.is_empty() {
        // Check if it's a known subcommand
        let subcommands = [
            "init",
            "doctor",
            "index",
            "stats",
            "watch",
            "batch",
            "blame",
            "chat",
            "completions",
            "deps",
            "callers",
            "callees",
            "onboard",
            "notes",
            "ref",
            "diff",
            "drift",
            "explain",
            "similar",
            "impact",
            "impact-diff",
            "review",
            "ci",
            "trace",
            "test-map",
            "context",
            "dead",
            "gather",
            "project",
            "gc",
            "health",
            "audit-mode",
            "stale",
            "suggest",
            "read",
            "related",
            "where",
            "scout",
            "plan",
            "task",
            "convert",
            "train-data",
            "help",
        ];

        if subcommands.contains(&cmd) {
            // It's a subcommand — look for query in remaining args
            let query = args.iter().skip(2).find(|a| !a.starts_with('-')).cloned();
            return (cmd.to_string(), query);
        }

        // Bare query — it's a search
        return ("search".to_string(), Some(cmd.to_string()));
    }

    (cmd.to_string(), None)
}
