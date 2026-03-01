//! Blame command — semantic git blame for a function
//!
//! Core logic is in `build_blame_data()` so batch mode can reuse it.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::store::{CallerInfo, ChunkSummary, Store};
use cqs::{normalize_path, rel_display, resolve_target};

use crate::cli::Cli;

// ─── Data structures ─────────────────────────────────────────────────────────

/// A single git commit that touched the function's line range.
pub(crate) struct BlameEntry {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
}

/// All data needed to render blame output (JSON or terminal).
pub(crate) struct BlameData {
    pub chunk: ChunkSummary,
    pub commits: Vec<BlameEntry>,
    pub callers: Vec<CallerInfo>,
}

// ─── Core logic ──────────────────────────────────────────────────────────────

/// Build blame data: resolve target, run git log -L, parse commits, optionally
/// fetch callers.
pub(crate) fn build_blame_data(
    store: &Store,
    root: &Path,
    target: &str,
    depth: usize,
    show_callers: bool,
) -> Result<BlameData> {
    let _span = tracing::info_span!("build_blame_data", target, depth).entered();

    let resolved = resolve_target(store, target)
        .map_err(|e| anyhow::anyhow!("{}", e))
        .context("Failed to resolve blame target")?;

    let chunk = resolved.chunk;
    let rel_file = rel_display(&chunk.file, root);

    let output = run_git_log_line_range(root, &rel_file, chunk.line_start, chunk.line_end, depth)?;
    let commits = parse_git_log_output(&output);

    let callers = if show_callers {
        store.get_callers_full(&chunk.name).unwrap_or_else(|e| {
            tracing::warn!(error = %e, name = %chunk.name, "Failed to fetch callers");
            Vec::new()
        })
    } else {
        Vec::new()
    };

    Ok(BlameData {
        chunk,
        commits,
        callers,
    })
}

/// Run `git log -L` for a specific line range and return raw output.
fn run_git_log_line_range(
    root: &Path,
    rel_file: &str,
    start: u32,
    end: u32,
    depth: usize,
) -> Result<String> {
    let _span =
        tracing::info_span!("run_git_log_line_range", file = rel_file, start, end).entered();

    if rel_file.starts_with('-') {
        anyhow::bail!("Invalid file path '{}': must not start with '-'", rel_file);
    }

    let line_range = format!("{},{}:{}", start, end, rel_file);
    let depth_str = depth.to_string();

    let output = std::process::Command::new("git")
        .args(["--no-pager", "log", "--no-patch"])
        .args(["--format=%h%x00%aN%x00%ai%x00%s"])
        .args(["-L", &line_range])
        .args(["-n", &depth_str])
        .current_dir(root)
        .output()
        .context("Failed to run 'git log'. Is git installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();

        if stderr.contains("not a git repository") {
            anyhow::bail!("Not a git repository: {}", root.display());
        }
        if stderr.contains("no path") || stderr.contains("There is no path") {
            anyhow::bail!("File '{}' not found in git history", rel_file);
        }
        if stderr.contains("has only") {
            tracing::warn!(stderr, "Line range may exceed file length");
            // Return empty — no commits touch those lines
            return Ok(String::new());
        }

        anyhow::bail!("git log failed: {}", stderr);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Parse NUL-delimited git log output into BlameEntry list.
///
/// Expected format per line: `hash\0author\0date\0message`
pub(crate) fn parse_git_log_output(output: &str) -> Vec<BlameEntry> {
    let mut entries = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(4, '\0').collect();
        if parts.len() != 4 {
            tracing::warn!(
                line,
                "Skipping malformed git log line (expected 4 NUL-separated fields)"
            );
            continue;
        }

        entries.push(BlameEntry {
            hash: parts[0].to_string(),
            author: parts[1].to_string(),
            date: parts[2].to_string(),
            message: parts[3].to_string(),
        });
    }

    entries
}

// ─── JSON output ─────────────────────────────────────────────────────────────

/// Build JSON output from BlameData.
pub(crate) fn blame_to_json(data: &BlameData, root: &Path) -> serde_json::Value {
    let commits: Vec<serde_json::Value> = data
        .commits
        .iter()
        .map(|c| {
            serde_json::json!({
                "hash": c.hash,
                "author": c.author,
                "date": c.date,
                "message": c.message,
            })
        })
        .collect();

    let mut result = serde_json::json!({
        "function": data.chunk.name,
        "file": normalize_path(&data.chunk.file),
        "lines": [data.chunk.line_start, data.chunk.line_end],
        "signature": data.chunk.signature,
        "commits": commits,
        "total_commits": commits.len(),
    });

    if !data.callers.is_empty() {
        let callers: Vec<serde_json::Value> = data
            .callers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": rel_display(&c.file, root),
                    "line": c.line,
                })
            })
            .collect();
        result["callers"] = serde_json::Value::Array(callers);
    }

    result
}

// ─── Terminal output ─────────────────────────────────────────────────────────

fn print_blame_terminal(data: &BlameData, root: &Path) {
    let file = rel_display(&data.chunk.file, root);
    println!(
        "{} {} ({}:{}-{})",
        "●".bright_blue(),
        data.chunk.name.bold(),
        file.dimmed(),
        data.chunk.line_start,
        data.chunk.line_end,
    );
    println!("  {}", data.chunk.signature.dimmed());
    println!();

    if data.commits.is_empty() {
        println!("  {}", "No git history for this line range.".dimmed());
    } else {
        for entry in &data.commits {
            // Truncate date to just date portion (YYYY-MM-DD)
            let short_date = entry.date.split(' ').next().unwrap_or(&entry.date);
            println!(
                "  {} {} {} {}",
                entry.hash.yellow(),
                short_date.dimmed(),
                entry.author.cyan(),
                entry.message,
            );
        }
    }

    if !data.callers.is_empty() {
        println!();
        println!("  {} ({}):", "Callers".bold(), data.callers.len());
        for caller in &data.callers {
            let caller_file = rel_display(&caller.file, root);
            println!(
                "    {} ({}:{})",
                caller.name.green(),
                caller_file.dimmed(),
                caller.line,
            );
        }
    }
}

// ─── CLI command ─────────────────────────────────────────────────────────────

pub(crate) fn cmd_blame(
    _cli: &Cli,
    target: &str,
    json: bool,
    depth: usize,
    show_callers: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_blame", target).entered();

    let (store, root, _cqs_dir) = crate::cli::open_project_store()?;
    let data = build_blame_data(&store, &root, target, depth, show_callers)?;

    if json {
        let value = blame_to_json(&data, &root);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).context("Failed to serialize blame output")?
        );
    } else {
        print_blame_terminal(&data, &root);
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_git_log_output_single() {
        let output = "abc1234\0Alice\02026-02-20 14:30:00 -0500\0fix: some bug\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc1234");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[0].date, "2026-02-20 14:30:00 -0500");
        assert_eq!(entries[0].message, "fix: some bug");
    }

    #[test]
    fn test_parse_git_log_output_multiple() {
        let output = "abc1234\0Alice\02026-02-20\0first commit\n\
                       def5678\0Bob\02026-02-19\0second commit\n\
                       ghi9012\0Charlie\02026-02-18\0third commit\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].hash, "abc1234");
        assert_eq!(entries[2].author, "Charlie");
    }

    #[test]
    fn test_parse_git_log_output_empty() {
        let entries = parse_git_log_output("");
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_git_log_output_malformed() {
        // Lines without exactly 4 NUL-separated fields are skipped
        let output = "just-a-hash\n\
                       abc1234\0Alice\02026-02-20\0valid line\n\
                       incomplete\0two-parts\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc1234");
    }

    #[test]
    fn test_parse_git_log_output_message_with_pipe() {
        // Pipe in commit message should not break parsing (NUL separator handles it)
        let output = "abc1234\0Alice\02026-02-20\0fix: search | callers pipeline\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "fix: search | callers pipeline");
    }

    #[test]
    fn test_blame_to_json_shape() {
        let data = BlameData {
            chunk: ChunkSummary {
                id: "test-id".to_string(),
                file: PathBuf::from("src/search.rs"),
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                name: "resolve_target".to_string(),
                signature: "pub fn resolve_target(store: &Store, target: &str)".to_string(),
                content: String::new(),
                doc: None,
                line_start: 23,
                line_end: 96,
                parent_id: None,
            },
            commits: vec![BlameEntry {
                hash: "abc1234".to_string(),
                author: "Alice".to_string(),
                date: "2026-02-20".to_string(),
                message: "fix: something".to_string(),
            }],
            callers: vec![CallerInfo {
                name: "cmd_explain".to_string(),
                file: PathBuf::from("src/cli/commands/explain.rs"),
                line: 52,
            }],
        };

        let root = Path::new("");
        let json = blame_to_json(&data, root);

        assert_eq!(json["function"], "resolve_target");
        assert_eq!(json["file"], "src/search.rs");
        assert_eq!(json["lines"][0], 23);
        assert_eq!(json["lines"][1], 96);
        assert_eq!(json["commits"].as_array().unwrap().len(), 1);
        assert_eq!(json["commits"][0]["hash"], "abc1234");
        assert_eq!(json["total_commits"], 1);
        assert_eq!(json["callers"].as_array().unwrap().len(), 1);
        assert_eq!(json["callers"][0]["name"], "cmd_explain");
    }

    #[test]
    fn test_blame_to_json_no_callers() {
        let data = BlameData {
            chunk: ChunkSummary {
                id: "test-id".to_string(),
                file: PathBuf::from("src/lib.rs"),
                language: cqs::language::Language::Rust,
                chunk_type: cqs::language::ChunkType::Function,
                name: "foo".to_string(),
                signature: "fn foo()".to_string(),
                content: String::new(),
                doc: None,
                line_start: 1,
                line_end: 5,
                parent_id: None,
            },
            commits: vec![],
            callers: vec![],
        };

        let root = Path::new("");
        let json = blame_to_json(&data, root);

        assert!(json.get("callers").is_none());
        assert_eq!(json["total_commits"], 0);
    }
}
