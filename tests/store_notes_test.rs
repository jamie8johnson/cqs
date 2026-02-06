//! Notes tests (T4, T5, T6)
//!
//! Tests for note_embeddings and note_stats.

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

    let ns = store.note_stats().unwrap();
    assert_eq!(ns.total, 0);
    assert_eq!(ns.warnings, 0);
    assert_eq!(ns.patterns, 0);
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

    let ns = store.note_stats().unwrap();
    assert_eq!(ns.total, 6, "Should have 6 total notes");
    assert_eq!(ns.warnings, 2, "Should have 2 warnings (sentiment < -0.3)");
    assert_eq!(ns.patterns, 2, "Should have 2 patterns (sentiment > 0.3)");
}
