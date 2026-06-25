//! Batch command parsing and dispatch routing.

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::BatchView;

use crate::cli::args::{
    BlameArgs, CallersArgs, CiArgs, ContextArgs, DeadArgs, DepsArgs, DiffArgs, DriftArgs,
    ExplainArgs, GatherArgs, ImpactArgs, ImpactDiffArgs, NotesListArgs, OnboardArgs, PlanArgs,
    ReadArgs, ReconcileArgs, RelatedArgs, ReviewArgs, ScoutArgs, SearchArgs, SearchLegsArgs,
    SimilarArgs, StaleArgs, SuggestArgs, TaskArgs, TestMapArgs, TraceArgs, WaitFreshArgs,
    WhereArgs,
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
/// batch *always* serializes to JSON. The flag is accepted and silently a
/// no-op (the handler ignores `output.json`/`output.format` because the batch
/// transport itself frames the response as JSONL on the daemon socket and on
/// stdout).
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
    /// Embeds shared `SearchArgs` so CLI and batch share one source of truth
    /// for search flags.
    Search {
        #[command(flatten)]
        args: SearchArgs,
        #[command(flatten)]
        #[allow(dead_code, reason = "Task #8: --json accepted for CLI parity")]
        output: TextJsonArgs,
    },
    /// SPLADE-fusion inspector: the three pre-fusion legs (dense / sparse / fused)
    SearchLegs {
        #[command(flatten)]
        args: SearchLegsArgs,
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
        /// `OutputArgs` here matches the CLI side (text/json/mermaid).
        /// Mermaid is silently downgraded to JSON in batch because the daemon
        /// socket framer assumes JSONL. Adding a non-JSON wire format would
        /// require re-shaping `dispatch_line`.
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
        /// See the `Impact` variant — `OutputArgs` mirrors the CLI's
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
    /// Add a note (MCP Phase 2a gated mutation channel).
    ///
    /// `#[command(skip)]` — NOT argv-reachable on the daemon socket. The only
    /// constructor is `json_args::build_batch_cmd`, gated behind
    /// `CQS_MCP_ENABLE_MUTATIONS`. The handler writes `docs/notes.toml` (a file,
    /// not the `Store<ReadOnly>`); the watch loop reindexes — so the daemon's
    /// read-only Store typestate is preserved.
    #[command(skip)]
    NotesAdd {
        args: crate::cli::commands::notes::NotesAddArgs,
    },
    /// Update a note (MCP Phase 2a gated mutation channel). See `NotesAdd`.
    #[command(skip)]
    NotesUpdate {
        args: crate::cli::commands::notes::NotesUpdateArgs,
    },
    /// Remove a note (MCP Phase 2a gated mutation channel). See `NotesAdd`.
    #[command(skip)]
    NotesRemove {
        args: crate::cli::commands::notes::NotesRemoveArgs,
    },
    /// Queue a reindex (MCP Phase 2b — `cqs_index` fire-and-forget).
    ///
    /// `#[command(skip)]` — NOT argv-reachable on the daemon socket. The only
    /// constructor is `json_args::build_batch_cmd`, gated behind
    /// `CQS_MCP_ENABLE_MUTATIONS`. The handler does NOT build the index: it
    /// flips the shared `SharedReconcileSignal` (the same primitive `reconcile`
    /// uses) and returns immediately; the watch loop performs the actual
    /// reindex on its next tick. So it never acquires a writable `Store` — the
    /// daemon's read-only Store typestate is preserved. The destructive
    /// `index --force` variant is withheld by ABSENCE: the core `IndexArgs`
    /// exposes no `force` field.
    #[command(skip)]
    Index {
        args: crate::cli::commands::index::IndexArgs,
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
    /// Zero-arg command served by `cqs watch --serve`. The CLI
    /// `cqs status --watch-fresh [--json]` is the user-facing surface;
    /// the daemon returns the JSON payload of `cqs::watch_status::WatchSnapshot`.
    /// `cqs batch` (no watch loop) returns the default `unknown` snapshot.
    Status,
    /// Request an out-of-band reconciliation pass.
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
        #[command(flatten)]
        args: ReconcileArgs,
    },
    /// Block until the watch loop transitions to Fresh, or `wait_secs`
    /// elapses. Server-side wait, no client-side polling.
    ///
    /// One round-trip total — the daemon parks the request on a
    /// `FreshNotifier` shared with the watch loop. When `publish_watch_snapshot`
    /// observes a `false → true` transition it issues a `notify_all`,
    /// the parked handler wakes, and replies with the latest snapshot.
    /// On deadline the handler replies with the still-stale snapshot.
    ///
    /// `cqs status --watch-fresh --wait` and `cqs eval --require-fresh`
    /// route through this when talking to a daemon. Outside `cqs watch
    /// --serve` the notifier never flips and the call hits the deadline
    /// naturally.
    #[command(name = "wait-fresh")]
    WaitFresh {
        #[command(flatten)]
        args: WaitFreshArgs,
    },
    /// Show help
    Help,
    /// Test-only: sleep `--ms` milliseconds before returning. Used by the
    /// daemon-parallelism regression tests to force two concurrent handlers to
    /// overlap on wall-clock; the variant is `#[cfg(test)]`-gated so it never
    /// reaches a release binary.
    #[cfg(test)]
    #[command(name = "test-sleep")]
    TestSleep {
        #[arg(long, default_value_t = 200)]
        ms: u64,
    },
}

/// Per-variant dispatch table — single source of truth for both
/// `BatchCmd::is_pipeable` and `dispatch()`.
///
/// Each row carries `(Variant, handler_fn, pipeable)`. Three buckets:
/// - `args_variants`: struct variants with `args: XArgs`. Handler signature
///   is `fn(ctx: &BatchView, args: &XArgs) -> Result<Value>`.
/// - `ctx_only_variants`: struct variants without `args` (e.g. `Stats`,
///   `Health` — output-flag-only). Handler signature is `fn(ctx: &BatchView)`.
/// - `unit_variants`: zero-field variants (e.g. `Refresh`, `Ping`).
///   Handler signature is `fn(ctx: &BatchView)`.
///
/// Adding a new variant requires:
/// 1. Adding the variant to `BatchCmd`.
/// 2. Writing the handler.
/// 3. Adding one row to this macro.
///
/// All three steps are compile-enforced by the exhaustive match the macro
/// emits — a missing row produces a `non-exhaustive patterns` error, not a
/// silent drop. The classification lives in the variant list itself: each row
/// is `(variant_name, is_pipeable)`, expanded into an exhaustive match.
///
/// Struct variants and unit variants are listed in two named blocks so the
/// macro emitter can build the right pattern shape (`Variant { .. }` vs `Variant`)
/// without ambiguity.
///
/// Pipeable: primary input is a function name (so a previous segment's output
/// can be piped in). Not pipeable: queries, paths, git refs, or no positional arg.
/// Single source of truth for both `BatchCmd::is_pipeable` AND `dispatch()`;
/// each row is `(Variant, handler_fn, pipeable)`.
macro_rules! for_each_batch_cmd {
    ($emit:ident) => {
        $emit! {
            args_variants: {
                // Pipeable — primary input is a function name. Each row is
                // `(Variant, handler_fn, cli_name, pipeable)`; `cli_name` is
                // the canonical surface command (matches `clap`'s subcommand
                // name and `telemetry::describe_command`'s output), used to
                // attribute kind-fallback telemetry to the top-level command.
                (Blame,      dispatch_blame,        "blame",       true)
                (Callers,    dispatch_callers,      "callers",     true)
                (Callees,    dispatch_callees,      "callees",     true)
                (Deps,       dispatch_deps,         "deps",        true)
                (Explain,    dispatch_explain,      "explain",     true)
                (Similar,    dispatch_similar,      "similar",     true)
                (Impact,     dispatch_impact,       "impact",      true)
                (TestMap,    dispatch_test_map,     "test-map",    true)
                (Related,    dispatch_related,      "related",     true)
                (Scout,      dispatch_scout,        "scout",       true)

                // Not pipeable — queries, paths, git refs.
                (Search,     dispatch_search,       "search",      false)
                (SearchLegs, dispatch_search_legs,  "search-legs", false)
                (Gather,     dispatch_gather,       "gather",      false)
                (Trace,      dispatch_trace,        "trace",       false)
                (Dead,       dispatch_dead,         "dead",        false)
                (Context,    dispatch_context,      "context",     false)
                (Onboard,    dispatch_onboard,      "onboard",     false)
                (Where,      dispatch_where,        "where",       false)
                (Read,       dispatch_read,         "read",        false)
                (Stale,      dispatch_stale,        "stale",       false)
                (Drift,      dispatch_drift,        "drift",       false)
                (Notes,      dispatch_notes,        "notes",       false)
                (NotesAdd,    dispatch_notes_add,    "notes-add",    false)
                (NotesUpdate, dispatch_notes_update, "notes-update", false)
                (NotesRemove, dispatch_notes_remove, "notes-remove", false)
                (Index,      dispatch_index,        "index",       false)
                (Task,       dispatch_task,         "task",        false)
                (Review,     dispatch_review,       "review",      false)
                (Ci,         dispatch_ci,           "ci",          false)
                (Diff,       dispatch_diff,         "diff",        false)
                (ImpactDiff, dispatch_impact_diff,  "impact-diff", false)
                (Plan,       dispatch_plan,         "plan",        false)
                (Suggest,    dispatch_suggest,      "suggest",     false)
                (Reconcile,  dispatch_reconcile,    "reconcile",   false)
                // wait_secs-only — no positional function name to receive
                // a pipe.
                (WaitFresh,  dispatch_wait_fresh,   "wait-fresh",  false)
            }
            ctx_only_variants: {
                // Struct variants with only `output: TextJsonArgs` (no
                // primary `args` payload). Dispatched as `handler(ctx)`.
                (Stats,   dispatch_stats,   "stats",  false)
                (Health,  dispatch_health,  "health", false)
                (Gc,      dispatch_gc,      "gc",     false)
            }
            unit_variants: {
                (Refresh,  dispatch_refresh,  "refresh", false)
                (Ping,     dispatch_ping,     "ping",    false)
                (Status,   dispatch_status,   "status",  false)
                (Help,     dispatch_help,     "help",    false)
            }
        }
    };
}

/// Emits `BatchCmd::is_pipeable` from the table above.
///
/// The generated `match` is intentionally exhaustive (no wildcard arm), so a
/// new `BatchCmd` variant without a row fails to compile.
/// `test_is_pipeable_exhaustive` below double-pins this.
macro_rules! gen_is_pipeable_impl {
    (
        args_variants:     { $(($v:ident, $h:ident, $n:literal, $p:expr))* }
        ctx_only_variants: { $(($v2:ident, $h2:ident, $n2:literal, $p2:expr))* }
        unit_variants:     { $(($v3:ident, $h3:ident, $n3:literal, $p3:expr))* }
    ) => {
        impl BatchCmd {
            /// Whether this command accepts a piped function name as its first positional arg.
            /// Used by pipeline execution to validate downstream segments.
            pub(crate) fn is_pipeable(&self) -> bool {
                // The handler-fn idents are consumed by `gen_dispatch_impl`,
                // not this one — `let _` arms keep them in scope without
                // tripping the unused-ident lint.
                match self {
                    $(BatchCmd::$v { .. } => { let _ = stringify!($h); let _ = $n; $p },)*
                    $(BatchCmd::$v2 { .. } => { let _ = stringify!($h2); let _ = $n2; $p2 },)*
                    $(BatchCmd::$v3 => { let _ = stringify!($h3); let _ = $n3; $p3 },)*
                    #[cfg(test)]
                    BatchCmd::TestSleep { .. } => false,
                }
            }

            /// Canonical surface command name for this variant — the same
            /// string `clap` parses and `telemetry::describe_command` records
            /// for the CLI path. Used at the dispatch chokepoint to attribute
            /// a kind-fallback fired by an internal graph core to the
            /// top-level command the agent actually invoked.
            ///
            /// `TestSleep` is a test-only fixture with no surface command; it
            /// maps to `"test-sleep"` for completeness.
            pub(crate) fn command_name(&self) -> &'static str {
                match self {
                    $(BatchCmd::$v { .. } => { let _ = stringify!($h); let _ = $p; $n },)*
                    $(BatchCmd::$v2 { .. } => { let _ = stringify!($h2); let _ = $p2; $n2 },)*
                    $(BatchCmd::$v3 => { let _ = stringify!($h3); let _ = $p3; $n3 },)*
                    #[cfg(test)]
                    BatchCmd::TestSleep { .. } => "test-sleep",
                }
            }

            /// Every name `command_name()` can return in a test build, in
            /// table order plus the `#[cfg(test)]` `TestSleep` fixture (which
            /// IS a real clap subcommand under test, so the bidirectional
            /// exhaustiveness check must account for it). Built from the SAME
            /// table as `command_name()`, so the test reads from the one
            /// source of truth — no second hand-maintained list to drift.
            #[cfg(test)]
            pub(crate) const ALL_COMMAND_NAMES: &'static [&'static str] = &[
                $($n,)*
                $($n2,)*
                $($n3,)*
                "test-sleep",
            ];
        }
    };
}

for_each_batch_cmd!(gen_is_pipeable_impl);

/// Emits `dispatch(ctx, cmd)` from the same table. One arm per row;
/// the exhaustive match is the compile-time guarantee that every variant
/// has a handler.
macro_rules! gen_dispatch_impl {
    (
        args_variants:     { $(($v:ident, $h:ident, $n:literal, $p:expr))* }
        ctx_only_variants: { $(($v2:ident, $h2:ident, $n2:literal, $p2:expr))* }
        unit_variants:     { $(($v3:ident, $h3:ident, $n3:literal, $p3:expr))* }
    ) => {
        /// Execute a batch command and return a JSON value. The
        /// BatchCmd → handler mapping lives in `for_each_batch_cmd!`
        /// — the only edit needed when adding a new command is one row
        /// in that table plus the handler implementation.
        ///
        /// Takes a [`BatchView`] (snapshot of BatchContext caches built
        /// under a brief critical section).
        ///
        /// This is the single daemon/batch/stdin/pipeline/JSON-args dispatch
        /// chokepoint (every surface funnels through here), so it installs the
        /// kind-fallback origin — the top-level command name plus the served
        /// project dir — for the duration of the handler. A graph core that
        /// fires a fallback deeper in the same synchronous call reads it back
        /// and attributes the fallback to this command and this project rather
        /// than to its own sub-op name or the daemon's process cwd.
        pub(crate) fn dispatch(ctx: &BatchView, cmd: BatchCmd) -> Result<serde_json::Value> {
            let _span = tracing::debug_span!("batch_dispatch").entered();
            // Single table-driven query-log call.
            log_query_for(&cmd);
            let _fallback_origin = crate::cli::telemetry::enter_fallback_origin(
                cmd.command_name(),
                &ctx.cqs_dir,
            );
            // `output` field on each variant is intentionally dropped —
            // batch always emits JSON. Pattern-match `..` so the
            // destructure stays exhaustive even if future fields are
            // added to a variant. `$n` (cli_name) is consumed by
            // `command_name()` above, referenced here to keep the column live.
            match cmd {
                $(BatchCmd::$v { args, .. } => {
                    let _ = $p; // keep the pipeability column referenced
                    let _ = $n;
                    handlers::$h(ctx, &args)
                },)*
                $(BatchCmd::$v2 { .. } => {
                    let _ = $p2;
                    let _ = $n2;
                    handlers::$h2(ctx)
                },)*
                $(BatchCmd::$v3 => {
                    let _ = $p3;
                    let _ = $n3;
                    handlers::$h3(ctx)
                },)*
                #[cfg(test)]
                BatchCmd::TestSleep { ms } => {
                    // Regression test fixture. Sleeps the dispatcher
                    // thread for `ms` milliseconds, then returns a tiny
                    // envelope. Two concurrent daemon connections both
                    // running `test-sleep --ms N` must finish in
                    // ~max(N, N) — *not* 2*N — when the lock is held only
                    // across checkout_view.
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    Ok(serde_json::json!({"slept_ms": ms}))
                }
            }
        }
    };
}

for_each_batch_cmd!(gen_dispatch_impl);

// ─── Query logging ───────────────────────────────────────────────────────────

/// Per-variant table for the eval-capture query log.
///
/// Centralises the `log_query(command_name, query_field)` mapping so adding a
/// new logged variant is one row in this table. Each row pairs a command-name
/// string with the right field name (`args.query` vs `args.description` vs ...).
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
    // Prefer the platform's native cache dir; fall back to `~/.cache`.
    // Skip silently if neither is resolvable.
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

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::assert_matches;

    #[test]
    fn test_parse_search() {
        let input = BatchInput::try_parse_from(["search", "hello"]).unwrap();
        match input.cmd {
            BatchCmd::Search { ref args, .. } => {
                assert_eq!(args.query, "hello");
                assert_eq!(args.limit_arg.limit, 5); // default
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
                assert_eq!(args.limit_arg.limit, 3);
                assert!(args.name_only);
            }
            _ => panic!("Expected Search command"),
        }
    }

    /// Default-on flip: the batch `search` surface mirrors the CLI —
    /// `--no-overlay` parses, and `--overlay --no-overlay` is rejected by clap.
    #[test]
    fn test_parse_search_overlay_flags_conflict() {
        let off = BatchInput::try_parse_from(["search", "hello", "--no-overlay"]).unwrap();
        match off.cmd {
            BatchCmd::Search { ref args, .. } => {
                assert!(args.overlay.no_overlay, "--no-overlay sets the flag");
                assert!(!args.overlay.overlay);
            }
            _ => panic!("Expected Search command"),
        }
        let conflict = BatchInput::try_parse_from(["search", "hello", "--overlay", "--no-overlay"]);
        assert!(
            conflict.is_err(),
            "batch search --overlay and --no-overlay must conflict"
        );
    }

    /// Part A: scout/gather/task accept the same overlay tri-state via the
    /// flattened `OverlayArgs` — `--overlay` / `--no-overlay` parse and conflict,
    /// and the hidden `--overlay-root` rides the wire.
    #[test]
    fn test_parse_seed_overlay_flags() {
        for cmd in ["scout", "gather", "task"] {
            let on = BatchInput::try_parse_from([cmd, "q", "--overlay", "--overlay-root", "/wt"])
                .unwrap();
            let (overlay, no_overlay, root) = match on.cmd {
                BatchCmd::Scout { ref args, .. } => (
                    args.overlay.overlay,
                    args.overlay.no_overlay,
                    args.overlay.overlay_root.clone(),
                ),
                BatchCmd::Gather { ref args, .. } => (
                    args.overlay.overlay,
                    args.overlay.no_overlay,
                    args.overlay.overlay_root.clone(),
                ),
                BatchCmd::Task { ref args, .. } => (
                    args.overlay.overlay,
                    args.overlay.no_overlay,
                    args.overlay.overlay_root.clone(),
                ),
                _ => panic!("unexpected command for {cmd}"),
            };
            assert!(overlay, "{cmd} --overlay sets the flag");
            assert!(!no_overlay);
            assert_eq!(root.as_deref(), Some(std::path::Path::new("/wt")));

            let conflict = BatchInput::try_parse_from([cmd, "q", "--overlay", "--no-overlay"]);
            assert!(
                conflict.is_err(),
                "{cmd} --overlay and --no-overlay must conflict"
            );
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
                assert_matches!(args.min_confidence, cqs::store::DeadConfidence::High);
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
        assert_matches!(input.cmd, BatchCmd::Ping);
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
        assert_matches!(input.cmd, BatchCmd::Stats { .. });
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
                assert_eq!(args.limit_arg.limit, 5); // default
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
            "--search-limit",
            "30",
            "--search-threshold",
            "0.05",
            "--min-gap-ratio",
            "0.25",
        ])
        .unwrap();
        match input.cmd {
            BatchCmd::Scout { ref args, .. } => {
                assert_eq!(args.query, "error handling");
                assert_eq!(args.limit_arg.limit, 20);
                assert_eq!(args.tokens, Some(2000));
                assert_eq!(args.search_limit, Some(30));
                assert_eq!(args.search_threshold, Some(0.05));
                assert_eq!(args.min_gap_ratio, Some(0.25));
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
                // --limit defaults to 5.
                assert_eq!(args.limit_arg.limit, 5);
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
        assert_matches!(input.cmd, BatchCmd::Stale { .. });
    }

    #[test]
    fn test_parse_health() {
        let input = BatchInput::try_parse_from(["health"]).unwrap();
        assert_matches!(input.cmd, BatchCmd::Health { .. });
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
        // blame's short flag is `-n`, long flag is `--commits` (no `--depth`
        // alias).
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

    // Compile-time guard that every BatchCmd variant is either
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
                edge_kind: None,
                overlay: Default::default(),
            },
            output: TextJsonArgs { json: false },
        };
        assert!(callers.is_pipeable());

        let scout = BatchCmd::Scout {
            args: crate::cli::args::ScoutArgs {
                query: "foo".into(),
                limit_arg: crate::cli::args::LimitArg { limit: 5 },
                tokens: None,
                search_limit: None,
                search_threshold: None,
                min_gap_ratio: None,
                overlay: crate::cli::args::OverlayArgs::default(),
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
                verdict: None,
                overlay: Default::default(),
            },
            output: TextJsonArgs { json: false },
        };
        assert!(!dead.is_pipeable());
    }

    /// The `#[command(skip)]` variants — argv-UNreachable on the daemon
    /// socket (their only constructor is `json_args::build_batch_cmd`, gated
    /// behind `CQS_MCP_ENABLE_MUTATIONS`), so clap's `find_subcommand` does
    /// NOT know them. They never fire kind-fallbacks (they're notes-mutation /
    /// reindex commands, not graph cores), so their `command_name()` is inert
    /// for telemetry attribution. Listed here so the exhaustiveness test below
    /// excludes them DELIBERATELY: adding a new `#[command(skip)]` variant
    /// forces a conscious edit to this slice, and dropping `#[command(skip)]`
    /// from one of these (making it a real subcommand) trips the
    /// `skip_set_is_genuinely_not_clap_subcommands` arm.
    #[cfg(test)]
    const CLAP_SKIPPED_COMMAND_NAMES: &[&str] =
        &["notes-add", "notes-update", "notes-remove", "index"];

    /// `command_name()` must return the canonical clap subcommand string for
    /// EVERY argv-reachable variant — the same name
    /// `telemetry::describe_command` records on the CLI path, so kind-fallback
    /// telemetry buckets line up across the CLI and daemon surfaces.
    ///
    /// Exhaustive and bidirectional (no hand-picked spot-check):
    /// - Every name in `BatchCmd::ALL_COMMAND_NAMES` (built from the dispatch
    ///   table, the single source of truth) that is NOT a documented
    ///   `#[command(skip)]` variant must resolve to a real clap subcommand.
    ///   A renamed `cli_name` that drifts from its clap name fails here.
    /// - Conversely, every clap subcommand must appear in
    ///   `ALL_COMMAND_NAMES`. A new argv-reachable variant whose `cli_name`
    ///   doesn't match its clap subcommand name fails here.
    /// - Every skip-listed name must genuinely NOT be a clap subcommand, so
    ///   the exclusion stays honest: lose `#[command(skip)]` and this fails.
    #[test]
    fn command_name_matches_clap_subcommand_exhaustively() {
        use clap::CommandFactory;
        use std::collections::HashSet;

        let batch = BatchInput::command();
        let skip: HashSet<&str> = CLAP_SKIPPED_COMMAND_NAMES.iter().copied().collect();

        // Forward: every table name (minus skips) is a real clap subcommand.
        let table_names: HashSet<&str> = BatchCmd::ALL_COMMAND_NAMES.iter().copied().collect();
        for &name in BatchCmd::ALL_COMMAND_NAMES {
            if skip.contains(name) {
                continue;
            }
            assert!(
                batch.find_subcommand(name).is_some(),
                "BatchCmd::command_name() yields `{name}`, which is not a clap \
                 subcommand — a `cli_name` drifted from its clap name, or the \
                 variant should be in CLAP_SKIPPED_COMMAND_NAMES"
            );
        }

        // Reverse: every clap subcommand is covered by a table name.
        for sub in batch.get_subcommands() {
            let name = sub.get_name();
            assert!(
                table_names.contains(name),
                "clap subcommand `{name}` has no matching BatchCmd::command_name() \
                 entry — a new argv-reachable variant whose cli_name doesn't match \
                 its clap subcommand name"
            );
        }

        // The skip-list documents a REAL exclusion: each skipped name must
        // genuinely be absent from clap, so dropping `#[command(skip)]` from
        // one of these variants is a conscious, test-visible change.
        for &name in CLAP_SKIPPED_COMMAND_NAMES {
            assert!(
                batch.find_subcommand(name).is_none(),
                "`{name}` is in CLAP_SKIPPED_COMMAND_NAMES but IS a clap subcommand \
                 — it lost its `#[command(skip)]`; remove it from the skip-list"
            );
            // …and it must still be a real table row (the inert telemetry name
            // exists even though clap can't reach the variant from argv).
            assert!(
                table_names.contains(name),
                "skip-listed `{name}` is missing from the dispatch table"
            );
        }
    }

    /// Exhaustiveness link between the two enums the daemon-forward path
    /// straddles: every `Commands` variant marked daemon-capable
    /// (`#[cqs_cmd(batch = "daemon")]`, plus `"runtime"` whose support may
    /// resolve to Daemon per-invocation) must have a same-named `BatchCmd`
    /// subcommand. Without this pin, a variant marked daemon without a batch
    /// handler fails only at runtime, only daemon-up — `cqs <cmd>` errors
    /// with a daemon parse failure while working daemon-down.
    #[test]
    fn every_daemon_capable_command_has_a_batch_subcommand() {
        use clap::CommandFactory;
        let batch = BatchInput::command();
        let names = crate::cli::definitions::Commands::daemon_capable_variant_names();
        assert!(
            !names.is_empty(),
            "daemon_capable_variant_names() must not be empty — derive regression"
        );
        for name in names {
            assert!(
                batch.find_subcommand(name).is_some(),
                "Commands::{name} is marked daemon-capable but BatchCmd has no `{name}` \
                 subcommand — daemon-up `cqs {name}` would fail at runtime. Either add the \
                 batch handler or reclassify the variant as batch = \"cli\""
            );
        }
    }
}
