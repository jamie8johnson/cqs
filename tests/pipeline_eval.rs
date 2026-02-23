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

use cqs::embedder::Embedder;
use cqs::generate_nl_description;
use cqs::hnsw::HnswIndex;
use cqs::parser::{Language, Parser};
use cqs::store::{ModelInfo, SearchFilter, Store};
use cqs::VectorIndex;
use eval_common::{fixture_path, hard_fixture_path, EvalCase, HARD_EVAL_CASES};
use std::collections::HashMap;
use tempfile::TempDir;

/// Languages tested in the pipeline eval
const LANGUAGES: [Language; 5] = [
    Language::Rust,
    Language::Python,
    Language::TypeScript,
    Language::JavaScript,
    Language::Go,
];

/// Metrics for a single scoring configuration
struct ConfigMetrics {
    name: &'static str,
    recall_at_1: f64,
    recall_at_5: f64,
    mrr: f64,
    per_lang_mrr: HashMap<Language, f64>,
}

/// Compute metrics from search results for a set of eval cases.
///
/// For each case, finds the rank of `expected_name` in results (1-indexed).
/// Returns (recall@1, recall@5, MRR, per-language MRR).
fn compute_metrics(
    results_per_case: &[(usize, Option<usize>)], // (case_index, rank_of_expected)
    cases: &[EvalCase],
) -> (f64, f64, f64, HashMap<Language, f64>) {
    let total = results_per_case.len() as f64;
    if total == 0.0 {
        return (0.0, 0.0, 0.0, HashMap::new());
    }

    let mut hits_at_1 = 0usize;
    let mut hits_at_5 = 0usize;
    let mut total_rr = 0.0f64;
    let mut lang_rr: HashMap<Language, (f64, usize)> = HashMap::new();

    for &(case_idx, rank) in results_per_case {
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
    }

    let recall_1 = hits_at_1 as f64 / total;
    let recall_5 = hits_at_5 as f64 / total;
    let mrr = total_rr / total;

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

    (recall_1, recall_5, mrr, per_lang)
}

#[test]
#[ignore] // Slow - needs embedding. Run with: cargo test pipeline_eval -- --ignored --nocapture
fn test_pipeline_scoring() {
    // === Setup ===
    eprintln!("Initializing embedder...");
    let embedder = Embedder::new().expect("Failed to initialize embedder");
    let parser = Parser::new().expect("Failed to initialize parser");

    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("pipeline_eval.db");
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Parse and index both original AND hard fixtures for all 5 languages
    eprintln!("Parsing and indexing fixtures...");
    let mut chunk_count = 0;

    for lang in LANGUAGES {
        // Original fixtures
        let path = fixture_path(lang);
        eprintln!("  Parsing {:?}...", path);
        let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
        eprintln!("    Found {} chunks", chunks.len());

        for chunk in &chunks {
            let text = generate_nl_description(chunk);
            let embeddings = embedder
                .embed_documents(&[&text])
                .expect("Failed to embed chunk");
            let embedding = embeddings.into_iter().next().unwrap().with_sentiment(0.0);
            store
                .upsert_chunk(chunk, &embedding, None)
                .expect("Failed to store chunk");
            chunk_count += 1;
        }

        // Hard fixtures (confusable functions)
        let hard_path = hard_fixture_path(lang);
        if hard_path.exists() {
            eprintln!("  Parsing {:?}...", hard_path);
            let chunks = parser
                .parse_file(&hard_path)
                .expect("Failed to parse hard fixture");
            eprintln!("    Found {} chunks", chunks.len());

            for chunk in &chunks {
                let text = generate_nl_description(chunk);
                let embeddings = embedder
                    .embed_documents(&[&text])
                    .expect("Failed to embed chunk");
                let embedding = embeddings.into_iter().next().unwrap().with_sentiment(0.0);
                store
                    .upsert_chunk(chunk, &embedding, None)
                    .expect("Failed to store chunk");
                chunk_count += 1;
            }
        }
    }
    eprintln!("Indexed {} total chunks\n", chunk_count);

    // Build HNSW index from the store
    eprintln!("Building HNSW index...");
    let chunk_total = store.chunk_count().unwrap() as usize;
    let hnsw = HnswIndex::build_batched(store.embedding_batches(10_000), chunk_total)
        .expect("Failed to build HNSW index");
    eprintln!("  HNSW index: {} vectors\n", hnsw.len());

    // Pre-embed all queries
    eprintln!("Embedding {} queries...", HARD_EVAL_CASES.len());
    let query_embeddings: Vec<_> = HARD_EVAL_CASES
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

    // Config A: Cosine-only (brute-force, baseline)
    {
        eprintln!("--- Config A: Cosine-only ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                ..Default::default()
            };
            let results = store
                .search_filtered(&query_embeddings[i], &filter, 10, 0.0)
                .expect("Search failed");

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "A: Cosine-only",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // Config B: RRF (brute-force + keyword fusion)
    {
        eprintln!("\n--- Config B: RRF ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_rrf: true,
                query_text: case.query.to_string(),
                ..Default::default()
            };
            let results = store
                .search_filtered(&query_embeddings[i], &filter, 10, 0.0)
                .expect("Search failed");

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "B: RRF",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // Config C: RRF + name_boost (full brute-force pipeline)
    {
        eprintln!("\n--- Config C: RRF + name_boost ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_rrf: true,
                name_boost: 0.2,
                query_text: case.query.to_string(),
                ..Default::default()
            };
            let results = store
                .search_filtered(&query_embeddings[i], &filter, 10, 0.0)
                .expect("Search failed");

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "C: RRF + name_boost",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // Config D: HNSW-guided + name_boost (production path)
    {
        eprintln!("\n--- Config D: HNSW + name_boost ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
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

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "D: HNSW + name_boost",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // Config E: Cosine + demotion (measures demotion effect on cosine baseline)
    {
        eprintln!("\n--- Config E: Cosine + demotion ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
            let filter = SearchFilter {
                languages: Some(vec![case.language]),
                enable_demotion: true,
                ..Default::default()
            };
            let results = store
                .search_filtered(&query_embeddings[i], &filter, 10, 0.0)
                .expect("Search failed");

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "E: Cosine + demotion",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // Config F: HNSW + name_boost + demotion (production path with demotion)
    {
        eprintln!("\n--- Config F: HNSW + name_boost + demote ---");
        let mut results_per_case = Vec::new();

        for (i, case) in HARD_EVAL_CASES.iter().enumerate() {
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

            let rank = results
                .iter()
                .position(|r| r.chunk.name == case.expected_name)
                .map(|pos| pos + 1);

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

            results_per_case.push((i, rank));
        }

        let (r1, r5, mrr, per_lang) = compute_metrics(&results_per_case, HARD_EVAL_CASES);
        all_metrics.push(ConfigMetrics {
            name: "F: HNSW + boost + demote",
            recall_at_1: r1,
            recall_at_5: r5,
            mrr,
            per_lang_mrr: per_lang,
        });
    }

    // === Print comparison table ===
    eprintln!(
        "\n=== Pipeline Scoring Comparison ({} hard eval queries) ===\n",
        HARD_EVAL_CASES.len()
    );
    eprintln!(
        "{:<25} {:>10} {:>10} {:>10}",
        "Config", "Recall@1", "Recall@5", "MRR"
    );
    eprintln!("{}", "-".repeat(55));
    for m in &all_metrics {
        eprintln!(
            "{:<25} {:>9.1}% {:>9.1}% {:>10.4}",
            m.name,
            m.recall_at_1 * 100.0,
            m.recall_at_5 * 100.0,
            m.mrr,
        );
    }

    // Per-language MRR table
    eprintln!("\n=== Per-Language MRR ===\n");
    eprintln!(
        "{:<25} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "Config", "Rust", "Py", "TS", "JS", "Go"
    );
    eprintln!("{}", "-".repeat(70));
    for m in &all_metrics {
        let mut row = format!("{:<25}", m.name);
        for lang in &LANGUAGES {
            let lang_mrr = m.per_lang_mrr.get(lang).copied().unwrap_or(0.0);
            row += &format!(" {:>7.4}", lang_mrr);
        }
        eprintln!("{}", row);
    }
    eprintln!();

    // === Assertions ===
    let config_a = &all_metrics[0];
    assert!(
        config_a.recall_at_1 >= 0.85,
        "Config A (Cosine-only) Recall@1 below 85% threshold: {:.1}%",
        config_a.recall_at_1 * 100.0,
    );

    // No config should be dramatically worse than cosine baseline
    let baseline_mrr = config_a.mrr;
    for m in &all_metrics[1..] {
        assert!(
            m.mrr >= baseline_mrr * 0.90,
            "Config '{}' MRR ({:.4}) is >10% worse than cosine baseline ({:.4})",
            m.name,
            m.mrr,
            baseline_mrr,
        );
    }
}
