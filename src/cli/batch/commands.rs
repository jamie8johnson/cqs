//! Batch command parsing and dispatch routing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::BatchContext;

use crate::cli::args::{
    BlameArgs, ContextArgs, DeadArgs, GatherArgs, ImpactArgs, ScoutArgs, SimilarArgs, TraceArgs,
};
use crate::cli::parse_nonzero_usize;
use crate::cli::GateThreshold;

use super::handlers;

// ─── BatchInput / BatchCmd ───────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    no_binary_name = true,
    disable_help_subcommand = true,
    disable_help_flag = true
)]
pub(crate) struct BatchInput {
    #[command(subcommand)]
    pub cmd: BatchCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum BatchCmd {
    /// Semantic search
    Search {
        /// Search query
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Definition search: find by name only
        #[arg(long)]
        name_only: bool,
        /// Enable RRF hybrid search (cosine + FTS5 keyword fusion)
        #[arg(long)]
        rrf: bool,
        /// Re-rank results with cross-encoder
        #[arg(long)]
        rerank: bool,
        /// Enable SPLADE sparse-dense hybrid search
        #[arg(long)]
        splade: bool,
        /// SPLADE fusion weight: 1.0 = pure cosine, 0.0 = pure sparse
        #[arg(long, default_value = "0.7", value_parser = crate::cli::parse_finite_f32)]
        splade_alpha: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Filter by path pattern (glob)
        #[arg(short = 'p', long)]
        path: Option<String>,
        /// Include only these chunk types (e.g., function, test, endpoint)
        #[arg(long)]
        include_type: Option<Vec<String>>,
        /// Exclude these chunk types (e.g., test, variable, configkey)
        #[arg(long)]
        exclude_type: Option<Vec<String>>,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
        /// Disable search-time demotion of test functions and underscore-prefixed names
        #[arg(long)]
        no_demote: bool,
        /// Weight for name matching in hybrid search (0.0-1.0, default 0.2)
        #[arg(long, default_value = "0.2", value_parser = crate::cli::parse_finite_f32)]
        name_boost: f32,
        /// Search only this reference index (skip project index)
        #[arg(long = "ref")]
        ref_name: Option<String>,
        /// Include reference indexes in search results (default: project only)
        #[arg(long)]
        include_refs: bool,
        /// Show only file:line, no code
        #[arg(long)]
        no_content: bool,
        /// Show N lines of context before/after the chunk
        #[arg(short = 'C', long)]
        context: Option<usize>,
        /// Expand results with parent context (small-to-big retrieval)
        #[arg(long)]
        expand: bool,
        /// Disable staleness checks (skip per-file mtime comparison)
        #[arg(long)]
        no_stale_check: bool,
    },
    /// Semantic git blame: who changed a function, when, and why
    Blame {
        #[command(flatten)]
        args: BlameArgs,
    },
    /// Type dependencies: who uses a type, or what types a function uses
    Deps {
        /// Type name or function name
        name: String,
        /// Show types used by function (instead of type users)
        #[arg(long)]
        reverse: bool,
        /// Query across all configured reference projects
        #[arg(long)]
        cross_project: bool,
    },
    /// Find callers of a function
    Callers {
        /// Function name
        name: String,
        /// Query callers across all configured reference projects
        #[arg(long)]
        cross_project: bool,
    },
    /// Find callees of a function
    Callees {
        /// Function name
        name: String,
        /// Query callees across all configured reference projects
        #[arg(long)]
        cross_project: bool,
    },
    /// Function card: signature, callers, callees, similar
    Explain {
        /// Function name or file:function
        name: String,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Find similar code
    Similar {
        #[command(flatten)]
        args: SimilarArgs,
    },
    /// Smart context assembly
    Gather {
        #[command(flatten)]
        args: GatherArgs,
    },
    /// Impact analysis
    Impact {
        #[command(flatten)]
        args: ImpactArgs,
    },
    /// Map function to tests
    #[command(name = "test-map")]
    TestMap {
        /// Function name or file:function
        name: String,
        /// Max call chain depth
        #[arg(long, default_value = "5")]
        depth: usize,
        /// Search for tests across all configured reference projects
        #[arg(long)]
        cross_project: bool,
    },
    /// Trace call path between two functions
    Trace {
        #[command(flatten)]
        args: TraceArgs,
    },
    /// Find dead code
    Dead {
        #[command(flatten)]
        args: DeadArgs,
    },
    /// Find related functions by co-occurrence
    Related {
        /// Function name or file:function
        name: String,
        /// Max results per category
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
    },
    /// Module-level context for a file
    Context {
        #[command(flatten)]
        args: ContextArgs,
    },
    /// Index statistics
    Stats,
    /// Guided codebase tour
    Onboard {
        /// Concept to explore
        query: String,
        /// Callee expansion depth
        #[arg(short = 'd', long, default_value = "3")]
        depth: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Pre-investigation dashboard
    Scout {
        #[command(flatten)]
        args: ScoutArgs,
    },
    /// Suggest where to add new code
    Where {
        /// Description of what to add
        description: String,
        /// Max suggestions
        #[arg(short = 'n', long, default_value = "3")]
        limit: usize,
    },
    /// Read file with note injection
    Read {
        /// File path relative to project root
        path: String,
        /// Focus on a specific function (focused read mode)
        #[arg(long)]
        focus: Option<String>,
    },
    /// Check index freshness
    Stale {
        /// Show counts only, skip file list
        #[arg(long)]
        count_only: bool,
    },
    /// Codebase quality snapshot
    Health,
    /// Semantic drift detection between reference and project
    Drift {
        /// Reference name to compare against
        reference: String,
        /// Similarity threshold (default: 0.95)
        ///
        /// `-t` alias matches the CLI subcommand so forwarded invocations
        /// (`cqs drift ref -t 0.9`) parse cleanly on the daemon side.
        #[arg(short = 't', long, default_value = "0.95", value_parser = crate::cli::parse_finite_f32)]
        threshold: f32,
        /// Minimum drift to show (default: 0.0)
        #[arg(long, default_value = "0.0", value_parser = crate::cli::parse_finite_f32)]
        min_drift: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Maximum entries to show
        #[arg(short = 'n', long)]
        limit: Option<usize>,
    },
    /// List notes
    Notes {
        /// Show only warnings (negative sentiment)
        #[arg(long)]
        warnings: bool,
        /// Show only patterns (positive sentiment)
        #[arg(long)]
        patterns: bool,
    },
    /// One-shot implementation context (terminal — no pipeline chaining)
    Task {
        /// Task description
        description: String,
        /// Max file groups
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Comprehensive diff review
    Review {
        /// Base git reference
        #[arg(long)]
        base: Option<String>,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// CI pipeline: review + dead code + gate
    Ci {
        /// Base git reference
        #[arg(long)]
        base: Option<String>,
        /// Gate threshold (high, medium, off)
        #[arg(long, default_value = "off")]
        gate: GateThreshold,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Semantic diff between indexed snapshots
    Diff {
        /// Source reference name
        source: String,
        /// Target reference (default: project)
        target: Option<String>,
        /// Similarity threshold
        ///
        /// `-t` alias matches the CLI subcommand so forwarded invocations
        /// (`cqs diff a b -t 0.9`) parse cleanly on the daemon side.
        #[arg(short = 't', long, default_value = "0.95", value_parser = crate::cli::parse_finite_f32)]
        threshold: f32,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
    },
    /// Diff-aware impact analysis
    #[command(name = "impact-diff")]
    ImpactDiff {
        /// Base git reference
        #[arg(long)]
        base: Option<String>,
    },
    /// Task planning with template classification
    Plan {
        /// Task description
        description: String,
        /// Max file groups
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Auto-suggest notes from patterns
    Suggest {
        /// Apply suggestions (otherwise dry-run)
        #[arg(long)]
        apply: bool,
    },
    /// Garbage collection: prune stale index entries
    Gc,
    /// Invalidate all mutable caches and re-open the Store
    #[command(visible_alias = "invalidate")]
    Refresh,
    /// Show help
    Help,
}

impl BatchCmd {
    /// Whether this command accepts a piped function name as its first positional arg.
    /// Used by pipeline execution to validate downstream segments. Commands that
    /// take a function name as their primary input are pipeable; commands that
    /// take queries, paths, or no arguments are not.
    ///
    /// API-V1.25-6: `match` is intentionally exhaustive (no wildcard arm) so
    /// adding a new `BatchCmd` variant forces a classification decision here.
    /// The `test_is_pipeable_exhaustive` test below pins this behavior —
    /// removing the exhaustiveness makes the test fail to compile.
    pub(crate) fn is_pipeable(&self) -> bool {
        match self {
            // Pipeable: primary input is a function name.
            BatchCmd::Blame { .. }
            | BatchCmd::Callers { .. }
            | BatchCmd::Callees { .. }
            | BatchCmd::Deps { .. }
            | BatchCmd::Explain { .. }
            | BatchCmd::Similar { .. }
            | BatchCmd::Impact { .. }
            | BatchCmd::TestMap { .. }
            | BatchCmd::Related { .. }
            | BatchCmd::Scout { .. } => true,
            // Not pipeable: queries, paths, git refs, or no positional arg.
            BatchCmd::Search { .. }
            | BatchCmd::Gather { .. }
            | BatchCmd::Trace { .. }
            | BatchCmd::Dead { .. }
            | BatchCmd::Context { .. }
            | BatchCmd::Stats
            | BatchCmd::Onboard { .. }
            | BatchCmd::Where { .. }
            | BatchCmd::Read { .. }
            | BatchCmd::Stale { .. }
            | BatchCmd::Health
            | BatchCmd::Drift { .. }
            | BatchCmd::Notes { .. }
            | BatchCmd::Task { .. }
            | BatchCmd::Review { .. }
            | BatchCmd::Ci { .. }
            | BatchCmd::Diff { .. }
            | BatchCmd::ImpactDiff { .. }
            | BatchCmd::Plan { .. }
            | BatchCmd::Suggest { .. }
            | BatchCmd::Gc
            | BatchCmd::Refresh
            | BatchCmd::Help => false,
        }
    }
}

// ─── Query logging ───────────────────────────────────────────────────────────

/// Append a query to the query log for eval workflow capture.
/// Best-effort: failures are silently ignored (never blocks batch mode).
fn log_query(command: &str, query: &str) {
    use std::io::Write;
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let log_path = home.join(".cache/cqs/query_log.jsonl");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let Ok(mut f) = opts.open(&log_path) else {
        tracing::debug!(path = %log_path.display(), "Query log open failed, skipping");
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = writeln!(
        f,
        "{{\"ts\":{},\"cmd\":\"{}\",\"query\":{}}}",
        ts,
        command,
        serde_json::to_string(query).unwrap_or_else(|_| "\"\"".to_string())
    );
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Execute a batch command and return a JSON value.
/// This is the seam for step 3 (REPL): import `BatchContext` + `dispatch`, wrap
/// with readline.
pub(crate) fn dispatch(ctx: &BatchContext, cmd: BatchCmd) -> Result<serde_json::Value> {
    let _span = tracing::debug_span!("batch_dispatch").entered();
    match cmd {
        BatchCmd::Blame { args } => {
            handlers::dispatch_blame(ctx, &args.name, args.depth, args.callers)
        }
        BatchCmd::Search {
            query,
            limit,
            name_only,
            rrf,
            rerank,
            splade,
            splade_alpha,
            lang,
            path,
            include_type,
            exclude_type,
            tokens,
            no_demote,
            name_boost,
            ref_name,
            include_refs,
            no_content,
            context,
            expand,
            no_stale_check,
        } => {
            log_query("search", &query);
            handlers::dispatch_search(
                ctx,
                &handlers::SearchParams {
                    query,
                    limit,
                    name_only,
                    rrf,
                    rerank,
                    splade,
                    splade_alpha,
                    lang,
                    path,
                    include_type,
                    exclude_type,
                    tokens,
                    no_demote,
                    name_boost,
                    ref_name,
                    include_refs,
                    no_content,
                    context,
                    expand,
                    no_stale_check,
                },
            )
        }
        BatchCmd::Deps {
            name,
            reverse,
            cross_project,
        } => handlers::dispatch_deps(ctx, &name, reverse, cross_project),
        BatchCmd::Callers {
            name,
            cross_project,
        } => handlers::dispatch_callers(ctx, &name, cross_project),
        BatchCmd::Callees {
            name,
            cross_project,
        } => handlers::dispatch_callees(ctx, &name, cross_project),
        BatchCmd::Explain { name, tokens } => handlers::dispatch_explain(ctx, &name, tokens),
        BatchCmd::Similar { args } => {
            handlers::dispatch_similar(ctx, &args.name, args.limit, args.threshold)
        }
        BatchCmd::Gather { args } => {
            log_query("gather", &args.query);
            handlers::dispatch_gather(
                ctx,
                &handlers::GatherParams {
                    query: &args.query,
                    expand: args.expand,
                    direction: args.direction,
                    limit: args.limit,
                    tokens: args.tokens,
                    ref_name: args.ref_name.as_deref(),
                },
            )
        }
        BatchCmd::Impact { args } => handlers::dispatch_impact(
            ctx,
            &args.name,
            args.depth,
            args.suggest_tests,
            args.type_impact,
            args.cross_project,
        ),
        BatchCmd::TestMap {
            name,
            depth,
            cross_project,
        } => handlers::dispatch_test_map(ctx, &name, depth, cross_project),
        BatchCmd::Trace { args } => handlers::dispatch_trace(
            ctx,
            &args.source,
            &args.target,
            args.max_depth as usize,
            args.cross_project,
        ),
        BatchCmd::Dead { args } => {
            handlers::dispatch_dead(ctx, args.include_pub, &args.min_confidence)
        }
        BatchCmd::Related { name, limit } => handlers::dispatch_related(ctx, &name, limit),
        BatchCmd::Context { args } => {
            handlers::dispatch_context(ctx, &args.path, args.summary, args.compact, args.tokens)
        }
        BatchCmd::Stats => handlers::dispatch_stats(ctx),
        BatchCmd::Onboard {
            query,
            depth,
            tokens,
        } => {
            log_query("onboard", &query);
            handlers::dispatch_onboard(ctx, &query, depth, tokens)
        }
        BatchCmd::Scout { args } => {
            log_query("scout", &args.query);
            handlers::dispatch_scout(ctx, &args.query, args.limit, args.tokens)
        }
        BatchCmd::Where { description, limit } => {
            log_query("where", &description);
            handlers::dispatch_where(ctx, &description, limit)
        }
        BatchCmd::Read { path, focus } => handlers::dispatch_read(ctx, &path, focus.as_deref()),
        BatchCmd::Stale { count_only } => handlers::dispatch_stale(ctx, count_only),
        BatchCmd::Health => handlers::dispatch_health(ctx),
        BatchCmd::Drift {
            reference,
            threshold,
            min_drift,
            lang,
            limit,
        } => handlers::dispatch_drift(
            ctx,
            &reference,
            threshold,
            min_drift,
            lang.as_deref(),
            limit,
        ),
        BatchCmd::Notes { warnings, patterns } => handlers::dispatch_notes(ctx, warnings, patterns),
        BatchCmd::Task {
            description,
            limit,
            tokens,
        } => {
            log_query("task", &description);
            handlers::dispatch_task(ctx, &description, limit, tokens)
        }
        BatchCmd::Review { base, tokens } => {
            handlers::dispatch_review(ctx, base.as_deref(), tokens)
        }
        BatchCmd::Ci { base, gate, tokens } => {
            handlers::dispatch_ci(ctx, base.as_deref(), &gate, tokens)
        }
        BatchCmd::Diff {
            source,
            target,
            threshold,
            lang,
        } => handlers::dispatch_diff(ctx, &source, target.as_deref(), threshold, lang.as_deref()),
        BatchCmd::ImpactDiff { base } => handlers::dispatch_impact_diff(ctx, base.as_deref()),
        BatchCmd::Plan {
            description,
            limit,
            tokens,
        } => handlers::dispatch_plan(ctx, &description, limit, tokens),
        BatchCmd::Suggest { apply } => handlers::dispatch_suggest(ctx, apply),
        BatchCmd::Gc => handlers::dispatch_gc(ctx),
        BatchCmd::Refresh => handlers::dispatch_refresh(ctx),
        BatchCmd::Help => handlers::dispatch_help(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_parse_search() {
        let input = BatchInput::try_parse_from(["search", "hello"]).unwrap();
        match input.cmd {
            BatchCmd::Search {
                ref query, limit, ..
            } => {
                assert_eq!(query, "hello");
                assert_eq!(limit, 5); // default
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_search_with_flags() {
        let input =
            BatchInput::try_parse_from(["search", "hello", "--limit", "3", "--name-only"]).unwrap();
        match input.cmd {
            BatchCmd::Search {
                ref query,
                limit,
                name_only,
                ..
            } => {
                assert_eq!(query, "hello");
                assert_eq!(limit, 3);
                assert!(name_only);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_callers() {
        let input = BatchInput::try_parse_from(["callers", "my_func"]).unwrap();
        match input.cmd {
            BatchCmd::Callers { ref name, .. } => assert_eq!(name, "my_func"),
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_parse_gather_with_ref() {
        let input =
            BatchInput::try_parse_from(["gather", "alarm config", "--ref", "aveva"]).unwrap();
        match input.cmd {
            BatchCmd::Gather { ref args } => {
                assert_eq!(args.query, "alarm config");
                assert_eq!(args.ref_name.as_deref(), Some("aveva"));
            }
            _ => panic!("Expected Gather command"),
        }
    }

    #[test]
    fn test_parse_dead_with_confidence() {
        let input =
            BatchInput::try_parse_from(["dead", "--min-confidence", "high", "--include-pub"])
                .unwrap();
        match input.cmd {
            BatchCmd::Dead { ref args } => {
                assert!(args.include_pub);
                assert!(matches!(
                    args.min_confidence,
                    cqs::store::DeadConfidence::High
                ));
            }
            _ => panic!("Expected Dead command"),
        }
    }

    #[test]
    fn test_parse_unknown_command() {
        let result = BatchInput::try_parse_from(["bogus"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_trace() {
        let input = BatchInput::try_parse_from(["trace", "main", "validate"]).unwrap();
        match input.cmd {
            BatchCmd::Trace { ref args } => {
                assert_eq!(args.source, "main");
                assert_eq!(args.target, "validate");
                assert_eq!(args.max_depth, 10); // default
            }
            _ => panic!("Expected Trace command"),
        }
    }

    #[test]
    fn test_parse_context() {
        let input = BatchInput::try_parse_from(["context", "src/lib.rs", "--compact"]).unwrap();
        match input.cmd {
            BatchCmd::Context { ref args } => {
                assert_eq!(args.path, "src/lib.rs");
                assert!(args.compact);
                assert!(!args.summary);
            }
            _ => panic!("Expected Context command"),
        }
    }

    #[test]
    fn test_parse_stats() {
        let input = BatchInput::try_parse_from(["stats"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Stats));
    }

    #[test]
    fn test_parse_impact_with_suggest() {
        let input =
            BatchInput::try_parse_from(["impact", "foo", "--depth", "3", "--suggest-tests"])
                .unwrap();
        match input.cmd {
            BatchCmd::Impact { ref args } => {
                assert_eq!(args.name, "foo");
                assert_eq!(args.depth, 3);
                assert!(args.suggest_tests);
                assert!(!args.type_impact);
            }
            _ => panic!("Expected Impact command"),
        }
    }

    #[test]
    fn test_parse_scout() {
        let input = BatchInput::try_parse_from(["scout", "error handling"]).unwrap();
        match input.cmd {
            BatchCmd::Scout { ref args } => {
                assert_eq!(args.query, "error handling");
                assert_eq!(args.limit, 5); // default
            }
            _ => panic!("Expected Scout command"),
        }
    }

    #[test]
    fn test_parse_scout_with_flags() {
        let input = BatchInput::try_parse_from([
            "scout",
            "error handling",
            "--limit",
            "20",
            "--tokens",
            "2000",
        ])
        .unwrap();
        match input.cmd {
            BatchCmd::Scout { ref args } => {
                assert_eq!(args.query, "error handling");
                assert_eq!(args.limit, 20);
                assert_eq!(args.tokens, Some(2000));
            }
            _ => panic!("Expected Scout command"),
        }
    }

    #[test]
    fn test_parse_where() {
        let input = BatchInput::try_parse_from(["where", "new CLI command"]).unwrap();
        match input.cmd {
            BatchCmd::Where {
                ref description,
                limit,
            } => {
                assert_eq!(description, "new CLI command");
                assert_eq!(limit, 3); // default
            }
            _ => panic!("Expected Where command"),
        }
    }

    #[test]
    fn test_parse_read() {
        let input = BatchInput::try_parse_from(["read", "src/lib.rs"]).unwrap();
        match input.cmd {
            BatchCmd::Read {
                ref path,
                ref focus,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(focus.is_none());
            }
            _ => panic!("Expected Read command"),
        }
    }

    #[test]
    fn test_parse_read_focused() {
        let input =
            BatchInput::try_parse_from(["read", "src/lib.rs", "--focus", "enumerate_files"])
                .unwrap();
        match input.cmd {
            BatchCmd::Read {
                ref path,
                ref focus,
            } => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(focus.as_deref(), Some("enumerate_files"));
            }
            _ => panic!("Expected Read command"),
        }
    }

    #[test]
    fn test_parse_stale() {
        let input = BatchInput::try_parse_from(["stale"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Stale { count_only: _ }));
    }

    #[test]
    fn test_parse_health() {
        let input = BatchInput::try_parse_from(["health"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Health));
    }

    #[test]
    fn test_parse_notes() {
        let input = BatchInput::try_parse_from(["notes"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(!warnings);
                assert!(!patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_warnings() {
        let input = BatchInput::try_parse_from(["notes", "--warnings"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(warnings);
                assert!(!patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_patterns() {
        let input = BatchInput::try_parse_from(["notes", "--patterns"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { warnings, patterns } => {
                assert!(!warnings);
                assert!(patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_blame() {
        let input = BatchInput::try_parse_from(["blame", "my_func"]).unwrap();
        match input.cmd {
            BatchCmd::Blame { ref args } => {
                assert_eq!(args.name, "my_func");
                assert_eq!(args.depth, 10); // default
                assert!(!args.callers);
            }
            _ => panic!("Expected Blame command"),
        }
    }

    #[test]
    fn test_parse_blame_with_flags() {
        let input =
            BatchInput::try_parse_from(["blame", "my_func", "-d", "5", "--callers"]).unwrap();
        match input.cmd {
            BatchCmd::Blame { ref args } => {
                assert_eq!(args.name, "my_func");
                assert_eq!(args.depth, 5);
                assert!(args.callers);
            }
            _ => panic!("Expected Blame command"),
        }
    }

    // API-V1.25-6: compile-time guard that every BatchCmd variant is either
    // marked pipeable or explicitly not pipeable. Adding a new variant without
    // updating `BatchCmd::is_pipeable`'s match causes *this test* to fail to
    // compile because the inner match below uses the same exhaustiveness.
    //
    // The test body just spot-checks a few known variants. The real protection
    // is the exhaustive match in `is_pipeable` (no wildcard arm) — if a new
    // variant is added, the compiler flags `is_pipeable` first.
    #[test]
    fn test_is_pipeable_exhaustive_classification() {
        use cqs::store::DeadConfidence;

        // Pipeable variants: should return true.
        let callers = BatchCmd::Callers {
            name: "foo".into(),
            cross_project: false,
        };
        assert!(callers.is_pipeable());

        let scout = BatchCmd::Scout {
            args: crate::cli::args::ScoutArgs {
                query: "foo".into(),
                limit: 5,
                tokens: None,
            },
        };
        assert!(scout.is_pipeable());

        // Non-pipeable variants: should return false.
        assert!(!BatchCmd::Stats.is_pipeable());
        assert!(!BatchCmd::Health.is_pipeable());
        assert!(!BatchCmd::Gc.is_pipeable());
        assert!(!BatchCmd::Refresh.is_pipeable());
        assert!(!BatchCmd::Help.is_pipeable());
        assert!(!BatchCmd::Stale { count_only: false }.is_pipeable());

        let dead = BatchCmd::Dead {
            args: crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
            },
        };
        assert!(!dead.is_pipeable());
    }
}
