//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::normalize_path;
use cqs::store::CallerInfo;

/// Build JSON array from caller info — shared between CLI and batch.
pub(crate) fn callers_to_json(callers: &[CallerInfo]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = callers
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "file": normalize_path(&c.file),
                "line": c.line,
            })
        })
        .collect();
    serde_json::json!(arr)
}

/// Build JSON object from callees — shared between CLI and batch.
pub(crate) fn callees_to_json(name: &str, callees: &[(String, u32)]) -> serde_json::Value {
    serde_json::json!({
        "function": name,
        "calls": callees.iter().map(|(n, line)| {
            serde_json::json!({"name": n, "line": line})
        }).collect::<Vec<_>>(),
        "count": callees.len(),
    })
}

/// Find functions that call the specified function
pub(crate) fn cmd_callers(name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callers", name).entered();
    let (store, _, _) = crate::cli::open_project_store_readonly()?;
    // Use full call graph (includes large functions)
    let callers = store
        .get_callers_full(name)
        .context("Failed to load callers")?;

    if callers.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No callers found for '{}'", name);
        }
        return Ok(());
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&callers_to_json(&callers))?
        );
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
pub(crate) fn cmd_callees(name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callees", name).entered();
    let (store, _, _) = crate::cli::open_project_store_readonly()?;
    // Use full call graph (includes large functions)
    // No file context available from CLI input — pass None
    let callees = store
        .get_callees_full(name, None)
        .context("Failed to load callees")?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&callees_to_json(name, &callees))?
        );
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
