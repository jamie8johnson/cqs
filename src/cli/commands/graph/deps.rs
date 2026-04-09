//! Type dependency command for cqs
//!
//! Shows which chunks reference a type (forward), or what types a function uses (reverse).
//! Core JSON builders are shared between CLI and batch handlers.

use std::path::Path;

use anyhow::{Context as _, Result};
use colored::Colorize;

use cqs::store::{ChunkSummary, TypeUsage};

// ─── Output types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize)]
pub(crate) struct TypeUsageEntry {
    pub type_name: String,
    pub edge_kind: String,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsReverseOutput {
    pub name: String, // was "function"
    pub types: Vec<TypeUsageEntry>,
    pub count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct DepsUserEntry {
    pub name: String,
    pub file: String,
    pub line_start: u32,
    pub chunk_type: String,
}

// ─── Shared JSON builders ──────────────────────────────────────────────────

/// Build typed reverse deps output (types used by a function) -- shared between CLI and batch.
pub(crate) fn build_deps_reverse(name: &str, types: &[TypeUsage]) -> DepsReverseOutput {
    let _span = tracing::info_span!("build_deps_reverse", name).entered();
    DepsReverseOutput {
        name: name.to_string(),
        types: types
            .iter()
            .map(|t| TypeUsageEntry {
                type_name: t.type_name.clone(),
                edge_kind: t.edge_kind.clone(),
            })
            .collect(),
        count: types.len(),
    }
}

/// Build typed forward deps output (chunks that use a type) -- shared between CLI and batch.
pub(crate) fn build_deps_forward(users: &[ChunkSummary], root: &Path) -> Vec<DepsUserEntry> {
    let _span = tracing::info_span!("build_deps_forward", count = users.len()).entered();
    users
        .iter()
        .map(|c| DepsUserEntry {
            name: c.name.clone(),
            file: cqs::rel_display(&c.file, root).to_string(),
            line_start: c.line_start,
            chunk_type: c.chunk_type.to_string(),
        })
        .collect()
}

// ─── CLI command ────────────────────────────────────────────────────────────

/// Show type dependencies.
///
/// Forward (default): `cqs deps Config` -- who uses this type?
/// Reverse: `cqs deps --reverse func_name` -- what types does this function use?
pub(crate) fn cmd_deps(
    ctx: &crate::cli::CommandContext,
    name: &str,
    reverse: bool,
    cross_project: bool,
    json: bool,
) -> Result<()> {
    let _span = tracing::info_span!("cmd_deps", name, reverse, cross_project).entered();
    if cross_project {
        tracing::warn!("--cross-project for deps is not yet implemented, using local only");
    }
    let store = &ctx.store;
    let root = &ctx.root;

    if reverse {
        let types = store
            .get_types_used_by(name)
            .context("Failed to load type dependencies")?;
        if json {
            let output = build_deps_reverse(name, &types);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if types.is_empty() {
            println!("No type dependencies found for '{}'", name);
        } else {
            println!("Types used by '{}':", name.cyan());
            println!();
            for t in &types {
                if t.edge_kind.is_empty() {
                    println!("  {}", t.type_name);
                } else {
                    println!("  {} ({})", t.type_name, t.edge_kind.dimmed());
                }
            }
            println!();
            println!("Total: {} type(s)", types.len());
        }
    } else {
        let users = store
            .get_type_users(name)
            .context("Failed to load type users")?;
        if json {
            let output = build_deps_forward(&users, root);
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if users.is_empty() {
            println!("No users found for type '{}'", name);
        } else {
            println!("Chunks that use type '{}':", name.cyan());
            println!();
            for user in &users {
                println!(
                    "  {} ({}:{})",
                    user.name.cyan(),
                    cqs::rel_display(&user.file, root),
                    user.line_start
                );
            }
            println!();
            println!("Total: {} user(s)", users.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deps_reverse_field_names() {
        let output = DepsReverseOutput {
            name: "my_func".into(),
            types: vec![TypeUsageEntry {
                type_name: "Config".into(),
                edge_kind: "Param".into(),
            }],
            count: 1,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["name"], "my_func"); // was "function"
        assert!(json.get("function").is_none());
    }

    #[test]
    fn test_deps_reverse_empty() {
        let output = DepsReverseOutput {
            name: "foo".into(),
            types: vec![],
            count: 0,
        };
        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["count"], 0);
        assert!(json["types"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_deps_forward_empty() {
        let output = build_deps_forward(&[], std::path::Path::new("/"));
        assert!(output.is_empty());
    }

    #[test]
    fn test_deps_user_entry_field_names() {
        let entry = DepsUserEntry {
            name: "bar".into(),
            file: "src/foo.rs".into(),
            line_start: 15,
            chunk_type: "function".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert!(json.get("line_start").is_some());
        assert!(json.get("line").is_none());
    }
}
