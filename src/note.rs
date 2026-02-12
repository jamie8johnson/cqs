//! Note parsing and types
//!
//! Notes are developer observations with sentiment, stored in TOML and
//! indexed for semantic search.

use serde::{Deserialize, Serialize};
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
    /// TOML serialization error
    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    /// Note not found
    #[error("Note not found: {0}")]
    NotFound(String),
}

/// Raw note entry from TOML (round-trippable via serde)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoteEntry {
    /// Sentiment: -1.0 (negative/pain) to +1.0 (positive/gain)
    #[serde(default)]
    pub sentiment: f32,
    /// The note content - natural language
    pub text: String,
    /// Code paths/functions mentioned (for linking)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<String>,
}

/// TOML file structure (round-trippable via serde)
#[derive(Debug, Deserialize, Serialize)]
pub struct NoteFile {
    #[serde(default)]
    pub note: Vec<NoteEntry>,
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

    /// Get the human-readable sentiment label for this note.
    ///
    /// Returns "WARNING" for negative, "PATTERN" for positive, "NOTE" for neutral.
    /// Used by read commands for note injection headers.
    pub fn sentiment_label(&self) -> &'static str {
        if self.sentiment < SENTIMENT_NEGATIVE_THRESHOLD {
            "WARNING"
        } else if self.sentiment > SENTIMENT_POSITIVE_THRESHOLD {
            "PATTERN"
        } else {
            "NOTE"
        }
    }
}

/// File header preserved across rewrites
pub const NOTES_HEADER: &str = "\
# Notes - unified memory for AI collaborators
# Surprises (prediction errors) worth remembering
# sentiment: DISCRETE values only: -1, -0.5, 0, 0.5, 1
#   -1 = serious pain, -0.5 = notable pain, 0 = neutral, 0.5 = notable gain, 1 = major win
";

/// Parse notes from a notes.toml file
pub fn parse_notes(path: &Path) -> Result<Vec<Note>, NoteError> {
    // Acquire shared lock and read from the locked handle directly
    use std::io::Read;
    let mut lock_file = std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| {
            NoteError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", path.display(), e),
            ))
        })?;
    lock_file.lock_shared().map_err(|e| {
        NoteError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Could not lock {} for reading: {}", path.display(), e),
        ))
    })?;

    // Size guard: notes.toml should be well under 10MB
    const MAX_NOTES_FILE_SIZE: u64 = 10 * 1024 * 1024;
    if let Ok(meta) = lock_file.metadata() {
        if meta.len() > MAX_NOTES_FILE_SIZE {
            return Err(NoteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{}: file too large ({}MB, limit {}MB)",
                    path.display(),
                    meta.len() / (1024 * 1024),
                    MAX_NOTES_FILE_SIZE / (1024 * 1024)
                ),
            )));
        }
    }
    let mut content = String::new();
    lock_file.read_to_string(&mut content).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", path.display(), e),
        ))
    })?;
    // lock_file dropped here, releasing shared lock
    parse_notes_str(&content)
}

/// Rewrite notes.toml by applying a mutation to the parsed entries.
///
/// Reads the file, parses into `NoteEntry` structs, applies `mutate`,
/// serializes back with the standard header, and writes atomically.
/// Holds an exclusive file lock for the entire read-modify-write cycle.
pub fn rewrite_notes_file(
    notes_path: &Path,
    mutate: impl FnOnce(&mut Vec<NoteEntry>) -> Result<(), NoteError>,
) -> Result<Vec<NoteEntry>, NoteError> {
    // Acquire exclusive lock before the read-modify-write cycle
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .open(notes_path)
        .map_err(|e| {
            NoteError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", notes_path.display(), e),
            ))
        })?;
    lock_file.lock().map_err(|e| {
        NoteError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Could not lock {} for writing: {}", notes_path.display(), e),
        ))
    })?;
    // lock_file held until end of function (dropped automatically)

    // Size guard (same limit as read path)
    const MAX_NOTES_FILE_SIZE: u64 = 10 * 1024 * 1024;
    if let Ok(meta) = std::fs::metadata(notes_path) {
        if meta.len() > MAX_NOTES_FILE_SIZE {
            return Err(NoteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{}: file too large ({}MB, limit {}MB)",
                    notes_path.display(),
                    meta.len() / (1024 * 1024),
                    MAX_NOTES_FILE_SIZE / (1024 * 1024)
                ),
            )));
        }
    }
    let content = std::fs::read_to_string(notes_path).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", notes_path.display(), e),
        ))
    })?;
    let mut file: NoteFile = toml::from_str(&content)?;

    mutate(&mut file.note)?;

    // Atomic write: temp file + rename (unpredictable suffix to prevent symlink attacks)
    let tmp_path = notes_path.with_extension(format!(
        "toml.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));

    let serialized = match toml::to_string_pretty(&file) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    };

    let output = format!("{}\n{}", NOTES_HEADER, serialized);
    std::fs::write(&tmp_path, &output).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", tmp_path.display(), e),
        ))
    })?;
    if let Err(rename_err) = std::fs::rename(&tmp_path, notes_path) {
        // Rename can fail with EXDEV on cross-device (Docker overlayfs, some CI).
        // Fall back to copy + remove.
        if let Err(copy_err) = std::fs::copy(&tmp_path, notes_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(NoteError::Io(std::io::Error::new(
                copy_err.kind(),
                format!(
                    "rename {} -> {} failed ({}), copy fallback also failed: {}",
                    tmp_path.display(),
                    notes_path.display(),
                    rename_err,
                    copy_err
                ),
            )));
        }
        let _ = std::fs::remove_file(&tmp_path);
    }

    Ok(file.note)
}

/// Parse notes from a string (for testing)
///
/// Note IDs are generated from a hash of the text content (first 16 hex chars = 64 bits).
/// This ensures IDs are stable when notes are reordered in the file.
/// With 16 hex chars, collision probability is ~0.003% at 10k notes (birthday paradox).
/// Limited to MAX_NOTES (10k) to prevent memory exhaustion.
pub fn parse_notes_str(content: &str) -> Result<Vec<Note>, NoteError> {
    let file: NoteFile = toml::from_str(content)?;

    let notes = file
        .note
        .into_iter()
        .take(MAX_NOTES)
        .map(|entry| {
            // Use content hash for stable IDs (reordering notes won't break references)
            // 16 hex chars = 64 bits, collision probability ~0.003% at 10k notes
            let hash = blake3::hash(entry.text.as_bytes());
            let id = format!("note:{}", &hash.to_hex()[..16]);

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

/// Check if a mention matches a path by component suffix matching.
/// "gather.rs" matches "src/gather.rs" but not "src/gatherer.rs"
/// "src/store" matches "src/store/chunks.rs" but not "my_src/store.rs"
pub fn path_matches_mention(path: &str, mention: &str) -> bool {
    // Check if mention matches as a path suffix (component-aligned)
    if let Some(stripped) = path.strip_suffix(mention) {
        // Must be at component boundary: empty prefix or ends with /
        stripped.is_empty() || stripped.ends_with('/')
    } else if let Some(stripped) = path.strip_prefix(mention) {
        // Check prefix match at component boundary
        stripped.is_empty() || stripped.starts_with('/')
    } else {
        false
    }
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

        // Verify ID format (note:16-hex-chars)
        assert!(notes1[0].id.starts_with("note:"));
        assert_eq!(notes1[0].id.len(), 5 + 16); // "note:" + 16 hex chars
    }

    // ===== rewrite_notes_file tests =====

    #[test]
    fn test_rewrite_update_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.toml");
        std::fs::write(
            &path,
            "# header\n\n[[note]]\nsentiment = -0.5\ntext = \"old text\"\nmentions = [\"file.rs\"]\n",
        )
        .unwrap();

        rewrite_notes_file(&path, |entries| {
            let entry = entries.iter_mut().find(|e| e.text == "old text").unwrap();
            entry.text = "new text".to_string();
            entry.sentiment = 0.5;
            Ok(())
        })
        .unwrap();

        let notes = parse_notes(&path).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "new text");
        assert_eq!(notes[0].sentiment, 0.5);
        assert_eq!(notes[0].mentions, vec!["file.rs"]);
    }

    #[test]
    fn test_rewrite_remove_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.toml");
        std::fs::write(
            &path,
            "[[note]]\ntext = \"keep\"\n\n[[note]]\ntext = \"remove\"\n",
        )
        .unwrap();

        rewrite_notes_file(&path, |entries| {
            entries.retain(|e| e.text != "remove");
            Ok(())
        })
        .unwrap();

        let notes = parse_notes(&path).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "keep");
    }

    #[test]
    fn test_rewrite_preserves_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.toml");
        std::fs::write(&path, "[[note]]\ntext = \"hello\"\n").unwrap();

        rewrite_notes_file(&path, |_entries| Ok(())).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.starts_with("# Notes"),
            "Should have standard header"
        );
    }

    #[test]
    fn test_rewrite_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.toml");
        std::fs::write(&path, "[[note]]\ntext = \"exists\"\n").unwrap();

        let result = rewrite_notes_file(&path, |entries| {
            entries
                .iter()
                .find(|e| e.text == "nonexistent")
                .ok_or_else(|| NoteError::NotFound("not found".into()))?;
            Ok(())
        });

        assert!(result.is_err());
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
