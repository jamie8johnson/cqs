//! Notes tests (T4, T5, T6)
//!
//! Tests for search_notes_by_ids, note_embeddings, and note_stats.

mod common;

use common::{mock_embedding, TestStore};
use cqs::note::Note;
use std::path::PathBuf;

/// Helper to create a test note
fn test_note(id: &str, text: &str, sentiment: f32) -> Note {
    Note {
        id: id.to_string(),
        text: text.to_string(),
        sentiment,
        mentions: vec![],
    }
}

// ===== search_notes_by_ids tests =====

#[test]
fn test_search_notes_by_ids_empty() {
    let store = TestStore::new();

    // Insert a note so we have something in the DB
    let note = test_note("1", "Test note", 0.0);
    let embedding = mock_embedding(1.0);
    store
        .upsert_notes_batch(
            &[(note, embedding.clone())],
            &PathBuf::from("notes.toml"),
            12345,
        )
        .unwrap();

    // Search with empty candidate list
    let results = store.search_notes_by_ids(&[], &embedding, 10, 0.0).unwrap();
    assert!(
        results.is_empty(),
        "Empty candidate list should return no results"
    );
}

#[test]
fn test_search_notes_by_ids_found() {
    let store = TestStore::new();

    // Insert multiple notes - all with same direction embedding for simplicity
    // (mock_embedding normalizes, so similar positive seeds = same direction)
    let note1 = test_note("1", "First note about errors", -0.5);
    let note2 = test_note("2", "Second note about patterns", 0.5);
    let note3 = test_note("3", "Third neutral note", 0.0);

    let emb = mock_embedding(1.0);

    store
        .upsert_notes_batch(
            &[
                (note1, emb.clone()),
                (note2, emb.clone()),
                (note3, emb.clone()),
            ],
            &PathBuf::from("notes.toml"),
            12345,
        )
        .unwrap();

    // Search for notes 1 and 2 only (not 3)
    let candidates = vec!["1", "2"];
    let results = store
        .search_notes_by_ids(&candidates, &emb, 10, 0.0)
        .unwrap();

    // Should find exactly 2 notes (the candidates), not note 3
    assert_eq!(results.len(), 2, "Should find 2 notes from candidates");

    let ids: Vec<_> = results.iter().map(|r| r.note.id.as_str()).collect();
    assert!(ids.contains(&"1"), "Should find note 1");
    assert!(ids.contains(&"2"), "Should find note 2");
    assert!(
        !ids.contains(&"3"),
        "Should NOT find note 3 (not in candidates)"
    );

    // All should have high similarity (identical embeddings)
    for r in &results {
        assert!(
            r.score > 0.99,
            "Identical embeddings should have score ~1.0"
        );
    }
}

#[test]
fn test_search_notes_by_ids_below_threshold() {
    let store = TestStore::new();

    // Insert note with embedding pointing in opposite direction
    let note = test_note("1", "Test note", 0.0);
    let note_embedding = mock_embedding(-1.0);
    let query_embedding = mock_embedding(1.0);

    store
        .upsert_notes_batch(
            &[(note, note_embedding)],
            &PathBuf::from("notes.toml"),
            12345,
        )
        .unwrap();

    // Search with high threshold - opposite embeddings won't match
    let results = store
        .search_notes_by_ids(&["1"], &query_embedding, 10, 0.9)
        .unwrap();

    assert!(
        results.is_empty(),
        "Notes below threshold should not be returned"
    );
}

#[test]
fn test_search_notes_by_ids_limit() {
    let store = TestStore::new();

    // Insert 5 notes
    let notes: Vec<_> = (1..=5)
        .map(|i| {
            let note = test_note(&i.to_string(), &format!("Note {}", i), 0.0);
            let emb = mock_embedding(1.0 + i as f32 * 0.01);
            (note, emb)
        })
        .collect();

    store
        .upsert_notes_batch(&notes, &PathBuf::from("notes.toml"), 12345)
        .unwrap();

    let candidates: Vec<&str> = (1..=5)
        .map(|i| {
            // Leak strings to get &str - ok for tests
            Box::leak(i.to_string().into_boxed_str()) as &str
        })
        .collect();

    let query = mock_embedding(1.0);
    let results = store
        .search_notes_by_ids(&candidates, &query, 2, 0.0)
        .unwrap();

    assert_eq!(results.len(), 2, "Should respect limit of 2");
}

// ===== note_embeddings tests =====

#[test]
fn test_note_embeddings_empty() {
    let store = TestStore::new();

    let embeddings = store.note_embeddings().unwrap();
    assert!(
        embeddings.is_empty(),
        "Empty store should have no note embeddings"
    );
}

#[test]
fn test_note_embeddings_returns_prefixed_ids() {
    let store = TestStore::new();

    let note1 = test_note("abc123", "First note", 0.0);
    let note2 = test_note("xyz789", "Second note", 0.5);

    store
        .upsert_notes_batch(
            &[(note1, mock_embedding(1.0)), (note2, mock_embedding(2.0))],
            &PathBuf::from("notes.toml"),
            12345,
        )
        .unwrap();

    let embeddings = store.note_embeddings().unwrap();
    assert_eq!(embeddings.len(), 2);

    // All IDs should have "note:" prefix
    for (id, _emb) in &embeddings {
        assert!(
            id.starts_with("note:"),
            "Note embedding ID should have 'note:' prefix, got: {}",
            id
        );
    }

    // Check specific IDs
    let ids: Vec<_> = embeddings.iter().map(|(id, _)| id.as_str()).collect();
    assert!(ids.contains(&"note:abc123"));
    assert!(ids.contains(&"note:xyz789"));
}

// ===== note_stats tests =====

#[test]
fn test_note_stats_empty() {
    let store = TestStore::new();

    let (total, warnings, patterns) = store.note_stats().unwrap();
    assert_eq!(total, 0);
    assert_eq!(warnings, 0);
    assert_eq!(patterns, 0);
}

#[test]
fn test_note_stats_sentiments() {
    let store = TestStore::new();

    // Create notes with various sentiments
    // Warnings: sentiment < -0.3
    // Patterns: sentiment > 0.3
    // Neutral: -0.3 <= sentiment <= 0.3
    let notes = vec![
        (test_note("1", "Warning 1", -1.0), mock_embedding(1.0)), // warning
        (test_note("2", "Warning 2", -0.5), mock_embedding(1.0)), // warning
        (test_note("3", "Neutral", 0.0), mock_embedding(1.0)),    // neutral
        (
            test_note("4", "Slightly positive", 0.2),
            mock_embedding(1.0),
        ), // neutral (within threshold)
        (test_note("5", "Pattern 1", 0.5), mock_embedding(1.0)),  // pattern
        (test_note("6", "Pattern 2", 1.0), mock_embedding(1.0)),  // pattern
    ];

    store
        .upsert_notes_batch(&notes, &PathBuf::from("notes.toml"), 12345)
        .unwrap();

    let (total, warnings, patterns) = store.note_stats().unwrap();
    assert_eq!(total, 6, "Should have 6 total notes");
    assert_eq!(warnings, 2, "Should have 2 warnings (sentiment < -0.3)");
    assert_eq!(patterns, 2, "Should have 2 patterns (sentiment > 0.3)");
}
