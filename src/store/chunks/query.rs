//! Chunk retrieval, search, identity, and statistics.

use std::collections::HashMap;
use std::path::PathBuf;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::nl::normalize_for_fts;
use crate::parser::{ChunkType, Language};
use crate::store::helpers::sql::max_rows_per_statement;
use crate::store::helpers::{
    bytes_to_embedding, clamp_line_number, ChunkIdentity, ChunkRow, ChunkSummary, IndexStats,
    StoreError,
};
use crate::store::Store;

/// Row cap for [`Store::lookup_by_name`]. Matches the kind-mismatch
/// fallback's `definitions[]` render cap downstream (the CLI/daemon
/// graph fallbacks cap definitions at 100 entries), so the lookup never
/// fetches summaries the fallback would discard, while still feeding the
/// kind classifier enough evidence to route a hot name.
pub const LOOKUP_BY_NAME_LIMIT: usize = 100;

/// Hard ceiling on the page size [`Store::chunks_paged`] accepts. The
/// enrichment / doc-comment loops pass an operator-tunable page size
/// (`CQS_ENRICHMENT_PAGE_SIZE`, default 500); this caps a pathological or
/// hostile value so a single page can't materialize an unbounded row Vec.
/// 10k rows × the per-`ChunkSummary` footprint stays comfortably bounded
/// while leaving every realistic page size untouched.
const CHUNKS_PAGED_MAX_LIMIT: usize = 10_000;

/// SQL `CASE chunk_type ... END` expression ranking rows by routing
/// priority for [`Store::lookup_by_name`]'s ORDER BY. Generated from
/// `ChunkType::ALL` through `classify_chunk_type` + `routing_priority`,
/// so the priority groups can never drift from the kind classifier: a
/// new `ChunkType` variant is ranked automatically by its `Kind`.
fn chunk_type_priority_case() -> &'static str {
    use std::fmt::Write as _;
    use std::sync::OnceLock;
    static CASE_SQL: OnceLock<String> = OnceLock::new();
    CASE_SQL.get_or_init(|| {
        let mut case = String::from("CASE chunk_type");
        for ct in ChunkType::ALL {
            let priority = crate::kind::routing_priority(crate::kind::classify_chunk_type(*ct));
            // ChunkType's Display strings are the exact values stored in
            // the chunk_type column; none contain quotes.
            let _ = write!(case, " WHEN '{ct}' THEN {priority}");
        }
        // Unknown values (future schema drift) sort last.
        case.push_str(" ELSE 99 END");
        case
    })
}

impl<Mode> Store<Mode> {
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

    /// Phase 5: count chunks with a non-NULL `embedding_base` column.
    ///
    /// Returns 0 right after the v17→v18 migration (column added but not
    /// populated) and climbs to [`chunk_count`] once the next index pass
    /// has run. Used by the dual HNSW builder to decide whether to build
    /// the base index at all.
    pub fn base_embedding_count(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("base_embedding_count").entered();
        self.rt.block_on(async {
            let row: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE embedding_base IS NOT NULL")
                    .fetch_one(&self.pool)
                    .await?;
            Ok(row.0 as u64)
        })
    }

    /// Count of chunks awaiting a real embedding.
    ///
    /// Triggers `enrichment_pass` from `cmd_index` whenever `> 0` so the
    /// first-pass-skip (`--llm-summaries` flow) doesn't leave chunks at
    /// `needs_embedding=1` indefinitely. Backed by the partial index
    /// `idx_chunks_needs_embedding`, so the lookup is O(needs) regardless of
    /// total chunk count.
    pub fn needs_embedding_count(&self) -> Result<u64, StoreError> {
        let _span = tracing::debug_span!("needs_embedding_count").entered();
        self.rt.block_on(async {
            let row: (i64,) =
                sqlx::query_as("SELECT COUNT(*) FROM chunks WHERE needs_embedding = 1")
                    .fetch_one(&self.pool)
                    .await?;
            Ok(row.0 as u64)
        })
    }

    /// Chunk IDs awaiting a real embedding.
    ///
    /// Used by `enrichment_pass` to bypass the
    /// "skip-chunks-with-no-enrichment-context" early-out for any chunk
    /// that's still at `needs_embedding=1`. The base NL embedding is
    /// equivalent to what the first-pass would have produced
    /// (see `nl/mod.rs:104-108` — empty ctx + None summary collapses to
    /// `generate_nl_description_with_seq_len`), so the chunk gets a
    /// meaningful vector either way.
    pub fn needs_embedding_ids(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        let _span = tracing::debug_span!("needs_embedding_ids").entered();
        self.rt.block_on(async {
            let rows: Vec<(String,)> =
                sqlx::query_as("SELECT id FROM chunks WHERE needs_embedding = 1")
                    .fetch_all(&self.pool)
                    .await?;
            Ok(rows.into_iter().map(|(id,)| id).collect())
        })
    }

    /// Get index statistics
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
    /// Returns chunks sorted by line_start. Used by `cqs context` to list
    /// all functions/types in a file.
    pub fn get_chunks_by_origin(&self, origin: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("get_chunks_by_origin", origin = %origin).entered();
        self.rt.block_on(async {
            let sql = format!(
                "SELECT {cols} FROM chunks WHERE origin = ?1 ORDER BY line_start",
                cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
            );
            let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
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

            const BATCH_SIZE: usize = max_rows_per_statement(1);
            for batch in origins.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT {cols} FROM chunks WHERE origin IN ({placeholders}) \
                     ORDER BY origin, line_start",
                    cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
                );

                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                for origin in batch {
                    query = query.bind(*origin);
                }

                let rows: Vec<_> = query.fetch_all(&self.pool).await?;
                for row in &rows {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(row));
                    // origin is at ordinal 1 in CHUNK_ROW_SELECT_COLUMNS
                    let origin_key: String = row.get(1);
                    result.entry(origin_key).or_default().push(chunk);
                }
            }

            Ok(result)
        })
    }

    /// Exact-match lookup: return chunks whose `name` equals `name`,
    /// ordered by routing priority (callables, then types, then consts,
    /// then modules — see [`crate::kind::routing_priority`]) with
    /// `(chunk_type, origin, line_start)` as deterministic tiebreakers,
    /// capped at [`LOOKUP_BY_NAME_LIMIT`] rows. Polymorphic-routing
    /// building block — the `cqs::kind::classify_hits` reducer consumes
    /// the result to decide which command to dispatch (or whether to fall
    /// through to a freeform search). See `docs/polymorphic-routing.md`.
    ///
    /// The cap exists so a hot name (`Result`, `Error`, `new`) never
    /// deserializes thousands of full `ChunkSummary` rows just to answer
    /// a routing question. Under the cap, ordering is load-bearing: the
    /// classifier reduces over the set of kinds it sees, so the priority
    /// ORDER BY guarantees the highest-priority kinds survive the cut.
    /// A name whose matches all fit under the cap classifies exactly as
    /// it did unbounded; a name with more matches than the cap classifies
    /// from its highest-priority kinds (a name that is overwhelmingly
    /// callable routes as callable rather than tipping into a fallback
    /// because alphabetically-earlier kinds crowded the result).
    ///
    /// Distinct from [`Store::search_by_name`] (FTS-driven, prefix +
    /// fuzzy) and [`Self::get_chunks_by_names_batch`] (also exact, but
    /// optimized for many names at once). For the single-name kind-
    /// detection case, this method is the right primitive: one SQL
    /// query, no FTS overhead, no batching ceremony.
    pub fn lookup_by_name(&self, name: &str) -> Result<Vec<ChunkSummary>, StoreError> {
        let _span = tracing::debug_span!("lookup_by_name", %name).entered();
        if name.is_empty() {
            return Ok(Vec::new());
        }
        self.rt.block_on(async {
            let sql = format!(
                "SELECT {cols} FROM chunks WHERE name = ?1 \
                 ORDER BY {priority}, chunk_type, origin, line_start \
                 LIMIT {limit}",
                cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
                priority = chunk_type_priority_case(),
                limit = LOOKUP_BY_NAME_LIMIT,
            );
            let rows = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(name)
                .fetch_all(&self.pool)
                .await?;
            Ok(rows
                .into_iter()
                .map(|row| ChunkSummary::from(ChunkRow::from_row(&row)))
                .collect())
        })
    }

    /// Batch-fetch chunks by multiple function names.
    /// Returns a map of name -> Vec<ChunkSummary> for all found names.
    /// Single-bind IN-list batched at the modern SQLite variable limit.
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

            const BATCH_SIZE: usize = max_rows_per_statement(1);
            for batch in names.chunks(BATCH_SIZE) {
                let placeholders = crate::store::helpers::make_placeholders(batch.len());
                let sql = format!(
                    "SELECT {cols} FROM chunks WHERE name IN ({placeholders}) \
                     ORDER BY origin, line_start",
                    cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
                );

                let rows: Vec<_> = {
                    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
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
    /// Get a chunk with its embedding vector.
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
                    Ok(emb) => Some((ChunkSummary::from(row), Embedding::new(emb))),
                    Err(e) => {
                        tracing::warn!(chunk_id = %row.id, error = %e, "Corrupt embedding for chunk, skipping");
                        None
                    }
                }
            }))
        })
    }

    /// Batch-fetch chunks by IDs.
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
    /// Returns a map of chunk ID → Embedding for all found IDs.
    /// Skips chunks with corrupt embeddings. Batches queries in groups of 500
    /// to stay within SQLite's parameter limit (~999).
    /// Used by `semantic_diff` to avoid N+1 queries when comparing matched pairs.
    pub fn get_embeddings_by_ids(
        &self,
        ids: &[&str],
    ) -> Result<HashMap<String, Embedding>, StoreError> {
        let _span = tracing::debug_span!("get_embeddings_by_ids", count = ids.len()).entered();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        const BATCH_SIZE: usize = max_rows_per_statement(1);
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
                    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
                    for id in batch {
                        q = q.bind(*id);
                    }
                    q.fetch_all(&self.pool).await?
                };

                for row in rows {
                    let id: String = row.get(0);
                    let bytes: Vec<u8> = row.get(1);
                    match bytes_to_embedding(&bytes, dim) {
                        Ok(emb) => {
                            result.insert(id, Embedding::new(emb));
                        }
                        Err(e) => {
                            tracing::warn!(chunk_id = %id, error = %e, "Corrupt embedding blob, skipping — run 'cqs index --force' to rebuild");
                        }
                    }
                }
            }
            Ok(result)
        })
    }

    /// Batch name search: look up multiple names in a single call.
    /// For each name, returns up to `limit_per_name` matching chunks.
    /// Batches names into groups of 20 and issues a combined FTS OR query
    /// per batch, then post-filters results to assign to matching names.
    /// Used by `gather` BFS expansion to avoid N+1 query patterns.
    ///
    /// PF-6: Two-phase approach — first fetches lightweight id+name rows via FTS,
    /// scores and assigns to query names, then hydrates only matched IDs with full
    /// content via `fetch_chunks_by_ids_async`. Avoids loading full content for
    /// rows that won't match any query name.
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

                // Phase 1: lightweight id+name fetch via FTS
                let total_limit = limit_per_name * batch.len();
                // SHL-V1.33-8: BM25 column weights flow through the canonical
                // constants in `helpers/mod.rs` — must stay in sync with
                // `store::search::search_by_name`.
                let sql = format!(
                    "SELECT c.id, c.name
                     FROM chunks c
                     JOIN chunks_fts f ON c.id = f.id
                     WHERE chunks_fts MATCH ?1
                     ORDER BY {}
                     LIMIT ?2",
                    crate::store::helpers::bm25_ordering_expr()
                );
                let light_rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                    .bind(&combined_fts)
                    .bind(total_limit as i64)
                    .fetch_all(&self.pool)
                    .await?;

                // Phase 2: score name matches and collect IDs to hydrate.
                // Track (chunk_id, query_name, score) for matched rows.
                let mut matched: Vec<(String, String, f32)> = Vec::new();
                let mut ids_to_fetch: Vec<String> = Vec::new();

                for row in &light_rows {
                    let id: String = row.get("id");
                    let chunk_name: String = row.get("name");

                    for (original_name, _normalized) in batch {
                        let score = crate::store::score_name_match(&chunk_name, original_name);
                        if score > 0.0 {
                            let entry = result.entry(original_name.to_string()).or_default();
                            if entry.len() < limit_per_name {
                                ids_to_fetch.push(id.clone());
                                matched.push((id.clone(), original_name.to_string(), score));
                            }
                            break;
                        }
                    }
                }

                if ids_to_fetch.is_empty() {
                    continue;
                }

                // Phase 3: hydrate matched IDs with full content
                let id_refs: Vec<&str> = ids_to_fetch.iter().map(|s| s.as_str()).collect();
                let full_chunks = self.fetch_chunks_by_ids_async(&id_refs).await?;

                for (id, query_name, score) in matched {
                    if let Some(chunk_row) = full_chunks.get(&id) {
                        let entry = result.entry(query_name).or_default();
                        if entry.len() < limit_per_name {
                            entry.push(crate::store::SearchResult {
                                chunk: ChunkSummary::from(chunk_row.clone()),
                                score,
                            });
                        }
                    }
                }
            }

            Ok(result)
        })
    }

    /// Get identity metadata for all chunks (for diff comparison).
    /// Returns minimal metadata needed to match chunks across stores.
    /// Loads all rows but only lightweight columns (no content or embeddings).
    pub fn all_chunk_identities(&self) -> Result<Vec<ChunkIdentity>, StoreError> {
        let _span = tracing::debug_span!("all_chunk_identities").entered();
        self.all_chunk_identities_filtered(None)
    }

    /// Bulk lookup of chunk_type and language for all chunks, keyed by chunk ID.
    /// Used by HNSW traversal-time filtering to decide which chunks to skip.
    /// Cached chunk type + language map. Computed once per Store lifetime (PF-12).
    pub fn chunk_type_language_map(
        &self,
    ) -> Result<std::sync::Arc<crate::store::ChunkTypeMap>, StoreError> {
        if let Some(cached) = self.chunk_type_map_cache.get() {
            return Ok(std::sync::Arc::clone(cached));
        }
        let _span = tracing::debug_span!("chunk_type_language_map").entered();
        let map = self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query("SELECT id, chunk_type, language FROM chunks")
                .fetch_all(&self.pool)
                .await?;
            let mut map = HashMap::with_capacity(rows.len());
            for row in &rows {
                let id: String = row.get("id");
                let ct: String = row.get("chunk_type");
                let lang: String = row.get("language");
                match (ct.parse(), lang.parse()) {
                    (Ok(chunk_type), Ok(language)) => {
                        map.insert(id, (chunk_type, language));
                    }
                    (ct_result, lang_result) => {
                        tracing::warn!(
                            chunk_id = %id,
                            chunk_type = %ct,
                            language = %lang,
                            ct_err = ?ct_result.err(),
                            lang_err = ?lang_result.err(),
                            "Skipping chunk with unparseable chunk_type or language"
                        );
                    }
                }
            }
            Ok::<_, StoreError>(map)
        })?;
        let arc = std::sync::Arc::new(map);
        let _ = self.chunk_type_map_cache.set(std::sync::Arc::clone(&arc));
        Ok(arc)
    }

    /// Fetch a page of full chunks by rowid cursor.
    /// Returns `(chunks, next_cursor)`. When the returned vec is empty, iteration
    /// is complete. Used by the enrichment pass to iterate all chunks without
    /// loading everything into memory.
    pub fn chunks_paged(
        &self,
        after_rowid: i64,
        limit: usize,
    ) -> Result<(Vec<ChunkSummary>, i64), StoreError> {
        // Clamp the caller-supplied page size to a module ceiling so a
        // pathological env-tuned page size can't materialize an unbounded
        // row Vec in a single query.
        let limit = limit.min(CHUNKS_PAGED_MAX_LIMIT);
        let _span = tracing::debug_span!("chunks_paged", after_rowid, limit).entered();
        self.rt.block_on(async {
            // rowid appended AFTER the pinned ChunkRow columns so the
            // ordinal contract for `ChunkRow::from_row` (0..15) holds.
            let sql = format!(
                "SELECT {cols}, rowid FROM chunks WHERE rowid > ?1 \
                 ORDER BY rowid ASC LIMIT ?2",
                cols = crate::store::helpers::CHUNK_ROW_SELECT_COLUMNS,
            );
            let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(after_rowid)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?;

            let mut max_rowid = after_rowid;
            let chunks: Vec<ChunkSummary> = rows
                .iter()
                .map(|row| {
                    let rowid: i64 = row.get(16);
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

    /// An above-ceiling page size is clamped to `CHUNKS_PAGED_MAX_LIMIT`
    /// before binding to SQL, but still returns every available row when
    /// the table is smaller than the ceiling — the clamp guards the bind
    /// against a pathological value without breaking correctness.
    #[test]
    fn test_chunks_paged_clamps_oversized_limit() {
        assert_eq!(super::CHUNKS_PAGED_MAX_LIMIT, 10_000);
        let (store, _dir) = setup_store();
        let pairs: Vec<_> = (0..3)
            .map(|i| {
                let c = make_chunk(&format!("fn_{}", i), &format!("src/{}.rs", i));
                (c, mock_embedding(i as f32))
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(100)).unwrap();

        // Pass a limit far above the ceiling — the clamp applies but all 3
        // rows still come back.
        let (chunks, _max_rowid) = store.chunks_paged(0, usize::MAX).unwrap();
        assert_eq!(chunks.len(), 3);
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

    // ===== lookup_by_name (polymorphic-routing Phase 1 plumbing) =====

    #[test]
    fn test_lookup_by_name_returns_empty_for_missing() {
        let (store, _dir) = setup_store();
        let hits = store.lookup_by_name("nonexistent_name").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn test_lookup_by_name_returns_empty_for_empty_string() {
        let (store, _dir) = setup_store();
        // Empty name short-circuits without an SQL roundtrip — pin the
        // contract so the kind classifier doesn't have to special-case it.
        let hits = store.lookup_by_name("").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn test_lookup_by_name_returns_single_function_match() {
        let (store, _dir) = setup_store();
        let chunk = make_chunk("foo", "src/lib.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        let hits = store.lookup_by_name("foo").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "foo");
        assert_eq!(hits[0].chunk_type, crate::parser::ChunkType::Function);
    }

    #[test]
    fn test_lookup_by_name_returns_multiple_same_name_different_files() {
        let (store, _dir) = setup_store();
        let mut c1 = make_chunk("foo", "src/a.rs");
        c1.line_start = 10;
        c1.id = format!("src/a.rs:10:{}", &c1.content_hash[..8]);
        let mut c2 = make_chunk("foo", "src/b.rs");
        c2.line_start = 20;
        c2.id = format!("src/b.rs:20:{}", &c2.content_hash[..8]);

        store
            .upsert_chunks_batch(
                &[(c1, mock_embedding(1.0)), (c2, mock_embedding(2.0))],
                Some(100),
            )
            .unwrap();

        let hits = store.lookup_by_name("foo").unwrap();
        assert_eq!(hits.len(), 2, "two different chunks with same name");
        // ORDER BY chunk_type, origin, line_start: same chunk_type
        // (Function), so origin (file path) is the tiebreaker. a.rs
        // sorts before b.rs.
        assert_eq!(hits[0].file.to_string_lossy(), "src/a.rs");
        assert_eq!(hits[1].file.to_string_lossy(), "src/b.rs");
    }

    #[test]
    fn test_lookup_by_name_orders_by_routing_priority_not_alphabetical() {
        // A name colliding across kinds must come back callables-first.
        // Alphabetically, 'constant' < 'function' < 'struct'; routing
        // priority is function < struct (type) < constant. Seed the
        // collision in alphabetical-friendly file order so a regression
        // back to `ORDER BY chunk_type` flips the assertion.
        let (store, _dir) = setup_store();

        let mut const_chunk = make_chunk("collide", "src/a.rs");
        const_chunk.chunk_type = crate::parser::ChunkType::Constant;
        const_chunk.id = format!("src/a.rs:1:{}", &const_chunk.content_hash[..8]);

        let mut struct_chunk = make_chunk("collide", "src/b.rs");
        struct_chunk.chunk_type = crate::parser::ChunkType::Struct;
        struct_chunk.id = format!("src/b.rs:1:{}", &struct_chunk.content_hash[..8]);

        let mut fn_chunk = make_chunk("collide", "src/c.rs");
        fn_chunk.id = format!("src/c.rs:1:{}", &fn_chunk.content_hash[..8]);

        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(
                &[
                    (const_chunk, emb.clone()),
                    (struct_chunk, emb.clone()),
                    (fn_chunk, emb.clone()),
                ],
                Some(100),
            )
            .unwrap();

        let hits = store.lookup_by_name("collide").unwrap();
        let order: Vec<_> = hits.iter().map(|h| h.chunk_type).collect();
        assert_eq!(
            order,
            vec![
                crate::parser::ChunkType::Function,
                crate::parser::ChunkType::Struct,
                crate::parser::ChunkType::Constant,
            ],
            "lookup_by_name must rank callable > type > const"
        );
    }

    #[test]
    fn test_lookup_by_name_caps_result_rows() {
        // Hot names must not deserialize unbounded row sets — the lookup
        // is a routing primitive, and its consumers (kind classifier +
        // fallback definitions) never need more than the cap.
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let pairs: Vec<_> = (0..(super::LOOKUP_BY_NAME_LIMIT + 25))
            .map(|i| {
                let mut c = make_chunk("hot_name", &format!("src/f{i:04}.rs"));
                c.id = format!("src/f{i:04}.rs:1:{}", &c.content_hash[..8]);
                (c, emb.clone())
            })
            .collect();
        store.upsert_chunks_batch(&pairs, Some(1000)).unwrap();

        let hits = store.lookup_by_name("hot_name").unwrap();
        assert_eq!(hits.len(), super::LOOKUP_BY_NAME_LIMIT);
    }

    #[test]
    fn test_lookup_by_name_priority_keeps_callable_evidence_under_cap() {
        // One function buried among LIMIT consts whose chunk_type sorts
        // alphabetically earlier: the priority ORDER BY must surface the
        // callable inside the capped window (first, in fact), so the kind
        // classifier still sees the Function evidence on hot names.
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let mut pairs: Vec<_> = (0..super::LOOKUP_BY_NAME_LIMIT)
            .map(|i| {
                let mut c = make_chunk("busy", &format!("src/c{i:04}.rs"));
                c.chunk_type = crate::parser::ChunkType::Constant;
                c.id = format!("src/c{i:04}.rs:1:{}", &c.content_hash[..8]);
                (c, emb.clone())
            })
            .collect();
        let mut f = make_chunk("busy", "src/zzz.rs");
        f.id = format!("src/zzz.rs:1:{}", &f.content_hash[..8]);
        pairs.push((f, emb.clone()));
        store.upsert_chunks_batch(&pairs, Some(1000)).unwrap();

        let hits = store.lookup_by_name("busy").unwrap();
        assert_eq!(hits.len(), super::LOOKUP_BY_NAME_LIMIT);
        assert_eq!(
            hits[0].chunk_type,
            crate::parser::ChunkType::Function,
            "the callable must survive the cap and rank first"
        );
    }

    #[test]
    fn test_chunk_type_priority_case_covers_every_chunk_type() {
        // The CASE expression is generated from ChunkType::ALL, so every
        // stored chunk_type string must appear in it — pin the generation
        // so a refactor to a hand-written list can't silently drop one.
        let case = super::chunk_type_priority_case();
        for ct in crate::parser::ChunkType::ALL {
            assert!(
                case.contains(&format!("WHEN '{ct}' THEN")),
                "priority CASE missing chunk_type '{ct}'"
            );
        }
        assert!(case.ends_with("ELSE 99 END"));
    }

    #[test]
    fn test_lookup_by_name_does_not_match_substring() {
        // Pin exact-equality semantics — "foo" must NOT match "foo_bar".
        // The kind classifier depends on this for Multiple vs Ambiguous
        // disambiguation.
        let (store, _dir) = setup_store();
        let chunk = make_chunk("foo_bar", "src/lib.rs");
        store
            .upsert_chunks_batch(&[(chunk, mock_embedding(1.0))], Some(100))
            .unwrap();

        let hits = store.lookup_by_name("foo").unwrap();
        assert!(
            hits.is_empty(),
            "lookup_by_name should not return prefix-substring matches"
        );

        let exact_hits = store.lookup_by_name("foo_bar").unwrap();
        assert_eq!(exact_hits.len(), 1);
    }
}
