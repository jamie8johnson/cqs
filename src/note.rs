//! Note parsing and types
//!
//! Notes are developer observations with sentiment, stored in TOML and
//! indexed for semantic search.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Sentiment thresholds for classification
/// 0.3 chosen to separate neutral observations from significant notes:
/// - Values near 0 are neutral observations
/// - Values beyond ±0.3 indicate meaningful sentiment (warning/pattern)
/// - Matches discrete values: -1, -0.5, 0, 0.5, 1 (see CLAUDE.md)
pub const SENTIMENT_NEGATIVE_THRESHOLD: f32 = -0.3;
pub const SENTIMENT_POSITIVE_THRESHOLD: f32 = 0.3;

/// Maximum number of notes to parse from a single file.
/// Prevents memory exhaustion from malicious or corrupted note files.
///
/// SHL-V1.30-7: env override `CQS_NOTES_MAX_ENTRIES`. Documented in README.md.
const MAX_NOTES_DEFAULT: usize = 10_000;

/// Maximum size of `notes.toml` (in bytes) before reads/writes refuse to load
/// the file. Both the read-only `parse_notes` path and the read-modify-write
/// `rewrite_notes_file` path enforce the same cap so a truncated/corrupt
/// rewrite can't exceed it either.
///
/// SHL-V1.30-7: hoisted from per-call-site duplicate `const` declarations to
/// module scope. Env override `CQS_NOTES_MAX_FILE_SIZE`. Default 10 MiB.
const MAX_NOTES_FILE_SIZE_DEFAULT: u64 = 10 * 1024 * 1024;

/// SHL-V1.30-7: resolve `CQS_NOTES_MAX_FILE_SIZE` (default 10 MiB).
fn max_notes_file_size() -> u64 {
    std::env::var("CQS_NOTES_MAX_FILE_SIZE")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(MAX_NOTES_FILE_SIZE_DEFAULT)
}

/// SHL-V1.30-7: resolve `CQS_NOTES_MAX_ENTRIES` (default 10_000).
fn max_notes() -> usize {
    std::env::var("CQS_NOTES_MAX_ENTRIES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(MAX_NOTES_DEFAULT)
}

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
    /// v25 / #1133: free-form kind tag (`todo`, `design-decision`,
    /// `deprecation`, `known-bug`, …). When present, takes precedence
    /// over `sentiment`'s implicit "Warning:"/"Pattern:" mapping for
    /// the embedding-text prefix; the structural field also enables
    /// `cqs notes list --kind <kind>` filtering.
    ///
    /// Free-string by design: a closed enum would force a coordinated
    /// edit every time someone wants a new tag. The cqs convention is
    /// kebab-case lowercase; `cqs notes add --kind` does not validate
    /// the value beyond rejecting empty strings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// TOML file structure (round-trippable via serde)
#[derive(Debug, Deserialize, Serialize)]
pub struct NoteFile {
    #[serde(default)]
    pub note: Vec<NoteEntry>,
}

/// A parsed note entry
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    /// Unique identifier: "note:{index}"
    pub id: String,
    /// The note content
    pub text: String,
    /// Sentiment: -1.0 to +1.0
    pub sentiment: f32,
    /// Code paths/functions mentioned
    pub mentions: Vec<String>,
    /// v25 / #1133: optional structured kind tag (see [`NoteEntry::kind`]).
    /// Drives the embedding-text prefix when present; serialized to JSON
    /// only when set so the existing wire shape stays backward-compatible
    /// for kind-less notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl Note {
    /// Generate embedding text for this note.
    ///
    /// Prefix priority (#1133):
    /// 1. `kind` is set: `"<Kind>: "` — capitalized, kind takes
    ///    precedence over the sentiment-based fallback so structured
    ///    notes get a structured retrieval signal.
    /// 2. `kind` is None, sentiment < -0.3: `"Warning: "`.
    /// 3. `kind` is None, sentiment > +0.3: `"Pattern: "`.
    /// 4. `kind` is None, sentiment in [-0.3, +0.3]: no prefix.
    ///
    /// Capitalisation: kebab-case kinds (`design-decision`) become
    /// title-case prefixes (`Design-Decision: `) so the embedder sees
    /// readable English. Bare lowercase kinds (`todo`) become
    /// `Todo: `.
    pub fn embedding_text(&self) -> String {
        if let Some(kind) = self.kind.as_deref() {
            return format!("{}: {}", capitalize_kind_for_prefix(kind), self.text);
        }
        let prefix = if self.sentiment < SENTIMENT_NEGATIVE_THRESHOLD {
            "Warning: "
        } else if self.sentiment > SENTIMENT_POSITIVE_THRESHOLD {
            "Pattern: "
        } else {
            ""
        };
        format!("{}{}", prefix, self.text)
    }

    /// Returns the sentiment score of this analysis result.
    /// # Returns
    /// A floating-point value representing the sentiment score, typically in the range [-1.0, 1.0] where negative values indicate negative sentiment, zero indicates neutral sentiment, and positive values indicate positive sentiment.
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
    let _span = tracing::debug_span!("parse_notes", path = %path.display()).entered();
    // Lock a separate .lock file (shared) to coordinate with writers.
    // Using a separate lock file avoids the inode-vs-rename race: if we locked
    // the data file itself, a concurrent writer's atomic rename would orphan
    // our lock onto the old inode, letting a third process read stale data.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting notes,
    // but cannot protect against external Windows process modifications.
    let lock_path = path.with_extension("toml.lock");
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| {
            NoteError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", lock_path.display(), e),
            ))
        })?;
    lock_file.lock_shared().map_err(|e| {
        NoteError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!("Could not lock {} for reading: {}", lock_path.display(), e),
        ))
    })?;

    // Now open and read the data file (protected by the lock file)
    use std::io::Read;
    let mut data_file = std::fs::File::open(path).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", path.display(), e),
        ))
    })?;

    // Size guard: notes.toml should be well under 10MB. SHL-V1.30-7: env-overridable
    // via CQS_NOTES_MAX_FILE_SIZE, single source of truth at module scope.
    let max_size = max_notes_file_size();
    if let Ok(meta) = data_file.metadata() {
        if meta.len() > max_size {
            return Err(NoteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{}: file too large ({}MB, limit {}MB)",
                    path.display(),
                    meta.len() / (1024 * 1024),
                    max_size / (1024 * 1024)
                ),
            )));
        }
    }
    let mut content = String::new();
    data_file.read_to_string(&mut content).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", path.display(), e),
        ))
    })?;
    // lock_file dropped here, releasing shared lock
    parse_notes_str(&content)
}

/// Rewrite notes.toml by applying a mutation to the parsed entries.
/// Reads the file, parses into `NoteEntry` structs, applies `mutate`,
/// serializes back with the standard header, and writes atomically.
/// Holds an exclusive file lock for the entire read-modify-write cycle.
pub fn rewrite_notes_file(
    notes_path: &Path,
    mutate: impl FnOnce(&mut Vec<NoteEntry>) -> Result<(), NoteError>,
) -> Result<Vec<NoteEntry>, NoteError> {
    let _span = tracing::debug_span!("rewrite_notes_file", path = %notes_path.display()).entered();
    // Lock a separate .lock file (exclusive) to coordinate with readers/writers.
    // See parse_notes() for why we use a separate lock file instead of the data file.
    //
    // NOTE: File locking is advisory only on WSL over 9P (DrvFs/NTFS mounts).
    // This prevents concurrent cqs processes from corrupting notes,
    // but cannot protect against external Windows process modifications.
    let lock_path = notes_path.with_extension("toml.lock");
    let _lock_file = {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| {
                NoteError::Io(std::io::Error::new(
                    e.kind(),
                    format!("{}: {}", lock_path.display(), e),
                ))
            })?;
        f.lock().map_err(|e| {
            NoteError::Io(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!("Could not lock {} for writing: {}", lock_path.display(), e),
            ))
        })?;
        f // held until end of function
    };

    // Now open and read the data file (protected by the lock file)
    use std::io::Read;
    let mut data_file = std::fs::OpenOptions::new()
        .read(true)
        .open(notes_path)
        .map_err(|e| {
            NoteError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", notes_path.display(), e),
            ))
        })?;

    // Size guard (same limit as read path). SHL-V1.30-7: shared resolver.
    let max_size = max_notes_file_size();
    if let Ok(meta) = data_file.metadata() {
        if meta.len() > max_size {
            return Err(NoteError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{}: file too large ({}MB, limit {}MB)",
                    notes_path.display(),
                    meta.len() / (1024 * 1024),
                    max_size / (1024 * 1024)
                ),
            )));
        }
    }
    let mut content = String::new();
    data_file.read_to_string(&mut content).map_err(|e| {
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!("{}: {}", notes_path.display(), e),
        ))
    })?;
    let mut file: NoteFile = toml::from_str(&content)?;

    mutate(&mut file.note)?;

    // Atomic write: temp file + rename (unpredictable suffix to prevent symlink attacks)
    let suffix = crate::temp_suffix();
    let tmp_path = notes_path.with_extension(format!("toml.{:016x}.tmp", suffix));

    let serialized = match toml::to_string_pretty(&file) {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    };

    // RM-V1.33-8: wrap the write + permission fixup in a closure so
    // any intermediate failure (disk full, EIO, EROFS) cleans up the
    // tmp file before propagating. Previously the bare `?` on the
    // `std::fs::write` left `notes.toml.<hex>.tmp` files behind on
    // every failed attempt — names include 16 hex chars of randomness
    // so failures piled up rather than collided.
    let output = format!("{}\n{}", NOTES_HEADER, serialized);
    let write_result: Result<(), NoteError> = (|| {
        std::fs::write(&tmp_path, &output).map_err(|e| {
            NoteError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {}", tmp_path.display(), e),
            ))
        })?;

        // Restrict permissions BEFORE rename so the file is never world-readable.
        // set_permissions failure is logged at debug! and does not propagate —
        // it's a best-effort hardening, not a correctness requirement, so we
        // don't want to leak the tmp on a permission-set error.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            {
                tracing::debug!(path = %tmp_path.display(), error = %e, "Failed to set file permissions");
            }
        }
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }

    // atomic_replace: fsync tmp, rename with EXDEV fallback, fsync parent dir.
    // Previously the notes path open-coded the rename + fs::copy fallback but
    // never fsynced the tmp or the parent dir, so a power cut between write
    // and rename could lose notes that appeared committed to the user.
    crate::fs::atomic_replace(&tmp_path, notes_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        NoteError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "Failed to persist {} -> {}: {}",
                tmp_path.display(),
                notes_path.display(),
                e
            ),
        ))
    })?;

    Ok(file.note)
}

/// Parse notes from a string (for testing)
/// Note IDs are generated from a hash of the text content (first 16 hex chars = 64 bits).
/// This ensures IDs are stable when notes are reordered in the file.
/// With 16 hex chars, collision probability is ~0.003% at 10k notes (birthday paradox).
/// Limited to `CQS_NOTES_MAX_ENTRIES` (default 10k) to prevent memory exhaustion.
pub fn parse_notes_str(content: &str) -> Result<Vec<Note>, NoteError> {
    let file: NoteFile = toml::from_str(content)?;

    // SHL-V1.30-7: surface truncation rather than silently dropping entries.
    // Previously `.take(MAX_NOTES)` ate the surplus with no signal; now we warn so
    // users see they need to lift the cap (or split the file).
    let cap = max_notes();
    let total = file.note.len();
    if total > cap {
        tracing::warn!(
            total,
            cap,
            dropped = total - cap,
            "parse_notes_str: note count exceeds CQS_NOTES_MAX_ENTRIES; truncating"
        );
    }

    let notes = file
        .note
        .into_iter()
        .take(cap)
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
                kind: entry.kind.and_then(normalize_kind),
            }
        })
        .collect();

    Ok(notes)
}

/// Normalize a `kind` value: trim whitespace, lowercase, reject empty.
/// Returns `None` when the input was effectively empty so absent /
/// blank kinds round-trip as None.
///
/// We don't enforce a closed taxonomy — kebab-case lowercase is the
/// convention, but `cqs notes add --kind WeirdValue` is allowed.
fn normalize_kind(raw: String) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Capitalize a kebab-case kind for embedding-text prefix display.
/// `"design-decision"` → `"Design-Decision"`. Each `-`-separated
/// segment is title-cased; a bare `"todo"` becomes `"Todo"`. The
/// trailing `": "` separator is added by the caller.
fn capitalize_kind_for_prefix(kind: &str) -> String {
    kind.split('-')
        .map(|seg| {
            let mut chars = seg.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Normalize `path` for slash-matching without allocating when possible.
///
/// PF-V1.25-13: `normalize_slashes` always allocates a fresh `String` even
/// when the input has no backslashes (the Unix common case and, in practice,
/// most indexed paths on Windows that already came from `normalize_path`).
/// This helper returns `Cow::Borrowed(s)` when no substitution is needed,
/// avoiding the allocation entirely.
fn normalize_slashes_cow(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\\') {
        std::borrow::Cow::Owned(s.replace('\\', "/"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// Check if a mention matches a path by component suffix matching.
/// "gather.rs" matches "src/gather.rs" but not "src/gatherer.rs"
/// "src/store" matches "src/store/chunks.rs" but not "my_src/store.rs"
///
/// PF-V1.25-13: previously allocated two `String`s per (note mention ×
/// candidate) via `normalize_slashes`. In search scoring this runs
/// O(notes × path_mentions × candidates) times per query; on Unix every
/// allocation was wasted (no backslashes to replace) and on Windows only
/// paths that weren't already slash-normalized upstream needed the work.
/// Switched to `Cow<str>`: zero allocation on the already-slash-clean path,
/// identical semantics on the backslash path.
pub fn path_matches_mention(path: &str, mention: &str) -> bool {
    // Normalize backslashes to forward slashes for cross-platform matching
    let path = normalize_slashes_cow(path);
    let mention = normalize_slashes_cow(mention);
    let path: &str = path.as_ref();
    let mention: &str = mention.as_ref();

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

    // ===== TC-ADV-7: size guard + MAX_NOTES truncation =====
    //
    // `parse_notes` has two guards against memory exhaustion:
    //   1. A 10MB file-size cap (`MAX_NOTES_FILE_SIZE = 10 * 1024 * 1024`)
    //      that errors with `ErrorKind::InvalidData` before reading content.
    //   2. A `MAX_NOTES = 10_000` cap in `parse_notes_str` that silently
    //      truncates via `.take(MAX_NOTES)`.
    // Neither was exercised by the suite before TC-ADV-7.

    /// Over-sized notes file is rejected with a clear `InvalidData` error
    /// and the `"file too large"` phrase, before any TOML parsing.
    #[test]
    fn test_parse_notes_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.toml");

        // Build a > 10 MB file. Each entry is cheap to write; pad a single
        // entry's `text` with enough bytes to cross the threshold without
        // needing 10k individual entries (faster + avoids TOML parse cost
        // at the filesystem-read stage).
        //
        // Text size = 11 MB of `x` characters.
        let big_text: String = "x".repeat(11 * 1024 * 1024);
        let content = format!("[[note]]\ntext = \"{}\"\n", big_text);
        std::fs::write(&path, content).unwrap();

        let result = parse_notes(&path);
        match result {
            Err(NoteError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
                let msg = e.to_string();
                assert!(
                    msg.contains("file too large"),
                    "error should mention size limit, got: {}",
                    msg
                );
            }
            other => panic!(
                "expected Io(InvalidData)/'file too large', got: {:?}",
                other.map(|v| v.len())
            ),
        }
    }

    /// Files with > `MAX_NOTES` entries are truncated silently; the parse
    /// succeeds and returns exactly `MAX_NOTES` notes. This is by design —
    /// the memory-exhaustion protection sits at the post-parse step so the
    /// TOML itself can be arbitrarily shaped.
    #[test]
    fn test_parse_notes_str_truncates_at_max_notes() {
        // 10_500 notes is above the 10_000 cap but small enough that TOML
        // parsing stays fast. Each entry text is unique so blake3 IDs differ.
        let mut content = String::with_capacity(10_500 * 40);
        for i in 0..10_500 {
            content.push_str(&format!("[[note]]\ntext = \"note {}\"\n", i));
        }

        let notes = parse_notes_str(&content).expect("truncation must not error");
        assert_eq!(
            notes.len(),
            10_000,
            "parse_notes_str must truncate at MAX_NOTES=10_000, got {}",
            notes.len()
        );
        // First note preserved (the `take(MAX_NOTES)` keeps the prefix).
        assert_eq!(notes[0].text, "note 0");
        // Last note kept is the 10_000th (index 9999).
        assert_eq!(notes[9_999].text, "note 9999");
    }

    /// Exactly `MAX_NOTES` entries is the boundary case — no truncation,
    /// all notes preserved.
    #[test]
    fn test_parse_notes_str_exactly_max_notes() {
        let mut content = String::with_capacity(10_000 * 40);
        for i in 0..10_000 {
            content.push_str(&format!("[[note]]\ntext = \"n{}\"\n", i));
        }

        let notes = parse_notes_str(&content).unwrap();
        assert_eq!(notes.len(), 10_000, "at boundary, no truncation applied");
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

    // ===== TC-ADV-1.29-5: adversarial content in note entries =====
    //
    // `parse_notes_str` trusts the TOML parser to sanitize, then blake3-hashes
    // the raw text for stable IDs. These tests pin behaviour on three input
    // shapes that were previously untested:
    //
    // * a huge `mentions` array (10k+ entries) — memory-bound, should not
    //   panic or get silently truncated. Only the outer `MAX_NOTES` cap
    //   bounds the note count; per-note mentions have no explicit cap.
    // * an empty `text` field — the blake3 hash of "" is a fixed constant,
    //   so two empty-text notes collide. Parsing succeeds; collision is a
    //   known accepted trade-off documented at `parse_notes_str:322-324`.
    // * a NUL byte embedded in `text` — the TOML parser accepts NUL in a
    //   double-quoted string (encoded as ` `) and `parse_notes_str`
    //   preserves it verbatim. This pins the current behaviour so a future
    //   sanitation layer is deliberate.

    /// 10k-entry `mentions` array round-trips without panic or truncation.
    /// `MAX_NOTES = 10_000` caps the note count, not the per-note mentions,
    /// so a single note can carry an arbitrary-length `mentions` vector
    /// bounded only by the file-size guard.
    #[test]
    fn test_parse_notes_str_huge_mentions_array() {
        let mut mentions = String::with_capacity(10_000 * 16);
        mentions.push('[');
        for i in 0..10_000 {
            if i > 0 {
                mentions.push(',');
            }
            mentions.push_str(&format!("\"file{i}.rs\""));
        }
        mentions.push(']');

        let content = format!(
            "[[note]]\ntext = \"huge mentions\"\nmentions = {}\n",
            mentions
        );
        let notes = parse_notes_str(&content)
            .expect("parse_notes_str must accept a 10k-entry mentions array");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes[0].mentions.len(),
            10_000,
            "all mentions preserved — no silent truncation"
        );
        assert_eq!(notes[0].mentions[0], "file0.rs");
        assert_eq!(notes[0].mentions[9_999], "file9999.rs");
    }

    /// Empty `text` field parses cleanly. The hash-based ID is a fixed
    /// constant for the empty string, so multiple empty-text notes collide
    /// on a single ID — the 0.003% collision rate documented at `:322-324`
    /// is accepted for the higher-priority "stable across reordering"
    /// property. This test pins that empty text is NOT rejected.
    #[test]
    fn test_parse_notes_str_empty_text() {
        let content = "[[note]]\ntext = \"\"\n";
        let notes = parse_notes_str(content).expect("empty text must parse (not error)");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "", "empty text preserved as empty string");
        assert!(
            notes[0].id.starts_with("note:"),
            "ID should still be generated from blake3 hash of empty string"
        );
    }

    /// A NUL byte embedded in `text` (via TOML ` `) is preserved
    /// verbatim. AUDIT-FOLLOWUP (TC-ADV-1.29-5): if a future sanitation
    /// layer rejects or strips NUL bytes, this test should be updated to
    /// reflect the new contract.
    #[test]
    fn test_parse_notes_str_nul_byte_in_text_preserved_verbatim() {
        let content = "[[note]]\ntext = \"has \\u0000 nul\"\n";
        let notes =
            parse_notes_str(content).expect("NUL in text must not error at parse time today");
        assert_eq!(notes.len(), 1);
        assert!(
            notes[0].text.contains('\0'),
            "AUDIT-FOLLOWUP (TC-ADV-1.29-5): NUL byte preserved verbatim today — \
             got {:?}",
            notes[0].text
        );
        // The full text should retain its surrounding characters.
        assert!(notes[0].text.starts_with("has "));
        assert!(notes[0].text.ends_with(" nul"));
    }

    /// NUL-only text (`" "`) — boundary case for the preserve-verbatim
    /// contract. After `.trim()` on `text` the NUL stays (trim only strips
    /// ASCII whitespace). The ID derives from the unstripped raw text so
    /// two "NUL-only" notes collide — the audit documents no guard here.
    #[test]
    fn test_parse_notes_str_nul_only_text() {
        let content = "[[note]]\ntext = \"\\u0000\"\n";
        let notes = parse_notes_str(content).expect("NUL-only text should not error");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            notes[0].text, "\0",
            "NUL-only text survives trim — it is not ASCII whitespace"
        );
    }

    /// #1133: kind round-trips through TOML parse. Lower-cased, trimmed,
    /// with empty-string normalized to `None`.
    #[test]
    fn test_parse_notes_str_with_kind() {
        let toml = r#"
[[note]]
text = "Add caching layer for embeddings"
sentiment = 0
kind = "todo"

[[note]]
text = "BGE-large beats E5 on v3.v2"
sentiment = 1
kind = "Design-Decision"

[[note]]
text = "neutral observation"
sentiment = 0

[[note]]
text = "blank kind reads as none"
sentiment = 0
kind = ""
"#;
        let notes = parse_notes_str(toml).unwrap();
        assert_eq!(notes.len(), 4);
        assert_eq!(notes[0].kind.as_deref(), Some("todo"));
        // Mixed-case input is normalized to lowercase by `normalize_kind`.
        assert_eq!(notes[1].kind.as_deref(), Some("design-decision"));
        // Sentiment-only note: kind is None.
        assert_eq!(notes[2].kind, None);
        // Explicit empty kind is normalized to None — same as absent.
        assert_eq!(notes[3].kind, None);
    }

    /// #1133: `embedding_text()` prefix priority. Kind wins over
    /// sentiment when set; sentiment-based prefixes still apply when
    /// kind is None.
    #[test]
    fn test_embedding_text_prefix_priority() {
        // Kind set → kind prefix (capitalized).
        let n = Note {
            id: "x".into(),
            text: "Investigate dim mismatch".into(),
            sentiment: -1.0,
            mentions: vec![],
            kind: Some("known-bug".into()),
        };
        assert_eq!(
            n.embedding_text(),
            "Known-Bug: Investigate dim mismatch",
            "kind prefix wins over negative-sentiment Warning prefix"
        );

        // No kind, negative sentiment → "Warning:".
        let n = Note {
            id: "y".into(),
            text: "Index lock not released".into(),
            sentiment: -1.0,
            mentions: vec![],
            kind: None,
        };
        assert_eq!(n.embedding_text(), "Warning: Index lock not released");

        // No kind, positive sentiment → "Pattern:".
        let n = Note {
            id: "z".into(),
            text: "BGE-large is the production embedder".into(),
            sentiment: 1.0,
            mentions: vec![],
            kind: None,
        };
        assert_eq!(
            n.embedding_text(),
            "Pattern: BGE-large is the production embedder"
        );

        // Kind="todo" + neutral sentiment → "Todo:" (single-segment).
        let n = Note {
            id: "w".into(),
            text: "Review the eval fixture".into(),
            sentiment: 0.0,
            mentions: vec![],
            kind: Some("todo".into()),
        };
        assert_eq!(n.embedding_text(), "Todo: Review the eval fixture");
    }

    /// #1133: `capitalize_kind_for_prefix` handles single + multi-segment
    /// kebab-case strings. Pinning the formatting protects against a
    /// future refactor that passes raw lowercase to the embedder (the
    /// embedder treats `Design-Decision` as more meaningful English
    /// than `design-decision`).
    #[test]
    fn test_capitalize_kind_for_prefix() {
        assert_eq!(capitalize_kind_for_prefix("todo"), "Todo");
        assert_eq!(
            capitalize_kind_for_prefix("design-decision"),
            "Design-Decision"
        );
        assert_eq!(capitalize_kind_for_prefix("known-bug"), "Known-Bug");
        // Three-segment kebab-case stays consistent.
        assert_eq!(capitalize_kind_for_prefix("a-b-c"), "A-B-C");
        // Single empty segment falls through cleanly.
        assert_eq!(capitalize_kind_for_prefix(""), "");
    }
}
