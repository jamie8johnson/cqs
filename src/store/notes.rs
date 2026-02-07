//! Note CRUD operations and search

use std::path::Path;

use sqlx::Row;

use super::helpers::{
    embedding_slice, embedding_to_bytes, NoteSearchResult, NoteStats, NoteSummary, StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::math::cosine_similarity;
use crate::nl::normalize_for_fts;
use crate::note::Note;
use crate::note::{SENTIMENT_NEGATIVE_THRESHOLD, SENTIMENT_POSITIVE_THRESHOLD};

/// Insert a single note + FTS entry within an existing transaction.
async fn insert_note_with_fts(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    note: &Note,
    embedding: &Embedding,
    source_str: &str,
    file_mtime: i64,
    now: &str,
) -> Result<(), StoreError> {
    let mentions_json = serde_json::to_string(&note.mentions)
        .map_err(|e| StoreError::Runtime(format!("JSON serialization: {e}")))?;

    sqlx::query(
        "INSERT OR REPLACE INTO notes (id, text, sentiment, mentions, embedding, source_file, file_mtime, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&note.id)
    .bind(&note.text)
    .bind(note.sentiment)
    .bind(&mentions_json)
    .bind(embedding_to_bytes(embedding)?)
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

/// Score a note row and return (NoteSummary, score) if it meets the threshold.
///
/// Shared scoring logic between brute-force search and ID-based search.
fn score_note_row(
    row: &sqlx::sqlite::SqliteRow,
    query: &Embedding,
    threshold: f32,
) -> Option<(NoteSummary, f32)> {
    let id: String = row.get(0);
    let text: String = row.get(1);
    let sentiment: f64 = row.get(2);
    let mentions_json: String = row.get(3);
    let embedding_bytes: Vec<u8> = row.get(4);

    let mentions: Vec<String> = serde_json::from_str(&mentions_json).unwrap_or_else(|e| {
        tracing::warn!(note_id = %id, error = %e, "Failed to deserialize note mentions, using empty list");
        Vec::new()
    });

    let embedding = embedding_slice(&embedding_bytes)?;
    let score = cosine_similarity(query.as_slice(), embedding)?;

    if score >= threshold {
        Some((
            NoteSummary {
                id,
                text,
                sentiment: sentiment as f32,
                mentions,
            },
            score,
        ))
    } else {
        None
    }
}

impl Store {
    /// Insert or update notes in batch
    pub fn upsert_notes_batch(
        &self,
        notes: &[(Note, Embedding)],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let source_str = source_file.to_string_lossy().into_owned();
        tracing::debug!(
            source = %source_str,
            count = notes.len(),
            "upserting notes batch"
        );

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for (note, embedding) in notes {
                insert_note_with_fts(&mut tx, note, embedding, &source_str, file_mtime, &now)
                    .await?;
            }

            tx.commit().await?;
            Ok(notes.len())
        })
    }

    /// Search notes by embedding similarity
    ///
    /// Note: This performs brute-force O(n) similarity search over all notes.
    /// For large note collections, prefer using the unified HNSW index which
    /// includes notes with `note:` prefix for efficient ANN search.
    ///
    /// The query is limited to MAX_NOTES_SCAN (1000) to prevent OOM on very
    /// large collections. If you have more notes, use the unified search.
    pub fn search_notes(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<NoteSearchResult>, StoreError> {
        // Limit scan to prevent OOM - notes in large collections should use HNSW
        const MAX_NOTES_SCAN: i64 = 1000;

        tracing::debug!(
            limit,
            threshold,
            max_scan = MAX_NOTES_SCAN,
            "searching notes"
        );

        self.rt.block_on(async {
            // Use LIMIT to avoid loading unbounded data
            let rows: Vec<_> =
                sqlx::query("SELECT id, text, sentiment, mentions, embedding FROM notes LIMIT ?1")
                    .bind(MAX_NOTES_SCAN)
                    .fetch_all(&self.pool)
                    .await?;

            let scanned = rows.len();
            let mut scored: Vec<(NoteSummary, f32)> = rows
                .iter()
                .filter_map(|row| score_note_row(row, query, threshold))
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);

            if scanned == MAX_NOTES_SCAN as usize {
                tracing::warn!(
                    "Note search limit reached ({}). Consider using unified HNSW search.",
                    MAX_NOTES_SCAN
                );
            }

            Ok(scored
                .into_iter()
                .map(|(note, score)| NoteSearchResult { note, score })
                .collect())
        })
    }

    /// Replace all notes for a source file in a single transaction.
    ///
    /// Atomically deletes existing notes and inserts new ones, preventing
    /// data loss if the process crashes mid-operation.
    pub fn replace_notes_for_file(
        &self,
        notes: &[(Note, Embedding)],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let source_str = source_file.to_string_lossy().into_owned();
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
            for (note, embedding) in notes {
                insert_note_with_fts(&mut tx, note, embedding, &source_str, file_mtime, &now)
                    .await?;
            }

            tx.commit().await?;
            Ok(notes.len())
        })
    }

    /// Check if notes file needs reindexing based on mtime.
    ///
    /// Returns `Ok(Some(mtime))` if reindex needed (with the file's current mtime),
    /// or `Ok(None)` if no reindex needed. This avoids reading file metadata twice.
    pub fn notes_need_reindex(&self, source_file: &Path) -> Result<Option<i64>, StoreError> {
        let current_mtime = source_file
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StoreError::SystemTime)?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(i64,)> =
                sqlx::query_as("SELECT file_mtime FROM notes WHERE source_file = ?1 LIMIT 1")
                    .bind(source_file.to_string_lossy().into_owned())
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
        self.rt.block_on(async {
            let row: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM notes")
                .fetch_optional(&self.pool)
                .await?;
            Ok(row.map(|(c,)| c as u64).unwrap_or(0))
        })
    }

    /// Get note statistics (total, warnings, patterns)
    pub fn note_stats(&self) -> Result<NoteStats, StoreError> {
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

    /// Get all note embeddings for HNSW index building.
    ///
    /// Returns (id, embedding) pairs with `note:` prefix on IDs to distinguish from chunks.
    pub fn note_embeddings(&self) -> Result<Vec<(String, Embedding)>, StoreError> {
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, embedding FROM notes")
                .fetch_all(&self.pool)
                .await?;

            let results: Vec<(String, Embedding)> = rows
                .into_iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    super::helpers::bytes_to_embedding(&bytes)
                        .map(|emb| (format!("note:{}", id), Embedding::new(emb)))
                })
                .collect();

            Ok(results)
        })
    }
}
