//! Analysis dispatch handlers: dead, health, stale, suggest, review, ci.
//!
//! Handlers take a single `&XArgs` argument so the macro-driven
//! `BatchCmd::dispatch` calls every row uniformly.

use anyhow::Result;

use super::super::BatchView;
use crate::cli::args::{CiArgs, DeadArgs, ReviewArgs, StaleArgs, SuggestArgs};

/// Identifies and reports dead code in a codebase.
/// Analyzes code to find functions that are never called, filtering results based on confidence level and visibility. Returns structured JSON containing categorized dead code findings.
/// # Arguments
/// * `ctx` - Batch context containing the code store and root directory path
/// * `include_pub` - Whether to include public functions in the dead code analysis
/// * `min_confidence` - Minimum confidence threshold for including results
/// # Returns
/// A JSON object with four fields:
/// - `dead`: Array of confidently identified dead functions
/// - `possibly_dead_pub`: Array of possibly dead public functions
/// - `count`: Count of confidently dead functions
/// - `possibly_pub_count`: Count of possibly dead public functions
/// Each function entry includes name, file path, line range, type, signature, language, and confidence level.
/// # Errors
/// Returns an error if the code store query fails.
pub(in crate::cli::batch) fn dispatch_dead(
    ctx: &BatchView,
    args: &DeadArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_dead").entered();

    // Thin adapter over the shared `dead_core` — identical JSON shape across
    // the CLI and daemon surfaces.
    let core_args = crate::cli::commands::DeadArgs {
        include_pub: args.include_pub,
        min_confidence: args.min_confidence,
    };
    let output = crate::cli::commands::dead_core(&ctx.store(), &ctx.root, &core_args)?;
    Ok(serde_json::to_value(&output)?)
}

/// Dispatches a request to identify stale and missing files in the batch store.
/// Retrieves the file set from the batch context and queries the store for files whose modification times have changed or are no longer present on disk. Returns a JSON report containing lists of stale files with their stored and current modification times, missing files, and summary statistics.
/// # Arguments
/// * `ctx` - The batch context containing the store and file set information.
/// * `count_only` - If true, emits only count fields (stale_count, missing_count,
///   total_indexed), omitting the per-file arrays. Matches the CLI `--count-only`
///   flag so forwarded invocations parse cleanly.
/// # Returns
/// A JSON object containing:
/// - `stale`: Array of stale files with their origin path, stored mtime, and current mtime (omitted if `count_only`)
/// - `missing`: Array of missing file paths (omitted if `count_only`)
/// - `total_indexed`: Total number of indexed files
/// - `stale_count`: Count of stale files
/// - `missing_count`: Count of missing files
/// # Errors
/// Returns an error if the file set cannot be retrieved from the context or if the store query fails.
pub(in crate::cli::batch) fn dispatch_stale(
    ctx: &BatchView,
    args: &StaleArgs,
) -> Result<serde_json::Value> {
    let count_only = args.count_only;
    let _span = tracing::info_span!("batch_stale", count_only).entered();

    // `file_set` is `Arc<HashSet<PathBuf>>` — the daemon keeps it cached so we
    // avoid re-enumerating the tree on every probe. `stale_core` takes the set
    // by ref so this hot path reuses the cache (the CLI enumerates once).
    let file_set = ctx.file_set()?;
    let output = crate::cli::commands::stale_core(
        &ctx.store(),
        &ctx.root,
        &file_set,
        &crate::cli::commands::StaleArgs { count_only },
    )?;
    if count_only {
        Ok(serde_json::json!({
            "stale_count": output.stale_count,
            "missing_count": output.missing_count,
            "total_indexed": output.total_indexed,
        }))
    } else {
        Ok(serde_json::to_value(&output)?)
    }
}

/// Performs a health check on the batch processing system and returns the results as JSON.
/// This function executes a comprehensive health check that validates the store, file set, and CQS directory, then serializes the health report to a JSON value for reporting purposes.
/// # Arguments
/// * `ctx` - The batch processing context containing the store, file set, and CQS directory paths.
/// # Returns
/// A `Result` containing a `serde_json::Value` representing the health check report, or an error if the health check fails or serialization fails.
/// # Errors
/// Returns an error if retrieving the file set fails, if the health check itself fails, or if serializing the report to JSON fails.
pub(in crate::cli::batch) fn dispatch_health(ctx: &BatchView) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_health").entered();

    // `Arc<HashSet>` auto-derefs through `&file_set` for the borrowed-only
    // callee. Thin adapter over the shared `health_core`.
    let file_set = ctx.file_set()?;
    let report = crate::cli::commands::health_core(
        &ctx.store(),
        &file_set,
        &ctx.cqs_dir,
        &ctx.root,
        &crate::cli::commands::HealthArgs::default(),
    )?;

    Ok(serde_json::to_value(&report)?)
}

/// Suggests notes from codebase patterns. `--apply` is rejected on the
/// daemon path.
///
/// The daemon holds a `Store<ReadOnly>`, so the reindex half of
/// `--apply` (which goes through `index_notes` → `replace_notes_for_file`)
/// cannot compile here. `Commands::Suggest` is classified as
/// `BatchSupport::Cli` precisely because of this — a user who runs
/// `cqs suggest --apply` falls through to the CLI path, which opens a
/// `Store<ReadWrite>`. Bailing when `apply=true` documents that
/// invariant instead of silently producing a stale (unapplied) result.
pub(in crate::cli::batch) fn dispatch_suggest(
    ctx: &BatchView,
    args: &SuggestArgs,
) -> Result<serde_json::Value> {
    let apply = args.apply;
    let _span = tracing::info_span!("batch_suggest", apply).entered();

    if apply {
        anyhow::bail!(
            "suggest --apply requires a writable store; run `cqs suggest --apply` outside \
             the daemon. (Commands::Suggest is BatchSupport::Cli; reaching this branch \
             means a classifier regressed — see #946.)"
        );
    }

    // Thin adapter over the shared `suggest_core` — converges the daemon JSON
    // onto the CLI's typed `SuggestOutput` schema (`count` replaces the old
    // `total`; per-entry shape unchanged).
    let output = crate::cli::commands::suggest_core(
        &ctx.store(),
        &ctx.root,
        &crate::cli::commands::SuggestArgs::default(),
    )?;
    Ok(serde_json::to_value(&output)?)
}

/// Runs a diff-aware review and returns results as JSON.
/// Executes `git diff` against the given base ref (or HEAD) and runs the
/// review pipeline: diff impact, risk scoring, note matching, staleness.
pub(in crate::cli::batch) fn dispatch_review(
    ctx: &BatchView,
    args: &ReviewArgs,
) -> Result<serde_json::Value> {
    let base = args.base.as_deref();
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_review", ?base).entered();

    // Thin adapter over the shared `review_core` — same schema + budgeting as
    // the CLI JSON surface. Adapter owns diff I/O (git only, no stdin).
    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let output = crate::cli::commands::review_core(
        &ctx.store(),
        &ctx.root,
        &diff_text,
        &crate::cli::commands::ReviewArgs { tokens },
    )?;
    Ok(serde_json::to_value(&output)?)
}

/// Runs CI analysis (review + dead code + gate) and returns results as JSON.
/// Note: In batch mode, gate failure is reported in the JSON output rather than
/// causing a process exit, since the batch session must continue.
pub(in crate::cli::batch) fn dispatch_ci(
    ctx: &BatchView,
    args: &CiArgs,
) -> Result<serde_json::Value> {
    let base = args.base.as_deref();
    let gate = args.gate;
    let tokens = args.tokens;
    let _span = tracing::info_span!("batch_ci", ?gate).entered();

    // Thin adapter over the shared `ci_core` — same schema + budgeting as the
    // CLI JSON surface. Gate failure is reported in the JSON (no exit) because
    // the batch session must continue. Adapter owns diff I/O (git only).
    let diff_text = crate::cli::commands::run_git_diff(base)?;
    let output = crate::cli::commands::ci_core(
        &ctx.store(),
        &ctx.root,
        &diff_text,
        gate,
        &crate::cli::commands::CiArgs { tokens },
    )?;
    Ok(serde_json::to_value(&output)?)
}

// ---------------------------------------------------------------------------
// Parity tests — each `dispatch_*` is a thin adapter over the shared `*_core`,
// so the daemon JSON must be byte-equal to `serde_json::to_value(*_core(...))`
// for the same inputs. dead/health/suggest are embedder-free and git-free
// (they read the store directly), so the full dispatch path is exercised.
// review/ci acquire their diff via `run_git_diff` (needs a real repo with a
// diff), so their core-equivalence is asserted at the core level against the
// empty-diff convergence shape; the dispatchers are parity-by-construction
// (they literally call the core with `run_git_diff` output then `to_value`).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod parity_tests {
    use crate::cli::batch::create_test_context;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::{DeadConfidence, ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_chunk(id: &str, name: &str) -> Chunk {
        let content = format!("fn {name}() {{ }}");
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from("src/lib.rs"),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: format!("fn {name}()"),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash,
            canonical_hash: String::new(),
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    fn seed_minimal_ctx() -> (TempDir, crate::cli::batch::BatchContext) {
        let dir = TempDir::new().expect("tempdir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("mkdir .cqs");
        let index_path = cqs_dir.join(cqs::INDEX_DB_FILENAME);

        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("open store");
            store.init(&ModelInfo::default()).expect("init");
            let chunks = vec![(make_chunk("src/lib.rs:1:foo", "foo"), embedding)];
            store.upsert_chunks_batch(&chunks, Some(0)).expect("upsert");
        }
        let ctx = create_test_context(&cqs_dir).expect("ctx");
        (dir, ctx)
    }

    /// `dispatch_dead` is byte-equal to `serde_json::to_value(dead_core(...))`.
    #[test]
    fn parity_dead_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let core = crate::cli::commands::dead_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
            },
        )
        .expect("dead_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        let dispatched = super::dispatch_dead(
            &view,
            &crate::cli::args::DeadArgs {
                include_pub: false,
                min_confidence: DeadConfidence::Low,
            },
        )
        .expect("dispatch_dead");

        assert_eq!(dispatched, core_val, "dispatch_dead must equal dead_core");
    }

    /// `dispatch_health` is byte-equal to `serde_json::to_value(health_core(...))`.
    #[test]
    fn parity_health_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let file_set = view.file_set().expect("file_set");
        let core = crate::cli::commands::health_core(
            &view.store(),
            &file_set,
            &view.cqs_dir,
            &view.root,
            &crate::cli::commands::HealthArgs::default(),
        )
        .expect("health_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        let dispatched = super::dispatch_health(&view).expect("dispatch_health");

        assert_eq!(
            dispatched, core_val,
            "dispatch_health must equal health_core"
        );
    }

    /// `dispatch_suggest` (apply=false) is byte-equal to
    /// `serde_json::to_value(suggest_core(...))`. Pins the daemon-side schema
    /// convergence (`count`, not the old `total`).
    #[test]
    fn parity_suggest_dispatch_equals_core() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let core = crate::cli::commands::suggest_core(
            &view.store(),
            &view.root,
            &crate::cli::commands::SuggestArgs::default(),
        )
        .expect("suggest_core");
        let core_val = serde_json::to_value(&core).expect("serialize core");

        let dispatched =
            super::dispatch_suggest(&view, &crate::cli::args::SuggestArgs { apply: false })
                .expect("dispatch_suggest");

        assert_eq!(
            dispatched, core_val,
            "dispatch_suggest must equal suggest_core (count, not total)"
        );
        // Explicit guard against the pre-unification `total` field re-appearing.
        assert!(
            dispatched.get("total").is_none(),
            "daemon suggest must use `count`, not `total`: {dispatched}"
        );
        assert!(
            dispatched.get("count").is_some(),
            "daemon suggest must carry `count`: {dispatched}"
        );
    }

    /// `review_core` on an empty diff produces the converged empty-review shape
    /// (the daemon's old hand-rolled empty case omitted `relevant_notes` /
    /// `stale_warning`; the core now emits the full CLI shape). `dispatch_review`
    /// is parity-by-construction over `run_git_diff` + this core.
    #[test]
    fn review_core_empty_diff_converged_shape() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let out = crate::cli::commands::review_core(
            &view.store(),
            &view.root,
            "",
            &crate::cli::commands::ReviewArgs { tokens: None },
        )
        .expect("review_core");
        let val = serde_json::to_value(&out).expect("serialize");

        assert!(val.get("changed_functions").is_some());
        assert!(val.get("affected_callers").is_some());
        assert!(val.get("affected_tests").is_some());
        assert!(
            val.get("relevant_notes").is_some(),
            "converged empty review carries relevant_notes: {val}"
        );
        assert_eq!(val["risk_summary"]["overall"], "low");
        // No budget fields on the empty case even though they're optional.
        assert!(val.get("token_count").is_none());
        assert!(val.get("token_budget").is_none());
    }

    /// `ci_core` on an empty diff produces a gate-passing report with no budget
    /// telemetry; `dispatch_ci` is parity-by-construction over `run_git_diff`.
    #[test]
    fn ci_core_empty_diff_shape() {
        let (_dir, ctx) = seed_minimal_ctx();
        let view = ctx.build_view(None);

        let out = crate::cli::commands::ci_core(
            &view.store(),
            &view.root,
            "",
            cqs::ci::GateThreshold::High,
            &crate::cli::commands::CiArgs { tokens: None },
        )
        .expect("ci_core");
        let val = serde_json::to_value(&out).expect("serialize");

        assert!(
            val.get("review").is_some(),
            "ci output carries review: {val}"
        );
        assert!(val.get("gate").is_some(), "ci output carries gate: {val}");
        assert!(val.get("token_count").is_none());
        assert!(val.get("token_budget").is_none());
    }
}
