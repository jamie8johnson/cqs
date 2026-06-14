// WRITE_LOCK guard is held across .await inside block_on().
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
                let mut query = sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(sql.as_str()));
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
    /// Used by the enrichment pass to avoid per-page hash fetches.
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
                let mut query = sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(sql.as_str()));
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
    /// Used to validate batch results against the current index.
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
}

/// Write the reconcile fingerprint columns for one origin inside an open
/// transaction. Shared by [`Store::set_file_fingerprint`] (own tx, watch
/// path), [`Store::set_file_fingerprints_batch`] (one tx, many files), and
/// [`Store::upsert_embedded_batch`] (stamp fused into the chunk-write tx).
///
/// Writes the fingerprint to BOTH the chunk rows (so a file WITH chunks carries
/// it inline, the steady state) AND the `file_registry` table (v29, #1774) so
/// the fingerprint survives even for a file that has zero chunk rows. The
/// chunk-row UPDATE affects 0 rows for a zero-chunk origin; the registry UPSERT
/// is the load-bearing persistence in that case. Both run in the SAME
/// transaction as the chunk writes, honoring the crash-safety convention
/// #1772: a stamp never commits without its chunks, and vice versa.
///
/// `origin_str` must already be slash-normalized via `crate::normalize_path`.
///
/// Returns the number of *chunk* rows updated (unchanged contract — callers
/// use it to diagnose origin-format drift); the registry UPSERT always touches
/// exactly one row and isn't counted.
async fn set_fingerprint_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    origin_str: &str,
    fp: &crate::store::chunks::staleness::FileFingerprint,
) -> Result<u64, StoreError> {
    let size_i64: Option<i64> = fp.size.and_then(|s| i64::try_from(s).ok());
    let hash_blob: Option<Vec<u8>> = fp.content_hash.map(|h| h.to_vec());
    let result = sqlx::query(
        "UPDATE chunks \
         SET source_mtime = ?1, source_size = ?2, source_content_hash = ?3 \
         WHERE origin = ?4",
    )
    .bind(fp.mtime)
    .bind(size_i64)
    .bind(hash_blob.clone())
    .bind(origin_str)
    .execute(&mut **tx)
    .await?;

    // Persist the same fingerprint in the registry. For a file WITH chunks this
    // is redundant with the chunk-row columns — the staleness readers take a
    // per-column MAX() over the UNION of both sources, which collapses to one
    // row precisely BECAUSE this write keeps the two sides identical (every
    // stamp path routes through here, so they cannot drift apart). It also
    // makes the registry a complete shadow so a later prune-to-zero leaves the
    // fingerprint intact. For a ZERO-chunk file the UPDATE above touched
    // nothing and THIS is the only place the fingerprint lands — exactly the
    // #1774 fix.
    set_registry_fingerprint_in_tx(tx, origin_str, fp.mtime, size_i64, hash_blob).await?;

    Ok(result.rows_affected())
}

/// UPSERT one origin's fingerprint into the v29 `file_registry` table inside an
/// open transaction #1774. Pre-normalized `origin_str` only.
///
/// Split out so the zero-chunk pipeline paths (which never call the chunk-row
/// stamp) can write the registry directly, and so [`set_fingerprint_in_tx`] can
/// shadow every chunk-row stamp into the registry through one code path.
async fn set_registry_fingerprint_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    origin_str: &str,
    mtime: Option<i64>,
    size_i64: Option<i64>,
    hash_blob: Option<Vec<u8>>,
) -> Result<(), StoreError> {
    // A successful stamp clears any drift parse-failure marker (v31): reaching
    // this path means the file parsed (with or without chunks), so it is no
    // longer the unparseable-drifted file the marker was suppressing. Drift is
    // already healed by the re-stamped `chunks.parser_version`; resetting the
    // marker to NULL keeps the registry state honest for a later bump.
    sqlx::query(
        "INSERT INTO file_registry (origin, source_mtime, source_size, source_content_hash, parse_failed_parser_version) \
         VALUES (?1, ?2, ?3, ?4, NULL) \
         ON CONFLICT(origin) DO UPDATE SET \
             source_mtime = excluded.source_mtime, \
             source_size = excluded.source_size, \
             source_content_hash = excluded.source_content_hash, \
             parse_failed_parser_version = NULL",
    )
    .bind(origin_str)
    .bind(mtime)
    .bind(size_i64)
    .bind(hash_blob)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

// Write methods live on `impl Store<ReadWrite>` — the compiler refuses to
// call them on a `Store<ReadOnly>`.
impl Store<ReadWrite> {
    /// Insert or update chunks in batch using multi-row INSERT.
    ///
    /// Batch size is set by `max_rows_per_statement(22)` in `batch_insert_chunks`
    /// (22 binds per row against the SQLite 32766-variable limit, roughly
    /// 1488 rows per statement). FTS operations remain per-row because FTS5
    /// doesn't support upsert.
    ///
    /// **Cascade contract:**
    ///
    /// This uses `INSERT … ON CONFLICT(id) DO UPDATE SET …` (upsert): the row
    /// is updated *in place*, no `DELETE` fires, and `calls` / `type_edges`
    /// rows are preserved as-is.
    ///
    /// When a chunk's `content_hash` changes, its outgoing calls / type uses
    /// likely change too, so the preserved rows now reference a stale call
    /// graph. Callers **must** re-populate `calls` and `type_edges` for any
    /// chunk whose content changed (compare returned `content_hash` to the
    /// pre-existing snapshot from `snapshot_content_hashes`). The preserved
    /// rows aren't wrong, just stale until the caller refreshes.
    ///
    /// `enrichment_hash` and `enrichment_version` columns *are* preserved
    /// across upsert so the enrichment pass doesn't get its work invalidated
    /// by every reindex.
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

        // Compute the vendored bit per chunk from the store's configured
        // vendored-path prefixes. Empty prefix list → all-false. Origin
        // path is normalised to forward-slash form via
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

        let source_mtimes = vec![source_mtime; chunks.len()];
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(
                &mut tx,
                chunks,
                &embedding_bytes,
                &vendored_per_chunk,
                &source_mtimes,
                &now,
                false, // real embeddings → needs_embedding=0
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

    /// Update embeddings in batch without changing enrichment hashes.
    ///
    /// `updates` is a slice of `(chunk_id, embedding)` pairs. Chunk IDs not
    /// found in the store are logged and skipped (rows_affected == 0).
    /// Returns the count of actually updated rows.
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

        // Temp table + single UPDATE...FROM instead of N individual UPDATEs.
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

            // 2. Batch INSERT into temp table. `max_rows_per_statement(3)`
            // derives ~10822 rows per statement against SQLite's 32766-variable
            // limit (3 binds per row). On a full reindex with 50k updated
            // embeddings that's ~5 INSERTs.
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
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for (i, (id, _, hash)) in batch.iter().enumerate() {
                    query = query.bind(id);
                    query = query.bind(&batch_bytes[i]);
                    query = query.bind(hash.as_deref());
                }
                query.execute(&mut *tx).await?;
            }

            // 3. Single UPDATE...FROM join (SQLite 3.33+).
            //
            // Clear `needs_embedding=0` on rows that get a real embedding
            // written. The first-pass-skip path writes chunks with
            // `needs_embedding=1` + zero-vec sentinel; a subsequent
            // enrichment_pass call lands here with the real vector and must
            // clear the flag so the chunk becomes visible to HNSW build /
            // search hydration. For chunks already at `needs_embedding=0`
            // this is a no-op write.
            //
            // Also repopulate `embedding_base` when it was previously NULL.
            // The first-pass-skip path inserts chunks with
            // `embedding_base = NULL` (per `upsert_embedded_batch`'s sentinel mode);
            // without this, every `--llm-summaries` reindex permanently
            // leaves the chunk invisible to `build_hnsw_base_index` (which
            // filters `WHERE embedding_base IS NOT NULL`), silently degrading
            // the DenseBase routing target (conceptual / behavioral /
            // negation queries).
            //
            // Using `COALESCE(chunks.embedding_base, t.embedding)`
            // preserves the prior base bytes for chunks that were
            // already populated, so the enrichment-time second pass
            // (which overwrites `embedding` with the call-context
            // enriched vector) doesn't trash their base copy.
            let result = sqlx::query(
                "UPDATE chunks SET \
                    embedding = t.embedding, \
                    embedding_base = COALESCE(chunks.embedding_base, t.embedding), \
                    enrichment_hash = COALESCE(t.enrichment_hash, chunks.enrichment_hash), \
                    needs_embedding = 0 \
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

            // Embedding updates must advance the store write generation.
            // The HNSW sidecar stamp (chunk_count + splade_generation) is
            // how the dirty-flag self-heal proves a sidecar postdates the
            // last vector-affecting write; an enrichment pass rewrites
            // `embedding`/`embedding_base` without touching chunk rows or
            // sparse vectors, so without this bump a crash between this
            // commit and the HNSW save leaves a sidecar whose stamp still
            // matches the live store — and the self-heal would serve the
            // pre-enrichment vectors as current. The bump also invalidates
            // the persisted SPLADE index (its loader compares the same
            // counter); that rebuild is spurious — sparse vectors are
            // unchanged — but cheap, and a single write-generation counter
            // is simpler than splitting dense/sparse generations.
            crate::store::sparse::bump_splade_generation_tx(&mut tx).await?;

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

            // TEMP TABLE is connection-scoped, not transaction-scoped.
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
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
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
                // ON CONFLICT DO UPDATE (not INSERT OR REPLACE) so the upsert
                // is a true UPDATE on conflict and never fires the implicit
                // DELETE that INSERT OR REPLACE emits. There's no FK to chunks
                // today, but a future ON DELETE CASCADE addition would
                // otherwise turn every summary refresh into a splade-trigger
                // fire (full SPLADE invalidation).
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
    /// write-coalescing queue.
    ///
    /// The queue holds rows in-memory until either the row threshold or
    /// the time interval is crossed, at which point a synchronous flush
    /// drains every queued row inside one `begin_write()` transaction, so
    /// all `index.db` writes serialize through `WRITE_LOCK` with one fsync
    /// per batch instead of one per row.
    #[cfg(feature = "llm-summaries")]
    pub fn queue_summary_write(&self, custom_id: &str, text: &str, model: &str, purpose: &str) {
        // Validate prose summaries before they reach the cache. The
        // doc-comment purpose is intentionally exempt — its prompt asks for
        // imperative reference docs which trip the heuristics on legitimate
        // content. Doc-comment write-back has its own review gate.
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
    /// This path goes through `WRITE_LOCK` via the queue's flush, so a
    /// concurrent reindex serializes through the same in-process mutex.
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

            // `function_calls` has no FK to `chunks` (it stores `caller_name`
            // strings, not chunk IDs), so deleting chunks does not cascade.
            // Without this DELETE, every incremental delete path leaves orphan
            // call-graph rows that surface as ghost callers in
            // `cqs callers`/`callees`/`dead`.
            sqlx::query("DELETE FROM function_calls WHERE file = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // v32: the `candidate_edges` side-table is also file-keyed with no FK
            // to chunks, so the wholesale file-removal path must delete its rows
            // for the same origin — otherwise a deleted file leaves orphan
            // candidate rows behind (a later consumer would read stale candidates
            // for a file that no longer exists).
            sqlx::query("DELETE FROM candidate_edges WHERE file = ?1")
                .bind(&origin_str)
                .execute(&mut *tx)
                .await?;

            // Prune the v29 `file_registry` fingerprint for this origin #1774.
            // `delete_by_origin` is the wholesale file-removal path (watch
            // delete event, model swap, slot teardown), so the persisted
            // zero-chunk fingerprint must go with the chunks — same transaction
            // so the registry never outlives the rows it described.
            sqlx::query("DELETE FROM file_registry WHERE origin = ?1")
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
    /// When the watch loop's `parse_file_all_with_chunk_calls` fails (syntax
    /// error in the user's code), the watch path emits an empty chunk vector
    /// for that file. Without bumping stored mtime, the existing chunks stay
    /// AND `chunks.source_mtime` is never refreshed, so `run_daemon_reconcile`
    /// keeps classifying the file MODIFIED on every tick (default 30 s) — an
    /// unbounded reindex-fail-warn loop until the user fixes the syntax.
    ///
    /// This helper lets the parse-failure path bump stored mtime so reconcile
    /// sees `disk == stored` and stops re-queuing the file. The chunks are
    /// intentionally left as-is — they may still serve from the index until
    /// the next successful re-parse, but that's strictly better than ghost
    /// chunks plus a hot reindex loop.
    ///
    /// The stored `source_size` / `source_content_hash` are CLEARED (set NULL)
    /// alongside the mtime bump. They describe content the parser couldn't
    /// process; leaving them in place keeps the `MtimeOrHash` tiebreak
    /// classifying the file divergent on every tick whenever the failing edit
    /// changed the file's size — exactly the loop this helper exists to break.
    /// NULL size+hash degrades the origin to the documented mtime-only
    /// matching until the next successful reindex restores a full fingerprint.
    ///
    /// The same touched fingerprint is UPSERTed into `file_registry` so the
    /// loop also breaks for origins with zero chunk rows (a comment-only file
    /// edited into a syntax error, or a brand-new file that has never parsed) —
    /// the chunk-row UPDATE alone touches nothing for those.
    ///
    /// Returns the number of chunk rows whose `source_mtime` was updated (the
    /// registry UPSERT is not counted). Callers can log a warn if
    /// `rows_affected == 0` for an origin expected to carry chunks (origin
    /// format mismatch would be the most likely cause).
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
            let result = sqlx::query(
                "UPDATE chunks SET source_mtime = ?1, \
                     source_size = NULL, source_content_hash = NULL \
                 WHERE origin = ?2",
            )
            .bind(mtime_ms)
            .bind(&origin_str)
            .execute(&mut *tx)
            .await?;
            // Mirror the touched mtime-only fingerprint into the registry so
            // zero-chunk / never-parsed origins also stop re-queuing. NULL
            // size+hash here as well — the readers take MAX() across chunk
            // rows and registry, so a stale registry hash would resurrect the
            // tiebreak the chunk-side NULLing just disabled.
            set_registry_fingerprint_in_tx(&mut tx, &origin_str, Some(mtime_ms), None, None)
                .await?;
            tx.commit().await?;
            Ok(result.rows_affected() as u32)
        })
    }

    /// Record the drift parse-failure marker (v31) for one or more origins: the
    /// parser version they failed to parse at. UPSERTs into `file_registry`,
    /// preserving any existing fingerprint columns (a parse failure does not
    /// touch the fingerprint — that is the FINGERPRINT loop-breaker's job, via
    /// `touch_source_mtime`).
    ///
    /// `origins_with_parser_drift` excludes origins whose marker equals the
    /// current parser version, so a version-drifted file that cannot parse is
    /// not re-queued by drift every reconcile / `cqs index` tick until its
    /// content changes. A successful re-parse clears the marker
    /// (`set_registry_fingerprint_in_tx` writes NULL), and any content edit
    /// re-routes the file through the parse path where it either succeeds
    /// (clears) or fails again (re-stamps the marker).
    ///
    /// Origins must be slash-normalized (`crate::normalize_path`).
    pub fn record_parse_failures(
        &self,
        origins: &[&str],
        parser_version: u32,
    ) -> Result<(), StoreError> {
        let _span = tracing::debug_span!(
            "record_parse_failures",
            count = origins.len(),
            parser_version
        )
        .entered();
        if origins.is_empty() {
            return Ok(());
        }
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            for origin in origins {
                // Preserve fingerprint columns: ON CONFLICT updates ONLY the
                // marker. A brand-new (never-fingerprinted) origin inserts a row
                // with NULL fingerprint columns and the marker set.
                sqlx::query(
                    "INSERT INTO file_registry (origin, parse_failed_parser_version) \
                     VALUES (?1, ?2) \
                     ON CONFLICT(origin) DO UPDATE SET \
                         parse_failed_parser_version = excluded.parse_failed_parser_version",
                )
                .bind(*origin)
                .bind(i64::from(parser_version))
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
    }

    /// Refresh the full reconcile fingerprint (`source_mtime`, `source_size`,
    /// `source_content_hash`) on every chunk for `origin`.
    ///
    /// The `source_size` and `source_content_hash` columns let Layer 2
    /// reconciliation (`run_daemon_reconcile`) fall back to BLAKE3 when
    /// mtime/size alone is unreliable (coarse-mtime FAT32/NTFS/HFS+/SMB;
    /// `git checkout` and formatter passes that bump mtime without changing
    /// content). The watch reindex path (`cli/watch/reindex.rs`) calls this
    /// helper after its per-file chunk upsert; the bulk pipeline
    /// (`cli/pipeline/upsert.rs`) stamps fingerprints inside the chunk-write
    /// transaction via [`Self::upsert_embedded_batch`] instead, so both
    /// production paths leave populated fingerprints for the next reconcile.
    ///
    /// `None` fields stay NULL; callers that can't read disk pass a
    /// fingerprint with all three set to `None` and get mtime-only behavior.
    /// `read_disk` always populates mtime+size; only the hash is conditional
    /// on policy.
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
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let rows = set_fingerprint_in_tx(&mut tx, &origin_str, fp).await?;
            tx.commit().await?;
            Ok(rows as u32)
        })
    }

    /// Refresh the reconcile fingerprint for many origins in one write
    /// transaction.
    ///
    /// Used by the pipeline's staleness pre-filter when a file's mtime/size
    /// moved but the BLAKE3 tiebreak proved the content identical (`git
    /// checkout`, formatter no-op, `touch`): the chunks need no reindex, but
    /// without refreshing the stored fingerprint every subsequent index run
    /// and daemon reconcile pass would re-hash the file to reach the same
    /// conclusion. One transaction for the whole batch — this path can see
    /// hundreds of files after a branch flip.
    ///
    /// Returns the total number of chunk rows updated.
    pub fn set_file_fingerprints_batch(
        &self,
        entries: &[(
            std::path::PathBuf,
            crate::store::chunks::staleness::FileFingerprint,
        )],
    ) -> Result<u64, StoreError> {
        let _span =
            tracing::info_span!("set_file_fingerprints_batch", files = entries.len()).entered();
        if entries.is_empty() {
            return Ok(0);
        }
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let mut total = 0u64;
            for (path, fp) in entries {
                total += set_fingerprint_in_tx(&mut tx, &crate::normalize_path(path), fp).await?;
            }
            tx.commit().await?;
            Ok(total)
        })
    }

    /// Persist reconcile fingerprints in the v29 `file_registry` table for
    /// files that produced ZERO chunks this run #1774.
    ///
    /// A zero-chunk file (comment-only source, parser-emitted-empty) has no
    /// chunk row to carry the fingerprint, so [`Self::set_file_fingerprint`] /
    /// [`Self::upsert_embedded_batch`]'s chunk-row UPDATE is a no-op for it.
    /// Without a persisted fingerprint the staleness pre-filter reclassifies it
    /// "not indexed" every run and re-parses it forever. This writes the
    /// fingerprint straight into the registry so the next run's pre-filter sees
    /// a stored fingerprint and skips the parse like any unchanged file.
    ///
    /// Called by the bulk pipeline's store stage and the watch reindex path for
    /// their respective zero-chunk sets. One transaction for the whole batch.
    /// Returns the number of origins stamped.
    pub fn set_file_registry_fingerprints_batch(
        &self,
        entries: &[(
            std::path::PathBuf,
            crate::store::chunks::staleness::FileFingerprint,
        )],
    ) -> Result<u64, StoreError> {
        let _span = tracing::info_span!(
            "set_file_registry_fingerprints_batch",
            files = entries.len()
        )
        .entered();
        if entries.is_empty() {
            return Ok(0);
        }
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            for (path, fp) in entries {
                let size_i64: Option<i64> = fp.size.and_then(|s| i64::try_from(s).ok());
                let hash_blob: Option<Vec<u8>> = fp.content_hash.map(|h| h.to_vec());
                set_registry_fingerprint_in_tx(
                    &mut tx,
                    &crate::normalize_path(path),
                    fp.mtime,
                    size_i64,
                    hash_blob,
                )
                .await?;
            }
            tx.commit().await?;
            Ok(entries.len() as u64)
        })
    }

    /// Atomically upsert chunks — real-embedding plus zero-vec sentinel,
    /// spanning any number of files — in a single write transaction, and stamp
    /// each file's reconcile fingerprint (`source_mtime`, `source_size`,
    /// `source_content_hash`) in the same transaction.
    ///
    /// SECONDARY write path. The bulk `cqs index` store stage now writes through
    /// the per-FILE fused primitive `upsert_file_fused` (chunks + calls +
    /// function_calls + prune + stamp, all-or-nothing per file) — the
    /// orphan-impossible #1835 path. This batch-shaped method remains the write
    /// path for callers that re-stamp chunks WITHOUT the call-graph dimension:
    /// the enrichment pass (`enrichment_pass`, which rewrites embeddings for
    /// already-coherent chunks), the reuse-resolver hydration, and store-level
    /// tests/seeds. It folds chunks + FTS + fingerprint stamp into one tx so
    /// those re-writes never desync chunks from FTS.
    ///
    /// Crash-atomicity contract: a crash may lose whole uncommitted batches, but
    /// the chunks and FTS rows that DID land always committed together — the
    /// index is never left with a chunk/FTS desync for the rows it contains.
    ///
    /// Fingerprint ordering: a file's chunks can straddle batches, so callers
    /// stamp a file's fingerprint only in the batch carrying its LAST chunk; the
    /// stamp then commits no earlier than every one of the file's chunks. If the
    /// process dies between those commits the file is left UNSTAMPED, so the next
    /// run's staleness pre-filter (`filter_stale_files`) reclassifies it STALE.
    /// `fingerprints` therefore only ever mentions files whose every chunk is
    /// present in THIS batch's row sets.
    ///
    /// `sentinel` chunks are written via the same zero-vec
    /// `needs_embedding=1` contract that `enrichment_pass` and the reuse
    /// resolver depend on (skip-first-pass under `--llm-summaries`): each
    /// sentinel row carries a zero vector in `embedding`, NULL
    /// `embedding_base`, and stays invisible to HNSW build / search until
    /// enrichment lands its real vector. `real` chunks land at
    /// `needs_embedding=0`.
    ///
    /// `fingerprints` is keyed by `Chunk::file`. Per-row `source_mtime` binds
    /// come from the matching fingerprint; files absent from the map get
    /// NULL mtime and no fingerprint stamp (degraded mtime-only behavior).
    /// The stamp runs as an `UPDATE … WHERE origin = ?` *after* the row
    /// upserts so it also covers rows the `ON CONFLICT … WHERE`
    /// short-circuit skipped (content unchanged, mtime bumped) — a stored
    /// `source_content_hash` therefore never describes a previous content
    /// version of the file.
    pub fn upsert_embedded_batch(
        &self,
        real: &[(Chunk, Embedding)],
        sentinel: &[Chunk],
        fingerprints: &std::collections::HashMap<
            std::path::PathBuf,
            crate::store::chunks::staleness::FileFingerprint,
        >,
    ) -> Result<usize, StoreError> {
        let _span = tracing::info_span!(
            "upsert_embedded_batch",
            real = real.len(),
            sentinel = sentinel.len(),
            files = fingerprints.len()
        )
        .entered();
        if real.is_empty() && sentinel.is_empty() {
            return Ok(0);
        }

        let dim = self.dim;
        let real_bytes: Vec<Vec<u8>> = real
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;
        let zero_vec = Embedding::new(vec![0.0_f32; dim]);
        let sentinel_pairs: Vec<(Chunk, Embedding)> = sentinel
            .iter()
            .map(|c| (c.clone(), zero_vec.clone()))
            .collect();
        let sentinel_bytes: Vec<Vec<u8>> = sentinel_pairs
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        let prefixes = self.vendored_prefixes_slice();
        let vendored_for = |chunk: &Chunk| {
            let origin = crate::normalize_path(&chunk.file);
            crate::vendored::is_vendored_origin(&origin, prefixes)
        };
        let real_vendored: Vec<bool> = real.iter().map(|(c, _)| vendored_for(c)).collect();
        let sentinel_vendored: Vec<bool> = sentinel_pairs
            .iter()
            .map(|(c, _)| vendored_for(c))
            .collect();

        let mtime_for = |chunk: &Chunk| fingerprints.get(&chunk.file).and_then(|fp| fp.mtime);
        let real_mtimes: Vec<Option<i64>> = real.iter().map(|(c, _)| mtime_for(c)).collect();
        let sentinel_mtimes: Vec<Option<i64>> =
            sentinel_pairs.iter().map(|(c, _)| mtime_for(c)).collect();

        // Only stamp fingerprints for files actually present in this batch's
        // chunk sets. The pipeline already restricts `fingerprints` to files
        // whose LAST chunk rides in this batch (file-complete stamping), and
        // the GPU-failure split keeps a straddling file's fingerprint with the
        // later-landing requeued half — so this intersection is a defensive
        // belt-and-braces against stamping a file before all its rows landed.
        let batch_files: std::collections::HashSet<&std::path::PathBuf> = real
            .iter()
            .map(|(c, _)| &c.file)
            .chain(sentinel.iter().map(|c| &c.file))
            .collect();

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let now = chrono::Utc::now().to_rfc3339();
            if !real.is_empty() {
                let old_hashes = snapshot_content_hashes(&mut tx, real).await?;
                batch_insert_chunks(
                    &mut tx,
                    real,
                    &real_bytes,
                    &real_vendored,
                    &real_mtimes,
                    &now,
                    false, // real embeddings → needs_embedding=0
                )
                .await?;
                upsert_fts_conditional(&mut tx, real, &old_hashes).await?;
            }
            if !sentinel_pairs.is_empty() {
                let old_hashes = snapshot_content_hashes(&mut tx, &sentinel_pairs).await?;
                batch_insert_chunks(
                    &mut tx,
                    &sentinel_pairs,
                    &sentinel_bytes,
                    &sentinel_vendored,
                    &sentinel_mtimes,
                    &now,
                    true, // zero-vec sentinel → needs_embedding=1
                )
                .await?;
                upsert_fts_conditional(&mut tx, &sentinel_pairs, &old_hashes).await?;
            }
            for file in batch_files {
                if let Some(fp) = fingerprints.get(file) {
                    set_fingerprint_in_tx(&mut tx, &crate::normalize_path(file), fp).await?;
                }
            }
            tx.commit().await?;
            Ok(real.len() + sentinel_pairs.len())
        })
    }

    /// Atomically upsert chunks + calls AND prune phantom chunks for a file,
    /// all inside a single `begin_write()` transaction.
    ///
    /// Folding the upsert and the phantom prune into one tx makes the reindex
    /// all-or-nothing: a crash can't leave the index half-pruned (new chunks
    /// visible but removed chunks still present, plus a dirty HNSW flag).
    ///
    /// When `prune_file` is `None`, phantom pruning is skipped. When
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
        self.upsert_chunks_calls_and_prune_inner(
            chunks,
            source_mtime,
            calls,
            prune_file,
            live_ids,
            None,
            &[],
            None,
            None,
        )
    }

    /// Same as [`Self::upsert_chunks_calls_and_prune`] but ALSO writes
    /// the file-level `function_calls` table in the same transaction.
    ///
    /// Folding the function_calls write into this per-file tx makes the
    /// reindex all-or-nothing. Otherwise a crash during the embed phase can
    /// leave the function_calls table with the new state while chunks/FTS
    /// lags — `cqs callers <new_fn>` works but `cqs explain <new_fn>` and
    /// search-by-name don't.
    ///
    /// `file_function_calls` MUST be paired with `prune_file` (the file the
    /// function_calls belong to). When `prune_file = None`, this method
    /// asserts `file_function_calls.is_none()` because there's no file
    /// scope to delete-then-insert against.
    ///
    /// `file_candidate_edges` (Lane 2) is the file's low-confidence
    /// `candidate_edges` set, written wholesale in the same tx as
    /// `function_calls`; like it, it requires `prune_file` to scope the DELETE.
    /// `None` leaves candidates untouched; `Some(&[])` clears the file's
    /// candidates.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_chunks_calls_and_prune_with_file_calls(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
        prune_file: Option<&std::path::Path>,
        live_ids: &[&str],
        file_function_calls: Option<&[crate::parser::FunctionCalls]>,
        file_candidate_edges: Option<&[crate::parser::CandidateSite]>,
    ) -> Result<usize, StoreError> {
        if file_function_calls.is_some() {
            debug_assert!(
                prune_file.is_some(),
                "file_function_calls requires prune_file to scope the DELETE"
            );
        }
        self.upsert_chunks_calls_and_prune_inner(
            chunks,
            source_mtime,
            calls,
            prune_file,
            live_ids,
            file_function_calls,
            &[],
            None,
            file_candidate_edges,
        )
    }

    /// Per-FILE fused write for the BULK pipeline (`cqs index`), #1835.
    ///
    /// Folds EVERYTHING a single file needs into ONE `begin_write` transaction:
    /// real-embedding chunks + zero-vec sentinel chunks + their FTS rows +
    /// per-chunk `calls` + file-level `function_calls` + phantom-chunk prune +
    /// the reconcile fingerprint stamp (chunk-row columns AND `file_registry`
    /// shadow). All-or-nothing: a failure anywhere rolls the whole file back to
    /// its prior coherent state, leaving it UNstamped so the next reconcile
    /// re-selects and re-indexes it. This is what makes the bulk path
    /// orphan-impossible in BOTH directions — no chunks-without-calls and no
    /// calls-without-chunks (the `find_orphaned_function_calls` magnet), and no
    /// stamp-ahead-of-content (false-DEAD) — the same all-or-nothing shape the
    /// watch path's `upsert_chunks_calls_and_prune_with_file_calls` has, plus the
    /// in-tx stamp.
    ///
    /// Replaces the old three-transaction bulk sequence (calls → chunks → stamp),
    /// whose every ordering left a mid-sequence window that stranded a committed
    /// write. `prune_file`/`live_ids` follow the same contract as the other fused
    /// methods (empty `live_ids` + `Some(origin)` = the file was emptied; every
    /// chunk for the origin is pruned). `fingerprint` is always `Some` here.
    ///
    /// `file_candidate_edges` is the v32 call-graph candidate side-table for the
    /// file, folded into the same tx as `function_calls`. An empty slice clears
    /// the file's candidates (DELETE WHERE file = ?), which is the common case
    /// until a parser-emit pass (a later lane) produces candidate sites.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_file_fused(
        &self,
        real: &[(Chunk, Embedding)],
        sentinel: &[Chunk],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
        prune_file: &std::path::Path,
        live_ids: &[&str],
        file_function_calls: &[crate::parser::FunctionCalls],
        file_candidate_edges: &[crate::parser::CandidateSite],
        fingerprint: &crate::store::chunks::staleness::FileFingerprint,
    ) -> Result<usize, StoreError> {
        self.upsert_chunks_calls_and_prune_inner(
            real,
            source_mtime,
            calls,
            Some(prune_file),
            live_ids,
            Some(file_function_calls),
            sentinel,
            Some(fingerprint),
            Some(file_candidate_edges),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_chunks_calls_and_prune_inner(
        &self,
        chunks: &[(Chunk, Embedding)],
        source_mtime: Option<i64>,
        calls: &[(String, crate::parser::CallSite)],
        prune_file: Option<&std::path::Path>,
        live_ids: &[&str],
        file_function_calls: Option<&[crate::parser::FunctionCalls]>,
        // #1835: zero-vec sentinel chunks (`needs_embedding=1`) for the bulk
        // pipeline's `--llm-summaries` skip-first-pass path. The watch path
        // passes `&[]`. Folded into the same tx as `chunks` so a file's whole
        // chunk set commits atomically regardless of which half each chunk is in.
        sentinel: &[Chunk],
        // #1835: when `Some`, stamp the file's reconcile fingerprint (chunk-row
        // columns + `file_registry` shadow) INSIDE this transaction, AFTER the
        // chunk/calls/function_calls/prune writes. This makes the stamp
        // all-or-nothing with the content it certifies — the bulk path's
        // orphan-impossible guarantee (a rolled-back tx leaves the file in its
        // prior coherent state, unstamped, so it re-indexes next run). The watch
        // path passes `None` and keeps its own post-tx best-effort stamp.
        // `prune_file` must be `Some` so the stamp has an origin to key on.
        fingerprint: Option<&crate::store::chunks::staleness::FileFingerprint>,
        // v32: when `Some`, write the file's `candidate_edges` (the call-graph
        // candidate side-table) in the SAME tx as function_calls, folded in
        // right after the function_calls write so the two file-keyed call-graph
        // tables commit together. Requires `prune_file` to scope the DELETE,
        // same as `file_function_calls`. Empty/`None` is the common case until a
        // parser-emit pass (a later lane) produces candidate sites.
        file_candidate_edges: Option<&[crate::parser::CandidateSite]>,
    ) -> Result<usize, StoreError> {
        if fingerprint.is_some() {
            debug_assert!(
                prune_file.is_some(),
                "in-tx fingerprint stamp requires prune_file to supply the origin"
            );
        }
        if file_candidate_edges.is_some() {
            debug_assert!(
                prune_file.is_some(),
                "file_candidate_edges requires prune_file to scope the DELETE"
            );
        }
        let _span = tracing::info_span!(
            "upsert_chunks_calls_and_prune",
            chunks = chunks.len(),
            sentinel = sentinel.len(),
            calls = calls.len(),
            prune = prune_file.is_some(),
            live_count = live_ids.len(),
            file_function_calls = file_function_calls.is_some(),
            file_candidate_edges = file_candidate_edges.is_some(),
            stamp = fingerprint.is_some()
        )
        .entered();
        let dim = self.dim;
        let embedding_bytes: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;
        // Sentinel chunks carry a zero-vec embedding stamped `needs_embedding=1`;
        // the enrichment pass overwrites it later.
        let zero_vec = Embedding::new(vec![0.0_f32; dim]);
        let sentinel_pairs: Vec<(Chunk, Embedding)> = sentinel
            .iter()
            .map(|c| (c.clone(), zero_vec.clone()))
            .collect();
        let sentinel_bytes: Vec<Vec<u8>> = sentinel_pairs
            .iter()
            .map(|(_, emb)| embedding_to_bytes(emb, dim))
            .collect::<Result<Vec<_>, _>>()?;

        // Same vendored pre-compute as the simpler `upsert_chunks_batch` path.
        let prefixes = self.vendored_prefixes_slice();
        let vendored_for = |chunk: &Chunk| {
            let origin = crate::normalize_path(&chunk.file);
            crate::vendored::is_vendored_origin(&origin, prefixes)
        };
        let vendored_per_chunk: Vec<bool> = chunks.iter().map(|(c, _)| vendored_for(c)).collect();
        let sentinel_vendored: Vec<bool> = sentinel_pairs
            .iter()
            .map(|(c, _)| vendored_for(c))
            .collect();

        let source_mtimes = vec![source_mtime; chunks.len()];
        let sentinel_mtimes = vec![source_mtime; sentinel_pairs.len()];
        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;
            let old_hashes = snapshot_content_hashes(&mut tx, chunks).await?;
            let now = chrono::Utc::now().to_rfc3339();
            batch_insert_chunks(
                &mut tx,
                chunks,
                &embedding_bytes,
                &vendored_per_chunk,
                &source_mtimes,
                &now,
                false, // real embeddings → needs_embedding=0
            )
            .await?;
            upsert_fts_conditional(&mut tx, chunks, &old_hashes).await?;
            if !sentinel_pairs.is_empty() {
                let sentinel_old = snapshot_content_hashes(&mut tx, &sentinel_pairs).await?;
                batch_insert_chunks(
                    &mut tx,
                    &sentinel_pairs,
                    &sentinel_bytes,
                    &sentinel_vendored,
                    &sentinel_mtimes,
                    &now,
                    true, // zero-vec sentinel → needs_embedding=1
                )
                .await?;
                upsert_fts_conditional(&mut tx, &sentinel_pairs, &sentinel_old).await?;
            }

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
                    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                    for id in batch {
                        query = query.bind(*id);
                    }
                    query.execute(&mut *tx).await?;
                }

                // 3 binds per row → SQLite's variable limit yields
                // ~10822 rows per statement.
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

            // Phantom-chunk pruning fused into the same transaction.
            //
            // This is a deliberate near-verbatim copy of `delete_phantom_chunks`,
            // NOT an accidental duplicate. The distinction is the prune scope:
            //   - This fused path operates on a *survivor set* — it deletes only
            //     the chunks for `prune_file` that are absent from `live_ids`,
            //     the rows that survived the just-applied upsert *inside the
            //     active transaction*. Folding it into the same `tx` keeps the
            //     upsert + prune atomic so a reindex never exposes a half-state.
            //   - Standalone `delete_phantom_chunks` does a *wholesale* prune in
            //     its own transaction, with no survivor set in scope.
            // The two can't share an impl because this one must run on the
            // already-open `tx` rather than opening its own connection.
            //
            // An empty `live_ids` with `Some(prune_file)` degrades to a full
            // DELETE of the file — the same edge-case contract as
            // `delete_phantom_chunks` → `delete_by_origin`.
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
                        let mut stmt = sqlx::query(sqlx::AssertSqlSafe(insert_sql.as_str()));
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
                    // `delete_phantom_chunks`. The parse-driven writer
                    // (`upsert_function_calls_for_files`) DELETE-then-INSERTs
                    // the current set for the file BEFORE this prune runs, so
                    // adding a DELETE here would wipe those just-written rows.
                    // The `delete_by_origin` / `prune_missing` paths (file
                    // fully removed, no upsert follows) DO include that DELETE.
                }
            }

            // Fold the file-level `function_calls` write into the same tx
            // as chunks/FTS/calls so a crash here can't leave the tables in
            // an asymmetric state where the call graph knows about a function
            // the chunks/FTS index doesn't.
            if let (Some(file), Some(fcs)) = (prune_file, file_function_calls) {
                let file_str = crate::normalize_path(file);
                crate::store::calls::write_function_calls_in_tx(&mut tx, &file_str, fcs).await?;
            }

            // v32: fold the file-level `candidate_edges` write into the same tx,
            // right after function_calls. Both are file-keyed call-graph tables
            // refreshed wholesale per file; committing them together keeps them
            // from drifting into an asymmetric state. candidate_edges is never
            // joined by a graph query, so it cannot surface as a false caller.
            if let (Some(file), Some(cands)) = (prune_file, file_candidate_edges) {
                let file_str = crate::normalize_path(file);
                crate::store::calls::write_candidate_edges_in_tx(&mut tx, &file_str, cands).await?;
            }

            // #1835: stamp the reconcile fingerprint LAST and INSIDE the same tx
            // (chunk-row columns + `file_registry` shadow). Because every prior
            // write — chunks, FTS, calls, function_calls, prune — and this stamp
            // commit together, a file is NEVER left with content but no stamp
            // (skip-forever) NOR a stamp but stale content (false-DEAD); a
            // rolled-back tx reverts to the prior coherent state and the file
            // re-indexes next run. For a zero-chunk file the chunk-row UPDATE
            // touches nothing and the registry UPSERT is the load-bearing write,
            // so `function_calls` for a zero-chunk origin and its registry row
            // commit atomically (no transient orphan window).
            if let (Some(file), Some(fp)) = (prune_file, fingerprint) {
                set_fingerprint_in_tx(&mut tx, &crate::normalize_path(file), fp).await?;
            }

            tx.commit().await?;
            Ok(chunks.len())
        })
    }

    /// Delete chunks for a file that are no longer in the current parse output.
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
                let mut stmt = sqlx::query(sqlx::AssertSqlSafe(insert_sql.as_str()));
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

    /// Prune phantom chunks for many files in ONE write transaction.
    ///
    /// Batched form of [`Self::delete_phantom_chunks`] for the bulk pipeline's
    /// post-loop prune pass: the per-file variant opens a transaction per
    /// origin, which on a multi-thousand-file reindex is thousands of
    /// BEGIN/COMMIT round-trips of pure overhead. Same temp-table pattern
    /// per file, single commit for the whole sweep.
    ///
    /// An entry with empty `live_ids` deletes every chunk for that origin
    /// (FTS + chunks). This primitive touches `chunks` and `chunks_fts` ONLY —
    /// it makes NO call-graph decision. `function_calls` is keyed by source
    /// file, not chunk presence: a file whose single function exceeds
    /// `CQS_PARSER_MAX_CHUNK_BYTES` parses to ZERO chunks but a NON-EMPTY call
    /// set (Pass 1 drops the oversize chunk; Pass 2 emits file-level calls with
    /// no size gate). "Zero chunks" is therefore not "zero calls", so this
    /// chunk-frame primitive must never gate the call-graph table — doing so
    /// destroys legitimate oversize-function edges. `function_calls` is replaced
    /// per-file from the freshly parsed set by exactly one writer
    /// (`replace_function_calls` / `upsert_function_calls_for_files` /
    /// `write_function_calls_in_tx`), driven by parse-completion, on EVERY
    /// reindex of the file (empty set → clears, non-empty → refreshes).
    ///
    /// Returns the total number of chunk rows deleted across all files.
    pub fn delete_phantom_chunks_batch(
        &self,
        files: &[(&std::path::Path, Vec<&str>)],
    ) -> Result<u32, StoreError> {
        let _span =
            tracing::info_span!("delete_phantom_chunks_batch", files = files.len()).entered();
        if files.is_empty() {
            return Ok(0);
        }

        self.rt.block_on(async {
            let (_guard, mut tx) = self.begin_write().await?;

            // Temp table created once, cleared per file. Same SQLite
            // 999-parameter-limit rationale as `delete_phantom_chunks`.
            sqlx::query("CREATE TEMP TABLE IF NOT EXISTS _live_ids (id TEXT PRIMARY KEY)")
                .execute(&mut *tx)
                .await?;

            let mut deleted_total = 0u32;
            for (file, live_ids) in files {
                let origin_str = crate::normalize_path(file);

                if live_ids.is_empty() {
                    // Whole file emptied — delete every chunk for the origin.
                    sqlx::query(
                        "DELETE FROM chunks_fts WHERE id IN \
                         (SELECT id FROM chunks WHERE origin = ?1)",
                    )
                    .bind(&origin_str)
                    .execute(&mut *tx)
                    .await?;
                    let result = sqlx::query("DELETE FROM chunks WHERE origin = ?1")
                        .bind(&origin_str)
                        .execute(&mut *tx)
                        .await?;
                    // Chunks + FTS only. This primitive makes NO call-graph
                    // decision: `function_calls` is replaced per-file from the
                    // parsed set by the single parse-driven writer, never gated
                    // on chunk count (an oversize-function file parses to zero
                    // chunks but a non-empty call set, so a DELETE here would
                    // destroy legitimate edges). No `file_registry` DELETE
                    // either: zero-chunk finalization stamps the registry right
                    // after this prune.
                    deleted_total += result.rows_affected() as u32;
                    continue;
                }

                sqlx::query("DELETE FROM _live_ids")
                    .execute(&mut *tx)
                    .await?;
                for batch in live_ids.chunks(crate::store::helpers::sql::max_rows_per_statement(1))
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
                    let mut stmt = sqlx::query(sqlx::AssertSqlSafe(insert_sql.as_str()));
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
                let deleted = result.rows_affected() as u32;
                if deleted > 0 {
                    tracing::info!(
                        origin = %origin_str,
                        deleted,
                        "Removed phantom chunks (batched tx)"
                    );
                }
                deleted_total += deleted;
            }

            tx.commit().await?;
            Ok(deleted_total)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_utils::make_chunk;
    use crate::parser::{CallEdgeKind, CallSite, CandidateSite, FunctionCalls};
    use crate::test_helpers::{mock_embedding, setup_store};

    /// `upsert_chunks_calls_and_prune_with_file_calls` must write
    /// chunks/FTS/calls AND function_calls in the same SQLite transaction.
    /// Pins the invariant: after a successful upsert, BOTH the chunks/FTS
    /// shadow AND the function_calls table reflect the new function. Without
    /// atomicity, a daemon crash mid-embed can leave function_calls ahead of
    /// chunks/FTS — search-by-name returns 0 hits for the new function while
    /// `cqs callers <new_fn>` works.
    #[test]
    fn test_upsert_chunks_calls_and_prune_with_file_calls_atomic() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);

        let chunk = make_chunk("new_fn", "src/m.rs");
        let pairs = [(chunk.clone(), emb.clone())];

        // file-level function_calls: new_fn calls callee_alpha
        let fcs = vec![FunctionCalls {
            name: "new_fn".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "callee_alpha".to_string(),
                line_number: 2,
                kind: CallEdgeKind::Call,
            }],
        }];

        store
            .upsert_chunks_calls_and_prune_with_file_calls(
                &pairs,
                Some(123),
                &[],
                Some(std::path::Path::new("src/m.rs")),
                &[chunk.id.as_str()],
                Some(&fcs),
                None,
            )
            .expect("upsert with file_calls must succeed");

        // chunks table has the new function
        let stored = store.get_chunks_by_ids(&[chunk.id.as_str()]).unwrap();
        assert_eq!(stored.len(), 1, "chunk row must be present");
        assert_eq!(stored.get(&chunk.id).unwrap().name, "new_fn");

        // FTS shadow has the new function (this is the table that backs
        // `search_by_name`)
        let fts_hits = store
            .search_by_name("new_fn", 5)
            .expect("search_by_name must succeed");
        assert!(
            fts_hits.iter().any(|h| h.chunk.name == "new_fn"),
            "search_by_name must find new_fn after upsert: got {:?}",
            fts_hits.iter().map(|h| &h.chunk.name).collect::<Vec<_>>()
        );

        // function_calls table has the new caller (this is the call-graph
        // table; it and the FTS shadow must land in the same transaction)
        let callers = store
            .get_callers_full("callee_alpha")
            .expect("get_callers_full");
        assert!(
            callers.iter().any(|c| c.name == "new_fn"),
            "function_calls must record new_fn → callee_alpha: got {:?}",
            callers.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    /// #1835 `upsert_file_fused`: the bulk per-file fused write stamps the
    /// reconcile fingerprint (chunk-row columns + `file_registry` shadow) INSIDE
    /// the same transaction as chunks/FTS/calls/function_calls/prune. After a
    /// successful call the staleness reader resolves the file as stamped.
    #[test]
    fn upsert_file_fused_stamps_fingerprint_in_tx() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let chunk = make_chunk("fused_fn", "src/f.rs");
        let pairs = [(chunk.clone(), emb)];
        let fp = crate::store::chunks::staleness::FileFingerprint {
            mtime: Some(42),
            size: Some(7),
            content_hash: Some(*blake3::hash(b"fused").as_bytes()),
        };
        store
            .upsert_file_fused(
                &pairs,
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("src/f.rs"),
                &[chunk.id.as_str()],
                &[],
                &[],
                &fp,
            )
            .expect("fused write must succeed");

        let fps = store.fingerprints_for_origins(&["src/f.rs"]).unwrap();
        let got = fps.get("src/f.rs").expect("origin stamped");
        assert!(
            got.content_hash.is_some() && got.size.is_some() && got.mtime.is_some(),
            "fused write must stamp the full fingerprint in-tx; got {got:?}"
        );
    }

    /// #1835 `upsert_file_fused` zero-chunk path: empty chunks + empty live set
    /// (prune) + function_calls + registry stamp commit atomically. The file has
    /// NO chunk rows but IS stamped in `file_registry`, and its function_calls
    /// are not orphaned (the registry row makes the origin "known").
    #[test]
    fn upsert_file_fused_zero_chunk_stamps_registry_and_keeps_calls_coherent() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        // Seed a prior chunk so the zero-chunk prune has something to remove.
        let seed = make_chunk("old_fn", "src/z.rs");
        store
            .upsert_chunks_batch(&[(seed.clone(), emb)], Some(1))
            .unwrap();
        assert_eq!(store.get_chunks_by_origin("src/z.rs").unwrap().len(), 1);

        // Oversize-function class: zero chunks but a function_calls edge.
        let fcs = vec![FunctionCalls {
            name: "oversize".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "callee".to_string(),
                line_number: 2,
                kind: CallEdgeKind::Call,
            }],
        }];
        let fp = crate::store::chunks::staleness::FileFingerprint {
            mtime: Some(99),
            size: Some(3),
            content_hash: Some(*blake3::hash(b"zero").as_bytes()),
        };
        store
            .upsert_file_fused(
                &[],
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("src/z.rs"),
                &[], // empty live set → prune all chunks for the origin
                &fcs,
                &[],
                &fp,
            )
            .expect("zero-chunk fused write must succeed");

        // Chunks pruned to zero...
        assert_eq!(
            store.get_chunks_by_origin("src/z.rs").unwrap().len(),
            0,
            "zero-chunk fused write prunes the prior chunk"
        );
        // ...but the registry stamp persists (staleness reader resolves it)...
        let fps = store.fingerprints_for_origins(&["src/z.rs"]).unwrap();
        let got = fps.get("src/z.rs").expect("registry stamped");
        assert!(
            got.content_hash.is_some() && got.size.is_some(),
            "zero-chunk fused write must stamp the registry; got {got:?}"
        );
        // ...and the function_calls are NOT orphaned (registry makes it known).
        assert!(
            store.find_orphaned_function_calls().unwrap().is_empty(),
            "zero-chunk fused write must not leave orphaned function_calls"
        );
    }

    /// v32 `candidate_edges`: the fused write folds the candidate side-table
    /// into the same per-file tx and round-trips a row (file/callee_name/
    /// ref_line/candidate_kind). The candidate is written to its OWN table — it
    /// must NOT appear in `function_calls`, so `get_callers_full` for the same
    /// callee name returns nothing (the side-table is never joined by the graph
    /// queries, so a candidate can never surface as a false caller).
    #[test]
    fn upsert_file_fused_writes_candidate_edges_separate_from_callers() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let chunk = make_chunk("real_fn", "src/c.rs");
        let pairs = [(chunk.clone(), emb)];
        let fp = crate::store::chunks::staleness::FileFingerprint {
            mtime: Some(7),
            size: Some(11),
            content_hash: Some(*blake3::hash(b"cand").as_bytes()),
        };

        let candidates = vec![CandidateSite {
            file: std::path::PathBuf::from("src/c.rs"),
            callee_name: "maybe_callee".to_string(),
            ref_line: 9,
            candidate_kind: "unresolved".to_string(),
        }];

        store
            .upsert_file_fused(
                &pairs,
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("src/c.rs"),
                &[chunk.id.as_str()],
                &[],
                &candidates,
                &fp,
            )
            .expect("fused write with candidate edges must succeed");

        // The candidate row round-trips from candidate_edges.
        store.runtime().block_on(async {
            let rows: Vec<(String, String, i64, String)> = sqlx::query_as(
                "SELECT file, callee_name, ref_line, candidate_kind \
                 FROM candidate_edges WHERE callee_name = ?1",
            )
            .bind("maybe_callee")
            .fetch_all(&store.pool)
            .await
            .unwrap();
            assert_eq!(rows.len(), 1, "exactly one candidate row must be stored");
            assert_eq!(rows[0].0, "src/c.rs");
            assert_eq!(rows[0].1, "maybe_callee");
            assert_eq!(rows[0].2, 9);
            assert_eq!(rows[0].3, "unresolved");
        });

        // The candidate must NOT leak into the caller graph — get_callers_full
        // for the candidate's callee name finds nothing (it lives in a table the
        // graph queries never join).
        let callers = store
            .get_callers_full("maybe_callee")
            .expect("get_callers_full");
        assert!(
            callers.is_empty(),
            "a candidate edge must never surface as a caller; got {:?}",
            callers.iter().map(|c| &c.name).collect::<Vec<_>>()
        );

        // Re-running the fused write with an EMPTY candidate slice clears the
        // file's candidates (DELETE-then-INSERT wholesale per file).
        store
            .upsert_file_fused(
                &pairs,
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("src/c.rs"),
                &[chunk.id.as_str()],
                &[],
                &[],
                &fp,
            )
            .expect("fused rewrite with no candidates must succeed");
        store.runtime().block_on(async {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM candidate_edges WHERE file = ?1")
                    .bind("src/c.rs")
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
            assert_eq!(
                n, 0,
                "empty candidate slice must clear the file's candidates"
            );
        });
    }

    /// Lane-2 END-TO-END invariant: a real parser-emitted candidate (a
    /// cross-file bare fn-pointer arg, #1818) persisted via the fused write
    /// lands in `candidate_edges` but NEVER surfaces in `get_callers_full` for
    /// the candidate's callee — closing the loop from parser emit (Lane 2)
    /// through the side-table to the Lane-1 caller-graph isolation invariant.
    #[test]
    fn parser_emitted_candidate_never_pollutes_callers() {
        use crate::parser::Parser;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("install.rs");
        std::fs::write(
            &src,
            "fn install() {\n    set_handler(imported_handler);\n}\n",
        )
        .unwrap();

        let parser = Parser::new().unwrap();
        let (function_calls, _types, candidates) = parser
            .parse_file_relationships_with_candidates(&src)
            .expect("parse must succeed");

        // The parser produced exactly the cross-file bare-arg candidate.
        assert!(
            candidates
                .iter()
                .any(|c| c.callee_name == "imported_handler"
                    && c.candidate_kind == "bare_arg_unresolved"),
            "parser must emit imported_handler as a candidate, got {:?}",
            candidates
                .iter()
                .map(|c| (&c.callee_name, &c.candidate_kind))
                .collect::<Vec<_>>()
        );

        // Persist via the fused write, keyed on a relative origin.
        let (store, _store_dir) = setup_store();
        let emb = mock_embedding(1.0);
        let chunk = make_chunk("install", "install.rs");
        let pairs = [(chunk.clone(), emb)];
        let fp = crate::store::chunks::staleness::FileFingerprint {
            mtime: Some(3),
            size: Some(4),
            content_hash: Some(*blake3::hash(b"install").as_bytes()),
        };
        store
            .upsert_file_fused(
                &pairs,
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("install.rs"),
                &[chunk.id.as_str()],
                &function_calls,
                &candidates,
                &fp,
            )
            .expect("fused write of parser-emitted candidate must succeed");

        // The candidate is in candidate_edges...
        store.runtime().block_on(async {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM candidate_edges WHERE callee_name = 'imported_handler'",
            )
            .fetch_one(&store.pool)
            .await
            .unwrap();
            assert_eq!(n, 1, "candidate must be persisted");
        });

        // ...but it must NOT be a caller of imported_handler (Lane-1 invariant).
        let callers = store
            .get_callers_full("imported_handler")
            .expect("get_callers_full");
        assert!(
            callers.is_empty(),
            "a parser-emitted candidate must never surface as a caller; got {:?}",
            callers.iter().map(|c| &c.name).collect::<Vec<_>>()
        );
    }

    /// v32 COMPLETENESS GUARD: every wholesale `function_calls` replace/delete
    /// path must also sweep `candidate_edges` for the same files — both are
    /// file-keyed call-graph tables refreshed wholesale per file, and a path
    /// that refreshes one but not the other strands orphan rows. This pins the
    /// parse-driven batch writer `upsert_function_calls_for_files` (the watch
    /// zero-chunk finalize path): it does NOT receive candidate sites, so the
    /// symmetric action is to CLEAR the files' candidates. Seed a candidate
    /// directly, then run the batch writer for the same file and assert the
    /// candidate is gone.
    #[test]
    fn upsert_function_calls_for_files_clears_candidate_edges() {
        let (store, _dir) = setup_store();

        // Seed a candidate row for src/q.rs via the fused write (the only
        // store-public path that writes candidate_edges in Lane 1).
        let emb = mock_embedding(1.0);
        let chunk = make_chunk("seed_fn", "src/q.rs");
        let pairs = [(chunk.clone(), emb)];
        let fp = crate::store::chunks::staleness::FileFingerprint {
            mtime: Some(1),
            size: Some(2),
            content_hash: Some(*blake3::hash(b"q").as_bytes()),
        };
        let candidates = vec![CandidateSite {
            file: std::path::PathBuf::from("src/q.rs"),
            callee_name: "ghost_callee".to_string(),
            ref_line: 4,
            candidate_kind: "unresolved".to_string(),
        }];
        store
            .upsert_file_fused(
                &pairs,
                &[],
                fp.mtime,
                &[],
                std::path::Path::new("src/q.rs"),
                &[chunk.id.as_str()],
                &[],
                &candidates,
                &fp,
            )
            .expect("seed fused write must succeed");

        // Sanity: the candidate exists.
        store.runtime().block_on(async {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM candidate_edges WHERE file = ?1")
                    .bind("src/q.rs")
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
            assert_eq!(n, 1, "candidate must be seeded before the batch replace");
        });

        // Run the parse-driven batch function_calls replace for the same file
        // (the watch zero-chunk finalize path). It must clear the candidates.
        let entries = vec![(
            std::path::PathBuf::from("src/q.rs"),
            vec![FunctionCalls {
                name: "seed_fn".to_string(),
                line_start: 1,
                calls: vec![CallSite {
                    callee_name: "real_callee".to_string(),
                    line_number: 2,
                    kind: CallEdgeKind::Call,
                }],
            }],
        )];
        store
            .upsert_function_calls_for_files(&entries)
            .expect("batch function_calls replace must succeed");

        store.runtime().block_on(async {
            let (n,): (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM candidate_edges WHERE file = ?1")
                    .bind("src/q.rs")
                    .fetch_one(&store.pool)
                    .await
                    .unwrap();
            assert_eq!(
                n, 0,
                "upsert_function_calls_for_files must clear the file's candidate_edges \
                 (wholesale call-graph replace must sweep both file-keyed tables)"
            );
        });
    }

    /// Passing `None` for file_function_calls leaves the existing
    /// function_calls rows untouched. Pin so a future refactor that defaults
    /// to "always write" doesn't silently wipe call-graph state for callers
    /// using this method.
    #[test]
    fn test_upsert_chunks_calls_and_prune_none_leaves_function_calls() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);

        // Seed function_calls via the standalone API
        let seed_fcs = vec![FunctionCalls {
            name: "seeded_fn".to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: "seeded_callee".to_string(),
                line_number: 2,
                kind: CallEdgeKind::Call,
            }],
        }];
        store
            .upsert_function_calls(std::path::Path::new("src/m.rs"), &seed_fcs)
            .expect("seed function_calls");

        // Now run the legacy upsert (None for file_function_calls)
        let chunk = make_chunk("other_fn", "src/m.rs");
        let pairs = [(chunk.clone(), emb)];
        store
            .upsert_chunks_calls_and_prune(
                &pairs,
                Some(123),
                &[],
                Some(std::path::Path::new("src/m.rs")),
                &[chunk.id.as_str()],
            )
            .expect("legacy upsert must succeed");

        // Seeded function_calls row must still be present
        let callers = store.get_callers_full("seeded_callee").unwrap();
        assert!(
            callers.iter().any(|c| c.name == "seeded_fn"),
            "legacy upsert path must NOT wipe pre-existing function_calls"
        );
    }

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

    // ===== needs_embedding flag round-trip =====

    /// `upsert_embedded_batch`'s sentinel mode writes chunks with
    /// `needs_embedding=1` and a zero-vec sentinel in the `embedding`
    /// column. `needs_embedding_count` reports them; `needs_embedding_ids`
    /// returns their IDs.
    #[test]
    fn upsert_unembedded_marks_needs_embedding_and_zero_vec() {
        let (store, _dir) = setup_store();
        let c1 = make_chunk("alpha", "src/a.rs");
        let c2 = make_chunk("beta", "src/b.rs");

        let count = store
            .upsert_embedded_batch(
                &[],
                &[c1.clone(), c2.clone()],
                &std::collections::HashMap::new(),
            )
            .unwrap();
        assert_eq!(count, 2);

        // Both chunks visible to the count query.
        assert_eq!(store.needs_embedding_count().unwrap(), 2);
        let ids = store.needs_embedding_ids().unwrap();
        assert!(ids.contains(&c1.id));
        assert!(ids.contains(&c2.id));

        // The on-disk embedding is the zero-vec sentinel. Read the raw
        // bytes directly — the by-hash lookups gate on `needs_embedding = 0`
        // so sentinels are invisible through them by design.
        let blobs: Vec<(Vec<u8>,)> = store.rt.block_on(async {
            sqlx::query_as("SELECT embedding FROM chunks WHERE content_hash IN (?1, ?2)")
                .bind(&c1.content_hash)
                .bind(&c2.content_hash)
                .fetch_all(&store.pool)
                .await
                .unwrap()
        });
        assert_eq!(blobs.len(), 2);
        for (bytes,) in &blobs {
            let floats: &[f32] = bytemuck::cast_slice(bytes);
            assert!(
                floats.iter().all(|&x| x == 0.0),
                "unembedded chunks must carry a zero-vec sentinel"
            );
        }

        // The gated by-hash lookup must NOT serve the sentinel as a reuse hit.
        let embs = store
            .get_embeddings_by_hashes(&[&c1.content_hash, &c2.content_hash])
            .unwrap();
        assert!(
            embs.is_empty(),
            "needs_embedding=1 sentinels must be invisible to by-hash embedding lookup"
        );
    }

    /// `update_embeddings_with_hashes_batch` (used by `enrichment_pass`)
    /// clears `needs_embedding=0` on every row it writes, so the next
    /// HNSW build / search picks up the chunk.
    #[test]
    fn enrichment_update_clears_needs_embedding() {
        let (store, _dir) = setup_store();
        let c = make_chunk("alpha", "src/a.rs");

        store
            .upsert_embedded_batch(
                &[],
                std::slice::from_ref(&c),
                &std::collections::HashMap::new(),
            )
            .unwrap();
        assert_eq!(store.needs_embedding_count().unwrap(), 1);

        // Land a real embedding (mimics enrichment_pass.flush_enrichment_batch).
        let real_emb = mock_embedding(0.5);
        let updates = vec![(c.id.clone(), real_emb, Some("hash".to_string()))];
        store.update_embeddings_with_hashes_batch(&updates).unwrap();

        // Flag cleared, count zero, ID gone from the set.
        assert_eq!(store.needs_embedding_count().unwrap(), 0);
        assert!(store.needs_embedding_ids().unwrap().is_empty());
    }

    /// Embedding rewrites must advance the store write generation
    /// (`splade_generation`). The HNSW sidecar stamp uses the counter to
    /// prove a sidecar postdates the last vector-affecting write; an
    /// enrichment pass that rewrote vectors without moving the counter
    /// would let the dirty-flag self-heal serve pre-enrichment vectors
    /// after a crash between the enrichment commit and the HNSW save.
    #[test]
    fn enrichment_update_bumps_splade_generation() {
        let (store, _dir) = setup_store();
        let c = make_chunk("alpha", "src/a.rs");
        store
            .upsert_embedded_batch(
                &[],
                std::slice::from_ref(&c),
                &std::collections::HashMap::new(),
            )
            .unwrap();

        let before = store.splade_generation().unwrap();
        let updates = vec![(c.id.clone(), mock_embedding(0.5), Some("hash".to_string()))];
        store.update_embeddings_with_hashes_batch(&updates).unwrap();
        let after = store.splade_generation().unwrap();
        assert!(
            after > before,
            "embedding update must bump splade_generation (before={before}, after={after})"
        );
    }

    /// First-pass-skip → enrichment-clear contract: search and HNSW build
    /// both filter `needs_embedding=0`, so an unembedded chunk is invisible
    /// from FTS-name search until enrichment lands its real vector.
    #[test]
    fn unembedded_chunks_invisible_from_name_search() {
        let (store, _dir) = setup_store();
        let c = make_chunk("alpha_function", "src/a.rs");

        store
            .upsert_embedded_batch(
                &[],
                std::slice::from_ref(&c),
                &std::collections::HashMap::new(),
            )
            .unwrap();

        // Name search must NOT find the unembedded chunk.
        let hits = store.search_by_name("alpha_function", 10).unwrap();
        assert!(
            hits.is_empty(),
            "unembedded chunk must be invisible from name search; got {} hit(s)",
            hits.len()
        );

        // After enrichment, the chunk surfaces.
        let real_emb = mock_embedding(0.5);
        let updates = vec![(c.id.clone(), real_emb, Some("hash".to_string()))];
        store.update_embeddings_with_hashes_batch(&updates).unwrap();
        let hits_post = store.search_by_name("alpha_function", 10).unwrap();
        assert_eq!(
            hits_post.len(),
            1,
            "after enrichment, the chunk must surface from name search"
        );
    }

    /// Skip-first-pass writes `embedding_base = NULL` (not the zero-vec
    /// sentinel). Otherwise `build_hnsw_base_index`
    /// (`SELECT ... WHERE embedding_base IS NOT NULL`) would join the
    /// base HNSW with corrupt zeros for every partial-state chunk — the
    /// base index is the routing-fallback channel and silently degrading
    /// it would break conceptual / behavioral / negation routing.
    ///
    /// `update_embeddings_with_hashes_batch` repopulates `embedding_base`
    /// (when previously NULL) using
    /// `COALESCE(chunks.embedding_base, t.embedding)`, so the first
    /// enrichment hit fills the base bytes and the chunk becomes routable
    /// on the DenseBase path.
    #[test]
    fn unembedded_chunks_have_null_embedding_base() {
        let (store, _dir) = setup_store();
        let c = make_chunk("alpha", "src/a.rs");

        store
            .upsert_embedded_batch(
                &[],
                std::slice::from_ref(&c),
                &std::collections::HashMap::new(),
            )
            .unwrap();

        // Pre-enrichment: embedding_base IS NULL (not zero-vec). The base
        // index count query (`base_embedding_count`) drops these chunks.
        assert_eq!(
            store.base_embedding_count().unwrap(),
            0,
            "skip-first-pass chunks must be invisible to base_embedding_count"
        );

        // Post-enrichment: enrichment writes the real `embedding` AND
        // repopulates the previously-NULL `embedding_base`. The base index
        // now sees the chunk and DenseBase routing serves it correctly.
        let real_emb = mock_embedding(0.5);
        let updates = vec![(c.id.clone(), real_emb, Some("hash".to_string()))];
        store.update_embeddings_with_hashes_batch(&updates).unwrap();
        assert_eq!(
            store.base_embedding_count().unwrap(),
            1,
            "DS-V1.38-2: post-enrichment, base_embedding_count must be 1 — \
             enrichment refreshes `embedding` AND repopulates a previously-NULL \
             `embedding_base` so base-HNSW coverage closes for `--llm-summaries` chunks."
        );
    }

    // The "preserve existing embedding_base" invariant is covered by
    // `test_enrichment_does_not_overwrite_base` in
    // `src/store/chunks/async_helpers.rs`. The above test
    // (`unembedded_chunks_have_null_embedding_base`) covers the
    // sibling NULL → COALESCE → repopulate case.

    /// End-to-end vendored-flag round-trip. With the default
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

    /// An empty prefix list (operator opt-out via
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

    // ===== touch_source_mtime =====

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

    /// `set_file_fingerprint` round-trips mtime+size+hash so the next
    /// `indexed_file_origins()` read sees a fully-populated `FileFingerprint`.
    /// Before the call the rows have NULL size/hash because the upsert path
    /// doesn't bind those columns; the helper upgrades them in place.
    #[test]
    fn test_set_file_fingerprint_round_trips_all_three_fields() {
        use crate::store::chunks::staleness::FileFingerprint;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let chunk = make_chunk("alpha", "src/alpha.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        // Pre-state: only mtime set, size/hash columns NULL.
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

    /// Separator normalization mirrors `touch_source_mtime`. A Windows-style
    /// backslash origin must round-trip through `normalize_path` so the
    /// UPDATE matches the slash-form indexer key. Without this the
    /// fingerprint columns silently stay NULL on Windows tools that emit
    /// `\\` separators.
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

    // ===== LLM summary functions =====

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

    // ===== purpose coexistence =====

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

    // ===== delete_phantom_chunks tests =====

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

    // ===== upsert_embedded_batch tests =====

    fn full_fp(mtime: i64, size: u64, bytes: &[u8]) -> crate::store::FileFingerprint {
        crate::store::FileFingerprint {
            mtime: Some(mtime),
            size: Some(size),
            content_hash: Some(*blake3::hash(bytes).as_bytes()),
        }
    }

    /// The bulk-pipeline write path must stamp the full reconcile
    /// fingerprint (mtime + size + BLAKE3) for every file in the batch, in
    /// the same call as the chunk upsert. This is the CLI-path half of the
    /// fingerprint contract — previously only the watch daemon stamped
    /// these columns and CLI-indexed rows fell back to mtime-only
    /// staleness on coarse-mtime filesystems.
    #[test]
    fn upsert_embedded_batch_stamps_fingerprints() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let c1 = make_chunk("alpha", "src/a.rs");
        let c2 = make_chunk("beta", "src/b.rs");
        let emb = mock_embedding(1.0);

        let fp_a = full_fp(1_000, 42, b"a-bytes");
        let fp_b = full_fp(2_000, 7, b"b-bytes");
        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("src/a.rs"), fp_a.clone());
        fps.insert(PathBuf::from("src/b.rs"), fp_b.clone());

        let n = store
            .upsert_embedded_batch(&[(c1.clone(), emb.clone()), (c2.clone(), emb)], &[], &fps)
            .unwrap();
        assert_eq!(n, 2);

        // Read the columns back through the reconcile read path.
        let map = store.indexed_file_origins().unwrap();
        let stored_a = map.get("src/a.rs").expect("origin a present");
        let stored_b = map.get("src/b.rs").expect("origin b present");
        assert_eq!(stored_a, &fp_a, "size+hash must be non-NULL and correct");
        assert_eq!(stored_b, &fp_b, "size+hash must be non-NULL and correct");

        // FTS landed in the same transaction.
        let hits = store.search_by_name("alpha", 5).unwrap();
        assert!(hits.iter().any(|h| h.chunk.name == "alpha"));
    }

    /// A content change through `upsert_embedded_batch` must replace the
    /// stored fingerprint — a stored `source_content_hash` may never
    /// describe a previous content version. Also covers the
    /// `ON CONFLICT … WHERE` short-circuit: a re-upsert with UNCHANGED
    /// content (mtime bump only) skips the row UPDATE, but the fingerprint
    /// stamp must still land.
    #[test]
    fn upsert_embedded_batch_refreshes_fingerprint_on_change_and_on_skip() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let mut chunk = make_chunk("evolving", "src/e.rs");
        let file = PathBuf::from("src/e.rs");

        let fp_v1 = full_fp(1_000, 10, b"v1");
        let mut fps = HashMap::new();
        fps.insert(file.clone(), fp_v1.clone());
        store
            .upsert_embedded_batch(&[(chunk.clone(), mock_embedding(0.1))], &[], &fps)
            .unwrap();
        assert_eq!(
            store.indexed_file_origins().unwrap().get("src/e.rs"),
            Some(&fp_v1)
        );

        // Content changed → row updates AND fingerprint replaced.
        chunk.content = "fn evolving() { /* changed */ }".to_string();
        chunk.content_hash = "new-hash-v2".to_string();
        let fp_v2 = full_fp(2_000, 31, b"v2");
        fps.insert(file.clone(), fp_v2.clone());
        store
            .upsert_embedded_batch(&[(chunk.clone(), mock_embedding(0.2))], &[], &fps)
            .unwrap();
        assert_eq!(
            store.indexed_file_origins().unwrap().get("src/e.rs"),
            Some(&fp_v2),
            "content change must replace the stored fingerprint"
        );

        // Content unchanged, mtime bumped → ON CONFLICT WHERE skips the row
        // UPDATE, but the fingerprint stamp still refreshes all three
        // columns.
        let fp_v3 = full_fp(3_000, 31, b"v2");
        fps.insert(file.clone(), fp_v3.clone());
        store
            .upsert_embedded_batch(&[(chunk.clone(), mock_embedding(0.2))], &[], &fps)
            .unwrap();
        assert_eq!(
            store.indexed_file_origins().unwrap().get("src/e.rs"),
            Some(&fp_v3),
            "fingerprint must refresh even when the ON CONFLICT WHERE skips the row"
        );
    }

    /// One call writes real-embedding chunks AND zero-vec sentinel chunks
    /// (skip-first-pass under `--llm-summaries`) with the correct
    /// `needs_embedding` flags, in a single transaction, with fingerprints
    /// for both files. Pins the sentinel contract the reuse resolver and
    /// `enrichment_pass` depend on.
    #[test]
    fn upsert_embedded_batch_mixed_real_and_sentinel() {
        use std::collections::HashMap;
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let c_real = make_chunk("real_fn", "src/r.rs");
        let c_sent = make_chunk("sent_fn", "src/s.rs");

        let mut fps = HashMap::new();
        fps.insert(PathBuf::from("src/r.rs"), full_fp(100, 1, b"r"));
        fps.insert(PathBuf::from("src/s.rs"), full_fp(200, 2, b"s"));

        let n = store
            .upsert_embedded_batch(
                &[(c_real.clone(), mock_embedding(1.0))],
                std::slice::from_ref(&c_sent),
                &fps,
            )
            .unwrap();
        assert_eq!(n, 2);

        // Sentinel chunk flagged needs_embedding=1; real chunk not.
        assert_eq!(store.needs_embedding_count().unwrap(), 1);
        let ids = store.needs_embedding_ids().unwrap();
        assert!(ids.contains(&c_sent.id));
        assert!(!ids.contains(&c_real.id));

        // Sentinel invisible to the gated by-hash reuse lookup; real visible.
        let embs = store
            .get_embeddings_by_hashes(&[&c_real.content_hash, &c_sent.content_hash])
            .unwrap();
        assert!(embs.contains_key(&c_real.content_hash));
        assert!(!embs.contains_key(&c_sent.content_hash));

        // Both files got fingerprints.
        let map = store.indexed_file_origins().unwrap();
        assert!(map.get("src/r.rs").unwrap().content_hash.is_some());
        assert!(map.get("src/s.rs").unwrap().content_hash.is_some());
    }

    /// Empty batch is a no-op.
    #[test]
    fn upsert_embedded_batch_empty_is_noop() {
        let (store, _dir) = setup_store();
        let n = store
            .upsert_embedded_batch(&[], &[], &std::collections::HashMap::new())
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(store.chunk_count().unwrap(), 0);
    }

    // ===== set_file_fingerprints_batch =====

    /// Batched fingerprint refresh writes all entries under one call and
    /// round-trips through `indexed_file_origins`. Used by the pipeline's
    /// staleness pre-filter for mtime-bumped content-identical files.
    #[test]
    fn set_file_fingerprints_batch_round_trips() {
        use std::path::PathBuf;
        let (store, _dir) = setup_store();
        let c1 = make_chunk("a", "src/a.rs");
        let c2 = make_chunk("b", "src/b.rs");
        store
            .upsert_chunks_batch(
                &[(c1, mock_embedding(1.0)), (c2, mock_embedding(2.0))],
                Some(100),
            )
            .unwrap();

        let entries = vec![
            (PathBuf::from("src/a.rs"), full_fp(1_111, 5, b"aa")),
            (PathBuf::from("src/b.rs"), full_fp(2_222, 6, b"bb")),
        ];
        let rows = store.set_file_fingerprints_batch(&entries).unwrap();
        assert_eq!(rows, 2, "one chunk row per file must be updated");

        let map = store.indexed_file_origins().unwrap();
        assert_eq!(map.get("src/a.rs"), Some(&entries[0].1));
        assert_eq!(map.get("src/b.rs"), Some(&entries[1].1));
    }

    // ===== delete_phantom_chunks_batch =====

    /// Batched prune removes phantoms across multiple files in one call
    /// (single transaction), leaves live chunks intact, and handles the
    /// empty-live-ids "file emptied" case inline.
    #[test]
    fn delete_phantom_chunks_batch_prunes_across_files() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let a1 = make_chunk("a1", "f1.rs");
        let a2 = make_chunk("a2", "f1.rs");
        let b1 = make_chunk("b1", "f2.rs");
        let b2 = make_chunk("b2", "f2.rs");
        let c1 = make_chunk("c1", "f3.rs");
        let a1_id = a1.id.clone();
        let b2_id = b2.id.clone();
        store
            .upsert_chunks_batch(
                &[
                    (a1, emb.clone()),
                    (a2, emb.clone()),
                    (b1, emb.clone()),
                    (b2, emb.clone()),
                    (c1, emb.clone()),
                ],
                Some(100),
            )
            .unwrap();

        let files: Vec<(&std::path::Path, Vec<&str>)> = vec![
            (std::path::Path::new("f1.rs"), vec![a1_id.as_str()]),
            (std::path::Path::new("f2.rs"), vec![b2_id.as_str()]),
            (std::path::Path::new("f3.rs"), vec![]), // file emptied
        ];
        let deleted = store.delete_phantom_chunks_batch(&files).unwrap();
        assert_eq!(deleted, 3, "a2 + b1 + c1 must be pruned");
        assert_eq!(store.chunk_count().unwrap(), 2);

        // FTS rows for pruned chunks are gone in the same transaction.
        assert!(store.search_by_name("a2", 5).unwrap().is_empty());
        assert!(store
            .search_by_name("a1", 5)
            .unwrap()
            .iter()
            .any(|h| h.chunk.name == "a1"));
    }

    /// Empty input is a no-op.
    #[test]
    fn delete_phantom_chunks_batch_empty_is_noop() {
        let (store, _dir) = setup_store();
        let deleted = store.delete_phantom_chunks_batch(&[]).unwrap();
        assert_eq!(deleted, 0);
    }

    /// The chunk-prune primitive must touch `function_calls` for NEITHER
    /// branch. "Zero chunks" is not "zero calls": an oversize-function file
    /// parses to zero chunks but a non-empty call set, so deleting
    /// `function_calls` from the empty branch would destroy legitimate edges.
    /// `function_calls` is owned by the single parse-driven writer; this
    /// primitive is chunks/FTS only. This pins the single-responsibility
    /// decouple — both the empty-live-set and partial-prune branches leave the
    /// call graph alone.
    #[test]
    fn delete_phantom_chunks_batch_leaves_function_calls_alone_both_branches() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let f1 = make_chunk("f1a", "emptied.rs");
        let f2 = make_chunk("f2a", "partial.rs");
        let f2b = make_chunk("f2b", "partial.rs");
        let f2_live = f2.id.clone();
        store
            .upsert_chunks_batch(
                &[(f1, emb.clone()), (f2, emb.clone()), (f2b, emb.clone())],
                Some(100),
            )
            .unwrap();

        // Both files are callers in the call graph.
        let mk = |caller: &str, callee: &str| FunctionCalls {
            name: caller.to_string(),
            line_start: 1,
            calls: vec![CallSite {
                callee_name: callee.to_string(),
                line_number: 1,
                kind: CallEdgeKind::Call,
            }],
        };
        store
            .upsert_function_calls(std::path::Path::new("emptied.rs"), &[mk("f1a", "x")])
            .unwrap();
        store
            .upsert_function_calls(std::path::Path::new("partial.rs"), &[mk("f2a", "y")])
            .unwrap();
        assert_eq!(store.get_callers_full("x").unwrap().len(), 1);
        assert_eq!(store.get_callers_full("y").unwrap().len(), 1);

        let files: Vec<(&std::path::Path, Vec<&str>)> = vec![
            (std::path::Path::new("emptied.rs"), vec![]), // whole file emptied
            (std::path::Path::new("partial.rs"), vec![f2_live.as_str()]), // partial prune
        ];
        store.delete_phantom_chunks_batch(&files).unwrap();

        // Chunks were pruned for both files...
        assert!(store.get_chunks_by_origin("emptied.rs").unwrap().is_empty());
        assert_eq!(store.get_chunks_by_origin("partial.rs").unwrap().len(), 1);
        // ...but the prune did NOT touch function_calls for either. Clearing
        // the emptied file's edges is the parse-driven writer's job (with the
        // file's freshly parsed call set), not the chunk primitive's — gating
        // on chunk count would destroy the oversize-function case.
        assert_eq!(
            store.get_callers_full("x").unwrap().len(),
            1,
            "empty-live-set branch must NOT touch function_calls"
        );
        assert_eq!(
            store.get_callers_full("y").unwrap().len(),
            1,
            "partial-prune branch must NOT touch function_calls"
        );
    }

    /// STANDING INVARIANT (the mechanization): `find_orphaned_function_calls`
    /// flags any `function_calls.file` absent from BOTH `chunks` and
    /// `file_registry`, and ONLY those. Three cases pinned in one test:
    ///   1. A file with chunks + an edge → coherent (not flagged).
    ///   2. An oversize-function file: ZERO chunks but a file_registry row and a
    ///      NON-empty call set → coherent (the `file_registry` shadow keeps it
    ///      from being flagged; gating on chunks alone is the bug this guards).
    ///   3. A deliberately-orphaned edge: function_calls row for a file with NO
    ///      presence in chunks OR file_registry → FLAGGED.
    /// Fail-before/pass-after: before the parse-driven decouple, the watch
    /// whole-batch-empty route left exactly the case-3 orphan; this check fires
    /// on it. After the decouple, only a genuinely-incoherent row trips it.
    #[test]
    fn find_orphaned_function_calls_flags_only_unknown_files() {
        use crate::store::chunks::staleness::FileFingerprint;
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);

        // Case 1: a normal chunk-bearing caller.
        let c = make_chunk("ok_fn", "ok.rs");
        store
            .upsert_chunks_batch(&[(c, emb.clone())], Some(100))
            .unwrap();
        let mk = |caller: &str, callee: &str, file: &str| {
            store
                .upsert_function_calls(
                    std::path::Path::new(file),
                    &[FunctionCalls {
                        name: caller.to_string(),
                        line_start: 1,
                        calls: vec![CallSite {
                            callee_name: callee.to_string(),
                            line_number: 1,
                            kind: CallEdgeKind::Call,
                        }],
                    }],
                )
                .unwrap();
        };
        mk("ok_fn", "callee1", "ok.rs");

        // Case 2: oversize-function file — NO chunks, but a file_registry row
        // (the v29 shadow) and a NON-empty call set. Must NOT be flagged.
        store
            .set_file_registry_fingerprints_batch(&[(
                std::path::PathBuf::from("big.rs"),
                FileFingerprint {
                    mtime: Some(1),
                    size: Some(200_000),
                    content_hash: None,
                },
            )])
            .unwrap();
        mk("big_fn", "helper", "big.rs");

        // Coherent so far: both files are known.
        assert!(
            store.find_orphaned_function_calls().unwrap().is_empty(),
            "chunk-bearing file AND registry-shadowed oversize file are both coherent"
        );

        // Case 3: a deliberately-orphaned edge — a function_calls row for a file
        // present in NEITHER chunks NOR file_registry.
        mk("ghost_fn", "ghost_callee", "ghost.rs");
        let orphans = store.find_orphaned_function_calls().unwrap();
        assert_eq!(
            orphans,
            vec!["ghost.rs".to_string()],
            "only the file absent from chunks+file_registry is flagged (not ok.rs, not big.rs)"
        );
    }

    // ===== update_umap_coords_batch happy path =====

    /// Seed chunks, call `update_umap_coords_batch` with finite values,
    /// verify the rows are written and `umap_x`/`umap_y` round-trip via raw
    /// SELECT (matching how `build_cluster` reads them in `serve/data.rs`).
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

    /// Passing an extra unknown ID — the warn fires + `updated` is < input
    /// length. Documents the partial-update path the function uses when the
    /// projection script's input drifts.
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

    // ===== update_umap_coords_batch NaN/Inf handling =====

    /// Pins current behaviour around NaN/Inf coords. The temp table's
    /// `umap_x REAL NOT NULL` schema rejects NaN (sqlx binds NaN as NULL →
    /// constraint violation), so the call surfaces a SQLite error rather
    /// than panicking or silently writing corrupt floats to `chunks`. Adding
    /// an explicit `is_finite` guard at the helper boundary would produce a
    /// different, more user-friendly error — flipping this test signals that
    /// contract change.
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
