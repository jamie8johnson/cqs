//! Batch command parsing and dispatch routing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::BatchContext;

use crate::cli::args::{
    BlameArgs, CallersArgs, CiArgs, ContextArgs, DeadArgs, DepsArgs, DiffArgs, DriftArgs,
    ExplainArgs, GatherArgs, ImpactArgs, ImpactDiffArgs, NotesListArgs, OnboardArgs, PlanArgs,
    ReadArgs, RelatedArgs, ReviewArgs, ScoutArgs, SearchArgs, SimilarArgs, StaleArgs, SuggestArgs,
    TaskArgs, TestMapArgs, TraceArgs, WhereArgs,
};

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
    ///
    /// #947: embeds shared `SearchArgs` so both CLI and batch share one
    /// source of truth for search flags. Previously 21 fields were
    /// inline-duplicated here and drift was the default outcome.
    Search {
        #[command(flatten)]
        args: SearchArgs,
    },
    /// Semantic git blame: who changed a function, when, and why
    Blame {
        #[command(flatten)]
        args: BlameArgs,
    },
    /// Type dependencies: who uses a type, or what types a function uses
    Deps {
        #[command(flatten)]
        args: DepsArgs,
    },
    /// Find callers of a function
    Callers {
        #[command(flatten)]
        args: CallersArgs,
    },
    /// Find callees of a function
    Callees {
        #[command(flatten)]
        args: CallersArgs,
    },
    /// Function card: signature, callers, callees, similar
    Explain {
        #[command(flatten)]
        args: ExplainArgs,
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
        #[command(flatten)]
        args: TestMapArgs,
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
        #[command(flatten)]
        args: RelatedArgs,
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
        #[command(flatten)]
        args: OnboardArgs,
    },
    /// Pre-investigation dashboard
    Scout {
        #[command(flatten)]
        args: ScoutArgs,
    },
    /// Suggest where to add new code
    Where {
        #[command(flatten)]
        args: WhereArgs,
    },
    /// Read file with note injection
    Read {
        #[command(flatten)]
        args: ReadArgs,
    },
    /// Check index freshness
    Stale {
        #[command(flatten)]
        args: StaleArgs,
    },
    /// Codebase quality snapshot
    Health,
    /// Semantic drift detection between reference and project
    Drift {
        #[command(flatten)]
        args: DriftArgs,
    },
    /// List notes
    Notes {
        #[command(flatten)]
        args: NotesListArgs,
    },
    /// One-shot implementation context (terminal — no pipeline chaining)
    Task {
        #[command(flatten)]
        args: TaskArgs,
    },
    /// Comprehensive diff review
    Review {
        #[command(flatten)]
        args: ReviewArgs,
    },
    /// CI pipeline: review + dead code + gate
    Ci {
        #[command(flatten)]
        args: CiArgs,
    },
    /// Semantic diff between indexed snapshots
    Diff {
        #[command(flatten)]
        args: DiffArgs,
    },
    /// Diff-aware impact analysis
    #[command(name = "impact-diff")]
    ImpactDiff {
        #[command(flatten)]
        args: ImpactDiffArgs,
    },
    /// Task planning with template classification
    Plan {
        #[command(flatten)]
        args: PlanArgs,
    },
    /// Auto-suggest notes from patterns
    Suggest {
        #[command(flatten)]
        args: SuggestArgs,
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
        BatchCmd::Search { args } => {
            log_query("search", &args.query);
            handlers::dispatch_search(ctx, &args)
        }
        BatchCmd::Deps { args } => {
            handlers::dispatch_deps(ctx, &args.name, args.reverse, args.cross_project)
        }
        BatchCmd::Callers { args } => {
            handlers::dispatch_callers(ctx, &args.name, args.cross_project)
        }
        BatchCmd::Callees { args } => {
            handlers::dispatch_callees(ctx, &args.name, args.cross_project)
        }
        BatchCmd::Explain { args } => handlers::dispatch_explain(ctx, &args.name, args.tokens),
        BatchCmd::Similar { args } => {
            handlers::dispatch_similar(ctx, &args.name, args.limit, args.threshold)
        }
        BatchCmd::Gather { args } => {
            log_query("gather", &args.query);
            handlers::dispatch_gather(ctx, &args)
        }
        BatchCmd::Impact { args } => handlers::dispatch_impact(
            ctx,
            &args.name,
            args.depth,
            args.suggest_tests,
            args.type_impact,
            args.cross_project,
        ),
        BatchCmd::TestMap { args } => {
            handlers::dispatch_test_map(ctx, &args.name, args.depth, args.cross_project)
        }
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
        BatchCmd::Related { args } => handlers::dispatch_related(ctx, &args.name, args.limit),
        BatchCmd::Context { args } => {
            handlers::dispatch_context(ctx, &args.path, args.summary, args.compact, args.tokens)
        }
        BatchCmd::Stats => handlers::dispatch_stats(ctx),
        BatchCmd::Onboard { args } => {
            log_query("onboard", &args.query);
            handlers::dispatch_onboard(ctx, &args.query, args.depth, args.tokens)
        }
        BatchCmd::Scout { args } => {
            log_query("scout", &args.query);
            handlers::dispatch_scout(ctx, &args.query, args.limit, args.tokens)
        }
        BatchCmd::Where { args } => {
            log_query("where", &args.description);
            handlers::dispatch_where(ctx, &args.description, args.limit)
        }
        BatchCmd::Read { args } => handlers::dispatch_read(ctx, &args.path, args.focus.as_deref()),
        BatchCmd::Stale { args } => handlers::dispatch_stale(ctx, args.count_only),
        BatchCmd::Health => handlers::dispatch_health(ctx),
        BatchCmd::Drift { args } => handlers::dispatch_drift(
            ctx,
            &args.reference,
            args.threshold,
            args.min_drift,
            args.lang.as_deref(),
            args.limit,
        ),
        BatchCmd::Notes { args } => handlers::dispatch_notes(ctx, args.warnings, args.patterns),
        BatchCmd::Task { args } => {
            log_query("task", &args.description);
            handlers::dispatch_task(ctx, &args.description, args.limit, args.tokens)
        }
        BatchCmd::Review { args } => {
            handlers::dispatch_review(ctx, args.base.as_deref(), args.tokens)
        }
        BatchCmd::Ci { args } => {
            handlers::dispatch_ci(ctx, args.base.as_deref(), &args.gate, args.tokens)
        }
        BatchCmd::Diff { args } => handlers::dispatch_diff(
            ctx,
            &args.source,
            args.target.as_deref(),
            args.threshold,
            args.lang.as_deref(),
        ),
        BatchCmd::ImpactDiff { args } => handlers::dispatch_impact_diff(ctx, args.base.as_deref()),
        BatchCmd::Plan { args } => {
            handlers::dispatch_plan(ctx, &args.description, args.limit, args.tokens)
        }
        BatchCmd::Suggest { args } => handlers::dispatch_suggest(ctx, args.apply),
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
            BatchCmd::Search { ref args } => {
                assert_eq!(args.query, "hello");
                assert_eq!(args.limit, 5); // default
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_search_with_flags() {
        let input =
            BatchInput::try_parse_from(["search", "hello", "--limit", "3", "--name-only"]).unwrap();
        match input.cmd {
            BatchCmd::Search { ref args } => {
                assert_eq!(args.query, "hello");
                assert_eq!(args.limit, 3);
                assert!(args.name_only);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_parse_callers() {
        let input = BatchInput::try_parse_from(["callers", "my_func"]).unwrap();
        match input.cmd {
            BatchCmd::Callers { ref args } => assert_eq!(args.name, "my_func"),
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
            BatchCmd::Where { ref args } => {
                assert_eq!(args.description, "new CLI command");
                assert_eq!(args.limit, 3); // default
            }
            _ => panic!("Expected Where command"),
        }
    }

    #[test]
    fn test_parse_read() {
        let input = BatchInput::try_parse_from(["read", "src/lib.rs"]).unwrap();
        match input.cmd {
            BatchCmd::Read { ref args } => {
                assert_eq!(args.path, "src/lib.rs");
                assert!(args.focus.is_none());
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
            BatchCmd::Read { ref args } => {
                assert_eq!(args.path, "src/lib.rs");
                assert_eq!(args.focus.as_deref(), Some("enumerate_files"));
            }
            _ => panic!("Expected Read command"),
        }
    }

    #[test]
    fn test_parse_stale() {
        let input = BatchInput::try_parse_from(["stale"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Stale { .. }));
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
            BatchCmd::Notes { ref args } => {
                assert!(!args.warnings);
                assert!(!args.patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_warnings() {
        let input = BatchInput::try_parse_from(["notes", "--warnings"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { ref args } => {
                assert!(args.warnings);
                assert!(!args.patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_patterns() {
        let input = BatchInput::try_parse_from(["notes", "--patterns"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { ref args } => {
                assert!(!args.warnings);
                assert!(args.patterns);
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
            args: crate::cli::args::CallersArgs {
                name: "foo".into(),
                cross_project: false,
            },
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
        assert!(!BatchCmd::Stale {
            args: crate::cli::args::StaleArgs { count_only: false },
        }
        .is_pipeable());

        let dead = BatchCmd::Dead {
            args: crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
            },
        };
        assert!(!dead.is_pipeable());
    }
}
