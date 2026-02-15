//! Type dependency command for cqs
//!
//! Shows which chunks reference a type (forward), or what types a function uses (reverse).

use anyhow::Result;
use colored::Colorize;

use crate::cli::Cli;

/// Show type dependencies.
///
/// Forward (default): `cqs deps Config` — who uses this type?
/// Reverse: `cqs deps --reverse func_name` — what types does this function use?
pub(crate) fn cmd_deps(_cli: &Cli, name: &str, reverse: bool, json: bool) -> Result<()> {
    let _span = tracing::info_span!("cmd_deps", name, reverse).entered();
    let (store, root, _) = crate::cli::open_project_store()?;

    if reverse {
        let types = store.get_types_used_by(name)?;
        if json {
            let json_types: Vec<serde_json::Value> = types
                .iter()
                .map(|(tn, kind)| serde_json::json!({"type_name": tn, "edge_kind": kind}))
                .collect();
            let output = serde_json::json!({
                "function": name,
                "types": json_types,
                "count": json_types.len(),
            });
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else if types.is_empty() {
            println!("No type dependencies found for '{}'", name);
        } else {
            println!("Types used by '{}':", name.cyan());
            println!();
            for (type_name, kind) in &types {
                if kind.is_empty() {
                    println!("  {}", type_name);
                } else {
                    println!("  {} ({})", type_name, kind.dimmed());
                }
            }
            println!();
            println!("Total: {} type(s)", types.len());
        }
    } else {
        let users = store.get_type_users(name)?;
        if json {
            let json_users: Vec<serde_json::Value> = users
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "name": c.name,
                        "file": cqs::rel_display(&c.file, &root),
                        "line_start": c.line_start,
                        "chunk_type": c.chunk_type.to_string(),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_users)?);
        } else if users.is_empty() {
            println!("No users found for type '{}'", name);
        } else {
            println!("Chunks that use type '{}':", name.cyan());
            println!();
            for user in &users {
                println!(
                    "  {} ({}:{})",
                    user.name.cyan(),
                    cqs::rel_display(&user.file, &root),
                    user.line_start
                );
            }
            println!();
            println!("Total: {} user(s)", users.len());
        }
    }
    Ok(())
}
