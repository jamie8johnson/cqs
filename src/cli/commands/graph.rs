//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.

use anyhow::{bail, Result};
use colored::Colorize;

use cqs::Store;

use crate::cli::{find_project_root, Cli};

/// Find functions that call the specified function
pub(crate) fn cmd_callers(_cli: &Cli, name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callers", name).entered();
    let root = find_project_root();
    let index_path = cqs::resolve_index_dir(&root).join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    // Use full call graph (includes large functions)
    let callers = store.get_callers_full(name)?;

    if callers.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No callers found for '{}'", name);
        }
        return Ok(());
    }

    if json {
        let json_output: Vec<serde_json::Value> = callers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "file": c.file.to_string_lossy().replace('\\', "/"),
                    "line": c.line,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        println!("Functions that call '{}':", name);
        println!();
        for caller in &callers {
            println!(
                "  {} ({}:{})",
                caller.name.cyan(),
                caller.file.display(),
                caller.line
            );
        }
        println!();
        println!("Total: {} caller(s)", callers.len());
    }

    Ok(())
}

/// Find functions called by the specified function
pub(crate) fn cmd_callees(_cli: &Cli, name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callees", name).entered();
    let root = find_project_root();
    let index_path = cqs::resolve_index_dir(&root).join("index.db");

    if !index_path.exists() {
        bail!("Index not found. Run 'cqs init && cqs index' first.");
    }

    let store = Store::open(&index_path)?;
    // Use full call graph (includes large functions)
    // No file context available from CLI input — pass None
    let callees = store.get_callees_full(name, None)?;

    if json {
        let json_output = serde_json::json!({
            "function": name,
            "calls": callees.iter().map(|(n, line)| {
                serde_json::json!({"name": n, "line": line})
            }).collect::<Vec<_>>(),
            "count": callees.len(),
        });
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        println!("Functions called by '{}':", name.cyan());
        println!();
        if callees.is_empty() {
            println!("  (no function calls found)");
        } else {
            for (callee_name, _line) in &callees {
                println!("  {}", callee_name);
            }
        }
        println!();
        println!("Total: {} call(s)", callees.len());
    }

    Ok(())
}
