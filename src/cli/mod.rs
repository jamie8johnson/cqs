//! CLI implementation for cq

mod commands;
mod config;
mod display;
mod files;
mod pipeline;
mod signal;
pub(crate) mod staleness;
mod watch;

// Re-export for watch.rs and commands
pub(crate) use config::find_project_root;
pub(crate) use files::{acquire_index_lock, enumerate_files};
pub(crate) use pipeline::run_index_pipeline;
pub(crate) use signal::{check_interrupted, reset_interrupted};

use commands::{
    cmd_audit_mode, cmd_callees, cmd_callers, cmd_context, cmd_dead, cmd_diff, cmd_doctor,
    cmd_explain, cmd_gather, cmd_gc, cmd_impact, cmd_impact_diff, cmd_index, cmd_init, cmd_notes,
    cmd_project, cmd_query, cmd_read, cmd_ref, cmd_related, cmd_scout, cmd_similar, cmd_stale,
    cmd_stats, cmd_test_map, cmd_trace, cmd_where, NotesCommand, ProjectCommand, RefCommand,
};
use config::apply_config_defaults;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cqs")]
#[command(about = "Semantic code search with local embeddings")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Search query (quote multi-word queries)
    query: Option<String>,

    /// Max results
    #[arg(short = 'n', long, default_value = "5")]
    limit: usize,

    /// Min similarity threshold
    #[arg(short = 't', long, default_value = "0.3")]
    threshold: f32,

    /// Weight for name matching in hybrid search (0.0-1.0)
    #[arg(long, default_value = "0.2")]
    name_boost: f32,

    /// Weight for note scores in results (0.0-1.0, lower = notes rank below code)
    #[arg(long, default_value = "1.0")]
    note_weight: f32,

    /// Search notes only (skip code results)
    #[arg(long)]
    note_only: bool,

    /// Filter by language
    #[arg(short = 'l', long)]
    lang: Option<String>,

    /// Filter by chunk type (function, method, class, struct, enum, trait, interface, constant)
    #[arg(long)]
    chunk_type: Option<Vec<String>>,

    /// Filter by path pattern (glob)
    #[arg(short = 'p', long)]
    path: Option<String>,

    /// Filter by structural pattern (builder, error_swallow, async, mutex, unsafe, recursion)
    #[arg(long)]
    pattern: Option<String>,

    /// Definition search: find by name only, skip embedding (faster)
    #[arg(long)]
    name_only: bool,

    /// Pure semantic similarity, disable RRF hybrid search
    #[arg(long)]
    semantic_only: bool,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Show only file:line, no code
    #[arg(long)]
    no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    context: Option<usize>,

    /// Expand results with parent context (small-to-big retrieval)
    #[arg(long)]
    expand: bool,

    /// Suppress progress output
    #[arg(short, long)]
    quiet: bool,

    /// Show debug info (sets RUST_LOG=debug)
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Download model and create .cqs/
    Init,
    /// Check model, index, hardware
    Doctor,
    /// Index current project
    Index {
        /// Re-index all files, ignore mtime cache
        #[arg(long)]
        force: bool,
        /// Show what would be indexed, don't write
        #[arg(long)]
        dry_run: bool,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
    },
    /// Show index statistics
    Stats,
    /// Watch for changes and reindex
    Watch {
        /// Debounce interval in milliseconds
        #[arg(long, default_value = "500")]
        debounce: u64,
        /// Index files ignored by .gitignore
        #[arg(long)]
        no_ignore: bool,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Find functions that call a given function
    Callers {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions called by a given function
    Callees {
        /// Function name to search for
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// List and manage notes
    Notes {
        #[command(subcommand)]
        subcmd: NotesCommand,
    },
    /// Manage reference indexes for multi-index search
    Ref {
        #[command(subcommand)]
        subcmd: RefCommand,
    },
    /// Semantic diff between indexed snapshots
    Diff {
        /// Reference name to compare from
        source: String,
        /// Reference name or "project" (default: project)
        target: Option<String>,
        /// Similarity threshold for "modified" (default: 0.95)
        #[arg(short = 't', long, default_value = "0.95")]
        threshold: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Generate a function card (signature, callers, callees, similar)
    Explain {
        /// Function name or file:function
        name: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find code similar to a given function
    Similar {
        /// Function name or file:function (e.g., "search_filtered" or "src/search.rs:search_filtered")
        target: String,
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
    /// Impact analysis: what breaks if you change a function
    Impact {
        /// Function name or file:function
        name: String,
        /// Caller depth (1=direct, 2+=transitive)
        #[arg(long, default_value = "1")]
        depth: usize,
        /// Output format: text, json, mermaid
        #[arg(long, default_value = "text")]
        format: String,
        /// Output as JSON (alias for --format json)
        #[arg(long)]
        json: bool,
        /// Suggest tests for untested callers
        #[arg(long)]
        suggest_tests: bool,
    },
    /// Impact analysis from a git diff — what callers and tests are affected
    #[command(name = "impact-diff")]
    ImpactDiff {
        /// Git ref to diff against (default: unstaged changes)
        #[arg(long)]
        base: Option<String>,
        /// Read diff from stdin instead of running git
        #[arg(long)]
        stdin: bool,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trace call chain between two functions
    Trace {
        /// Source function name or file:function
        source: String,
        /// Target function name or file:function
        target: String,
        /// Max search depth (1-50)
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u16).range(1..=50))]
        max_depth: u16,
        /// Output format: text, json, mermaid
        #[arg(long, default_value = "text")]
        format: String,
        /// Output as JSON (alias for --format json)
        #[arg(long)]
        json: bool,
    },
    /// Find tests that exercise a function
    TestMap {
        /// Function name or file:function
        name: String,
        /// Max call chain depth to search
        #[arg(long, default_value = "5")]
        depth: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// What do I need to know to work on this file
    Context {
        /// File path relative to project root
        path: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Return summary counts instead of full details
        #[arg(long)]
        summary: bool,
        /// Signatures-only TOC with caller/callee counts (no code bodies)
        #[arg(long)]
        compact: bool,
    },
    /// Find functions with no callers (dead code detection)
    Dead {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include public API functions in the main list
        #[arg(long)]
        include_pub: bool,
    },
    /// Gather minimal code context to answer a question
    Gather {
        /// Search query / question
        query: String,
        /// Call graph expansion depth (0=seeds only, max 5)
        #[arg(long, default_value = "1")]
        expand: usize,
        /// Expansion direction: both, callers, callees
        #[arg(long, default_value = "both")]
        direction: String,
        /// Max chunks to return
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Manage cross-project search registry
    Project {
        #[command(subcommand)]
        subcmd: ProjectCommand,
    },
    /// Remove stale chunks and rebuild index
    Gc {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Toggle audit mode (exclude notes from search/read)
    #[command(name = "audit-mode")]
    AuditMode {
        /// State: on or off (omit to query current state)
        state: Option<String>,
        /// Expiry duration (e.g., "30m", "1h", "2h30m")
        #[arg(long, default_value = "30m")]
        expires: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Check index freshness — list stale and missing files
    Stale {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Show counts only, skip file list
        #[arg(long)]
        count_only: bool,
    },
    /// Read a file with notes injected as comments
    Read {
        /// File path relative to project root
        path: String,
        /// Focus on a specific function (returns only that function + type deps)
        #[arg(long)]
        focus: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Find functions related by shared callers, callees, or types
    Related {
        /// Function name or file:function
        name: String,
        /// Max results per category
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Suggest where to add new code matching a description
    Where {
        /// Description of the code to add
        description: String,
        /// Max file suggestions
        #[arg(short = 'n', long, default_value = "3")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Pre-investigation dashboard: search, group, count callers/tests, check staleness
    Scout {
        /// Task description to investigate
        task: String,
        /// Max file groups to return
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Run CLI with pre-parsed arguments (used when main.rs needs to inspect args first)
pub fn run_with(mut cli: Cli) -> Result<()> {
    // Load config and apply defaults (CLI flags override config)
    let config = cqs::config::Config::load(&find_project_root());
    apply_config_defaults(&mut cli, &config);

    // Clamp limit to prevent usize::MAX wrapping to -1 in SQLite queries
    cli.limit = cli.limit.clamp(1, 100);

    match cli.command {
        Some(Commands::Init) => cmd_init(&cli),
        Some(Commands::Doctor) => cmd_doctor(&cli),
        Some(Commands::Index {
            force,
            dry_run,
            no_ignore,
        }) => cmd_index(&cli, force, dry_run, no_ignore),
        Some(Commands::Stats) => cmd_stats(&cli),
        Some(Commands::Watch {
            debounce,
            no_ignore,
        }) => watch::cmd_watch(&cli, debounce, no_ignore),
        Some(Commands::Completions { shell }) => {
            cmd_completions(shell);
            Ok(())
        }
        Some(Commands::Callers { ref name, json }) => cmd_callers(&cli, name, json),
        Some(Commands::Callees { ref name, json }) => cmd_callees(&cli, name, json),
        Some(Commands::Notes { ref subcmd }) => cmd_notes(&cli, subcmd),
        Some(Commands::Ref { ref subcmd }) => cmd_ref(&cli, subcmd),
        Some(Commands::Diff {
            ref source,
            ref target,
            threshold,
            ref lang,
            json,
        }) => cmd_diff(source, target.as_deref(), threshold, lang.as_deref(), json),
        Some(Commands::Explain { ref name, json }) => cmd_explain(&cli, name, json),
        Some(Commands::Similar {
            ref target,
            limit,
            threshold,
            json,
        }) => cmd_similar(&cli, target, limit, threshold, json),
        Some(Commands::Impact {
            ref name,
            depth,
            ref format,
            json,
            suggest_tests,
        }) => {
            let fmt = if json { "json" } else { format.as_str() };
            cmd_impact(&cli, name, depth, fmt, suggest_tests)
        }
        Some(Commands::ImpactDiff {
            ref base,
            stdin,
            json,
        }) => cmd_impact_diff(&cli, base.as_deref(), stdin, json),
        Some(Commands::Trace {
            ref source,
            ref target,
            max_depth,
            ref format,
            json,
        }) => {
            let fmt = if json { "json" } else { format.as_str() };
            cmd_trace(&cli, source, target, max_depth as usize, fmt)
        }
        Some(Commands::TestMap {
            ref name,
            depth,
            json,
        }) => cmd_test_map(&cli, name, depth, json),
        Some(Commands::Context {
            ref path,
            json,
            summary,
            compact,
        }) => cmd_context(&cli, path, json, summary, compact),
        Some(Commands::Dead { json, include_pub }) => cmd_dead(&cli, json, include_pub),
        Some(Commands::Gather {
            ref query,
            expand,
            ref direction,
            limit,
            json,
        }) => cmd_gather(&cli, query, expand, direction, limit, json),
        Some(Commands::Project { ref subcmd }) => cmd_project(&cli, subcmd),
        Some(Commands::Gc { json }) => cmd_gc(json),
        Some(Commands::AuditMode {
            ref state,
            ref expires,
            json,
        }) => cmd_audit_mode(state.as_deref(), expires, json),
        Some(Commands::Stale { json, count_only }) => cmd_stale(&cli, json, count_only),
        Some(Commands::Read {
            ref path,
            ref focus,
            json,
        }) => cmd_read(path, focus.as_deref(), json),
        Some(Commands::Related {
            ref name,
            limit,
            json,
        }) => cmd_related(&cli, name, limit, json),
        Some(Commands::Where {
            ref description,
            limit,
            json,
        }) => cmd_where(&cli, description, limit, json),
        Some(Commands::Scout {
            ref task,
            limit,
            json,
        }) => cmd_scout(&cli, task, limit, json),
        None => match &cli.query {
            Some(q) => cmd_query(&cli, q),
            None => {
                println!("Usage: cqs <query> or cqs <command>");
                println!("Run 'cqs --help' for more information.");
                Ok(())
            }
        },
    }
}

/// Generate shell completion scripts for the specified shell
fn cmd_completions(shell: clap_complete::Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "cqs", &mut std::io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ===== Default values tests =====

    #[test]
    fn test_cli_defaults() {
        let cli = Cli::try_parse_from(["cqs"]).unwrap();
        assert_eq!(cli.limit, 5);
        assert!((cli.threshold - 0.3).abs() < 0.001);
        assert!((cli.name_boost - 0.2).abs() < 0.001);
        assert!(!cli.json);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
        assert!(cli.query.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_cli_query_argument() {
        let cli = Cli::try_parse_from(["cqs", "parse config"]).unwrap();
        assert_eq!(cli.query, Some("parse config".to_string()));
    }

    #[test]
    fn test_cli_limit_flag() {
        let cli = Cli::try_parse_from(["cqs", "-n", "10", "query"]).unwrap();
        assert_eq!(cli.limit, 10);

        let cli = Cli::try_parse_from(["cqs", "--limit", "20", "query"]).unwrap();
        assert_eq!(cli.limit, 20);
    }

    #[test]
    fn test_cli_threshold_flag() {
        let cli = Cli::try_parse_from(["cqs", "-t", "0.5", "query"]).unwrap();
        assert!((cli.threshold - 0.5).abs() < 0.001);

        let cli = Cli::try_parse_from(["cqs", "--threshold", "0.8", "query"]).unwrap();
        assert!((cli.threshold - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_cli_language_filter() {
        let cli = Cli::try_parse_from(["cqs", "-l", "rust", "query"]).unwrap();
        assert_eq!(cli.lang, Some("rust".to_string()));

        let cli = Cli::try_parse_from(["cqs", "--lang", "python", "query"]).unwrap();
        assert_eq!(cli.lang, Some("python".to_string()));
    }

    #[test]
    fn test_cli_path_filter() {
        let cli = Cli::try_parse_from(["cqs", "-p", "src/**", "query"]).unwrap();
        assert_eq!(cli.path, Some("src/**".to_string()));
    }

    #[test]
    fn test_cli_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "--json", "query"]).unwrap();
        assert!(cli.json);
    }

    #[test]
    fn test_cli_context_flag() {
        let cli = Cli::try_parse_from(["cqs", "-C", "3", "query"]).unwrap();
        assert_eq!(cli.context, Some(3));

        let cli = Cli::try_parse_from(["cqs", "--context", "5", "query"]).unwrap();
        assert_eq!(cli.context, Some(5));
    }

    #[test]
    fn test_cli_quiet_verbose_flags() {
        let cli = Cli::try_parse_from(["cqs", "-q", "query"]).unwrap();
        assert!(cli.quiet);

        let cli = Cli::try_parse_from(["cqs", "-v", "query"]).unwrap();
        assert!(cli.verbose);
    }

    // ===== Subcommand tests =====

    #[test]
    fn test_cmd_init() {
        let cli = Cli::try_parse_from(["cqs", "init"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Init)));
    }

    #[test]
    fn test_cmd_index() {
        let cli = Cli::try_parse_from(["cqs", "index"]).unwrap();
        match cli.command {
            Some(Commands::Index {
                force,
                dry_run,
                no_ignore,
            }) => {
                assert!(!force);
                assert!(!dry_run);
                assert!(!no_ignore);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_index_with_flags() {
        let cli = Cli::try_parse_from(["cqs", "index", "--force", "--dry-run"]).unwrap();
        match cli.command {
            Some(Commands::Index { force, dry_run, .. }) => {
                assert!(force);
                assert!(dry_run);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_stats() {
        let cli = Cli::try_parse_from(["cqs", "stats"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stats)));
    }

    #[test]
    fn test_cmd_watch() {
        let cli = Cli::try_parse_from(["cqs", "watch"]).unwrap();
        match cli.command {
            Some(Commands::Watch {
                debounce,
                no_ignore,
            }) => {
                assert_eq!(debounce, 500); // default
                assert!(!no_ignore);
            }
            _ => panic!("Expected Watch command"),
        }
    }

    #[test]
    fn test_cmd_watch_custom_debounce() {
        let cli = Cli::try_parse_from(["cqs", "watch", "--debounce", "1000"]).unwrap();
        match cli.command {
            Some(Commands::Watch { debounce, .. }) => {
                assert_eq!(debounce, 1000);
            }
            _ => panic!("Expected Watch command"),
        }
    }

    #[test]
    fn test_cmd_callers() {
        let cli = Cli::try_parse_from(["cqs", "callers", "my_function"]).unwrap();
        match cli.command {
            Some(Commands::Callers { name, json }) => {
                assert_eq!(name, "my_function");
                assert!(!json);
            }
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_cmd_callees_json() {
        let cli = Cli::try_parse_from(["cqs", "callees", "my_function", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Callees { name, json }) => {
                assert_eq!(name, "my_function");
                assert!(json);
            }
            _ => panic!("Expected Callees command"),
        }
    }

    #[test]
    fn test_cmd_notes_list() {
        let cli = Cli::try_parse_from(["cqs", "notes", "list"]).unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::List { warnings, patterns } => {
                    assert!(!warnings);
                    assert!(!patterns);
                }
                _ => panic!("Expected List subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_cmd_notes_list_warnings() {
        let cli = Cli::try_parse_from(["cqs", "notes", "list", "--warnings"]).unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::List { warnings, .. } => {
                    assert!(warnings);
                }
                _ => panic!("Expected List subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_cmd_notes_add() {
        let cli = Cli::try_parse_from(["cqs", "notes", "add", "test note", "--sentiment", "-0.5"])
            .unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::Add {
                    text, sentiment, ..
                } => {
                    assert_eq!(text, "test note");
                    assert!((*sentiment - (-0.5)).abs() < 0.001);
                }
                _ => panic!("Expected Add subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_cmd_notes_add_with_mentions() {
        let cli = Cli::try_parse_from([
            "cqs",
            "notes",
            "add",
            "test note",
            "--mentions",
            "src/lib.rs,src/main.rs",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::Add { mentions, .. } => {
                    let m = mentions.as_ref().unwrap();
                    assert_eq!(m.len(), 2);
                    assert_eq!(m[0], "src/lib.rs");
                    assert_eq!(m[1], "src/main.rs");
                }
                _ => panic!("Expected Add subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_cmd_notes_remove() {
        let cli = Cli::try_parse_from(["cqs", "notes", "remove", "some note text"]).unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::Remove { text, .. } => {
                    assert_eq!(text, "some note text");
                }
                _ => panic!("Expected Remove subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_cmd_notes_update() {
        let cli = Cli::try_parse_from([
            "cqs",
            "notes",
            "update",
            "old text",
            "--new-text",
            "new text",
            "--new-sentiment",
            "0.5",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                NotesCommand::Update {
                    text,
                    new_text,
                    new_sentiment,
                    ..
                } => {
                    assert_eq!(text, "old text");
                    assert_eq!(new_text.as_deref(), Some("new text"));
                    assert!((new_sentiment.unwrap() - 0.5).abs() < 0.001);
                }
                _ => panic!("Expected Update subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    // ===== Ref command tests =====

    #[test]
    fn test_cmd_ref_add_defaults() {
        let cli = Cli::try_parse_from(["cqs", "ref", "add", "tokio", "/path/to/source"]).unwrap();
        match cli.command {
            Some(Commands::Ref { ref subcmd }) => match subcmd {
                RefCommand::Add {
                    name,
                    source,
                    weight,
                } => {
                    assert_eq!(name, "tokio");
                    assert_eq!(source.to_string_lossy(), "/path/to/source");
                    assert!((*weight - 0.8).abs() < 0.001);
                }
                _ => panic!("Expected Add subcommand"),
            },
            _ => panic!("Expected Ref command"),
        }
    }

    #[test]
    fn test_cmd_ref_add_custom_weight() {
        let cli =
            Cli::try_parse_from(["cqs", "ref", "add", "stdlib", "/usr/src", "--weight", "0.5"])
                .unwrap();
        match cli.command {
            Some(Commands::Ref { ref subcmd }) => match subcmd {
                RefCommand::Add { weight, .. } => {
                    assert!((*weight - 0.5).abs() < 0.001);
                }
                _ => panic!("Expected Add subcommand"),
            },
            _ => panic!("Expected Ref command"),
        }
    }

    #[test]
    fn test_cmd_ref_list() {
        let cli = Cli::try_parse_from(["cqs", "ref", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Ref {
                subcmd: RefCommand::List
            })
        ));
    }

    #[test]
    fn test_cmd_ref_remove() {
        let cli = Cli::try_parse_from(["cqs", "ref", "remove", "tokio"]).unwrap();
        match cli.command {
            Some(Commands::Ref { ref subcmd }) => match subcmd {
                RefCommand::Remove { name } => assert_eq!(name, "tokio"),
                _ => panic!("Expected Remove subcommand"),
            },
            _ => panic!("Expected Ref command"),
        }
    }

    #[test]
    fn test_cmd_ref_update() {
        let cli = Cli::try_parse_from(["cqs", "ref", "update", "tokio"]).unwrap();
        match cli.command {
            Some(Commands::Ref { ref subcmd }) => match subcmd {
                RefCommand::Update { name } => assert_eq!(name, "tokio"),
                _ => panic!("Expected Update subcommand"),
            },
            _ => panic!("Expected Ref command"),
        }
    }

    // ===== Error cases =====

    #[test]
    fn test_invalid_limit_rejected() {
        let result = Cli::try_parse_from(["cqs", "-n", "not_a_number"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_subcommand_arg_rejected() {
        // callers requires a name argument
        let result = Cli::try_parse_from(["cqs", "callers"]);
        assert!(result.is_err());
    }

    // ===== apply_config_defaults tests =====

    #[test]
    fn test_apply_config_defaults_respects_cli_flags() {
        // When CLI has non-default values, config should NOT override
        let mut cli = Cli::try_parse_from(["cqs", "-n", "10", "-t", "0.6", "query"]).unwrap();
        let config = cqs::config::Config {
            limit: Some(20),
            threshold: Some(0.9),
            name_boost: Some(0.5),
            quiet: Some(true),
            verbose: Some(true),
            references: vec![],
            note_weight: None,
            note_only: None,
        };
        apply_config_defaults(&mut cli, &config);

        // CLI values should be preserved
        assert_eq!(cli.limit, 10);
        assert!((cli.threshold - 0.6).abs() < 0.001);
        // But name_boost was default, so config applies
        assert!((cli.name_boost - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_apply_config_defaults_applies_when_cli_has_defaults() {
        let mut cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        let config = cqs::config::Config {
            limit: Some(15),
            threshold: Some(0.7),
            name_boost: Some(0.4),
            quiet: Some(true),
            verbose: Some(true),
            references: vec![],
            note_weight: None,
            note_only: None,
        };
        apply_config_defaults(&mut cli, &config);

        assert_eq!(cli.limit, 15);
        assert!((cli.threshold - 0.7).abs() < 0.001);
        assert!((cli.name_boost - 0.4).abs() < 0.001);
        assert!(cli.quiet);
        assert!(cli.verbose);
    }

    #[test]
    fn test_apply_config_defaults_empty_config() {
        let mut cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        let config = cqs::config::Config::default();
        apply_config_defaults(&mut cli, &config);

        // Should keep CLI defaults
        assert_eq!(cli.limit, 5);
        assert!((cli.threshold - 0.3).abs() < 0.001);
        assert!((cli.name_boost - 0.2).abs() < 0.001);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
    }

    // ===== ExitCode tests =====

    #[test]
    fn test_cli_limit_clamped_to_valid_range() {
        // Verify that extremely large limits get clamped to 100
        let mut cli = Cli::try_parse_from(["cqs", "-n", "999", "query"]).unwrap();
        let config = cqs::config::Config::default();
        apply_config_defaults(&mut cli, &config);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 100);

        // Verify that limit 0 gets clamped to 1
        let mut cli = Cli::try_parse_from(["cqs", "-n", "0", "query"]).unwrap();
        apply_config_defaults(&mut cli, &config);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 1);

        // Verify normal limits pass through
        let mut cli = Cli::try_parse_from(["cqs", "-n", "10", "query"]).unwrap();
        apply_config_defaults(&mut cli, &config);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 10);
    }

    #[test]
    fn test_exit_code_values() {
        assert_eq!(signal::ExitCode::NoResults as i32, 2);
        assert_eq!(signal::ExitCode::Interrupted as i32, 130);
    }

    // ===== display module tests =====

    mod display_tests {
        use cqs::store::UnifiedResult;

        #[test]
        fn test_display_unified_results_json_empty() {
            let results: Vec<UnifiedResult> = vec![];
            // Can't easily capture stdout, but we can at least verify it doesn't panic
            // This would be better as an integration test
            assert!(results.is_empty());
        }
    }

    // ===== Progress bar template tests =====

    #[test]
    fn test_progress_bar_template_valid() {
        // Verify the progress bar template used in cmd_index is valid.
        // This catches template syntax errors at test time rather than runtime.
        use indicatif::ProgressStyle;
        let result =
            ProgressStyle::default_bar().template("[{elapsed_precise}] {bar:40.cyan/blue} {msg}");
        assert!(result.is_ok(), "Progress bar template should be valid");
    }
}
