//! Batch command parsing and dispatch routing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::BatchView;

use crate::cli::args::{
    BlameArgs, CallersArgs, CiArgs, ContextArgs, DeadArgs, DepsArgs, DiffArgs, DriftArgs,
    ExplainArgs, GatherArgs, ImpactArgs, ImpactDiffArgs, NotesListArgs, OnboardArgs, PlanArgs,
    ReadArgs, RelatedArgs, ReviewArgs, ScoutArgs, SearchArgs, SimilarArgs, StaleArgs, SuggestArgs,
    TaskArgs, TestMapArgs, TraceArgs, WhereArgs,
};
use crate::cli::definitions::{OutputArgs, TextJsonArgs};

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

/// BatchCmd variants flatten an output struct (`TextJsonArgs` for text/json
/// commands, `OutputArgs` for the impact/trace pair that supports `--format
/// mermaid`) so callers can pass `--json` for parity with the CLI even though
/// batch *always* serializes to JSON. Task #8: previously
/// `echo 'callers Foo --json' | cqs batch` errored with "unexpected argument
/// --json"; now the flag is accepted and silently a no-op (the handler
/// ignores `output.json`/`output.format` because the batch transport itself
/// frames the response as JSONL on the daemon socket and as JSONL on stdout).
///
/// We *intentionally* do not delete the `output` field — clap requires the
/// flatten target to back the `--json` flag, and removing the field would
/// re-introduce the "unexpected argument" parse error for users who want
/// CLI-batch flag parity. The unused-field warning is silenced via the
/// `#[allow]` attribute on each variant rather than a global allow so the
/// dead-code signal stays meaningful elsewhere in the crate.
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
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Semantic git blame: who changed a function, when, and why
    Blame {
        #[command(flatten)]
        args: BlameArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Type dependencies: who uses a type, or what types a function uses
    Deps {
        #[command(flatten)]
        args: DepsArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Find callers of a function
    Callers {
        #[command(flatten)]
        args: CallersArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Find callees of a function
    Callees {
        #[command(flatten)]
        args: CallersArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Function card: signature, callers, callees, similar
    Explain {
        #[command(flatten)]
        args: ExplainArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Find similar code
    Similar {
        #[command(flatten)]
        args: SimilarArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Smart context assembly
    Gather {
        #[command(flatten)]
        args: GatherArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Impact analysis
    Impact {
        #[command(flatten)]
        args: ImpactArgs,
        /// Task #8: `OutputArgs` here matches the CLI side (text/json/mermaid).
        /// Mermaid is silently downgraded to JSON in batch because the daemon
        /// socket framer assumes JSONL. Adding a non-JSON wire format would
        /// require re-shaping `dispatch_line` — out of scope.
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json/--format accepted for CLI parity")]
        output: OutputArgs,
    },
    /// Map function to tests
    #[command(name = "test-map")]
    TestMap {
        #[command(flatten)]
        args: TestMapArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Trace call path between two functions
    Trace {
        #[command(flatten)]
        args: TraceArgs,
        /// Task #8: see the `Impact` variant — `OutputArgs` mirrors the CLI's
        /// `--format mermaid` flag for parity even though batch downgrades it.
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json/--format accepted for CLI parity")]
        output: OutputArgs,
    },
    /// Find dead code
    Dead {
        #[command(flatten)]
        args: DeadArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Find related functions by co-occurrence
    Related {
        #[command(flatten)]
        args: RelatedArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Module-level context for a file
    Context {
        #[command(flatten)]
        args: ContextArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Index statistics
    Stats {
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Guided codebase tour
    Onboard {
        #[command(flatten)]
        args: OnboardArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Pre-investigation dashboard
    Scout {
        #[command(flatten)]
        args: ScoutArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Suggest where to add new code
    Where {
        #[command(flatten)]
        args: WhereArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Read file with note injection
    Read {
        #[command(flatten)]
        args: ReadArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Check index freshness
    Stale {
        #[command(flatten)]
        args: StaleArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Codebase quality snapshot
    Health {
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Semantic drift detection between reference and project
    Drift {
        #[command(flatten)]
        args: DriftArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// List notes
    Notes {
        #[command(flatten)]
        args: NotesListArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// One-shot implementation context (terminal — no pipeline chaining)
    Task {
        #[command(flatten)]
        args: TaskArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Comprehensive diff review
    Review {
        #[command(flatten)]
        args: ReviewArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// CI pipeline: review + dead code + gate
    Ci {
        #[command(flatten)]
        args: CiArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Semantic diff between indexed snapshots
    Diff {
        #[command(flatten)]
        args: DiffArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Diff-aware impact analysis
    #[command(name = "impact-diff")]
    ImpactDiff {
        #[command(flatten)]
        args: ImpactDiffArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Task planning with template classification
    Plan {
        #[command(flatten)]
        args: PlanArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Auto-suggest notes from patterns
    Suggest {
        #[command(flatten)]
        args: SuggestArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Garbage collection: prune stale index entries
    Gc {
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// Invalidate all mutable caches and re-open the Store
    #[command(visible_alias = "invalidate")]
    Refresh,
    /// Daemon healthcheck — returns model, uptime, query/error counts
    ///
    /// Task B2: zero-arg command. The CLI `cqs ping` builds the request
    /// from a fixed string, so we don't need any flags here. The handler
    /// returns the JSON payload of `cqs::daemon_translate::PingResponse`.
    Ping,
    /// Watch-mode freshness snapshot — returns the latest `WatchSnapshot`.
    ///
    /// #1182: zero-arg command served by `cqs watch --serve`. The CLI
    /// `cqs status --watch-fresh [--json]` is the user-facing surface;
    /// the daemon returns the JSON payload of `cqs::watch_status::WatchSnapshot`.
    /// `cqs batch` (no watch loop) returns the default `unknown` snapshot.
    Status,
    /// Request an out-of-band reconciliation pass. (#1182 — Layer 1.)
    ///
    /// Used by the git-hook scripts (`cqs hook fire post-checkout ...`)
    /// to tell the watch loop the working tree just shifted. Flips the
    /// shared `SharedReconcileSignal` AtomicBool to `true`; the watch
    /// loop observes it on its next 100 ms tick and runs an immediate
    /// `run_daemon_reconcile` pass (bypassing the periodic-tick idle
    /// gating, since this call is itself the user signal).
    ///
    /// Optional positional fields are advisory only — they ride along
    /// for tracing/logging and don't change the reconcile algorithm
    /// (which always walks the full tree). Hooks pass:
    ///   `cqs hook fire post-checkout <prev_HEAD> <new_HEAD> <branch_flag>`
    /// `--hook` carries the hook name; everything else lands in `--arg`.
    Reconcile {
        /// Name of the hook that fired this reconcile (e.g. `post-checkout`).
        /// Logged for operator diagnostics; not used for the walk itself.
        #[arg(long)]
        hook: Option<String>,
        /// Free-form positional payload from the hook (e.g. previous and
        /// current commit SHAs). Captured for tracing only.
        #[arg(long = "arg", value_name = "ARG")]
        args: Vec<String>,
    },
    /// Block until the watch loop transitions to Fresh, or `wait_secs`
    /// elapses. (#1228 — RM-2: server-side wait, no client-side polling.)
    ///
    /// One round-trip total — the daemon parks the request on a
    /// `FreshNotifier` shared with the watch loop. When `publish_watch_snapshot`
    /// observes a `false → true` transition it issues a `notify_all`,
    /// the parked handler wakes, and replies with the latest snapshot.
    /// On deadline the handler replies with the still-stale snapshot.
    ///
    /// Replaces the prior 250 ms-poll loop in `wait_for_fresh` (4-5k
    /// connect/parse round-trips per 60 s wait at the default budget).
    /// `cqs status --watch-fresh --wait` and `cqs eval --require-fresh`
    /// route through this when talking to a daemon. Outside `cqs watch
    /// --serve` the notifier never flips and the call hits the deadline
    /// naturally.
    #[command(name = "wait-fresh")]
    WaitFresh {
        /// Maximum seconds to block before returning the current
        /// (still-stale) snapshot. Capped server-side at 86_400 (24 h)
        /// for parity with the client-side cap in `wait_for_fresh`.
        #[arg(long, default_value_t = 60)]
        wait_secs: u64,
    },
    /// Show help
    Help,
    /// #1127 (test-only): sleep `--ms` milliseconds before returning. Used by
    /// the daemon-parallelism regression tests to force two concurrent
    /// handlers to overlap on wall-clock; the variant is `#[cfg(test)]`-gated
    /// so it never reaches a release binary.
    #[cfg(test)]
    #[command(name = "test-sleep")]
    TestSleep {
        #[arg(long, default_value_t = 200)]
        ms: u64,
    },
}

/// Per-variant pipeability table — single source of truth for `BatchCmd::is_pipeable`.
///
/// Issue #1137 (audit finding EX-V1.30-1): the pipeability classification used
/// to live in a hand-maintained match arm, decoupled from the variant declaration.
/// Adding a new `BatchCmd` variant required a coordinated edit in two places.
/// This macro folds the classification into the variant list itself: each row is
/// `(variant_name, is_pipeable)`. The `gen_is_pipeable_impl` emitter expands the
/// table into an exhaustive match — adding a new variant without a row is a
/// compile-time error (the match is non-exhaustive).
///
/// Struct variants and unit variants are listed in two named blocks so the
/// macro emitter can build the right pattern shape (`Variant { .. }` vs `Variant`)
/// without ambiguity.
///
/// Pipeable: primary input is a function name (so a previous segment's output
/// can be piped in). Not pipeable: queries, paths, git refs, or no positional arg.
macro_rules! for_each_batch_cmd_pipeability {
    ($emit:ident) => {
        $emit! {
            struct_variants: {
                // Pipeable — primary input is a function name.
                (Blame, true)
                (Callers, true)
                (Callees, true)
                (Deps, true)
                (Explain, true)
                (Similar, true)
                (Impact, true)
                (TestMap, true)
                (Related, true)
                (Scout, true)

                // Not pipeable — queries, paths, git refs, or no positional arg.
                (Search, false)
                (Gather, false)
                (Trace, false)
                (Dead, false)
                (Context, false)
                (Stats, false)
                (Onboard, false)
                (Where, false)
                (Read, false)
                (Stale, false)
                (Health, false)
                (Drift, false)
                (Notes, false)
                (Task, false)
                (Review, false)
                (Ci, false)
                (Diff, false)
                (ImpactDiff, false)
                (Plan, false)
                (Suggest, false)
                (Gc, false)
                (Reconcile, false)
                // #1228 (RM-2): not pipeable — wait_secs is the only
                // arg, no positional function name to receive a pipe.
                (WaitFresh, false)
            }
            unit_variants: {
                (Refresh, false)
                (Ping, false)
                (Status, false)
                (Help, false)
            }
        }
    };
}

/// Emits `BatchCmd::is_pipeable` from the table above.
///
/// API-V1.25-6: the generated `match` is intentionally exhaustive (no wildcard
/// arm), so a new `BatchCmd` variant without a pipeability row fails to compile.
/// `test_is_pipeable_exhaustive` below double-pins this behaviour.
macro_rules! gen_is_pipeable_impl {
    (
        struct_variants: { $(($svar:ident, $sp:expr))* }
        unit_variants:   { $(($uvar:ident, $up:expr))* }
    ) => {
        impl BatchCmd {
            /// Whether this command accepts a piped function name as its first positional arg.
            /// Used by pipeline execution to validate downstream segments.
            pub(crate) fn is_pipeable(&self) -> bool {
                match self {
                    $(BatchCmd::$svar { .. } => $sp,)*
                    $(BatchCmd::$uvar => $up,)*
                    #[cfg(test)]
                    BatchCmd::TestSleep { .. } => false,
                }
            }
        }
    };
}

for_each_batch_cmd_pipeability!(gen_is_pipeable_impl);

// ─── Query logging ───────────────────────────────────────────────────────────

/// Per-variant table for the eval-capture query log.
///
/// EX-V1.30.1-3 (P3-EX-1): the `log_query("search", &args.query)` calls
/// used to live at six hand-sprinkled sites in `dispatch`. Each site had
/// to remember the right command-name string and the right field name
/// (`args.query` vs `args.description` vs ...). Centralised here so
/// adding a new logged variant is one row in this table instead of a
/// new sprinkled call inside `dispatch`.
///
/// The `gen_log_query_dispatch` emitter expands the table into one
/// `log_query_for(cmd: &BatchCmd)` function that pattern-matches the
/// variant and pulls the field by name. Variants that aren't listed
/// here fall through (no log line emitted) — most batch commands take
/// a function name or a path, not a query, and those don't go in the
/// eval-replay log.
macro_rules! for_each_logged_batch_cmd {
    ($emit:ident) => {
        $emit! {
            // (BatchCmd variant, log-name, field accessor on the variant's `args`)
            (Search,  "search",  query)
            (Gather,  "gather",  query)
            (Onboard, "onboard", query)
            (Scout,   "scout",   query)
            (Where,   "where",   description)
            (Task,    "task",    description)
        }
    };
}

/// Emit `fn log_query_for(cmd: &BatchCmd)` from the table above.
///
/// Each row produces a `BatchCmd::$variant { args, .. } => log_query(...)`
/// arm; a final `_ => {}` arm covers the un-logged variants. Adding a
/// new logged variant is one new row in `for_each_logged_batch_cmd!`.
macro_rules! gen_log_query_dispatch {
    ( $( ($var:ident, $log_name:literal, $field:ident) )* ) => {
        fn log_query_for(cmd: &BatchCmd) {
            match cmd {
                $(
                    BatchCmd::$var { args, .. } => log_query($log_name, &args.$field),
                )*
                _ => {}
            }
        }
    };
}

for_each_logged_batch_cmd!(gen_log_query_dispatch);

/// Append a query to the query log for eval workflow capture.
/// Best-effort: failures are silently ignored (never blocks batch mode).
fn log_query(command: &str, query: &str) {
    use std::io::Write;
    // P3.32: prefer the platform's native cache dir; fall back to `~/.cache`
    // for legacy behavior. Skip silently if neither is resolvable.
    let Some(cache_root) = dirs::cache_dir().or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
    else {
        return;
    };
    let log_path = cache_root.join("cqs").join("query_log.jsonl");
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
/// This is the seam for step 3 (REPL): import `BatchView` + `dispatch`, wrap
/// with readline.
///
/// #1127: takes a [`BatchView`] (snapshot of BatchContext caches built under a
/// brief critical section) instead of `&BatchContext`. Handlers operate on the
/// view's `Arc`-cloned data; the daemon's outer mutex is released before
/// dispatch, so concurrent reads no longer serialize through one lock.
pub(crate) fn dispatch(ctx: &BatchView, cmd: BatchCmd) -> Result<serde_json::Value> {
    let _span = tracing::debug_span!("batch_dispatch").entered();
    // EX-V1.30.1-3 (P3-EX-1): single table-driven query-log call replaces
    // six hand-sprinkled `log_query(...)` invocations inside the match arms.
    // The `for_each_logged_batch_cmd!` table at the top of this module
    // owns the variant → log-name + field-accessor mapping.
    log_query_for(&cmd);
    // Task #8: every variant now also carries `output` (TextJsonArgs or
    // OutputArgs) for CLI flag parity, but batch always emits JSON via the
    // socket framer / stdout JSONL — the output field is intentionally
    // dropped here. Pattern-match `..` so the destructure stays exhaustive
    // even if future fields are added to the variant.
    match cmd {
        BatchCmd::Blame { args, .. } => {
            handlers::dispatch_blame(ctx, &args.name, args.commits, args.callers)
        }
        BatchCmd::Search { args, .. } => handlers::dispatch_search(ctx, &args),
        BatchCmd::Deps { args, .. } => handlers::dispatch_deps(
            ctx,
            &args.name,
            args.reverse,
            args.limit_arg.limit,
            args.cross_project,
        ),
        BatchCmd::Callers { args, .. } => {
            handlers::dispatch_callers(ctx, &args.name, args.limit_arg.limit, args.cross_project)
        }
        BatchCmd::Callees { args, .. } => {
            handlers::dispatch_callees(ctx, &args.name, args.limit_arg.limit, args.cross_project)
        }
        BatchCmd::Explain { args, .. } => {
            handlers::dispatch_explain(ctx, &args.name, args.limit_arg.limit, args.tokens)
        }
        BatchCmd::Similar { args, .. } => {
            handlers::dispatch_similar(ctx, &args.name, args.limit, args.threshold)
        }
        BatchCmd::Gather { args, .. } => handlers::dispatch_gather(ctx, &args),
        BatchCmd::Impact { args, .. } => handlers::dispatch_impact(
            ctx,
            &args.name,
            args.depth,
            args.limit_arg.limit,
            args.suggest_tests,
            args.type_impact,
            args.cross_project,
        ),
        BatchCmd::TestMap { args, .. } => handlers::dispatch_test_map(
            ctx,
            &args.name,
            args.depth,
            args.limit_arg.limit,
            args.cross_project,
        ),
        BatchCmd::Trace { args, .. } => handlers::dispatch_trace(
            ctx,
            &args.source,
            &args.target,
            args.max_depth as usize,
            args.limit_arg.limit,
            args.cross_project,
        ),
        BatchCmd::Dead { args, .. } => {
            handlers::dispatch_dead(ctx, args.include_pub, &args.min_confidence)
        }
        BatchCmd::Related { args, .. } => handlers::dispatch_related(ctx, &args.name, args.limit),
        BatchCmd::Context { args, .. } => {
            handlers::dispatch_context(ctx, &args.path, args.summary, args.compact, args.tokens)
        }
        BatchCmd::Stats { .. } => handlers::dispatch_stats(ctx),
        BatchCmd::Onboard { args, .. } => handlers::dispatch_onboard(
            ctx,
            &args.query,
            args.depth,
            args.limit_arg.limit,
            args.tokens,
        ),
        BatchCmd::Scout { args, .. } => {
            handlers::dispatch_scout(ctx, &args.query, args.limit, args.tokens)
        }
        BatchCmd::Where { args, .. } => {
            handlers::dispatch_where(ctx, &args.description, args.limit)
        }
        BatchCmd::Read { args, .. } => {
            handlers::dispatch_read(ctx, &args.path, args.focus.as_deref())
        }
        BatchCmd::Stale { args, .. } => handlers::dispatch_stale(ctx, args.count_only),
        BatchCmd::Health { .. } => handlers::dispatch_health(ctx),
        BatchCmd::Drift { args, .. } => handlers::dispatch_drift(
            ctx,
            &args.reference,
            args.threshold,
            args.min_drift,
            args.lang.as_deref(),
            args.limit,
        ),
        BatchCmd::Notes { args, .. } => {
            // API-V1.29-4: pass `check` through so the daemon path matches
            // `cqs notes list --check` when routed via the socket.
            handlers::dispatch_notes(
                ctx,
                args.warnings,
                args.patterns,
                args.kind.as_deref(),
                args.check,
            )
        }
        BatchCmd::Task { args, .. } => {
            handlers::dispatch_task(ctx, &args.description, args.limit, args.tokens)
        }
        BatchCmd::Review { args, .. } => {
            handlers::dispatch_review(ctx, args.base.as_deref(), args.tokens)
        }
        BatchCmd::Ci { args, .. } => {
            handlers::dispatch_ci(ctx, args.base.as_deref(), &args.gate, args.tokens)
        }
        BatchCmd::Diff { args, .. } => handlers::dispatch_diff(
            ctx,
            &args.source,
            args.target.as_deref(),
            args.threshold,
            args.lang.as_deref(),
        ),
        BatchCmd::ImpactDiff { args, .. } => {
            handlers::dispatch_impact_diff(ctx, args.base.as_deref())
        }
        BatchCmd::Plan { args, .. } => {
            handlers::dispatch_plan(ctx, &args.description, args.limit, args.tokens)
        }
        BatchCmd::Suggest { args, .. } => handlers::dispatch_suggest(ctx, args.apply),
        BatchCmd::Gc { .. } => handlers::dispatch_gc(ctx),
        BatchCmd::Refresh => handlers::dispatch_refresh(ctx),
        BatchCmd::Ping => handlers::dispatch_ping(ctx),
        BatchCmd::Status => handlers::dispatch_status(ctx),
        BatchCmd::Reconcile { hook, args } => handlers::dispatch_reconcile(ctx, hook, args),
        BatchCmd::WaitFresh { wait_secs } => handlers::dispatch_wait_fresh(ctx, wait_secs),
        BatchCmd::Help => handlers::dispatch_help(),
        #[cfg(test)]
        BatchCmd::TestSleep { ms } => {
            // #1127 regression test fixture. Sleeps the dispatcher thread for
            // `ms` milliseconds, then returns a tiny envelope. Two concurrent
            // daemon connections both running `test-sleep --ms N` must finish
            // in ~max(N, N) — *not* 2*N — when the lock is held only across
            // checkout_view.
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(serde_json::json!({"slept_ms": ms}))
        }
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
            BatchCmd::Search { ref args, .. } => {
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
            BatchCmd::Search { ref args, .. } => {
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
            BatchCmd::Callers { ref args, .. } => assert_eq!(args.name, "my_func"),
            _ => panic!("Expected Callers command"),
        }
    }

    #[test]
    fn test_parse_gather_with_ref() {
        let input =
            BatchInput::try_parse_from(["gather", "alarm config", "--ref", "aveva"]).unwrap();
        match input.cmd {
            BatchCmd::Gather { ref args, .. } => {
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
            BatchCmd::Dead { ref args, .. } => {
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

    // Task B2: `ping` parses as a zero-arg subcommand. Pin both the parse
    // and the `is_pipeable` classification so a future variant tweak
    // doesn't accidentally make ping show up as a pipeable stage.
    #[test]
    fn test_parse_ping() {
        let input = BatchInput::try_parse_from(["ping"]).unwrap();
        assert!(matches!(input.cmd, BatchCmd::Ping));
        assert!(
            !input.cmd.is_pipeable(),
            "ping is a healthcheck, not pipeable"
        );
    }

    #[test]
    fn test_parse_trace() {
        let input = BatchInput::try_parse_from(["trace", "main", "validate"]).unwrap();
        match input.cmd {
            BatchCmd::Trace { ref args, .. } => {
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
            BatchCmd::Context { ref args, .. } => {
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
        assert!(matches!(input.cmd, BatchCmd::Stats { .. }));
    }

    #[test]
    fn test_parse_impact_with_suggest() {
        let input =
            BatchInput::try_parse_from(["impact", "foo", "--depth", "3", "--suggest-tests"])
                .unwrap();
        match input.cmd {
            BatchCmd::Impact { ref args, .. } => {
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
            BatchCmd::Scout { ref args, .. } => {
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
            BatchCmd::Scout { ref args, .. } => {
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
            BatchCmd::Where { ref args, .. } => {
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
            BatchCmd::Read { ref args, .. } => {
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
            BatchCmd::Read { ref args, .. } => {
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
        assert!(matches!(input.cmd, BatchCmd::Health { .. }));
    }

    #[test]
    fn test_parse_notes() {
        let input = BatchInput::try_parse_from(["notes"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { ref args, .. } => {
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
            BatchCmd::Notes { ref args, .. } => {
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
            BatchCmd::Notes { ref args, .. } => {
                assert!(!args.warnings);
                assert!(args.patterns);
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_notes_kind() {
        let input = BatchInput::try_parse_from(["notes", "--kind", "todo"]).unwrap();
        match input.cmd {
            BatchCmd::Notes { ref args, .. } => {
                assert_eq!(args.kind.as_deref(), Some("todo"));
            }
            _ => panic!("Expected Notes command"),
        }
    }

    #[test]
    fn test_parse_blame() {
        let input = BatchInput::try_parse_from(["blame", "my_func"]).unwrap();
        match input.cmd {
            BatchCmd::Blame { ref args, .. } => {
                assert_eq!(args.name, "my_func");
                assert_eq!(args.commits, 10); // default
                assert!(!args.callers);
            }
            _ => panic!("Expected Blame command"),
        }
    }

    #[test]
    fn test_parse_blame_with_flags() {
        // API-V1.22-4: short flag is `-n`, long flag is `--commits`. Old
        // `-d`/`--depth` is hard-renamed (no alias) — see CLAUDE.md
        // "No External Users" / agents-only contract.
        let input =
            BatchInput::try_parse_from(["blame", "my_func", "-n", "5", "--callers"]).unwrap();
        match input.cmd {
            BatchCmd::Blame { ref args, .. } => {
                assert_eq!(args.name, "my_func");
                assert_eq!(args.commits, 5);
                assert!(args.callers);
            }
            _ => panic!("Expected Blame command"),
        }
    }

    #[test]
    fn test_parse_blame_long_commits() {
        let input = BatchInput::try_parse_from(["blame", "my_func", "--commits", "3"]).unwrap();
        match input.cmd {
            BatchCmd::Blame { ref args, .. } => {
                assert_eq!(args.commits, 3);
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
                limit_arg: crate::cli::args::LimitArg { limit: 5 },
            },
            output: TextJsonArgs { json: false },
        };
        assert!(callers.is_pipeable());

        let scout = BatchCmd::Scout {
            args: crate::cli::args::ScoutArgs {
                query: "foo".into(),
                limit: 5,
                tokens: None,
            },
            output: TextJsonArgs { json: false },
        };
        assert!(scout.is_pipeable());

        // Non-pipeable variants: should return false.
        assert!(!BatchCmd::Stats {
            output: TextJsonArgs { json: false }
        }
        .is_pipeable());
        assert!(!BatchCmd::Health {
            output: TextJsonArgs { json: false }
        }
        .is_pipeable());
        assert!(!BatchCmd::Gc {
            output: TextJsonArgs { json: false }
        }
        .is_pipeable());
        assert!(!BatchCmd::Refresh.is_pipeable());
        assert!(!BatchCmd::Help.is_pipeable());
        assert!(!BatchCmd::Stale {
            args: crate::cli::args::StaleArgs { count_only: false },
            output: TextJsonArgs { json: false },
        }
        .is_pipeable());

        let dead = BatchCmd::Dead {
            args: crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
            },
            output: TextJsonArgs { json: false },
        };
        assert!(!dead.is_pipeable());
    }
}
