//! Chunk retrieval, search, identity, and statistics.

use std::collections::HashMap;
use std::path::PathBuf;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{ChunkType, Language};
use crate::store::helpers::{
    bytes_to_embedding, clamp_line_number, ChunkIdentity, ChunkRow, ChunkSummary, IndexStats,
    StoreError,
};
use crate::store::Store;

impl Store {
    /// Get the number of chunks in the index
    pub fn chunk_count(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("chunk_count").entered();
        self.rt.block_on(async {
            let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM chunks")
                .fetch_one(&self.pool)
                .await?;
            Ok(row.0 as u64)
        })
    }

    /// Get index statistics
    ///
    /// Uses batched queries to minimize database round trips:
    /// 1. Single query for counts with GROUP BY using CTEs
    /// 2. Single query for all metadata keys
    pub fn stats(&self) -> Result<IndexStats, StoreError> {
        let _span = tracing::debug_span!("stats").entered();
        self.rt.block_on(async {
            // Combined counts query using CTEs (3 queries → 1)
            let (total_chunks, total_files): (i64, i64) = sqlx::query_as(
                "SELECT
                    (SELECT COUNT(*) FROM chunks),
                    (SELECT COUNT(DISTINCT origin) FROM chunks)",
            )
            .fetch_one(&self.pool)
            .await?;

            let lang_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT language, COUNT(*) FROM chunks GROUP BY language")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_language: HashMap<Language, u64> = lang_rows
                .into_iter()
                .filter_map(|(lang, count)| {
                    lang.parse()
                        .map_err(|_| {
                            tracing::warn!(
                                language = %lang,
                                count,
                                "Unknown language in database, skipping in stats"
                            );
                        })
                        .ok()
                        .map(|l| (l, count as u64))
                })
                .collect();

            let type_rows: Vec<(String, i64)> =
                sqlx::query_as("SELECT chunk_type, COUNT(*) FROM chunks GROUP BY chunk_type")
                    .fetch_all(&self.pool)
                    .await?;

            let chunks_by_type: HashMap<ChunkType, u64> = type_rows
                .into_iter()
                .filter_map(|(ct, count)| {
                    ct.parse()
                        .map_err(|_| {
                            tracing::warn!(
                                chunk_type = %ct,
                                count,
                                "Unknown chunk_type in database, skipping in stats"
                            );
                        })
                        .ok()
                        .map(|c| (c, count as u64))
                })
                .collect();

            // Batch metadata query (4 queries → 1)
            let metadata_rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT key, value FROM metadata WHERE key IN ('model_name', 'created_at', 'updated_at', 'schema_version')",
            )
            .fetch_all(&self.pool)
            .await?;

            let metadata: HashMap<String, String> = metadata_rows.into_iter().collect();

            let model_name = metadata.get("model_name").cloned().unwrap_or_else(|| {
                tracing::debug!("metadata key 'model_name' missing, defaulting to empty");
                String::new()
            });
            let created_at = metadata.get("created_at").cloned().unwrap_or_else(|| {
                tracing::debug!("metadata key 'created_at' missing, defaulting to empty");
                String::new()
            });
            let updated_at = metadata
                .get("updated_at")
                .cloned()
                .unwrap_or_else(|| created_at.clone());
            let schema_version: i32 = metadata
                .get("schema_version")
                .and_then(|s| {
                    s.parse().map_err(|e| {
                        tracing::warn!(raw = %s, error = %e, "Failed to parse schema_version, defaulting to 0");
                    }).ok()
                })
                .unwrap_or(0);

            Ok(IndexStats {
                total_chunks: total_chunks as u64,
                total_files: total_files as u64,
                chunks_by_language,
                chunks_by_type,
                index_size_bytes: 0,
                created_at,
                updated_at,
                model_name,
                schema_version,
            })
        })
    }

    /// Get all chunks for a given file (origin).
    ///
    /// Returns chunks sorted by line_start. Used by `cqs context` to list
    /// all functions/types in a file.
    pub fn get_chunks_by_origin(&self, origin: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("get_chunks_by_origin", origin = %origin).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                        line_start, line_end, parent_id, parent_type_name
                 FROM chunks WHERE origin = ?1
                 ORDER BY line_start",
            )
            .bind(origin)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows
                .iter()
                .map(|r| ChunkSummary::from(ChunkRow::from_row(r)))
                .collect())
        })
    }

    /// Batch-fetch chunks by multiple origin paths.
    ///
    /// Returns a map of origin -> Vec<ChunkSummary> for all found origins.
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    /// Used by `cqs where` to avoid N+1 `get_chunks_by_origin` calls.
    pub fn get_chunks_by_origins_batch(
        &self,
        origins: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        let _span =
            tracing::debug_span!("get_chunks_by_origins_batch", count = origins.len()).entered();
        if origins.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id, parent_type_name
                     FROM chunks WHERE origin IN ({})
                     ORDER BY origin, line_start",
                    placeholders
                );

                let mut query = sqlx::query(&sql);
                for origin in batch {
                    query = query.bind(*origin);
                }

                let rows: Vec<_> = query.fetch_all(&self.pool).await?;
                for row in &rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                    let origin_key: String = row.get("origin");
                    result.entry(origin_key).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Batch-fetch chunks by multiple function names.
    ///
    /// Returns a map of name -> Vec<ChunkSummary> for all found names.
    /// Batches queries in groups of 500 to stay within SQLite's parameter limit (~999).
    /// Used by `cqs related` to avoid N+1 `get_chunks_by_name` calls.
    pub fn get_chunks_by_names_batch(
        &self,
        names: &[&str],
    ) -> Result<HashMap<String, Vec<ChunkSummary>>, StoreError> {
        let _span =
            tracing::debug_span!("get_chunks_by_names_batch", count = names.len()).entered();
        if names.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<ChunkSummary>> = HashMap::new();

            const BATCH_SIZE: usize = 500;
            for batch in names.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, origin, language, chunk_type, name, signature, content, doc,
                            line_start, line_end, parent_id, parent_type_name
                     FROM chunks WHERE name IN ({})
                     ORDER BY origin, line_start",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for name in batch {
                        q = q.bind(*name);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in &rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                    result.entry(chunk.name.clone()).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Batch signature search: find function/method chunks matching any of the given type names.
    ///
    /// Get a chunk with its embedding vector.
    ///
    /// Returns `Ok(None)` if the chunk doesn't exist or has a corrupt embedding.
    /// Used by `cqs similar` and `cqs explain` to search by example.
    pub fn get_chunk_with_embedding(
        &self,
        id: &str,
    ) -> Result<Option<(ChunkSummary, Embedding)>, StoreError> {
        let _span = tracing::debug_span!("get_chunk_with_embedding", id = %id).entered();
        let dim = self.dim;
        self.rt.block_on(async {
            let results = self
                .fetch_chunks_with_embeddings_by_ids_async(&[id])
                .await?;
            Ok(results.into_iter().next().and_then(|(row, bytes)| {
                match bytes_to_embedding(&bytes, dim) {
                    Some(emb) => Some((ChunkSummary::from(row), Embedding::new(emb))),
                    None => {
                        tracing::warn!(chunk_id = %row.id, "Corrupt embedding for chunk, skipping");
                        None
                    }
                }
            }))
        })
    }

    /// Batch-fetch chunks by IDs.
    ///
    /// Returns a map of chunk ID → ChunkSummary for all found IDs.
    /// Used by `--expand` to fetch parent chunks for small-to-big retrieval.
    pub fn get_chunks_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("get_chunks_by_ids", count = ids.len()).entered();
        self.rt.block_on(async {
            let rows = self.fetch_chunks_by_ids_async(ids).await?;
            Ok(rows
                .into_iter()
                .map(|(id, row)| (id, ChunkSummary::from(row)))
                .collect())
        })
    }

    /// Batch-fetch embeddings by chunk IDs.
    ///
    /// Returns a map of chunk ID → Embedding for all found IDs.
    /// Skips chunks with corrupt embeddings. Batches queries in groups of 500
    /// to stay within SQLite's parameter limit (~999).
    ///
    /// Used by `semantic_diff` to avoid N+1 queries when comparing matched pairs.
    pub fn get_embeddings_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, Embedding>, StoreError> {
        let _span = tracing::debug_span!("get_embeddings_by_ids", count = ids.len()).entered();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = 500;
        let dim = self.dim;
        let mut result = HashMap::new();

        self.rt.block_on(async {
            for batch in ids.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT id, embedding FROM chunks WHERE id IN ({})",
                    placeholders
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for id in batch {
                        q = q.bind(*id);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in rows {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    if let Some(emb) = bytes_to_embedding(&bytes, dim) {
                        result.insert(id, Embedding::new(emb));
                    }
                }
            }
            Ok(result)
        })
    }

    /// Batch name search: look up multiple names in a single call.
    ///
    /// For each name, returns up to `limit_per_name` matching chunks.
    /// Batches names into groups of 20 and issues a combined FTS OR query
    /// per batch, then post-filters results to assign to matching names.
    ///
    /// Used by `gather` BFS expansion to avoid N+1 query patterns.
    pub fn search_by_names_batch(
        &self,
        names: &[&str],
        limit_per_name: usize,
    ) -> Result<HashMap<String, Vec<crate::store::SearchResult>>, StoreError> {
        let _span =
            tracing::info_span!("search_by_names_batch", count = names.len(), limit_per_name)
                .entered();
        if names.is_empty() {
            return Ok(HashMap::new());
        }

        self.rt.block_on(async {
            let mut result: HashMap<String, Vec<crate::store::SearchResult>> = HashMap::new();

            // Normalize and sanitize all names upfront, keeping originals for scoring
            let normalized_names: Vec<(&str, String)> = names
                .iter()
                .map(|n| (*n, crate::store::sanitize_fts_query(&normalize_for_fts(n))))
                .filter(|(_, norm)| !norm.is_empty())
                .collect();

            // Batch into groups of 20 to avoid overly complex FTS queries
            const BATCH_SIZE: usize = 20;
            for batch in normalized_names.chunks(BATCH_SIZE) {
                // Build combined FTS query with OR
                // SAFETY: sanitize_fts_query independently strips all FTS5-significant
                // characters including double quotes, so format!-constructed FTS5
                // queries are safe even without normalize_for_fts().
                let fts_terms: Vec<String> = batch
                    .iter()
                    .filter_map(|(_, norm)| {
                        debug_assert!(
                            !norm.contains('"'),
                            "sanitized query must not contain double quotes"
                        );
                        if norm.contains('"') {
                            return None;
                        }
                        Some(format!("name:\"{}\" OR name:\"{}\"*", norm, norm))
                    })
                    .collect();
                let combined_fts = fts_terms.join(" OR ");

                // Single query for the batch with higher limit
                let total_limit = limit_per_name * batch.len();
                let rows: Vec<_> = sqlx::query(
                    "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.parent_id, c.parent_type_name
                     FROM chunks c
                     JOIN chunks_fts f ON c.id = f.id
                     WHERE chunks_fts MATCH ?1
                     ORDER BY bm25(chunks_fts, 10.0, 1.0, 1.0, 1.0)
                     LIMIT ?2",
                )
                .bind(&combined_fts)
                .bind(total_limit as i64)
                .fetch_all(&self.pool)
                .await?;

                // Post-filter: assign each row to matching names.
                // Complexity: O(results × batch.len()), but batch.len() ≤ BATCH_SIZE = 20
                // and score_name_match does fuzzy substring/prefix scoring, so a HashMap
                // on exact name is not applicable here. The bound is acceptable.
                for row in rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));

                    // Find which query names this result matches
                    for (original_name, _normalized) in batch {
                        let score = crate::store::score_name_match(&chunk.name, original_name);
                        if score > 0.0 {
                            let entry = result.entry(original_name.to_string()).or_default();
                            if entry.len() < limit_per_name {
                                entry.push(crate::store::SearchResult {
                                    chunk: chunk.clone(),
                                    score,
                                });
                            }
                            break;
                        }
                    }
                }
            }

            Ok(result)
        })
    }

    /// Get identity metadata for all chunks (for diff comparison).
    ///
    /// Returns minimal metadata needed to match chunks across stores.
    /// Loads all rows but only lightweight columns (no content or embeddings).
    pub fn all_chunk_identities(&self) -> Result<Vec<ChunkIdentity>, StoreError> {
        let _span = tracing::debug_span!("all_chunk_identities").entered();
        self.all_chunk_identities_filtered(None)
    }

    /// Fetch a page of full chunks by rowid cursor.
    ///
    /// Returns `(chunks, next_cursor)`. When the returned vec is empty, iteration
    /// is complete. Used by the enrichment pass to iterate all chunks without
    /// loading everything into memory.
    pub fn chunks_paged(
        &self,
        after_rowid: i64,
        limit: usize,
    ) -> Result<(Vec<ChunkSummary>, i64), StoreError> {
        let _span = tracing::debug_span!("chunks_paged", after_rowid, limit).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT rowid, id, origin, language, chunk_type, name, signature, content, doc, \
                 line_start, line_end, content_hash, window_idx, parent_id, parent_type_name \
                 FROM chunks WHERE rowid > ?1 ORDER BY rowid ASC LIMIT ?2",
            )
            .bind(after_rowid)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            let mut max_rowid = after_rowid;
            let chunks: Vec<ChunkSummary> = rows
                .iter()
                .map(|row| {
                    let rowid: i64 = row.get("rowid");
                    if rowid > max_rowid {
                        max_rowid = rowid;
                    }
                    ChunkSummary::from(ChunkRow::from_row(row))
                })
                .collect();

            Ok((chunks, max_rowid))
        })
    }

    /// Like `all_chunk_identities` but with an optional language filter.
    ///
    /// When `language` is `Some`, only chunks matching that language are returned,
    /// avoiding loading all chunks into memory when only one language is needed.
    pub fn all_chunk_identities_filtered(
        &self,
        language: Option<&str>,
    ) -> Result<Vec<ChunkIdentity>, StoreError> {
        let _span =
            tracing::debug_span!("all_chunk_identities_filtered", language = ?language).entered();
        self.rt.block_on(async {
            let rows: Vec<_> = if let Some(lang) = language {
                sqlx::query(
                    "SELECT id, origin, name, chunk_type, language, line_start, parent_id, window_idx FROM chunks WHERE language = ?1",
                )
                .bind(lang)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query(
                    "SELECT id, origin, name, chunk_type, language, line_start, parent_id, window_idx FROM chunks",
                )
                .fetch_all(&self.pool)
                .await?
            };

            Ok(rows
                .iter()
                .map(|row| ChunkIdentity {
                    id: row.get("id"),
                    file: PathBuf::from(row.get::<String, _>("origin")),
                    name: row.get("name"),
                    chunk_type: {
                        let raw: String = row.get("chunk_type");
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown chunk_type in DB, defaulting to Function");
                            ChunkType::Function
                        })
                    },
                    line_start: clamp_line_number(row.get::<i64, _>("line_start")),
                    language: {
                        let raw: String = row.get("language");
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "Unknown language in DB, defaulting to Rust");
                            Language::Rust
                        })
                    },
                    parent_id: row.get("parent_id"),
                    window_idx: row
                        .get::<Option<i64>, _>("window_idx")
                        .map(|i| i.clamp(0, u32::MAX as i64) as u32),
                })
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_utils::make_chunk;
    use crate::parser::Language;
    use crate::test_helpers::{mock_embedding, setup_store};

    // ===== all_chunk_identities_filtered tests =====

    #[test]
    fn test_all_chunk_identities_filtered_by_language() {
        let (store, _dir) = setup_store();

        let mut rust_chunk = make_chunk("rs_fn", "src/lib.rs");
        rust_chunk.language = Language::Rust;

        let mut py_chunk = make_chunk("py_fn", "src/main.py");
        py_chunk.language = Language::Python;
        py_chunk.id = format!("src/main.py:1:{}", &py_chunk.content_hash[..8]);

        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(
                &[(rust_chunk, emb.clone()), (py_chunk, emb.clone())],
                Some(100),
            )
            .unwrap();

        // Filter to Rust only
        let identities = store.all_chunk_identities_filtered(Some("rust")).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].language, Language::Rust);

        // Filter to Python only
        let identities = store.all_chunk_identities_filtered(Some("python")).unwrap();
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].language, Language::Python);

        // No filter returns all
        let identities = store.all_chunk_identities_filtered(None).unwrap();
        assert_eq!(identities.len(), 2);
    }

    // ===== get_chunks_by_origin tests =====

    #[test]
    fn test_get_chunks_by_origin_sorted_by_line() {
        let (store, _dir) = setup_store();

        let mut c1 = make_chunk("fn_late", "src/lib.rs");
        c1.line_start = 50;
        c1.line_end = 60;

        let mut c2 = make_chunk("fn_early", "src/lib.rs");
        c2.line_start = 1;
        c2.line_end = 10;
        c2.id = format!("src/lib.rs:1:{}", &c2.content_hash[..8]);

        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(100))
            .unwrap();

        let chunks = store.get_chunks_by_origin("src/lib.rs").unwrap();
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks[0].line_start <= chunks[1].line_start,
            "Chunks should be sorted by line_start"
        );
    }

    #[test]
    fn test_get_chunks_by_origin_empty() {
        let (store, _dir) = setup_store();
        let chunks = store.get_chunks_by_origin("nonexistent.rs").unwrap();
        assert!(chunks.is_empty());
    }

    // ===== TC-11: chunks_paged =====

    #[test]
    fn test_chunks_paged_empty() {
        let (store, _dir) = setup_store();
        let (chunks, max_rowid) = store.chunks_paged(0, 10).unwrap();
        assert!(chunks.is_empty());
        assert_eq!(max_rowid, 0);
    }

    #[test]
    fn test_chunks_paged_single_page() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..3)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        let (chunks, max_rowid) = store.chunks_paged(0, 10).unwrap();
        assert_eq!(chunks.len(), 3);
        assert!(max_rowid > 0);
    }

    #[test]
    fn test_chunks_paged_multi_page() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..5)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Page 1: limit=2
        let (page1, cursor1) = store.chunks_paged(0, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert!(cursor1 > 0);

        // Page 2
        let (page2, cursor2) = store.chunks_paged(cursor1, 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert!(cursor2 > cursor1);

        // Page 3: remaining
        let (page3, _cursor3) = store.chunks_paged(cursor2, 2).unwrap();
        assert_eq!(page3.len(), 1);

        // Total across all pages
        assert_eq!(page1.len() + page2.len() + page3.len(), 5);
    }

    #[test]
    fn test_chunks_paged_exact_boundary() {
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..4)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Fetch exactly 4 with limit=4
        let (page1, cursor1) = store.chunks_paged(0, 4).unwrap();
        assert_eq!(page1.len(), 4);

        // Next page should be empty
        let (page2, cursor2) = store.chunks_paged(cursor1, 4).unwrap();
        assert!(page2.is_empty());
        assert_eq!(cursor2, cursor1);
    }
}
