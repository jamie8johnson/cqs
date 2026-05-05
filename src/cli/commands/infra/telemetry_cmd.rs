//! Telemetry dashboard — usage patterns at a glance.
//!
//! Core struct is [`TelemetryOutput`]; build with [`build_telemetry`].
//! CLI uses `print_telemetry_text()` for human output, JSON path serializes directly.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

// ---------------------------------------------------------------------------
// Output structs
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct TopQuery {
    pub query: String,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DateRange {
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct TelemetryOutput {
    pub events: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_range: Option<DateRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<usize>,
    pub commands: HashMap<String, usize>,
    pub categories: HashMap<String, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub top_queries: Vec<TopQuery>,
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// Command categories for grouping telemetry data.
fn category_for(cmd: &str) -> &'static str {
    match cmd {
        "search" | "gather" | "scout" | "onboard" | "where" | "related" | "similar" => "Search",
        "callers" | "callees" | "impact" | "impact-diff" | "test-map" | "deps" | "trace"
        | "explain" | "context" | "dead" => "Structural",
        "task" | "review" | "plan" | "ci" => "Orchestrator",
        "read" | "notes" | "blame" | "diff" | "drift" | "stale" | "suggest" | "reconstruct" => {
            "Read/Write"
        }
        _ => "Infra",
    }
}

/// Category display order (most interesting first).
const CATEGORY_ORDER: &[&str] = &[
    "Orchestrator",
    "Search",
    "Structural",
    "Read/Write",
    "Infra",
];

#[derive(Debug, serde::Deserialize)]
struct RawEntry {
    #[serde(default)]
    cmd: Option<String>,
    #[serde(default)]
    event: Option<String>,
    ts: u64,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug)]
enum Entry {
    Command {
        cmd: String,
        query: Option<String>,
        ts: u64,
    },
    Reset {
        ts: u64,
        _reason: Option<String>,
    },
}

fn parse_entries(path: &Path) -> Result<Vec<Entry>> {
    let file = fs::File::open(path).with_context(|| format!("Cannot open {}", path.display()))?;
    let reader = std::io::BufReader::new(file);
    let mut entries = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(raw) = serde_json::from_str::<RawEntry>(&line) {
            if raw.event.is_some() {
                entries.push(Entry::Reset {
                    ts: raw.ts,
                    _reason: raw.reason,
                });
            } else if let Some(cmd) = raw.cmd {
                entries.push(Entry::Command {
                    cmd,
                    query: raw.query,
                    ts: raw.ts,
                });
            }
        }
    }
    Ok(entries)
}

/// Detect sessions by splitting on reset events or 4-hour gaps.
///
/// RB-9: starts at 0 sessions; first Command opens session 1.
/// Reset events or gaps > 4 hours open a new session.
///
/// Production code uses the inlined version in [`TelemetryAggregator::push`].
/// This standalone function is retained for direct unit tests.
#[cfg(test)]
fn count_sessions(entries: &[Entry]) -> usize {
    const GAP_SECS: u64 = 4 * 3600;
    let mut sessions = 0usize;
    let mut last_ts: Option<u64> = None;
    for entry in entries {
        let ts = match entry {
            Entry::Command { ts, .. } => {
                if sessions == 0 {
                    sessions = 1;
                }
                *ts
            }
            Entry::Reset { ts, .. } => {
                if sessions > 0 {
                    sessions += 1;
                }
                last_ts = Some(*ts);
                continue;
            }
        };
        if let Some(prev) = last_ts {
            if ts.saturating_sub(prev) > GAP_SECS {
                sessions += 1;
            }
        }
        last_ts = Some(ts);
    }
    sessions
}

fn format_ts(ts: u64) -> String {
    // Simple date formatting without chrono dep
    let secs = ts as i64;
    let days_since_epoch = secs / 86400;
    // Zeller-like calculation for year/month/day
    let mut y = 1970i64;
    let mut remaining = days_since_epoch;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    for (i, &days) in month_days.iter().enumerate() {
        if remaining < days {
            m = i;
            break;
        }
        remaining -= days;
    }
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{} {:02}", month_names[m], remaining + 1)
}

/// Build a bar string of given width using block characters.
fn bar(width: usize) -> String {
    "█".repeat(width)
}

// ---------------------------------------------------------------------------
// Streaming aggregator (RM-10, RM-11)
// ---------------------------------------------------------------------------

/// Accumulates telemetry stats in a single pass without storing raw entries.
///
/// RM-10: `--all` previously loaded every archived file into one `Vec<Entry>`.
/// RM-11: `build_telemetry` then cloned entries into an intermediate `Vec` and
/// iterated it four separate times. Now we aggregate in one pass per file,
/// keeping only the fixed-size accumulators — not the raw entries.
struct TelemetryAggregator {
    event_count: usize,
    min_ts: u64,
    max_ts: u64,
    cmd_counts: HashMap<String, usize>,
    cat_counts: HashMap<String, usize>,
    query_counts: HashMap<String, usize>,
    // Session tracking (inlined from count_sessions)
    sessions: usize,
    last_ts: Option<u64>,
}

impl TelemetryAggregator {
    const GAP_SECS: u64 = 4 * 3600;

    fn new() -> Self {
        Self {
            event_count: 0,
            min_ts: u64::MAX,
            max_ts: 0,
            cmd_counts: HashMap::with_capacity(32),
            cat_counts: HashMap::with_capacity(8),
            query_counts: HashMap::with_capacity(64),
            sessions: 0,
            last_ts: None,
        }
    }

    /// Feed a single entry into the aggregator. Entry is not retained.
    fn push(&mut self, entry: &Entry) {
        match entry {
            Entry::Command { cmd, query, ts } => {
                self.event_count += 1;
                let ts = *ts;

                // Date range
                if ts < self.min_ts {
                    self.min_ts = ts;
                }
                if ts > self.max_ts {
                    self.max_ts = ts;
                }

                // Command + category counts (single pass).
                // Use get_mut to avoid cloning the key on every hit.
                if let Some(c) = self.cmd_counts.get_mut(cmd.as_str()) {
                    *c += 1;
                } else {
                    self.cmd_counts.insert(cmd.clone(), 1);
                }
                let cat = category_for(cmd);
                if let Some(c) = self.cat_counts.get_mut(cat) {
                    *c += 1;
                } else {
                    self.cat_counts.insert(cat.to_string(), 1);
                }

                // Query counts
                if let Some(q) = query {
                    if !q.is_empty() {
                        if let Some(c) = self.query_counts.get_mut(q.as_str()) {
                            *c += 1;
                        } else {
                            self.query_counts.insert(q.clone(), 1);
                        }
                    }
                }

                // Session tracking (inlined from count_sessions)
                if self.sessions == 0 {
                    self.sessions = 1;
                }
                if let Some(prev) = self.last_ts {
                    if ts.saturating_sub(prev) > Self::GAP_SECS {
                        self.sessions += 1;
                    }
                }
                self.last_ts = Some(ts);
            }
            Entry::Reset { ts, .. } => {
                if self.sessions > 0 {
                    self.sessions += 1;
                }
                self.last_ts = Some(*ts);
            }
        }
    }

    /// Feed all entries from a slice. Entries are borrowed, not stored.
    fn push_all(&mut self, entries: &[Entry]) {
        for entry in entries {
            self.push(entry);
        }
    }

    /// Consume the aggregator and produce the final output.
    fn finish(self) -> TelemetryOutput {
        if self.event_count == 0 {
            return TelemetryOutput {
                events: 0,
                date_range: None,
                sessions: None,
                commands: HashMap::new(),
                categories: HashMap::new(),
                top_queries: Vec::new(),
            };
        }

        // Top queries (sorted descending, capped at 10)
        let mut query_sorted: Vec<_> = self.query_counts.into_iter().collect();
        query_sorted.sort_by_key(|q| std::cmp::Reverse(q.1));

        TelemetryOutput {
            events: self.event_count,
            date_range: Some(DateRange {
                from: self.min_ts,
                to: self.max_ts,
            }),
            sessions: Some(self.sessions),
            commands: self.cmd_counts,
            categories: self.cat_counts,
            top_queries: query_sorted
                .into_iter()
                .take(10)
                .map(|(q, c)| TopQuery { query: q, count: c })
                .collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Builder (delegates to aggregator)
// ---------------------------------------------------------------------------

/// Build telemetry output from parsed entries.
///
/// Production code uses [`TelemetryAggregator`] directly in `cmd_telemetry`.
/// This convenience wrapper is retained for tests.
#[cfg(test)]
fn build_telemetry(entries: &[Entry]) -> TelemetryOutput {
    let mut agg = TelemetryAggregator::new();
    agg.push_all(entries);
    agg.finish()
}

// ---------------------------------------------------------------------------
// CLI command
// ---------------------------------------------------------------------------

pub(crate) fn cmd_telemetry(cqs_dir: &Path, json: bool, all: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_telemetry").entered();

    // RM-10: streaming aggregation — each file's entries are fed into the
    // aggregator then dropped, so --all never holds all files in memory at once.
    let mut agg = TelemetryAggregator::new();

    if all {
        // Read all telemetry files (archived + current), one at a time
        if let Ok(dir) = fs::read_dir(cqs_dir) {
            let mut paths: Vec<_> = dir
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("telemetry") && n.ends_with(".jsonl"))
                })
                .map(|e| e.path())
                .collect();
            paths.sort();
            for path in paths {
                match parse_entries(&path) {
                    Ok(entries) => {
                        agg.push_all(&entries);
                        // entries dropped here — not accumulated
                    }
                    Err(err) => tracing::warn!(path = %path.display(), error = %err, "Skipping"),
                }
            }
        }
    } else {
        let path = cqs_dir.join("telemetry.jsonl");
        if path.exists() {
            let entries = parse_entries(&path)?;
            agg.push_all(&entries);
        }
    }

    let output = agg.finish();

    if output.events == 0 {
        if json {
            crate::cli::json_envelope::emit_json(&output)?;
        } else {
            println!("No telemetry data. Set CQS_TELEMETRY=1 to enable.");
        }
        return Ok(());
    }

    if json {
        crate::cli::json_envelope::emit_json(&output)?;
    } else {
        print_telemetry_text(&output);
    }

    Ok(())
}

/// Format the "Sessions:" line, guarding against divide-by-zero when
/// `sessions == 0` (RB-V1.33-6: a telemetry log can record session-id rows
/// before any command event lands).
fn format_sessions_line(sessions: usize, total: usize) -> String {
    if sessions > 0 {
        format!(
            "Sessions: {} (avg {:.0} events/session)",
            sessions,
            total as f64 / sessions as f64,
        )
    } else {
        "Sessions: 0".to_string()
    }
}

/// Render telemetry as human-readable text with bar charts.
fn print_telemetry_text(output: &TelemetryOutput) {
    let total = output.events;

    // Sort commands by count descending for display
    let mut cmd_sorted: Vec<_> = output.commands.iter().collect();
    cmd_sorted.sort_by(|a, b| b.1.cmp(a.1));

    // Header
    if let Some(ref dr) = output.date_range {
        let days = (dr.to.saturating_sub(dr.from)) / 86400 + 1;
        println!(
            "{}: {} events over {} day{} ({} – {})",
            "Telemetry".bold(),
            total,
            days,
            if days == 1 { "" } else { "s" },
            format_ts(dr.from),
            format_ts(dr.to),
        );
    } else {
        println!("{}: {} events", "Telemetry".bold(), total);
    }
    println!();

    // Command frequency with bar chart
    let max_count = cmd_sorted.first().map(|(_, &c)| c).unwrap_or(1);
    let bar_max = 20usize;
    println!("{}:", "Command Usage".cyan());
    for (cmd, &count) in &cmd_sorted {
        let bar_width = (count * bar_max) / max_count.max(1);
        // Guard against total==0 producing NaN% — sibling guard to
        // the sessions divisor protected by the if-block above.
        let pct = if total > 0 {
            (count as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<14} {:>4}  {}  ({:.1}%)",
            cmd,
            count,
            bar(bar_width).blue(),
            pct,
        );
    }
    println!();

    // Categories
    println!("{}:", "Categories".cyan());
    for &cat in CATEGORY_ORDER {
        let count = output.categories.get(cat).copied().unwrap_or(0);
        if count > 0 {
            let pct = (count as f64 / total as f64) * 100.0;
            let label = match cat {
                "Orchestrator" => {
                    if pct < 5.0 {
                        format!("{:.0}%", pct).red().to_string()
                    } else {
                        format!("{:.0}%", pct).green().to_string()
                    }
                }
                _ => format!("{:.0}%", pct),
            };
            println!("  {:<14} {:>4}  ({})", cat, count, label);
        }
    }
    println!();

    // Sessions
    if let Some(sessions) = output.sessions {
        println!("{}", format_sessions_line(sessions, total));
    }

    // Top queries
    if !output.top_queries.is_empty() {
        println!();
        println!("{}:", "Top Queries".cyan());
        for tq in &output.top_queries {
            // RB-7: char-boundary-safe truncation (avoids panic on multi-byte UTF-8)
            let display = if tq.query.len() > 50 {
                let truncated: String = tq.query.chars().take(47).collect();
                format!("{truncated}...")
            } else {
                tq.query.clone()
            };
            println!("  {:>4}  {}", tq.count, display);
        }
    }
}

pub(crate) fn cmd_telemetry_reset(cqs_dir: &Path, reason: Option<&str>, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_telemetry_reset", json).entered();

    let current = cqs_dir.join("telemetry.jsonl");
    if !current.exists() {
        if json {
            // API-V1.29-3: emit an envelope even when there is nothing to
            // archive so `--json` consumers always get a parseable document.
            crate::cli::json_envelope::emit_json(&serde_json::json!({
                "archived_events": 0,
                "archive_path": serde_json::Value::Null,
                "lock_path": cqs_dir.join("telemetry.lock").display().to_string(),
            }))?;
        } else {
            println!("No telemetry file to reset.");
        }
        return Ok(());
    }

    // DS-NEW-2: advisory file lock to prevent races with concurrent log_command
    let lock_path = cqs_dir.join("telemetry.lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .context("Failed to create telemetry lock file")?;
    lock_file
        .lock()
        .context("Failed to acquire telemetry lock")?;

    // SEC-7 / RM-11: count lines via BufReader, never load entire file into memory
    let line_count = {
        let f = fs::File::open(&current)
            .with_context(|| format!("Cannot open {}", current.display()))?;
        BufReader::new(f).lines().count()
    };

    // Archive with timestamp
    let now = format_utc_timestamp();
    let archive = cqs_dir.join(format!("telemetry_{now}.jsonl"));
    fs::copy(&current, &archive)
        .with_context(|| format!("Failed to archive to {}", archive.display()))?;

    // Write reset event
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let reason_str = reason.unwrap_or("manual reset");
    let entry = serde_json::json!({
        "event": "reset",
        "ts": timestamp,
        "reason": reason_str,
    });
    fs::write(&current, format!("{}\n", entry)).context("Failed to write reset event")?;

    if json {
        // API-V1.29-3: `cqs telemetry --reset --json` used to silently drop
        // the --json flag and print a human-readable line. Emit the envelope
        // consumers expect: line count, the archive file actually written,
        // and the advisory lock path so debugging concurrent-reset races is
        // easy.
        crate::cli::json_envelope::emit_json(&serde_json::json!({
            "archived_events": line_count,
            "archive_path": archive.display().to_string(),
            "lock_path": lock_path.display().to_string(),
        }))?;
    } else {
        println!(
            "Archived {} events to {}",
            line_count,
            archive.file_name().unwrap_or_default().to_string_lossy(),
        );
    }

    // lock_file dropped here, releasing advisory lock
    drop(lock_file);
    Ok(())
}

/// Produce a YYYYMMDD_HHMMSS UTC timestamp in pure Rust.
///
/// PB-10: no longer spawns POSIX `date` — works on all platforms.
fn format_utc_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;

    // Decompose epoch seconds into date/time components (UTC)
    let secs_of_day = secs.rem_euclid(86400);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let days_since_epoch = secs.div_euclid(86400);
    let mut y = 1970i64;
    let mut remaining = days_since_epoch;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0u32;
    for (i, &days) in month_days.iter().enumerate() {
        if remaining < days {
            m = i as u32;
            break;
        }
        remaining -= days;
    }
    let day = remaining + 1;

    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        y,
        m + 1,
        day,
        hour,
        minute,
        second,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_test_telemetry(dir: &Path, lines: &[&str]) {
        let path = dir.join("telemetry.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for line in lines {
            writeln!(f, "{line}").unwrap();
        }
    }

    #[test]
    fn test_category_assignment() {
        assert_eq!(category_for("search"), "Search");
        assert_eq!(category_for("gather"), "Search");
        assert_eq!(category_for("callers"), "Structural");
        assert_eq!(category_for("impact"), "Structural");
        assert_eq!(category_for("task"), "Orchestrator");
        assert_eq!(category_for("review"), "Orchestrator");
        assert_eq!(category_for("read"), "Read/Write");
        assert_eq!(category_for("notes"), "Read/Write");
        assert_eq!(category_for("index"), "Infra");
        assert_eq!(category_for("unknown_cmd"), "Infra");
    }

    #[test]
    fn test_parse_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_test_telemetry(
            dir.path(),
            &[
                r#"{"event":"reset","ts":1000,"reason":"test"}"#,
                r#"{"cmd":"search","query":"foo","ts":1001}"#,
                r#"{"cmd":"impact","query":"bar","results":5,"ts":1002}"#,
            ],
        );
        let entries = parse_entries(&dir.path().join("telemetry.jsonl")).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(&entries[0], Entry::Reset { _reason: Some(r), .. } if r == "test"));
        assert!(matches!(&entries[1], Entry::Command { cmd, .. } if cmd == "search"));
        assert!(matches!(&entries[2], Entry::Command { cmd, .. } if cmd == "impact"));
    }

    #[test]
    fn test_count_sessions_by_reset() {
        let entries = vec![
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000,
            },
            Entry::Reset {
                ts: 2000,
                _reason: None,
            },
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 2001,
            },
        ];
        assert_eq!(count_sessions(&entries), 2);
    }

    #[test]
    fn test_count_sessions_by_gap() {
        let entries = vec![
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000,
            },
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000 + 5 * 3600,
            },
        ];
        // 5-hour gap > 4-hour threshold → 2 sessions
        assert_eq!(count_sessions(&entries), 2);
    }

    #[test]
    fn test_count_sessions_no_gap() {
        let entries = vec![
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000,
            },
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000 + 3600,
            },
        ];
        // 1-hour gap < 4-hour threshold → 1 session
        assert_eq!(count_sessions(&entries), 1);
    }

    #[test]
    fn test_format_ts() {
        // 2026-04-02 = some known timestamp
        let ts = 1774917165; // from test data
        let formatted = format_ts(ts);
        // Should contain a month abbreviation and day
        assert!(formatted.len() >= 5); // "Mon DD"
    }

    #[test]
    fn test_empty_telemetry_json() {
        let dir = tempfile::tempdir().unwrap();
        write_test_telemetry(dir.path(), &[]);
        // Should not error on empty file
        let result = cmd_telemetry(dir.path(), true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_reset_archives_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        write_test_telemetry(
            dir.path(),
            &[
                r#"{"cmd":"search","query":"foo","ts":1000}"#,
                r#"{"cmd":"impact","query":"bar","ts":1001}"#,
            ],
        );

        cmd_telemetry_reset(dir.path(), Some("test reset"), false).unwrap();

        // Current file should have just the reset event
        let current = fs::read_to_string(dir.path().join("telemetry.jsonl")).unwrap();
        assert!(current.contains("reset"));
        assert!(current.contains("test reset"));
        assert_eq!(current.lines().count(), 1);

        // Archive should exist
        let archives: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with("telemetry_") && name.ends_with(".jsonl")
            })
            .collect();
        assert_eq!(archives.len(), 1);
    }

    #[test]
    fn test_telemetry_output_serialization() {
        let mut commands = HashMap::new();
        commands.insert("search".to_string(), 10);
        commands.insert("impact".to_string(), 5);

        let mut categories = HashMap::new();
        categories.insert("Search".to_string(), 10);
        categories.insert("Structural".to_string(), 5);

        let output = TelemetryOutput {
            events: 15,
            date_range: Some(DateRange {
                from: 1000,
                to: 2000,
            }),
            sessions: Some(3),
            commands,
            categories,
            top_queries: vec![
                TopQuery {
                    query: "foo bar".to_string(),
                    count: 5,
                },
                TopQuery {
                    query: "baz".to_string(),
                    count: 2,
                },
            ],
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["events"], 15);
        assert_eq!(json["date_range"]["from"], 1000);
        assert_eq!(json["date_range"]["to"], 2000);
        assert_eq!(json["sessions"], 3);
        assert_eq!(json["commands"]["search"], 10);
        assert_eq!(json["categories"]["Search"], 10);
        assert_eq!(json["top_queries"][0]["query"], "foo bar");
        assert_eq!(json["top_queries"][0]["count"], 5);
    }

    #[test]
    fn test_telemetry_output_empty() {
        let output = TelemetryOutput {
            events: 0,
            date_range: None,
            sessions: None,
            commands: HashMap::new(),
            categories: HashMap::new(),
            top_queries: Vec::new(),
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["events"], 0);
        // Optional fields omitted when None/empty
        assert!(json.get("date_range").is_none());
        assert!(json.get("sessions").is_none());
        assert!(json.get("top_queries").is_none());
    }

    #[test]
    fn test_build_telemetry_with_data() {
        let entries = vec![
            Entry::Command {
                cmd: "search".into(),
                query: Some("foo".into()),
                ts: 1000,
            },
            Entry::Command {
                cmd: "search".into(),
                query: Some("bar".into()),
                ts: 1001,
            },
            Entry::Command {
                cmd: "impact".into(),
                query: Some("baz".into()),
                ts: 1002,
            },
        ];

        let output = build_telemetry(&entries);
        assert_eq!(output.events, 3);
        assert_eq!(output.commands.get("search"), Some(&2));
        assert_eq!(output.commands.get("impact"), Some(&1));
        assert_eq!(output.categories.get("Search"), Some(&2));
        assert_eq!(output.categories.get("Structural"), Some(&1));
        assert!(output.date_range.is_some());
        let dr = output.date_range.unwrap();
        assert_eq!(dr.from, 1000);
        assert_eq!(dr.to, 1002);
        assert_eq!(output.top_queries.len(), 3);
    }

    #[test]
    fn test_build_telemetry_empty() {
        let entries: Vec<Entry> = vec![];
        let output = build_telemetry(&entries);
        assert_eq!(output.events, 0);
        assert!(output.date_range.is_none());
        assert!(output.sessions.is_none());
        assert!(output.commands.is_empty());
    }

    // RB-9: count_sessions should return 0 for empty entries
    #[test]
    fn test_count_sessions_empty() {
        assert_eq!(count_sessions(&[]), 0);
    }

    // RB-V1.33-6: format_sessions_line must not produce inf/NaN strings
    // when sessions == 0 (or sessions > 0 but total > 0 — normal case).
    #[test]
    fn test_format_sessions_line_zero_sessions() {
        // Both zero — the corner case the audit flagged.
        let line = format_sessions_line(0, 0);
        assert_eq!(line, "Sessions: 0");
        assert!(!line.contains("NaN"));
        assert!(!line.contains("inf"));

        // Sessions == 0 but total > 0 (telemetry log with session-id rows
        // before any command event lands).
        let line = format_sessions_line(0, 5);
        assert_eq!(line, "Sessions: 0");
    }

    #[test]
    fn test_format_sessions_line_normal() {
        let line = format_sessions_line(3, 12);
        assert_eq!(line, "Sessions: 3 (avg 4 events/session)");
    }

    // RB-9: resets before any command should not inflate session count
    #[test]
    fn test_count_sessions_leading_resets() {
        let entries = vec![
            Entry::Reset {
                ts: 500,
                _reason: None,
            },
            Entry::Reset {
                ts: 600,
                _reason: None,
            },
            Entry::Command {
                cmd: "search".into(),
                query: None,
                ts: 1000,
            },
        ];
        // Two resets before any command, then one command = 1 session
        assert_eq!(count_sessions(&entries), 1);
    }

    // RB-9: only-resets (no commands) should return 0 sessions
    #[test]
    fn test_count_sessions_only_resets() {
        let entries = vec![
            Entry::Reset {
                ts: 500,
                _reason: None,
            },
            Entry::Reset {
                ts: 600,
                _reason: None,
            },
        ];
        assert_eq!(count_sessions(&entries), 0);
    }

    // RB-7: multi-byte UTF-8 query truncation must not panic
    #[test]
    fn test_truncation_multibyte_utf8() {
        // Build a query with multi-byte chars that would panic with &query[..47]
        // Each emoji is 4 bytes, so 13 emojis = 52 bytes > 50
        let emoji_query = "\u{1F600}".repeat(13); // 52 bytes, 13 chars
        assert!(emoji_query.len() > 50);

        let output = TelemetryOutput {
            events: 1,
            date_range: Some(DateRange {
                from: 1000,
                to: 1000,
            }),
            sessions: Some(1),
            commands: {
                let mut m = HashMap::new();
                m.insert("search".to_string(), 1);
                m
            },
            categories: {
                let mut m = HashMap::new();
                m.insert("Search".to_string(), 1);
                m
            },
            top_queries: vec![TopQuery {
                query: emoji_query,
                count: 1,
            }],
        };

        // This previously panicked; now it should succeed
        print_telemetry_text(&output);
    }

    // PB-10: format_utc_timestamp produces YYYYMMDD_HHMMSS pattern
    #[test]
    fn test_format_utc_timestamp() {
        let ts = format_utc_timestamp();
        assert_eq!(ts.len(), 15); // "YYYYMMDD_HHMMSS"
        assert_eq!(ts.as_bytes()[8], b'_');
        // All other positions are digits
        for (i, b) in ts.bytes().enumerate() {
            if i == 8 {
                continue;
            }
            assert!(
                b.is_ascii_digit(),
                "Expected digit at position {i}, got '{}'",
                b as char
            );
        }
    }
}
