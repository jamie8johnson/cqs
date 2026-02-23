//! Eval suite for measuring search quality
//!
//! Run with: cargo test eval -- --ignored --nocapture
//! (Ignored by default because embedding generation is slow)

mod eval_common;

use cqs::embedder::Embedder;
use cqs::generate_nl_description;
use cqs::parser::Language;
use cqs::store::{ModelInfo, SearchFilter, Store};
use eval_common::{fixture_path, EVAL_CASES};
use std::collections::HashMap;
use tempfile::TempDir;

#[test]
#[ignore] // Slow test - run with: cargo test eval -- --ignored --nocapture
fn test_recall_at_5() {
    // Initialize embedder
    eprintln!("Initializing embedder...");
    let embedder = Embedder::new().expect("Failed to initialize embedder");

    // Initialize parser
    let parser = cqs::parser::Parser::new().expect("Failed to initialize parser");

    // Create temporary store
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("eval.db");
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    // Parse and index all fixtures
    eprintln!("Parsing and indexing fixtures...");
    let mut chunk_count = 0;
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        let path = fixture_path(lang);
        eprintln!("  Parsing {:?}...", path);

        let chunks = parser.parse_file(&path).expect("Failed to parse fixture");
        eprintln!("    Found {} chunks", chunks.len());

        for chunk in &chunks {
            // Generate embedding using NL pipeline (same as production)
            let text = generate_nl_description(chunk);
            let embeddings = embedder
                .embed_documents(&[&text])
                .expect("Failed to embed chunk");
            let embedding = embeddings.into_iter().next().unwrap().with_sentiment(0.0);

            // Store chunk (no mtime since these are test fixtures)
            store
                .upsert_chunk(chunk, &embedding, None)
                .expect("Failed to store chunk");
            chunk_count += 1;
        }
    }
    eprintln!("Indexed {} total chunks", chunk_count);

    // Run eval cases
    eprintln!("\nRunning {} eval cases...\n", EVAL_CASES.len());

    let mut results_by_lang: HashMap<Language, (usize, usize)> = HashMap::new();
    let mut total_hits = 0;
    let mut total_cases = 0;

    for case in EVAL_CASES {
        // Generate query embedding
        let query_embedding = embedder
            .embed_query(case.query)
            .expect("Failed to embed query");

        // Search with language filter
        let filter = SearchFilter {
            languages: Some(vec![case.language]),
            ..Default::default()
        };
        let results = store
            .search_filtered(&query_embedding, &filter, 5, 0.0)
            .expect("Failed to search");

        // Check if expected name is in top-5
        let found = results.iter().any(|r| r.chunk.name == case.expected_name);

        // Track results
        let (hits, total) = results_by_lang.entry(case.language).or_insert((0, 0));
        *total += 1;
        if found {
            *hits += 1;
            total_hits += 1;
        }
        total_cases += 1;

        // Print result
        let status = if found { "+" } else { "-" };
        let top_names: Vec<_> = results
            .iter()
            .take(3)
            .map(|r| r.chunk.name.as_str())
            .collect();
        eprintln!(
            "{} [{:?}] \"{}\" -> expected: {}, got: {:?}",
            status, case.language, case.query, case.expected_name, top_names
        );
    }

    // Print summary
    eprintln!("\n=== Results ===");
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        if let Some((hits, total)) = results_by_lang.get(&lang) {
            let pct = (*hits as f64 / *total as f64) * 100.0;
            eprintln!("{:?}: {}/{} ({:.0}%)", lang, hits, total, pct);
        }
    }
    let overall_pct = (total_hits as f64 / total_cases as f64) * 100.0;
    eprintln!(
        "\nOverall Recall@5: {}/{} ({:.0}%)",
        total_hits, total_cases, overall_pct
    );

    // Assert minimum quality threshold (8/10 = 80% per language is the goal)
    assert!(
        overall_pct >= 60.0,
        "Recall@5 below 60% threshold: {:.0}%",
        overall_pct
    );
}

#[test]
fn test_fixtures_exist() {
    // Quick sanity check that all fixtures exist
    for lang in [
        Language::Rust,
        Language::Python,
        Language::TypeScript,
        Language::JavaScript,
        Language::Go,
    ] {
        let path = fixture_path(lang);
        assert!(path.exists(), "Fixture missing: {:?}", path);
    }
}
