//! V2 eval harness — ablation matrix with bootstrap CIs and per-query result storage.
//!
//! Runs against the live cqs index (the indexed cqs codebase itself).
//! Loads queries from evals/queries/v2_300q.json.
//!
//! Run with: cargo test --features gpu-index --test eval_harness -- --ignored --nocapture

mod eval_common;

use eval_common::{
    bootstrap_ci, paired_bootstrap, EvalQuery, EvalQueryResult, EvalQuerySet, EvalSplit,
    MetricWithCI, QueryCategory,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ===== Configuration matrix =====

#[derive(Debug, Clone)]
struct EvalConfig {
    id: String,
    dense_model: DenseModel,
    sparse_mode: SparseMode,
    reranker: RerankerMode,
}

#[derive(Debug, Clone, Copy)]
enum DenseModel {
    BgeLarge,
    // Future: BgeLargeFt, E5Base, V9_200k
}

#[derive(Debug, Clone, Copy)]
enum SparseMode {
    None,
    // Future: Splade with alpha values
}

#[derive(Debug, Clone, Copy)]
enum RerankerMode {
    None,
    MiniLmV1,
    // Future: code-trained cross-encoders
}

impl DenseModel {
    fn label(&self) -> &str {
        match self {
            Self::BgeLarge => "bge-large",
        }
    }
}

impl SparseMode {
    fn label(&self) -> &str {
        match self {
            Self::None => "no-sparse",
        }
    }
}

impl RerankerMode {
    fn label(&self) -> &str {
        match self {
            Self::None => "no-rerank",
            Self::MiniLmV1 => "minilm-v1",
        }
    }
}

impl EvalConfig {
    fn new(dense: DenseModel, sparse: SparseMode, reranker: RerankerMode) -> Self {
        Self {
            id: format!("{}_{}_{}", dense.label(), sparse.label(), reranker.label()),
            dense_model: dense,
            sparse_mode: sparse,
            reranker: reranker,
        }
    }
}

// ===== Core eval logic =====

/// Find rank of ground truth chunk in search results (by name match)
fn find_rank(results: &[cqs::store::SearchResult], query: &EvalQuery) -> (Option<u32>, bool, bool) {
    let primary = &query.primary_answer.name;
    let acceptable: Vec<&str> = query
        .acceptable_answers
        .iter()
        .map(|a| a.name.as_str())
        .collect();

    let mut rank_of_correct: Option<u32> = None;
    let mut top_5_acceptable = false;

    for (i, r) in results.iter().enumerate() {
        let rank = (i + 1) as u32;

        if r.chunk.name == *primary {
            if rank_of_correct.is_none() {
                rank_of_correct = Some(rank);
            }
        }

        if rank <= 5 && (r.chunk.name == *primary || acceptable.contains(&r.chunk.name.as_str())) {
            top_5_acceptable = true;
        }
    }

    let top_1 = rank_of_correct == Some(1);
    let top_5 = rank_of_correct.map(|r| r <= 5).unwrap_or(false);

    (rank_of_correct, top_1, top_5 || top_5_acceptable)
}

/// Run a single query against a configuration, return per-query result
fn eval_single_query<Mode>(
    query: &EvalQuery,
    store: &cqs::Store<Mode>,
    embedder: &cqs::Embedder,
    reranker: Option<&cqs::Reranker>,
    config: &EvalConfig,
    run_id: &str,
) -> EvalQueryResult {
    let retrieval_start = Instant::now();

    // Embed query
    let query_emb = match embedder.embed_query(&query.query) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("  Embed failed for {}: {}", query.id, e);
            return empty_result(run_id, &query.id, &config.id);
        }
    };

    // Build search filter
    let filter = cqs::SearchFilter {
        query_text: query.query.clone(),
        ..Default::default()
    };

    // Search with limit=100 for deep rank tracking
    let mut results = match store.search_filtered(&query_emb, &filter, 100, 0.0) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  Search failed for {}: {}", query.id, e);
            return empty_result(run_id, &query.id, &config.id);
        }
    };

    let retrieval_ms = retrieval_start.elapsed().as_secs_f64() * 1000.0;

    // Optional reranking
    let rerank_start = Instant::now();
    if let Some(rr) = reranker {
        let rerank_limit = results.len().min(20);
        if let Err(e) = rr.rerank(&query.query, &mut results, rerank_limit) {
            eprintln!("  Rerank failed for {}: {}", query.id, e);
        }
    }
    let rerank_ms = if reranker.is_some() {
        rerank_start.elapsed().as_secs_f64() * 1000.0
    } else {
        0.0
    };

    // Score
    let (rank, top_1, top_5_acc) = find_rank(&results, query);
    let top_5 = rank.map(|r| r <= 5).unwrap_or(false);
    let rr = rank.map(|r| 1.0 / r as f64).unwrap_or(0.0);

    let top_1_score = results.first().map(|r| r.score as f64).unwrap_or(0.0);
    let top_2_score = results.get(1).map(|r| r.score as f64).unwrap_or(0.0);

    EvalQueryResult {
        run_id: run_id.to_string(),
        query_id: query.id.clone(),
        config_id: config.id.clone(),
        rank_of_correct: rank,
        reciprocal_rank: rr,
        top_1_correct: top_1,
        top_5_correct: top_5,
        top_5_acceptable: top_5_acc,
        top_1_score,
        top_2_score,
        retrieval_ms,
        rerank_ms,
    }
}

fn empty_result(run_id: &str, query_id: &str, config_id: &str) -> EvalQueryResult {
    EvalQueryResult {
        run_id: run_id.to_string(),
        query_id: query_id.to_string(),
        config_id: config_id.to_string(),
        rank_of_correct: None,
        reciprocal_rank: 0.0,
        top_1_correct: false,
        top_5_correct: false,
        top_5_acceptable: false,
        top_1_score: 0.0,
        top_2_score: 0.0,
        retrieval_ms: 0.0,
        rerank_ms: 0.0,
    }
}

// ===== Aggregation =====

struct AggregateRow {
    config_id: String,
    n: usize,
    r_at_1: MetricWithCI,
    r_at_5: MetricWithCI,
    mrr: MetricWithCI,
    r_at_5_acceptable: MetricWithCI,
}

fn aggregate(results: &[EvalQueryResult]) -> AggregateRow {
    let n = results.len();
    let r1_vals: Vec<f64> = results
        .iter()
        .map(|r| if r.top_1_correct { 1.0 } else { 0.0 })
        .collect();
    let r5_vals: Vec<f64> = results
        .iter()
        .map(|r| if r.top_5_correct { 1.0 } else { 0.0 })
        .collect();
    let mrr_vals: Vec<f64> = results.iter().map(|r| r.reciprocal_rank).collect();
    let r5a_vals: Vec<f64> = results
        .iter()
        .map(|r| if r.top_5_acceptable { 1.0 } else { 0.0 })
        .collect();

    let config_id = results
        .first()
        .map(|r| r.config_id.clone())
        .unwrap_or_default();

    AggregateRow {
        config_id,
        n,
        r_at_1: bootstrap_ci(&r1_vals, 10_000),
        r_at_5: bootstrap_ci(&r5_vals, 10_000),
        mrr: bootstrap_ci(&mrr_vals, 10_000),
        r_at_5_acceptable: bootstrap_ci(&r5a_vals, 10_000),
    }
}

fn aggregate_by_category(
    results: &[EvalQueryResult],
    queries: &[EvalQuery],
) -> HashMap<QueryCategory, AggregateRow> {
    let query_map: HashMap<&str, &EvalQuery> = queries.iter().map(|q| (q.id.as_str(), q)).collect();

    let mut by_cat: HashMap<QueryCategory, Vec<&EvalQueryResult>> = HashMap::new();
    for r in results {
        if let Some(q) = query_map.get(r.query_id.as_str()) {
            by_cat.entry(q.category).or_default().push(r);
        }
    }

    by_cat
        .into_iter()
        .map(|(cat, refs)| {
            let owned: Vec<EvalQueryResult> = refs.into_iter().cloned().collect();
            (cat, aggregate(&owned))
        })
        .collect()
}

// ===== Report generation =====

fn generate_report(
    all_results: &HashMap<String, Vec<EvalQueryResult>>,
    queries: &[EvalQuery],
    run_id: &str,
) -> String {
    let mut report = String::new();

    report.push_str(&format!("# Eval Run: {}\n\n", run_id));
    report.push_str(&format!(
        "Date: {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    ));
    report.push_str(&format!("Queries: {}\n\n", queries.len()));

    // Summary table
    report.push_str("## Summary\n\n");
    report.push_str("| Config | N | R@1 | R@5 | MRR | R@5(acc) |\n");
    report.push_str("|--------|---|-----|-----|-----|----------|\n");

    let mut configs: Vec<&String> = all_results.keys().collect();
    configs.sort();

    for config_id in &configs {
        let results = &all_results[*config_id];
        let agg = aggregate(results);
        report.push_str(&format!(
            "| {} | {} | {} | {} | {:.4} [{:.4}, {:.4}] | {} |\n",
            agg.config_id,
            agg.n,
            agg.r_at_1,
            agg.r_at_5,
            agg.mrr.value,
            agg.mrr.ci_lower,
            agg.mrr.ci_upper,
            agg.r_at_5_acceptable,
        ));
    }

    // Per-category breakdown
    report.push_str("\n## Per-Category R@1\n\n");
    report.push_str("| Config | Category | N | R@1 | MRR |\n");
    report.push_str("|--------|----------|---|-----|-----|\n");

    for config_id in &configs {
        let results = &all_results[*config_id];
        let by_cat = aggregate_by_category(results, queries);
        let mut cats: Vec<_> = by_cat.into_iter().collect();
        cats.sort_by_key(|(cat, _)| format!("{}", cat));
        for (cat, agg) in &cats {
            report.push_str(&format!(
                "| {} | {} | {} | {} | {:.4} [{:.4}, {:.4}] |\n",
                config_id,
                cat,
                agg.n,
                agg.r_at_1,
                agg.mrr.value,
                agg.mrr.ci_lower,
                agg.mrr.ci_upper,
            ));
        }
    }

    // Pairwise comparison (if multiple configs)
    if configs.len() >= 2 {
        report.push_str("\n## Pairwise Comparisons\n\n");
        let baseline_id = configs[0];
        let baseline_results = &all_results[baseline_id];
        let baseline_r1: Vec<f64> = baseline_results
            .iter()
            .map(|r| if r.top_1_correct { 1.0 } else { 0.0 })
            .collect();

        for config_id in configs.iter().skip(1) {
            let results = &all_results[*config_id];
            let r1: Vec<f64> = results
                .iter()
                .map(|r| if r.top_1_correct { 1.0 } else { 0.0 })
                .collect();

            let (delta, ci_lo, ci_hi, p) = paired_bootstrap(&baseline_r1, &r1, 10_000);
            report.push_str(&format!(
                "**{} vs {}**: delta R@1 = {:.1}pp [{:.1}, {:.1}], p = {:.3}\n\n",
                config_id,
                baseline_id,
                delta * 100.0,
                ci_lo * 100.0,
                ci_hi * 100.0,
                p,
            ));
        }
    }

    // Failure inventory (queries where best config fails)
    report.push_str("\n## Failure Inventory (best config, top-1 misses)\n\n");
    if let Some(best_config) = configs.first() {
        let results = &all_results[best_config.as_str()];
        let query_map: HashMap<&str, &EvalQuery> =
            queries.iter().map(|q| (q.id.as_str(), q)).collect();

        let mut failures: Vec<_> = results.iter().filter(|r| !r.top_1_correct).collect();
        failures.sort_by(|a, b| {
            b.rank_of_correct
                .unwrap_or(999)
                .cmp(&a.rank_of_correct.unwrap_or(999))
        });

        for f in failures.iter().take(20) {
            if let Some(q) = query_map.get(f.query_id.as_str()) {
                let rank_str = f
                    .rank_of_correct
                    .map(|r| format!("rank {}", r))
                    .unwrap_or_else(|| "miss".to_string());
                report.push_str(&format!(
                    "- **{}** [{}] \"{}\": expected `{}`, got {} (score gap: {:.4})\n",
                    f.query_id,
                    q.category,
                    q.query,
                    q.primary_answer.name,
                    rank_str,
                    f.top_1_score - f.top_2_score,
                ));
            }
        }
    }

    report
}

// ===== Per-query result persistence =====

fn save_results_jsonl(results: &[EvalQueryResult], path: &Path) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).expect("Failed to create results file");
    for r in results {
        let line = serde_json::to_string(r).expect("Failed to serialize result");
        writeln!(f, "{}", line).expect("Failed to write result");
    }
}

// ===== Load query set =====

fn load_query_set() -> EvalQuerySet {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = PathBuf::from(&manifest_dir).join("evals/queries/v2_300q.json");

    let data = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read query set at {}: {}", path.display(), e));
    serde_json::from_str(&data).unwrap_or_else(|e| panic!("Invalid query set JSON: {}", e))
}

// ===== Tests =====

#[test]
fn test_bootstrap_ci_basic() {
    // All 1s → CI should be tight around 1.0
    let vals = vec![1.0; 100];
    let ci = bootstrap_ci(&vals, 1000);
    assert!((ci.value - 1.0).abs() < 1e-6);
    assert!(ci.ci_lower > 0.99);
    assert!(ci.ci_upper <= 1.0 + 1e-6);
}

#[test]
fn test_bootstrap_ci_mixed() {
    // 50/50 → CI should be around 0.5 ± ~0.1
    let mut vals = vec![1.0; 50];
    vals.extend(vec![0.0; 50]);
    let ci = bootstrap_ci(&vals, 10_000);
    assert!((ci.value - 0.5).abs() < 0.01);
    assert!(ci.ci_lower > 0.35);
    assert!(ci.ci_upper < 0.65);
}

#[test]
fn test_bootstrap_ci_empty() {
    let ci = bootstrap_ci(&[], 1000);
    assert_eq!(ci.value, 0.0);
}

#[test]
fn test_paired_bootstrap_identical() {
    let a = vec![1.0, 0.0, 1.0, 0.0, 1.0];
    let (delta, _, _, p) = paired_bootstrap(&a, &a, 1000);
    assert!((delta).abs() < 1e-6);
    assert!(p > 0.5); // not significant
}

#[test]
fn test_paired_bootstrap_clear_winner() {
    let a = vec![0.0; 100];
    let b = vec![1.0; 100];
    let (delta, ci_lo, _, p) = paired_bootstrap(&a, &b, 10_000);
    assert!((delta - 1.0).abs() < 1e-6);
    assert!(ci_lo > 0.9);
    assert!(p < 0.01);
}

/// Full matrix eval — run against live cqs index
#[test]
#[ignore] // Slow - requires indexed cqs codebase. Run with: cargo test eval_harness -- --ignored --nocapture
fn test_eval_matrix() {
    let query_set = load_query_set();

    // Filter to train split for tuning runs
    let train_queries: Vec<&EvalQuery> = query_set
        .queries
        .iter()
        .filter(|q| q.split == EvalSplit::Train)
        .collect();

    eprintln!(
        "Loaded {} queries ({} train, {} held-out)",
        query_set.queries.len(),
        train_queries.len(),
        query_set.queries.len() - train_queries.len()
    );

    // Open cqs index (uses CARGO_MANIFEST_DIR as project root)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let root = PathBuf::from(&manifest_dir);
    let cqs_dir = cqs::resolve_index_dir(&root);
    let db_path = cqs_dir.join("index.db");
    let store = cqs::Store::open_readonly_pooled(&db_path).expect("Failed to open store");
    let model_config = cqs::embedder::ModelConfig::resolve(None, None);
    let embedder = cqs::Embedder::new_cpu(model_config).expect("Failed to init embedder");

    // Configuration matrix (start small, expand as models are available)
    let configs = vec![
        EvalConfig::new(DenseModel::BgeLarge, SparseMode::None, RerankerMode::None),
        EvalConfig::new(
            DenseModel::BgeLarge,
            SparseMode::None,
            RerankerMode::MiniLmV1,
        ),
    ];

    let run_id = format!("run_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S"));

    eprintln!(
        "\n=== Eval Run: {} ({} configs × {} queries) ===\n",
        run_id,
        configs.len(),
        train_queries.len()
    );

    let mut all_results: HashMap<String, Vec<EvalQueryResult>> = HashMap::new();

    for config in &configs {
        eprintln!("--- Config: {} ---", config.id);

        // Init reranker if needed
        let reranker = match config.reranker {
            RerankerMode::None => None,
            RerankerMode::MiniLmV1 => Some(cqs::Reranker::new().expect("Failed to init reranker")),
        };

        let mut config_results: Vec<EvalQueryResult> = Vec::new();

        for query in &train_queries {
            let result =
                eval_single_query(query, &store, &embedder, reranker.as_ref(), config, &run_id);

            let rank_str = result
                .rank_of_correct
                .map(|r| format!("{}", r))
                .unwrap_or_else(|| "miss".to_string());

            if !result.top_1_correct {
                eprintln!(
                    "  [{}] \"{}\" → exp: {}, got: {} ({:.1}ms)",
                    query.id,
                    &query.query[..query.query.len().min(50)],
                    query.primary_answer.name,
                    rank_str,
                    result.retrieval_ms,
                );
            }

            config_results.push(result);
        }

        let agg = aggregate(&config_results);
        eprintln!(
            "  R@1: {}  MRR: {:.4} [{:.4}, {:.4}]  ({} queries)\n",
            agg.r_at_1, agg.mrr.value, agg.mrr.ci_lower, agg.mrr.ci_upper, agg.n
        );

        all_results.insert(config.id.clone(), config_results);
    }

    // Generate report
    let report = generate_report(&all_results, &query_set.queries, &run_id);

    // Save artifacts
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let run_dir = PathBuf::from(&manifest_dir).join(format!("evals/runs/{}", run_id));
    std::fs::create_dir_all(&run_dir).expect("Failed to create run directory");

    // Save report
    let report_path = run_dir.join("report.md");
    std::fs::write(&report_path, &report).expect("Failed to write report");
    eprintln!("Report: {}", report_path.display());

    // Save per-query results as JSONL
    let all_flat: Vec<EvalQueryResult> = all_results.values().flatten().cloned().collect();
    let results_path = run_dir.join("results.jsonl");
    save_results_jsonl(&all_flat, &results_path);
    eprintln!("Results: {}", results_path.display());

    // Print summary to console
    eprintln!("\n{}", report);
}
