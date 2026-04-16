//! Pipeline evaluation — tests the full search scoring pipeline.
//!
//! Unlike model_eval (in-memory cosine only), this tests:
//! - Store-based search (search_filtered)
//! - HNSW-guided search (search_filtered_with_index)
//! - RRF fusion (keyword + semantic)
//! - Name boost blending
//!
//! Run with: cargo test pipeline_eval -- --ignored --nocapture

mod eval_common;

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::hnsw::HnswIndex;
use cqs::parser::{ChunkType, Language, Parser};
use cqs::store::{ModelInfo, SearchFilter, Store};
use cqs::VectorIndex;
use cqs::{generate_nl_description, generate_nl_with_call_context, CallContext};
use eval_common::{fixture_path, hard_fixture_path, EvalCase, HARD_EVAL_CASES, HOLDOUT_EVAL_CASES};
use std::collections::HashMap;
use tempfile::TempDir;

/// Simplified call graph enrichment for stress eval.
/// Re-embeds chunks that have callers/callees in the function_calls table.
fn enrich_with_call_context(store: &Store, embedder: &Embedder) -> usize {
    let stats = store.stats().expect("stats");
    let total_chunks = stats.total_chunks as f32;
    if total_chunks < 1.0 {
        return 0;
    }

    let callee_freq = store.callee_caller_counts().unwrap_or_default();
    let callee_doc_freq: HashMap<String, f32> = callee_freq
        .into_iter()
        .map(|(name, count)| (name, count as f32 / total_chunks))
        .collect();

    let identities = store.all_chunk_identities().unwrap_or_default();
    let all_names: Vec<&str> = identities.iter().map(|ci| ci.name.as_str()).collect();
    let callers_map = store.get_callers_full_batch(&all_names).unwrap_or_default();
    let callees_map = store.get_callees_full_batch(&all_names).unwrap_or_default();

    let mut enriched = 0usize;
    let mut cursor = 0i64;

    loop {
        let (chunks, next) = store.chunks_paged(cursor, 500).expect("paged");
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        let mut batch: Vec<(String, String)> = Vec::new();
        for cs in &chunks {
            let callers = callers_map.get(&cs.name);
            let callees = callees_map.get(&cs.name);
            if callers.is_none_or(|v| v.is_empty()) && callees.is_none_or(|v| v.is_empty()) {
                continue;
            }

            let ctx = CallContext {
                callers: callers
                    .map(|v| v.iter().map(|c| c.name.clone()).collect())
                    .unwrap_or_default(),
                callees: callees
                    .map(|v| v.iter().map(|(name, _)| name.clone()).collect())
                    .unwrap_or_default(),
            };

            let chunk: cqs::parser::Chunk = cs.into();
            let nl = generate_nl_with_call_context(&chunk, &ctx, &callee_doc_freq, 5, 5);
            batch.push((cs.id.clone(), nl));

            if batch.len() >= 64 {
                let texts: Vec<&str> = batch.iter().map(|(_, nl)| nl.as_str()).collect();
                let embs = embedder.embed_documents(&texts).expect("embed");
                let updates: Vec<(String, Embedding)> = batch
                    .drain(..)
                    .zip(embs)
                    .map(|((id, _), emb)| (id, emb))
                    .collect();
                store.update_embeddings_batch(&updates).expect("update");
                enriched += updates.len();
            }
        }

        if !batch.is_empty() {
            let texts: Vec<&str> = batch.iter().map(|(_, nl)| nl.as_str()).collect();
            let embs = embedder.embed_documents(&texts).expect("embed");
            let updates: Vec<(String, Embedding)> = batch
                .drain(..)
                .zip(embs)
                .map(|((id, _), emb)| (id, emb))
                .collect();
            store.update_embeddings_batch(&updates).expect("update");
            enriched += updates.len();
        }
    }

    enriched
}

/// Languages tested in the pipeline eval
fn eval_languages() -> Vec<Language> {
    let mut langs = vec![
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
        Language::Java,
    ];
    #[cfg(feature = "lang-php")]
    langs.push(Language::Php);
    langs
}

/// Metrics for a single scoring configuration
struct ConfigMetrics {
    name: &'static str,
    recall_at_1: f64,
    recall_at_5: f64,
    mrr: f64,
    per_lang_mrr: HashMap<Language, f64>,
    /// Relaxed recall@1: accepts `also_accept` alternatives
    relaxed_recall_at_1: f64,
}

/// Check if a result name matches an eval case (strict or relaxed).
fn matches_case(result_name: &str, case: &EvalCase, relaxed: bool) -> bool {
    if result_name == case.expected_name {
        return true;
    }
    if relaxed {
        return case.also_accept.contains(&result_name);
    }
    false
}

/// Find rank of expected name in results (1-indexed). Returns strict and relaxed ranks.
fn find_ranks(
    results: &[cqs::store::SearchResult],
    case: &EvalCase,
) -> (Option<usize>, Option<usize>) {
    let strict = results
        .iter()
        .position(|r| r.chunk.name == case.expected_name)
        .map(|p| p + 1);
    let relaxed = results
        .iter()
        .position(|r| matches_case(&r.chunk.name, case, true))
        .map(|p| p + 1);
    (strict, relaxed)
}

/// Compute metrics from search results for a set of eval cases.
///
/// For each case, finds the rank of `expected_name` in results (1-indexed).
/// Returns (recall@1, recall@5, MRR, per-language MRR, relaxed_recall@1).
fn compute_metrics_refs(
    results_per_case: &[(usize, Option<usize>, Option<usize>)],
    cases: &[&EvalCase],
) -> (f64, f64, f64, HashMap<Language, f64>, f64) {
    // Delegate — cases[idx] works identically for &[&EvalCase]
    let total = results_per_case.len() as f64;
    if total == 0.0 {
        return (0.0, 0.0, 0.0, HashMap::new(), 0.0);
    }

    let mut hits_at_1 = 0usize;
    let mut hits_at_5 = 0usize;
    let mut total_rr = 0.0f64;
    let mut relaxed_hits_at_1 = 0usize;
    let mut lang_rr: HashMap<Language, (f64, usize)> = HashMap::new();

    for &(case_idx, rank, relaxed_rank) in results_per_case {
        let lang = cases[case_idx].language;
        let entry = lang_rr.entry(lang).or_insert((0.0, 0));
        entry.1 += 1;

        if let Some(r) = rank {
            if r == 1 {
                hits_at_1 += 1;
            }
            if r <= 5 {
                hits_at_5 += 1;
            }
            let rr = 1.0 / r as f64;
            total_rr += rr;
            entry.0 += rr;
        }

        if let Some(r) = relaxed_rank {
            if r == 1 {
                relaxed_hits_at_1 += 1;
            }
        }
    }

    let recall_1 = hits_at_1 as f64 / total;
    let recall_5 = hits_at_5 as f64 / total;
    let mrr = total_rr / total;
    let relaxed_r1 = relaxed_hits_at_1 as f64 / total;

    let per_lang: HashMap<Language, f64> = lang_rr
        .into_iter()
        .map(|(lang, (rr_sum, count))| {
            let lang_mrr = if count > 0 {
                rr_sum / count as f64
            } else {
                0.0
            };
            (lang, lang_mrr)
        })
        .collect();

    (recall_1, recall_5, mrr, per_lang, relaxed_r1)
}

fn compute_metrics(
    results_per_case: &[(usize, Option<usize>, Option<usize>)], // (case_index, strict_rank, relaxed_rank)
    cases: &[EvalCase],
) -> (f64, f64, f64, HashMap<Language, f64>, f64) {
    let total = results_per_case.len() as f64;
    if total == 0.0 {
        return (0.0, 0.0, 0.0, HashMap::new(), 0.0);
    }

    let mut hits_at_1 = 0usize;
    let mut hits_at_5 = 0usize;
    let mut total_rr = 0.0f64;
    let mut relaxed_hits_at_1 = 0usize;
    let mut lang_rr: HashMap<Language, (f64, usize)> = HashMap::new();

    for &(case_idx, rank, relaxed_rank) in results_per_case {
        let lang = cases[case_idx].language;
        let entry = lang_rr.entry(lang).or_insert((0.0, 0));
        entry.1 += 1;

        if let Some(r) = rank {
            if r == 1 {
                hits_at_1 += 1;
            }
            if r <= 5 {
                hits_at_5 += 1;
            }
            let rr = 1.0 / r as f64;
            total_rr += rr;
            entry.0 += rr;
        }

        if let Some(r) = relaxed_rank {
            if r == 1 {
                relaxed_hits_at_1 += 1;
            }
        }
    }

    let recall_1 = hits_at_1 as f64 / total;
    let recall_5 = hits_at_5 as f64 / total;
    let mrr = total_rr / total;
    let relaxed_r1 = relaxed_hits_at_1 as f64 / total;

    let per_lang: HashMap<Language, f64> = lang_rr
        .into_iter()
        .map(|(lang, (rr_sum, count))| {
            let lang_mrr = if count > 0 {
                rr_sum / count as f64
            } else {
                0.0
            };
            (lang, lang_mrr)
        })
        .collect();

    (recall_1, recall_5, mrr, per_lang, relaxed_r1)
}

#[test]
#[ignore] // Slow - needs embedding. Run with: cargo test fixture_eval_296q -- --ignored --nocapture
fn test_fixture_eval_296q() {
    // === Setup ===
    eprintln!("Initializing embedder...");
    let embedder =
        Embedder::new(ModelConfig::resolve(None, None)).expect("Failed to initialize embedder");
    let parser = Parser::new().expect("Failed to initialize parser");

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("pipeline_eval.db");
    let mut store = Store::open(&db_path).unwrap();
    let mc = ModelConfig::resolve(None, None);
    // Warm up embedder to detect actual dim (CQS_ONNX_DIR may override model)
    embedder.warm().expect("Failed to warm embedder");
    let actual_dim = embedder.embedding_dim();
    store.init(&ModelInfo::new(&mc.repo, actual_dim)).unwrap();
    store.set_dim(actual_dim);

    // Parse and index both original AND hard fixtures for all languages
    eprintln!("Parsing and indexing fixtures...");
    let mut chunk_count = 0;

    for lang in eval_languages() {
        for path in [fixture_path(lang), hard_fixture_path(lang)] {
            if !path.exists() {
                continue;
            }
            eprintln!("  Parsing {:?}...", path);
            let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
            eprintln!("    Found {} chunks", chunks.len());

            for chunk in &chunks {
                let text = generate_nl_description(chunk);
                let embeddings = embedder
                    .embed_documents(&[&text])
                    .expect("Failed to embed chunk");
                let embedding = embeddings.into_iter().next().unwrap();
                store
                    .upsert_chunk(chunk, &embedding, None)
                    .expect("Failed to store chunk");
                chunk_count += 1;
            }
        }
    }
    eprintln!("Indexed {} total chunks\n", chunk_count);

    // SPLADE encoding (if model available)
    let splade_model_dir = dirs::home_dir()
        .map(|h| h.join(".cache/huggingface/splade-onnx"))
        .unwrap_or_default();
    let splade_index = if splade_model_dir.join("model.onnx").exists() {
        eprintln!("Encoding SPLADE sparse vectors...");
        match cqs::splade::SpladeEncoder::new(&splade_model_dir, 0.01) {
            Ok(encoder) => {
                let texts = store.chunk_splade_texts().unwrap_or_default();
                let mut sparse_vecs = Vec::new();
                for (id, text) in &texts {
                    if let Ok(sv) = encoder.encode(text) {
                        if !sv.is_empty() {
                            sparse_vecs.push((id.clone(), sv));
                        }
                    }
                }
                if !sparse_vecs.is_empty() {
                    store.upsert_sparse_vectors(&sparse_vecs).unwrap();
                }
                let loaded = store.load_all_sparse_vectors().unwrap();
                let idx = cqs::splade::index::SpladeIndex::build(loaded);
                eprintln!("  SPLADE index: {} chunks\n", idx.len());
                Some((encoder, idx))
            }
            Err(e) => {
                eprintln!("  SPLADE encoder unavailable: {e}\n");
                None
            }
        }
    } else {
        eprintln!("SPLADE model not found, skipping SPLADE configs\n");
        None
    };

    // Build HNSW index from the store
    eprintln!("Building HNSW index...");
    let chunk_total = store.chunk_count().unwrap() as usize;
    let hnsw = HnswIndex::build_batched_with_dim(
        store.embedding_batches(10_000),
        chunk_total,
        store.dim(),
    )
    .expect("Failed to build HNSW index");
    eprintln!("  HNSW index: {} vectors\n", hnsw.len());

    // Combine hard + holdout cases for unified eval
    let all_cases: Vec<&EvalCase> = HARD_EVAL_CASES
        .iter()
        .chain(HOLDOUT_EVAL_CASES.iter())
        .collect();

    // Pre-embed all queries
    eprintln!(
        "Embedding {} queries ({} hard + {} holdout)...",
        all_cases.len(),
        HARD_EVAL_CASES.len(),
        HOLDOUT_EVAL_CASES.len()
    );
    let query_embeddings: Vec<_> = all_cases
        .iter()
        .map(|case| {
            embedder
                .embed_query(case.query)
                .expect("Failed to embed query")
        })
        .collect();
    eprintln!("  Done.\n");

    // === Run 4 scoring configs ===

    let mut all_metrics: Vec<ConfigMetrics> = Vec::new();

    // Per-query diagnostics: collect structured results for JSON dump
    let mut query_diagnostics: Vec<serde_json::Value> = Vec::new();

    // Config A: Cosine-only (brute-force, baseline)
    {
        eprintln!("--- Config A: Cosine-only ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                ..Default::default()
            };
            let results = match store.search_filtered(&query_embeddings[i], &filter, 10, 0.0) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  ! [{:?}] \"{}\" -> SEARCH ERROR: {}",
                        case.language, case.query, e
                    );
                    results_per_case.push((i, None, None));
                    query_diagnostics.push(serde_json::json!({
                        "query": case.query,
                        "language": format!("{:?}", case.language),
                        "expected": case.expected_name,
                        "error": e.to_string(),
                    }));
                    continue;
                }
            };

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top5: Vec<serde_json::Value> = results
                .iter()
                .take(5)
                .map(|r| {
                    serde_json::json!({
                        "name": r.chunk.name,
                        "score": (r.score * 10000.0).round() / 10000.0,
                        "file": r.chunk.file.display().to_string(),
                    })
                })
                .collect();
            let top3_names: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3_names
            );

            query_diagnostics.push(serde_json::json!({
                "query": case.query,
                "language": format!("{:?}", case.language),
                "expected": case.expected_name,
                "rank": rank,
                "relaxed_rank": relaxed_rank,
                "status": status,
                "top5": top5,
            }));

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "A: Cosine-only",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config B: RRF (brute-force + keyword fusion)
    {
        eprintln!("\n--- Config B: RRF ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_rrf: true,
                query_text: case.query.to_string(),
                ..Default::default()
            };
            let results = match store.search_filtered(&query_embeddings[i], &filter, 10, 0.0) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  ! [{:?}] \"{}\" -> SEARCH ERROR: {}",
                        case.language, case.query, e
                    );
                    results_per_case.push((i, None, None));
                    continue;
                }
            };

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3
            );

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "B: RRF",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config C: RRF + name_boost (full brute-force pipeline)
    {
        eprintln!("\n--- Config C: RRF + name_boost ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_rrf: true,
                name_boost: 0.2,
                query_text: case.query.to_string(),
                ..Default::default()
            };
            let results = match store.search_filtered(&query_embeddings[i], &filter, 10, 0.0) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  ! [{:?}] \"{}\" -> SEARCH ERROR: {}",
                        case.language, case.query, e
                    );
                    results_per_case.push((i, None, None));
                    continue;
                }
            };

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3
            );

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "C: RRF + name_boost",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config D: HNSW-guided + name_boost (production path)
    {
        eprintln!("\n--- Config D: HNSW + name_boost ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                name_boost: 0.2,
                query_text: case.query.to_string(),
                ..Default::default()
            };
            let results = store
                .search_filtered_with_index(
                    &query_embeddings[i],
                    &filter,
                    10,
                    0.0,
                    Some(&hnsw as &dyn VectorIndex),
                )
                .expect("Search failed");

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3
            );

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "D: HNSW + name_boost",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config E: Cosine + demotion (measures demotion effect on cosine baseline)
    {
        eprintln!("\n--- Config E: Cosine + demotion ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_demotion: true,
                ..Default::default()
            };
            let results = match store.search_filtered(&query_embeddings[i], &filter, 10, 0.0) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  ! [{:?}] \"{}\" -> SEARCH ERROR: {}",
                        case.language, case.query, e
                    );
                    results_per_case.push((i, None, None));
                    continue;
                }
            };

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3
            );

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "E: Cosine + demotion",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config F: HNSW + name_boost + demotion (production path with demotion)
    {
        eprintln!("\n--- Config F: HNSW + name_boost + demote ---");
        let mut results_per_case = Vec::new();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                name_boost: 0.2,
                query_text: case.query.to_string(),
                enable_demotion: true,
                ..Default::default()
            };
            let results = store
                .search_filtered_with_index(
                    &query_embeddings[i],
                    &filter,
                    10,
                    0.0,
                    Some(&hnsw as &dyn VectorIndex),
                )
                .expect("Search failed");

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                top3
            );

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "F: HNSW + boost + demote",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // Config G: SPLADE rerank (cosine retrieval + SPLADE re-ranking, α=0)
    if let Some((ref splade_enc, ref splade_idx)) = splade_index {
        eprintln!("\n--- Config G: SPLADE rerank ---");
        let mut results_per_case = Vec::new();

        // Pre-encode SPLADE queries
        let splade_queries: Vec<_> = all_cases
            .iter()
            .map(|case| splade_enc.encode(case.query).unwrap_or_default())
            .collect();

        for (i, case) in all_cases.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_splade: true,
                splade_alpha: 0.0, // pure rerank
                ..Default::default()
            };
            let splade_arg = Some((splade_idx, &splade_queries[i]));
            let results = match store.search_hybrid(
                &query_embeddings[i],
                &filter,
                10,
                0.0,
                Some(&hnsw),
                splade_arg,
            ) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!(
                        "  ! [{:?}] \"{}\" -> SEARCH ERROR: {}",
                        case.language, case.query, e
                    );
                    results_per_case.push((i, None, None));
                    continue;
                }
            };

            let (rank, relaxed_rank) = find_ranks(&results, case);
            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            let top3_names: Vec<&str> = results
                .iter()
                .take(3)
                .map(|r| r.chunk.name.as_str())
                .collect();
            eprintln!(
                "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                status,
                case.language,
                case.query,
                case.expected_name,
                rank.map_or("miss".to_string(), |r| r.to_string()),
                top3_names
            );
            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics_refs(&results_per_case, &all_cases);
        all_metrics.push(ConfigMetrics {
            name: "G: SPLADE rerank",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
            relaxed_recall_at_1: relaxed_r1,
        });
    }

    // === Print comparison table ===
    eprintln!(
        "\n=== Pipeline Scoring Comparison ({} queries: {} hard + {} holdout) ===\n",
        all_cases.len(),
        HARD_EVAL_CASES.len(),
        HOLDOUT_EVAL_CASES.len()
    );
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10} {:>12}",
        "Config", "Recall@1", "Recall@5", "MRR", "Relaxed R@1"
    );
    eprintln!("{}", "-".repeat(68));
    for m in &all_metrics {
        eprintln!(
            "{:<25} {:>9.1}% {:>9.1}% {:>10.4} {:>11.1}%",
            m.name,
            m.recall_at_1 * 100.0,
            m.recall_at_5 * 100.0,
            m.mrr,
            m.relaxed_recall_at_1 * 100.0,
        );
    }

    // Per-language MRR table
    let langs = eval_languages();
    eprintln!("\n=== Per-Language MRR ===\n");
    {
        let mut header = format!("{:<25}", "Config");
        for lang in &langs {
            header += &format!(" {:>8}", format!("{:?}", lang));
        }
        eprintln!("{}", header);
    }
    eprintln!("{}", "-".repeat(25 + 9 * langs.len()));
    for m in &all_metrics {
        let mut row = format!("{:<25}", m.name);
        for lang in &langs {
            let lang_mrr = m.per_lang_mrr.get(lang).copied().unwrap_or(0.0);
            row += &format!(" {:>8.4}", lang_mrr);
        }
        eprintln!("{}", row);
    }
    eprintln!();

    // === Soft assertions (warn, don't fail) ===
    // The expanded eval (297 queries) may have lower absolute scores than the
    // original 55-query hard eval. Report rather than assert.
    let config_a = &all_metrics[0];
    if config_a.recall_at_1 < 0.75 {
        eprintln!(
            "⚠ Config A (Cosine-only) Recall@1 below 75%: {:.1}% ({} queries)",
            config_a.recall_at_1 * 100.0,
            all_cases.len(),
        );
    }

    let baseline_mrr = config_a.mrr;
    for m in &all_metrics[1..] {
        if m.mrr < baseline_mrr * 0.85 {
            eprintln!(
                "⚠ Config '{}' MRR ({:.4}) is >15% worse than cosine baseline ({:.4})",
                m.name, m.mrr, baseline_mrr,
            );
        }
    }

    // Dump per-query diagnostics to JSON file if CQS_EVAL_OUTPUT is set
    if let Ok(path) = std::env::var("CQS_EVAL_OUTPUT") {
        // Classify difficulty per query: easy (rank 1, gap >0.05), medium (rank 1-5 close), hard (miss)
        let mut easy = 0u32;
        let mut medium = 0u32;
        let mut hard = 0u32;
        let mut weighted_hits = 0.0f64;
        let mut weighted_total = 0.0f64;
        for q in &query_diagnostics {
            let rank = q.get("rank").and_then(|r| r.as_u64());
            let top5 = q.get("top5").and_then(|t| t.as_array());
            let (difficulty, weight) = match rank {
                Some(1) => {
                    let gap = top5
                        .and_then(|t| {
                            if t.len() >= 2 {
                                let s1 = t[0].get("score")?.as_f64()?;
                                let s2 = t[1].get("score")?.as_f64()?;
                                Some(s1 - s2)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0.0);
                    if gap > 0.05 {
                        easy += 1;
                        ("easy", 1.0)
                    } else {
                        medium += 1;
                        ("medium", 2.0)
                    }
                }
                Some(r) if r <= 5 => {
                    medium += 1;
                    ("medium", 2.0)
                }
                _ => {
                    hard += 1;
                    ("hard", 3.0)
                }
            };
            weighted_total += weight;
            if rank == Some(1) {
                weighted_hits += weight;
            }
            // Tag the query with its difficulty (mutate in place via index)
            let _ = difficulty; // used below in summary
        }
        let weighted_r1 = if weighted_total > 0.0 {
            weighted_hits / weighted_total
        } else {
            0.0
        };

        let output = serde_json::json!({
            "config": "A: Cosine-only",
            "total_queries": query_diagnostics.len(),
            "metrics": {
                "recall_at_1": all_metrics[0].recall_at_1,
                "recall_at_5": all_metrics[0].recall_at_5,
                "mrr": all_metrics[0].mrr,
                "relaxed_recall_at_1": all_metrics[0].relaxed_recall_at_1,
                "weighted_r1": weighted_r1,
            },
            "difficulty": {
                "easy": easy,
                "medium": medium,
                "hard": hard,
            },
            "queries": query_diagnostics,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&output).unwrap())
            .unwrap_or_else(|e| eprintln!("Failed to write eval output to {}: {}", path, e));
        eprintln!("\nPer-query diagnostics written to {}", path);
    }
}

/// Holdout eval — runs held-out queries against the best config (HNSW + boost + demote).
///
/// Reports metrics only, no assertions. This set was never tuned against,
/// so it measures true generalization.
///
/// Run with: cargo test holdout_eval -- --ignored --nocapture
#[test]
#[ignore]
fn test_holdout_eval() {
    let embedder = Embedder::new(ModelConfig::resolve(None, None)).expect("Embedder init failed");
    let parser = Parser::new().expect("Parser init failed");

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("holdout_eval.db");
    let mut store = Store::open(&db_path).unwrap();
    let mc = ModelConfig::resolve(None, None);
    embedder.warm().expect("Failed to warm embedder");
    let actual_dim = embedder.embedding_dim();
    store.init(&ModelInfo::new(&mc.repo, actual_dim)).unwrap();
    store.set_dim(actual_dim);

    // Index both original AND hard fixtures (holdout queries target both)
    eprintln!("Parsing and indexing fixtures for holdout eval...");
    let mut chunk_count = 0;

    for lang in eval_languages() {
        for path in [fixture_path(lang), hard_fixture_path(lang)] {
            if !path.exists() {
                continue;
            }
            let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
            for chunk in &chunks {
                let text = generate_nl_description(chunk);
                let embeddings = embedder
                    .embed_documents(&[&text])
                    .expect("Failed to embed chunk");
                let embedding = embeddings.into_iter().next().unwrap();
                store
                    .upsert_chunk(chunk, &embedding, None)
                    .expect("Failed to store chunk");
                chunk_count += 1;
            }
        }
    }
    eprintln!("Indexed {} total chunks\n", chunk_count);

    // Build HNSW index
    let chunk_total = store.chunk_count().unwrap() as usize;
    let hnsw = HnswIndex::build_batched_with_dim(
        store.embedding_batches(10_000),
        chunk_total,
        store.dim(),
    )
    .expect("Failed to build HNSW index");

    // Embed all holdout queries
    let query_embeddings: Vec<_> = HOLDOUT_EVAL_CASES
        .iter()
        .map(|case| {
            embedder
                .embed_query(case.query)
                .expect("Failed to embed query")
        })
        .collect();

    // Run holdout eval with best config: HNSW + name_boost + demotion
    eprintln!(
        "--- Holdout Eval: HNSW + boost + demote ({} queries) ---",
        HOLDOUT_EVAL_CASES.len()
    );
    let mut results_per_case = Vec::new();

    for (i, case) in HOLDOUT_EVAL_CASES.iter().enumerate() {
        let filter = SearchFilter {
            languages: Some(vec![case.language]),
            include_types: Some(ChunkType::code_types()),
            name_boost: 0.2,
            query_text: case.query.to_string(),
            enable_demotion: true,
            ..Default::default()
        };
        let results = store
            .search_filtered_with_index(
                &query_embeddings[i],
                &filter,
                10,
                0.0,
                Some(&hnsw as &dyn VectorIndex),
            )
            .expect("Search failed");

        let (rank, relaxed_rank) = find_ranks(&results, case);

        let status = match rank {
            Some(1) => "+",
            Some(r) if r <= 5 => "~",
            _ => "-",
        };
        let top3: Vec<&str> = results
            .iter()
            .take(3)
            .map(|r| r.chunk.name.as_str())
            .collect();
        eprintln!(
            "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
            status,
            case.language,
            case.query,
            case.expected_name,
            rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
            top3
        );

        results_per_case.push((i, rank, relaxed_rank));
    }

    let (r1, r5, mrr, per_lang, relaxed_r1) =
        compute_metrics(&results_per_case, HOLDOUT_EVAL_CASES);

    eprintln!(
        "\n=== Holdout Eval Results ({} queries) ===\n",
        HOLDOUT_EVAL_CASES.len()
    );
    eprintln!("  Recall@1:         {:.1}%", r1 * 100.0);
    eprintln!("  Relaxed R@1:      {:.1}%", relaxed_r1 * 100.0);
    eprintln!("  Recall@5:         {:.1}%", r5 * 100.0);
    eprintln!("  MRR:              {:.4}", mrr);

    eprintln!("\n  Per-Language MRR:");
    for lang in &eval_languages() {
        let lang_mrr = per_lang.get(lang).copied().unwrap_or(0.0);
        eprintln!("    {:?}: {:.4}", lang, lang_mrr);
    }
    eprintln!();

    // No assertions — this is diagnostic only.
    // If holdout metrics diverge significantly from tuning set, we're overfitting.
}

/// Stress eval — holdout queries against fixtures + real open-source codebases as noise.
///
/// Indexes cqs source (Rust), Flask (Python), Zod (TypeScript), Express (JavaScript),
/// Chi (Go) alongside eval fixtures. Tests how ranking degrades with real distractors.
///
/// Run with: cargo test noise_eval_143q -- --ignored --nocapture
#[test]
#[ignore]
fn test_noise_eval_143q() {
    use std::path::Path;
    use walkdir::WalkDir;

    let embedder = Embedder::new(ModelConfig::resolve(None, None)).expect("Embedder init failed");
    let parser = Parser::new().expect("Parser init failed");

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("stress_eval.db");
    let mut store = Store::open(&db_path).unwrap();
    let mc = ModelConfig::resolve(None, None);
    embedder.warm().expect("Failed to warm embedder");
    let actual_dim = embedder.embedding_dim();
    store.init(&ModelInfo::new(&mc.repo, actual_dim)).unwrap();
    store.set_dim(actual_dim);

    // 1. Index eval fixtures (same as holdout) — with relationships for call graph enrichment
    eprintln!("=== Indexing eval fixtures ===");
    let mut fixture_chunks = 0;
    for lang in eval_languages() {
        for path in [fixture_path(lang), hard_fixture_path(lang)] {
            if !path.exists() {
                continue;
            }
            let (chunks, calls, _type_edges) = parser
                .parse_file_all(&path)
                .expect("Failed to parse fixture");
            for chunk in &chunks {
                let text = generate_nl_description(chunk);
                let embeddings = embedder
                    .embed_documents(&[&text])
                    .expect("Failed to embed chunk");
                let embedding = embeddings.into_iter().next().unwrap();
                store
                    .upsert_chunk(chunk, &embedding, None)
                    .expect("Failed to store chunk");
                fixture_chunks += 1;
            }
            if !calls.is_empty() {
                store
                    .upsert_function_calls(&path, &calls)
                    .expect("Failed to store calls");
            }
        }
    }
    eprintln!("  Fixture chunks: {}", fixture_chunks);

    // 2. Index real codebases as noise
    let noise_sources: Vec<(&str, &str, &[&str])> = vec![
        ("cqs (Rust)", "src/", &["rs"]),
        ("Flask (Python)", "/tmp/flask/src/flask/", &["py"]),
        ("Zod (TypeScript)", "/tmp/zod/src/", &["ts"]),
        ("Express (JS)", "/tmp/express/lib/", &["js"]),
        ("Chi (Go)", "/tmp/chi/", &["go"]),
    ];

    let mut noise_chunks = 0;
    let mut batch_texts: Vec<String> = Vec::new();
    let mut batch_chunks: Vec<cqs::parser::Chunk> = Vec::new();

    for (name, dir_path, exts) in &noise_sources {
        let base = Path::new(dir_path);
        if !base.exists() {
            eprintln!("  Skipping {} (not found: {})", name, dir_path);
            continue;
        }

        let mut repo_chunks = 0;
        for entry in WalkDir::new(base)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !exts.contains(&ext) {
                continue;
            }
            // Skip test files
            let path_str = path.to_string_lossy();
            if path_str.contains("test") || path_str.contains("spec") {
                continue;
            }

            if let Ok((chunks, calls, _type_edges)) = parser.parse_file_all(path) {
                if !calls.is_empty() {
                    store.upsert_function_calls(path, &calls).ok();
                }
                for chunk in &chunks {
                    batch_texts.push(generate_nl_description(chunk));
                    batch_chunks.push(chunk.clone());
                    repo_chunks += 1;

                    // Embed in batches of 64
                    if batch_texts.len() >= 64 {
                        let refs: Vec<&str> = batch_texts.iter().map(|s| s.as_str()).collect();
                        let embeddings = embedder
                            .embed_documents(&refs)
                            .expect("Failed to embed batch");
                        for (c, emb) in batch_chunks.drain(..).zip(embeddings) {
                            store.upsert_chunk(&c, &emb, None).ok();
                        }
                        batch_texts.clear();
                    }
                }
            }
        }
        noise_chunks += repo_chunks;
        eprintln!("  {}: {} chunks", name, repo_chunks);
    }

    // Flush remaining batch
    if !batch_texts.is_empty() {
        let refs: Vec<&str> = batch_texts.iter().map(|s| s.as_str()).collect();
        let embeddings = embedder
            .embed_documents(&refs)
            .expect("Failed to embed batch");
        for (c, emb) in batch_chunks.drain(..).zip(embeddings) {
            let emb = emb;
            store.upsert_chunk(&c, &emb, None).ok();
        }
    }

    let total = store.chunk_count().unwrap();
    eprintln!(
        "\n  Total: {} chunks ({} fixture + {} noise)\n",
        total, fixture_chunks, noise_chunks
    );

    // LLM summary pass (SQ-6) — if ANTHROPIC_API_KEY is set
    #[cfg(feature = "llm-summaries")]
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        eprintln!("Running LLM summary pass...");
        let config = cqs::config::Config::default();
        match cqs::llm::llm_summary_pass(&store, false, &config, None) {
            Ok(count) => eprintln!("  LLM summaries: {} new", count),
            Err(e) => eprintln!("  LLM summary pass failed (continuing without): {}", e),
        }

        // Re-embed chunks that have summaries with summary prepended to NL
        eprintln!("Re-embedding with summaries...");
        let mut re_embedded = 0usize;
        let mut cursor = 0i64;
        loop {
            let (chunks, next) = store.chunks_paged(cursor, 500).expect("paged");
            if chunks.is_empty() {
                break;
            }
            cursor = next;

            let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
            let summaries = store
                .get_summaries_by_hashes(&hashes, "summary")
                .unwrap_or_default();

            let mut batch_updates: Vec<(String, cqs::Embedding)> = Vec::new();
            for cs in &chunks {
                if let Some(summary) = summaries.get(&cs.content_hash) {
                    let chunk: cqs::Chunk = cs.into();
                    let base_nl = generate_nl_description(&chunk);
                    let nl_with_summary = format!("{} {}", summary, base_nl);
                    let embs = embedder
                        .embed_documents(&[&nl_with_summary])
                        .expect("embed");
                    let emb = embs.into_iter().next().unwrap();
                    batch_updates.push((cs.id.clone(), emb));
                }
            }
            if !batch_updates.is_empty() {
                re_embedded += batch_updates.len();
                store
                    .update_embeddings_batch(&batch_updates)
                    .expect("update");
            }
        }
        eprintln!("  Re-embedded {} chunks with summaries", re_embedded);
    }

    // Call graph enrichment (two-pass re-embedding)
    eprintln!("Running call graph enrichment...");
    let enriched = enrich_with_call_context(&store, &embedder);
    eprintln!("  Enriched {} chunks with call context\n", enriched);

    // Build HNSW index
    eprintln!("Building HNSW index...");
    let hnsw = HnswIndex::build_batched_with_dim(
        store.embedding_batches(10_000),
        total as usize,
        store.dim(),
    )
    .expect("Failed to build HNSW index");
    eprintln!("  HNSW: {} vectors\n", hnsw.len());

    // Embed queries
    let query_embeddings: Vec<_> = HOLDOUT_EVAL_CASES
        .iter()
        .map(|case| {
            embedder
                .embed_query(case.query)
                .expect("Failed to embed query")
        })
        .collect();

    // Sweep name_boost values
    let boost_values: &[f32] = &[0.0, 0.2, 0.4, 0.6, 0.8];
    let mut all_sweep: Vec<(f32, f64, f64, f64, f64, HashMap<Language, f64>)> = Vec::new();

    for &boost in boost_values {
        eprintln!(
            "\n--- name_boost={:.1} ({} queries, {} chunks) ---",
            boost,
            HOLDOUT_EVAL_CASES.len(),
            total
        );
        let mut results_per_case = Vec::new();

        for (i, case) in HOLDOUT_EVAL_CASES.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                include_types: Some(ChunkType::code_types()),
                name_boost: boost,
                query_text: case.query.to_string(),
                enable_demotion: true,
                enable_rrf: true,
                ..Default::default()
            };
            let results = store
                .search_filtered_with_index(
                    &query_embeddings[i],
                    &filter,
                    10,
                    0.0,
                    Some(&hnsw as &dyn VectorIndex),
                )
                .expect("Search failed");

            let (rank, relaxed_rank) = find_ranks(&results, case);

            let status = match rank {
                Some(1) => "+",
                Some(r) if r <= 5 => "~",
                _ => "-",
            };
            if boost == 0.0 || boost == 0.8 {
                // Only print details for endpoints to save output
                let top3: Vec<&str> = results
                    .iter()
                    .take(3)
                    .map(|r| r.chunk.name.as_str())
                    .collect();
                eprintln!(
                    "  {} [{:?}] \"{}\" -> exp: {} (rank: {}), top3: {:?}",
                    status,
                    case.language,
                    case.query,
                    case.expected_name,
                    rank.map(|r| r.to_string()).unwrap_or("miss".to_string()),
                    top3
                );
            }

            results_per_case.push((i, rank, relaxed_rank));
        }

        let (r1, r5, mrr, per_lang, relaxed_r1) =
            compute_metrics(&results_per_case, HOLDOUT_EVAL_CASES);
        all_sweep.push((boost, r1, relaxed_r1, r5, mrr, per_lang));
    }

    // Print sweep comparison table
    eprintln!(
        "\n=== Name Boost Sweep ({} queries, {} chunks) ===\n",
        HOLDOUT_EVAL_CASES.len(),
        total
    );
    eprintln!(
        "{:>10} {:>10} {:>12} {:>10} {:>10}",
        "boost", "R@1", "Relaxed R@1", "R@5", "MRR"
    );
    eprintln!("{}", "-".repeat(55));
    for &(boost, r1, relaxed_r1, r5, mrr, ref _per_lang) in &all_sweep {
        eprintln!(
            "{:>10.1} {:>9.1}% {:>11.1}% {:>9.1}% {:>10.4}",
            boost,
            r1 * 100.0,
            relaxed_r1 * 100.0,
            r5 * 100.0,
            mrr,
        );
    }

    let stress_langs = eval_languages();
    eprintln!("\n  Per-Language MRR:");
    {
        let mut header = format!("{:>10}", "boost");
        for lang in &stress_langs {
            header += &format!(" {:>8}", format!("{:?}", lang));
        }
        eprintln!("{}", header);
    }
    eprintln!("{}", "-".repeat(10 + 9 * stress_langs.len()));
    for &(boost, _, _, _, _, ref per_lang) in &all_sweep {
        let mut row = format!("{:>10.1}", boost);
        for lang in &stress_langs {
            let lang_mrr = per_lang.get(lang).copied().unwrap_or(0.0);
            row += &format!(" {:>8.4}", lang_mrr);
        }
        eprintln!("{}", row);
    }
    eprintln!();
}
