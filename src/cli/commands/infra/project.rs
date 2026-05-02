//! Project management command — register, list, remove, search across projects
//!
//! Core struct is [`ProjectSearchResult`]; built in the `Search` subcommand handler.

use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;

use cqs::embedder::ModelConfig;
use cqs::normalize_path;
use cqs::Embedder;
use cqs::{search_across_projects, ProjectRegistry};

use crate::cli::definitions::TextJsonArgs;
use crate::cli::Cli;

// ---------------------------------------------------------------------------
// Output struct
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectSearchResult {
    pub project: String,
    pub name: String,
    pub file: String,
    pub line_start: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub score: f32,
}

/// API-V1.29-1: JSON envelope row for `cqs --json project list`.
/// `indexed` is true if either `.cqs/index.db` or the legacy `.cq/index.db`
/// sits on disk — mirrors the text-mode `ok` / `missing index` status string.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectListEntry {
    pub name: String,
    pub path: String,
    pub indexed: bool,
}

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// Project subcommands
#[derive(clap::Subcommand)]
pub(crate) enum ProjectCommand {
    /// Add a project to the cross-project search registry
    ///
    /// P3-29: renamed from `register` to align with `ref add`, `slot create`,
    /// and `cache clear`. Old `register` form preserved as a visible alias
    /// so existing scripts and `--help` searches keep working.
    #[command(visible_alias = "register")]
    Add {
        /// Project name (used for identification)
        name: String,
        /// Path to project root (must have .cqs/index.db)
        path: PathBuf,
        /// P3-25: shared `--json` arg so `cqs project add --json` emits the
        /// envelope. Without this, `cqs --json project register` silently
        /// dropped the top-level flag and `cqs project register --json` was
        /// rejected at parse time as `unexpected argument`.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// List registered projects
    List {
        /// API-V1.29-1: shared `--json` arg so `cqs --json project list`
        /// honors the top-level flag. Without this, the `cli.json` bit was
        /// dropped and agents consuming JSON got colored ANSI text.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove a registered project
    Remove {
        /// Project name to remove
        name: String,
        /// API-V1.29-1: shared `--json` arg — see `List` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Search across all registered projects
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Min similarity threshold
        #[arg(short = 't', long, default_value = "0.3")]
        threshold: f32,
        /// API-V1.22-2: shared `--json` arg (was inline `json: bool`).
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

pub(crate) fn cmd_project(
    cli: &Cli,
    subcmd: &ProjectCommand,
    model_config: &ModelConfig,
) -> Result<()> {
    // OB-V1.29-3: per-subcommand span so traces distinguish register / list /
    // remove / search and carry the discriminating field (project name or
    // query). The previous single `cmd_project` span collapsed four very
    // different code paths into one trace entry.
    let _span = match subcmd {
        ProjectCommand::Add { name, .. } => {
            tracing::info_span!("cmd_project_add", name = %name).entered()
        }
        ProjectCommand::List { .. } => tracing::info_span!("cmd_project_list").entered(),
        ProjectCommand::Remove { name, .. } => {
            tracing::info_span!("cmd_project_remove", name = %name).entered()
        }
        ProjectCommand::Search { query, limit, .. } => {
            tracing::info_span!("cmd_project_search", query = %query, limit).entered()
        }
    };
    match subcmd {
        ProjectCommand::Add { name, path, output } => {
            let abs_path = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()?.join(path)
            };
            let abs_path = dunce::canonicalize(&abs_path).unwrap_or_else(|_| abs_path.clone());

            let mut registry = ProjectRegistry::load()?;
            registry.register(name.clone(), abs_path.clone())?;
            // P3-25: emit JSON envelope when `--json` was passed at either
            // the top level (`cqs --json project add ...`) or the subcommand
            // level (`cqs project add ... --json`). Mirrors the `Remove` arm
            // shape; agents piping through `jq` no longer need to special-
            // case `register` as the one mutation that prints text.
            let json = cli.json || output.json;
            if json {
                crate::cli::json_envelope::emit_json(&serde_json::json!({
                    "status": "registered",
                    "name": name,
                    "path": normalize_path(&abs_path),
                }))?;
            } else {
                println!("Registered '{}' at {}", name, abs_path.display());
            }
            Ok(())
        }
        ProjectCommand::List { output } => {
            let json = cli.json || output.json;
            let registry = ProjectRegistry::load()?;
            if json {
                let entries: Vec<ProjectListEntry> = registry
                    .project
                    .iter()
                    .map(|e| ProjectListEntry {
                        name: e.name.clone(),
                        path: normalize_path(&e.path),
                        indexed: e.path.join(".cqs/index.db").exists()
                            || e.path.join(".cq/index.db").exists(),
                    })
                    .collect();
                crate::cli::json_envelope::emit_json(&serde_json::json!({
                    "projects": entries,
                }))?;
            } else if registry.project.is_empty() {
                println!("No projects registered.");
                println!("Use 'cqs project register <name> <path>' to add one.");
            } else {
                println!("Registered projects:");
                for entry in &registry.project {
                    let status = if entry.path.join(".cqs/index.db").exists()
                        || entry.path.join(".cq/index.db").exists()
                    {
                        "ok".green().to_string()
                    } else {
                        "missing index".red().to_string()
                    };
                    println!("  {} — {} [{}]", entry.name, entry.path.display(), status);
                }
            }
            Ok(())
        }
        ProjectCommand::Remove { name, output } => {
            let json = cli.json || output.json;
            let mut registry = ProjectRegistry::load()?;
            let removed = registry.remove(name)?;
            if json {
                let status = if removed { "removed" } else { "not_found" };
                crate::cli::json_envelope::emit_json(&serde_json::json!({
                    "status": status,
                    "name": name,
                }))?;
            } else if removed {
                println!("Removed '{}'", name);
            } else {
                println!("Project '{}' not found", name);
            }
            Ok(())
        }
        ProjectCommand::Search {
            query,
            limit,
            threshold,
            output,
        } => {
            let embedder = Embedder::new(model_config.clone())?;
            let query_embedding = embedder.embed_query(query)?;

            let results = search_across_projects(&query_embedding, query, *limit, *threshold)?;

            // Top-level `--json` always wins (mirrors `cmd_model` at
            // `src/cli/commands/infra/model.rs:113`). `cqs --json project search foo`
            // must emit envelope JSON without `--json` after the subcommand.
            let json = cli.json || output.json;
            if json {
                let json_results: Vec<_> = results
                    .iter()
                    .map(|r| ProjectSearchResult {
                        project: r.project_name.clone(),
                        name: r.name.clone(),
                        file: normalize_path(&r.file),
                        line_start: r.line_start,
                        signature: r.signature.clone(),
                        score: r.score,
                    })
                    .collect();
                crate::cli::json_envelope::emit_json(&json_results)?;
            } else if results.is_empty() {
                println!("No results found across registered projects.");
            } else {
                for r in &results {
                    println!(
                        "[{}] {} {}:{} ({:.3})",
                        r.project_name.cyan(),
                        r.name.bold(),
                        r.file.display(),
                        r.line_start,
                        r.score,
                    );
                    if let Some(ref sig) = r.signature {
                        println!("  {}", sig.dimmed());
                    }
                }
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_search_result_serialization() {
        let result = ProjectSearchResult {
            project: "my-lib".into(),
            name: "do_stuff".into(),
            file: "src/lib.rs".into(),
            line_start: 42,
            signature: Some("fn do_stuff(x: i32) -> bool".into()),
            score: 0.875,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["project"], "my-lib");
        assert_eq!(json["name"], "do_stuff");
        assert_eq!(json["file"], "src/lib.rs");
        assert_eq!(json["line_start"], 42);
        assert_eq!(json["signature"], "fn do_stuff(x: i32) -> bool");
    }

    #[test]
    fn test_project_search_result_no_signature() {
        let result = ProjectSearchResult {
            project: "my-lib".into(),
            name: "Widget".into(),
            file: "src/types.rs".into(),
            line_start: 10,
            signature: None,
            score: 0.5,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("signature").is_none());
    }
}
