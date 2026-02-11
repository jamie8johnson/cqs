//! Impact-diff command — what breaks based on a git diff

use std::io::Read;

use anyhow::Result;

use cqs::diff_parse::parse_unified_diff;
use cqs::{analyze_diff_impact, diff_impact_to_json, map_hunks_to_functions, Store};

use crate::cli::find_project_root;

pub(crate) fn cmd_impact_diff(
    _cli: &crate::cli::Cli,
    base: Option<&str>,
    from_stdin: bool,
    json: bool,
) -> Result<()> {
    let root = find_project_root();
    let cqs_dir = cqs::resolve_index_dir(&root);
    let index_path = cqs_dir.join("index.db");

    if !index_path.exists() {
        anyhow::bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    // 1. Get diff text
    let diff_text = if from_stdin {
        read_stdin()?
    } else {
        run_git_diff(base)?
    };

    // 2. Parse hunks
    let hunks = parse_unified_diff(&diff_text);
    if hunks.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changed_functions": [],
                    "callers": [],
                    "tests": [],
                    "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
                }))?
            );
        } else {
            println!("No changes detected.");
        }
        return Ok(());
    }

    // 3. Map hunks to functions
    let store = Store::open(&index_path)?;
    let changed = map_hunks_to_functions(&store, &hunks);

    if changed.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "changed_functions": [],
                    "callers": [],
                    "tests": [],
                    "summary": { "changed_count": 0, "caller_count": 0, "test_count": 0 }
                }))?
            );
        } else {
            println!("No indexed functions affected by this diff.");
        }
        return Ok(());
    }

    // 4. Analyze impact
    let mut result = analyze_diff_impact(&store, &changed)?;
    // Fill in changed_functions (analyze_diff_impact leaves it empty for the caller to set)
    result.changed_functions = changed;

    // 5. Display
    if json {
        let json_val = diff_impact_to_json(&result, &root);
        println!("{}", serde_json::to_string_pretty(&json_val)?);
    } else {
        display_diff_impact_text(&result, &root);
    }

    Ok(())
}

fn read_stdin() -> Result<String> {
    const MAX_STDIN_SIZE: usize = 50 * 1024 * 1024; // 50 MB
    let mut buf = String::new();
    std::io::stdin()
        .take(MAX_STDIN_SIZE as u64 + 1)
        .read_to_string(&mut buf)?;
    if buf.len() > MAX_STDIN_SIZE {
        anyhow::bail!("stdin input exceeds 50 MB limit");
    }
    Ok(buf)
}

fn run_git_diff(base: Option<&str>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("diff");
    if let Some(b) = base {
        if b.starts_with('-') {
            anyhow::bail!("Invalid base ref '{}': must not start with '-'", b);
        }
        cmd.arg(b);
    }

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run 'git diff': {}. Is git installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn display_diff_impact_text(result: &cqs::DiffImpactResult, root: &std::path::Path) {
    use colored::Colorize;

    // Changed functions
    println!(
        "{} ({}):",
        "Changed functions".bold(),
        result.changed_functions.len()
    );
    for f in &result.changed_functions {
        println!("  {} ({}:{})", f.name, f.file, f.line_start);
    }

    // Callers
    if result.all_callers.is_empty() {
        println!();
        println!("{}", "No affected callers.".dimmed());
    } else {
        println!();
        println!(
            "{} ({}):",
            "Affected callers".cyan(),
            result.all_callers.len()
        );
        for c in &result.all_callers {
            let rel = c
                .file
                .strip_prefix(root)
                .unwrap_or(&c.file)
                .to_string_lossy()
                .replace('\\', "/");
            println!(
                "  {} ({}:{}, call at line {})",
                c.name, rel, c.line, c.call_line
            );
        }
    }

    // Tests
    if result.all_tests.is_empty() {
        println!();
        println!("{}", "No affected tests.".dimmed());
    } else {
        println!();
        println!(
            "{} ({}):",
            "Tests to re-run".yellow(),
            result.all_tests.len()
        );
        for t in &result.all_tests {
            let rel = t
                .file
                .strip_prefix(root)
                .unwrap_or(&t.file)
                .to_string_lossy()
                .replace('\\', "/");
            println!(
                "  {} ({}:{}) [via {}, depth {}]",
                t.name, rel, t.line, t.via, t.call_depth
            );
        }
    }
}
