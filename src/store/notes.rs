//! Note CRUD operations and search

use std::path::Path;

use sqlx::Row;

use super::helpers::{
    embedding_slice, embedding_to_bytes, NoteSearchResult, NoteSummary, StoreError,
};
use super::Store;
use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::note::Note;
use crate::search::cosine_similarity;

impl Store {
    /// Insert or update notes in batch
    pub fn upsert_notes_batch(
        &self,
        notes: &[(Note, Embedding)],
        source_file: &Path,
        file_mtime: i64,
    ) -> Result<usize, StoreError> {
        let source_str = source_file.to_string_lossy().to_string();
        tracing::debug!(
            source = %source_str,
            count = notes.len(),
            "upserting notes batch"
        );

        self.rt.block_on(async {
            let mut tx = self.pool.begin().await?;

            let now = chrono::Utc::now().to_rfc3339();
            for (note, embedding) in notes {
                let mentions_json = serde_json::to_string(&note.mentions).unwrap_or_default();

                sqlx::query(
                    "INSERT OR REPLACE INTO notes (id, text, sentiment, mentions, embedding, source_file, file_mtime, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .bind(&note.id)
                .bind(&note.text)
                .bind(note.sentiment)
                .bind(&mentions_json)
                .bind(embedding_to_bytes(embedding))
                .bind(&source_str)
                .bind(file_mtime)
                .bind(&now)
                .bind(&now)
                .execute(&mut *tx)
                .await?;

                if let Err(e) = sqlx::query("DELETE FROM notes_fts WHERE id = ?1")
                    .bind(&note.id)
                    .execute(&mut *tx)
                    .await
                {
                    tracing::warn!("Failed to delete from notes_fts: {}", e);
                }

                sqlx::query("INSERT INTO notes_fts (id, text) VALUES (?1, ?2)")
                    .bind(&note.id)
                    .bind(normalize_for_fts(&note.text))
                    .execute(&mut *tx)
                    .await?;
            }

            tx.commit().await?;
            Ok(notes.len())
        })
    }

    /// Search notes by embedding similarity
    pub fn search_notes(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<NoteSearchResult>, StoreError> {
        tracing::debug!(limit, threshold, "searching notes");

        self.rt.block_on(async {
            let rows: Vec<_> =
                sqlx::query("SELECT id, text, sentiment, mentions, embedding FROM notes")
                    .fetch_all(&self.pool)
                    .await?;

            let mut scored: Vec<(NoteSummary, f32)> = rows
                .into_iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let text: String = row.get(1);
                    let sentiment: f64 = row.get(2);
                    let mentions_json: String = row.get(3);
                    let embedding_bytes: Vec<u8> = row.get(4);

                    let mentions: Vec<String> =
                        serde_json::from_str(&mentions_json).unwrap_or_default();

                    let embedding = embedding_slice(&embedding_bytes)?;
                    let score = cosine_similarity(query.as_slice(), embedding);

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
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(limit);

            Ok(scored
                .into_iter()
                .map(|(note, score)| NoteSearchResult { note, score })
                .collect())
        })
    }

    /// Delete all notes from a source file
    pub fn delete_notes_by_file(&self, source_file: &Path) -> Result<u32, StoreError> {
        let source_str = source_file.to_string_lossy().to_string();

        self.rt.block_on(async {
            sqlx::query(
                "DELETE FROM notes_fts WHERE id IN (SELECT id FROM notes WHERE source_file = ?1)",
            )
            .bind(&source_str)
            .execute(&self.pool)
            .await?;

            let result = sqlx::query("DELETE FROM notes WHERE source_file = ?1")
                .bind(&source_str)
                .execute(&self.pool)
                .await?;

            Ok(result.rows_affected() as u32)
        })
    }

    /// Check if notes file needs reindexing
    pub fn notes_need_reindex(&self, source_file: &Path) -> Result<bool, StoreError> {
        let current_mtime = source_file
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| std::io::Error::other("time error"))?
            .as_secs() as i64;

        self.rt.block_on(async {
            let row: Option<(i64,)> =
                sqlx::query_as("SELECT file_mtime FROM notes WHERE source_file = ?1 LIMIT 1")
                    .bind(source_file.to_string_lossy().to_string())
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((mtime,)) if mtime >= current_mtime => Ok(false),
                _ => Ok(true),
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
    pub fn note_stats(&self) -> Result<(u64, u64, u64), StoreError> {
        self.rt.block_on(async {
            let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM notes")
                .fetch_one(&self.pool)
                .await?;

            // Thresholds match crate::note::SENTIMENT_*_THRESHOLD constants
            let (warnings,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM notes WHERE sentiment < -0.3")
                    .fetch_one(&self.pool)
                    .await?;

            let (patterns,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM notes WHERE sentiment > 0.3")
                    .fetch_one(&self.pool)
                    .await?;

            Ok((total as u64, warnings as u64, patterns as u64))
        })
    }
}
