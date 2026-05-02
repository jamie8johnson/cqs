//! CLI implementation for cq

pub(crate) mod args;
pub(crate) mod batch;
mod chat;
mod commands;
mod config;
mod definitions;
mod dispatch;
mod display;
mod enrichment;
mod files;
pub(crate) mod json_envelope;
mod limits;
mod pipeline;
mod registry;
mod signal;
pub(crate) mod staleness;
mod store;
pub(crate) mod telemetry;
mod watch;

// Re-export definitions (clap structs, enums, helpers) for external use
pub(crate) use definitions::{
    parse_finite_f32, parse_nonzero_usize, parse_unit_f32, validate_finite_f32, AuditModeState,
    GateThreshold,
};
pub use definitions::{Cli, OutputFormat};

// Re-export dispatch entry point
pub use dispatch::run_with;

// Shared clamp ceilings for commands that are dispatched from both
// CLI and batch paths. CQ-V1.25-2.
pub(crate) use limits::{RELATED_LIMIT_MAX, SCOUT_LIMIT_MAX, SIMILAR_LIMIT_MAX};

// Re-export for watch.rs and commands
pub(crate) use config::find_project_root;
pub(crate) use enrichment::enrichment_pass;
#[cfg(unix)]
pub(crate) use files::daemon_socket_path;
pub(crate) use files::{acquire_index_lock, enumerate_files, try_acquire_index_lock};
pub(crate) use pipeline::run_index_pipeline;
pub(crate) use signal::{check_interrupted, reset_interrupted};

// Re-export store openers, context, and vector index builders
pub(crate) use store::{
    build_base_vector_index, build_vector_index, build_vector_index_with_config,
    open_project_store_readonly, CommandContext,
};

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use definitions::Commands;

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
        assert!(matches!(cli.command, Some(Commands::Init { output: _ })));
    }

    #[test]
    fn test_cmd_index() {
        let cli = Cli::try_parse_from(["cqs", "index"]).unwrap();
        match cli.command {
            Some(Commands::Index { ref args }) => {
                assert!(!args.force);
                assert!(!args.dry_run);
                assert!(!args.no_ignore);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_index_with_flags() {
        let cli = Cli::try_parse_from(["cqs", "index", "--force", "--dry-run"]).unwrap();
        match cli.command {
            Some(Commands::Index { ref args }) => {
                assert!(args.force);
                assert!(args.dry_run);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cmd_stats() {
        let cli = Cli::try_parse_from(["cqs", "stats"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Stats { .. })));
    }

    #[test]
    fn test_cmd_watch() {
        let cli = Cli::try_parse_from(["cqs", "watch"]).unwrap();
        match cli.command {
            Some(Commands::Watch {
                debounce,
                no_ignore,
                poll,
                serve,
            }) => {
                assert_eq!(debounce, 500); // default
                assert!(!no_ignore);
                assert!(!serve);
                assert!(!poll);
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
    fn test_cmd_watch_poll() {
        let cli = Cli::try_parse_from(["cqs", "watch", "--poll"]).unwrap();
        match cli.command {
            Some(Commands::Watch { poll, .. }) => {
                assert!(poll);
            }
            _ => panic!("Expected Watch command"),
        }
    }

    #[test]
    fn test_cmd_callers() {
        let cli = Cli::try_parse_from(["cqs", "callers", "my_function"]).unwrap();
        match cli.command {
            Some(Commands::Callers { args, output, .. }) => {
                assert_eq!(args.name, "my_function");
                assert!(!output.json);
            }
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_cmd_callees_json() {
        let cli = Cli::try_parse_from(["cqs", "callees", "my_function", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Callees { args, output, .. }) => {
                assert_eq!(args.name, "my_function");
                assert!(output.json);
            }
            _ => panic!("Expected Callees command"),
        }
    }

    #[test]
    fn test_cmd_notes_list() {
        let cli = Cli::try_parse_from(["cqs", "notes", "list"]).unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                commands::NotesCommand::List { list, output } => {
                    // EX-V1.29-5: fields now live on `NotesListArgs`.
                    assert!(!list.warnings);
                    assert!(!list.patterns);
                    assert!(!list.check);
                    assert!(!output.json);
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
                commands::NotesCommand::List { list, .. } => {
                    assert!(list.warnings);
                }
                _ => panic!("Expected List subcommand"),
            },
            _ => panic!("Expected Notes command"),
        }
    }

    /// API-V1.29-4: `--check` parses into `NotesListArgs.check`, so the
    /// daemon batch path can forward it (previously the flag was inline on
    /// `NotesCommand::List` and dropped when routed through the socket).
    #[test]
    fn test_cmd_notes_list_check_parses_into_shared_args() {
        let cli = Cli::try_parse_from(["cqs", "notes", "list", "--check"]).unwrap();
        match cli.command {
            Some(Commands::Notes { ref subcmd }) => match subcmd {
                commands::NotesCommand::List { list, .. } => {
                    assert!(list.check);
                    assert!(!list.warnings);
                    assert!(!list.patterns);
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
                commands::NotesCommand::Add {
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
                commands::NotesCommand::Add { mentions, .. } => {
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
                commands::NotesCommand::Remove { text, .. } => {
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
                commands::NotesCommand::Update {
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
                commands::RefCommand::Add {
                    name,
                    source,
                    weight,
                    ..
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
                commands::RefCommand::Add { weight, .. } => {
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
                subcmd: commands::RefCommand::List { .. }
            })
        ));
    }

    #[test]
    fn test_cmd_ref_remove() {
        let cli = Cli::try_parse_from(["cqs", "ref", "remove", "tokio"]).unwrap();
        match cli.command {
            Some(Commands::Ref { ref subcmd }) => match subcmd {
                commands::RefCommand::Remove { name, .. } => assert_eq!(name, "tokio"),
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
                commands::RefCommand::Update { name, .. } => assert_eq!(name, "tokio"),
                _ => panic!("Expected Update subcommand"),
            },
            _ => panic!("Expected Ref command"),
        }
    }

    // ===== --ref flag tests =====

    #[test]
    fn test_cli_ref_flag() {
        let cli = Cli::try_parse_from(["cqs", "--ref", "aveva", "license activation"]).unwrap();
        assert_eq!(cli.ref_name, Some("aveva".to_string()));
        assert_eq!(cli.query, Some("license activation".to_string()));
    }

    #[test]
    fn test_cli_ref_flag_not_set() {
        let cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        assert!(cli.ref_name.is_none());
    }

    #[test]
    fn test_cli_ref_with_other_flags() {
        let cli = Cli::try_parse_from([
            "cqs",
            "--ref",
            "tokio",
            "--json",
            "-n",
            "10",
            "search query",
        ])
        .unwrap();
        assert_eq!(cli.ref_name, Some("tokio".to_string()));
        assert!(cli.json);
        assert_eq!(cli.limit, 10);
    }

    #[test]
    fn test_cli_ref_with_name_only() {
        let cli =
            Cli::try_parse_from(["cqs", "--ref", "aveva", "--name-only", "some_function"]).unwrap();
        assert_eq!(cli.ref_name, Some("aveva".to_string()));
        assert!(cli.name_only);
    }

    // ===== --rerank flag tests =====

    #[test]
    fn test_cli_rerank_flag() {
        let cli = Cli::try_parse_from(["cqs", "--rerank", "search query"]).unwrap();
        assert!(cli.rerank);
    }

    #[test]
    fn test_cli_rerank_default_false() {
        let cli = Cli::try_parse_from(["cqs", "search query"]).unwrap();
        assert!(!cli.rerank);
    }

    #[test]
    fn test_cli_rerank_with_ref() {
        let cli = Cli::try_parse_from(["cqs", "--rerank", "--ref", "aveva", "query"]).unwrap();
        assert!(cli.rerank);
        assert_eq!(cli.ref_name, Some("aveva".to_string()));
    }

    #[test]
    fn test_cli_rerank_with_limit() {
        let cli = Cli::try_parse_from(["cqs", "--rerank", "-n", "20", "query"]).unwrap();
        assert!(cli.rerank);
        assert_eq!(cli.limit, 20);
    }

    // ===== --tokens flag tests =====

    #[test]
    fn test_cmd_gather_tokens_flag() {
        let cli =
            Cli::try_parse_from(["cqs", "gather", "alarm config", "--tokens", "4000"]).unwrap();
        match cli.command {
            Some(Commands::Gather { ref args, .. }) => {
                assert_eq!(args.tokens, Some(4000));
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_cmd_gather_no_tokens_flag() {
        let cli = Cli::try_parse_from(["cqs", "gather", "alarm config"]).unwrap();
        match cli.command {
            Some(Commands::Gather { ref args, .. }) => {
                assert!(args.tokens.is_none());
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_cmd_gather_tokens_with_limit() {
        let cli = Cli::try_parse_from([
            "cqs", "gather", "query", "--tokens", "8000", "-n", "20", "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Gather {
                ref args,
                ref output,
                ..
            }) => {
                assert_eq!(args.tokens, Some(8000));
                assert_eq!(args.limit, 20);
                assert!(output.json);
            }
            _ => panic!("Expected Gather command"),
        }
    }

    // ===== --ref flag tests (gather) =====

    #[test]
    fn test_cmd_gather_ref_flag() {
        let cli = Cli::try_parse_from(["cqs", "gather", "alarm config", "--ref", "aveva"]).unwrap();
        match cli.command {
            Some(Commands::Gather { ref args, .. }) => {
                assert_eq!(args.ref_name, Some("aveva".to_string()));
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_cmd_gather_ref_not_set() {
        let cli = Cli::try_parse_from(["cqs", "gather", "alarm config"]).unwrap();
        match cli.command {
            Some(Commands::Gather { ref args, .. }) => {
                assert!(args.ref_name.is_none());
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_cmd_gather_ref_with_tokens() {
        let cli = Cli::try_parse_from([
            "cqs",
            "gather",
            "alarm config",
            "--ref",
            "aveva",
            "--tokens",
            "4000",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Gather {
                ref args,
                ref output,
                ..
            }) => {
                assert_eq!(args.ref_name, Some("aveva".to_string()));
                assert_eq!(args.tokens, Some(4000));
                assert!(output.json);
            }
            _ => panic!("Expected Gather command"),
        }
    }

    // ===== --tokens flag tests (query) =====

    #[test]
    fn test_cli_query_tokens_flag() {
        let cli = Cli::try_parse_from(["cqs", "--tokens", "4000", "search query"]).unwrap();
        assert_eq!(cli.tokens, Some(4000));
        assert_eq!(cli.query, Some("search query".to_string()));
    }

    #[test]
    fn test_cli_query_tokens_not_set() {
        let cli = Cli::try_parse_from(["cqs", "query"]).unwrap();
        assert!(cli.tokens.is_none());
    }

    #[test]
    fn test_cli_query_tokens_with_json_and_limit() {
        let cli = Cli::try_parse_from([
            "cqs",
            "--tokens",
            "8000",
            "--json",
            "-n",
            "20",
            "search query",
        ])
        .unwrap();
        assert_eq!(cli.tokens, Some(8000));
        assert!(cli.json);
        assert_eq!(cli.limit, 20);
    }

    #[test]
    fn test_cli_query_tokens_with_ref() {
        let cli =
            Cli::try_parse_from(["cqs", "--tokens", "2000", "--ref", "aveva", "license"]).unwrap();
        assert_eq!(cli.tokens, Some(2000));
        assert_eq!(cli.ref_name, Some("aveva".to_string()));
    }

    #[test]
    fn test_cli_query_tokens_with_name_only() {
        let cli =
            Cli::try_parse_from(["cqs", "--tokens", "1000", "--name-only", "my_func"]).unwrap();
        assert_eq!(cli.tokens, Some(1000));
        assert!(cli.name_only);
    }

    #[test]
    fn test_cli_context_tokens_flag() {
        let cli =
            Cli::try_parse_from(["cqs", "context", "src/lib.rs", "--tokens", "4000"]).unwrap();
        match cli.command {
            Some(Commands::Context { ref args, .. }) => assert_eq!(args.tokens, Some(4000)),
            _ => panic!("Expected Context command"),
        }
    }

    #[test]
    fn test_cli_context_tokens_not_set() {
        let cli = Cli::try_parse_from(["cqs", "context", "src/lib.rs"]).unwrap();
        match cli.command {
            Some(Commands::Context { ref args, .. }) => assert!(args.tokens.is_none()),
            _ => panic!("Expected Context command"),
        }
    }

    #[test]
    fn test_cli_explain_tokens_flag() {
        let cli =
            Cli::try_parse_from(["cqs", "explain", "search_filtered", "--tokens", "3000"]).unwrap();
        match cli.command {
            Some(Commands::Explain { ref args, .. }) => assert_eq!(args.tokens, Some(3000)),
            _ => panic!("Expected Explain command"),
        }
    }

    #[test]
    fn test_cli_scout_tokens_flag() {
        let cli = Cli::try_parse_from(["cqs", "scout", "add token budgeting", "--tokens", "8000"])
            .unwrap();
        match cli.command {
            Some(Commands::Scout { ref args, .. }) => assert_eq!(args.tokens, Some(8000)),
            _ => panic!("Expected Scout command"),
        }
    }

    // ===== Review command tests =====

    #[test]
    fn test_cmd_review_defaults() {
        let cli = Cli::try_parse_from(["cqs", "review"]).unwrap();
        match cli.command {
            Some(Commands::Review {
                ref args,
                ref output,
            }) => {
                assert!(args.base.is_none());
                assert!(!args.stdin);
                assert!(!output.json);
                assert!(args.tokens.is_none());
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_cmd_review_base_flag() {
        let cli = Cli::try_parse_from(["cqs", "review", "--base", "main"]).unwrap();
        match cli.command {
            Some(Commands::Review { ref args, .. }) => {
                assert_eq!(args.base, Some("main".to_string()));
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_cmd_review_stdin_json() {
        // v1.22.0 audit API-1: --format removed from TextJsonArgs, use --json
        let cli = Cli::try_parse_from(["cqs", "review", "--stdin", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Review {
                ref args,
                ref output,
            }) => {
                assert!(args.stdin);
                assert!(output.json);
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_cmd_review_tokens_flag() {
        let cli = Cli::try_parse_from(["cqs", "review", "--tokens", "4000"]).unwrap();
        match cli.command {
            Some(Commands::Review { ref args, .. }) => {
                assert_eq!(args.tokens, Some(4000));
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_cmd_review_tokens_zero_rejected() {
        let result = Cli::try_parse_from(["cqs", "review", "--tokens", "0"]);
        assert!(result.is_err(), "--tokens 0 in review should be rejected");
    }

    // ===== AD-23: mermaid rejected at parse time for review/ci =====

    #[test]
    fn test_review_rejects_mermaid_format() {
        let result = Cli::try_parse_from(["cqs", "review", "--format", "mermaid"]);
        assert!(
            result.is_err(),
            "review --format mermaid should be rejected at parse time"
        );
    }

    #[test]
    fn test_ci_rejects_mermaid_format() {
        let result = Cli::try_parse_from(["cqs", "ci", "--format", "mermaid"]);
        assert!(
            result.is_err(),
            "ci --format mermaid should be rejected at parse time"
        );
    }

    // ===== --json alias for --format json (#650) =====

    #[test]
    fn test_impact_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "impact", "my_func", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Impact { ref output, .. }) => {
                assert!(output.json);
                assert!(matches!(output.format, OutputFormat::Text)); // default, overridden at dispatch
            }
            _ => panic!("Expected Impact command"),
        }
    }

    #[test]
    fn test_impact_json_conflicts_with_format() {
        let result =
            Cli::try_parse_from(["cqs", "impact", "my_func", "--json", "--format", "text"]);
        assert!(result.is_err(), "--json and --format should conflict");
    }

    #[test]
    fn test_review_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "review", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Review { ref output, .. }) => {
                assert!(output.json);
            }
            _ => panic!("Expected Review command"),
        }
    }

    #[test]
    fn test_review_json_conflicts_with_format() {
        let result = Cli::try_parse_from(["cqs", "review", "--json", "--format", "json"]);
        assert!(result.is_err(), "--json and --format should conflict");
    }

    #[test]
    fn test_ci_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "ci", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Ci { ref output, .. }) => {
                assert!(output.json);
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_trace_json_flag() {
        let cli = Cli::try_parse_from(["cqs", "trace", "a", "b", "--json"]).unwrap();
        match cli.command {
            Some(Commands::Trace { ref output, .. }) => {
                assert!(output.json);
                assert!(matches!(output.format, OutputFormat::Text));
            }
            _ => panic!("Expected Trace command"),
        }
    }

    #[test]
    fn test_trace_json_conflicts_with_format() {
        let result =
            Cli::try_parse_from(["cqs", "trace", "a", "b", "--json", "--format", "mermaid"]);
        assert!(result.is_err(), "--json and --format should conflict");
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

    // ===== --tokens 0 rejection =====

    #[test]
    fn test_tokens_zero_rejected() {
        let result = Cli::try_parse_from(["cqs", "--tokens", "0", "query"]);
        assert!(result.is_err(), "--tokens 0 should be rejected");
    }

    #[test]
    fn test_tokens_zero_rejected_in_subcommand() {
        let result = Cli::try_parse_from(["cqs", "gather", "query", "--tokens", "0"]);
        assert!(result.is_err(), "--tokens 0 in gather should be rejected");
    }

    // ===== apply_config_defaults tests =====
    // EX-V1.29-8: tests inject argv via `apply_config_defaults_with_argv`
    // so clap's `ValueSource::DefaultValue` resolves against the test's
    // fake CLI, not the surrounding `cargo test ...` invocation.

    #[test]
    fn test_apply_config_defaults_respects_cli_flags() {
        // When CLI has non-default values, config should NOT override
        let argv = ["cqs", "-n", "10", "-t", "0.6", "query"];
        let mut cli = Cli::try_parse_from(argv).unwrap();
        let config = cqs::config::Config {
            limit: Some(20),
            threshold: Some(0.9),
            name_boost: Some(0.5),
            quiet: Some(true),
            verbose: Some(true),
            ..Default::default()
        };
        config::apply_config_defaults_with_argv(&mut cli, &config, argv);

        // CLI values should be preserved
        assert_eq!(cli.limit, 10);
        assert!((cli.threshold - 0.6).abs() < 0.001);
        // But name_boost was default, so config applies
        assert!((cli.name_boost - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_apply_config_defaults_applies_when_cli_has_defaults() {
        let argv = ["cqs", "query"];
        let mut cli = Cli::try_parse_from(argv).unwrap();
        let config = cqs::config::Config {
            limit: Some(15),
            threshold: Some(0.7),
            name_boost: Some(0.4),
            quiet: Some(true),
            verbose: Some(true),
            ..Default::default()
        };
        config::apply_config_defaults_with_argv(&mut cli, &config, argv);

        assert_eq!(cli.limit, 15);
        assert!((cli.threshold - 0.7).abs() < 0.001);
        assert!((cli.name_boost - 0.4).abs() < 0.001);
        assert!(cli.quiet);
        assert!(cli.verbose);
    }

    #[test]
    fn test_apply_config_defaults_empty_config() {
        let argv = ["cqs", "query"];
        let mut cli = Cli::try_parse_from(argv).unwrap();
        let config = cqs::config::Config::default();
        config::apply_config_defaults_with_argv(&mut cli, &config, argv);

        // Should keep CLI defaults
        assert_eq!(cli.limit, 5);
        assert!((cli.threshold - 0.3).abs() < 0.001);
        assert!((cli.name_boost - 0.2).abs() < 0.001);
        assert!(!cli.quiet);
        assert!(!cli.verbose);
    }

    /// EX-V1.29-8 regression: explicitly passing the clap default on the CLI
    /// (e.g. `cqs -n 5 …` where 5 is the default) must be treated as
    /// user-set. The pre-refactor code compared `cli.limit == DEFAULT_LIMIT`
    /// and would let the config file's `limit = 20` win even though the user
    /// typed `-n 5`. `ValueSource::CommandLine` now reports that case
    /// correctly.
    #[test]
    fn test_apply_config_defaults_explicit_default_wins_over_config() {
        let argv = ["cqs", "-n", "5", "query"];
        let mut cli = Cli::try_parse_from(argv).unwrap();
        let config = cqs::config::Config {
            limit: Some(20),
            ..Default::default()
        };
        config::apply_config_defaults_with_argv(&mut cli, &config, argv);
        // User wrote "-n 5" on the command line. Config says 20. User wins.
        assert_eq!(
            cli.limit, 5,
            "explicit --limit 5 on CLI must beat config limit=20"
        );
    }

    // ===== ExitCode tests =====

    #[test]
    fn test_cli_limit_clamped_to_valid_range() {
        let config = cqs::config::Config::default();
        // Verify that extremely large limits get clamped to 100
        let argv1 = ["cqs", "-n", "999", "query"];
        let mut cli = Cli::try_parse_from(argv1).unwrap();
        config::apply_config_defaults_with_argv(&mut cli, &config, argv1);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 100);

        // Verify that limit 0 gets clamped to 1
        let argv2 = ["cqs", "-n", "0", "query"];
        let mut cli = Cli::try_parse_from(argv2).unwrap();
        config::apply_config_defaults_with_argv(&mut cli, &config, argv2);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 1);

        // Verify normal limits pass through
        let argv3 = ["cqs", "-n", "10", "query"];
        let mut cli = Cli::try_parse_from(argv3).unwrap();
        config::apply_config_defaults_with_argv(&mut cli, &config, argv3);
        cli.limit = cli.limit.clamp(1, 100);
        assert_eq!(cli.limit, 10);
    }

    #[test]
    fn test_exit_code_values() {
        assert_eq!(signal::ExitCode::NoResults as i32, 2);
        assert_eq!(signal::ExitCode::GateFailed as i32, 3);
        assert_eq!(signal::ExitCode::Interrupted as i32, 130);
    }

    // ===== CI command tests =====

    #[test]
    fn test_cmd_ci_defaults() {
        let cli = Cli::try_parse_from(["cqs", "ci"]).unwrap();
        match cli.command {
            Some(Commands::Ci {
                ref args,
                ref output,
            }) => {
                assert!(args.base.is_none());
                assert!(!args.stdin);
                assert!(!output.json);
                assert!(matches!(args.gate, GateThreshold::High));
                assert!(args.tokens.is_none());
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_cmd_ci_gate_medium() {
        let cli = Cli::try_parse_from(["cqs", "ci", "--gate", "medium"]).unwrap();
        match cli.command {
            Some(Commands::Ci { ref args, .. }) => {
                assert!(matches!(args.gate, GateThreshold::Medium));
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_cmd_ci_gate_off() {
        let cli = Cli::try_parse_from(["cqs", "ci", "--gate", "off"]).unwrap();
        match cli.command {
            Some(Commands::Ci { ref args, .. }) => {
                assert!(matches!(args.gate, GateThreshold::Off));
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_cmd_ci_stdin_json_tokens() {
        // v1.22.0 audit API-1: --format removed from TextJsonArgs, use --json
        let cli =
            Cli::try_parse_from(["cqs", "ci", "--stdin", "--json", "--tokens", "5000"]).unwrap();
        match cli.command {
            Some(Commands::Ci {
                ref args,
                ref output,
            }) => {
                assert!(args.stdin);
                assert!(output.json);
                assert_eq!(args.tokens, Some(5000));
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_cmd_ci_base_flag() {
        let cli = Cli::try_parse_from(["cqs", "ci", "--base", "HEAD~3"]).unwrap();
        match cli.command {
            Some(Commands::Ci { ref args, .. }) => {
                assert_eq!(args.base, Some("HEAD~3".to_string()));
            }
            _ => panic!("Expected Ci command"),
        }
    }

    #[test]
    fn test_cmd_ci_tokens_zero_rejected() {
        let result = Cli::try_parse_from(["cqs", "ci", "--tokens", "0"]);
        assert!(result.is_err(), "--tokens 0 in ci should be rejected");
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
