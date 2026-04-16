//! Eval suite for measuring search quality
//!
//! Run with: cargo test eval -- --ignored --nocapture
//! (Ignored by default because embedding generation is slow)

mod eval_common;

use cqs::embedder::{Embedder, Embedding, ModelConfig};
use cqs::generate_nl_description;
use cqs::parser::{Chunk, ChunkType, Language};
use cqs::store::{ModelInfo, SearchFilter, Store};
use eval_common::{fixture_path, EVAL_CASES};
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
#[ignore] // Slow test - run with: cargo test eval -- --ignored --nocapture
fn test_recall_at_5() {
    // Initialize embedder
    eprintln!("Initializing embedder...");
    let embedder =
        Embedder::new(ModelConfig::resolve(None, None)).expect("Failed to initialize embedder");

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
            let embedding = embeddings.into_iter().next().unwrap();

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

// ============ Always-on recall test (issue #975) ============
//
// Pins the CI recall ceiling without requiring a downloaded model.
// Unlike `test_recall_at_5` (which is `#[ignore]` and needs BGE-large),
// this test runs on every build. It seeds a fresh store with chunks
// whose embeddings are crafted so one chunk is strictly closest to a
// known query vector, then exercises the full `search_filtered` path
// (cosine scoring, threshold, top-K truncation, RRF-off ordering).
//
// Failure modes this guards against:
//   - RRF ordering breaks
//   - Top-K truncation bug
//   - `SearchFilter::default()` regression (e.g., accidental demotion
//     of non-test chunks, incorrect `enable_rrf` default)
//   - Embedding storage/retrieval corruption (dim mismatch, byte
//     conversion round-trip error)

/// Build a deterministic 1024-dim unit vector from an integer seed.
///
/// Uses `sin((seed * 0.1) + (i * 0.001))` per position — each seed
/// produces a distinct direction, unlike the repeat-scalar
/// `mock_embedding` in `tests/common/mod.rs` which collapses any
/// positive scalar to the same unit vector after L2 normalization.
fn seeded_embedding(seed: u32) -> Embedding {
    let mut v = vec![0.0f32; cqs::EMBEDDING_DIM];
    for (i, val) in v.iter_mut().enumerate() {
        *val = ((seed as f32 * 0.1) + (i as f32 * 0.001)).sin();
    }
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut v {
            *val /= norm;
        }
    }
    Embedding::new(v)
}

/// Build a minimal function chunk with the given name, file, and hash suffix.
/// Uses `ChunkType::Function` so the default `enable_demotion` does not apply
/// (demotion targets `ChunkType::Test`).
fn seed_chunk(name: &str, file: &str, hash: &str) -> Chunk {
    Chunk {
        id: format!("{}:1:{}", file, hash),
        file: PathBuf::from(file),
        language: Language::Rust,
        chunk_type: ChunkType::Function,
        name: name.to_string(),
        signature: format!("fn {}()", name),
        content: format!("fn {}() {{ /* body */ }}", name),
        doc: None,
        line_start: 1,
        line_end: 5,
        content_hash: hash.to_string(),
        parent_id: None,
        window_idx: None,
        parent_type_name: None,
    }
}

#[test]
fn test_search_pipeline_mock_embedder() {
    // Five chunks covering different concepts. Each seed produces a
    // unique direction via `seeded_embedding`; the query embedding
    // below matches seed 1 (error handling) exactly, so that chunk
    // must appear in the top-3 under any sane scoring regime.
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join(cqs::INDEX_DB_FILENAME);
    let store = Store::open(&db_path).unwrap();
    store.init(&ModelInfo::default()).unwrap();

    let seeds: [(&str, &str, u32); 5] = [
        ("handle_error", "src/errors.rs", 1),
        ("spawn_task", "src/runtime.rs", 2),
        ("score_splade", "src/splade.rs", 3),
        ("hnsw_search", "src/hnsw.rs", 4),
        ("parse_token", "src/parser.rs", 5),
    ];

    let pairs: Vec<_> = seeds
        .iter()
        .map(|(name, file, seed)| {
            let hash = format!("{:08x}", seed);
            (seed_chunk(name, file, &hash), seeded_embedding(*seed))
        })
        .collect();

    store
        .upsert_chunks_batch(&pairs, Some(1_700_000_000_000))
        .expect("Failed to upsert seeded chunks");

    // Query vector == error-handling chunk's embedding → cosine 1.0
    // for that chunk, strictly less than 1.0 for the other four.
    let query = seeded_embedding(1);
    let filter = SearchFilter::default();
    let results = store
        .search_filtered(&query, &filter, 3, 0.0)
        .expect("Failed to search");

    assert!(
        !results.is_empty(),
        "search_filtered returned no results for a populated store"
    );
    assert!(
        results.len() <= 3,
        "Top-K truncation broken: requested 3, got {}",
        results.len()
    );

    let top_names: Vec<&str> = results.iter().map(|r| r.chunk.name.as_str()).collect();
    assert!(
        top_names.contains(&"handle_error"),
        "Expected 'handle_error' in top-3, got {:?}",
        top_names
    );

    // The exact match (cosine ~1.0) should outrank any other chunk.
    assert_eq!(
        results[0].chunk.name, "handle_error",
        "Exact embedding match should rank #1, got top-3 {:?}",
        top_names
    );
}
