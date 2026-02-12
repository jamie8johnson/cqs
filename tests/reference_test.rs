//! Reference index tests (TC9)
//!
//! Tests for merge_results, search_reference, weight application,
//! and reference name validation.

mod common;

use common::{mock_embedding, test_chunk, TestStore};
use cqs::parser::{ChunkType, Language};
use cqs::reference::{self, merge_results, ReferenceIndex};
use cqs::store::{SearchFilter, SearchResult, UnifiedResult};
use std::path::PathBuf;

// ============ Helpers ============

fn make_code_result(name: &str, score: f32) -> SearchResult {
    SearchResult {
        chunk: cqs::store::ChunkSummary {
            id: format!("id-{}", name),
            file: PathBuf::from(format!("src/{}.rs", name)),
            language: Language::Rust,
            chunk_type: ChunkType::Function,
            name: name.to_string(),
            signature: String::new(),
            content: format!("fn {}() {{}}", name),
            doc: None,
            line_start: 1,
            line_end: 1,
            parent_id: None,
        },
        score,
    }
}

/// Insert chunks with identical embeddings
fn insert_chunks(store: &TestStore, chunks: &[cqs::Chunk], seed: f32) {
    let emb = mock_embedding(seed);
    let pairs: Vec<_> = chunks.iter().map(|c| (c.clone(), emb.clone())).collect();
    store.upsert_chunks_batch(&pairs, Some(12345)).unwrap();
}

// ===== merge_results tests =====

#[test]
fn test_merge_results_interleaves_by_score() {
    let primary = vec![
        UnifiedResult::Code(make_code_result("p1", 0.95)),
        UnifiedResult::Code(make_code_result("p2", 0.5)),
    ];
    let refs = vec![
        (
            "ref_a".to_string(),
            vec![make_code_result("r1", 0.8), make_code_result("r2", 0.6)],
        ),
        ("ref_b".to_string(), vec![make_code_result("r3", 0.7)]),
    ];

    let merged = merge_results(primary, refs, 10);
    assert_eq!(merged.len(), 5);

    // Verify descending score order
    for w in merged.windows(2) {
        assert!(
            w[0].result.score() >= w[1].result.score(),
            "Scores should be descending: {} >= {}",
            w[0].result.score(),
            w[1].result.score()
        );
    }

    // Verify source tags
    assert!(merged[0].source.is_none()); // p1 at 0.95
}

#[test]
fn test_merge_results_multiple_refs_tagged_correctly() {
    let primary = vec![];
    let refs = vec![
        ("alpha".to_string(), vec![make_code_result("a1", 0.9)]),
        ("beta".to_string(), vec![make_code_result("b1", 0.8)]),
        ("gamma".to_string(), vec![make_code_result("g1", 0.7)]),
    ];

    let merged = merge_results(primary, refs, 10);
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0].source.as_deref(), Some("alpha"));
    assert_eq!(merged[1].source.as_deref(), Some("beta"));
    assert_eq!(merged[2].source.as_deref(), Some("gamma"));
}

#[test]
fn test_merge_results_truncates_strictly() {
    let primary: Vec<UnifiedResult> = (0..5)
        .map(|i| UnifiedResult::Code(make_code_result(&format!("p{}", i), 0.9 - i as f32 * 0.1)))
        .collect();
    let refs = vec![(
        "ref".to_string(),
        (0..5)
            .map(|i| make_code_result(&format!("r{}", i), 0.85 - i as f32 * 0.1))
            .collect(),
    )];

    let merged = merge_results(primary, refs, 3);
    assert_eq!(merged.len(), 3, "Should truncate to limit=3");
}

#[test]
fn test_merge_results_empty_inputs() {
    let primary: Vec<UnifiedResult> = vec![];
    let refs: Vec<(String, Vec<SearchResult>)> = vec![];

    let merged = merge_results(primary, refs, 10);
    assert!(merged.is_empty());
}

// ===== search_reference tests =====

#[test]
fn test_search_reference_applies_weight() {
    let store = TestStore::new();
    let c1 = test_chunk("weighted_fn", "fn weighted_fn() { test }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.7,
    };

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    let results = reference::search_reference(&ref_idx, &query, &filter, 10, 0.0).unwrap();
    assert!(!results.is_empty());

    // All scores should be multiplied by weight
    for r in &results {
        assert!(
            r.score <= 0.71,
            "Score {} should be <= weight 0.7 (with rounding)",
            r.score
        );
    }
}

#[test]
fn test_search_reference_weight_filters_below_threshold() {
    let store = TestStore::new();
    let c1 = test_chunk("fn_a", "fn fn_a() { test }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5, // Low weight
    };

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    // With weight=0.5, max score is ~0.5. Threshold 0.8 should filter everything.
    let results = reference::search_reference(&ref_idx, &query, &filter, 10, 0.8).unwrap();
    assert!(
        results.is_empty(),
        "Weighted scores below threshold should be filtered"
    );
}

#[test]
fn test_search_reference_by_name_weight() {
    let store = TestStore::new();
    let c1 = test_chunk("lookup_fn", "fn lookup_fn() { lookup }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.6,
    };

    let results = reference::search_reference_by_name(&ref_idx, "lookup_fn", 10, 0.0).unwrap();
    assert!(!results.is_empty());

    // Score should be scaled by weight
    for r in &results {
        assert!(
            r.score <= 0.61,
            "Name search score {} should be <= weight 0.6",
            r.score
        );
    }
}

// ===== validate_ref_name tests =====

#[test]
fn test_validate_ref_name_edge_cases() {
    // Single character names are valid
    assert!(reference::validate_ref_name("a").is_ok());

    // Numeric names are valid
    assert!(reference::validate_ref_name("123").is_ok());

    // Dots are valid (not ".." though)
    assert!(reference::validate_ref_name("v1.0").is_ok());

    // Hyphens and underscores
    assert!(reference::validate_ref_name("my-ref_v2").is_ok());

    // Double dot in name
    assert!(reference::validate_ref_name("foo..bar").is_err());
}

// ===== search_reference_unweighted tests =====

#[test]
fn test_search_reference_unweighted_returns_raw_scores() {
    let store = TestStore::new();
    let c1 = test_chunk("unweighted_fn", "fn unweighted_fn() { test }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5, // Low weight — should NOT be applied
    };

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    let unweighted =
        reference::search_reference_unweighted(&ref_idx, &query, &filter, 10, 0.0).unwrap();
    assert!(!unweighted.is_empty());

    // Score should NOT be multiplied by weight — raw scores should be higher
    for r in &unweighted {
        assert!(
            r.score > 0.51,
            "Unweighted score {} should be above weight 0.5 (raw score)",
            r.score
        );
    }
}

#[test]
fn test_search_reference_unweighted_vs_weighted() {
    let store = TestStore::new();
    let c1 = test_chunk("compare_fn", "fn compare_fn() { compare }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store_w = cqs::Store::open(&store.db_path()).unwrap();
    let ref_store_u = cqs::Store::open(&store.db_path()).unwrap();

    let weight = 0.6;
    let ref_idx_w = ReferenceIndex {
        name: "weighted".to_string(),
        store: ref_store_w,
        index: None,
        weight,
    };
    let ref_idx_u = ReferenceIndex {
        name: "unweighted".to_string(),
        store: ref_store_u,
        index: None,
        weight,
    };

    let query = mock_embedding(1.0);
    let filter = SearchFilter::default();

    let weighted = reference::search_reference(&ref_idx_w, &query, &filter, 10, 0.0).unwrap();
    let unweighted =
        reference::search_reference_unweighted(&ref_idx_u, &query, &filter, 10, 0.0).unwrap();

    assert!(!weighted.is_empty());
    assert!(!unweighted.is_empty());

    // Unweighted score should be higher than weighted score
    let w_score = weighted[0].score;
    let u_score = unweighted[0].score;
    assert!(
        u_score > w_score,
        "Unweighted {} should be > weighted {} (weight={})",
        u_score,
        w_score,
        weight
    );

    // Weighted should be approximately unweighted * weight
    let expected = u_score * weight;
    assert!(
        (w_score - expected).abs() < 0.01,
        "Weighted {} should ≈ unweighted {} * weight {} = {}",
        w_score,
        u_score,
        weight,
        expected
    );
}

#[test]
fn test_search_reference_by_name_unweighted() {
    let store = TestStore::new();
    let c1 = test_chunk("name_search_fn", "fn name_search_fn() { test }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5,
    };

    let results =
        reference::search_reference_by_name_unweighted(&ref_idx, "name_search_fn", 10, 0.0)
            .unwrap();
    assert!(!results.is_empty());

    // Scores should NOT be attenuated by weight
    for r in &results {
        assert!(
            r.score > 0.51,
            "Unweighted name search score {} should be > weight 0.5",
            r.score
        );
    }
}

#[test]
fn test_search_reference_by_name_unweighted_threshold() {
    let store = TestStore::new();
    let c1 = test_chunk("threshold_fn", "fn threshold_fn() { test }");
    insert_chunks(&store, &[c1], 1.0);

    let ref_store = cqs::Store::open(&store.db_path()).unwrap();
    let ref_idx = ReferenceIndex {
        name: "test-ref".to_string(),
        store: ref_store,
        index: None,
        weight: 0.5,
    };

    // Very high threshold should filter everything
    let results =
        reference::search_reference_by_name_unweighted(&ref_idx, "threshold_fn", 10, 2.0).unwrap();
    assert!(
        results.is_empty(),
        "Threshold above max score should return empty"
    );
}

// ===== Integration: merge with weighted reference results =====

#[test]
fn test_merge_weighted_ref_results_rank_correctly() {
    // Simulate: primary result at 0.8, reference result at raw 0.9 * weight 0.7 = 0.63
    let primary = vec![UnifiedResult::Code(make_code_result("primary_fn", 0.8))];
    let refs = vec![(
        "ref".to_string(),
        vec![make_code_result("ref_fn", 0.63)], // Already weighted
    )];

    let merged = merge_results(primary, refs, 10);
    assert_eq!(merged.len(), 2);
    // Primary (0.8) should rank above weighted ref (0.63)
    assert!(merged[0].source.is_none(), "Primary should rank first");
    assert_eq!(merged[1].source.as_deref(), Some("ref"));
}
