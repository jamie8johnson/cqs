//! Project management command — register, list, remove, search across projects
//!
//! Core struct is [`ProjectSearchResult`]; built in the `Search` subcommand handler.

use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;

use cqs::embedder::ModelConfig;
use cqs::normalize_path;
use cqs::Embedder;
use cqs::SearchFilter;
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

/// JSON envelope row for `cqs --json project list`.
/// `indexed` is true if either `.cqs/index.db` or `.cq/index.db` sits on
/// disk — mirrors the text-mode `ok` / `missing index` status string.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectListEntry {
    pub name: String,
    pub path: String,
    pub indexed: bool,
}

/// `cqs project list --json` envelope: `{projects: [...]}`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectListOutput {
    pub projects: Vec<ProjectListEntry>,
}

/// `cqs project add --json` payload.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectAddOutput {
    pub status: &'static str,
    pub name: String,
    pub path: String,
}

/// `cqs project remove --json` payload. `status` is `removed` / `not_found`.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ProjectRemoveOutput {
    pub status: &'static str,
    pub name: String,
}

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

/// Project subcommands
#[derive(clap::Subcommand)]
pub(crate) enum ProjectCommand {
    /// Add a project to the cross-project search registry
    ///
    /// `register` is a visible alias for this subcommand.
    #[command(visible_alias = "register")]
    Add {
        /// Project name (used for identification)
        name: String,
        /// Path to project root (must have .cqs/index.db)
        path: PathBuf,
        /// Shared `--json` arg so both `cqs --json project add` and
        /// `cqs project add --json` emit the envelope.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// List registered projects
    List {
        /// Shared `--json` arg so `cqs --json project list` honors the
        /// top-level flag.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Remove a registered project
    Remove {
        /// Project name to remove
        name: String,
        /// Shared `--json` arg — see `List` above.
        #[command(flatten)]
        output: TextJsonArgs,
    },
    /// Search across all registered projects.
    ///
    /// Mirrors the top-level `cqs <q>` filter surface so `cqs project
    /// search` accepts the same `--lang`, `--include-type`,
    /// `--exclude-type`, `--path`, `--name-boost`, `--rrf`, and
    /// `--include-docs` flags. Each project's per-store search applies
    /// the filter consistently before results are merged.
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Min similarity threshold
        #[arg(short = 't', long, default_value = "0.3", value_parser = crate::cli::definitions::parse_finite_f32)]
        threshold: f32,
        /// Weight for name matching in hybrid search (0.0-1.0). Mirrors the
        /// top-level `cqs <q> --name-boost` flag.
        #[arg(long, default_value = "0.2", value_parser = crate::cli::definitions::parse_unit_f32)]
        name_boost: f32,
        /// Filter by language (mirrors top-level `-l/--lang`).
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Include only these chunk types (mirrors top-level `--include-type`).
        #[arg(long, alias = "chunk-type")]
        include_type: Option<Vec<String>>,
        /// Exclude these chunk types (mirrors top-level `--exclude-type`).
        #[arg(long)]
        exclude_type: Option<Vec<String>>,
        /// Filter by path glob (mirrors top-level `-p/--path`).
        #[arg(short = 'p', long)]
        path: Option<String>,
        /// Enable RRF hybrid search (keyword + semantic fusion). Mirrors top-level `--rrf`.
        #[arg(long)]
        rrf: bool,
        /// Include documentation, markdown, and config chunks. Mirrors top-level `--include-docs`.
        #[arg(long)]
        include_docs: bool,
        /// Shared `--json` arg.
        #[command(flatten)]
        output: TextJsonArgs,
    },
}

pub(crate) fn cmd_project(
    cli: &Cli,
    subcmd: &ProjectCommand,
    model_config: &ModelConfig,
) -> Result<()> {
    // Per-subcommand span so traces distinguish register / list / remove /
    // search and carry the discriminating field (project name or query).
    let _span = match subcmd {
        ProjectCommand::Add { name, .. } => {
            tracing::info_span!("cmd_project_add", name = %name).entered()
        }
        ProjectCommand::List { .. } => tracing::info_span!("cmd_project_list").entered(),
        ProjectCommand::Remove { name, .. } => {
            tracing::info_span!("cmd_project_remove", name = %name).entered()
        }
        ProjectCommand::Search { query, limit, .. } => {
            tracing::info_span!("cmd_project_search", query = %query, limit, parity = "1459-1a")
                .entered()
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
            // Emit JSON envelope when `--json` was passed at either the top
            // level (`cqs --json project add ...`) or the subcommand level
            // (`cqs project add ... --json`). Mirrors the `Remove` arm shape.
            let json = cli.json || output.json;
            if json {
                crate::cli::json_envelope::emit_json(&ProjectAddOutput {
                    status: "registered",
                    name: name.clone(),
                    path: normalize_path(&abs_path),
                })?;
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
                crate::cli::json_envelope::emit_json(&ProjectListOutput { projects: entries })?;
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
                crate::cli::json_envelope::emit_json(&ProjectRemoveOutput {
                    status,
                    name: name.clone(),
                })?;
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
            name_boost,
            lang,
            include_type,
            exclude_type,
            path,
            rrf,
            include_docs,
            output,
        } => {
            let embedder = Embedder::new(model_config.clone())?;
            let query_embedding = embedder.embed_query(query)?;

            let filter = build_project_search_filter(
                query,
                *name_boost,
                *rrf,
                path.as_deref(),
                lang.as_deref(),
                include_type.as_deref(),
                exclude_type.as_deref(),
                *include_docs,
            );

            let results = search_across_projects(&query_embedding, &filter, *limit, *threshold)?;

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

/// Build the per-project `SearchFilter` from the `cqs project search`
/// flag surface. Split out of `cmd_project`'s `Search` arm so the
/// precedence rules between `--include-type`, `--include-docs`, and the
/// code-only default can be tested without loading the embedder or any
/// cross-project store.
///
/// Mirrors the top-level `cqs <q>` filter assembly at
/// `src/cli/commands/search/query.rs` so per-project search behaves
/// identically to per-project search inside `cmd_search`.
///
/// Precedence (top-level parity):
///   1. `--include-type X,Y` — explicit allowlist (wins over `--include-docs`)
///   2. `--include-docs` and no `--include-type` — `None` (search everything)
///   3. default — `ChunkType::code_types()` (callable + type defs)
///
/// `--lang` strings that don't parse as a known `Language` are
/// `tracing::warn!`-logged and ignored (filter is left unset). This
/// matches the quiet-fallback shape of `cmd_search`.
#[allow(clippy::too_many_arguments)] // mirrors the clap-flag surface; bundling into a struct
                                     // would just push the unpacking one frame down.
pub(crate) fn build_project_search_filter(
    query: &str,
    name_boost: f32,
    rrf: bool,
    path: Option<&str>,
    lang: Option<&str>,
    include_type: Option<&[String]>,
    exclude_type: Option<&[String]>,
    include_docs: bool,
) -> SearchFilter {
    // SearchFilter is `#[non_exhaustive]`, so cross-crate
    // construction starts from `Default` then mutates fields.
    let mut filter = SearchFilter::default();
    filter.query_text = query.to_string();
    filter.name_boost = name_boost;
    filter.enable_rrf = rrf;
    filter.path_pattern = path.map(|p| p.to_string());
    if let Some(lang_str) = lang {
        if let Ok(parsed) = lang_str.parse::<cqs::parser::Language>() {
            filter.languages = Some(vec![parsed]);
        } else {
            tracing::warn!(
                lang = lang_str,
                "Unknown language for project search; ignoring"
            );
        }
    }
    filter.include_types = match include_type {
        Some(types) => {
            let parsed: Vec<_> = types
                .iter()
                .filter_map(|s| s.parse::<cqs::parser::ChunkType>().ok())
                .collect();
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        }
        None if include_docs => None,
        None => Some(cqs::parser::ChunkType::code_types().to_vec()),
    };
    if let Some(types) = exclude_type {
        let parsed: Vec<_> = types
            .iter()
            .filter_map(|s| s.parse::<cqs::parser::ChunkType>().ok())
            .collect();
        if !parsed.is_empty() {
            filter.exclude_types = Some(parsed);
        }
    }
    filter
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

    // ===== build_project_search_filter behavior =====
    //
    // The parse-only test at `src/cli/mod.rs::test_cmd_project_search_full_flag_parity`
    // confirms each flag binds to the right field on `ProjectCommand::Search`.
    // These tests confirm the parsed flags actually produce the right
    // `SearchFilter` — pinning the precedence rules between
    // `--include-type`, `--include-docs`, and the code-only default.

    /// Default (no `--include-type`, no `--include-docs`) — code-only filter.
    /// Pins the bottom of the precedence cascade.
    #[test]
    fn build_filter_default_uses_code_types() {
        let f = build_project_search_filter("query", 0.2, false, None, None, None, None, false);
        assert_eq!(f.query_text, "query");
        assert!((f.name_boost - 0.2).abs() < f32::EPSILON);
        assert!(!f.enable_rrf);
        assert!(f.path_pattern.is_none());
        assert!(f.languages.is_none());
        assert!(f.exclude_types.is_none());
        let inc = f
            .include_types
            .expect("default must set code-only allowlist");
        assert_eq!(
            inc,
            cqs::parser::ChunkType::code_types().to_vec(),
            "default include_types must equal ChunkType::code_types()"
        );
    }

    /// `--include-docs` alone (no explicit `--include-type`) — `include_types = None`
    /// = "search everything". Pins the doc-opt-in branch.
    #[test]
    fn build_filter_include_docs_clears_default_allowlist() {
        let f = build_project_search_filter("q", 0.2, false, None, None, None, None, true);
        assert!(
            f.include_types.is_none(),
            "--include-docs alone must remove the code-only allowlist"
        );
    }

    /// `--include-type X,Y` with `--include-docs` — explicit allowlist wins.
    /// Pins the top of the cascade — the operator's explicit list always
    /// beats the docs flag.
    #[test]
    fn build_filter_include_type_overrides_include_docs() {
        let types = vec!["function".to_string(), "struct".to_string()];
        let f = build_project_search_filter("q", 0.2, false, None, None, Some(&types), None, true);
        let inc = f.include_types.expect("explicit allowlist must be Some");
        assert!(inc.contains(&cqs::parser::ChunkType::Function));
        assert!(inc.contains(&cqs::parser::ChunkType::Struct));
        assert_eq!(
            inc.len(),
            2,
            "explicit allowlist must NOT be widened by --include-docs"
        );
    }

    /// `--exclude-type X` with valid string → set; mixed valid+invalid drops
    /// the invalids silently.
    #[test]
    fn build_filter_exclude_type_filters_invalid_strings() {
        let bad = vec![
            "test".to_string(),
            "this-is-not-a-real-chunk-type".to_string(),
        ];
        let f = build_project_search_filter("q", 0.2, false, None, None, None, Some(&bad), false);
        let exc = f
            .exclude_types
            .expect("exclude must be set when at least one parses");
        assert!(exc.contains(&cqs::parser::ChunkType::Test));
        assert_eq!(
            exc.len(),
            1,
            "invalid chunk-type strings must be dropped, not propagated"
        );
    }

    /// `--exclude-type` with all-invalid strings → field stays None
    /// (would otherwise be Some(vec![]) and silently filter NOTHING but
    /// still trigger the SQL exclude branch).
    #[test]
    fn build_filter_exclude_type_all_invalid_stays_none() {
        let bad = vec!["nope".to_string(), "also-nope".to_string()];
        let f = build_project_search_filter("q", 0.2, false, None, None, None, Some(&bad), false);
        assert!(
            f.exclude_types.is_none(),
            "all-invalid exclude list must stay None — not Some(vec![])"
        );
    }

    /// `--lang rust` → languages set to `[Rust]`.
    #[test]
    fn build_filter_lang_parses_to_language_enum() {
        let f = build_project_search_filter("q", 0.2, false, None, Some("rust"), None, None, false);
        let langs = f
            .languages
            .expect("--lang rust must produce a languages filter");
        assert_eq!(langs, vec![cqs::parser::Language::Rust]);
    }

    /// Unknown `--lang` is silently dropped (tracing::warn-logged, not an
    /// error). Pin so a future change to bail on bad input is deliberate.
    #[test]
    fn build_filter_lang_unknown_falls_through() {
        let f = build_project_search_filter(
            "q",
            0.2,
            false,
            None,
            Some("not-a-real-language"),
            None,
            None,
            false,
        );
        assert!(
            f.languages.is_none(),
            "unknown --lang must be ignored (warned), not bailed on"
        );
    }

    /// `--rrf` and `--name-boost` flow through verbatim — these are the
    /// hybrid-fusion knobs for cross-project search.
    #[test]
    fn build_filter_rrf_and_name_boost_flow_through() {
        let f = build_project_search_filter("q", 0.5, true, None, None, None, None, false);
        assert!(
            f.enable_rrf,
            "--rrf must enable hybrid fusion in the filter"
        );
        assert!(
            (f.name_boost - 0.5).abs() < f32::EPSILON,
            "name_boost must flow through verbatim"
        );
    }

    /// `-p src/**` — path glob flows through to the filter.
    #[test]
    fn build_filter_path_glob_flows_through() {
        let f =
            build_project_search_filter("q", 0.2, false, Some("src/**"), None, None, None, false);
        assert_eq!(f.path_pattern.as_deref(), Some("src/**"));
    }
}
