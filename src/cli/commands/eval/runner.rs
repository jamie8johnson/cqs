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

use cqs::eval::schema::{GoldChunk, QuerySet};
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
}

/// Per-category aggregate. R@1/5/20 are fractions in [0.0, 1.0].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct CategoryStats {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
}

/// Top-level aggregate.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(crate) struct Overall {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
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

/// Per-query rank tracker. `None` = gold not found in top `limit`.
struct QueryHit {
    category: String,
    rank: Option<usize>, // 1-indexed; None = miss
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
        // Filter by category if requested. Skip silently — these don't count
        // toward total or skipped (they're outside the requested slice).
        if let Some(cat) = category_filter {
            if q.category.as_deref() != Some(cat) {
                continue;
            }
        }

        let gold = match &q.gold_chunk {
            Some(g) => g,
            None => {
                tracing::debug!(
                    query = %q.query,
                    "Skipping query: no gold_chunk"
                );
                skipped += 1;
                continue;
            }
        };

        let category = q
            .category
            .clone()
            .unwrap_or_else(|| "uncategorized".to_string());

        let rank = match search_for_rank(
            ctx,
            embedder,
            index_ref,
            &q.query,
            gold,
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
                None
            }
        };

        hits.push(QueryHit { category, rank });

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

    // Aggregate by category.
    let mut by_cat: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    let (mut all_n, mut all_h1, mut all_h5, mut all_h20) = (0usize, 0usize, 0usize, 0usize);
    for h in hits {
        let entry = by_cat.entry(h.category.clone()).or_insert((0, 0, 0, 0));
        entry.0 += 1;
        all_n += 1;
        if let Some(r) = h.rank {
            if r <= 1 {
                entry.1 += 1;
                all_h1 += 1;
            }
            if r <= 5 {
                entry.2 += 1;
                all_h5 += 1;
            }
            if r <= 20 {
                entry.3 += 1;
                all_h20 += 1;
            }
        }
    }

    let by_category: BTreeMap<String, CategoryStats> = by_cat
        .into_iter()
        .map(|(cat, (n, h1, h5, h20))| {
            let n_f = n as f64;
            let stats = CategoryStats {
                n,
                r_at_1: if n > 0 { h1 as f64 / n_f } else { 0.0 },
                r_at_5: if n > 0 { h5 as f64 / n_f } else { 0.0 },
                r_at_20: if n > 0 { h20 as f64 / n_f } else { 0.0 },
            };
            (cat, stats)
        })
        .collect();

    let overall = Overall {
        n: all_n,
        r_at_1: if all_n > 0 {
            all_h1 as f64 / all_n as f64
        } else {
            0.0
        },
        r_at_5: if all_n > 0 {
            all_h5 as f64 / all_n as f64
        } else {
            0.0
        },
        r_at_20: if all_n > 0 {
            all_h20 as f64 / all_n as f64
        } else {
            0.0
        },
    };

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

/// Issue one search and return the 1-indexed rank of the gold chunk
/// (or `None` if it doesn't appear in the top `limit`).
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
    gold: &GoldChunk,
    limit: usize,
    reranker: Option<&dyn cqs::Reranker>,
) -> Result<Option<usize>> {
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

    // Find gold rank (1-indexed). Match on (file == origin) AND
    // (name == gold.name).
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
    let target_file = gold.origin.as_str();
    for (i, r) in results.iter().enumerate() {
        let UnifiedResult::Code(sr) = r;
        let file_str = cqs::normalize_path(&sr.chunk.file);
        if file_str == target_file && sr.chunk.name == gold.name {
            return Ok(Some(i + 1));
        }
    }
    Ok(None)
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
}
