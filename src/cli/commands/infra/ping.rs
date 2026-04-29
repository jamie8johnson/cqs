//! `cqs ping` — daemon healthcheck.
//!
//! Task B2: bypasses the normal store-opening dispatch path. Connects
//! directly to the daemon socket, sends `{"command":"ping","args":[]}`,
//! and prints the resulting [`PingResponse`] either as a human-friendly
//! text block or as raw JSON.
//!
//! Reasons to special-case this in the CLI:
//!
//! - Must work on a fresh project (no `cqs init` / `cqs index` yet) so we
//!   cannot open a `Store` during dispatch — going through the regular
//!   `try_daemon_query` path would be fine, but going through Group B's
//!   `CommandContext::open_readonly` would not.
//! - Needs explicit "no daemon running" exit code (1) and message; the
//!   regular daemon-forward path silently falls back to CLI on a missing
//!   socket, which is the wrong behaviour for a healthcheck.
//! - Reuses the [`daemon_ping`] library helper so Task B1
//!   (`cqs doctor --verbose`) can call the same helper without
//!   duplicating socket I/O code.
//!
//! [`PingResponse`]: cqs::daemon_translate::PingResponse
//! [`daemon_ping`]: cqs::daemon_translate::daemon_ping

use anyhow::Result;

use crate::cli::find_project_root;

/// Format a `uptime_secs` value as `"<hours>h <minutes>m"` / `"<minutes>m <seconds>s"` / `"<seconds>s"`.
///
/// Compact form so it fits on a single text line. Avoids depending on
/// `humantime` for one trivial helper. Ranges:
///
/// - `≥ 3600s`: `2h 35m`
/// - `≥ 60s`:   `2m 30s`
/// - `< 60s`:   `45s`
fn format_uptime(secs: u64) -> String {
    if secs >= 3600 {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        format!("{}h {}m", hours, minutes)
    } else if secs >= 60 {
        let minutes = secs / 60;
        let seconds = secs % 60;
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", secs)
    }
}

/// Format a unix timestamp as `"<RFC3339-UTC> (<relative>)"`.
///
/// Returns `"unknown"` when `last_indexed_at` is `None`. The relative
/// component is hand-rolled (same scale buckets as `format_uptime`) so we
/// don't grow a chrono dep just for `"4m ago"`.
fn format_last_indexed(ts: Option<i64>, now_secs: i64) -> String {
    let Some(ts) = ts else {
        return "unknown".to_string();
    };
    let utc = format_unix_utc(ts);
    let relative = if now_secs >= ts {
        let ago = (now_secs - ts) as u64;
        format!("{} ago", format_uptime(ago))
    } else {
        // Clock skew between daemon host and CLI host (extremely rare on
        // the same machine, but possible if the daemon is on WSL and the
        // user's clock just got ntp-corrected). Don't pretend we know.
        "in the future?".to_string()
    };
    format!("{} ({})", utc, relative)
}

/// Format a unix timestamp as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// Uses `time` only via the timestamp arithmetic — we don't pull in chrono
/// just for this. The math is gregorian-correct from 1970 to ~9999.
fn format_unix_utc(ts: i64) -> String {
    // Clamp negative timestamps to epoch for display sanity. A negative
    // mtime means "before 1970" which can only happen if the FS is lying.
    let ts = ts.max(0) as u64;
    let secs_per_day: u64 = 86_400;
    let days = ts / secs_per_day;
    let secs_of_day = ts % secs_per_day;
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // Gregorian year/month/day from `days` since 1970-01-01.
    // Howard Hinnant's `civil_from_days` algorithm (public domain).
    // See: http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    if month <= 2 {
        year += 1;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hour, minute, second
    )
}

/// Print the ping response as a multi-line text report.
///
/// Output shape (matches Task B2 spec):
/// ```text
/// daemon: running
/// uptime: 2h 35m
/// model: BAAI/bge-large-en-v1.5 (1024-dim)
/// last indexed: 2026-04-16 18:42:12 UTC (4m ago)
/// queries: 12,453 served (3 errors)
/// loaded: splade=yes reranker=no
/// ```
fn print_text(resp: &cqs::daemon_translate::PingResponse) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    println!("daemon: running");
    println!("uptime: {}", format_uptime(resp.uptime_secs));
    println!("model: {} ({}-dim)", resp.model, resp.dim);
    println!(
        "last indexed: {}",
        format_last_indexed(resp.last_indexed_at, now_secs)
    );
    println!(
        "queries: {} served ({} errors)",
        format_thousands(resp.total_queries),
        resp.error_count
    );
    println!(
        "loaded: splade={} reranker={}",
        if resp.splade_loaded { "yes" } else { "no" },
        if resp.reranker_loaded { "yes" } else { "no" },
    );
}

/// Comma-thousands separator for human-friendly counters (`12453 → 12,453`).
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Run `cqs ping` — connect to the daemon socket and print its state.
///
/// On Unix: dispatches via [`cqs::daemon_translate::daemon_ping`]. On
/// Windows / non-unix: prints "not supported" to stderr and exits 1
/// because the daemon socket is unix-only.
///
/// Exit codes:
/// - `0` — daemon responded with a valid `PingResponse`.
/// - `1` — no daemon running, or transport / parse error.
pub(crate) fn cmd_ping(json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_ping", json).entered();

    #[cfg(unix)]
    {
        let root = find_project_root();
        let cqs_dir = cqs::resolve_index_dir(&root);
        match cqs::daemon_translate::daemon_ping(&cqs_dir) {
            Ok(resp) => {
                if json {
                    crate::cli::json_envelope::emit_json(&resp)?;
                } else {
                    print_text(&resp);
                }
                Ok(())
            }
            Err(err) => {
                // Spec: "no daemon running" → exit 1. PR #1038 envelope contract
                // requires `{data:null, error:{code,message}, version:1}` on JSON
                // failure paths so health-monitor scripts get a single uniform
                // shape. IO_ERROR is the right code: socket connection failures
                // and timeouts both fall under filesystem/socket I/O.
                let msg = err.as_message();
                if json {
                    crate::cli::json_envelope::emit_json_error(
                        crate::cli::json_envelope::error_codes::IO_ERROR,
                        &msg,
                    )?;
                } else {
                    eprintln!("cqs: {msg}");
                }
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = json;
        let _ = find_project_root;
        eprintln!("cqs: ping is unix-only (daemon socket uses Unix domain sockets)");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_uptime_seconds() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(59), "59s");
    }

    #[test]
    fn format_uptime_minutes() {
        assert_eq!(format_uptime(60), "1m 0s");
        assert_eq!(format_uptime(150), "2m 30s");
        assert_eq!(format_uptime(3599), "59m 59s");
    }

    #[test]
    fn format_uptime_hours() {
        assert_eq!(format_uptime(3600), "1h 0m");
        assert_eq!(format_uptime(9_375), "2h 36m"); // 2h 36m 15s
        assert_eq!(format_uptime(7200), "2h 0m");
    }

    #[test]
    fn format_thousands_small() {
        assert_eq!(format_thousands(0), "0");
        assert_eq!(format_thousands(42), "42");
        assert_eq!(format_thousands(999), "999");
    }

    #[test]
    fn format_thousands_separator() {
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(12_453), "12,453");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
        assert_eq!(format_thousands(u64::MAX), "18,446,744,073,709,551,615");
    }

    /// Pin the gregorian conversion against known timestamps verified
    /// against `date -u -d @<ts>`. Catches drift in Hinnant's algorithm.
    #[test]
    fn format_unix_utc_known_date() {
        // 2024-12-13 20:00:00 UTC — middle of the year, after epoch.
        assert_eq!(format_unix_utc(1_734_120_000), "2024-12-13 20:00:00 UTC");
        // Epoch itself.
        assert_eq!(format_unix_utc(0), "1970-01-01 00:00:00 UTC");
        // Negative clamps to epoch.
        assert_eq!(format_unix_utc(-100), "1970-01-01 00:00:00 UTC");
        // 2024-02-29 (leap year) at 12:34:56 UTC — exercises the
        // mp<10/mp>=10 month branch and Feb→Jan-of-next-year edge.
        // 2024-02-29T12:34:56 UTC = 1709210096.
        assert_eq!(format_unix_utc(1_709_210_096), "2024-02-29 12:34:56 UTC");
    }

    #[test]
    fn format_last_indexed_none_is_unknown() {
        assert_eq!(format_last_indexed(None, 1_000_000), "unknown");
    }

    #[test]
    fn format_last_indexed_relative() {
        // "300s ago" → "5m 0s ago". Pin the structure.
        let s = format_last_indexed(Some(1_000_000 - 300), 1_000_000);
        assert!(s.contains("ago"), "expected 'ago' in {s}");
        assert!(s.contains("5m"), "expected '5m' in {s}");
    }

    #[test]
    fn format_last_indexed_future_clock_skew() {
        // ts > now means clock skew between daemon and CLI hosts. Don't
        // print a negative duration.
        let s = format_last_indexed(Some(1_000_500), 1_000_000);
        assert!(s.contains("future"), "expected 'future?' marker in {s}");
    }
}
