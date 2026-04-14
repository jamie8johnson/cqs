//! Note-based score boosting.

use std::collections::HashMap;
use std::sync::Arc;

use crate::note::path_matches_mention;
use crate::store::helpers::NoteSummary;

use super::config::ScoringConfig;

/// Compute the note-based score boost for a chunk.
/// Checks if any note's mentions match the chunk's file path or name.
/// When multiple notes match, takes the strongest absolute sentiment
/// (preserving sign) to avoid averaging away strong signals.
/// Returns a multiplier: `1.0 + sentiment * ScoringConfig::DEFAULT.note_boost_factor`
/// Production code uses [`NoteBoostIndex::boost`] for amortized O(1) lookups.
/// This function is retained for unit tests.
#[cfg(test)]
fn note_boost(file_path: &str, chunk_name: &str, notes: &[NoteSummary]) -> f32 {
    let mut strongest: Option<f32> = None;
    for note in notes {
        for mention in &note.mentions {
            if path_matches_mention(file_path, mention) || chunk_name == mention {
                match strongest {
                    Some(prev) if note.sentiment.abs() > prev.abs() => {
                        strongest = Some(note.sentiment);
                    }
                    None => {
                        strongest = Some(note.sentiment);
                    }
                    _ => {}
                }
                break; // This note already matched, check next note
            }
        }
    }
    match strongest {
        Some(s) => 1.0 + s * ScoringConfig::DEFAULT.note_boost_factor,
        None => 1.0,
    }
}

/// Pre-computed note boost lookup for O(1) name matching and reduced path scans.
/// Built once from notes before the scoring loop, amortizing the O(notes x mentions)
/// cost across all chunks. Name mentions use exact HashMap lookup (O(1)).
/// Path mentions are stored separately for suffix/prefix matching, but with only
/// the path-type mentions instead of all mentions.
pub(crate) struct NoteBoostIndex<'a> {
    /// Exact name -> strongest sentiment (absolute value wins, preserving sign)
    #[cfg(test)]
    pub(super) name_sentiments: HashMap<&'a str, f32>,
    #[cfg(not(test))]
    name_sentiments: HashMap<&'a str, f32>,
    /// (mention_str, sentiment) pairs for path-based mentions
    #[cfg(test)]
    pub(super) path_mentions: Vec<(&'a str, f32)>,
    #[cfg(not(test))]
    path_mentions: Vec<(&'a str, f32)>,
}

impl<'a> NoteBoostIndex<'a> {
    /// Build the lookup index from notes. O(notes x mentions), done once.
    pub fn new(notes: &'a [NoteSummary]) -> Self {
        let mut name_sentiments: HashMap<&'a str, f32> = HashMap::new();
        let mut path_mentions: Vec<(&'a str, f32)> = Vec::new();

        for note in notes {
            for mention in &note.mentions {
                // Heuristic: mentions containing '/' or '.' or '\' are path-like,
                // others are name-like (exact match on chunk name)
                let is_path_like =
                    mention.contains('/') || mention.contains('.') || mention.contains('\\');
                if is_path_like {
                    path_mentions.push((mention.as_str(), note.sentiment));
                } else {
                    let entry = name_sentiments.entry(mention.as_str()).or_insert(0.0);
                    if note.sentiment.abs() > entry.abs() {
                        *entry = note.sentiment;
                    }
                }
            }
        }

        // AC-11: Deduplicate path mentions — keep strongest sentiment per mention string
        let mut deduped_paths: HashMap<&'a str, f32> = HashMap::new();
        for (mention, sentiment) in &path_mentions {
            let entry = deduped_paths.entry(mention).or_insert(0.0);
            if sentiment.abs() > entry.abs() {
                *entry = *sentiment;
            }
        }
        let path_mentions: Vec<(&'a str, f32)> = deduped_paths.into_iter().collect();

        Self {
            name_sentiments,
            path_mentions,
        }
    }

    /// Compute the note-based score boost for a chunk.
    /// Checks name mentions via HashMap lookup (O(1)), then scans path mentions
    /// for suffix/prefix matches. Takes strongest absolute sentiment across all
    /// matches (preserving sign).
    /// Returns a multiplier: `1.0 + sentiment * note_boost_factor`
    #[inline]
    pub fn boost(&self, file_path: &str, chunk_name: &str) -> f32 {
        let mut strongest: Option<f32> = None;

        // O(1) name lookup
        if let Some(&sentiment) = self.name_sentiments.get(chunk_name) {
            strongest = Some(sentiment);
        }

        // Path mention scan (only path-like mentions, not all mentions)
        for &(mention, sentiment) in &self.path_mentions {
            if path_matches_mention(file_path, mention) {
                match strongest {
                    Some(prev) if sentiment.abs() > prev.abs() => {
                        strongest = Some(sentiment);
                    }
                    None => {
                        strongest = Some(sentiment);
                    }
                    _ => {}
                }
            }
        }

        match strongest {
            Some(s) => 1.0 + s * ScoringConfig::DEFAULT.note_boost_factor,
            None => 1.0,
        }
    }
}

/// Owned, shareable counterpart to [`NoteBoostIndex`].
///
/// PF-V1.25-4: `NoteBoostIndex<'a>` holds `&'a str` borrows into the
/// caller-supplied `&[NoteSummary]`, which means it cannot be stored on a
/// long-lived cache (e.g. `Store::note_boost_cache`) without fighting the
/// borrow checker. `OwnedNoteBoostIndex` copies the mention strings into
/// its own maps so the index is self-contained and can be shared as
/// `Arc<OwnedNoteBoostIndex>` across search calls.
///
/// Build cost is the same as `NoteBoostIndex::new` plus a `String::clone`
/// per mention (typically a handful of notes × a few mentions each).
/// Amortized across all searches between two note changes the build cost
/// becomes negligible.
///
/// The `boost()` method is byte-for-byte equivalent to
/// `NoteBoostIndex::boost`, just reading from `String`/`&str` rather than
/// `&'a str`.
pub(crate) struct OwnedNoteBoostIndex {
    name_sentiments: HashMap<String, f32>,
    path_mentions: Vec<(String, f32)>,
}

impl OwnedNoteBoostIndex {
    /// Build the owned lookup index from notes. Same shape as
    /// [`NoteBoostIndex::new`]; the only difference is `.to_string()` on
    /// mention keys so the index doesn't borrow from the input.
    pub fn new(notes: &[NoteSummary]) -> Self {
        let mut name_sentiments: HashMap<String, f32> = HashMap::new();
        let mut raw_paths: Vec<(String, f32)> = Vec::new();

        for note in notes {
            for mention in &note.mentions {
                let is_path_like =
                    mention.contains('/') || mention.contains('.') || mention.contains('\\');
                if is_path_like {
                    raw_paths.push((mention.clone(), note.sentiment));
                } else {
                    let entry = name_sentiments.entry(mention.clone()).or_insert(0.0);
                    if note.sentiment.abs() > entry.abs() {
                        *entry = note.sentiment;
                    }
                }
            }
        }

        // Deduplicate path mentions — keep strongest sentiment per mention.
        // (Mirrors `NoteBoostIndex::new`'s AC-11 logic.)
        let mut deduped: HashMap<String, f32> = HashMap::new();
        for (mention, sentiment) in raw_paths {
            let entry = deduped.entry(mention).or_insert(0.0);
            if sentiment.abs() > entry.abs() {
                *entry = sentiment;
            }
        }
        let path_mentions: Vec<(String, f32)> = deduped.into_iter().collect();

        Self {
            name_sentiments,
            path_mentions,
        }
    }

    /// Compute the note-based score boost for a chunk. Byte-equivalent to
    /// [`NoteBoostIndex::boost`] — same sentinel selection (strongest
    /// absolute value wins, preserving sign) and same multiplier formula.
    #[inline]
    pub fn boost(&self, file_path: &str, chunk_name: &str) -> f32 {
        let mut strongest: Option<f32> = None;

        if let Some(&sentiment) = self.name_sentiments.get(chunk_name) {
            strongest = Some(sentiment);
        }

        for (mention, sentiment) in &self.path_mentions {
            if path_matches_mention(file_path, mention) {
                match strongest {
                    Some(prev) if sentiment.abs() > prev.abs() => {
                        strongest = Some(*sentiment);
                    }
                    None => {
                        strongest = Some(*sentiment);
                    }
                    _ => {}
                }
            }
        }

        match strongest {
            Some(s) => 1.0 + s * ScoringConfig::DEFAULT.note_boost_factor,
            None => 1.0,
        }
    }
}

/// Type-erasing wrapper so scoring paths can accept either the borrowed
/// [`NoteBoostIndex`] or a cached [`OwnedNoteBoostIndex`] without the
/// caller committing to one at type-construction time.
pub(crate) enum NoteBoost<'a> {
    Borrowed(NoteBoostIndex<'a>),
    Owned(Arc<OwnedNoteBoostIndex>),
}

impl<'a> NoteBoost<'a> {
    #[inline]
    pub fn boost(&self, file_path: &str, chunk_name: &str) -> f32 {
        match self {
            NoteBoost::Borrowed(b) => b.boost(file_path, chunk_name),
            NoteBoost::Owned(o) => o.boost(file_path, chunk_name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a test `NoteSummary` with the provided sentiment score and mentions.
    fn make_note(sentiment: f32, mentions: &[&str]) -> NoteSummary {
        NoteSummary {
            id: "note:test".to_string(),
            text: "test note".to_string(),
            sentiment,
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
        }
    }

    // ===== note_boost tests =====

    #[test]
    fn test_note_boost_no_notes() {
        let boost = note_boost("src/lib.rs", "my_fn", &[]);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_no_match() {
        let notes = vec![make_note(-0.5, &["other.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_file_match_negative() {
        let notes = vec![make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_file_match_positive() {
        let notes = vec![make_note(1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_name_match() {
        let notes = vec![make_note(0.5, &["my_fn"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.075).abs() < 0.001,
            "Expected ~1.075, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_wins() {
        // Two notes: weak positive and strong negative. Strong negative should win.
        let notes = vec![make_note(0.5, &["lib.rs"]), make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_absolute_preserves_sign() {
        // Two notes: strong positive and weak negative. Strong positive should win.
        let notes = vec![make_note(1.0, &["lib.rs"]), make_note(-0.5, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
    }

    // ===== NoteBoostIndex tests (TC-2) =====

    #[test]
    fn test_note_boost_index_empty_notes() {
        let notes: Vec<NoteSummary> = vec![];
        let index = NoteBoostIndex::new(&notes);
        assert_eq!(index.boost("src/lib.rs", "my_fn"), 1.0);
    }

    #[test]
    fn test_note_boost_index_name_mention_positive() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "good pattern".into(),
            sentiment: 0.5,
            mentions: vec!["my_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "my_fn");
        assert!(
            boost > 1.0,
            "Positive sentiment should boost > 1.0, got {boost}"
        );
        assert!((boost - (1.0 + 0.5 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_name_mention_negative() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "buggy code".into(),
            sentiment: -1.0,
            mentions: vec!["broken_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "broken_fn");
        assert!(
            boost < 1.0,
            "Negative sentiment should reduce score, got {boost}"
        );
        assert!((boost - (1.0 - 1.0 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_path_mention() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "important file".into(),
            sentiment: 0.5,
            mentions: vec!["src/search.rs".into()],
        }];
        let index = NoteBoostIndex::new(&notes);

        // Path mention should match file containing the path
        let boost = index.boost("src/search.rs", "unrelated_fn");
        assert!(
            boost > 1.0,
            "Path mention should boost matching file, got {boost}"
        );

        // Non-matching path should not be boosted
        let no_boost = index.boost("src/lib.rs", "unrelated_fn");
        assert_eq!(no_boost, 1.0, "Non-matching path should not be boosted");
    }

    #[test]
    fn test_note_boost_index_strongest_absolute_wins() {
        let notes = vec![
            NoteSummary {
                id: "1".into(),
                text: "mildly good".into(),
                sentiment: 0.5,
                mentions: vec!["my_fn".into()],
            },
            NoteSummary {
                id: "2".into(),
                text: "very bad".into(),
                sentiment: -1.0,
                mentions: vec!["my_fn".into()],
            },
        ];
        let index = NoteBoostIndex::new(&notes);
        let boost = index.boost("src/lib.rs", "my_fn");
        // -1.0 has stronger absolute value than 0.5, so it should win
        assert!(
            boost < 1.0,
            "Stronger negative should win over weaker positive, got {boost}"
        );
        assert!((boost - (1.0 - 1.0 * ScoringConfig::DEFAULT.note_boost_factor)).abs() < 1e-6);
    }

    #[test]
    fn test_note_boost_index_name_vs_path_classification() {
        // "search.rs" contains '.' so it's path-like
        // "my_fn" has no separators so it's name-like
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "note".into(),
            sentiment: 0.5,
            mentions: vec!["my_fn".into(), "search.rs".into()],
        }];
        let index = NoteBoostIndex::new(&notes);

        // Name-like mention should only match chunk name, not file path
        assert!(index.name_sentiments.contains_key("my_fn"));
        assert!(!index.name_sentiments.contains_key("search.rs"));
        assert_eq!(index.path_mentions.len(), 1);
    }

    #[test]
    fn test_note_boost_index_no_match() {
        let notes = vec![NoteSummary {
            id: "1".into(),
            text: "specific note".into(),
            sentiment: 1.0,
            mentions: vec!["other_fn".into()],
        }];
        let index = NoteBoostIndex::new(&notes);
        assert_eq!(index.boost("src/lib.rs", "my_fn"), 1.0);
    }
}
