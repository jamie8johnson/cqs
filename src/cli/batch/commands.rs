//! Batch command parsing and dispatch routing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::BatchContext;
use crate::cli::{parse_nonzero_usize, DeadConfidenceLevel};

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
        /// Pure semantic similarity, disable RRF hybrid
        #[arg(long)]
        semantic_only: bool,
        /// Re-rank results with cross-encoder
        #[arg(long)]
        rerank: bool,
        /// Filter by language
        #[arg(short = 'l', long)]
        lang: Option<String>,
        /// Filter by path pattern (glob)
        #[arg(short = 'p', long)]
        path: Option<String>,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Type dependencies: who uses a type, or what types a function uses
    Deps {
        /// Type name or function name
        name: String,
        /// Show types used by function (instead of type users)
        #[arg(long)]
        reverse: bool,
    },
    /// Find callers of a function
    Callers {
        /// Function name
        name: String,
    },
    /// Find callees of a function
    Callees {
        /// Function name
        name: String,
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
        /// Function name or file:function
        target: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "5")]
        limit: usize,
        /// Min similarity threshold
        #[arg(short = 't', long, default_value = "0.3")]
        threshold: f32,
    },
    /// Smart context assembly
    Gather {
        /// Search query
        query: String,
        /// Call graph expansion depth (0-5)
        #[arg(long, default_value = "1")]
        expand: usize,
        /// Direction: both, callers, callees
        #[arg(long, default_value = "both")]
        direction: String,
        /// Max chunks
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
        /// Cross-index gather from reference
        #[arg(long = "ref")]
        ref_name: Option<String>,
    },
    /// Impact analysis
    Impact {
        /// Function name or file:function
        name: String,
        /// Caller depth (1=direct, 2+=transitive)
        #[arg(long, default_value = "1")]
        depth: usize,
        /// Suggest tests for untested callers
        #[arg(long)]
        suggest_tests: bool,
        /// Include type-impacted functions
        #[arg(long)]
        include_types: bool,
    },
    /// Map function to tests
    #[command(name = "test-map")]
    TestMap {
        /// Function name or file:function
        name: String,
        /// Max call chain depth
        #[arg(long, default_value = "5")]
        depth: usize,
    },
    /// Trace call path between two functions
    Trace {
        /// Source function
        source: String,
        /// Target function
        target: String,
        /// Max search depth
        #[arg(long, default_value = "10", value_parser = clap::value_parser!(u16).range(1..=50))]
        max_depth: u16,
    },
    /// Find dead code
    Dead {
        /// Include public API functions
        #[arg(long)]
        include_pub: bool,
        /// Minimum confidence level
        #[arg(long, default_value = "low")]
        min_confidence: DeadConfidenceLevel,
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
        /// File path relative to project root
        path: String,
        /// Return summary counts
        #[arg(long)]
        summary: bool,
        /// Signatures-only TOC
        #[arg(long)]
        compact: bool,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
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
        /// Task description
        query: String,
        /// Max results
        #[arg(short = 'n', long, default_value = "10")]
        limit: usize,
        /// Maximum token budget
        #[arg(long, value_parser = parse_nonzero_usize)]
        tokens: Option<usize>,
    },
    /// Suggest where to add new code
    Where {
        /// Description of what to add
        description: String,
        /// Max suggestions
        #[arg(short = 'n', long, default_value = "5")]
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
    Stale,
    /// Codebase quality snapshot
    Health,
    /// Semantic drift detection between reference and project
    Drift {
        /// Reference name to compare against
        reference: String,
        /// Similarity threshold (default: 0.95)
        #[arg(long, default_value = "0.95")]
        threshold: f32,
        /// Minimum drift to show (default: 0.0)
        #[arg(long, default_value = "0.0")]
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
    /// Show help
    Help,
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

/// Execute a batch command and return a JSON value.
///
/// This is the seam for step 3 (REPL): import `BatchContext` + `dispatch`, wrap
/// with readline.
pub(crate) fn dispatch(ctx: &BatchContext, cmd: BatchCmd) -> Result<serde_json::Value> {
    match cmd {
        BatchCmd::Search {
            query,
            limit,
            name_only,
            semantic_only,
            rerank,
            lang,
            path,
            tokens,
        } => handlers::dispatch_search(
            ctx,
            &query,
            limit,
            name_only,
            semantic_only,
            rerank,
            lang,
            path,
            tokens,
        ),
        BatchCmd::Deps { name, reverse } => handlers::dispatch_deps(ctx, &name, reverse),
        BatchCmd::Callers { name } => handlers::dispatch_callers(ctx, &name),
        BatchCmd::Callees { name } => handlers::dispatch_callees(ctx, &name),
        BatchCmd::Explain { name, tokens } => handlers::dispatch_explain(ctx, &name, tokens),
        BatchCmd::Similar {
            target,
            limit,
            threshold,
        } => handlers::dispatch_similar(ctx, &target, limit, threshold),
        BatchCmd::Gather {
            query,
            expand,
            direction,
            limit,
            tokens,
            ref_name,
        } => handlers::dispatch_gather(
            ctx,
            &query,
            expand,
            &direction,
            limit,
            tokens,
            ref_name.as_deref(),
        ),
        BatchCmd::Impact {
            name,
            depth,
            suggest_tests,
            include_types,
        } => handlers::dispatch_impact(ctx, &name, depth, suggest_tests, include_types),
        BatchCmd::TestMap { name, depth } => handlers::dispatch_test_map(ctx, &name, depth),
        BatchCmd::Trace {
            source,
            target,
            max_depth,
        } => handlers::dispatch_trace(ctx, &source, &target, max_depth as usize),
        BatchCmd::Dead {
            include_pub,
            min_confidence,
        } => handlers::dispatch_dead(ctx, include_pub, &min_confidence),
        BatchCmd::Related { name, limit } => handlers::dispatch_related(ctx, &name, limit),
        BatchCmd::Context {
            path,
            summary,
            compact,
            tokens,
        } => handlers::dispatch_context(ctx, &path, summary, compact, tokens),
        BatchCmd::Stats => handlers::dispatch_stats(ctx),
        BatchCmd::Onboard {
            query,
            depth,
            tokens,
        } => handlers::dispatch_onboard(ctx, &query, depth, tokens),
        BatchCmd::Scout {
            query,
            limit,
            tokens,
        } => handlers::dispatch_scout(ctx, &query, limit, tokens),
        BatchCmd::Where { description, limit } => {
            handlers::dispatch_where(ctx, &description, limit)
        }
        BatchCmd::Read { path, focus } => handlers::dispatch_read(ctx, &path, focus.as_deref()),
        BatchCmd::Stale => handlers::dispatch_stale(ctx),
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
        } => handlers::dispatch_task(ctx, &description, limit, tokens),
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
            BatchCmd::Callers { ref name } => assert_eq!(name, "my_func"),
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_parse_gather_with_ref() {
        let input =
            BatchInput::try_parse_from(["gather", "alarm config", "--ref", "aveva"]).unwrap();
        match input.cmd {
            BatchCmd::Gather {
                ref query,
                ref ref_name,
                ..
            } => {
                assert_eq!(query, "alarm config");
                assert_eq!(ref_name.as_deref(), Some("aveva"));
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
            BatchCmd::Dead {
                include_pub,
                ref min_confidence,
            } => {
                assert!(include_pub);
                assert!(matches!(min_confidence, DeadConfidenceLevel::High));
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
            BatchCmd::Trace {
                ref source,
                ref target,
                max_depth,
            } => {
                assert_eq!(source, "main");
                assert_eq!(target, "validate");
                assert_eq!(max_depth, 10); // default
            }
            _ => panic!("Expected Trace command"),
        }
    }

    #[test]
    fn test_parse_context() {
        let input = BatchInput::try_parse_from(["context", "src/lib.rs", "--compact"]).unwrap();
        match input.cmd {
            BatchCmd::Context {
                ref path,
                compact,
                summary,
                ..
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(compact);
                assert!(!summary);
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
            BatchCmd::Impact {
                ref name,
                depth,
                suggest_tests,
                include_types,
            } => {
                assert_eq!(name, "foo");
                assert_eq!(depth, 3);
                assert!(suggest_tests);
                assert!(!include_types);
            }
            _ => panic!("Expected Impact command"),
        }
    }

    #[test]
    fn test_parse_scout() {
        let input = BatchInput::try_parse_from(["scout", "error handling"]).unwrap();
        match input.cmd {
            BatchCmd::Scout {
                ref query, limit, ..
            } => {
                assert_eq!(query, "error handling");
                assert_eq!(limit, 10); // default
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
            BatchCmd::Scout {
                ref query,
                limit,
                tokens,
            } => {
                assert_eq!(query, "error handling");
                assert_eq!(limit, 20);
                assert_eq!(tokens, Some(2000));
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
                assert_eq!(limit, 5); // default
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
        assert!(matches!(input.cmd, BatchCmd::Stale));
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
}
