// DS-5: WRITE_LOCK guard is held across .await inside block_on().
// This is safe — block_on runs single-threaded, no concurrent tasks can deadlock.
#![allow(clippy::await_holding_lock)]
//! Chunk upsert, metadata, delete, and summary operations.

use std::path::Path;

use crate::embedder::Embedding;
use crate::parser::Chunk;
use crate::store::helpers::{embedding_to_bytes, StoreError};
use crate::store::{ReadWrite, Store};

use super::async_helpers::{batch_insert_chunks, snapshot_content_hashes, upsert_fts_conditional};

impl<Mode> Store<Mode> {
    /// Retrieve a single metadata value by key.
    ///
    /// Returns `Ok(value)` if the key exists, or `Err` if not found or on DB error.
    /// Used for lightweight metadata checks (e.g., model compatibility between stores).
    pub fn get_metadata(&self, key: &str) -> Result<String, StoreError> {
        let _span = tracing::debug_span!("get_metadata", key = %key).entered();
        self.rt.block_on(async {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT value FROM metadata WHERE key = ?1")
                    .bind(key)
                    .fetch_optional(&self.pool)
                    .await?;
            row.map(|(v,)| v)
                .ok_or_else(|| StoreError::NotFound(format!("metadata key '{}'", key)))
        })
    }

    /// Get enrichment hashes for a batch of chunk IDs.
    ///
    /// Returns a map from chunk_id to enrichment_hash (only for chunks that have one).
    pub fn get_enrichment_hashes_batch(
        &self,
        chunk_ids: &[&str],
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span =
            tracing::debug_span!("get_enrichment_hashes_batch", count = chunk_ids.len()).entered();
        if chunk_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            use crate::store::helpers::sql::max_rows_per_statement;
            for batch in chunk_ids.chunks(max_rows_per_statement(1)) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, enrichment_hash FROM chunks WHERE id IN ({}) AND enrichment_hash IS NOT NULL",
                    placeholders
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for id in batch {
                    query = query.bind(*id);
                }
                let rows = query.fetch_all(&self.pool).await?;
                for (id, hash) in rows {
                    result.insert(id, hash);
                }
            }
            Ok(result)
        })
    }

    /// Fetch all enrichment hashes in a single query.
    ///
    /// Returns a map from chunk_id to enrichment_hash for all chunks that have one.
    /// Used by the enrichment pass to avoid per-page hash fetches (PERF-29).
    pub fn get_all_enrichment_hashes(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!("get_all_enrichment_hashes").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT id, enrichment_hash FROM chunks WHERE enrichment_hash IS NOT NULL",
            )
            .fetch_all(&self.pool)
            .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Get LLM summaries for a batch of content hashes.
    ///
    /// Returns a map from content_hash to summary text. Only includes hashes
    /// that have summaries in the llm_summaries table matching the given purpose.
    pub fn get_summaries_by_hashes(
        &self,
        content_hashes: &[&str],
        purpose: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!(
            "get_summaries_by_hashes",
            count = content_hashes.len(),
            purpose
        )
        .entered();
        if content_hashes.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        self.rt.block_on(async {
            let mut result = std::collections::HashMap::new();
            use crate::store::helpers::sql::max_rows_per_statement;
            // Reserve one param for the purpose bind, so (limit - 1) per batch
            for batch in content_hashes.chunks(max_rows_per_statement(1) - 1) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT content_hash, summary FROM llm_summaries WHERE content_hash IN ({}) AND purpose = ?{}",
                    placeholders,
                    batch.len() + 1
                );
                let mut query = sqlx::query_as::<_, (String, String)>(&sql);
                for hash in batch {
                    query = query.bind(*hash);
                }
                query = query.bind(purpose);
                let rows = query.fetch_all(&self.pool).await?;
                for (hash, summary) in rows {
                    result.insert(hash, summary);
                }
            }
            Ok(result)
        })
    }

    /// Fetch all LLM summaries as a map from content_hash to summary text.
    ///
    /// Single query, no batching needed (reads entire table). Used by the
    /// enrichment pass to avoid per-page summary fetches.
    pub fn get_all_summaries(
        &self,
        purpose: &str,
    ) -> Result<std::collections::HashMap<String, String>, StoreError> {
        let _span = tracing::debug_span!("get_all_summaries", purpose).entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT content_hash, summary FROM llm_summaries WHERE purpose = ?1",
            )
            .bind(purpose)
            .fetch_all(&self.pool)
            .await?;
            Ok(rows.into_iter().collect())
        })
    }

    /// Get all distinct content hashes currently in the chunks table.
    /// Used to validate batch results against the current index (DS-20).
    pub fn get_all_content_hashes(&self) -> Result<Vec<String>, StoreError> {
        let _span = tracing::debug_span!("get_all_content_hashes").entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT content_hash FROM chunks")
                .fetch_all(&self.pool)
                .await?;
            Ok(rows.into_iter().map(|(h,)| h).collect())
        })
    }

    /// Get all summaries with full metadata for backup/restore.
    /// Returns Vec of (content_hash, summary, model, purpose).
    pub fn get_all_summaries_full(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, StoreError> {
        let _span = tracing::debug_span!("get_all_summaries_full").entered();
        self.rt.block_on(async {
            let rows: Vec<(String, String, String, String)> =
                sqlx::query_as("SELECT content_hash, summary, model, purpose FROM llm_summaries")
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows)
        })
    }

    /// Check if a file needs reindexing based on mtime.
    ///
    /// Returns `Ok(Some(mtime))` if reindex needed (with the file's current mtime),
    /// or `Ok(None)` if no reindex needed. This avoids reading file metadata twice.
    pub fn needs_reindex(&self, path: &Path) -> Result<Option<i64>, StoreError> {
        let _span = tracing::debug_span!("needs_reindex", path = %path.display()).entered();
        let current_mtime = crate::duration_to_mtime_millis(
            path.metadata()?
                .modified()?
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| StoreError::SystemTime)?,
        );

        self.rt.block_on(async {
            let row: Option<(Option<i64>,)> =
                sqlx::query_as("SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1")
                    .bind(crate::normalize_path(path))
                    .fetch_optional(&self.pool)
                    .await?;

            match row {
                Some((Some(stored_mtime),)) if stored_mtime >= current_mtime => Ok(None),
                _ => Ok(Some(current_mtime)),
            }
        })
    }
}

// Write methods live on `impl Store<ReadWrite>` — the compiler refuses to
// call them on a `Store<ReadOnly>`. Closes the bug class in GitHub #946.
impl Store<ReadWrite> {
    /// Insert or update chunks in batch using multi-row INSERT.
    ///
    /// Batch size is set by `max_rows_per_statement(22)` in `batch_insert_chunks`
    /// (22 binds per row against the SQLite 32766-variable limit, roughly
    /// 1488 rows per statement). FTS operations remain per-row because FTS5
    /// doesn't support upsert.
    ///
    /// **DS-V1.33-10 / #1342 — actual cascade contract:**
    ///
    /// Pre-#1342 doc comment claimed `INSERT OR REPLACE`, which would trigger
    /// `ON DELETE CASCADE` on `calls` / `type_edges` and require callers to
    /// re-populate. The code was migrated to `INSERT … ON CONFLICT(id) DO
    /// UPDATE SET …` (upsert) some time ago — the row is updated *in place*,
    /// no `DELETE` fires, and `calls` / `type_edges` rows are preserved as-is.
    ///
    /// That preservation is *not* equivalent to the cascade: when a chunk's
    /// `content_hash` changes, its outgoing calls / type uses likely change
    /// too, and the old rows now reference a stale call graph. Callers
    /// **must still** re-populate `calls` and `type_edges` for any chunk
    /// whose content changed (compare returned `content_hash` to the
    /// pre-existing snapshot from `snapshot_content_hashes`). The
    /// pre-existing rows aren't *wrong* in the same way they would be after
    /// a cascade — they're just stale until the caller refreshes.
    ///
    /// `enrichment_hash` and `enrichment_version` columns *are* preserved
    /// across upsert so the enrichment pass doesn't get its work invalidated
    /// by every reindex (DS-2).
    pub fn upsert_chunks_batch(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("upsert_chunks_batch", count = chunks.len()).entered();

        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        // v24 / #1221: compute the vendored bit per chunk from the
        // store's configured vendored-path prefixes. Empty prefix list
        // → all-false (the pre-#1221 default and what unwired callers
        // see). Origin path is normalised to forward-slash form via
        // `crate::normalize_path` to match `is_vendored_origin`'s
        // path-segment matcher.
        let prefixes = self.vendored_prefixes_slice();
        let vendored_per_chunk: Vec<bool> = chunks
            .iter()
            .map(|(chunk, _)| {
                let origin = crate::normalize_path(&chunk.file);
                crate::vendored::is_vendored_origin(&origin, prefixes)
            })
            .collect();

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(
                &mut tx,
                chunks,
                &embedding_bytes,
                &vendored_per_chunk,
                source_mtime,
                &now,
            )
            .await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;
            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Insert or update a single chunk
    pub fn upsert_chunk(
        &self,
        chunk: &Chunk,
        embedding: &Embedding,
        source_mtime: Option<i64>,
    ) -> Result<(), StoreError> {
        let _span = tracing::info_span!("upsert_chunk", name = %chunk.name).entered();
        self.upsert_chunks_batch(&[(chunk.clone(), embedding.clone())], source_mtime)?;
        Ok(())
    }

    /// Update only the embedding for existing chunks by chunk ID.
    ///
    /// `updates` is a slice of `(chunk_id, embedding)` pairs. Chunk IDs not
    /// found in the store are logged and skipped (rows_affected == 0).
    /// Returns the count of actually updated rows.
    ///
    /// Update embeddings in batch (without changing enrichment hashes).
    ///
    /// Convenience wrapper around `update_embeddings_with_hashes_batch` that
    /// passes `None` for the enrichment hash, leaving it unchanged.
    pub fn update_embeddings_batch(
        &self,
        updates: &[(String, Embedding)],
    ) -> Result<usize, StoreError> {
        if updates.is_empty() {
            tracing::debug!("update_embeddings_batch called with empty batch, skipping");
            return Ok(0);
        }
        let with_none: Vec<(String, Embedding, Option<String>)> = updates
            .iter()
            .map(|(id, emb)| (id.clone(), emb.clone(), None))
            .collect();
        self.update_embeddings_with_hashes_batch(&with_none)
    }

    /// Update embeddings and optionally enrichment hashes in batch.
    ///
    /// When the hash is `Some`, stores the enrichment hash for idempotency
    /// detection. When `None`, leaves the existing enrichment hash unchanged.
    /// Used by the enrichment pass to record which call context was used,
    /// so re-indexing can skip unchanged chunks.
    pub fn update_embeddings_with_hashes_batch(
        &self,
        updates: &[(String, Embedding, Option<String>)],
    ) -> Result<usize, StoreError> {
        let _span =
            tracing::info_span!("update_embeddings_with_hashes_batch", count = updates.len())
                .entered();
        if updates.is_empty() {
            return Ok(0);
        }

        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = updates
            .iter()
            .map(|(_, emb, _)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        // PERF-40: Temp table + single UPDATE...FROM instead of N individual UPDATEs.
        // Reduces ~10K round-trips to ~100 batch INSERTs + 1 UPDATE.
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // 1. Create temp table for batch staging
            sqlx::query(
                "CREATE TEMP TABLE IF NOT EXISTS _update_embeddings \
                 (id TEXT PRIMARY KEY, embedding BLOB NOT NULL, enrichment_hash TEXT)",
            )
            .execute(&mut *tx)
            .await?;

            sqlx::query("DELETE FROM _update_embeddings")
                .execute(&mut *tx)
                .await?;

            // 2. Batch INSERT into temp table. PF-V1.25-9: previously
            // `BATCH_SIZE = 100` (100 × 3 = 300 binds), sized for the
            // pre-3.32 SQLite 999-variable limit. Modern SQLite permits
            // 32766; `max_rows_per_statement(3)` derives ~10822 rows per
            // statement. On a full reindex with 50k updated embeddings
            // that's ~5 INSERTs instead of 500 — a 100× reduction in
            // SQL round-trips.
            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(3);
            for batch_start in (0..updates.len()).step_by(BATCH_SIZE) {
                let batch_end = (batch_start + BATCH_SIZE).min(updates.len());
                let batch = &updates[batch_start..batch_end];
                let batch_bytes = &embedding_bytes[batch_start..batch_end];

                let mut placeholders = Vec::with_capacity(batch.len());
                for i in 0..batch.len() {
                    let base = i * 3;
                    placeholders.push(format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3));
                }
                let sql = format!(
                    "INSERT INTO _update_embeddings (id, embedding, enrichment_hash) VALUES {}",
                    placeholders.join(", ")
                );
                let mut query = sqlx::query(&sql);
                for (i, (id, _, hash)) in batch.iter().enumerate() {
                    query = query.bind(id);
                    query = query.bind(&batch_bytes[i]);
                    query = query.bind(hash.as_deref());
                }
                query.execute(&mut *tx).await?;
            }

            // 3. Single UPDATE...FROM join (SQLite 3.33+)
            let result = sqlx::query(
                "UPDATE chunks SET \
                    embedding = t.embedding, \
                    enrichment_hash = COALESCE(t.enrichment_hash, chunks.enrichment_hash) \
                 FROM _update_embeddings t \
                 WHERE chunks.id = t.id",
            )
            .execute(&mut *tx)
            .await?;
            let updated = result.rows_affected() as usize;

            if updated < updates.len() {
                let missing = updates.len() - updated;
                tracing::debug!(missing, "Enrichment update: some chunk IDs not found");
            }

            // 4. Drop temp table
            sqlx::query("DROP TABLE IF EXISTS _update_embeddings")
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(updated)
        })
    }

    /// Write UMAP 2D coordinates back to chunk rows in batch.
    ///
    /// Used by the `cqs index --umap` pass after `scripts/run_umap.py`
    /// projects the dense embeddings into 2D. Same temp-table + UPDATE...FROM
    /// pattern as `update_embeddings_with_hashes_batch`.
    ///
    /// Returns the number of rows actually updated. IDs that don't exist in
    /// `chunks` are silently skipped (the projection script may have been
    /// fed a stale embedding dump after a delete).
    pub fn update_umap_coords_batch(
        &self,
        coords: &[(String, f64, f64)],
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("update_umap_coords_batch", count = coords.len()).entered();
        if coords.is_empty() {
            return Ok(0);
        }

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // P3.40: TEMP TABLE is connection-scoped, not transaction-scoped.
            // A prior call on the same pooled connection (or a rollback path
            // that didn't reach the trailing DROP) can leave a stale
            // `_update_umap` with the wrong row count. DROP first, then
            // CREATE without IF NOT EXISTS so we always start from an empty
            // table — no DELETE pre-clear needed.
            sqlx::query("DROP TABLE IF EXISTS _update_umap")
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "CREATE TEMP TABLE _update_umap \
                 (id TEXT PRIMARY KEY, umap_x REAL NOT NULL, umap_y REAL NOT NULL)",
            )
            .execute(&mut *tx)
            .await?;

            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(3);
            for batch_start in (0..coords.len()).step_by(BATCH_SIZE) {
                let batch_end = (batch_start + BATCH_SIZE).min(coords.len());
                let batch = &coords[batch_start..batch_end];

                let mut placeholders = Vec::with_capacity(batch.len());
                for i in 0..batch.len() {
                    let base = i * 3;
                    placeholders.push(format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3));
                }
                let sql = format!(
                    "INSERT INTO _update_umap (id, umap_x, umap_y) VALUES {}",
                    placeholders.join(", ")
                );
                let mut query = sqlx::query(&sql);
                for (id, x, y) in batch {
                    query = query.bind(id).bind(*x).bind(*y);
                }
                query.execute(&mut *tx).await?;
            }

            let result = sqlx::query(
                "UPDATE chunks SET umap_x = t.umap_x, umap_y = t.umap_y \
                 FROM _update_umap t WHERE chunks.id = t.id",
            )
            .execute(&mut *tx)
            .await?;
            let updated = result.rows_affected() as usize;

            sqlx::query("DROP TABLE IF EXISTS _update_umap")
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;

            if updated < coords.len() {
                let missing = coords.len() - updated;
                tracing::warn!(
                    missing,
                    total = coords.len(),
                    "UMAP update: some chunk IDs no longer exist (deleted between projection and write)"
                );
            }
            Ok(updated)
        })
    }

    /// Insert or update LLM summaries in batch.
    ///
    /// Each entry is (content_hash, summary, model, purpose).
    pub fn upsert_summaries_batch(
        &self,
        summaries: &[(String, String, String, String)],
    ) -> Result<usize, StoreError> {
        let _span =
            tracing::debug_span!("upsert_summaries_batch", count = summaries.len()).entered();
        if summaries.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now().to_rfc3339();
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            use crate::store::helpers::sql::max_rows_per_statement;
            const BATCH_SIZE: usize = max_rows_per_statement(5);
            for batch in summaries.chunks(BATCH_SIZE) {
                // DS-V1.36-9 / P3: ON CONFLICT DO UPDATE instead of INSERT OR
                // REPLACE so the upsert is a true UPDATE on conflict and
                // never fires the implicit DELETE that INSERT OR REPLACE
                // emits. Matches PR #1342's chunks-table fix. Today there's
                // no FK to chunks, but a future ON DELETE CASCADE addition
                // would otherwise turn every summary refresh into a v20
                // splade-trigger fire (full SPLADE invalidation).
                let mut qb: sqlx::QueryBuilder<sqlx::Sqlite> = sqlx::QueryBuilder::new(
                    "INSERT INTO llm_summaries (content_hash, summary, model, purpose, created_at)",
                );
                qb.push_values(batch.iter(), |mut b, (hash, summary, model, purpose)| {
                    b.push_bind(hash)
                        .push_bind(summary)
                        .push_bind(model)
                        .push_bind(purpose)
                        .push_bind(&now);
                });
                qb.push(
                    " ON CONFLICT(content_hash, purpose) DO UPDATE SET \
                     summary = excluded.summary, \
                     model = excluded.model, \
                     created_at = excluded.created_at",
                );
                qb.build().execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(summaries.len())
        })
    }

    /// Enqueue one streamed `llm_summaries` row into the per-Store
    /// write-coalescing queue (#1126 / P2.60).
    ///
    /// The queue holds rows in-memory until either the row threshold or
    /// the time interval is crossed, at which point a synchronous flush
    /// drains every queued row inside one `begin_write()` transaction —
    /// restoring the invariant that all `index.db` writes serialize
    /// through `WRITE_LOCK`.
    ///
    /// Pre-#1126, the streaming callback executed `INSERT OR IGNORE`
    /// directly against `&pool`, bypassing `WRITE_LOCK` and racing
    /// concurrent reindex transactions for SQLite's exclusive lock with
    /// 1 fsync per row. The queue restores both correctness (no race)
    /// and throughput (one fsync per batch).
    #[cfg(feature = "llm-summaries")]
    pub fn queue_summary_write(&self, custom_id: &str, text: &str, model: &str, purpose: &str) {
        // #1170: validate prose summaries before they reach the cache. The
        // doc-comment purpose is intentionally exempt — its prompt asks for
        // imperative reference docs which trip the heuristics on legitimate
        // content. Doc-comment write-back has its own review gate (#1166).
        let validated_text = if purpose == "summary" {
            use crate::llm::validation::{
                validate_summary, SummaryValidationMode, ValidationOutcome,
            };
            let mode = SummaryValidationMode::from_env();
            match validate_summary(text, mode) {
                ValidationOutcome::Accept(t) => t,
                ValidationOutcome::Reject { pattern } => {
                    tracing::warn!(
                        custom_id = %custom_id,
                        pattern = %pattern,
                        "Dropping summary that matched injection pattern in strict mode"
                    );
                    return;
                }
            }
        } else {
            text.to_string()
        };

        self.summary_queue
            .push(crate::store::summary_queue::PendingSummary {
                custom_id: custom_id.to_string(),
                text: validated_text,
                model: model.to_string(),
                purpose: purpose.to_string(),
            });
    }

    /// Drain the queue (if any) under one `begin_write()` tx.
    ///
    /// Idempotent on an empty queue. Callers — every LLM pass and
    /// `cmd_index` — invoke this unconditionally at safe points
    /// (start, success, error, signal-interrupted exit) so a transient
    /// flush failure during streaming retries on the next call. The
    /// existing `fetch_batch_results` re-persist contract guarantees no
    /// row is permanently lost if a flush misses; the next run re-fetches
    /// it from the upstream batch.
    #[cfg(feature = "llm-summaries")]
    pub fn flush_pending_summaries(&self) -> Result<usize, StoreError> {
        let _span = tracing::info_span!("flush_pending_summaries").entered();
        self.summary_queue.flush()
    }

    /// Build a streaming per-item persist callback for the local LLM provider.
    ///
    /// Returns a `Box<dyn Fn(&str, &str) + Send + Sync>` that can be handed to
    /// [`crate::llm::create_client`] as its `on_item` arg, or to
    /// [`crate::llm::local::LocalProvider::with_on_item_complete`] for
    /// direct test-time construction. Each invocation
    /// `cb(custom_id, text)` enqueues one row into the per-Store
    /// `summary_queue`. The queue drains under [`Store::begin_write`] when
    /// either of its thresholds is crossed (rows ≥ N OR elapsed ≥ T), or
    /// when callers call [`Store::flush_pending_summaries`] explicitly at
    /// the end of an LLM pass / inside `cmd_index`.
    ///
    /// The callback captures `Arc`-cloned handles to the queue, model, and
    /// purpose so it can outlive any `&Store` reference on the caller's
    /// stack. Enqueue is in-memory and infallible; the conditional flush
    /// at threshold can fail (e.g. SQLITE_BUSY) — those errors are logged
    /// and swallowed because `flush_pending_summaries` is idempotent and
    /// the LLM-pass final flush will retry.
    ///
    /// #1126 / P2.60: this path now goes through `WRITE_LOCK` via the
    /// queue's flush. Concurrent reindex no longer collides — both sides
    /// serialize through the same in-process mutex.
    #[cfg(feature = "llm-summaries")]
    pub fn stream_summary_writer(
        &self,
        model: String,
        purpose: String,
    ) -> crate::llm::provider::OnItemCallback {
        use std::sync::Arc;
        let queue = Arc::clone(&self.summary_queue);
        Box::new(move |custom_id: &str, text: &str| {
            queue.push(crate::store::summary_queue::PendingSummary {
                custom_id: custom_id.to_string(),
                text: text.to_string(),
                model: model.clone(),
                purpose: purpose.clone(),
            });
        })
    }

    /// Delete orphan LLM summaries whose content_hash doesn't exist in any chunk.
    pub fn prune_orphan_summaries(&self) -> Result<usize, StoreError> {
        let _span = tracing::debug_span!("prune_orphan_summaries").entered();
        self.rt.block_on(async {
            let result = sqlx::query(
                "DELETE FROM llm_summaries WHERE content_hash NOT IN \
                 (SELECT DISTINCT content_hash FROM chunks)",
            )
            .execute(&self.pool)
            .await?;
            Ok(result.rows_affected() as usize)
        })
    }

    /// Delete all chunks for an origin (file path or source identifier)
    pub fn delete_by_origin(&self, origin: &Path) -> Result<u32, StoreError> {
        let _span = tracing::info_span!("delete_by_origin", origin = %origin.display()).entered();
        let origin_str = crate::normalize_path(origin);

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            sqlx::query(
                "DELETE FROM chunks_fts WHERE id IN (SELECT id FROM chunks WHERE origin = ?1)",
            )
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;

            let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // E.2 (P1 #17): `function_calls` has no FK to `chunks` (it stores
            // `caller_name` strings, not chunk IDs), so deleting chunks does
            // not cascade. Without this DELETE, every incremental delete path
            // leaves orphan call-graph rows that surface as ghost callers in
            // `cqs callers`/`callees`/`dead`.
            sqlx::query("DELETE FROM function_calls WHERE file = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            tx.commit().await?;
            Ok(result.rows_affected() as u32)
        })
    }

    /// Refresh `source_mtime` on every chunk for `origin` without touching
    /// content.
    ///
    /// EH-V1.30.1-1: when the watch loop's `parse_file_all_with_chunk_calls`
    /// fails (syntax error in the user's code), the watch path emits an empty
    /// chunk vector for that file. The previous chunks stay as ghosts AND
    /// `chunks.source_mtime` is never refreshed, so `run_daemon_reconcile`
    /// keeps classifying the file MODIFIED on every tick (default 30 s) —
    /// unbounded reindex-fail-warn loop until the user fixes the syntax.
    ///
    /// This helper lets the parse-failure path bump stored mtime so reconcile
    /// sees `disk == stored` and stops re-queuing the file. The chunks are
    /// intentionally left as-is — they may still serve from the index until
    /// the next successful re-parse, but that's strictly better than ghost
    /// chunks plus a hot reindex loop.
    ///
    /// Returns the number of chunk rows whose `source_mtime` was updated.
    /// Callers can log a warn if `rows_affected == 0` (origin format mismatch
    /// would be the most likely cause), but the typical case is `rows_affected
    /// > 0` matching the chunk count for that file.
    pub fn touch_source_mtime(&self, origin: &Path, mtime_ms: i64) -> Result<u32, StoreError> {
        let _span =
            tracing::debug_span!("touch_source_mtime", origin = %origin.display(), mtime_ms)
                .entered();
        // CRITICAL: the indexer keys chunks by `crate::normalize_path(origin)`
        // — see `delete_by_origin` above for the canonical pattern. Without
        // this normalization Windows `\\` vs Unix `/` separator drift makes
        // the UPDATE silently affect zero rows, defeating the entire fix.
        let origin_str = crate::normalize_path(origin);

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let result = sqlx::query("UPDATE chunks SET source_mtime = ?1 WHERE origin = ?2")
                .bind(mtime_ms)
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(result.rows_affected() as u32)
        })
    }

    /// Refresh the full reconcile fingerprint (`source_mtime`, `source_size`,
    /// `source_content_hash`) on every chunk for `origin`.
    ///
    /// Issue #1219 / EX-V1.30.1-6: schema v23 added the `source_size` and
    /// `source_content_hash` columns so Layer 2 reconciliation
    /// (`run_daemon_reconcile`) can fall back to BLAKE3 when mtime/size alone
    /// is unreliable (coarse-mtime FAT32/NTFS/HFS+/SMB; `git checkout` and
    /// formatter passes that bump mtime without changing content). Both
    /// production write paths (`cli/pipeline/upsert.rs` and
    /// `cli/watch/reindex.rs`) call this helper after their chunk upsert so
    /// the next reconcile pass sees a populated fingerprint.
    ///
    /// `None` fields stay NULL; callers that can't read disk pass a
    /// fingerprint with all three set to `None` and get the legacy
    /// mtime-only behavior. `read_disk` always populates mtime+size; only
    /// the hash is conditional on policy.
    ///
    /// Returns the number of chunk rows updated for telemetry; `0` typically
    /// means the origin path didn't match the canonicalized form stored in
    /// the chunks table (Windows `\\` vs Unix `/` drift) — same diagnostic
    /// shape as `touch_source_mtime`.
    pub fn set_file_fingerprint(
        &self,
        origin: &Path,
        fp: &crate::store::chunks::staleness::FileFingerprint,
    ) -> Result<u32, StoreError> {
        let _span =
            tracing::debug_span!("set_file_fingerprint", origin = %origin.display()).entered();
        let origin_str = crate::normalize_path(origin);
        let size_i64: Option<i64> = fp.size.and_then(|s| i64::try_from(s).ok());
        let hash_blob: Option<Vec<u8>> = fp.content_hash.map(|h| h.to_vec());

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let result = sqlx::query(
                "UPDATE chunks \
                 SET source_mtime = ?1, source_size = ?2, source_content_hash = ?3 \
                 WHERE origin = ?4",
            )
            .bind(fp.mtime)
            .bind(size_i64)
            .bind(hash_blob)
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(result.rows_affected() as u32)
        })
    }

    /// Atomically upsert chunks and their call graph in a single transaction.
    ///
    /// Combines chunk upsert (with FTS) and call graph upsert into one transaction,
    /// preventing inconsistency from crashes between separate operations.
    /// Chunks are inserted in batches of 52 rows (52 * 19 = 988 < SQLite's 999 limit).
    ///
    /// Convenience wrapper around [`Self::upsert_chunks_calls_and_prune`] without
    /// phantom-chunk pruning. Callers that know the full live-id set for a file
    /// (e.g. the watch loop) should call `upsert_chunks_calls_and_prune`
    /// directly so the upsert + phantom delete share one transaction.
    pub fn upsert_chunks_and_calls(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
    ) -> Result<usize, StoreError> {
        self.upsert_chunks_calls_and_prune(chunks, source_mtime, calls, None, &[])
    }

    /// Atomically upsert chunks + calls AND prune phantom chunks for a file,
    /// all inside a single `begin_write()` transaction.
    ///
    /// DS2-4: Before this method, `reindex_files` in the watch loop called
    /// `upsert_chunks_and_calls` and then `delete_phantom_chunks` in two
    /// independent transactions. A crash between the two left the index in a
    /// half-pruned state — new chunks were visible but removed chunks were
    /// still there, alongside a dirty HNSW flag. Merging both operations into
    /// one tx makes the reindex all-or-nothing.
    ///
    /// When `prune_file` is `None`, behaves identically to the old
    /// `upsert_chunks_and_calls` (phantom pruning is skipped). When
    /// `prune_file` is `Some(path)`, chunks matching that `origin` whose IDs
    /// are not present in `live_ids` are deleted alongside the upsert.
    ///
    /// An empty `live_ids` combined with `Some(prune_file)` intentionally
    /// matches `delete_phantom_chunks`'s contract: "no live chunks" means
    /// the file was emptied and every chunk for that origin is pruned.
    pub fn upsert_chunks_calls_and_prune(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
        prune_file: Option<&std::path::Path>,
        live_ids: &[&str],
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!(
            "upsert_chunks_calls_and_prune",
            chunks = chunks.len(),
            calls = calls.len(),
            prune = prune_file.is_some(),
            live_count = live_ids.len()
        )
        .entered();
        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        // v24 / #1221: same vendored pre-compute as the simpler
        // `upsert_chunks_batch` path.
        let prefixes = self.vendored_prefixes_slice();
        let vendored_per_chunk: Vec<bool> = chunks
            .iter()
            .map(|(chunk, _)| {
                let origin = crate::normalize_path(&chunk.file);
                crate::vendored::is_vendored_origin(&origin, prefixes)
            })
            .collect();

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(
                &mut tx,
                chunks,
                &embedding_bytes,
                &vendored_per_chunk,
                source_mtime,
                &now,
            )
            .await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;

            // Upsert calls: delete old calls for these chunk IDs, insert new ones
            if !calls.is_empty() {
                // Batch DELETE: collect unique caller IDs, delete in batches of 500
                let unique_ids: Vec<&str> = {
                    let mut seen = std::collections::HashSet::new();
                    calls
                        .iter()
                        .filter_map(|(id, _)| {
                            if seen.insert(id.as_str()) {
                                Some(id.as_str())
                            } else {
                                None
                            }
                        })
                        .collect()
                };
                for batch in
                    unique_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1))
                {
                    let placeholders: String = batch
                        .iter()
                        .enumerate()
                        .map(|(i, _)| format!("?{}", i + 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    let sql = format!("DELETE FROM calls WHERE caller_id IN ({})", placeholders);
                    let mut query = sqlx::query(&sql);
                    for id in batch {
                        query = query.bind(*id);
                    }
                    query.execute(&mut *tx).await?;
                }

                // 3 binds per row → modern SQLite variable limit yields
                // ~10822 rows per statement (was hardcoded 300).
                use crate::store::helpers::sql::max_rows_per_statement;
                const INSERT_BATCH: usize = max_rows_per_statement(3);
                for batch in calls.chunks(INSERT_BATCH) {
                    let mut query_builder: sqlx::QueryBuilder<sqlx::Sqlite> =
                        sqlx::QueryBuilder::new(
                            "INSERT INTO calls (caller_id, callee_name, line_number) ",
                        );
                    query_builder.push_values(batch.iter(), |mut b, (chunk_id, call)| {
                        b.push_bind(chunk_id)
                            .push_bind(&call.callee_name)
                            .push_bind(call.line_number as i64);
                    });
                    query_builder.build().execute(&mut *tx).await?;
                }
            }

            // DS2-4: Phantom-chunk pruning fused into the same transaction.
            // Mirrors `delete_phantom_chunks`, adapted to run on the open
            // `tx` instead of opening its own. An empty `live_ids` with
            // `Some(prune_file)` degrades to a full DELETE of the file —
            // same contract as `delete_phantom_chunks` → `delete_by_origin`.
            if let Some(file) = prune_file {
                let origin_str = crate::normalize_path(file);
                if live_ids.is_empty() {
                    // Whole file was emptied — inline `delete_by_origin`
                    // logic so the write stays in this tx.
                    sqlx::query(
                        "DELETE FROM chunks_fts WHERE id IN \
                         (SELECT id FROM chunks WHERE origin = ?1)",
                    )
                    .bind(&origin_str)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                        .bind(&origin_str)
                        .execute(&mut *tx)
                        .await?;
                    // NOTE: function_calls cleanup is handled by the watch
                    // loop's `upsert_function_calls` which DELETE-then-INSERTs
                    // the current set; same reasoning as `delete_phantom_chunks`
                    // below. See the NOTE at the end of this block.
                } else {
                    // Use a temp table to avoid SQLite's 999-parameter limit —
                    // a file can have 1000+ chunks.
                    sqlx::query("CREATE TEMP TABLE IF NOT EXISTS _live_ids (id TEXT PRIMARY KEY)")
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("DELETE FROM _live_ids")
                        .execute(&mut *tx)
                        .await?;

                    for batch in
                        live_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1))
                    {
                        let placeholders: Vec<String> = batch
                            .iter()
                            .enumerate()
                            .map(|(i, _)| format!("(?{})", i + 1))
                            .collect();
                        let insert_sql = format!(
                            "INSERT OR IGNORE INTO _live_ids (id) VALUES {}",
                            placeholders.join(",")
                        );
                        let mut stmt = sqlx::query(&insert_sql);
                        for id in batch {
                            stmt = stmt.bind(id);
                        }
                        stmt.execute(&mut *tx).await?;
                    }

                    let fts_query = "DELETE FROM chunks_fts WHERE id IN \
                         (SELECT id FROM chunks WHERE origin = ?1 \
                          AND id NOT IN (SELECT id FROM _live_ids))";
                    sqlx::query(fts_query)
                        .bind(&origin_str)
                        .execute(&mut *tx)
                        .await?;

                    let chunks_query = "DELETE FROM chunks WHERE origin = ?1 \
                         AND id NOT IN (SELECT id FROM _live_ids)";
                    let result = sqlx::query(chunks_query)
                        .bind(&origin_str)
                        .execute(&mut *tx)
                        .await?;
                    let deleted = result.rows_affected();
                    if deleted > 0 {
                        tracing::info!(
                            origin = %origin_str,
                            deleted,
                            "Removed phantom chunks (fused tx)"
                        );
                    }
                    // NOTE on `function_calls` cleanup: mirrors
                    // `delete_phantom_chunks`. The watch loop calls
                    // `upsert_function_calls` BEFORE us (at watch.rs :2492),
                    // which DELETE-then-INSERTs the current set for the
                    // file — adding a DELETE here would wipe those
                    // just-written rows. The `delete_by_origin` /
                    // `prune_missing` paths (file fully removed, no upsert
                    // follows) DO include that DELETE.
                }
            }

            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Delete chunks for a file that are no longer in the current parse output (RT-DATA-10).
    ///
    /// After re-parsing a file, some functions may have been removed. Their old
    /// chunks would linger as phantoms. This deletes chunks whose origin matches
    /// `file` but whose ID is not in `live_ids`.
    pub fn delete_phantom_chunks(
        &self,
        file: &std::path::Path,
        live_ids: &[&str],
    ) -> Result<u32, StoreError> {
        let _span =
            tracing::info_span!("delete_phantom_chunks", ?file, live_count = live_ids.len())
                .entered();
        let origin_str = crate::normalize_path(file);
        if live_ids.is_empty() {
            // No live chunks means the whole file was emptied/deleted —
            // delete_by_origin handles that case.
            return self.delete_by_origin(file);
        }

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // Use a temp table to avoid SQLite's 999-parameter limit.
            // A file can have 1000+ chunks (e.g., large generated files).
            sqlx::query("CREATE TEMP TABLE IF NOT EXISTS _live_ids (id TEXT PRIMARY KEY)")
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM _live_ids")
                .execute(&mut *tx)
                .await?;

            for batch in live_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1)) {
                let placeholders: Vec<String> = batch
                    .iter()
                    .enumerate()
                    .map(|(i, _)| format!("(?{})", i + 1))
                    .collect();
                let insert_sql = format!(
                    "INSERT OR IGNORE INTO _live_ids (id) VALUES {}",
                    placeholders.join(",")
                );
                let mut stmt = sqlx::query(&insert_sql);
                for id in batch {
                    stmt = stmt.bind(id);
                }
                stmt.execute(&mut *tx).await?;
            }

            let fts_query =
                "DELETE FROM chunks_fts WHERE id IN \
                 (SELECT id FROM chunks WHERE origin = ?1 AND id NOT IN (SELECT id FROM _live_ids))";
            sqlx::query(fts_query)
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            let chunks_query =
                "DELETE FROM chunks WHERE origin = ?1 AND id NOT IN (SELECT id FROM _live_ids)";
            let result = sqlx::query(chunks_query)
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // NOTE on `function_calls` cleanup: `delete_phantom_chunks` is
            // called by `cli/watch.rs` AFTER `upsert_function_calls`
            // (`watch.rs:2325`) for the same file, which already DELETE-then-
            // INSERTs the current set. Adding a `DELETE FROM function_calls`
            // here would wipe those just-written rows. The other two delete
            // paths (`delete_by_origin`, `prune_missing`) — file removed
            // entirely — DO have an explicit `function_calls` DELETE because
            // no upsert follows; see those functions.
            tx.commit().await?;
            let deleted = result.rows_affected() as u32;
            if deleted > 0 {
                tracing::info!(origin = %origin_str, deleted, "Removed phantom chunks");
            }
            Ok(deleted)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_utils::make_chunk;
    use crate::test_helpers::{mock_embedding, setup_store};

    // ===== upsert_chunks_batch tests =====

    #[test]
    fn test_upsert_chunks_batch_insert_and_fetch() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("alpha", "src/a.rs");
        let c2 = make_chunk("beta", "src/b.rs");
        let emb = mock_embedding(1.0);

        let count = store
            .upsert_chunks_batch(
                &[(c1.clone(), emb.clone()), (c2.clone(), emb.clone())],
                Some(100),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Verify via stats
        let stats = store.stats().unwrap();
        assert_eq!(stats.total_chunks, 2);
        assert_eq!(stats.total_files, 2);

        // Verify via chunk_count
        assert_eq!(store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn test_upsert_chunks_batch_updates_existing() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("alpha", "src/a.rs");
        let emb1 = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb1)], Some(100))
            .unwrap();

        // Re-insert same chunk with different embedding
        let emb2 = mock_embedding(2.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb2.clone())], Some(200))
            .unwrap();

        // Should still be 1 chunk (updated, not duplicated)
        assert_eq!(store.chunk_count().unwrap(), 1);

        // Embedding should be the updated one
        let found = store.get_embeddings_by_hashes(&[&c1.content_hash]).unwrap();
        assert!(found.contains_key(&c1.content_hash));
    }

    #[test]
    fn test_upsert_chunks_batch_empty() {
        let (store, _dir) = setup_store();
        let count = store.upsert_chunks_batch(&[], Some(100)).unwrap();
        assert_eq!(count, 0);
        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    /// #1221: end-to-end vendored-flag round-trip. With the default
    /// prefix list configured on the store, a chunk whose origin
    /// passes through `vendor/` is upserted with `vendored = 1` and
    /// retrieves as `ChunkSummary { vendored: true, .. }`; a sibling
    /// chunk under `src/` retrieves as `vendored: false`. The
    /// downstream JSON-emitter test on `SearchResult::to_json_with_origin`
    /// covers the trust_level wire shape; this test covers the
    /// upsert→SELECT round-trip that feeds it.
    #[test]
    fn test_upsert_round_trips_vendored_flag_under_default_prefixes() {
        use crate::store::ChunkSummary;
        let (store, _dir) = setup_store();

        // Apply the default prefix list (`vendor`, `node_modules`,
        // `third_party`, …) — this mirrors what `cmd_index` and
        // `cmd_watch` do at startup.
        store.set_vendored_prefixes(crate::vendored::effective_prefixes(None));

        let c_vend = make_chunk("oss_fn", "vendor/oss-lib/oss.rs");
        let c_user = make_chunk("user_fn", "src/main.rs");
        store
            .upsert_chunks_batch(
                &[
                    (c_vend.clone(), mock_embedding(1.0)),
                    (c_user.clone(), mock_embedding(1.0)),
                ],
                Some(100),
            )
            .expect("upsert");

        // Reach into the store via the same SELECT path search uses
        // (`fetch_chunks_by_ids_async` → `ChunkRow::from_row` → `ChunkSummary`).
        let map: std::collections::HashMap<String, ChunkSummary> = store
            .rt
            .block_on(async {
                store
                    .fetch_chunks_by_ids_async(&[c_vend.id.as_str(), c_user.id.as_str()])
                    .await
            })
            .expect("fetch")
            .into_iter()
            .map(|(id, row)| (id, ChunkSummary::from(row)))
            .collect();

        let vend_summary = map
            .get(&c_vend.id)
            .expect("vendored chunk round-trips through SELECT");
        let user_summary = map
            .get(&c_user.id)
            .expect("user chunk round-trips through SELECT");
        assert!(
            vend_summary.vendored,
            "vendor/-prefixed chunk must be flagged vendored after upsert+SELECT"
        );
        assert!(
            !user_summary.vendored,
            "src/-prefixed chunk must remain not-vendored"
        );
    }

    /// #1221: an empty prefix list (operator opt-out via
    /// `[index].vendored_paths = []` in `.cqs.toml`) disables vendored
    /// detection — even chunks under `vendor/` are stored with
    /// `vendored = 0`.
    #[test]
    fn test_upsert_round_trips_unvendored_when_prefix_list_empty() {
        use crate::store::ChunkSummary;
        let (store, _dir) = setup_store();

        // Explicit empty list: detection disabled.
        store.set_vendored_prefixes(crate::vendored::effective_prefixes(Some(&[])));

        let c = make_chunk("oss_fn_again", "vendor/oss-lib/oss.rs");
        store
            .upsert_chunks_batch(&[(c.clone(), mock_embedding(1.0))], Some(100))
            .expect("upsert");

        let map: std::collections::HashMap<String, ChunkSummary> = store
            .rt
            .block_on(async { store.fetch_chunks_by_ids_async(&[c.id.as_str()]).await })
            .expect("fetch")
            .into_iter()
            .map(|(id, row)| (id, ChunkSummary::from(row)))
            .collect();

        let summary = map.get(&c.id).expect("chunk round-trips");
        assert!(
            !summary.vendored,
            "explicit empty prefix list must disable vendored detection"
        );
    }

    // ===== EH-V1.30.1-1: touch_source_mtime =====

    /// Happy path: insert a chunk at one mtime, touch to a new mtime, verify
    /// `rows_affected > 0` and the stored value advanced. Pinned because the
    /// helper is the load-bearing piece of the parse-failure reconcile-loop
    /// fix — silent zero-row updates would defeat the entire fix.
    #[test]
    fn test_touch_source_mtime_updates_existing_chunk() {
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let chunk = make_chunk("alpha", "src/a.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(chunk.clone(), emb)], Some(100))
            .unwrap();

        // Touch to a far-future mtime; the row must be affected.
        let rows = store
            .touch_source_mtime(&PathBuf::from("src/a.rs"), 9_999_999_999)
            .unwrap();
        assert!(
            rows > 0,
            "touch_source_mtime must affect at least one row for an indexed origin"
        );

        // Verify the stored mtime advanced via `indexed_file_origins`, which
        // is the read path reconcile actually consults.
        let indexed = store.indexed_file_origins().unwrap();
        let stored = indexed
            .get("src/a.rs")
            .expect("origin must be present in indexed_file_origins");
        assert_eq!(stored.mtime, Some(9_999_999_999));
    }

    /// Origin that doesn't exist in the index → zero rows affected, no error.
    /// Reconcile depends on this graceful path so a touch on a path the
    /// indexer never saw doesn't crash the watch loop.
    #[test]
    fn test_touch_source_mtime_no_match_returns_zero() {
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let rows = store
            .touch_source_mtime(&PathBuf::from("src/never_indexed.rs"), 12345)
            .unwrap();
        assert_eq!(rows, 0);
    }

    /// #1219: `set_file_fingerprint` round-trips mtime+size+hash so the
    /// next `indexed_file_origins()` read sees a fully-populated
    /// `FileFingerprint`. Pre-test: rows have legacy NULLs because the
    /// upsert path doesn't bind the v23 columns. Calling the helper
    /// upgrades them in place.
    #[test]
    fn test_set_file_fingerprint_round_trips_all_three_fields() {
        use crate::store::chunks::staleness::FileFingerprint;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let chunk = make_chunk("alpha", "src/alpha.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        // Pre-state: legacy row (only mtime), v23 columns NULL.
        let pre = store.indexed_file_origins().unwrap();
        let pre_fp = pre
            .get("src/alpha.rs")
            .expect("origin must be present after upsert");
        assert_eq!(pre_fp.mtime, Some(100));
        assert_eq!(pre_fp.size, None);
        assert_eq!(pre_fp.content_hash, None);

        // Write a full fingerprint (mtime + size + 32-byte BLAKE3 hash).
        let fp = FileFingerprint {
            mtime: Some(9_999),
            size: Some(123),
            content_hash: Some(*blake3::hash(b"abc").as_bytes()),
        };
        let rows = store
            .set_file_fingerprint(&PathBuf::from("src/alpha.rs"), &fp)
            .unwrap();
        assert!(
            rows > 0,
            "set_file_fingerprint must affect at least one row for an indexed origin"
        );

        // Post-state: all three fields populated.
        let post = store.indexed_file_origins().unwrap();
        let post_fp = post
            .get("src/alpha.rs")
            .expect("origin must still be present");
        assert_eq!(post_fp.mtime, Some(9_999));
        assert_eq!(post_fp.size, Some(123));
        assert_eq!(post_fp.content_hash, fp.content_hash);
    }

    /// #1219: separator normalization mirrors `touch_source_mtime`. A
    /// Windows-style backslash origin must round-trip through
    /// `normalize_path` so the UPDATE matches the slash-form indexer key.
    /// Without this the v23 fingerprint columns silently stay NULL on
    /// Windows tools that emit `\\` separators.
    #[test]
    fn test_set_file_fingerprint_normalizes_separators() {
        use crate::store::chunks::staleness::FileFingerprint;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let chunk = make_chunk("beta", "src/b.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        let fp = FileFingerprint {
            mtime: Some(5_000),
            size: Some(11),
            content_hash: Some(*blake3::hash(b"fn b() {}").as_bytes()),
        };
        let rows = store
            .set_file_fingerprint(&PathBuf::from(r"src\b.rs"), &fp)
            .unwrap();
        assert_eq!(
            rows, 1,
            "set_file_fingerprint must normalize backslashes so the UPDATE matches the indexed origin"
        );
        let map = store.indexed_file_origins().unwrap();
        let stored = map.get("src/b.rs").expect("origin still slash-form");
        assert_eq!(stored.size, Some(11));
        assert_eq!(stored.content_hash, fp.content_hash);
    }

    /// CRITICAL invariant: the helper must call `crate::normalize_path()` on
    /// the origin so a Windows-style backslash path matches the indexer's
    /// forward-slash key. Pinned via the public API because the bug it
    /// guards against (zero-row UPDATEs from path format drift) is silent.
    #[test]
    fn test_touch_source_mtime_normalizes_separators() {
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        // Indexer always stores with forward slashes (see `normalize_path`).
        let chunk = make_chunk("beta", "src/b.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        // Caller passes a backslash-laden path (simulates a Windows path
        // arriving from the watch loop pre-normalization). The helper must
        // round-trip it through `normalize_path` to match the stored key.
        let rows = store
            .touch_source_mtime(&PathBuf::from(r"src\b.rs"), 7777)
            .unwrap();
        assert_eq!(
            rows, 1,
            "touch_source_mtime must normalize backslashes so the UPDATE matches the indexed origin"
        );
    }

    // ===== TC-8: LLM summary functions =====

    #[test]
    fn test_get_summaries_empty_input() {
        let (store, _dir) = setup_store();
        let result = store.get_summaries_by_hashes(&[], "summary").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_summaries_roundtrip() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "hash_a".to_string(),
                "summary A".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
            (
                "hash_b".to_string(),
                "summary B".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
            (
                "hash_c".to_string(),
                "summary C".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let result = store
            .get_summaries_by_hashes(&["hash_a", "hash_b", "hash_c"], "summary")
            .unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result["hash_a"], "summary A");
        assert_eq!(result["hash_b"], "summary B");
        assert_eq!(result["hash_c"], "summary C");
    }

    #[test]
    fn test_get_summaries_missing_keys() {
        let (store, _dir) = setup_store();
        let result = store
            .get_summaries_by_hashes(&["nonexistent_1", "nonexistent_2"], "summary")
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_summaries_mixed() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "h1".to_string(),
                "s1".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "h2".to_string(),
                "s2".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "h3".to_string(),
                "s3".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        // Query 5 hashes, only 3 exist
        let result = store
            .get_summaries_by_hashes(&["h1", "h2", "h3", "h4", "h5"], "summary")
            .unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains_key("h1"));
        assert!(result.contains_key("h2"));
        assert!(result.contains_key("h3"));
        assert!(!result.contains_key("h4"));
    }

    #[test]
    fn test_upsert_summaries_empty() {
        let (store, _dir) = setup_store();
        let count = store.upsert_summaries_batch(&[]).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_upsert_summaries_overwrites() {
        let (store, _dir) = setup_store();
        store
            .upsert_summaries_batch(&[(
                "h1".to_string(),
                "first".to_string(),
                "m".to_string(),
                "summary".to_string(),
            )])
            .unwrap();
        store
            .upsert_summaries_batch(&[(
                "h1".to_string(),
                "second".to_string(),
                "m".to_string(),
                "summary".to_string(),
            )])
            .unwrap();

        let result = store.get_summaries_by_hashes(&["h1"], "summary").unwrap();
        assert_eq!(result["h1"], "second");
    }

    #[test]
    fn test_get_all_summaries_empty() {
        let (store, _dir) = setup_store();
        let result = store.get_all_summaries("summary").unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_get_all_summaries_all() {
        let (store, _dir) = setup_store();
        let summaries = vec![
            (
                "ha".to_string(),
                "sa".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "hb".to_string(),
                "sb".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "hc".to_string(),
                "sc".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let all = store.get_all_summaries("summary").unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all["ha"], "sa");
        assert_eq!(all["hb"], "sb");
        assert_eq!(all["hc"], "sc");
    }

    #[test]
    fn test_prune_no_orphans() {
        let (store, _dir) = setup_store();

        // Insert chunks with known content_hashes
        let c1 = make_chunk("fn_a", "src/a.rs");
        let c2 = make_chunk("fn_b", "src/b.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb.clone()), (c2.clone(), emb)], Some(100))
            .unwrap();

        // Insert summaries matching those content_hashes
        let summaries = vec![
            (
                c1.content_hash,
                "summary a".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                c2.content_hash,
                "summary b".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 0);

        // All summaries survive
        let all = store.get_all_summaries("summary").unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_prune_removes_orphans() {
        let (store, _dir) = setup_store();

        // Insert one chunk
        let c1 = make_chunk("fn_a", "src/a.rs");
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1.clone(), emb)], Some(100))
            .unwrap();

        // Insert summaries: one matching, two orphans
        let summaries = vec![
            (
                c1.content_hash.clone(),
                "matching".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "orphan_hash_1".to_string(),
                "orphan 1".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
            (
                "orphan_hash_2".to_string(),
                "orphan 2".to_string(),
                "m".to_string(),
                "summary".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();
        assert_eq!(store.get_all_summaries("summary").unwrap().len(), 3);

        let pruned = store.prune_orphan_summaries().unwrap();
        assert_eq!(pruned, 2);

        let remaining = store.get_all_summaries("summary").unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&c1.content_hash));
    }

    // ===== TC-SQ8: purpose coexistence =====

    #[test]
    fn test_summaries_different_purposes_coexist() {
        let (store, _dir) = setup_store();

        // Insert same content_hash with two different purposes
        let summaries = vec![
            (
                "shared_hash".to_string(),
                "This function parses config files.".to_string(),
                "model-1".to_string(),
                "summary".to_string(),
            ),
            (
                "shared_hash".to_string(),
                "/// Parses configuration from TOML files.\n/// Returns a Config struct."
                    .to_string(),
                "model-1".to_string(),
                "doc-comment".to_string(),
            ),
        ];
        store.upsert_summaries_batch(&summaries).unwrap();

        // Each purpose returns only its own entry
        let by_summary = store
            .get_summaries_by_hashes(&["shared_hash"], "summary")
            .unwrap();
        assert_eq!(by_summary.len(), 1);
        assert_eq!(
            by_summary["shared_hash"],
            "This function parses config files."
        );

        let by_doc = store
            .get_summaries_by_hashes(&["shared_hash"], "doc-comment")
            .unwrap();
        assert_eq!(by_doc.len(), 1);
        assert!(by_doc["shared_hash"].contains("Parses configuration"));

        // get_all_summaries also filters by purpose
        let all_summary = store.get_all_summaries("summary").unwrap();
        assert_eq!(all_summary.len(), 1);
        let all_doc = store.get_all_summaries("doc-comment").unwrap();
        assert_eq!(all_doc.len(), 1);
    }

    // ===== delete_phantom_chunks tests (TC-42) =====

    #[test]
    fn delete_phantom_chunks_removes_stale() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let c2 = make_chunk("b", "file.rs");
        let c3 = make_chunk("c", "file.rs");
        let id1 = c1.id.clone();
        let id2 = c2.id.clone();
        store
            .upsert_chunks_batch(
                &[(c1, emb.clone()), (c2, emb.clone()), (c3, emb.clone())],
                Some(100),
            )
            .unwrap();

        // "c" was removed from the file
        let live: Vec<&str> = vec![id1.as_str(), id2.as_str()];
        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &live)
            .unwrap();
        assert_eq!(deleted, 1, "Should delete one phantom chunk");
        assert_eq!(store.chunk_count().unwrap(), 2);
    }

    #[test]
    fn delete_phantom_chunks_empty_live_ids_deletes_all() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let c2 = make_chunk("b", "file.rs");
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(100))
            .unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &[])
            .unwrap();
        assert_eq!(
            deleted, 2,
            "Empty live_ids should delete all chunks for file"
        );
    }

    #[test]
    fn delete_phantom_chunks_no_phantoms() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file.rs");
        let id1 = c1.id.clone();
        store.upsert_chunks_batch(&[(c1, emb)], Some(100)).unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file.rs"), &[id1.as_str()])
            .unwrap();
        assert_eq!(deleted, 0, "No phantoms to delete");
    }

    #[test]
    fn delete_phantom_chunks_wrong_file_unaffected() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let c1 = make_chunk("a", "file1.rs");
        let c2 = make_chunk("b", "file2.rs");
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb)], Some(100))
            .unwrap();

        let deleted = store
            .delete_phantom_chunks(std::path::Path::new("file1.rs"), &[])
            .unwrap();
        assert_eq!(deleted, 1, "Should only delete file1.rs chunks");
        assert_eq!(
            store.chunk_count().unwrap(),
            1,
            "file2.rs chunk should remain"
        );
    }

    // ===== TC-HAP-V1.33-7: update_umap_coords_batch happy path =====

    /// TC-HAP-V1.33-7: seed chunks, call `update_umap_coords_batch` with
    /// finite values, verify the rows are written and `umap_x`/`umap_y`
    /// round-trip via raw SELECT (matching how `build_cluster` reads them
    /// in `serve/data.rs`).
    #[test]
    fn test_update_umap_coords_batch_writes_coords_atomically() {
        let (store, _dir) = setup_store();

        // Seed two chunks first (need real chunk rows for UPDATE-FROM
        // semantics to land).
        let c1 = make_chunk("alpha", "src/a.rs");
        let c2 = make_chunk("beta", "src/b.rs");
        let id1 = c1.id.clone();
        let id2 = c2.id.clone();
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb)], Some(100))
            .unwrap();

        // Apply UMAP coords.
        let coords = vec![
            (id1.clone(), 1.5_f64, -2.25_f64),
            (id2.clone(), 0.25_f64, 3.75_f64),
        ];
        let updated = store.update_umap_coords_batch(&coords).unwrap();
        assert_eq!(updated, 2, "expected 2 rows updated, got {updated}");

        // Read back via raw SELECT (mirrors build_cluster's path).
        store.runtime().block_on(async {
            let (x1, y1): (f64, f64) =
                sqlx::query_as("SELECT umap_x, umap_y FROM chunks WHERE id = ?1")
                    .bind(&id1)
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
            assert!((x1 - 1.5).abs() < 1e-9, "x1 round-trip: got {x1}");
            assert!((y1 - (-2.25)).abs() < 1e-9, "y1 round-trip: got {y1}");

            let (x2, y2): (f64, f64) =
                sqlx::query_as("SELECT umap_x, umap_y FROM chunks WHERE id = ?1")
                    .bind(&id2)
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
            assert!((x2 - 0.25).abs() < 1e-9, "x2 round-trip: got {x2}");
            assert!((y2 - 3.75).abs() < 1e-9, "y2 round-trip: got {y2}");
        });
    }

    /// TC-HAP-V1.33-7: passing an extra unknown ID — the warn fires +
    /// `updated` is < input length. Documents the partial-update path
    /// the function uses when the projection script's input drifts.
    #[test]
    fn test_update_umap_coords_batch_handles_missing_ids() {
        let (store, _dir) = setup_store();

        // Seed one real chunk; submit two coords (one real id, one fake).
        let c = make_chunk("gamma", "src/g.rs");
        let real_id = c.id.clone();
        let emb = mock_embedding(1.0);
        store.upsert_chunks_batch(&[(c, emb)], Some(100)).unwrap();

        let coords = vec![
            (real_id.clone(), 0.5_f64, 0.5_f64),
            ("not-an-id".to_string(), 1.0_f64, 1.0_f64),
        ];
        let updated = store.update_umap_coords_batch(&coords).unwrap();
        assert_eq!(
            updated, 1,
            "fake id must not be written; expected 1 row updated, got {updated}"
        );
        assert!(
            updated < coords.len(),
            "updated ({updated}) < input.len() ({}) — pins the partial-update warn path",
            coords.len()
        );
    }

    // ===== TC-ADV-V1.33-10: update_umap_coords_batch NaN/Inf handling =====

    /// TC-ADV-V1.33-10: pin current behaviour around NaN/Inf coords.
    /// Today the temp table's `umap_x REAL NOT NULL` schema rejects NaN
    /// (sqlx binds NaN as NULL → constraint violation), so the call
    /// surfaces a SQLite error rather than panicking or silently
    /// writing corrupt floats to `chunks`. This test pins that
    /// observed status quo: a future fix that adds an explicit
    /// `is_finite` guard at the helper boundary should produce a
    /// different, more user-friendly error — flipping this test signals
    /// that contract change.
    #[test]
    fn test_update_umap_coords_batch_rejects_nan_today() {
        let (store, _dir) = setup_store();
        let c = make_chunk("delta", "src/d.rs");
        let id = c.id.clone();
        let emb = mock_embedding(1.0);
        store.upsert_chunks_batch(&[(c, emb)], Some(100)).unwrap();

        // Hostile input: a NaN coord. Today this fails the temp table's
        // NOT NULL constraint (NaN binds as NULL via sqlx).
        let coords = vec![(id.clone(), f64::NAN, 0.5_f64)];
        let result = store.update_umap_coords_batch(&coords);
        assert!(
            result.is_err(),
            "NaN coord must surface as an error today, got {result:?}"
        );
    }
}
