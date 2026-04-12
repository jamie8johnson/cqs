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

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// Project subcommands
#[derive(clap::Subcommand)]
pub(crate) enum ProjectCommand {
    /// Register a project for cross-project search
    Register {
        /// Project name (used for identification)
        name: String,
        /// Path to project root (must have .cqs/index.db)
        path: PathBuf,
    },
    /// List registered projects
    List,
    /// Remove a registered project
    Remove {
        /// Project name to remove
        name: String,
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
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

pub(crate) fn cmd_project(subcmd: &ProjectCommand, model_config: &ModelConfig) -> Result<()> {
    let _span = tracing::info_span!("cmd_project").entered();
    match subcmd {
        ProjectCommand::Register { name, path } => {
            let abs_path = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()?.join(path)
            };
            let abs_path = dunce::canonicalize(&abs_path).unwrap_or_else(|_| abs_path.clone());

            let mut registry = ProjectRegistry::load()?;
            registry.register(name.clone(), abs_path.clone())?;
            println!("Registered '{}' at {}", name, abs_path.display());
            Ok(())
        }
        ProjectCommand::List => {
            let registry = ProjectRegistry::load()?;
            if registry.project.is_empty() {
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
        ProjectCommand::Remove { name } => {
            let mut registry = ProjectRegistry::load()?;
            if registry.remove(name)? {
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
            json,
        } => {
            let embedder = Embedder::new(model_config.clone())?;
            let query_embedding = embedder.embed_query(query)?;

            let results = search_across_projects(&query_embedding, query, *limit, *threshold)?;

            if *json {
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
                println!("{}", serde_json::to_string_pretty(&json_results)?);
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
