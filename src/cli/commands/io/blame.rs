//! Blame command — semantic git blame for a function
//!
//! ## Command-core split (Phase 2b)
//!
//! Blame is display-oriented: the surface-agnostic logic is
//! [`build_blame_data`] (resolve target → `git log -L` → parse → optional
//! callers) and the single JSON-schema source is [`blame_to_json`] (via the
//! borrowing `BlameOutput`). [`blame_core`] is the named entry point both the
//! CLI [`cmd_blame`] and the daemon `dispatch_blame` drive; it takes a typed
//! [`BlameArgs`] and returns the [`BlameData`] the serializer + terminal
//! renderer share. Reads no env.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use cqs::store::{CallerInfo, ChunkSummary, ReadOnly, Store};
use cqs::{normalize_path, rel_display, resolve_target};

// ─── Args (surface-agnostic, MCP-ready) ──────────────────────────────────────

/// Input for [`blame_core`] — the blame knobs both the CLI and a future MCP
/// `blame` tool deserialize into. Store/root come from the adapter.
///
/// `#[serde(default)]` so a wire caller can supply just `name` and inherit the
/// production defaults (commits mirrors clap's `--commits` default of 10).
#[derive(Debug, Clone, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub(crate) struct BlameArgs {
    /// Function name or `file:function` to blame.
    pub name: String,
    /// Max commits to show from `git log -L`.
    pub commits: usize,
    /// Also resolve and show the function's callers.
    pub callers: bool,
}

impl Default for BlameArgs {
    fn default() -> Self {
        BlameArgs {
            name: String::new(),
            commits: 10,
            callers: false,
        }
    }
}

// ─── Core ─────────────────────────────────────────────────────────────────────

/// Surface-agnostic core for `cqs blame`. Thin wrapper over
/// [`build_blame_data`] keyed on the typed [`BlameArgs`]; returns the
/// [`BlameData`] both the JSON serializer ([`blame_to_json`]) and the terminal
/// renderer consume. Reads no env and never prints.
pub(crate) fn blame_core(
    store: &Store<ReadOnly>,
    root: &Path,
    args: &BlameArgs,
) -> Result<BlameData> {
    let _span = tracing::info_span!("blame_core", name = %args.name).entered();
    build_blame_data(store, root, &args.name, args.commits, args.callers)
}

// ─── Data structures ─────────────────────────────────────────────────────────

/// A single git commit that touched the function's line range.
#[derive(Debug, serde::Serialize)]
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
pub(crate) fn build_blame_data<Mode>(
    store: &Store<Mode>,
    root: &Path,
    target: &str,
    depth: usize,
    show_callers: bool,
) -> Result<BlameData> {
    let _span = tracing::info_span!("build_blame_data", target, depth).entered();

    let resolved = resolve_target(store, target).context("Failed to resolve blame target")?;

    let chunk = resolved.chunk;
    let rel_file = rel_display(&chunk.file, root);

    let output = run_git_log_line_range(root, &rel_file, chunk.line_start, chunk.line_end, depth)?;
    let commits = parse_git_log_output(&output);

    let callers = if show_callers {
        store
            .get_callers_full(&chunk.name)
            .with_context(|| format!("Failed to fetch callers for '{}'", chunk.name))?
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

    // Reject embedded colons — git `-L start,end:file` would misparse
    if rel_file.contains(':') {
        anyhow::bail!(
            "Invalid file path '{}': colons not supported (conflicts with git -L syntax)",
            rel_file
        );
    }

    // Reject absolute paths and `..` components. Store-indexed chunks always
    // have project-relative file paths; this is defense-in-depth for any
    // future path where the store gets content from an untrusted source
    // (reference-index merge, imported chunks).
    let p = std::path::Path::new(rel_file);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        anyhow::bail!(
            "Invalid file path '{}': must be project-relative (no absolute paths or '..')",
            rel_file
        );
    }

    // Ensure valid line range (start <= end); swap if inverted
    let (start, end) = if start > end {
        tracing::warn!(start, end, "Inverted line range, swapping");
        (end, start)
    } else {
        (start, end)
    };

    // Normalize backslashes + strip Windows verbatim `\\?\` prefix for git.
    // A bare `replace('\\', "/")` would turn `\\?\C:\...` into `//?/C:/...`,
    // which `git log -L start,end:<path>` parses as a pathspec containing a
    // literal `?`. `normalize_slashes` strips the verbatim prefix first, so
    // the resulting path matches the index entry.
    let git_file = cqs::normalize_slashes(rel_file);
    let line_range = format!("{},{}:{}", start, end, git_file);
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

        let sanitized = truncate_git_stderr(stderr);
        anyhow::bail!("git log failed: {}", sanitized);
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Truncate git stderr so user-controlled path content can't bloat error
/// messages, and so a non-ASCII path in git's error doesn't straddle the byte
/// cutoff and panic the process. Slices at a UTF-8 char boundary via
/// `floor_char_boundary`.
pub(crate) fn truncate_git_stderr(stderr: &str) -> String {
    const MAX_STDERR_LEN: usize = 256;
    if stderr.len() > MAX_STDERR_LEN {
        let truncate_at = stderr.floor_char_boundary(MAX_STDERR_LEN);
        format!("{}... (truncated)", &stderr[..truncate_at])
    } else {
        stderr.to_string()
    }
}

/// Parse NUL-delimited git log output into BlameEntry list.
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

/// Typed JSON output for blame. Borrows from `BlameData` to avoid cloning.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BlameOutput<'a> {
    pub name: &'a str,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: &'a str,
    pub commits: &'a [BlameEntry],
    pub total_commits: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<BlameCallerEntry>,
}

/// A caller entry in blame output with path already relativized.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BlameCallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
}

/// Build JSON output from BlameData.
pub(crate) fn blame_to_json(data: &BlameData, root: &Path) -> serde_json::Value {
    let output = BlameOutput {
        name: &data.chunk.name,
        file: normalize_path(&data.chunk.file),
        line_start: data.chunk.line_start,
        line_end: data.chunk.line_end,
        signature: &data.chunk.signature,
        commits: &data.commits,
        total_commits: data.commits.len(),
        callers: data
            .callers
            .iter()
            .map(|c| BlameCallerEntry {
                name: c.name.clone(),
                file: rel_display(&c.file, root),
                line_start: c.line,
            })
            .collect(),
    };

    serde_json::to_value(&output).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Failed to serialize BlameOutput");
        serde_json::json!({})
    })
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
    ctx: &crate::cli::CommandContext<'_, cqs::store::ReadOnly>,
    target: &str,
    commits: usize,
    show_callers: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_blame", target).entered();

    let store = &ctx.store;
    let root = &ctx.root;
    let args = BlameArgs {
        name: target.to_string(),
        commits,
        callers: show_callers,
    };
    let data = blame_core(store, root, &args)?;

    if json {
        let value = blame_to_json(&data, root);
        crate::cli::json_envelope::emit_json(&value).context("Failed to serialize blame output")?;
    } else {
        print_blame_terminal(&data, root);
    }

    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A wire/MCP caller can supply only `name` and inherit defaults.
    #[test]
    fn blame_args_deserialize_minimal() {
        let args: BlameArgs = serde_json::from_str(r#"{"name": "my_fn"}"#).unwrap();
        assert_eq!(args.name, "my_fn");
        assert_eq!(args.commits, 10);
        assert!(!args.callers);
    }

    /// `BlameArgs::default` must match the clap `BlameArgs` defaults.
    /// Parses `cqs blame <name>` via a throwaway `clap::Parser` wrapper.
    #[test]
    fn blame_args_default_matches_clap_defaults() {
        use clap::Parser;

        #[derive(Parser)]
        struct Wrap {
            #[command(flatten)]
            args: crate::cli::args::BlameArgs,
        }

        let clap_args = Wrap::try_parse_from(["cqs-blame", "my_fn"]).unwrap().args;
        let core = BlameArgs {
            name: clap_args.name.clone(),
            commits: clap_args.commits,
            callers: clap_args.callers,
        };
        let expected = BlameArgs {
            name: "my_fn".to_string(),
            ..BlameArgs::default()
        };
        assert_eq!(
            core, expected,
            "clap blame defaults drifted from BlameArgs::default — update both together"
        );
    }

    #[test]
    fn test_parse_git_log_output_single() {
        let output = "abc1234\0Alice\x002026-02-20 14:30:00 -0500\0fix: some bug\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc1234");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[0].date, "2026-02-20 14:30:00 -0500");
        assert_eq!(entries[0].message, "fix: some bug");
    }

    #[test]
    fn test_parse_git_log_output_multiple() {
        let output = "abc1234\0Alice\x002026-02-20\0first commit\n\
                       def5678\0Bob\x002026-02-19\0second commit\n\
                       ghi9012\0Charlie\x002026-02-18\0third commit\n";
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
                       abc1234\0Alice\x002026-02-20\0valid line\n\
                       incomplete\0two-parts\n";
        let entries = parse_git_log_output(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hash, "abc1234");
    }

    #[test]
    fn test_parse_git_log_output_message_with_pipe() {
        // Pipe in commit message should not break parsing (NUL separator handles it)
        let output = "abc1234\0Alice\x002026-02-20\0fix: search | callers pipeline\n";
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
                signature: "pub fn resolve_target(store: &Store<Mode>, target: &str)".to_string(),
                content: String::new(),
                doc: None,
                line_start: 23,
                line_end: 96,
                parent_id: None,
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
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
                edge_kind: cqs::parser::CallEdgeKind::Call,
            }],
        };

        let root = Path::new("");
        let json = blame_to_json(&data, root);

        assert_eq!(json["name"], "resolve_target");
        assert_eq!(json["file"], "src/search.rs");
        assert_eq!(json["line_start"], 23);
        assert_eq!(json["line_end"], 96);
        assert_eq!(json["commits"].as_array().unwrap().len(), 1);
        assert_eq!(json["commits"][0]["hash"], "abc1234");
        assert_eq!(json["total_commits"], 1);
        assert_eq!(json["callers"].as_array().unwrap().len(), 1);
        assert_eq!(json["callers"][0]["name"], "cmd_explain");
        assert_eq!(json["callers"][0]["line_start"], 52);
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
                parent_type_name: None,
                content_hash: String::new(),
                window_idx: None,
                parser_version: 0,
                vendored: false,
            },
            commits: vec![],
            callers: vec![],
        };

        let root = Path::new("");
        let json = blame_to_json(&data, root);

        assert!(json.get("callers").is_none());
        assert_eq!(json["total_commits"], 0);
    }

    /// When a non-ASCII path appears in git's stderr (CJK directory, emoji
    /// filename, accented Latin), a naive `&stderr[..MAX_STDERR_LEN]` slice
    /// would panic if byte 256 lands inside a multi-byte codepoint.
    /// `truncate_git_stderr` must use `floor_char_boundary` and produce valid
    /// UTF-8.
    #[test]
    fn git_log_stderr_truncate_handles_non_ascii_paths() {
        // Build a stderr message that exceeds MAX_STDERR_LEN (256) and
        // packs multi-byte codepoints near the cutoff.
        // Pad with ASCII out to byte 250, then drop CJK + emoji so the
        // byte-256 cutoff lands mid-codepoint.
        let prefix = "fatal: pathspec '".to_string();
        let pad = "x".repeat(250 - prefix.len());
        // 注釈🎉 is a mix of 3-byte CJK and 4-byte emoji that will straddle
        // byte 256 from offset 250.
        let multibyte = "注釈🎉注釈🎉注釈🎉more padding past the end";
        let stderr = format!("{prefix}{pad}{multibyte}");
        assert!(
            stderr.len() > 256,
            "test fixture must exceed MAX_STDERR_LEN to exercise the truncation branch"
        );
        // The naive byte slice would panic here; the helper must not.
        let truncated = truncate_git_stderr(&stderr);
        // Result must be valid UTF-8 (it is, since it's a String, but
        // confirm round-trip-via-bytes).
        assert!(
            std::str::from_utf8(truncated.as_bytes()).is_ok(),
            "truncated stderr must be valid UTF-8"
        );
        assert!(
            truncated.ends_with("... (truncated)"),
            "truncated stderr should end with the truncation marker, got: {truncated:?}"
        );
        // ASCII inputs still round-trip identically.
        let small = "fatal: short ascii error";
        assert_eq!(truncate_git_stderr(small), small);
    }
}
