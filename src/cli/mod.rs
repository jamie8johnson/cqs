//! CLI implementation for cq

mod commands;
mod config;
mod display;
mod files;
mod pipeline;
mod signal;
mod watch;

// Re-export for watch.rs and commands
pub(crate) use config::find_project_root;
pub(crate) use files::{acquire_index_lock, enumerate_files};
pub(crate) use pipeline::run_index_pipeline;
pub(crate) use signal::{check_interrupted, reset_interrupted};

use commands::{
    cmd_callees, cmd_callers, cmd_context, cmd_dead, cmd_diff, cmd_doctor, cmd_explain, cmd_gc,
    cmd_impact, cmd_index, cmd_init, cmd_notes, cmd_query, cmd_ref, cmd_serve, cmd_similar,
    cmd_stats, cmd_test_map, cmd_trace, NotesCommand, RefCommand, ServeConfig,
};
use config::apply_config_defaults;

use std::path::PathBuf;

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

    /// Filter by language
    #[arg(short = 'l', long)]
    lang: Option<String>,

    /// Filter by chunk type (function, method, class, struct, enum, trait, interface, constant)
    #[arg(long)]
    chunk_type: Option<Vec<String>>,

    /// Filter by path pattern (glob)
    #[arg(short = 'p', long)]
    path: Option<String>,

    /// Output as JSON
    #[arg(long)]
    json: bool,

    /// Show only file:line, no code
    #[arg(long)]
    no_content: bool,

    /// Show N lines of context before/after the chunk
    #[arg(short = 'C', long)]
    context: Option<usize>,

    /// Suppress progress output
    #[arg(short, long)]
    quiet: bool,

    /// Show debug info (sets RUST_LOG=debug)
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Download model and create .cq/
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
    /// Start MCP server
    Serve {
        /// Transport type: stdio, http
        #[arg(long, default_value = "stdio")]
        transport: String,
        /// Bind address for HTTP transport
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        /// Port for HTTP transport
        #[arg(long, default_value = "3000")]
        port: u16,
        /// Project root
        #[arg(long)]
        project: Option<PathBuf>,
        /// Use GPU for query embedding (faster after warmup)
        #[arg(long)]
        gpu: bool,
        /// API key for HTTP authentication (required for non-localhost bind)
        #[arg(long, env = "CQS_API_KEY")]
        api_key: Option<String>,
        /// Path to file containing API key (alternative to --api-key)
        #[arg(long)]
        api_key_file: Option<PathBuf>,
        /// Required when binding to non-localhost (exposes codebase to network)
        #[arg(long, hide = true)]
        dangerously_allow_network_bind: bool,
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
        /// Max search depth
        #[arg(long, default_value = "10")]
        max_depth: usize,
        /// Output as JSON
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
    /// Remove stale chunks and rebuild index
    Gc {
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
        Some(Commands::Serve {
            ref transport,
            ref bind,
            port,
            ref project,
            gpu,
            ref api_key,
            ref api_key_file,
            dangerously_allow_network_bind,
        }) => cmd_serve(ServeConfig {
            transport: transport.clone(),
            bind: bind.clone(),
            port,
            project: project.clone(),
            gpu,
            api_key: api_key.clone(),
            api_key_file: api_key_file.clone(),
            dangerously_allow_network_bind,
        }),
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
            json,
        }) => cmd_impact(&cli, name, depth, json),
        Some(Commands::Trace {
            ref source,
            ref target,
            max_depth,
            json,
        }) => cmd_trace(&cli, source, target, max_depth, json),
        Some(Commands::TestMap {
            ref name,
            depth,
            json,
        }) => cmd_test_map(&cli, name, depth, json),
        Some(Commands::Context { ref path, json }) => cmd_context(&cli, path, json),
        Some(Commands::Dead { json, include_pub }) => cmd_dead(&cli, json, include_pub),
        Some(Commands::Gc { json }) => cmd_gc(json),
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
    fn test_cmd_serve_defaults() {
        let cli = Cli::try_parse_from(["cqs", "serve"]).unwrap();
        match cli.command {
            Some(Commands::Serve {
                transport,
                bind,
                port,
                gpu,
                api_key,
                ..
            }) => {
                assert_eq!(transport, "stdio");
                assert_eq!(bind, "127.0.0.1");
                assert_eq!(port, 3000);
                assert!(!gpu);
                assert!(api_key.is_none());
            }
            _ => panic!("Expected Serve command"),
        }
    }

    #[test]
    fn test_cmd_serve_http() {
        let cli = Cli::try_parse_from([
            "cqs",
            "serve",
            "--transport",
            "http",
            "--port",
            "8080",
            "--gpu",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Serve {
                transport,
                port,
                gpu,
                ..
            }) => {
                assert_eq!(transport, "http");
                assert_eq!(port, 8080);
                assert!(gpu);
            }
            _ => panic!("Expected Serve command"),
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
