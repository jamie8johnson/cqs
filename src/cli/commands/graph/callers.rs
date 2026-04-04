//! Call graph commands for cqs
//!
//! Provides callers/callees analysis.

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::normalize_path;
use cqs::store::CallerInfo;

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct CallerEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32, // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleeEntry {
    pub name: String,
    pub line_start: u32, // was "line"
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct CalleesOutput {
    pub name: String, // was "function"
    pub calls: Vec<CalleeEntry>,
    pub count: usize,
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed caller output from caller info -- shared between CLI and batch.
pub(crate) fn build_callers(callers: &[CallerInfo]) -> Vec<CallerEntry> {
    let _span = tracing::info_span!("build_callers", count = callers.len()).entered();
    callers
        .iter()
        .map(|c| CallerEntry {
            name: c.name.clone(),
            file: normalize_path(&c.file).to_string(),
            line_start: c.line,
        })
        .collect()
}

/// Build typed callees output -- shared between CLI and batch.
pub(crate) fn build_callees(name: &str, callees: &[(String, u32)]) -> CalleesOutput {
    let _span = tracing::info_span!("build_callees", name, count = callees.len()).entered();
    CalleesOutput {
        name: name.to_string(),
        calls: callees
            .iter()
            .map(|(n, line)| CalleeEntry {
                name: n.clone(),
                line_start: *line,
            })
            .collect(),
        count: callees.len(),
    }
}

// ─── CLI commands ──────────────────────────────────────────────────────────

/// Find functions that call the specified function
pub(crate) fn cmd_callers(ctx: &crate::cli::CommandContext, name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callers", name).entered();
    let store = &ctx.store;
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
        let output = build_callers(&callers);
        println!("{}", serde_json::to_string_pretty(&output)?);
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
pub(crate) fn cmd_callees(ctx: &crate::cli::CommandContext, name: &str, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_callees", name).entered();
    let store = &ctx.store;
    // Use full call graph (includes large functions)
    // No file context available from CLI input -- pass None
    let callees = store
        .get_callees_full(name, None)
        .context("Failed to load callees")?;

    if json {
        let output = build_callees(name, &callees);
        println!("{}", serde_json::to_string_pretty(&output)?);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caller_entry_field_names() {
        let entry = CallerEntry {
            name: "foo".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none()); // normalized away
    }

    #[test]
    fn test_build_callers_empty() {
        let output = build_callers(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn test_build_callees_empty() {
        let output = build_callees("foo", &[]);
        assert_eq!(output.count, 0);
        assert!(output.calls.is_empty());
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "foo");
    }

    #[test]
    fn test_callees_output_field_names() {
        let output = build_callees("bar", &[("baz".into(), 10)]);
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "bar"); // was "function"
        assert!(json.get("function").is_none());
        assert_eq!(json["calls"][0]["line_start"], 10);
    }
}
