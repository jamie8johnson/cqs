//! Search methods for the Store (FTS, name search).

use std::future::Future;

use sqlx::Row;

use super::helpers::{self, ChunkRow, SearchResult};
use super::{sanitize_fts_query, ChunkSummary, Store, StoreError};
use crate::nl::normalize_for_fts;

/// One row of the brute-force candidate scan: cursor position plus the
/// scoring-relevant columns. `name` is populated only when the caller asked
/// for it (hybrid/demotion paths); the brute-force loop scores from these.
pub(crate) struct BruteForceRow {
    pub rowid: i64,
    pub id: String,
    pub embedding_bytes: Vec<u8>,
    pub name: Option<String>,
}

impl<Mode> Store<Mode> {
    /// Drive an async future to completion on the store's runtime.
    ///
    /// The store owns the tokio runtime; search drives its algorithmic
    /// pipeline (which interleaves store async SQL methods) through this
    /// wrapper rather than reaching the private `rt` field directly. Keeps
    /// the runtime an implementation detail of the store while letting the
    /// search module compose store async calls.
    pub(crate) fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    /// Fetch one cursor-paginated batch of brute-force scan candidates.
    ///
    /// `sql` is the pre-built `SELECT … FROM chunks WHERE … rowid > ? … LIMIT ?`
    /// template (filter conditions + cursor + limit binds appended by the
    /// caller); `bind_values` are the filter binds that precede the cursor and
    /// limit. `need_name` controls whether the `name` column is read into the
    /// returned rows. Store-executed SQL — the template is still composed
    /// search-side (its conditions come from the search filter), and the
    /// scoring loop reads the returned rows search-side.
    pub(crate) async fn fetch_brute_force_batch(
        &self,
        sql: &str,
        bind_values: &[String],
        last_rowid: i64,
        batch_size: i64,
        need_name: bool,
    ) -> Result<Vec<BruteForceRow>, StoreError> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for val in bind_values {
            q = q.bind(val);
        }
        q = q.bind(last_rowid);
        q = q.bind(batch_size);
        let rows = q.fetch_all(&self.pool).await?;

        Ok(rows
            .iter()
            .map(|row| BruteForceRow {
                rowid: row.get::<i64, _>("rowid"),
                id: row.get("id"),
                embedding_bytes: row.get("embedding"),
                name: if need_name { row.get("name") } else { None },
            })
            .collect())
    }
}

impl<Mode> Store<Mode> {
    /// Search FTS5 index for keyword matches.
    ///
    /// # Search Method Overview
    ///
    /// The Store provides several search methods with different characteristics:
    ///
    /// - **`search_fts`**: Full-text keyword search using SQLite FTS5. Returns chunk IDs.
    ///   Best for: Exact keyword matches, symbol lookup by name fragment.
    ///
    /// - **`search_by_name`**: Definition search by function/struct name. Uses FTS5 with
    ///   heavy weighting on the name column. Returns full `SearchResult` with scores.
    ///   Best for: "Where is X defined?" queries.
    ///
    /// - **`search_filtered`** (in `search/query.rs`): Semantic search with optional
    ///   language/path filters. Can use RRF hybrid search combining semantic + FTS scores.
    ///   Best for: Natural language queries like "retry with exponential backoff".
    ///
    /// - **`search_filtered_with_index`** (in `search/query.rs`): Like `search_filtered`
    ///   but uses HNSW/CAGRA vector index for O(log n) candidate retrieval instead of
    ///   brute force. Best for: Large indexes (>5k chunks) where brute force is slow.
    pub fn search_fts(&self, query: &str, limit: usize) -> Result<Vec<String>, StoreError> {
        let _span = tracing::info_span!("search_fts", limit).entered();
        let normalized_query = sanitize_fts_query(&normalize_for_fts(query));
        if normalized_query.is_empty() {
            tracing::debug!(
                original_query = %query,
                "Query normalized to empty string, returning no FTS results"
            );
            return Ok(vec![]);
        }

        self.rt
            .block_on(async { self.fts_match_ids(&normalized_query, limit).await })
    }

    /// Run an already-sanitized FTS5 MATCH query and return chunk IDs in
    /// bm25 order. The single home for the gated FTS id query — shared by
    /// `search_fts` and the RRF keyword leg in `finalize_results`.
    ///
    /// JOIN chunks + filter `needs_embedding = 0` so FTS-only candidates
    /// that haven't been embedded yet stay out of the RRF mix. They'd
    /// otherwise rank lower than expected (zero cosine score against
    /// zero-vec sentinel) and pollute the result list during a
    /// `--llm-summaries` reindex's partial state.
    ///
    /// `fts_query` must already be normalized/sanitized (callers run
    /// `normalize_for_fts` + `sanitize_fts_query`, optionally
    /// `expand_query_for_fts`); this helper binds it to MATCH verbatim.
    pub(crate) async fn fts_match_ids(
        &self,
        fts_query: &str,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT f.id FROM chunks_fts f \
             JOIN chunks c ON c.id = f.id \
             WHERE chunks_fts MATCH ?1 AND c.needs_embedding = 0 \
             ORDER BY bm25(chunks_fts) LIMIT ?2",
        )
        .bind(fts_query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Search for chunks by name (definition search).
    ///
    /// Searches the FTS5 name column for exact or prefix matches.
    /// Use this for "where is X defined?" queries instead of semantic search.
    ///
    /// # Limit cap
    ///
    /// `limit` is silently clamped to a hard ceiling of **100**. Callers
    /// requesting more get exactly 100 results. The clamp is logged at
    /// `WARN` level (`search_by_name cap hit`) so callers debugging
    /// missing definitions can find the cap. The ceiling is intentional:
    /// definition lookups should never need 100+ overloads, and the FTS5
    /// query cost grows linearly with `LIMIT`.
    pub fn search_by_name(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_by_name", %name, limit).entered();
        const NAME_SEARCH_CAP: usize = 100;
        if limit > NAME_SEARCH_CAP {
            tracing::warn!(
                requested = limit,
                cap = NAME_SEARCH_CAP,
                name = %name,
                "search_by_name cap hit; results truncated"
            );
        }
        let limit = limit.min(NAME_SEARCH_CAP);
        let normalized = sanitize_fts_query(&normalize_for_fts(name));
        if normalized.is_empty() {
            return Ok(vec![]);
        }

        // Pre-lowercase query once for score_name_match_pre_lower.
        let lower_name = name.to_lowercase();

        // Search name column specifically using FTS5 column filter
        // Use * for prefix matching (e.g., "parse" matches "parse_config")
        // Runtime guard — sanitize_fts_query strips `"` but defense-in-depth
        // prevents FTS5 injection if sanitization logic ever changes.
        if normalized.contains('"') {
            tracing::warn!(
                name = %name,
                "FTS injection guard: double quote in sanitized name, returning empty"
            );
            return Ok(vec![]);
        }
        let fts_query = format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized);

        // BM25 weights via canonical helper, plus SELECT `c.vendored` so
        // `resolve_target` / `read --focus` can emit the correct
        // `trust_level` for chunks under `node_modules/`/`vendor/`. Without
        // that column the `ChunkRow::from_row` `try_get` falls back to false
        // and every vendored chunk masquerades as user-code.
        // Filter `c.needs_embedding = 0` so chunks in the partial state of a
        // `--llm-summaries` reindex (parser stage written, not yet enriched)
        // are invisible from name-search until enrichment lands their real
        // embedding. Same visibility gate as HNSW build.
        let sql = format!(
            "SELECT {cols}
             FROM chunks c
             JOIN chunks_fts f ON c.id = f.id
             WHERE chunks_fts MATCH ?1
               AND c.needs_embedding = 0
             ORDER BY {ord}
             LIMIT ?2",
            cols = super::helpers::CHUNK_ROW_SELECT_COLUMNS_PREFIXED,
            ord = super::helpers::bm25_ordering_expr(),
        );

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(&fts_query)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?;

            // Skip the per-row `to_lowercase()` allocation when both query
            // and chunk name are pure ASCII (the dominant case for code
            // identifiers — exotic Unicode in function names is rare). ASCII
            // path uses `eq_ignore_ascii_case` + `score_name_match_ascii` for
            // zero-alloc scoring; Unicode names fall through to the
            // `to_lowercase()` + `score_name_match_pre_lower` path so
            // semantics are identical.
            let lower_name_ascii = lower_name.is_ascii();
            let mut results = rows
                .into_iter()
                .map(|row| {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));
                    let score = if lower_name_ascii && chunk.name.is_ascii() {
                        helpers::score_name_match_ascii(&chunk.name, &lower_name)
                    } else {
                        let name_lower = chunk.name.to_lowercase();
                        helpers::score_name_match_pre_lower(&name_lower, &lower_name)
                    };
                    SearchResult { chunk, score }
                })
                .collect::<Vec<_>>();

            // Re-sort by name-match score (FTS bm25 ordering may differ).
            // Chunk id sorts line numbers lexicographically
            // (`"file.rs:10:..." < "file.rs:2:..."`), so the *real* line-2
            // definition would lose to the line-10 stub at score ties.
            // Tuple key prefers earlier file (alphabetic), then earlier
            // line (numeric), then chunk id for absolute determinism.
            results.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then(a.chunk.file.cmp(&b.chunk.file))
                    .then(a.chunk.line_start.cmp(&b.chunk.line_start))
                    .then(a.chunk.id.cmp(&b.chunk.id))
            });

            Ok(results)
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_helpers::setup_store;

    /// Insert a minimal chunk + FTS row for `search_by_name` tie-breaker tests.
    /// Mirrors the production upsert path closely enough that the FTS index
    /// rowid matches the chunks row, which is what `search_by_name` joins on.
    fn insert_named_chunk(
        store: &crate::Store,
        id: &str,
        file: &str,
        name: &str,
        line_start: u32,
        line_end: u32,
    ) {
        store.rt.block_on(async {
            let embedding = crate::embedder::Embedding::new(vec![0.0f32; crate::EMBEDDING_DIM]);
            let embedding_bytes =
                crate::store::helpers::embedding_to_bytes(&embedding, crate::EMBEDDING_DIM)
                    .unwrap();
            let now = chrono::Utc::now().to_rfc3339();
            sqlx::query(
                "INSERT INTO chunks (id, origin, source_type, language, chunk_type, name,
                     signature, content, content_hash, doc, line_start, line_end, embedding,
                     source_mtime, created_at, updated_at)
                     VALUES (?1, ?2, 'file', 'rust', 'function', ?3,
                     '', '', '', NULL, ?4, ?5, ?6, 0, ?7, ?7)",
            )
            .bind(id)
            .bind(file)
            .bind(name)
            .bind(line_start as i64)
            .bind(line_end as i64)
            .bind(&embedding_bytes)
            .bind(&now)
            .execute(&store.pool)
            .await
            .unwrap();

            // FTS5 join target — `search_by_name` matches against `chunks_fts`
            // and the join is by `id`. Use the same `normalize_for_fts` the
            // real upsert uses so query tokens match.
            sqlx::query("INSERT INTO chunks_fts (id, name, signature, content, doc) VALUES (?1, ?2, '', '', '')")
                .bind(id)
                .bind(crate::nl::normalize_for_fts(name))
                .execute(&store.pool)
                .await
                .unwrap();
        });
    }

    /// When two chunks share a name in the same file but at different line
    /// numbers, the result for `cqs --name-only build` must list the
    /// *earlier* line first. A chunk-id-only tie-breaker would sort
    /// "file.rs:10:..." before "file.rs:2:..." lexicographically, letting the
    /// line-10 stub beat the line-2 real definition. The tuple key
    /// `(file, line_start, id)` prevents that.
    #[test]
    fn search_by_name_prefers_earlier_line_in_same_file() {
        let (store, _dir) = setup_store();
        // Same file, same name, different line numbers. ID prefix order
        // mirrors what the real chunker would emit: a longer chunk-id
        // string for line 10 sorts before line 2 lexicographically.
        insert_named_chunk(&store, "src/lib.rs:2:abc", "src/lib.rs", "build", 2, 5);
        insert_named_chunk(&store, "src/lib.rs:10:def", "src/lib.rs", "build", 10, 12);

        let results = store.search_by_name("build", 10).unwrap();
        assert_eq!(results.len(), 2, "should match both `build` definitions");
        // Earlier line wins under the tuple tie-breaker.
        assert_eq!(
            results[0].chunk.line_start, 2,
            "expected line 2 first (real definition), got line {}: \
             chunk-id-only sort regressed",
            results[0].chunk.line_start
        );
        assert_eq!(results[1].chunk.line_start, 10);
    }

    /// Cross-file tie-breaker: at equal score, the alphabetically-earlier
    /// file wins. Pins the documented contract from the doc comment so a
    /// future "swap to ordering by id-suffix-hash" refactor is caught.
    #[test]
    fn search_by_name_prefers_earlier_file_at_equal_score() {
        let (store, _dir) = setup_store();
        insert_named_chunk(&store, "src/zz.rs:1:aaa", "src/zz.rs", "boot", 1, 3);
        insert_named_chunk(&store, "src/aa.rs:1:zzz", "src/aa.rs", "boot", 1, 3);

        let results = store.search_by_name("boot", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].chunk.file.to_string_lossy(),
            "src/aa.rs",
            "expected src/aa.rs first (alphabetic), got {}",
            results[0].chunk.file.display()
        );
    }
}
