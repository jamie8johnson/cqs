//! Note parsing and types
//!
//! Notes are unified memory entries - surprises worth remembering.
//! Replaces separate Scar and Hunch types with a simpler schema.

use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

/// Sentiment thresholds for classification
///
/// 0.3 chosen to separate neutral observations from significant notes:
/// - Values near 0 are neutral observations
/// - Values beyond ±0.3 indicate meaningful sentiment (warning/pattern)
/// - Matches discrete values: -1, -0.5, 0, 0.5, 1 (see CLAUDE.md)
pub const SENTIMENT_NEGATIVE_THRESHOLD: f32 = -0.3;
pub const SENTIMENT_POSITIVE_THRESHOLD: f32 = 0.3;

/// Maximum number of notes to parse from a single file.
/// Prevents memory exhaustion from malicious or corrupted note files.
const MAX_NOTES: usize = 10_000;

/// Errors that can occur when parsing notes
#[derive(Error, Debug)]
pub enum NoteError {
    /// File read/write error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Invalid TOML syntax or structure
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Raw note entry from TOML
#[derive(Debug, Deserialize)]
struct NoteEntry {
    /// Sentiment: -1.0 (negative/pain) to +1.0 (positive/gain)
    #[serde(default)]
    sentiment: f32,
    /// The note content - natural language
    text: String,
    /// Code paths/functions mentioned (for linking)
    #[serde(default)]
    mentions: Vec<String>,
}

/// TOML file structure
#[derive(Debug, Deserialize)]
struct NoteFile {
    #[serde(default)]
    note: Vec<NoteEntry>,
}

/// A parsed note entry
#[derive(Debug, Clone)]
pub struct Note {
    /// Unique identifier: "note:{index}"
    pub id: String,
    /// The note content
    pub text: String,
    /// Sentiment: -1.0 to +1.0
    pub sentiment: f32,
    /// Code paths/functions mentioned
    pub mentions: Vec<String>,
}

impl Note {
    /// Generate embedding text for this note
    ///
    /// Adds a prefix based on sentiment to help with retrieval:
    /// - Negative sentiment: "Warning: "
    /// - Positive sentiment: "Pattern: "
    /// - Neutral: no prefix
    pub fn embedding_text(&self) -> String {
        let prefix = if self.sentiment < SENTIMENT_NEGATIVE_THRESHOLD {
            "Warning: "
        } else if self.sentiment > SENTIMENT_POSITIVE_THRESHOLD {
            "Pattern: "
        } else {
            ""
        };
        format!("{}{}", prefix, self.text)
    }

    /// Get the sentiment value
    pub fn sentiment(&self) -> f32 {
        self.sentiment
    }

    /// Check if this is a warning (negative sentiment)
    pub fn is_warning(&self) -> bool {
        self.sentiment < SENTIMENT_NEGATIVE_THRESHOLD
    }

    /// Check if this is a pattern (positive sentiment)
    pub fn is_pattern(&self) -> bool {
        self.sentiment > SENTIMENT_POSITIVE_THRESHOLD
    }
}

/// Parse notes from a notes.toml file
pub fn parse_notes(path: &Path) -> Result<Vec<Note>, NoteError> {
    let content = std::fs::read_to_string(path)?;
    parse_notes_str(&content)
}

/// Parse notes from a string (for testing)
///
/// Note IDs are generated from a hash of the text content (first 8 hex chars).
/// This ensures IDs are stable when notes are reordered in the file.
/// Limited to MAX_NOTES (10k) to prevent memory exhaustion.
pub fn parse_notes_str(content: &str) -> Result<Vec<Note>, NoteError> {
    let file: NoteFile = toml::from_str(content)?;

    let notes = file
        .note
        .into_iter()
        .take(MAX_NOTES)
        .map(|entry| {
            // Use content hash for stable IDs (reordering notes won't break references)
            let hash = blake3::hash(entry.text.as_bytes());
            let id = format!("note:{}", &hash.to_hex()[..8]);

            Note {
                id,
                text: entry.text.trim().to_string(),
                sentiment: entry.sentiment.clamp(-1.0, 1.0),
                mentions: entry.mentions,
            }
        })
        .collect();

    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_notes() {
        let content = r#"
[[note]]
sentiment = -0.8
text = "tree-sitter version mismatch causes mysterious failures"
mentions = ["tree-sitter", "Cargo.toml"]

[[note]]
sentiment = 0.9
text = "OnceCell lazy init pattern works cleanly"
mentions = ["embedder.rs"]

[[note]]
text = "neutral observation without explicit sentiment"
"#;

        let notes = parse_notes_str(content).unwrap();
        assert_eq!(notes.len(), 3);

        assert_eq!(notes[0].sentiment, -0.8);
        assert!(notes[0].is_warning());
        assert!(notes[0].embedding_text().starts_with("Warning: "));

        assert_eq!(notes[1].sentiment, 0.9);
        assert!(notes[1].is_pattern());
        assert!(notes[1].embedding_text().starts_with("Pattern: "));

        assert_eq!(notes[2].sentiment, 0.0); // default
        assert!(!notes[2].is_warning());
        assert!(!notes[2].is_pattern());
    }

    #[test]
    fn test_sentiment_clamping() {
        let content = r#"
[[note]]
sentiment = -5.0
text = "way too negative"

[[note]]
sentiment = 99.0
text = "way too positive"
"#;

        let notes = parse_notes_str(content).unwrap();
        assert_eq!(notes[0].sentiment, -1.0);
        assert_eq!(notes[1].sentiment, 1.0);
    }

    #[test]
    fn test_empty_file() {
        let content = "# Just a comment\n";
        let notes = parse_notes_str(content).unwrap();
        assert!(notes.is_empty());
    }

    #[test]
    fn test_stable_ids_across_reordering() {
        // Original order
        let content1 = r#"
[[note]]
text = "first note"

[[note]]
text = "second note"
"#;

        // Reversed order
        let content2 = r#"
[[note]]
text = "second note"

[[note]]
text = "first note"
"#;

        let notes1 = parse_notes_str(content1).unwrap();
        let notes2 = parse_notes_str(content2).unwrap();

        // IDs should be stable based on content, not order
        assert_eq!(notes1[0].id, notes2[1].id); // "first note" has same ID
        assert_eq!(notes1[1].id, notes2[0].id); // "second note" has same ID

        // Verify ID format (note:8-hex-chars)
        assert!(notes1[0].id.starts_with("note:"));
        assert_eq!(notes1[0].id.len(), 5 + 8); // "note:" + 8 hex chars
    }

    // ===== Fuzz tests =====

    mod fuzz {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Fuzz: parse_notes_str should never panic on arbitrary input
            #[test]
            fn fuzz_parse_notes_str_no_panic(input in "\\PC{0,500}") {
                // We don't care about the result, just that it doesn't panic
                let _ = parse_notes_str(&input);
            }

            /// Fuzz: parse_notes_str with TOML-like structure
            #[test]
            fn fuzz_parse_notes_toml_like(
                sentiment in -10.0f64..10.0,
                text in "[a-zA-Z0-9 ]{0,100}",
                mention in "[a-z.]{1,20}"
            ) {
                let input = format!(
                    "[[note]]\nsentiment = {}\ntext = \"{}\"\nmentions = [\"{}\"]",
                    sentiment, text, mention
                );
                let _ = parse_notes_str(&input);
            }

            /// Fuzz: deeply nested/repeated structures
            #[test]
            fn fuzz_parse_notes_repeated(count in 0usize..50) {
                let input: String = (0..count)
                    .map(|i| format!("[[note]]\ntext = \"note {}\"\n", i))
                    .collect();
                let result = parse_notes_str(&input);
                if let Ok(notes) = result {
                    prop_assert!(notes.len() <= count);
                }
            }
        }
    }
}
