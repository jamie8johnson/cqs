//! Eval runner: load query set, run search per query, score against gold chunks.
//!
//! Reuses the production search path (`search_unified_with_index` /
//! `search_hybrid`) so eval results match what `cqs <query>` returns. The
//! filter construction mirrors `cmd_query` in
//! `src/cli/commands/search/query.rs` — same SPLADE alpha resolution, same
//! routing decisions, same code-types default.
//!
//! Future Task C1 will reuse this scoring path with a side-built temporary
//! index (`--with-model`). To keep that swap mechanical, the search-and-score
//! loop is intentionally factored as a single helper that takes a
//! `CommandContext`-compatible store + embedder + index. C1 will add a
//! parallel helper that takes the same trio sourced from a temp dir.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use cqs::eval::schema::{GoldChunk, QuerySet};
use cqs::language::ChunkType;
use cqs::store::{ReadOnly, UnifiedResult};
use cqs::{SearchFilter, Store};

use crate::cli::CommandContext;

// Wire-format types (`QuerySet`, `EvalQuery`, `GoldChunk`) live in the lib
// crate at `cqs::eval::schema` so the integration tests and any future
// Rust-side eval tooling share one definition. Audit P2 #61.

/// Per-category aggregate. R@1/5/20 are fractions in [0.0, 1.0].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CategoryStats {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
}

/// Top-level aggregate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Overall {
    pub n: usize,
    pub r_at_1: f64,
    pub r_at_5: f64,
    pub r_at_20: f64,
}

/// Full eval output. Same shape for `--json` stdout and `--save` file.
///
/// `Deserialize` is derived so `--baseline` can load a previously saved
/// report and diff against the current run (Task C2). Optional fields at
/// the tail preserve forward-compat with baselines from older `cqs eval`
/// runs that pre-date them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct EvalReport {
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
}

/// Per-query rank tracker. `None` = gold not found in top `limit`.
struct QueryHit {
    category: String,
    rank: Option<usize>, // 1-indexed; None = miss
}

/// Run the eval and produce an `EvalReport`.
///
/// `query_file` is the path to a JSON queries file (v3 format).
/// `category_filter` restricts the run to one category (None = all).
/// `limit` is the per-query result count used to compute R@K (typically 20).
pub(crate) fn run_eval(
    ctx: &CommandContext<'_, ReadOnly>,
    query_file: &Path,
    category_filter: Option<&str>,
    limit: usize,
) -> Result<EvalReport> {
    let _span = tracing::info_span!(
        "run_eval",
        query_file = %query_file.display(),
        category = ?category_filter,
        limit
    )
    .entered();

    let raw = std::fs::read_to_string(query_file)
        .with_context(|| format!("Failed to read query file: {}", query_file.display()))?;
    let set: QuerySet = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse query JSON: {}", query_file.display()))?;

    // Pre-build the vector index once and reuse across queries — eval can
    // be hundreds of queries and rebuilding HNSW per query is the dominant
    // cost. The Python harness re-uses the same index via `cqs batch` so
    // this matches that behavior.
    let store = &ctx.store;
    let embedder = ctx.embedder()?;
    let index = crate::cli::build_vector_index(store, &ctx.cqs_dir)?;
    let index_ref = index.as_deref();

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

        let rank = match search_for_rank(ctx, embedder, store, index_ref, &q.query, gold, limit) {
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
    let scored = hits.len();
    let queries_per_sec = if elapsed_secs > 0.0 {
        scored as f64 / elapsed_secs
    } else {
        0.0
    };

    // Aggregate by category.
    let mut by_cat: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    let (mut all_n, mut all_h1, mut all_h5, mut all_h20) = (0usize, 0usize, 0usize, 0usize);
    for h in &hits {
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

    let index_model = store
        .stats()
        .map(|s| s.model_name.clone())
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "Failed to read index model name; reporting 'unknown'");
            "unknown".to_string()
        });

    Ok(EvalReport {
        query_count: scored,
        skipped,
        elapsed_secs,
        queries_per_sec,
        overall,
        by_category,
        index_model,
        cqs_version: env!("CARGO_PKG_VERSION").to_string(),
        query_file: query_file.display().to_string(),
        limit,
        category_filter: category_filter.map(|s| s.to_string()),
    })
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
    ctx: &CommandContext<'_, ReadOnly>,
    embedder: &cqs::Embedder,
    store: &Store<ReadOnly>,
    index: Option<&dyn cqs::index::VectorIndex>,
    query: &str,
    gold: &GoldChunk,
    limit: usize,
) -> Result<Option<usize>> {
    let query_embedding = embedder.embed_query(query)?;

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

    let filter = SearchFilter {
        languages: None,
        include_types: Some(ChunkType::code_types()),
        exclude_types: None,
        path_pattern: None,
        name_boost: 0.2,
        query_text: query.to_string(),
        enable_rrf: false,
        enable_demotion: true,
        enable_splade: use_splade,
        splade_alpha,
        type_boost_types: classification.type_hints.clone(),
        mmr_lambda: None, // Resolved by finalize_results via CQS_MMR_LAMBDA fallback.
    };
    filter
        .validate()
        .map_err(|e| anyhow::anyhow!("Invalid SearchFilter: {e}"))?;

    // SPLADE encoding (best-effort: missing encoder falls back to dense)
    let splade_query = if use_splade {
        ctx.splade_encoder()
            .and_then(|enc| match enc.encode(query) {
                Ok(sv) => Some(sv),
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE query encoding failed");
                    None
                }
            })
    } else {
        None
    };
    let splade_index = if use_splade { ctx.splade_index() } else { None };
    let splade_arg = splade_query
        .as_ref()
        .and_then(|sq| splade_index.map(|si| (si, sq)));

    let threshold = 0.0_f32; // Don't drop low-similarity results — we want full top-K for R@K.

    let results: Vec<UnifiedResult> = if splade_arg.is_some() {
        let code = store.search_hybrid(
            &query_embedding,
            &filter,
            limit,
            threshold,
            index,
            splade_arg,
        )?;
        code.into_iter().map(UnifiedResult::Code).collect()
    } else {
        store.search_unified_with_index(&query_embedding, &filter, limit, threshold, index)?
    };

    // Find gold rank (1-indexed). Match on (file == origin) AND
    // (name == gold.name).
    //
    // We deliberately *don't* check `line_start`. The fixture's line numbers
    // are frozen at the moment it was generated/refreshed, but every audit
    // wave shifts function definitions up or down by a few lines as code
    // moves around. Including `line_start` in the match key means a 1-line
    // shift in the source turns a correct retrieval into a counted miss —
    // an artifact of fixture staleness, not a real search regression. PR
    // #1109 (2026-04-25) re-pinned the fixture to absorb the v1.29.x
    // drift; weeks later the same drift had re-accumulated and reproduced
    // a 24pp R@5 phantom-regression. Matching on `(file, name)` is loose
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

    // Schema deserialization is exercised in `cqs::eval::schema::tests` —
    // the types live there now (audit P2 #61), and `tests/eval_test.rs`
    // runs `deny_unknown_fields` against the on-disk v3 fixture. The
    // remaining test below pins the runner's file-I/O contract.

    /// Writing a tiny query file and reading it back through `read_to_string`
    /// is the same path the runner uses — pin the I/O contract here.
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
}
