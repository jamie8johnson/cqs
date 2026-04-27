//! Search dispatch handler.

use anyhow::{Context, Result};

use super::super::types::ChunkOutput;
use super::super::BatchView;
use crate::cli::args::SearchArgs;

/// Dispatches a search query and returns results as JSON.
/// #947: takes the shared `SearchArgs` directly, no batch-local `SearchParams`
/// redirection. Both CLI top-level search and batch search deserialize into
/// the same struct, eliminating per-field drift as a possibility.
/// # Arguments
/// * `ctx` - The batch processing context containing the store and embedder
/// * `args` - Parsed search arguments (shared with CLI top-level)
/// # Returns
/// A `Result` containing a JSON object with:
/// * `results` - Array of matching search results
/// * `query` - The original query string
/// * `total` - Number of results returned
/// # Errors
/// Returns an error if:
/// * The embedder cannot be initialized
/// * Query embedding fails
/// * The language parameter is invalid
/// * Store operations fail
pub(in crate::cli::batch) fn dispatch_search(
    ctx: &BatchView,
    args: &SearchArgs,
) -> Result<serde_json::Value> {
    let _span = tracing::info_span!("batch_search", query = %args.query).entered();

    // Accepted for CLI parity; batch JSON doesn't use line-context, parent
    // expansion, include-docs, pattern, or no-stale-check yet. Assigning to
    // `_` avoids clippy unused-field warnings while preserving forwards
    // compat: when the batch path wires these up, removing the `_ =` line
    // makes the compiler surface remaining call-sites.
    let _ = (args.context, args.expand_parent, args.no_stale_check);
    let _ = (args.include_docs, args.pattern.as_ref());

    if args.name_only {
        let results = ctx
            .store()
            .search_by_name(&args.query, args.limit.clamp(1, 100))?;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::to_value(ChunkOutput::from_search_result(r, false))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %r.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": r.chunk.name})
                    })
            })
            .collect();
        return Ok(serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": json_results.len(),
        }));
    }

    // Pure textual argument validation runs BEFORE embedder load —
    // invalid `--lang` / `--include-type` / `--exclude-type` are user
    // typos, not model state, so the user-facing error must fast-fail
    // with the offending flag's name instead of "embed query failed"
    // when the embedder is uncached or contended (HF Hub lock race in
    // CI test env was the original symptom).
    let languages = match &args.lang {
        Some(l) => Some(vec![l
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid language '{}'", l))?]),
        None => None,
    };

    let include_types = match &args.include_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --include-type: {e}"))?)
        }
        None => Some(cqs::parser::ChunkType::code_types()),
    };
    let exclude_types = match &args.exclude_type {
        Some(types) => {
            let parsed: Result<Vec<cqs::parser::ChunkType>, _> =
                types.iter().map(|t| t.parse()).collect();
            Some(parsed.map_err(|e| anyhow::anyhow!("Invalid --exclude-type: {e}"))?)
        }
        None => None,
    };

    let embedder = ctx.embedder()?;
    let query_embedding = embedder
        .embed_query(&args.query)
        .context("Failed to embed query")?;

    let limit = args.limit.clamp(1, 100);
    // P3 #100: shared rerank pool sizing.
    let effective_limit = if args.rerank {
        crate::cli::limits::rerank_pool_size(limit)
    } else {
        limit
    };

    // Classify query for per-category routing (SPLADE alpha + base/enriched index).
    let classification = cqs::search::router::classify_query(&args.query);
    let pre_centroid_cat = classification.category;
    let classification =
        cqs::search::router::reclassify_with_centroid(classification, query_embedding.as_slice());
    let centroid_applied = classification.category != pre_centroid_cat;

    // SPLADE alpha resolution (matches cmd_query semantics):
    //   --splade-alpha X : explicit constant α (sweeps, debug)
    //   otherwise        : per-category router
    //   --splade         : force on even for Unknown category
    let (use_splade, mut splade_alpha) = match args.splade_alpha {
        Some(alpha) => (true, alpha),
        None => (
            true,
            cqs::search::router::resolve_splade_alpha(&classification.category),
        ),
    };
    if centroid_applied {
        splade_alpha = splade_alpha.max(0.7);
    }
    // `args.splade` is retained for CLI parity but the per-category
    // router always runs on batch queries — classify_query always
    // returns a category (possibly Unknown), so router is always live.
    let _ = args.splade;

    // Phase 5: base/enriched index routing.
    let use_base = matches!(
        classification.strategy,
        cqs::search::router::SearchStrategy::DenseBase
    ) || std::env::var("CQS_FORCE_BASE_INDEX").as_deref() == Ok("1");

    let filter = cqs::SearchFilter {
        languages,
        include_types,
        exclude_types,
        path_pattern: args.path.clone(),
        name_boost: args.name_boost,
        query_text: args.query.clone(),
        enable_rrf: args.rrf,
        enable_demotion: !args.no_demote,
        enable_splade: use_splade,
        splade_alpha,
        type_boost_types: classification.type_hints.clone(),
        mmr_lambda: None, // Resolved by finalize_results via CQS_MMR_LAMBDA fallback.
    };
    filter.validate().map_err(|e| anyhow::anyhow!(e))?;

    // --ref scoped search: search only the named reference
    if let Some(ref ref_name) = args.ref_name {
        let ref_idx = crate::cli::commands::resolve::find_reference(&ctx.root, ref_name)?;
        // P3 #100: shared rerank pool sizing.
        let ref_limit = if args.rerank {
            crate::cli::limits::rerank_pool_size(limit)
        } else {
            limit
        };
        let threshold = args.threshold;
        let mut results = cqs::reference::search_reference(
            &ref_idx,
            &query_embedding,
            &filter,
            ref_limit,
            threshold,
            false,
        )?;

        // Re-rank ref results
        if args.rerank && results.len() > 1 {
            let reranker = ctx.reranker()?;
            reranker
                .rerank(&args.query, &mut results, limit)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        }

        let show_content = !args.no_content;
        let json_results: Vec<serde_json::Value> = results
            .iter()
            .map(|r| {
                serde_json::to_value(ChunkOutput::from_search_result(r, show_content))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %r.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": r.chunk.name})
                    })
            })
            .collect();

        return Ok(serde_json::json!({
            "results": json_results,
            "query": args.query,
            "total": json_results.len(),
            "source": ref_name,
        }));
    }

    // SPLADE sparse encoding (if enabled by --splade flag OR per-category routing)
    let splade_query = if use_splade {
        ctx.splade_encoder()
            .and_then(|enc| match enc.encode(&args.query) {
                Ok(sv) => Some(sv),
                Err(e) => {
                    tracing::warn!(error = %e, "SPLADE query encoding failed, falling back to cosine-only");
                    None
                }
            })
    } else {
        None
    };
    if use_splade {
        ctx.ensure_splade_index();
    }

    let audit_mode = ctx.audit_state();

    let index = if use_base {
        match ctx.base_vector_index()? {
            Some(base_idx) => {
                tracing::info!(
                    category = %classification.category,
                    "Router selected base HNSW for non-enriched query (batch)"
                );
                Some(base_idx)
            }
            None => {
                tracing::info!("Base HNSW unavailable — falling back to enriched index (batch)");
                ctx.vector_index()?
            }
        }
    } else {
        ctx.vector_index()?
    };
    let index = index.as_deref();

    // #1127: borrow_splade_index now returns Option<Arc<SpladeIndex>>; deref
    // through the Arc when handing the &SpladeIndex to search_hybrid.
    let splade_index_ref = ctx.borrow_splade_index();

    let splade_arg = splade_query
        .as_ref()
        .and_then(|sq| splade_index_ref.as_ref().map(|si| (si.as_ref(), sq)));

    let threshold = args.threshold;
    let results = if audit_mode.is_active() || splade_arg.is_some() {
        let code_results = ctx.store().search_hybrid(
            &query_embedding,
            &filter,
            effective_limit,
            threshold,
            index,
            splade_arg,
        )?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        ctx.store().search_unified_with_index(
            &query_embedding,
            &filter,
            effective_limit,
            threshold,
            index,
        )?
    };

    // Re-rank if requested
    let results = if args.rerank && results.len() > 1 {
        let mut code_results: Vec<cqs::store::SearchResult> = results
            .into_iter()
            .map(|r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr,
            })
            .collect();
        let reranker = ctx.reranker()?;
        reranker
            .rerank(&args.query, &mut code_results, limit)
            .map_err(|e| anyhow::anyhow!("Reranking failed: {e}"))?;
        code_results
            .into_iter()
            .map(cqs::store::UnifiedResult::Code)
            .collect()
    } else {
        results
    };

    // --include-refs: merge reference results. RM-V1.29-1: use the
    // BatchContext LRU so repeated `--include-refs` queries in a daemon
    // session don't rebuild every reference Store+HNSW per call. The
    // rayon call below uses the default global pool — the old sequential
    // fallback that built a fresh 4-thread pool per query is gone.
    let results = if args.include_refs {
        let references = ctx.get_all_refs()?;
        if !references.is_empty() {
            use rayon::prelude::*;
            let ref_results: Vec<_> = references
                .par_iter()
                .filter_map(|ref_idx| {
                    match cqs::reference::search_reference(
                        ref_idx,
                        &query_embedding,
                        &filter,
                        limit,
                        threshold,
                        true,
                    ) {
                        Ok(r) if !r.is_empty() => Some((ref_idx.name.clone(), r)),
                        Err(e) => {
                            tracing::warn!(reference = %ref_idx.name, error = %e, "Reference search failed");
                            None
                        }
                        _ => None,
                    }
                })
                .collect();
            let tagged = cqs::reference::merge_results(results, ref_results, limit);
            tagged.into_iter().map(|t| t.result).collect()
        } else {
            results
        }
    } else {
        results
    };

    // Token-budget packing (shared with CLI search)
    let (results, token_info) = if let Some(budget) = args.tokens {
        let embedder = ctx.embedder()?;
        crate::cli::commands::token_pack_results(
            results,
            budget,
            crate::cli::commands::JSON_OVERHEAD_PER_RESULT,
            embedder,
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.chunk.content.as_str(),
            },
            |r| match r {
                cqs::store::UnifiedResult::Code(sr) => sr.score,
            },
            "batch_search",
        )
    } else {
        (results, None)
    };

    let show_content = !args.no_content;
    let json_results: Vec<serde_json::Value> = results
        .iter()
        .map(|r| match r {
            cqs::store::UnifiedResult::Code(sr) => {
                serde_json::to_value(ChunkOutput::from_search_result(sr, show_content))
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, name = %sr.chunk.name, "ChunkOutput serialization failed (NaN score?)");
                        serde_json::json!({"error": "serialization failed", "name": sr.chunk.name})
                    })
            }
        })
        .collect();

    let mut response = serde_json::json!({
        "results": json_results,
        "query": args.query,
        "total": json_results.len(),
    });
    crate::cli::commands::inject_token_info(&mut response, token_info);
    Ok(response)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

// TC-HP-7 (issue #973): content-asserting tests for `dispatch_search`.
//
// The batch `tests/cli_batch_test.rs` integration tests only assert schema
// (field names, non-empty arrays), so a regression that returned zeros or the
// wrong chunk would slip through. These tests exercise `dispatch_search`
// directly against a fixture store and assert the returned chunk content.
//
// The tests live inside the crate (not `tests/`) because `dispatch_search` is
// `pub(in crate::cli::batch)`, not reachable from an external integration
// test. Staying in-process is critical: `tests/cli_batch_test.rs` is gated
// behind `slow-tests` because it shells out to `cqs` and cold-loads the ONNX
// stack (~2 hours in CI). These tests use the in-process fixture pattern
// (build `Store` + `BatchContext`, call the handler directly) and each runs
// in well under a second because they exercise the `--name-only` branch that
// skips embedder init entirely.
//
// Coverage:
// - `--name-only` branch (dispatch_search:41-60): exact, prefix, substring,
//   no-match, and per-language content assertions.
// - `--include-type` parsing (dispatch_search:82-97): invalid type name path
//   returns an error (exercised before the embedder is touched, so still fast).
// - `--exclude-type` parsing (dispatch_search:90-97): same.
//
// The semantic search branch (post-line-62) requires a real ONNX embedder and
// is covered by the eval suite (`tests/eval_test.rs`, `#[ignore]`), not here.
#[cfg(test)]
mod tests {
    use super::*;
    // Inside `mod tests` in handlers/search.rs the super chain is:
    //   super              -> search module (this file)
    //   super::super       -> handlers module (handlers/mod.rs)
    //   super::super::super -> batch module (batch/mod.rs)
    // `commands` is a private sibling of `handlers`, unreachable via
    // `crate::cli::batch::commands`, but the super chain does reach it.
    use super::super::super::commands::{BatchCmd, BatchInput};
    use super::super::super::{create_test_context, BatchContext};
    use clap::Parser;
    use cqs::embedder::Embedding;
    use cqs::parser::{Chunk, ChunkType, Language};
    use cqs::store::{ModelInfo, Store};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a test Chunk with the usual test defaults.
    fn make_chunk(
        id: &str,
        file: &str,
        language: Language,
        chunk_type: ChunkType,
        name: &str,
        signature: &str,
        content: &str,
    ) -> Chunk {
        let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        Chunk {
            id: id.to_string(),
            file: PathBuf::from(file),
            language,
            chunk_type,
            name: name.to_string(),
            signature: signature.to_string(),
            content: content.to_string(),
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
            parser_version: 0,
        }
    }

    /// Build a test BatchContext pre-populated with the given chunks.
    ///
    /// Opens the Store ONCE per test, inits, batch-inserts all chunks in a
    /// single `upsert_chunks_batch` call, then drops — so the transaction
    /// overhead amortizes across all chunks.
    ///
    /// Runtime note: on WSL `/mnt/c` (NTFS over 9P) a single
    /// `upsert_chunks_batch` with a non-empty batch takes ~20s. This is a
    /// pre-existing environmental slowness that affects every Store-write
    /// test in the crate — e.g. `store::chunks::crud::tests::
    /// test_upsert_chunks_batch_insert_and_fetch` has the same profile. On
    /// Linux ext4 (CI) the same test completes in <100ms. Don't refactor
    /// this helper to "fix" the runtime — the fix belongs in the SQLite/
    /// sqlx/WSL layer, not in each test.
    fn ctx_with_chunks(chunks: Vec<Chunk>) -> (TempDir, BatchContext) {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let cqs_dir = dir.path().join(".cqs");
        std::fs::create_dir_all(&cqs_dir).expect("Failed to create .cqs dir");
        let index_path = cqs_dir.join("index.db");

        // Unit embedding: `upsert_chunk` validates dimension against
        // `ModelInfo::default()`. Content of the embedding vector doesn't
        // matter for the name-only branch under test — only the chunks
        // table + FTS index are consulted.
        let mut emb_vec = vec![0.0_f32; cqs::EMBEDDING_DIM];
        emb_vec[0] = 1.0;
        let embedding = Embedding::new(emb_vec);

        {
            let store = Store::open(&index_path).expect("Failed to open test store");
            store
                .init(&ModelInfo::default())
                .expect("Failed to init test store");
            // Batch all inserts in one call so we pay the transaction setup
            // cost once, not per chunk.
            if !chunks.is_empty() {
                let pairs: Vec<(Chunk, Embedding)> = chunks
                    .iter()
                    .map(|c| (c.clone(), embedding.clone()))
                    .collect();
                store
                    .upsert_chunks_batch(&pairs, Some(0))
                    .expect("upsert_chunks_batch failed");
            }
        } // drop store to flush WAL

        let ctx = create_test_context(&cqs_dir).expect("Failed to create test context");
        (dir, ctx)
    }

    /// Build a BatchContext with a single chunk. Thin wrapper around
    /// `ctx_with_chunks` for tests that only need one.
    fn ctx_with_chunk(
        id: &str,
        file: &str,
        language: Language,
        chunk_type: ChunkType,
        name: &str,
        signature: &str,
        content: &str,
    ) -> (TempDir, BatchContext) {
        ctx_with_chunks(vec![make_chunk(
            id, file, language, chunk_type, name, signature, content,
        )])
    }

    /// Build a BatchContext with zero chunks. For tests that only exercise
    /// the error-path parsing branches of `dispatch_search`.
    fn empty_ctx() -> (TempDir, BatchContext) {
        ctx_with_chunks(vec![])
    }

    /// Parse a `SearchArgs` by running the same clap pipeline as the daemon.
    /// This guarantees we hit exactly the defaults the production code sees,
    /// instead of hardcoding field values that could drift from the `Args`
    /// attributes.
    fn parse_search_args(cli_args: &[&str]) -> crate::cli::args::SearchArgs {
        let mut full = vec!["search"];
        full.extend_from_slice(cli_args);
        let input = BatchInput::try_parse_from(&full).expect("clap parse failed");
        match input.cmd {
            BatchCmd::Search { args, .. } => args,
            other => panic!("Expected Search, got {:?}", other),
        }
    }

    /// Issue #973 / TC-HP-7a: exact-name `--name-only` query returns the
    /// matching chunk as the *top* result with a deterministic score of 1.0.
    ///
    /// Adversarial contract: the test fails if a different chunk sorts first
    /// — catches a sort/scoring regression in `search_by_name`.
    #[test]
    fn test_dispatch_search_name_only_exact_match_top_result() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:aaaa0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "process_data",
                "fn process_data(input: &str) -> String",
                "fn process_data(input: &str) -> String { input.to_uppercase() }",
            ),
            make_chunk(
                "src/lib.rs:7:aaaa0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "unrelated_helper",
                "fn unrelated_helper()",
                "fn unrelated_helper() { println!(\"noop\"); }",
            ),
        ]);

        let args = parse_search_args(&["process_data", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        assert_eq!(json["query"], "process_data");
        assert_eq!(json["total"], 1, "Expected exactly 1 matching chunk");
        let results = json["results"].as_array().expect("results is array");
        assert_eq!(results.len(), 1, "results.len() must match total");
        assert_eq!(
            results[0]["name"], "process_data",
            "Top result must be the exact-name match, not '{}'",
            results[0]["name"]
        );
        let score = results[0]["score"]
            .as_f64()
            .expect("score is finite number");
        assert!(
            (score - 1.0).abs() < 1e-6,
            "Exact-name match should score 1.0, got {score}. A regression in \
             score_name_match_pre_lower or the sort in search_by_name would \
             break this."
        );
        assert_eq!(results[0]["chunk_type"], "function");
        assert_eq!(results[0]["language"], "rust");
    }

    /// Issue #973 / TC-HP-7b: prefix-match `--name-only` query returns the
    /// prefixed chunk with score 0.9 (from `score_name_match_pre_lower`).
    /// Ensures the FTS5 prefix-match (`name:"parse"*`) path actually fires.
    #[test]
    fn test_dispatch_search_name_only_prefix_match_ranks_first() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/parse.rs:1:bbbb0001",
                "src/parse.rs",
                Language::Rust,
                ChunkType::Function,
                "parse_config",
                "fn parse_config() -> Config",
                "fn parse_config() -> Config { Config::default() }",
            ),
            make_chunk(
                "src/lib.rs:1:bbbb0002",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "do_parse_config",
                "fn do_parse_config()",
                "fn do_parse_config() { parse_config(); }",
            ),
        ]);

        // "parse" is a prefix of parse_config (score 0.9) and a substring of
        // do_parse_config (score 0.7). The prefix-match must rank first.
        let args = parse_search_args(&["parse", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        let results = json["results"].as_array().expect("results is array");
        assert!(
            !results.is_empty(),
            "Expected at least one match for 'parse' prefix, got {}",
            results.len()
        );
        assert_eq!(
            results[0]["name"], "parse_config",
            "Prefix match (0.9) must outrank substring match (0.7); got '{}' first",
            results[0]["name"]
        );
        let top_score = results[0]["score"].as_f64().unwrap();
        assert!(
            (top_score - 0.9).abs() < 1e-6,
            "Prefix match should score 0.9, got {top_score}"
        );

        // If do_parse_config is also in the results, it must rank below.
        if results.len() > 1 {
            assert_eq!(
                results[1]["name"], "do_parse_config",
                "Second result should be the substring match"
            );
            let second = results[1]["score"].as_f64().unwrap();
            assert!(
                second < top_score,
                "Substring (score={second}) must rank below prefix (score={top_score})"
            );
        }
    }

    /// Issue #973 / TC-HP-7c: `--name-only --limit N` honours the limit
    /// *and* clamps out-of-range values via `limit.clamp(1, 100)`. A regression
    /// that passed the raw limit through would return unlimited rows.
    #[test]
    fn test_dispatch_search_name_only_limit_clamp() {
        let chunks: Vec<Chunk> = (0..10)
            .map(|i| {
                make_chunk(
                    &format!("src/lib.rs:{i}:cccc{i:04}"),
                    "src/lib.rs",
                    Language::Rust,
                    ChunkType::Function,
                    &format!("handler_{i}"),
                    &format!("fn handler_{i}()"),
                    &format!("fn handler_{i}() {{}}"),
                )
            })
            .collect();
        let (_dir, ctx) = ctx_with_chunks(chunks);

        // Default limit is 5 (per SearchArgs `#[arg(default_value = "5")]`).
        let default = parse_search_args(&["handler", "--name-only"]);
        let json =
            dispatch_search(&ctx.build_view(None), &default).expect("dispatch_search failed");
        let results = json["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            5,
            "Default limit=5 must bound results; got {} with total={}",
            results.len(),
            json["total"]
        );
        assert_eq!(json["total"], 5, "total must equal results.len()");
        for r in results {
            let name = r["name"].as_str().unwrap();
            assert!(
                name.starts_with("handler_"),
                "All results must be handler_* prefix matches, got '{name}'"
            );
        }

        // Explicit limit=3 narrows further.
        let three = parse_search_args(&["handler", "--name-only", "--limit", "3"]);
        let json = dispatch_search(&ctx.build_view(None), &three).expect("dispatch_search failed");
        assert_eq!(json["total"], 3);
        assert_eq!(json["results"].as_array().unwrap().len(), 3);
    }

    /// Issue #973 / TC-HP-7d: no-match `--name-only` query returns empty
    /// results with `total: 0`. The schema must still be present — callers
    /// rely on `results[]` / `total` keys existing regardless of row count.
    ///
    /// This is the adversarial case the issue requires: a silent regression
    /// that returned the wrong chunk for a no-match query would violate this.
    #[test]
    fn test_dispatch_search_name_only_no_match_returns_empty() {
        let (_dir, ctx) = ctx_with_chunk(
            "src/lib.rs:1:dddd0001",
            "src/lib.rs",
            Language::Rust,
            ChunkType::Function,
            "alpha",
            "fn alpha()",
            "fn alpha() {}",
        );

        let args = parse_search_args(&["zxyvwu_no_such_name", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        assert_eq!(json["query"], "zxyvwu_no_such_name");
        assert_eq!(json["total"], 0, "No-match query must return total=0");
        assert_eq!(
            json["results"].as_array().unwrap().len(),
            0,
            "Empty results array must be present (callers depend on schema)"
        );
    }

    /// Issue #973 / TC-HP-7e: name-only returns chunks from every inserted
    /// language. Covers `ChunkOutput::from_search_result` rendering for
    /// non-Rust languages — a refactor that special-cased Rust or dropped
    /// the `language` field would break this.
    #[test]
    fn test_dispatch_search_name_only_cross_language_content() {
        let (_dir, ctx) = ctx_with_chunks(vec![
            make_chunk(
                "src/lib.rs:1:eeee0001",
                "src/lib.rs",
                Language::Rust,
                ChunkType::Function,
                "validate_input",
                "fn validate_input()",
                "fn validate_input() {}",
            ),
            make_chunk(
                "src/app.py:1:eeee0002",
                "src/app.py",
                Language::Python,
                ChunkType::Function,
                "validate_input",
                "def validate_input()",
                "def validate_input():\n    pass",
            ),
        ]);

        let args = parse_search_args(&["validate_input", "--name-only"]);
        let json = dispatch_search(&ctx.build_view(None), &args).expect("dispatch_search failed");

        let results = json["results"].as_array().unwrap();
        assert_eq!(
            json["total"], 2,
            "Expected both Rust and Python 'validate_input' chunks"
        );
        assert_eq!(results.len(), 2);

        // Both must have name=validate_input, score=1.0 (exact match).
        let languages: std::collections::HashSet<&str> = results
            .iter()
            .map(|r| r["language"].as_str().unwrap())
            .collect();
        assert!(
            languages.contains("rust"),
            "Rust result missing from {languages:?}"
        );
        assert!(
            languages.contains("python"),
            "Python result missing from {languages:?}"
        );
        for r in results {
            assert_eq!(r["name"], "validate_input");
            let score = r["score"].as_f64().unwrap();
            assert!(
                (score - 1.0).abs() < 1e-6,
                "Exact match on {} should score 1.0, got {score}",
                r["language"]
            );
        }
    }

    /// Issue #973 / TC-HP-7f: `--include-type` with an invalid chunk type
    /// name returns an error at the parsing boundary (dispatch_search:82-87),
    /// *before* the embedder is touched. This guards the
    /// `ChunkType::from_str` pipeline that refactors have historically
    /// broken by regressing the `FromStr` impl or the CQ-5 alias handling.
    ///
    /// We intentionally don't assert embedder output — the embedder path is
    /// not reached. The test is still content-asserting: it asserts the
    /// error message contains the offending input.
    #[test]
    fn test_dispatch_search_invalid_include_type_errors_fast() {
        let (_dir, ctx) = empty_ctx();
        let args = parse_search_args(&["anything", "--include-type", "not_a_real_type"]);
        let err = dispatch_search(&ctx.build_view(None), &args)
            .expect_err("Invalid --include-type must error, not silently return all types");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Invalid --include-type"),
            "Error message must reference --include-type flag, got: {msg}"
        );
        assert!(
            msg.contains("not_a_real_type"),
            "Error must surface the offending input, got: {msg}"
        );
    }

    /// Issue #973 / TC-HP-7g: `--exclude-type` with an invalid chunk type
    /// name errors symmetrically with `--include-type`. The exclude path
    /// is a common forgotten mirror in refactors that rename the include
    /// branch.
    #[test]
    fn test_dispatch_search_invalid_exclude_type_errors_fast() {
        let (_dir, ctx) = empty_ctx();
        let args = parse_search_args(&["anything", "--exclude-type", "bogusbogus"]);
        let err = dispatch_search(&ctx.build_view(None), &args)
            .expect_err("Invalid --exclude-type must error, not silently accept");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("Invalid --exclude-type"),
            "Error message must reference --exclude-type flag, got: {msg}"
        );
        assert!(
            msg.contains("bogusbogus"),
            "Error must surface the offending input, got: {msg}"
        );
    }
}
