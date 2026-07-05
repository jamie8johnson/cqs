//! `cqs eval` — A/B harness for measuring search quality.
//!
//! A Rust subcommand that runs the production search path against a JSON
//! query set and prints R@K aggregates.
//!
//! Workflow:
//!   `cqs eval evals/queries/v3_test.json` — run + print
//!   `cqs eval evals/queries/v3_test.json --json` — machine-readable
//!   `cqs eval evals/queries/v3_test.json --save baseline.json` — capture
//!   `cqs eval evals/queries/v3_test.json --baseline baseline.json` — diff

mod baseline;
mod runner;

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use cqs::eval::schema::QuerySet;
use cqs::store::ReadOnly;

use crate::cli::args::RerankerMode;
use crate::cli::commands::{daemon_control_hint, DaemonHint};
use crate::cli::CommandContext;

pub(crate) use runner::{
    eval_core, EvalArgs, EvalCoreParams, EvalFreshness, EvalOutput, IndexSubstrate,
};

/// CLI args for `cqs eval` — the full flag surface (query set + limit +
/// category + reranker, plus the CLI-only `--save` / `--baseline` /
/// `--tolerance` / `--require-fresh` post-processing + gate knobs).
///
/// The WIRE subset the daemon/MCP surface accepts is the narrower
/// [`runner::EvalArgs`] (query_file, limit, category, reranker); the
/// post-processing + gate flags are adapter-owned and do not ride the wire.
/// `Commands::Eval` stays `batch = "cli"`: the generic auto-forward never
/// touches eval — [`cmd_eval`] OWNS the relay (gate → dial → honest outcome →
/// post-process), because eval's NUMBERS depend on which substrate computed
/// them, so the silent inline fallback the generic routing uses would lie.
#[derive(Debug, Clone, clap::Args)]
pub(crate) struct EvalCmdArgs {
    /// Path to the queries JSON file (v3 schema)
    pub query_file: PathBuf,

    /// Output as JSON instead of text
    #[arg(long)]
    pub json: bool,

    /// Max results retrieved per query (used for R@K denominator cap)
    ///
    /// `-n` short flag for parity with other CLI commands that accept a
    /// result cap (search, gather, related, etc.). The default 20 is the R@K
    /// cap, intentionally larger than the default of 5 used elsewhere.
    #[arg(short = 'n', long, default_value = "20")]
    pub limit: usize,

    /// Restrict the run to one category (e.g. `multi_step`)
    #[arg(long)]
    pub category: Option<String>,

    /// Save the resulting report to this path (JSON)
    #[arg(long)]
    pub save: Option<PathBuf>,

    /// Compare current run against a saved baseline
    #[arg(long)]
    pub baseline: Option<PathBuf>,

    /// Tolerance for `--baseline` diff (percentage points; default 1.0)
    #[arg(long, default_value = "1.0")]
    pub tolerance: f64,

    /// Off-switch for the default-on `--require-fresh` gate. Set
    /// `CQS_EVAL_REQUIRE_FRESH=0` for the env equivalent.
    ///
    /// Skips the watch-mode freshness gate that otherwise blocks the run
    /// until the running `cqs watch --serve` daemon reports
    /// `state == fresh`.
    ///
    /// The gate is on by default because eval against a stale index is
    /// indistinguishable from a regression — a 5-25pp R@K shift from
    /// fixture-line drift is identical in shape to a real model degradation.
    /// Pass `--no-require-fresh` for offline runs (no daemon, hand-built
    /// index) where the stale check is noise.
    #[arg(long = "no-require-fresh", action = clap::ArgAction::SetTrue)]
    pub no_require_fresh: bool,

    /// How long `--require-fresh` waits for the daemon to reach
    /// `state == fresh` before erroring out. Capped at 600 (10 min)
    /// inside the wait helper so a runaway agent can't pin the socket.
    ///
    /// Note: `cqs status --wait-secs` has the same semantics; default
    /// differs by use case (eval default = 600, status = 30).
    ///
    /// `--wait-secs` is a visible alias so an agent learning the status-side
    /// spelling transfers cleanly.
    #[arg(
        long = "require-fresh-secs",
        visible_alias = "wait-secs",
        default_value_t = 600u64
    )]
    pub require_fresh_secs: u64,

    /// Apply a reranker stage after retrieval. Default `none` skips
    /// reranking. `onnx` runs the cross-encoder configured by `[reranker]` /
    /// `CQS_RERANKER_MODEL`.
    ///
    /// When `onnx` or `llm` is selected, stage 1 over-retrieves to the
    /// `rerank_pool_size(limit)` cap (mirrors `cqs <q> --rerank`); stage 2
    /// truncates to `limit` after scoring. R@K can shift in either direction
    /// — reranker promotes the gold to position 1 (R@1 win) but a gold
    /// hovering near the pool edge can fall out of top-K under a poor
    /// scorer (R@K loss).
    #[arg(long = "reranker", value_enum, default_value_t = RerankerMode::None)]
    pub reranker: RerankerMode,
}

/// CLI handler for `cqs eval` — the adapter that OWNS the relay.
///
/// Flow: validate flags → freshness gate → [`obtain_report`] (dial the resident
/// daemon when eligible, else a fresh CLI-side build) → post-process (text/JSON,
/// `--save`, `--baseline`). The report's `index_substrate` records which
/// substrate produced the numbers so a gate can compare like with like.
///
/// Degradation + sharing: the daemon serves at most one eval at a time
/// (single-flight); a busy or absent daemon degrades to one explicit notice
/// then a fresh CLI-side run — an EXPLICIT substrate switch, never a silent one.
/// The dial is a ONE-SHOT request/response: there is no progress stream and a
/// disconnect does not abort the server-side run. A response lost past the eval
/// budget (the daemon booked it and is still running) is a hard error, not a
/// silent inline rerun — a second run would put a second GPU session on the same
/// work.
pub(crate) fn cmd_eval(ctx: &CommandContext<'_, ReadOnly>, args: &EvalCmdArgs) -> Result<()> {
    let _span = tracing::info_span!(
        "cmd_eval",
        query_file = %args.query_file.display(),
        category = ?args.category,
        limit = args.limit,
    )
    .entered();

    // Top-level `--json` always wins (mirrors `cmd_model`). `cqs --json eval
    // foo.json` must emit envelope JSON even if the user didn't repeat
    // `--json` after the subcommand — otherwise agents calling the CLI with
    // the global flag get text and a parse error.
    let json = ctx.cli.json || args.json;

    if args.limit == 0 {
        anyhow::bail!("--limit must be at least 1");
    }
    if !args.tolerance.is_finite() || args.tolerance < 0.0 {
        anyhow::bail!(
            "--tolerance must be a finite non-negative number, got {}",
            args.tolerance
        );
    }

    // Validate --save path: eval reports are JSON-only. Reject foreign
    // extensions so a typo (e.g. `--save baseline.txt`) surfaces immediately
    // instead of writing JSON to a misnamed file. Missing extension is
    // tolerated — append `.json` and inform the operator.
    let save_path: Option<PathBuf> = match args.save.as_deref() {
        None => None,
        Some(p) => {
            let ext = p.extension().and_then(|e| e.to_str());
            match ext {
                Some(e) if e.eq_ignore_ascii_case("json") => Some(p.to_path_buf()),
                Some(other) => {
                    anyhow::bail!(
                        "--save must end in .json (got .{other}); eval reports are JSON-only"
                    );
                }
                None => {
                    let with_ext = p.with_extension("json");
                    tracing::info!(path = %with_ext.display(), "appending .json to --save path");
                    Some(with_ext)
                }
            }
        }
    };

    // Gate the run on watch-mode freshness. Eval's R@K numbers shift 5-25pp
    // on a stale index from fixture-line drift alone, which is
    // indistinguishable from a real regression. Block until the daemon
    // reports `state == fresh`, surface a clear error if it can't. Runs on
    // EVERY invocation, before the relay — the gate is a CLI-surface concern.
    require_fresh_gate(&args.no_require_fresh, args.require_fresh_secs)?;

    // Acquire the report: relay to the resident daemon when eligible (its
    // in-memory index → `index_substrate = "resident"`), else compute a
    // fresh cold build CLI-side (`index_substrate = "fresh-built"`). The
    // relay is honest — an absent/busy daemon prints a notice and runs local;
    // a daemon that BOOKED the run but lost the response is a hard error
    // (rerunning inline would put a second GPU session on the same run).
    let report = obtain_report(ctx, args)?;

    // When --baseline is set, prefer the diff output over the raw report —
    // a CI-shaped invocation just wants the diff. The raw report still
    // lands on disk via --save below if requested.
    if args.baseline.is_none() {
        // Output (text or JSON) before --save so the user sees results even
        // if --save's directory is missing or unwritable.
        if json {
            crate::cli::json_envelope::emit_json(&report)?;
        } else {
            print_text_report(&report);
        }
    }

    if let Some(save_path) = save_path.as_ref() {
        let bytes =
            serde_json::to_vec_pretty(&report).context("Failed to serialize eval report")?;
        std::fs::write(save_path, &bytes)
            .with_context(|| format!("Failed to write baseline to {}", save_path.display()))?;
        eprintln!("[eval] saved baseline to {}", save_path.display());
    }

    if let Some(baseline_path) = &args.baseline {
        let diff = baseline::compare_against_baseline(&report, baseline_path, args.tolerance)?;
        if !diff.regressions.is_empty() {
            // Per-category regression past tolerance → CI-friendly exit 1.
            // In JSON mode, emit a single error envelope — failure paths
            // advertise `{data:null, error:{code,message}, version:1}`.
            // Skip print_diff_report — emitting both a success diff envelope and an
            // error envelope on the same stdout would produce two JSON documents
            // back-to-back, breaking single-doc consumers. Users who want the
            // structured diff on regression should re-run without --json or use --save.
            let msg = format!(
                "{} regression(s) past tolerance \u{00b1}{:.1}pp",
                diff.regressions.len(),
                diff.tolerance_pp
            );
            if json {
                // INVALID_INPUT fits the regression case: the eval ran fine, but the
                // inputs (current run + baseline) failed the user-defined gate.
                // INTERNAL would imply a cqs bug.
                crate::cli::json_envelope::emit_json_error(
                    crate::cli::json_envelope::error_codes::INVALID_INPUT,
                    &msg,
                )?;
            } else {
                baseline::print_diff_report(&diff, false);
                eprintln!("[eval] {} \u{2014} exit 1", msg);
            }
            std::process::exit(1);
        } else {
            baseline::print_diff_report(&diff, json);
        }
    }

    Ok(())
}

/// How an eval daemon dial resolved. The CLI adapter owns this decision — the
/// generic silent-fallback routing would lie about which substrate produced
/// the numbers.
#[cfg(unix)]
enum EvalDial {
    /// The daemon answered with a usable report (computed on its resident,
    /// memory-cached index; `index_substrate = "resident"`).
    Answered(Box<EvalOutput>),
    /// No daemon, a transient reject, or a pre-booking transport failure — run
    /// the eval CLI-side on a fresh cold index (`index_substrate = "fresh-built"`).
    /// `reason` is surfaced in the one-line notice.
    RunLocal { reason: String },
    /// The daemon read the request (booked the run) but its response was lost
    /// to a read-budget expiry — it is likely STILL RUNNING the eval. Rerunning
    /// inline would put a second GPU/ORT session on the same run, so this is a
    /// hard error, never a silent local rerun.
    BookedButLost(String),
}

/// Acquire the eval report — relay to the resident daemon when eligible, else
/// compute a fresh cold build CLI-side. Honest outcomes only: an absent/busy
/// daemon prints one notice and runs local; a booked-but-lost run is a hard
/// error (see [`EvalDial`]).
fn obtain_report(ctx: &CommandContext<'_, ReadOnly>, args: &EvalCmdArgs) -> Result<EvalOutput> {
    #[cfg(unix)]
    if let Some(canonical) = eval_relay_target(ctx, args) {
        let wire = build_wire_args(args, &canonical);
        match dial_daemon_eval(&ctx.project_cqs_dir, &wire) {
            EvalDial::Answered(report) => return Ok(*report),
            EvalDial::BookedButLost(msg) => {
                let budget =
                    cqs::daemon_translate::resolve_daemon_timeout_for_command("eval").as_secs();
                anyhow::bail!(
                    "the watch daemon booked this eval but its response was lost after the \
                     {budget}s relay budget — it is most likely STILL RUNNING the eval. \
                     Do NOT rerun (a second run would double up on the GPU); wait for it to \
                     finish, or run with CQS_NO_DAEMON=1 to force a fresh CLI-side build. ({msg})"
                );
            }
            EvalDial::RunLocal { reason } => {
                eprintln!(
                    "[eval] daemon unavailable ({reason}) — running CLI-side on a fresh-built index"
                );
            }
        }
    }

    run_eval_local(ctx, args)
}

/// Compute the eval CLI-side against a freshly-built cold index. Reads + parses
/// the fixture with friendly, path-echoing errors (the CLI is a trusted local
/// surface, unlike the daemon path guard) and drives the shared [`eval_core`]
/// with `index_substrate = "fresh-built"`.
fn run_eval_local(ctx: &CommandContext<'_, ReadOnly>, args: &EvalCmdArgs) -> Result<EvalOutput> {
    let raw = std::fs::read_to_string(&args.query_file)
        .with_context(|| format!("Failed to read query file: {}", args.query_file.display()))?;
    let set: QuerySet = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse query JSON: {}", args.query_file.display()))?;

    let params = EvalCoreParams {
        query_file_label: args.query_file.display().to_string(),
        category_filter: args.category.clone(),
        limit: args.limit,
        reranker: args.reranker,
        substrate: IndexSubstrate::FreshBuilt,
        // The CLI ran its own `--require-fresh` gate above; freshness metadata
        // is a daemon/MCP-surface concern (that surface does NOT block), so the
        // CLI report omits it.
        freshness: None,
    };
    eval_core(ctx, &set, &params)
}

/// Build the wire `arguments` object the daemon deserializes into
/// [`runner::EvalArgs`]. Only the four wire knobs ride — `--save` /
/// `--baseline` / `--tolerance` / freshness are adapter-owned post-processing.
/// `canonical` is the absolute canonicalized fixture path so the daemon (whose
/// cwd may differ from the client's) resolves the same file the CLI validated.
#[cfg(unix)]
fn build_wire_args(args: &EvalCmdArgs, canonical: &Path) -> serde_json::Value {
    serde_json::json!({
        "query_file": canonical.to_string_lossy(),
        "limit": args.limit,
        "category": args.category,
        "reranker": reranker_wire_str(args.reranker),
    })
}

/// Stable wire spelling for the reranker mode (matches the clap `ValueEnum` and
/// the `#[serde(rename_all = "lowercase")]` on `RerankerMode`). `RerankerMode`
/// derives only `Deserialize`, so it is mapped explicitly here rather than
/// serialized.
#[cfg(unix)]
fn reranker_wire_str(mode: RerankerMode) -> &'static str {
    match mode {
        RerankerMode::None => "none",
        RerankerMode::Onnx => "onnx",
    }
}

/// The canonical fixture path to relay to the resident daemon, or `None` to run
/// CLI-side.
///
/// Off the daemon fast path when a slot is pinned (the daemon serves its own
/// active slot, not the requested one), when `CQS_NO_DAEMON=1`, or when the
/// fixture lives outside the served project root (the daemon path guard would
/// reject it — the operator has legitimate local access, so run it CLI-side). A
/// non-existent fixture (canonicalize fails) reads as out-of-root so the local
/// path surfaces the friendly `Failed to read query file` error rather than the
/// daemon's opaque rejection. Returns the canonical absolute path so the wire
/// frame names the exact file the CLI validated.
#[cfg(unix)]
fn eval_relay_target(ctx: &CommandContext<'_, ReadOnly>, args: &EvalCmdArgs) -> Option<PathBuf> {
    if ctx.cli.slot.is_some() {
        return None;
    }
    if std::env::var("CQS_SLOT")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return None;
    }
    if std::env::var("CQS_NO_DAEMON").as_deref() == Ok("1") {
        return None;
    }
    let canonical = dunce::canonicalize(&args.query_file).ok()?;
    let root_canonical = dunce::canonicalize(&ctx.root).unwrap_or_else(|_| ctx.root.clone());
    canonical.starts_with(&root_canonical).then_some(canonical)
}

/// Dial the daemon's `eval` handler over the JSON-args socket frame and
/// classify the outcome. The eval relay budget (`max(40 × base, 20 min)`) is
/// applied automatically by `resolve_daemon_timeout_for_command("eval")` inside
/// the request helper.
#[cfg(unix)]
fn dial_daemon_eval(cqs_dir: &Path, wire_args: &serde_json::Value) -> EvalDial {
    use cqs::daemon_translate::{classify_slim_envelope, daemon_json_args_request, SlimEnvelope};

    match daemon_json_args_request(cqs_dir, "eval", wire_args) {
        Ok(envelope) => {
            let Some(output) = envelope.get("output") else {
                return EvalDial::RunLocal {
                    reason: "daemon response missing `output`".to_string(),
                };
            };
            match classify_slim_envelope(output) {
                Some(SlimEnvelope::Data { payload, .. }) => {
                    match serde_json::from_value::<EvalOutput>(payload.clone()) {
                        Ok(report) => EvalDial::Answered(Box::new(report)),
                        Err(e) => EvalDial::RunLocal {
                            reason: format!("daemon report unparseable ({e})"),
                        },
                    }
                }
                // A handler error (single-flight busy, a path rejection, ...) is
                // a fast pre-embedder failure — the daemon booked no long run, so
                // recomputing locally is safe and honest.
                Some(SlimEnvelope::Error { code, message }) => EvalDial::RunLocal {
                    reason: format!("daemon declined [{code}]: {message}"),
                },
                None => EvalDial::RunLocal {
                    reason: "daemon returned an unrecognized envelope".to_string(),
                },
            }
        }
        Err(e) => classify_dial_error(e),
    }
}

/// Map a daemon RPC error onto the honest eval outcome. Only a read-budget
/// expiry (`Timeout`) is booked-but-lost — the daemon read the request and is
/// still running the eval, so rerunning inline would double up on the GPU.
/// Every other error is a pre-booking failure (no socket, connect/write failed,
/// a socket-level reject), safe to recompute locally.
#[cfg(unix)]
fn classify_dial_error(e: cqs::daemon_translate::DaemonRpcError) -> EvalDial {
    use cqs::daemon_translate::DaemonRpcError;
    match e {
        DaemonRpcError::Timeout(msg) => EvalDial::BookedButLost(msg),
        other => EvalDial::RunLocal {
            reason: other.to_string(),
        },
    }
}

/// Consult the watch daemon and block until the index is fresh, or bail with
/// an actionable error.
///
/// Resolution order, lowest precedence first:
/// 1. Default: gate is **on**.
/// 2. `CQS_EVAL_REQUIRE_FRESH=0` (or `false`/`no`/`off`) in the environment
///    disables the gate.
/// 3. `--no-require-fresh` on the CLI disables the gate (always wins).
///
/// The strict default is the load-bearing piece: forgetting `cqs index`
/// after a branch switch would otherwise silently produce a 5-25pp R@K
/// shift that looks identical to a real regression. The escape hatch is
/// the documented path for offline runs where no daemon is available.
fn require_fresh_gate(no_require_fresh_flag: &bool, wait_secs: u64) -> Result<()> {
    let _span = tracing::info_span!("require_fresh_gate", wait_secs).entered();
    let start = std::time::Instant::now();

    if *no_require_fresh_flag {
        tracing::info!(
            outcome = "bypass_flag",
            "require_fresh_gate: disabled via --no-require-fresh",
        );
        return Ok(());
    }
    if env_disables_freshness_gate() {
        tracing::info!(
            outcome = "bypass_env",
            "require_fresh_gate: disabled via CQS_EVAL_REQUIRE_FRESH",
        );
        eprintln!(
            "[eval] CQS_EVAL_REQUIRE_FRESH disables the freshness gate; running against current index"
        );
        return Ok(());
    }

    #[cfg(unix)]
    {
        use cqs::daemon_translate::FreshnessWait;
        let root = crate::cli::find_project_root();
        let cqs_dir = cqs::resolve_index_dir(&root);
        // The cap is env-overridable via `CQS_EVAL_FRESH_BUDGET_CEILING`
        // (default 600 s). On a slow indexer doing a fresh full-reindex of a
        // 100k-chunk repo, embedder warmup + index build can exceed 10 min;
        // an operator can lift the ceiling without source edits. The
        // `wait_for_fresh` defense-in-depth at 86_400 s still bounds the
        // absolute upper limit. Garbage / zero / unparseable env values fall
        // back to 600.
        let ceiling = std::env::var("CQS_EVAL_FRESH_BUDGET_CEILING")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(600);
        let budget_secs = if wait_secs > ceiling {
            tracing::warn!(
                requested = wait_secs,
                capped = ceiling,
                "--require-fresh-secs capped at {ceiling} s (CQS_EVAL_FRESH_BUDGET_CEILING)",
            );
            eprintln!(
                "[eval] --require-fresh-secs={wait_secs} capped at {ceiling} s \
                 (CQS_EVAL_FRESH_BUDGET_CEILING); continuing with {ceiling} s budget"
            );
            ceiling
        } else {
            wait_secs
        };

        // Friendly heads-up on stderr so a long wait doesn't look like a hang.
        // Mirrors the ergonomic of `cargo build` printing "Compiling ..." —
        // the user wants to know something is happening.
        eprintln!(
            "[eval] checking watch-mode freshness (--no-require-fresh to skip; CQS_EVAL_REQUIRE_FRESH=0 in env)"
        );

        let result = cqs::daemon_translate::wait_for_fresh(&cqs_dir, budget_secs);
        let elapsed_ms = start.elapsed().as_millis() as u64;
        match result {
            FreshnessWait::Fresh(snap) => {
                tracing::info!(
                    outcome = "fresh",
                    elapsed_ms,
                    modified_files = snap.modified_files,
                    "require_fresh_gate: resolved",
                );
                Ok(())
            }
            FreshnessWait::Timeout(snap) => {
                tracing::info!(
                    outcome = "timeout",
                    elapsed_ms,
                    modified_files = snap.modified_files,
                    "require_fresh_gate: resolved",
                );
                anyhow::bail!(
                    "watch index is still stale after {budget_secs}s wait \
                     (modified_files={}, pending_notes={}, rebuild_in_flight={}, \
                     dropped_this_cycle={}, delta_saturated={}); \
                     wait longer with --require-fresh-secs N or skip with --no-require-fresh",
                    snap.modified_files,
                    snap.pending_notes,
                    snap.rebuild_in_flight,
                    snap.dropped_this_cycle,
                    snap.delta_saturated,
                )
            }
            FreshnessWait::NoDaemon(msg) => {
                tracing::info!(
                    outcome = "no_daemon",
                    elapsed_ms,
                    "require_fresh_gate: resolved",
                );
                anyhow::bail!(
                    "watch daemon not reachable: {msg}\n\
                     \n\
                     Eval --require-fresh requires a running `cqs watch --serve`. Either:\n  \
                       - start the daemon (`{start}`)\n  \
                       - rerun with `--no-require-fresh` for an offline check\n  \
                       - export `CQS_EVAL_REQUIRE_FRESH=0` to disable the gate for this shell\n\n\
                     NOTE: if you upgraded from <v1.30.1, the daemon socket name changed (BLAKE3); \
                     run `{restart}` so the daemon binds the new path.",
                    start = daemon_control_hint(DaemonHint::Start),
                    restart = daemon_control_hint(DaemonHint::Restart),
                )
            }
            // Transport — socket exists but daemon isn't responding (hung,
            // crashed mid-call, or set_*_timeout failed).
            FreshnessWait::Transport(msg) => {
                tracing::info!(
                    outcome = "transport",
                    elapsed_ms,
                    "require_fresh_gate: resolved",
                );
                anyhow::bail!(
                    "watch daemon transport error: {msg}\n\
                     \n\
                     The daemon socket exists but isn't responding. The daemon may be \
                     hung or crashed mid-call. Try:\n  \
                       - check the daemon log (`journalctl --user -u cqs-watch -n 50` on Linux)\n  \
                       - restart the daemon (`{restart}`)\n  \
                       - rerun with `--no-require-fresh` to skip the gate",
                    restart = daemon_control_hint(DaemonHint::Restart),
                )
            }
            // BadResponse — daemon replied but the envelope was unparseable.
            // Most often a CLI/daemon version skew.
            FreshnessWait::BadResponse(msg) => {
                tracing::info!(
                    outcome = "bad_response",
                    elapsed_ms,
                    "require_fresh_gate: resolved",
                );
                anyhow::bail!(
                    "watch daemon returned a malformed response: {msg}\n\
                     \n\
                     The daemon answered but the response was unparseable — likely a \
                     CLI/daemon version skew. Restart the daemon after rebuilding so it \
                     speaks the same protocol as this CLI:\n  \
                       - `{reinstall}`\n  \
                       - rerun with `--no-require-fresh` to skip the gate",
                    reinstall = daemon_control_hint(DaemonHint::Reinstall),
                )
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = wait_secs;
        tracing::info!(
            outcome = "non_unix",
            elapsed_ms = start.elapsed().as_millis() as u64,
            "require_fresh_gate: resolved",
        );
        anyhow::bail!(
            "watch-mode freshness gate is unix-only (daemon socket uses Unix domain sockets); \
             rerun with --no-require-fresh on this platform"
        );
    }
}

/// Read `CQS_EVAL_REQUIRE_FRESH` and decide whether the env var disables
/// the gate. Truthy / unset = gate stays on; falsy = gate off. The list
/// of falsy strings mirrors the convention used by other env-var knobs
/// (`CQS_NO_DAEMON`, etc.) so an operator who knows one knob's spelling
/// gets the other for free.
///
/// Falsy-string parsing lives in [`cqs::env_falsy`] so the spelling list is
/// shared across all such knobs.
fn env_disables_freshness_gate() -> bool {
    std::env::var("CQS_EVAL_REQUIRE_FRESH")
        .map(|v| cqs::env_falsy(&v))
        .unwrap_or(false)
}

/// Print the eval report in human-readable text.
///
/// Refuses to emit the report when the fixture has zero queries with
/// `gold_chunk`. Without this guard, `pct(0.0)` (what runner emits for the
/// empty case) prints `"  0.0%"` for every metric — looks like a real result,
/// but it's a structural zero, not a signal. Exits 2 so a downstream
/// `cqs eval | grep R@5` chain notices.
fn print_text_report(report: &EvalOutput) {
    if report.overall.n == 0 {
        eprintln!(
            "[eval] no queries with gold_chunk in {}; refusing to emit report \
             (skipped={}, total queries seen={})",
            report.query_file, report.skipped, report.query_count,
        );
        std::process::exit(2);
    }
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    // Stdout failures during a CLI tool's print step are unrecoverable;
    // surface as a panic so the operator sees the broken pipe / disk-full
    // condition rather than a silent zero-byte report.
    write_text_report(&mut handle, report)
        .expect("write_text_report must not fail on stdout — broken pipe / disk full?");
}

/// Writable-sink variant of `print_text_report` so a unit test can pin the
/// exact format without capturing process stdout. Caller is responsible for
/// the empty-fixture guard — this writer assumes the report is publishable
/// (`overall.n > 0`).
fn write_text_report<W: std::io::Write>(w: &mut W, report: &EvalOutput) -> std::io::Result<()> {
    writeln!(
        w,
        "=== eval results: {} (N={}) ===",
        report.query_file, report.overall.n
    )?;
    writeln!(
        w,
        "OVERALL: R@1={}  R@5={}  R@20={}",
        pct(report.overall.r_at_1),
        pct(report.overall.r_at_5),
        pct(report.overall.r_at_20)
    )?;
    if report.skipped > 0 {
        writeln!(w, "(skipped {} queries with no gold_chunk)", report.skipped)?;
    }
    writeln!(w)?;

    if !report.by_category.is_empty() {
        writeln!(
            w,
            "{:<24} {:>5} {:>7} {:>7} {:>7}",
            "category", "N", "R@1", "R@5", "R@20"
        )?;
        for (cat, stats) in &report.by_category {
            writeln!(
                w,
                "{:<24} {:>5} {:>7} {:>7} {:>7}",
                cat,
                stats.n,
                pct(stats.r_at_1),
                pct(stats.r_at_5),
                pct(stats.r_at_20),
            )?;
        }
        writeln!(w)?;
    }

    writeln!(
        w,
        "(eval took {:.1}s, {:.1} queries/sec, model={})",
        report.elapsed_secs, report.queries_per_sec, report.index_model
    )?;
    Ok(())
}

/// Format a fraction in [0.0, 1.0] as a percentage with one decimal place,
/// e.g. 0.4220 → "42.2%".
fn pct(x: f64) -> String {
    format!("{:>5.1}%", x * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The booked-but-lost discriminator: ONLY a read-budget expiry (`Timeout`)
    /// means the daemon booked the run and is still executing it — every other
    /// RPC error is a pre-booking failure safe to recompute locally. Guards the
    /// no-double-GPU rule without needing a live daemon.
    #[cfg(unix)]
    #[test]
    fn dial_timeout_is_booked_but_lost_everything_else_is_local() {
        use cqs::daemon_translate::DaemonRpcError;
        assert!(
            matches!(
                classify_dial_error(DaemonRpcError::Timeout("budget".into())),
                EvalDial::BookedButLost(_)
            ),
            "a read-budget expiry must be a hard error (daemon still running)"
        );
        for e in [
            DaemonRpcError::SocketMissing("s".into()),
            DaemonRpcError::Transport("t".into()),
            DaemonRpcError::DaemonError("d".into()),
            DaemonRpcError::BadResponse("b".into()),
            DaemonRpcError::ResponseTooLarge("r".into()),
        ] {
            assert!(
                matches!(classify_dial_error(e), EvalDial::RunLocal { .. }),
                "a pre-booking failure must fall back to a local run"
            );
        }
    }

    /// `pct` formats fractions consistently across the spectrum so the
    /// text report aligns in columns.
    #[test]
    fn test_pct_formatting() {
        assert_eq!(pct(0.0), "  0.0%");
        assert_eq!(pct(0.422), " 42.2%");
        assert_eq!(pct(1.0), "100.0%");
        assert_eq!(pct(0.5), " 50.0%");
    }

    /// Validate args: --limit 0 must fail before running anything.
    /// We can't construct a `CommandContext` here without an indexed store,
    /// so the limit guard is implicitly tested via the cmd_eval entry —
    /// integration tests in tests/eval_subcommand_test.rs cover the live path.
    #[test]
    fn test_args_default_limit_is_20() {
        // Mirror clap's default_value
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        let w = Wrapper::try_parse_from(["test", "queries.json"]).unwrap();
        assert_eq!(w.args.limit, 20);
        assert!(!w.args.json);
        assert!(w.args.category.is_none());
        assert!(w.args.save.is_none());
        assert!(w.args.baseline.is_none());
        assert!((w.args.tolerance - 1.0).abs() < 1e-9);
        // Gate is on by default — no_require_fresh stays false.
        assert!(!w.args.no_require_fresh);
        assert_eq!(w.args.require_fresh_secs, 600);
    }

    /// Parser accepts `--no-require-fresh` and a custom budget.
    #[test]
    fn test_no_require_fresh_flag_parses() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        let w = Wrapper::try_parse_from([
            "test",
            "queries.json",
            "--no-require-fresh",
            "--require-fresh-secs",
            "30",
        ])
        .unwrap();
        assert!(w.args.no_require_fresh);
        assert_eq!(w.args.require_fresh_secs, 30);
    }

    /// Env-var falsy values disable the gate. Drives the actual
    /// `env_disables_freshness_gate` helper so the helper's `matches!`
    /// pattern is what gets covered (re-implementing the logic inline would
    /// leave the function itself untested and bypass drift invisible).
    ///
    /// Coverage extends to whitespace-trimming, unset (= gate stays on),
    /// and unknown / garbage values (= gate stays on by default — only
    /// the canonical `0|false|no|off` shutoffs are honored).
    ///
    /// `#[serial_test::serial]` is required: this test mutates the
    /// process-wide `CQS_EVAL_REQUIRE_FRESH` env var, and parallel tests
    /// reading the same var would race.
    #[test]
    #[serial_test::serial(cqs_eval_require_fresh_env)]
    fn env_disables_freshness_gate_recognises_falsy_strings() {
        let saved = std::env::var("CQS_EVAL_REQUIRE_FRESH").ok();

        // Unset: gate stays on.
        // SAFETY: serial_test guards env mutation; no other thread is
        // touching CQS_EVAL_REQUIRE_FRESH while this test runs.
        unsafe {
            std::env::remove_var("CQS_EVAL_REQUIRE_FRESH");
        }
        assert!(
            !env_disables_freshness_gate(),
            "unset CQS_EVAL_REQUIRE_FRESH must leave the gate on"
        );

        let cases: &[(&str, bool)] = &[
            // Canonical falsy spellings.
            ("0", true),
            ("false", true),
            ("FALSE", true),
            ("no", true),
            ("off", true),
            // Whitespace trimming covered by the helper.
            ("  off  ", true),
            ("\toff\n", true),
            // Truthy spellings keep the gate on.
            ("1", false),
            ("true", false),
            ("yes", false),
            // Empty / unknown / garbage all leave the gate on.
            ("", false),
            ("garbage", false),
            ("maybe", false),
            ("2", false),
        ];
        for (input, expected) in cases {
            // SAFETY: serial_test guards env mutation; no other thread is
            // touching CQS_EVAL_REQUIRE_FRESH while this test runs.
            unsafe {
                std::env::set_var("CQS_EVAL_REQUIRE_FRESH", input);
            }
            let observed = env_disables_freshness_gate();
            assert_eq!(
                observed, *expected,
                "input {input:?} should disable={expected}"
            );
        }

        // SAFETY: same as above — restore prior state.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("CQS_EVAL_REQUIRE_FRESH", v),
                None => std::env::remove_var("CQS_EVAL_REQUIRE_FRESH"),
            }
        }
    }

    /// Drive `require_fresh_gate` via the `--no-require-fresh` flag
    /// short-circuit. No daemon needed because the flag bypass returns
    /// `Ok(())` before any socket I/O.
    #[test]
    fn require_fresh_gate_no_require_fresh_flag_returns_ok_without_daemon() {
        let result = require_fresh_gate(&true, 5);
        assert!(
            result.is_ok(),
            "--no-require-fresh must short-circuit to Ok, got: {result:?}"
        );
    }

    /// Drive `require_fresh_gate` via the `CQS_EVAL_REQUIRE_FRESH=0` env-var
    /// bypass. Pins the resolution order — env var disables the gate even
    /// when the CLI flag is absent.
    #[test]
    #[serial_test::serial(cqs_eval_require_fresh_env)]
    fn require_fresh_gate_env_disable_returns_ok_without_daemon() {
        let saved = std::env::var("CQS_EVAL_REQUIRE_FRESH").ok();
        // SAFETY: serial_test guards env mutation.
        unsafe {
            std::env::set_var("CQS_EVAL_REQUIRE_FRESH", "0");
        }
        let result = require_fresh_gate(&false, 5);
        // SAFETY: same as above — restore prior state.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("CQS_EVAL_REQUIRE_FRESH", v),
                None => std::env::remove_var("CQS_EVAL_REQUIRE_FRESH"),
            }
        }
        assert!(
            result.is_ok(),
            "CQS_EVAL_REQUIRE_FRESH=0 must short-circuit to Ok, got: {result:?}"
        );
    }

    #[test]
    fn test_args_parse_all_flags() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        let w = Wrapper::try_parse_from([
            "test",
            "queries.json",
            "--json",
            "--limit",
            "50",
            "--category",
            "structural_search",
            "--save",
            "out.json",
            "--baseline",
            "base.json",
            "--tolerance",
            "2.5",
        ])
        .unwrap();
        assert!(w.args.json);
        assert_eq!(w.args.limit, 50);
        assert_eq!(w.args.category.as_deref(), Some("structural_search"));
        assert_eq!(w.args.save.unwrap().to_str().unwrap(), "out.json");
        assert_eq!(w.args.baseline.unwrap().to_str().unwrap(), "base.json");
        assert!((w.args.tolerance - 2.5).abs() < 1e-9);
        // When not passed, the gate stays on by default even alongside every
        // other flag.
        assert!(!w.args.no_require_fresh);
        assert_eq!(w.args.require_fresh_secs, 600);
        // Default reranker mode is None — retrieval-only pipeline.
        assert_eq!(w.args.reranker, RerankerMode::None);
    }

    /// Pin the `--reranker` flag's three accepted values. Default is
    /// already covered in `test_args_default_limit_is_20`; here we drive
    /// every variant to catch typos in clap's value-enum derivation.
    #[test]
    fn reranker_flag_parses_each_variant() {
        use clap::Parser;
        #[derive(Parser)]
        struct Wrapper {
            #[command(flatten)]
            args: EvalCmdArgs,
        }
        for (input, expected) in [("none", RerankerMode::None), ("onnx", RerankerMode::Onnx)] {
            let w = Wrapper::try_parse_from(["test", "queries.json", "--reranker", input]).unwrap();
            assert_eq!(
                w.args.reranker, expected,
                "--reranker {input} should parse to {expected:?}"
            );
        }
        // Garbage values must error — covers the contract that clap rejects
        // unknown reranker spellings instead of silently falling back to None.
        let err = Wrapper::try_parse_from(["test", "queries.json", "--reranker", "fancy"]);
        assert!(err.is_err(), "--reranker fancy should be a clap error");
    }

    /// Pin the canonical row-by-row format so a downstream regex-based parser
    /// doesn't break silently when someone reorders columns or drops a label.
    /// Builds a deterministic report with two queries (1 hit at R@1, both at
    /// R@5) and asserts every expected substring on the output.
    #[test]
    fn print_text_report_renders_canonical_header_and_metrics() {
        use super::runner::{CategoryStats, EvalOutput, Overall};
        use std::collections::BTreeMap;

        let mut by_category = BTreeMap::new();
        by_category.insert(
            "structural_search".to_string(),
            CategoryStats {
                n: 2,
                r_at_1: 0.5,
                r_at_5: 1.0,
                r_at_20: 1.0,
            },
        );

        let report = EvalOutput {
            query_count: 2,
            skipped: 0,
            elapsed_secs: 1.5,
            queries_per_sec: 1.33,
            overall: Overall {
                n: 2,
                r_at_1: 0.5,
                r_at_5: 1.0,
                r_at_20: 1.0,
            },
            by_category,
            index_model: "BAAI/bge-large-en-v1.5".to_string(),
            cqs_version: "1.30.1".to_string(),
            query_file: "fixture.json".to_string(),
            limit: 20,
            category_filter: None,
            index_substrate: IndexSubstrate::FreshBuilt.as_str().to_string(),
            freshness: None,
        };

        let mut buf = Vec::new();
        write_text_report(&mut buf, &report).expect("write_text_report");
        let out = String::from_utf8(buf).expect("UTF-8");

        // Header row carries the fixture name and N.
        assert!(
            out.contains("=== eval results: fixture.json (N=2) ==="),
            "header row missing or reformatted: {out}"
        );
        // OVERALL line — pct() formats with leading space and one decimal.
        assert!(
            out.contains("OVERALL: R@1= 50.0%  R@5=100.0%  R@20=100.0%"),
            "OVERALL line missing or reformatted: {out}"
        );
        // Per-category table header row.
        assert!(
            out.contains("category"),
            "category column header missing: {out}"
        );
        assert!(out.contains("R@1"), "R@1 column header missing: {out}");
        assert!(out.contains("R@5"), "R@5 column header missing: {out}");
        assert!(out.contains("R@20"), "R@20 column header missing: {out}");
        // Category row.
        assert!(
            out.contains("structural_search"),
            "category row missing: {out}"
        );
        // Footer with elapsed + qps + model.
        assert!(
            out.contains("(eval took 1.5s, 1.3 queries/sec, model=BAAI/bge-large-en-v1.5)"),
            "footer line missing or reformatted: {out}"
        );
    }
}
