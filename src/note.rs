//! Note parsing and types
//!
//! Notes are unified memory entries - surprises worth remembering.
//! Replaces separate Scar and Hunch types with a simpler schema.

use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NoteError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
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
        let prefix = if self.sentiment < -0.3 {
            "Warning: "
        } else if self.sentiment > 0.3 {
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
        self.sentiment < -0.3
    }

    /// Check if this is a pattern (positive sentiment)
    pub fn is_pattern(&self) -> bool {
        self.sentiment > 0.3
    }
}

/// Parse notes from a notes.toml file
pub fn parse_notes(path: &Path) -> Result<Vec<Note>, NoteError> {
    let content = std::fs::read_to_string(path)?;
    parse_notes_str(&content)
}

/// Parse notes from a string (for testing)
pub fn parse_notes_str(content: &str) -> Result<Vec<Note>, NoteError> {
    let file: NoteFile = toml::from_str(content)?;

    let notes = file
        .note
        .into_iter()
        .enumerate()
        .map(|(i, entry)| {
            let id = format!("note:{}", i);

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
}
