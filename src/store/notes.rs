//! Note CRUD operations

use std::path::Path;

use sqlx::Row;

use super::helpers::{NoteStats, NoteSummary, StoreError};
use super::Store;
use crate::nl::normalize_for_fts;
use crate::note::Note;
use crate::note::{SENTIMENT_NEGATIVE_THRESHOLD, SENTIMENT_POSITIVE_THRESHOLD};

/// Insert a single note + FTS entry within an existing transaction.
async fn insert_note_with_fts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    note: &Note,
    source_str: &str,
    file_mtime: i64,
    now: &str,
) -> Result<(), StoreError> {
    let mentions_json = serde_json::to_string(&note.mentions)?;

    // Write empty blob for embedding column (SQ-9: note embeddings removed).
    // Column retained for SQLite compatibility (no DROP COLUMN in older versions).
    let empty_blob: &[u8] = &[];
    sqlx::query(
        "INSERT OR REPLACE INTO notes (id, text, sentiment, mentions, embedding, source_file, file_mtime, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&note.id)
    .bind(&note.text)
    .bind(note.sentiment)
    .bind(&mentions_json)
    .bind(empty_blob)
    .bind(source_str)
    .bind(file_mtime)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;

    // Delete from FTS before insert - error must fail transaction to prevent desync
    sqlx::query("DELETE FROM notes_fts WHERE id = ?1")
        .bind(&note.id)
        .execute(&mut **tx)
        .await?;

    sqlx::query("INSERT INTO notes_fts (id, text) VALUES (?1, ?2)")
        .bind(&note.id)
        .bind(normalize_for_fts(&note.text))
        .execute(&mut **tx)
        .await?;

    Ok(())
}

impl Store {
    /// Insert or update notes in batch
    pub fn upsert_notes_batch(
        &self,
        notes: &[Note],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("upsert_notes_batch", count = notes.len()).entered();
        let source_str = crate::normalize_path(source_file);
        tracing::debug!(
            source = %source_str,
            count = notes.len(),
            "upserting notes batch"
        );

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for note in notes {
                insert_note_with_fts(&mut tx, note, &source_str, file_mtime, &now).await?;
            }

            tx.commit().await?;
            self.invalidate_notes_cache();
            Ok(notes.len())
        })
    }

    /// Replace all notes for a source file in a single transaction.
    ///
    /// Atomically deletes existing notes and inserts new ones, preventing
    /// data loss if the process crashes mid-operation.
    pub fn replace_notes_for_file(
        &self,
        notes: &[Note],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let _span =
            tracing::info_span!("replace_notes_for_file", path = %source_file.display()).entered();
        let source_str = crate::normalize_path(source_file);
        tracing::debug!(
            source = %source_str,
            count = notes.len(),
            "replacing notes for file"
        );

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            // Step 1: Delete existing notes + FTS for this file
            sqlx::query(
                "DELETE FROM notes_fts WHERE id IN (SELECT id FROM notes WHERE source_file = ?1)",
            )
            .bind(&source_str)
            .execute(&mut *tx)
            .await?;

            sqlx::query("DELETE FROM notes WHERE source_file = ?1")
                .bind(&source_str)
                .execute(&mut *tx)
                .await?;

            // Step 2: Insert new notes + FTS
            let now = chrono::Utc::now().to_rfc3339();
            for note in notes {
                insert_note_with_fts(&mut tx, note, &source_str, file_mtime, &now).await?;
            }

            tx.commit().await?;
            self.invalidate_notes_cache();
            tracing::info!(source = %source_str, count = notes.len(), "Notes replaced successfully");
            Ok(notes.len())
        })
    }

    /// Check if notes file needs reindexing based on mtime.
    ///
    /// Returns `Ok(Some(mtime))` if reindex needed (with the file's current mtime),
    /// or `Ok(None)` if no reindex needed. This avoids reading file metadata twice.
    pub fn notes_need_reindex(&self, source_file: &Path) -> Result<Option<i64>, StoreError> {
        let _span =
            tracing::debug_span!("notes_need_reindex", path = %source_file.display()).entered();
        let current_mtime = source_file
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StoreError::SystemTime)?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(i64,)> =
                sqlx::query_as("SELECT file_mtime FROM notes WHERE source_file = ?1 LIMIT 1")
                    .bind(crate::normalize_path(source_file))
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((mtime,)) if mtime >= current_mtime => Ok(None),
                _ => Ok(Some(current_mtime)),
            }
        })
    }

    /// Get note count
    pub fn note_count(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("note_count").entered();
        self.rt.block_on(async {
            let row: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM notes")
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.map(|(c,)| c as u64).unwrap_or(0))
        })
    }

    /// Get note statistics (total, warnings, patterns).
    ///
    /// Uses `SENTIMENT_NEGATIVE_THRESHOLD` (-0.3) and `SENTIMENT_POSITIVE_THRESHOLD` (0.3)
    /// to classify notes. These thresholds work with discrete sentiment values
    /// (-1, -0.5, 0, 0.5, 1) -- negative values (-1, -0.5) count as warnings,
    /// positive values (0.5, 1) count as patterns.
    pub fn note_stats(&self) -> Result<NoteStats, StoreError> {
        let _span = tracing::debug_span!("note_stats").entered();
        self.rt.block_on(async {
            let (total, warnings, patterns): (i64, i64, i64) = sqlx::query_as(
                "SELECT COUNT(*),
                        SUM(CASE WHEN sentiment < ?1 THEN 1 ELSE 0 END),
                        SUM(CASE WHEN sentiment > ?2 THEN 1 ELSE 0 END)
                 FROM notes",
            )
            .bind(SENTIMENT_NEGATIVE_THRESHOLD)
            .bind(SENTIMENT_POSITIVE_THRESHOLD)
            .fetch_one(&self.pool)
            .await?;

            Ok(NoteStats {
                total: total as u64,
                warnings: warnings as u64,
                patterns: patterns as u64,
            })
        })
    }

    /// List all notes with metadata (no embeddings).
    ///
    /// Returns `NoteSummary` for each note, useful for mention-based filtering
    /// without the cost of loading embeddings.
    pub fn list_notes_summaries(&self) -> Result<Vec<NoteSummary>, StoreError> {
        let _span = tracing::debug_span!("list_notes_summaries").entered();
        self.rt.block_on(async {
            let rows: Vec<_> =
                sqlx::query("SELECT id, text, sentiment, mentions FROM notes ORDER BY created_at")
                    .fetch_all(&self.pool)
                    .await?;

            Ok(rows
                .into_iter()
                .map(|row| {
                    let id: String = row.get(0);
                    let text: String = row.get(1);
                    let sentiment: f64 = row.get(2);
                    let mentions_json: String = row.get(3);
                    let mentions: Vec<String> =
                        serde_json::from_str(&mentions_json).unwrap_or_else(|e| {
                            tracing::warn!(note_id = %id, error = %e, "Failed to deserialize note mentions");
                            Vec::new()
                        });
                    NoteSummary {
                        id,
                        text,
                        sentiment: sentiment as f32,
                        mentions,
                    }
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::note::{Note, SENTIMENT_NEGATIVE_THRESHOLD, SENTIMENT_POSITIVE_THRESHOLD};
    use crate::test_helpers::setup_store;
    use std::path::Path;

    fn make_note(id: &str, text: &str, sentiment: f32) -> Note {
        Note {
            id: id.to_string(),
            text: text.to_string(),
            sentiment,
            mentions: vec![],
        }
    }

    #[test]
    fn sentiment_thresholds_match_discrete_values() {
        // Discrete sentiment values: -1, -0.5, 0, 0.5, 1
        // Negative threshold must sit between -0.5 and 0 so that
        // -0.5 counts as a warning but 0 does not.
        assert!(SENTIMENT_NEGATIVE_THRESHOLD > -0.5);
        assert!(SENTIMENT_NEGATIVE_THRESHOLD < 0.0);
        // Positive threshold must sit between 0 and 0.5 so that
        // 0.5 counts as a pattern but 0 does not.
        assert!(SENTIMENT_POSITIVE_THRESHOLD > 0.0);
        assert!(SENTIMENT_POSITIVE_THRESHOLD < 0.5);
    }

    #[test]
    fn test_replace_notes_replaces_not_appends() {
        let (store, _dir) = setup_store();
        let source = Path::new("/tmp/notes.toml");

        // Insert 2 notes
        let notes = vec![
            make_note("n1", "first", 0.0),
            make_note("n2", "second", 0.0),
        ];
        store.upsert_notes_batch(&notes, source, 100).unwrap();
        assert_eq!(store.note_count().unwrap(), 2);

        // Replace with 1 note
        let replacement = vec![make_note("n3", "replacement", 0.0)];
        store
            .replace_notes_for_file(&replacement, source, 200)
            .unwrap();
        assert_eq!(store.note_count().unwrap(), 1);
    }

    #[test]
    fn test_replace_notes_with_empty_deletes() {
        let (store, _dir) = setup_store();
        let source = Path::new("/tmp/notes.toml");

        let notes = vec![
            make_note("n1", "first", 0.0),
            make_note("n2", "second", 0.5),
        ];
        store.upsert_notes_batch(&notes, source, 100).unwrap();
        assert_eq!(store.note_count().unwrap(), 2);

        // Replace with empty
        store.replace_notes_for_file(&[], source, 200).unwrap();
        assert_eq!(store.note_count().unwrap(), 0);
    }

    #[test]
    fn test_notes_need_reindex_stale() {
        let (store, dir) = setup_store();
        // Create a real temp file so metadata() works
        let notes_file = dir.path().join("notes.toml");
        std::fs::write(&notes_file, "# empty").unwrap();

        // Insert a note with an old mtime (0) so it's stale
        let notes = vec![make_note("n1", "old note", 0.0)];
        store.upsert_notes_batch(&notes, &notes_file, 0).unwrap();

        // Should return Some(current_mtime) because stored mtime (0) < file mtime
        let result = store.notes_need_reindex(&notes_file).unwrap();
        assert!(
            result.is_some(),
            "Should need reindex when stored mtime is old"
        );
    }

    #[test]
    fn test_notes_need_reindex_current() {
        let (store, dir) = setup_store();
        let notes_file = dir.path().join("notes.toml");
        std::fs::write(&notes_file, "# empty").unwrap();

        // Get the file's actual mtime
        let current_mtime = notes_file
            .metadata()
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Insert with the current mtime
        let notes = vec![make_note("n1", "current note", 0.0)];
        store
            .upsert_notes_batch(&notes, &notes_file, current_mtime)
            .unwrap();

        // Should return None — no reindex needed
        let result = store.notes_need_reindex(&notes_file).unwrap();
        assert!(
            result.is_none(),
            "Should not need reindex when mtime matches"
        );
    }

    #[test]
    fn test_note_count() {
        let (store, _dir) = setup_store();
        let source = Path::new("/tmp/notes.toml");

        assert_eq!(store.note_count().unwrap(), 0);

        let notes = vec![
            make_note("n1", "first", 0.0),
            make_note("n2", "second", -0.5),
            make_note("n3", "third", 1.0),
        ];
        store.upsert_notes_batch(&notes, source, 100).unwrap();
        assert_eq!(store.note_count().unwrap(), 3);
    }

    #[test]
    fn test_note_stats_sentiment() {
        let (store, _dir) = setup_store();
        let source = Path::new("/tmp/notes.toml");

        // -1 = warning, 0 = neutral, 0.5 = pattern
        let notes = vec![
            make_note("n1", "pain point", -1.0),
            make_note("n2", "neutral obs", 0.0),
            make_note("n3", "good pattern", 0.5),
        ];
        store.upsert_notes_batch(&notes, source, 100).unwrap();

        let stats = store.note_stats().unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.warnings, 1, "Only -1 should count as warning");
        assert_eq!(stats.patterns, 1, "Only 0.5 should count as pattern");
    }
}
