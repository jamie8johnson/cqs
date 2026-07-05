//! Eval runner: load query set, run search per query, score against gold chunks.
//!
//! Reuses the production search path (`search_code_results` /
//! `search_hybrid`) so eval results match what `cqs <query>` returns. The
//! filter construction mirrors `cmd_query` in
//! `src/cli/commands/search/query.rs` — same SPLADE alpha resolution, same
//! routing decisions, same code-types default.
//!
//! ## Surface-agnostic core
//!
//! [`eval_core`] runs against a [`SearchCtx`] (the same resource surface the
//! search core uses), so both the CLI ([`crate::cli::CommandContext`]) and the
//! daemon (`BatchView`) drive it. The CLI builds a fresh cold index per call
//! (`index_substrate = "fresh-built"`); the daemon reuses its resident cached
//! index (`index_substrate = "resident"`). The read/parse of the fixture and
//! the freshness/substrate posture are adapter concerns — the core receives an
//! already-parsed [`QuerySet`] plus the surface-supplied [`EvalCoreParams`].
//!
//! Alternate-model evals go through slots (`cqs eval --slot <name>` against a
//! slot built with that model); the `SearchCtx` supplies the slot's store +
//! index, so the loop stays agnostic to which slot the index came from.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use cqs::eval::schema::{EvalQuery, GoldChunk, QuerySet};
use cqs::language::ChunkType;
use cqs::store::{ReadOnly, Store, UnifiedResult};
use cqs::SearchFilter;

use crate::cli::args::RerankerMode;
use crate::cli::commands::search::search_ctx::{SearchCtx, SpladeIndexRef};

// Wire-format types (`QuerySet`, `EvalQuery`, `GoldChunk`) live in the lib
// crate at `cqs::eval::schema` so the integration tests and Rust-side eval
// tooling share one definition.

/// Which substrate computed an eval result. Eval scores can differ across
/// substrates (HNSW reconstruction is nondeterministic), so a gate that
/// compares runs must compare same-substrate runs — the marker on
/// [`EvalOutput::index_substrate`] makes that checkable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexSubstrate {
    /// The daemon computed the result against its resident, memory-cached index.
    Resident,
    /// The CLI computed the result against a freshly-built cold index.
    FreshBuilt,
}

impl IndexSubstrate {
    /// Stable wire string for [`EvalOutput::index_substrate`].
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            IndexSubstrate::Resident => "resident",
            IndexSubstrate::FreshBuilt => "fresh-built",
        }
    }
}

/// Watch-loop freshness snapshot carried on the daemon/MCP surface, where the
/// run does NOT block on a freshness gate. Lets an MCP consumer judge how fresh
/// the index was when the numbers were computed. Absent on the CLI surface,
/// which runs its own `--require-fresh` gate before the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct EvalFreshness {
    /// Watch-loop freshness state: `fresh` | `stale` | `rebuilding` | `unknown`.
    pub state: String,
    /// Unix timestamp (UTC seconds) of the last completed reindex; absent when
    /// the index has never been synced or the mtime is unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<i64>,
}

impl EvalFreshness {
    /// Project a watch snapshot onto the eval-facing freshness shape.
    pub(crate) fn from_snapshot(snap: &cqs::watch_status::WatchSnapshot) -> Self {
        Self {
            state: snap.state.as_str().to_string(),
            last_synced_at: snap.last_synced_at,
        }
    }
}

/// Wire/core INPUT for `eval` (`serde::Deserialize` + `schemars::JsonSchema`).
///
/// The exact four knobs the MCP `cqs_eval` inputSchema advertises. `--save` /
/// `--baseline` / `--tolerance` and the freshness gate are CLI-adapter concerns
/// (post-processing / gating, not routing) and deliberately do not exist on the
/// wire struct — the same by-absence withhold pattern the other cores use.
/// Input-only: never derives `Serialize`; the JSON OUTPUT is the separate
/// [`EvalOutput`].
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub(crate) struct EvalArgs {
    /// Path to the queries JSON file (v3 schema).
    pub query_file: std::path::PathBuf,
    /// Max results retrieved per query (R@K denominator cap; default 20).
    #[serde(default = "default_eval_limit")]
    pub limit: usize,
    /// Restrict the run to one category (e.g. `multi_step`).
    #[serde(default)]
    pub category: Option<String>,
    /// Reranker stage after retrieval (`none` | `onnx`). Default `none` runs
    /// the retrieval-only pipeline.
    #[serde(default)]
    pub reranker: RerankerMode,
}

fn default_eval_limit() -> usize {
    20
}

/// Surface-supplied context for [`eval_core`]: the run knobs plus the
/// adapter-owned substrate/freshness/label posture. Owned (no borrows) so the
/// CLI and daemon adapters can each build one from their own inputs.
pub(crate) struct EvalCoreParams {
    /// Label for [`EvalOutput::query_file`] — the CLI passes the display path,
    /// the daemon the relativized-to-served-root path (never the absolute host
    /// path, per the wire-leak guard).
    pub query_file_label: String,
    /// Restrict the run to one category (`None` = all).
    pub category_filter: Option<String>,
    /// Per-query result count used to compute R@K (typically 20).
    pub limit: usize,
    /// Reranker stage after retrieval.
    pub reranker: RerankerMode,
    /// Which substrate computed the run.
    pub substrate: IndexSubstrate,
    /// Watch-loop freshness, stamped only on the daemon/MCP surface.
    pub freshness: Option<EvalFreshness>,
    /// Shutdown/cancel side-channel (MCP task worker only). Checked once per
    /// query so a daemon shutdown bails a long task run within one query instead
    /// of detaching a 20-min thread against systemd's stop timeout. `None` on the
    /// CLI and direct-daemon-eval paths (a no-op there). Non-serde: it lives on
    /// this adapter bundle, never on `EvalOutput`, so the dispatch↔core parity
    /// guard is untouched. Shutdown-only until a user-facing `tasks/cancel` lane
    /// reuses the same token.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Live progress side-channel (MCP task worker only). Bumped once per
    /// post-filter processed query so a `tasks/get` poll can report `{done,
    /// total}`. Mirrors the `cancel` shape exactly: non-serde, lives on this
    /// adapter bundle and NEVER on [`EvalOutput`], so the dispatch↔core parity
    /// guard is untouched by construction. `None` on the CLI and
    /// direct-daemon-eval paths (a no-op there). See [`ProgressCounters`] for
    /// the one-frame contract.
    pub progress: Option<std::sync::Arc<ProgressCounters>>,
}

/// Live eval-task progress. std-only atomics, NON-serde — it rides the
/// [`EvalCoreParams`] adapter bundle exactly like [`EvalCoreParams::cancel`],
/// never touching [`EvalOutput`], so the dispatch↔core parity guard is
/// untouched by construction.
///
/// THE ONE FRAME (do NOT cross the numerator/denominator — the mirror bug two
/// seam reviews caught):
/// - `done` counts POST-FILTER **processed** queries: a query advances it iff
///   it survives the `--category` filter ([`query_in_scope`]), counting a
///   no-gold skip as processed, and it is bumped at ITERATION END (via
///   [`ProgressCounters::mark_processed`]) so `done` means *finished*, not
///   *started*.
/// - `total` = M = the post-filter query count ([`post_filter_count`]), stamped
///   ONCE at admission before the entry is pollable, so a poll between insert
///   and worker-start never serves `{done:0,total:0}`.
///
/// So at the run's end `done == final (hits.len() + skipped) == total`. This is
/// a DIFFERENT quantity than [`EvalOutput::query_count`] (= scored `hits.len()`,
/// which EXCLUDES no-gold skips) — the two must not be conflated.
#[derive(Debug, Default)]
pub(crate) struct ProgressCounters {
    /// Post-filter processed queries finished so far. Monotone, ceilings at
    /// `total`.
    pub done: std::sync::atomic::AtomicU64,
    /// M — the post-filter query count, stamped once at admission.
    pub total: std::sync::atomic::AtomicU64,
}

impl ProgressCounters {
    /// Mark one post-filter query FINISHED. Called at each PROCESSED iteration's
    /// end (both the no-gold skip and the scored path), after the `--category`
    /// `continue`, so a filtered-out query never advances it. `Relaxed` suffices
    /// — the value is published to a `tasks/get` reader through the entry's
    /// `status` Release/Acquire fence (the descriptor read-order pin), not by
    /// this increment.
    fn mark_processed(&self) {
        let done = self.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        // Mirror-bug tripwire on the real path: the numerator must never overtake
        // the admission-stamped denominator. `total == 0` = "no denominator
        // stamped" (a CLI/degraded 0/0 counter), where the check doesn't apply.
        debug_assert!(
            {
                let total = self.total.load(std::sync::atomic::Ordering::Relaxed);
                total == 0 || done <= total
            },
            "eval progress `done` overtook `total` — numerator/denominator frame mismatch"
        );
    }
}

/// Does `q` survive the eval's `--category` filter? The SINGLE predicate shared
/// by [`eval_core`]'s per-query skip AND [`post_filter_count`]'s denominator, so
/// the progress numerator and denominator are the same frame by construction —
/// the mirror bug (a numerator over the full set paired with a post-filter
/// denominator) is unrepresentable.
fn query_in_scope(q: &EvalQuery, category_filter: Option<&str>) -> bool {
    match category_filter {
        Some(cat) => q.category.as_deref() == Some(cat),
        None => true,
    }
}

/// M — the count of queries surviving the `--category` filter. The eval task's
/// progress `total`, computed at admission (the parsed set is in hand before the
/// entry is inserted) and stamped before the entry is pollable.
pub(crate) fn post_filter_count(set: &QuerySet, category_filter: Option<&str>) -> u64 {
    set.queries
        .iter()
        .filter(|q| query_in_scope(q, category_filter))
        .count() as u64
}

/// Per-category aggregate. R@1/5/20 are fractions in [0.0, 1.0].
///
/// Three metric families over the same N queries:
///   - `r_at_*` — PRIMARY: the `gold_chunk` `(origin, name)` ranks in top-K.
///     The headline number; unchanged by the gold_set/file-level addition.
///   - `goldset_r_at_*` — GOLD-SET: ANY member of `{gold_chunk} ∪ gold_set`
///     ranks (falls back to `gold_chunk` alone when `gold_set` is absent, so
///     it equals `r_at_*` for a query with no set). `goldset ≥ primary`.
///   - `file_r_at_*` — FILE-LEVEL: any top-K result shares the gold's `origin`
///     FILE (name-agnostic). The usefulness upper-band; `file ≥ primary`.
///
/// The two additive families carry `#[serde(default)]` so a baseline saved
/// before they existed still loads (reads as 0.0).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct CategoryStats {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
    #[serde(default)]
    pub goldset_r_at_1: f64,
    #[serde(default)]
    pub goldset_r_at_5: f64,
    #[serde(default)]
    pub goldset_r_at_20: f64,
    #[serde(default)]
    pub file_r_at_1: f64,
    #[serde(default)]
    pub file_r_at_5: f64,
    #[serde(default)]
    pub file_r_at_20: f64,
}

/// Top-level aggregate. Same three metric families as [`CategoryStats`] —
/// primary (`r_at_*`), gold-set (`goldset_r_at_*`), file-level
/// (`file_r_at_*`) — over all scored queries.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct Overall {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
    #[serde(default)]
    pub goldset_r_at_1: f64,
    #[serde(default)]
    pub goldset_r_at_5: f64,
    #[serde(default)]
    pub goldset_r_at_20: f64,
    #[serde(default)]
    pub file_r_at_1: f64,
    #[serde(default)]
    pub file_r_at_5: f64,
    #[serde(default)]
    pub file_r_at_20: f64,
}

/// Full eval output. Same shape for `--json` stdout, `--save` file, and the
/// daemon/MCP `data` payload.
///
/// `Deserialize` is derived so `--baseline` can load a saved report and diff
/// against the current run. Optional fields at the tail preserve forward-compat
/// with baselines from older `cqs eval` runs that omit them.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct EvalOutput {
    pub query_count: usize,
    pub skipped: usize,
    pub elapsed_secs: f64,
    pub queries_per_sec: f64,
    pub overall: Overall,
    pub by_category: BTreeMap<String, CategoryStats>,
    pub index_model: String,
    pub cqs_version: String,
    pub query_file: String,
    /// Effective per-query result limit used for ranking (default 20).
    pub limit: usize,
    /// When `--category` filtered the run, the category name; otherwise `None`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub category_filter: Option<String>,
    /// Which substrate computed these numbers: `resident` (the daemon's
    /// in-memory index) or `fresh-built` (the CLI's cold build). `#[serde(default)]`
    /// keeps baselines saved before this field loadable (they read as the empty
    /// string).
    #[serde(default)]
    pub index_substrate: String,
    /// Watch-loop freshness at run time, stamped only on the daemon/MCP surface
    /// (which does not block on a freshness gate). Absent on the CLI surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<EvalFreshness>,
}

/// Per-query rank tracker across the three metric families. Each rank is
/// 1-indexed; `None` = that family's target was not found in the top `limit`.
struct QueryHit {
    category: String,
    ranks: RankTriple,
}

/// The three per-query ranks a single scored query produces. Each is the
/// 1-indexed position of the first matching result, or `None` for a miss in
/// the top `limit`.
///
/// Invariants (by construction in [`score_ranks`]): a primary match is also a
/// gold-set match (the primary is always in the scored union) AND a file match
/// (same `(origin, name)` ⇒ same file), so `goldset ≤ primary` and
/// `file ≤ primary` whenever `primary` is `Some`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct RankTriple {
    /// PRIMARY — first result matching the `gold_chunk` `(origin, name)`.
    primary: Option<usize>,
    /// GOLD-SET — first result matching ANY member of `{gold_chunk} ∪ gold_set`
    /// on `(origin, name)`. Equals `primary` when `gold_set` is absent.
    goldset: Option<usize>,
    /// FILE-LEVEL — first result whose file == the gold's `origin` (name-agnostic).
    file: Option<usize>,
}

/// The gold targets for one query: the primary `gold_chunk` plus the optional
/// multi-gold `gold_set`. Bundled so [`search_for_rank`] takes a single gold
/// argument (keeping it under clippy's arity bound) and so the primary/set pair
/// travels together.
struct GoldTargets<'a> {
    primary: &'a GoldChunk,
    set: Option<&'a [cqs::eval::schema::GoldSetMember]>,
}

/// The single line-agnostic gold match predicate: a result identifies a gold
/// key iff its (already normalized) file == the gold `origin` AND its name ==
/// the gold `name`.
///
/// The ONE copy of the `(origin, name)` logic — primary scoring and every
/// `gold_set` member route through it, and file-level scoring reuses its
/// `origin` half (a bare file comparison). `line_start` is deliberately not
/// part of the key (see [`search_for_rank`] for the drift rationale).
fn gold_key_matches(
    result_file: &str,
    result_name: &str,
    gold_origin: &str,
    gold_name: &str,
) -> bool {
    result_file == gold_origin && result_name == gold_name
}

/// Score an ordered result list into the three per-query ranks.
///
/// `results` is the ranked list as `(normalized_file, chunk_name)`, index 0 ==
/// rank 1. `gold` is the primary; `gold_set` the optional multi-gold set (its
/// members need not include the primary — the primary is always folded into the
/// scored union, so `gold_set = None` reduces gold-set scoring to `gold` alone).
///
/// Single pass, first-match-wins per family (results are in rank order, so the
/// first match is the minimum rank — the most generous "did search surface
/// this" reading R@K asks for).
fn score_ranks(
    results: &[(String, String)],
    gold: &GoldChunk,
    gold_set: Option<&[cqs::eval::schema::GoldSetMember]>,
) -> RankTriple {
    let mut out = RankTriple::default();
    for (i, (result_file, result_name)) in results.iter().enumerate() {
        let rank = i + 1;
        // Origin half — shared by file-level and the (origin, name) predicate.
        let file_matches = result_file == &gold.origin;
        if out.file.is_none() && file_matches {
            out.file = Some(rank);
        }
        // Primary: the full (origin, name) key against gold_chunk.
        if out.primary.is_none()
            && gold_key_matches(result_file, result_name, &gold.origin, &gold.name)
        {
            out.primary = Some(rank);
        }
        // Gold-set: the same key against the union {gold_chunk} ∪ gold_set. The
        // primary is always a valid target, so it is checked first; folding it
        // in guarantees `goldset ≤ primary` and makes an absent gold_set score
        // identically to gold_chunk alone.
        if out.goldset.is_none() {
            let hits_union = gold_key_matches(result_file, result_name, &gold.origin, &gold.name)
                || gold_set.is_some_and(|members| {
                    members
                        .iter()
                        .any(|m| gold_key_matches(result_file, result_name, &m.origin, &m.name))
                });
            if hits_union {
                out.goldset = Some(rank);
            }
        }
    }
    out
}

/// Read the index model name from the store, degrading to `"unknown"` on a
/// metadata-read failure (never panics — a missing model name is not fatal to
/// reporting R@K).
fn read_index_model(store: &Store<ReadOnly>) -> String {
    store
        .stats()
        .map(|s| s.model_name.clone())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to read index model name; reporting 'unknown'");
            "unknown".to_string()
        })
}

/// Run the eval against an already-parsed [`QuerySet`] and produce an
/// [`EvalOutput`].
///
/// Surface-agnostic: takes a [`SearchCtx`] so the CLI (`CommandContext`) and
/// the daemon (`BatchView`) both drive it. The fixture read/parse is the
/// adapter's job (each surface controls its own error verbosity + path trust);
/// the core receives the parsed set plus the adapter-supplied [`EvalCoreParams`].
///
/// An EMPTY query set short-circuits before the embedder / vector index is
/// built — the dispatch↔core parity guard relies on this to run without an
/// ONNX session.
pub(crate) fn eval_core(
    ctx: &dyn SearchCtx,
    set: &QuerySet,
    params: &EvalCoreParams,
) -> Result<EvalOutput> {
    let _span = tracing::info_span!(
        "eval_core",
        query_file = params.query_file_label,
        category = ?params.category_filter,
        limit = params.limit,
        reranker = ?params.reranker,
        substrate = params.substrate.as_str(),
    )
    .entered();

    let index_model = read_index_model(ctx.store());

    // Empty fixture: nothing to score, so build no embedder / vector index and
    // report a deterministic zero (elapsed pinned to 0.0 so the value is stable
    // across surfaces — a wall-clock read here would break dispatch↔core parity).
    if set.queries.is_empty() {
        return Ok(assemble_output(&[], 0, 0.0, index_model, params));
    }

    // Build the embedder EAGERLY so a load failure is a hard error here rather
    // than surfacing per-query inside `search_for_rank` (whose errors are caught
    // and scored as misses — an embedder failure must NOT silently become an
    // all-miss run). Subsequent `ctx.embedder()` calls hit the same cached
    // instance. Pre-build the vector index once too — eval can be hundreds of
    // queries and rebuilding HNSW per query is the dominant cost.
    let embedder = ctx.embedder()?;
    let index = ctx.vector_index()?;
    let index_ref = index.as_deref();
    // Resolve the reranker once (lazy ONNX load) only when requested. `None`
    // short-circuits the entire stage-2 path.
    let reranker: Option<Arc<dyn cqs::Reranker>> = match params.reranker {
        RerankerMode::None => None,
        RerankerMode::Onnx => Some(ctx.reranker()?),
    };
    let reranker_ref = reranker.as_deref();

    let category_filter = params.category_filter.as_deref();
    let total_queries = set.queries.len();
    let mut hits: Vec<QueryHit> = Vec::with_capacity(total_queries);
    let mut skipped = 0usize;

    let started = Instant::now();
    let mut last_progress = Instant::now();

    for (idx, q) in set.queries.iter().enumerate() {
        // Daemon-shutdown bail: checked once per query so a 20-min task
        // aborts within one query instead of detaching a thread that keeps the
        // process alive against systemd's stop timeout. `None` on the CLI and
        // direct-eval paths, so this is a no-op there. Returning `Err` (rather
        // than a partial aggregate) keeps a truncated run from ever reading as a
        // complete R@K.
        if let Some(cancel) = &params.cancel {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("eval cancelled: daemon shutting down");
            }
        }

        // Filter by category if requested. Skip silently BEFORE the progress
        // bump — these are outside the requested slice, so they count toward
        // neither `total`/`skipped` NOR the progress numerator (`query_in_scope`
        // is the SAME predicate `post_filter_count` uses for the denominator).
        if !query_in_scope(q, category_filter) {
            continue;
        }

        let gold = match &q.gold_chunk {
            Some(g) => g,
            None => {
                tracing::debug!(
                    query = %q.query,
                    "Skipping query: no gold_chunk"
                );
                skipped += 1;
                // Iteration end for a no-gold query: it survived the category
                // filter, so it IS a processed query — advance `done`.
                if let Some(p) = &params.progress {
                    p.mark_processed();
                }
                continue;
            }
        };

        let category = q
            .category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());

        let targets = GoldTargets {
            primary: gold,
            set: q.gold_set.as_deref(),
        };
        let ranks = match search_for_rank(
            ctx,
            embedder,
            index_ref,
            &q.query,
            &targets,
            params.limit,
            reranker_ref,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    query = %q.query,
                    error = %e,
                    "Search failed for query, scoring as miss"
                );
                // A search failure is a miss for ALL three families, not just
                // the primary — an all-`None` triple.
                RankTriple::default()
            }
        };

        hits.push(QueryHit { category, ranks });

        // Iteration end for a scored query: the search has FINISHED, so advance
        // `done` here (not before the search) — a `tasks/get` mid-search then
        // reads `done` = finished queries, never a started-but-pending one.
        if let Some(p) = &params.progress {
            p.mark_processed();
        }

        // Progress every 10 queries (or at least every 5s). Routed through
        // `tracing::info!` so it honors `RUST_LOG`, JSON-log redirect, and
        // any future structured-logs sink. Use `RUST_LOG=cqs=info` to see.
        if (idx + 1) % 10 == 0 || last_progress.elapsed().as_secs() >= 5 {
            let done = hits.len() + skipped;
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            let qps = done as f64 / elapsed;
            tracing::info!(done, total = total_queries, qps, "eval progress");
            last_progress = Instant::now();
        }
    }

    let elapsed_secs = started.elapsed().as_secs_f64();
    Ok(assemble_output(
        &hits,
        skipped,
        elapsed_secs,
        index_model,
        params,
    ))
}

/// Running hit counts for one bucket (a category or the whole set), across all
/// three metric families. Each `*_1/5/20` is the number of queries whose family
/// rank was ≤ the corresponding K. `n` is the bucket's query count.
///
/// The K thresholds (1/5/20) are fixed and identical for every family — the
/// `limit` knob caps retrieval, not the R@K cut points.
#[derive(Debug, Default, Clone, Copy)]
struct HitAccum {
    n: usize,
    p1: usize,
    p5: usize,
    p20: usize,
    g1: usize,
    g5: usize,
    g20: usize,
    f1: usize,
    f5: usize,
    f20: usize,
}

impl HitAccum {
    /// Fold one query's three ranks into the running counts.
    fn add(&mut self, ranks: &RankTriple) {
        self.n += 1;
        Self::bump(ranks.primary, &mut self.p1, &mut self.p5, &mut self.p20);
        Self::bump(ranks.goldset, &mut self.g1, &mut self.g5, &mut self.g20);
        Self::bump(ranks.file, &mut self.f1, &mut self.f5, &mut self.f20);
    }

    /// Add a single family's rank into its 1/5/20 counters. A `None` (miss)
    /// contributes to none; a `Some(r)` contributes to every K it is ≤.
    fn bump(rank: Option<usize>, c1: &mut usize, c5: &mut usize, c20: &mut usize) {
        if let Some(r) = rank {
            if r <= 1 {
                *c1 += 1;
            }
            if r <= 5 {
                *c5 += 1;
            }
            if r <= 20 {
                *c20 += 1;
            }
        }
    }

    /// This bucket's count as a fraction of `n` (0.0 when `n == 0`).
    fn frac(&self, hits: usize) -> f64 {
        if self.n > 0 {
            hits as f64 / self.n as f64
        } else {
            0.0
        }
    }

    fn into_category_stats(self) -> CategoryStats {
        CategoryStats {
            n: self.n,
            r_at_1: self.frac(self.p1),
            r_at_5: self.frac(self.p5),
            r_at_20: self.frac(self.p20),
            goldset_r_at_1: self.frac(self.g1),
            goldset_r_at_5: self.frac(self.g5),
            goldset_r_at_20: self.frac(self.g20),
            file_r_at_1: self.frac(self.f1),
            file_r_at_5: self.frac(self.f5),
            file_r_at_20: self.frac(self.f20),
        }
    }

    fn into_overall(self) -> Overall {
        Overall {
            n: self.n,
            r_at_1: self.frac(self.p1),
            r_at_5: self.frac(self.p5),
            r_at_20: self.frac(self.p20),
            goldset_r_at_1: self.frac(self.g1),
            goldset_r_at_5: self.frac(self.g5),
            goldset_r_at_20: self.frac(self.g20),
            file_r_at_1: self.frac(self.f1),
            file_r_at_5: self.frac(self.f5),
            file_r_at_20: self.frac(self.f20),
        }
    }
}

/// Aggregate per-query hits into the final [`EvalOutput`]. Shared by the empty
/// and non-empty paths so both produce the same shape; the empty path passes a
/// zero elapsed so its output is deterministic.
fn assemble_output(
    hits: &[QueryHit],
    skipped: usize,
    elapsed_secs: f64,
    index_model: String,
    params: &EvalCoreParams,
) -> EvalOutput {
    let scored = hits.len();
    let queries_per_sec = if elapsed_secs > 0.0 {
        scored as f64 / elapsed_secs
    } else {
        0.0
    };

    // Aggregate by category, accumulating all three metric families side by
    // side over the same N. `all` is the top-level accumulator.
    let mut by_cat: BTreeMap<String, HitAccum> = BTreeMap::new();
    let mut all = HitAccum::default();
    for h in hits {
        let entry = by_cat.entry(h.category.clone()).or_default();
        entry.add(&h.ranks);
        all.add(&h.ranks);
    }

    let by_category: BTreeMap<String, CategoryStats> = by_cat
        .into_iter()
        .map(|(cat, acc)| (cat, acc.into_category_stats()))
        .collect();

    let overall = all.into_overall();

    EvalOutput {
        query_count: scored,
        skipped,
        elapsed_secs,
        queries_per_sec,
        overall,
        by_category,
        index_model,
        cqs_version: env!("CARGO_PKG_VERSION").to_string(),
        query_file: params.query_file_label.clone(),
        limit: params.limit,
        category_filter: params.category_filter.clone(),
        index_substrate: params.substrate.as_str().to_string(),
        freshness: params.freshness.clone(),
    }
}

/// Issue one search and return the three per-query ranks (primary / gold-set /
/// file-level) as a [`RankTriple`]; each is `None` if that family's target
/// doesn't appear in the top `limit`.
///
/// Mirrors the production search path (`cmd_query` in
/// `src/cli/commands/search/query.rs`) so eval scores reflect actual
/// production behavior:
///   - Per-category SPLADE alpha routing (via `classify_query`)
///   - Centroid reclassification with α floor
///   - DenseBase / Enriched index routing
///   - `code_types()` default include filter
fn search_for_rank(
    ctx: &dyn SearchCtx,
    embedder: &cqs::Embedder,
    index: Option<&dyn cqs::index::VectorIndex>,
    query: &str,
    gold: &GoldTargets<'_>,
    limit: usize,
    reranker: Option<&dyn cqs::Reranker>,
) -> Result<RankTriple> {
    let store = ctx.store();
    let query_embedding = embedder.embed_query(query)?;

    // Mirror cmd_query: when reranking, over-retrieve so the cross-encoder
    // sees more candidates than the user-facing limit. Cap respects
    // `CQS_RERANK_POOL_MAX` / `RERANK_POOL_MAX` (currently 20). Without
    // reranking, retrieval limit equals the requested limit.
    let stage1_limit = if reranker.is_some() {
        crate::cli::limits::rerank_pool_size(limit).max(limit)
    } else {
        limit
    };

    // Adaptive routing: classify, then refine via centroid.
    let classification = cqs::search::router::classify_query(query);
    let pre_centroid_cat = classification.category;
    let classification =
        cqs::search::router::reclassify_with_centroid(classification, query_embedding.as_slice());
    let centroid_applied = classification.category != pre_centroid_cat;

    // SPLADE alpha resolution (matches cmd_query): per-category router by
    // default; centroid floor at 0.7 prevents misclassifications zeroing
    // SPLADE on queries where it's load-bearing.
    let mut splade_alpha = cqs::search::router::resolve_splade_alpha(&classification.category);
    if centroid_applied {
        splade_alpha = splade_alpha.max(0.7);
    }
    let use_splade = true; // Always on when classification produced a category — same as production.

    // SearchFilter is `#[non_exhaustive]`; external-crate construction
    // goes through `Default` + field assignment. Only fields that differ from
    // the struct default are set here (defaults preserved: languages,
    // exclude_types, path_pattern, enable_rrf, enable_demotion, mmr_lambda).
    // `name_boost = 0.2` is retained because the struct default is 0.0
    // (CLI applies 0.2 via DEFAULT_NAME_BOOST; eval mirrors that here).
    let filter = {
        let mut f = SearchFilter::default();
        f.include_types = Some(ChunkType::code_types());
        f.name_boost = 0.2;
        f.query_text = query.to_string();
        f.enable_splade = use_splade;
        f.splade_alpha = splade_alpha;
        f.type_boost_types = classification.type_hints.clone();
        f
    };
    filter
        .validate()
        .map_err(|e| anyhow::anyhow!("Invalid SearchFilter: {e}"))?;

    // SPLADE encoding (best-effort: missing encoder falls back to dense).
    // `splade_encode` encapsulates the encoder-then-encode two-step and the
    // daemon's `ensure_splade_index` priming, so both surfaces resolve the same
    // way.
    let splade_query = if use_splade {
        ctx.splade_encode(query)
    } else {
        None
    };
    let splade_index: Option<SpladeIndexRef<'_>> =
        if use_splade { ctx.splade_index() } else { None };
    let splade_index_deref = splade_index.as_deref();
    let splade_arg = splade_index_deref.zip(splade_query.as_ref());

    let threshold = 0.0_f32; // Don't drop low-similarity results — we want full top-K for R@K.

    let stage1_results: Vec<UnifiedResult> = if splade_arg.is_some() {
        let code = store.search_hybrid(
            &query_embedding,
            &filter,
            stage1_limit,
            threshold,
            index,
            splade_arg,
        )?;
        code.into_iter().map(UnifiedResult::Code).collect()
    } else {
        store.search_code_results(&query_embedding, &filter, stage1_limit, threshold, index)?
    };

    // Apply reranker (when enabled): pull out the SearchResult payloads,
    // hand them to the trait, truncate the reordered list to `limit`.
    // `NoopReranker` is a valid mode but never reaches here — the adapter
    // only constructs a reranker for `Onnx`, leaving `None` to fall through
    // to retrieval-only.
    let results: Vec<UnifiedResult> = if let Some(reranker) = reranker {
        let mut code_results: Vec<cqs::store::SearchResult> = stage1_results
            .into_iter()
            .map(|r| match r {
                UnifiedResult::Code(sr) => sr,
            })
            .collect();
        if code_results.len() > 1 {
            reranker
                .rerank_with_preamble(query, &mut code_results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }
        // `Reranker::rerank` truncates to `limit` internally for impls that
        // honor the contract; double-truncate as defense-in-depth so a
        // misbehaving impl can't silently return more rows than asked.
        code_results.truncate(limit);
        code_results.into_iter().map(UnifiedResult::Code).collect()
    } else {
        stage1_results
    };

    // Score the three families off the ranked list. Match on (file == origin)
    // AND (name == gold.name) for primary/gold-set; origin-only for file-level.
    //
    // We deliberately *don't* check `line_start`. The fixture's line numbers
    // are frozen at the moment it was generated/refreshed, but every audit
    // wave shifts function definitions up or down by a few lines as code
    // moves around. Including `line_start` in the match key means a 1-line
    // shift in the source turns a correct retrieval into a counted miss —
    // an artifact of fixture staleness, not a real search regression.
    // Re-pinning the fixture only postpones the problem: the drift
    // re-accumulates and reproduces a phantom regression (~24pp R@5 in one
    // observed case). Matching on `(file, name)` is loose
    // enough to be drift-resilient, strict enough that retrieval has to
    // surface the right function in the right file. Where a file has
    // multiple chunks with the same name (overloads, windowed sub-chunks
    // of the same section), the first ranked match wins — that's the most
    // generous interpretation of "did search find this," which is what
    // R@K is asking.
    let rows: Vec<(String, String)> = results
        .iter()
        .map(|r| {
            let UnifiedResult::Code(sr) = r;
            (cqs::normalize_path(&sr.chunk.file), sr.chunk.name.clone())
        })
        .collect();
    Ok(score_ranks(&rows, gold.primary, gold.set))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Schema deserialization is exercised in `cqs::eval::schema::tests`
    // where those types live, and `tests/eval_test.rs` runs
    // `deny_unknown_fields` against the on-disk v3 fixture. The
    // remaining tests below pin the runner's file-I/O and wire contracts.

    /// Writing a tiny query file and reading it back through `read_to_string`
    /// is the same path the adapters use — pin the I/O contract here.
    #[test]
    fn test_query_file_round_trip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let payload = serde_json::json!({
            "queries": [
                {
                    "query": "find foo",
                    "category": "identifier_lookup",
                    "gold_chunk": {
                        "name": "foo",
                        "origin": "src/lib.rs",
                        "line_start": 42
                    }
                }
            ]
        });
        writeln!(&tmp, "{}", serde_json::to_string(&payload).unwrap()).unwrap();

        let raw = std::fs::read_to_string(tmp.path()).unwrap();
        let set: QuerySet = serde_json::from_str(&raw).unwrap();
        assert_eq!(set.queries.len(), 1);
        assert_eq!(set.queries[0].gold_chunk.as_ref().unwrap().line_start, 42);
    }

    /// The wire `EvalArgs` deserializes the four advertised knobs and defaults
    /// the rest, with the reranker riding the wire as the lowercase enum
    /// spelling.
    #[test]
    fn eval_args_wire_defaults_and_reranker() {
        // Minimal: only query_file → limit/category/reranker default.
        let a: EvalArgs = serde_json::from_value(serde_json::json!({
            "query_file": "evals/q.json"
        }))
        .expect("minimal eval args");
        assert_eq!(a.limit, 20);
        assert!(a.category.is_none());
        assert_eq!(a.reranker, RerankerMode::None);

        // Full: reranker rides the wire as `onnx`.
        let b: EvalArgs = serde_json::from_value(serde_json::json!({
            "query_file": "evals/q.json",
            "limit": 50,
            "category": "multi_step",
            "reranker": "onnx"
        }))
        .expect("full eval args");
        assert_eq!(b.limit, 50);
        assert_eq!(b.category.as_deref(), Some("multi_step"));
        assert_eq!(b.reranker, RerankerMode::Onnx);
    }

    /// `IndexSubstrate` renders the stable wire strings the recall-gate
    /// discipline keys off.
    #[test]
    fn index_substrate_wire_strings() {
        assert_eq!(IndexSubstrate::Resident.as_str(), "resident");
        assert_eq!(IndexSubstrate::FreshBuilt.as_str(), "fresh-built");
    }

    // ── Progress frame (D2) ──────────────────────────────────────────────
    // The one-frame contract lives in three decomposable pieces the loop
    // composes: `post_filter_count` is the DENOMINATOR M, `mark_processed`
    // the NUMERATOR bump, and both go through the SAME `query_in_scope`
    // predicate so the mirror bug (numerator over the full set paired with a
    // post-filter denominator) is unrepresentable. These pin each piece
    // without the ~500 MB embedder a full `eval_core` run needs (that wiring
    // is covered end-to-end by `eval_core_progress_frame_end_to_end` in the
    // dispatch tests, embedder-gated). The `EvalOutput` guard pins that the
    // side-channel never leaks into the serialized output.

    /// `post_filter_count` is the progress DENOMINATOR (M): the count of
    /// queries surviving `--category`, through the SAME `query_in_scope`
    /// predicate `eval_core`'s per-query skip uses. This pins (1) under a
    /// category, M is the POST-filter count — NOT the full set (the mirror
    /// bug would return the full set here); (2) a no-gold query still counts
    /// toward M (it is in-scope, hence processed); (3) no filter yields the
    /// full set; (4) an unmatched category yields a legitimate zero.
    #[test]
    fn post_filter_count_is_the_post_filter_denominator() {
        let set: QuerySet = serde_json::from_value(serde_json::json!({
            "queries": [
                {"query": "a1", "category": "alpha",
                 "gold_chunk": {"name": "x", "origin": "src/lib.rs", "line_start": 1}},
                // In-scope alpha query with NO gold — must still count toward M.
                {"query": "a2-nogold", "category": "alpha"},
                {"query": "b1", "category": "beta",
                 "gold_chunk": {"name": "y", "origin": "src/lib.rs", "line_start": 2}},
                {"query": "u1",
                 "gold_chunk": {"name": "z", "origin": "src/lib.rs", "line_start": 3}}
            ]
        }))
        .expect("parse set");

        // The full set is 4; the mirror bug would report 4 under a filter.
        assert_eq!(
            post_filter_count(&set, Some("alpha")),
            2,
            "M under --category=alpha is the POST-filter count, incl the no-gold alpha query"
        );
        assert_eq!(post_filter_count(&set, Some("beta")), 1);
        assert_eq!(
            post_filter_count(&set, None),
            4,
            "no filter ⇒ the whole set is in scope"
        );
        assert_eq!(
            post_filter_count(&set, Some("gamma")),
            0,
            "a category no query carries ⇒ M is a legitimate zero"
        );
    }

    /// `mark_processed` is the progress NUMERATOR: monotone, exactly one per
    /// call. After M calls a counter stamped with M reads `done == total ==
    /// M` — the run-end identity the descriptor reports.
    #[test]
    fn mark_processed_counts_to_the_stamped_total() {
        use std::sync::atomic::Ordering;
        let p = ProgressCounters::default();
        // Stamp M the way the admission path's `stamp_progress_total` does.
        p.total.store(3, Ordering::Relaxed);
        assert_eq!(p.done.load(Ordering::Relaxed), 0);
        for expected in 1..=3 {
            p.mark_processed();
            assert_eq!(
                p.done.load(Ordering::Relaxed),
                expected,
                "each mark_processed advances done by exactly one"
            );
        }
        assert_eq!(
            p.done.load(Ordering::Relaxed),
            p.total.load(Ordering::Relaxed),
            "at run end done == total == M"
        );
    }

    /// The mirror-bug tripwire: were the numerator bumped over the PRE-filter
    /// set (`done` reaching N) while `total` holds the POST-filter M (< N),
    /// `mark_processed`'s `debug_assert` fires. Pins that the runtime guard
    /// bites — delete the assert and this test stops panicking. Debug-only:
    /// the assert is compiled out under `--release`, so gate the test on
    /// `debug_assertions` to match.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "numerator/denominator frame mismatch")]
    fn mark_processed_tripwire_fires_when_numerator_overtakes_total() {
        use std::sync::atomic::Ordering;
        let p = ProgressCounters::default();
        // M = 1 (a one-query --category slice).
        p.total.store(1, Ordering::Relaxed);
        p.mark_processed(); // done = 1 == total: fine.
        p.mark_processed(); // done = 2 > total: tripwire.
    }

    /// The progress side-channel NEVER appears in `EvalOutput` — it rides
    /// `EvalCoreParams` (exactly like `cancel`), so the dispatch↔core parity
    /// guard holds by construction. Build an output from params
    /// carrying a NON-None, already-advanced progress counter and assert the
    /// serialized JSON carries none of the side-channel's keys (a regression
    /// here would be someone lifting progress onto `EvalOutput`).
    #[test]
    fn eval_output_never_serializes_progress_side_channel() {
        use std::sync::atomic::Ordering;
        let progress = std::sync::Arc::new(ProgressCounters::default());
        progress.total.store(7, Ordering::Relaxed);
        progress.mark_processed();
        let params = EvalCoreParams {
            query_file_label: "q.json".to_string(),
            category_filter: Some("alpha".to_string()),
            limit: 20,
            reranker: RerankerMode::None,
            substrate: IndexSubstrate::Resident,
            freshness: None,
            cancel: None,
            progress: Some(progress),
        };
        let out = assemble_output(&[], 0, 0.0, "model".to_string(), &params);
        let val = serde_json::to_value(&out).expect("serialize EvalOutput");
        let obj = val.as_object().expect("EvalOutput serializes as an object");
        for k in ["progress", "done", "total"] {
            assert!(
                !obj.contains_key(k),
                "EvalOutput must not carry the progress side-channel key `{k}`: {val}"
            );
        }
        // Positive control: the fields that SHOULD be present are.
        assert!(obj.contains_key("query_count"));
        assert!(obj.contains_key("category_filter"));
    }

    // ── gold_set + file-level scoring (score_ranks / aggregation) ─────────
    // These drive the pure scoring seam on a controllable result list — no
    // embedder, no vector index, no live store. `score_ranks` is the single
    // place the three families are computed; `assemble_output` folds ranks
    // into the metric families. Together they pin the honest-scoring contract.

    /// Build a `GoldChunk` with a throwaway `line_start` (matching is
    /// line-agnostic, so the value never matters).
    fn gold(name: &str, origin: &str) -> GoldChunk {
        GoldChunk {
            name: name.to_string(),
            origin: origin.to_string(),
            line_start: 0,
            id: None,
            line_end: None,
            chunk_type: None,
            language: None,
        }
    }

    /// A `gold_set` member (only the `(origin, name)` key is load-bearing).
    fn member(name: &str, origin: &str) -> cqs::eval::schema::GoldSetMember {
        cqs::eval::schema::GoldSetMember {
            name: name.to_string(),
            origin: origin.to_string(),
            line_start: None,
        }
    }

    /// `(file, name)` rows in rank order (index 0 == rank 1).
    fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(f, n)| (f.to_string(), n.to_string()))
            .collect()
    }

    /// BACK-COMPAT PIN: with NO `gold_set`, the gold-set rank is EXACTLY the
    /// primary rank — a pre-`gold_set` fixture scores identically. Pinned for
    /// both a hit and a miss (both fall back to `gold_chunk` alone).
    #[test]
    fn score_ranks_without_gold_set_equals_primary() {
        // Hit at rank 1.
        let hit = score_ranks(
            &rows(&[("src/a.rs", "foo"), ("src/b.rs", "bar")]),
            &gold("foo", "src/a.rs"),
            None,
        );
        assert_eq!(hit.primary, Some(1));
        assert_eq!(
            hit.goldset, hit.primary,
            "no gold_set ⇒ gold-set rank must equal primary rank exactly"
        );

        // Miss: gold absent from the list.
        let miss = score_ranks(
            &rows(&[("src/x.rs", "zzz"), ("src/y.rs", "qqq")]),
            &gold("foo", "src/a.rs"),
            None,
        );
        assert_eq!(miss.primary, None);
        assert_eq!(
            miss.goldset, miss.primary,
            "no gold_set ⇒ a primary miss is a gold-set miss too"
        );
    }

    /// GOLD-SET WIN: the primary gold ranks past K but a set member ranks
    /// within it → a gold-set hit that is NOT a primary hit (the +pp case).
    /// The set member lives in a DIFFERENT file than the gold origin, so this
    /// isolates gold-set scoring from file-level scoring.
    #[test]
    fn score_ranks_gold_set_member_hits_where_primary_misses() {
        // rank1 noise; rank2 = a gold_set member (different file); rank7 = the
        // primary (foo in src/a.rs).
        let result_rows = rows(&[
            ("src/n.rs", "noise"),
            ("src/z.rs", "member2"),
            ("src/f.rs", "fill3"),
            ("src/f.rs", "fill4"),
            ("src/f.rs", "fill5"),
            ("src/f.rs", "fill6"),
            ("src/a.rs", "foo"),
        ]);
        let triple = score_ranks(
            &result_rows,
            &gold("foo", "src/a.rs"),
            Some(&[member("member2", "src/z.rs")]),
        );
        assert_eq!(triple.primary, Some(7), "primary is the far-down foo");
        assert_eq!(
            triple.goldset,
            Some(2),
            "gold-set catches the rank-2 member"
        );
        assert_eq!(
            triple.file,
            Some(7),
            "file-level's first src/a.rs result is the primary at rank 7"
        );

        // The @K semantics via the aggregator: at K=5 primary misses, gold-set
        // hits; at K=20 both hit.
        let out = assemble_from(&[("cat".to_string(), triple)]);
        assert_eq!(out.overall.r_at_5, 0.0, "primary R@5 is a miss");
        assert_eq!(out.overall.goldset_r_at_5, 1.0, "gold-set R@5 is a hit");
        assert_eq!(out.overall.file_r_at_5, 0.0, "file-level R@5 is a miss");
        assert_eq!(out.overall.r_at_20, 1.0, "primary R@20 catches rank 7");
        assert_eq!(out.overall.goldset_r_at_20, 1.0);
        assert_eq!(out.overall.file_r_at_20, 1.0);
    }

    /// FILE-LEVEL REFRAME: the gold FUNCTION is never retrieved, but a
    /// same-file sibling ranks → a file hit that is NOT a function (primary or
    /// gold-set) hit. `gold_set` is absent, so gold-set tracks primary (both
    /// miss).
    #[test]
    fn score_ranks_file_level_hits_on_same_file_sibling() {
        let triple = score_ranks(
            &rows(&[("src/a.rs", "sibling"), ("src/b.rs", "other")]),
            &gold("foo", "src/a.rs"),
            None,
        );
        assert_eq!(
            triple.primary, None,
            "the gold function foo was not retrieved"
        );
        assert_eq!(
            triple.goldset, None,
            "no gold_set ⇒ gold-set misses with primary"
        );
        assert_eq!(
            triple.file,
            Some(1),
            "the same-file sibling makes this a file-level hit"
        );

        let out = assemble_from(&[("cat".to_string(), triple)]);
        assert_eq!(out.overall.r_at_5, 0.0, "function-exact R@5 is a miss");
        assert_eq!(out.overall.file_r_at_5, 1.0, "file-level R@5 is a hit");
    }

    /// `by_category` carries all three metric families, distinct per category.
    /// One category's query is a primary hit; the other's is a file-only hit —
    /// so the two categories' family numbers diverge, proving each family is
    /// tracked independently per bucket.
    #[test]
    fn assemble_output_by_category_carries_all_three_families() {
        let out = assemble_from(&[
            // A clean primary hit at rank 1: all three families hit@1.
            (
                "exact".to_string(),
                RankTriple {
                    primary: Some(1),
                    goldset: Some(1),
                    file: Some(1),
                },
            ),
            // A file-only hit: primary/gold-set miss, file hits@1.
            (
                "reframe".to_string(),
                RankTriple {
                    primary: None,
                    goldset: None,
                    file: Some(1),
                },
            ),
        ]);

        let exact = out
            .by_category
            .get("exact")
            .expect("exact category present");
        assert_eq!(
            (exact.r_at_1, exact.goldset_r_at_1, exact.file_r_at_1),
            (1.0, 1.0, 1.0)
        );

        let reframe = out
            .by_category
            .get("reframe")
            .expect("reframe category present");
        assert_eq!(
            (reframe.r_at_1, reframe.goldset_r_at_1, reframe.file_r_at_1),
            (0.0, 0.0, 1.0),
            "primary/gold-set miss but file-level hits for the reframe query"
        );

        // Overall blends the two: primary/gold-set 1/2, file-level 2/2.
        assert_eq!(out.overall.r_at_1, 0.5);
        assert_eq!(out.overall.goldset_r_at_1, 0.5);
        assert_eq!(out.overall.file_r_at_1, 1.0);
    }

    /// Test-only bridge: fold `(category, RankTriple)` rows into an
    /// `EvalOutput` through the real `assemble_output`, so the aggregation
    /// tests exercise the production folding path (not a re-implementation).
    fn assemble_from(rows: &[(String, RankTriple)]) -> EvalOutput {
        let hits: Vec<QueryHit> = rows
            .iter()
            .map(|(category, ranks)| QueryHit {
                category: category.clone(),
                ranks: *ranks,
            })
            .collect();
        let params = EvalCoreParams {
            query_file_label: "test.json".to_string(),
            category_filter: None,
            limit: 20,
            reranker: RerankerMode::None,
            substrate: IndexSubstrate::FreshBuilt,
            freshness: None,
            cancel: None,
            progress: None,
        };
        assemble_output(&hits, 0, 0.0, "model".to_string(), &params)
    }
}
