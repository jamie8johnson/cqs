//! Search query execution on `Store`.
//!
//! Contains the `impl Store` block with all search methods:
//! `search_embedding_only`, `search_filtered`, `finalize_results`,
//! `search_filtered_with_index`, `search_by_candidate_ids`, and
//! `search_unified_with_index`.

use std::collections::HashSet;

use sqlx::Row;

use crate::embedder::Embedding;
use crate::index::VectorIndex;
use crate::nl::normalize_for_fts;
use crate::store::helpers::{
    embedding_slice, CandidateRow, ChunkSummary, SearchFilter, SearchResult,
};
use crate::store::sanitize_fts_query;
use crate::store::{Store, StoreError};

use super::scoring::{
    apply_parent_boost, build_filter_sql, compile_glob_filter, extract_file_from_chunk_id,
    is_name_like_query, score_candidate, BoundedScoreHeap, NameMatcher, NoteBoostIndex,
    ScoringContext,
};

impl Store {
    /// Raw embedding-only cosine similarity search (no RRF, no keyword matching).
    ///
    /// **You almost certainly want `search_filtered()` instead.** This method skips
    /// hybrid RRF ranking, name boosting, and all filters. It exists for tests and
    /// internal building blocks only. Two production bugs came from calling this
    /// directly (PR #305).
    pub fn search_embedding_only(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        self.search_filtered(query, &SearchFilter::default(), limit, threshold)
    }

    /// Searches for embeddings matching a query with optional filtering and ranking.
    ///
    /// # Arguments
    ///
    /// * `query` - The embedding vector to search for
    /// * `filter` - Search filter configuration including path patterns, RRF settings, and demotion rules
    /// * `limit` - Maximum number of results to return
    /// * `threshold` - Minimum similarity score threshold for results
    ///
    /// # Returns
    ///
    /// A vector of search results ranked by relevance, containing up to `limit` entries that exceed the similarity threshold.
    ///
    /// # Errors
    ///
    /// Returns `StorageError` if loading cached note summaries fails or if the underlying search operation encounters a storage error.
    pub fn search_filtered(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_filtered", limit = limit, rrf = filter.enable_rrf)
            .entered();

        // Load notes once for note-boosted ranking (cheap — no embeddings)
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                Vec::new()
            }
        };

        self.rt.block_on(async {
            let fsql = build_filter_sql(filter);
            let semantic_limit = if fsql.use_rrf { limit * 3 } else { limit };
            let need_name = fsql.use_hybrid || filter.enable_demotion;

            // Compile glob pattern once outside the loop (not per-chunk).
            // Note: Invalid patterns are logged and silently ignored (returns all results).
            // Callers should validate patterns upfront via SearchFilter::validate() if they
            // want to reject invalid patterns. This lenient behavior is intentional to allow
            // partial searches when users provide malformed patterns interactively.
            let glob_matcher = compile_glob_filter(filter.path_pattern.as_ref());

            // Pre-tokenize query for name matching (avoids re-tokenizing per result)
            let name_matcher = if fsql.use_hybrid {
                Some(NameMatcher::new(&filter.query_text))
            } else {
                None
            };

            // Pre-compute note boost lookup for O(1) name matching in scoring loop
            let note_index = NoteBoostIndex::new(&notes);

            // Build loop-invariant scoring context once
            let scoring_ctx = ScoringContext {
                query: query.as_slice(),
                filter,
                name_matcher: name_matcher.as_ref(),
                glob_matcher: glob_matcher.as_ref(),
                note_index: &note_index,
                threshold,
            };

            // Use bounded heap to maintain only top-N results during iteration.
            // This bounds memory to O(semantic_limit) instead of O(total_chunks).
            let mut score_heap = BoundedScoreHeap::new(semantic_limit);

            // Cursor-based batching: load embeddings in batches of 5000 instead of
            // all at once. This bounds memory to O(batch_size) instead of O(total_chunks).
            // Uses the same cursor pattern as EmbeddingBatchIterator in store/chunks.rs.
            const BRUTE_FORCE_BATCH_SIZE: i64 = 5000;
            let mut last_rowid: i64 = 0;

            // Hoist SQL template out of cursor loop — only last_rowid changes per iteration
            let rowid_condition = format!("rowid > ?{}", fsql.bind_values.len() + 1);
            let limit_param = format!("?{}", fsql.bind_values.len() + 2);
            let batch_where = if fsql.conditions.is_empty() {
                format!(
                    " WHERE {} ORDER BY rowid ASC LIMIT {}",
                    rowid_condition, limit_param
                )
            } else {
                format!(
                    " WHERE {} AND {} ORDER BY rowid ASC LIMIT {}",
                    fsql.conditions.join(" AND "),
                    rowid_condition,
                    limit_param
                )
            };
            let sql = format!("SELECT {} FROM chunks{}", fsql.columns, batch_where);

            loop {
                let batch: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for val in &fsql.bind_values {
                        q = q.bind(val);
                    }
                    q = q.bind(last_rowid);
                    q = q.bind(BRUTE_FORCE_BATCH_SIZE);
                    q.fetch_all(&self.pool).await?
                };

                if batch.is_empty() {
                    break;
                }
                last_rowid = batch
                    .last()
                    .expect("batch non-empty checked above")
                    .get::<i64, _>("rowid");

                for row in &batch {
                    let id: String = row.get("id");
                    let embedding_bytes: Vec<u8> = row.get("embedding");
                    let name: Option<String> = if need_name { row.get("name") } else { None };

                    let Some(embedding) = embedding_slice(&embedding_bytes, self.dim) else {
                        continue;
                    };
                    let file_part = extract_file_from_chunk_id(&id);

                    if let Some(score) =
                        score_candidate(embedding, name.as_deref(), file_part, &scoring_ctx)
                    {
                        score_heap.push(id, score);
                    }
                }
            }

            let scored = score_heap.into_sorted_vec();

            let results = self
                .finalize_results(
                    scored,
                    &filter.query_text,
                    fsql.use_rrf,
                    limit,
                    filter.path_pattern.as_deref(),
                )
                .await?;

            tracing::debug!(count = results.len(), "search_filtered complete");
            Ok(results)
        })
    }

    /// Post-scoring pipeline: RRF fusion, content fetch, parent dedup, boost, truncate.
    ///
    /// Shared by `search_filtered` and `search_by_candidate_ids`. Both produce
    /// `Vec<(chunk_id, score)>` through different scoring paths (brute-force vs
    /// index-guided), then converge here for the same finalization steps.
    ///
    /// When `use_rrf` is true, fuses semantic rankings with FTS keyword results
    /// via Reciprocal Rank Fusion before fetching full content. Requests `limit * 2`
    /// candidates from RRF to compensate for parent dedup filtering.
    async fn finalize_results(
        &self,
        mut scored: Vec<(String, f32)>,
        query_text: &str,
        use_rrf: bool,
        limit: usize,
        path_pattern: Option<&str>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        // Step 1: RRF fusion with FTS keyword search, or plain truncate
        let final_scored: Vec<(String, f32)> = if use_rrf {
            let normalized = normalize_for_fts(query_text);
            let sanitized = sanitize_fts_query(&normalized);
            let fts_ids = if sanitized.is_empty() {
                vec![]
            } else {
                let fts_rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
                )
                .bind(&sanitized)
                .bind((limit * 3) as i64)
                .fetch_all(&self.pool)
                .await?;
                // Apply path filter to FTS results (FTS5 doesn't support JOIN filtering)
                let fts_all: Vec<String> = fts_rows.into_iter().map(|(id,)| id).collect();
                let path_owned = path_pattern.map(String::from);
                if let Some(fts_glob) = compile_glob_filter(path_owned.as_ref()) {
                    fts_all
                        .into_iter()
                        .filter(|id| {
                            let file = extract_file_from_chunk_id(id);
                            fts_glob.is_match(file)
                        })
                        .collect()
                } else {
                    fts_all
                }
            };
            let semantic_ids: Vec<&str> = scored.iter().map(|(id, _)| id.as_str()).collect();
            // Request extra candidates from RRF to compensate for parent dedup
            // filtering below — dedup can drop results, leaving fewer than `limit`.
            Self::rrf_fuse(&semantic_ids, &fts_ids, limit * 2)
        } else {
            scored.truncate(limit);
            scored
        };

        if final_scored.is_empty() {
            return Ok(vec![]);
        }

        // Step 2: Fetch full content only for top-N results (PF-5 payoff —
        // heavy content/doc/signature columns loaded only for winners)
        let ids: Vec<&str> = final_scored.iter().map(|(id, _)| id.as_str()).collect();
        let rows_map = self.fetch_chunks_by_ids_async(&ids).await?;

        // Step 3: Parent dedup — keep first occurrence per parent_id
        let mut seen_parents: HashSet<String> = HashSet::new();
        let mut results: Vec<SearchResult> = final_scored
            .into_iter()
            .filter_map(|(id, score)| {
                let row = rows_map.get(&id)?;
                let dedup_key = row.parent_id.clone().unwrap_or_else(|| row.id.clone());
                if seen_parents.insert(dedup_key) {
                    Some(SearchResult {
                        chunk: ChunkSummary::from(row.clone()),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Step 4: Boost container chunks when multiple child methods appear
        apply_parent_boost(&mut results);

        // Step 5: Truncate back to requested limit after parent dedup
        results.truncate(limit);

        Ok(results)
    }

    /// Search with optional vector index for O(log n) candidate retrieval
    pub fn search_filtered_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if let Some(idx) = index {
            let _span = tracing::info_span!("search_index_guided", limit = limit).entered();

            let candidate_count = (limit * 5).max(100);
            let index_results = idx.search(query, candidate_count);

            if index_results.is_empty() {
                tracing::info!("Index returned no candidates, falling back to brute-force search (performance may degrade)");
                return self.search_filtered(query, filter, limit, threshold);
            }

            tracing::debug!("Index returned {} candidates", index_results.len());

            let candidate_ids: Vec<&str> = index_results.iter().map(|r| r.id.as_str()).collect();
            return self.search_by_candidate_ids(&candidate_ids, query, filter, limit, threshold);
        }

        self.search_filtered(query, filter, limit, threshold)
    }

    /// Search within a set of candidate IDs (for HNSW-guided filtered search)
    pub fn search_by_candidate_ids(
        &self,
        candidate_ids: &[&str],
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!(
            "search_by_candidates",
            candidates = candidate_ids.len(),
            limit
        )
        .entered();

        if candidate_ids.is_empty() {
            return Ok(vec![]);
        }

        let use_hybrid = filter.name_boost > 0.0
            && !filter.query_text.is_empty()
            && is_name_like_query(&filter.query_text);
        let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();

        // Load notes once for note-boosted ranking
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                Vec::new()
            }
        };

        self.rt.block_on(async {
            // Phase 1: Lightweight candidate fetch — only scoring fields + embedding.
            // Excludes heavy content/doc/signature columns (PF-5).
            let candidates = self.fetch_candidates_by_ids_async(candidate_ids).await?;

            // Compile glob pattern once outside the loop (not per-chunk).
            let glob_matcher = compile_glob_filter(filter.path_pattern.as_ref());

            // Pre-tokenize query for name matching (avoids re-tokenizing per result)
            let name_matcher = if use_hybrid {
                Some(NameMatcher::new(&filter.query_text))
            } else {
                None
            };

            // Pre-compute note boost lookup for O(1) name matching in scoring loop
            let note_index = NoteBoostIndex::new(&notes);

            // Build loop-invariant scoring context once
            let scoring_ctx = ScoringContext {
                query: query.as_slice(),
                filter,
                name_matcher: name_matcher.as_ref(),
                glob_matcher: glob_matcher.as_ref(),
                note_index: &note_index,
                threshold,
            };

            // Pre-build filter sets once — avoids per-candidate string parsing (PF-1)
            let lang_set: Option<HashSet<String>> = filter
                .languages
                .as_ref()
                .map(|langs| langs.iter().map(|l| l.to_string().to_lowercase()).collect());
            let type_set: Option<HashSet<String>> = filter
                .chunk_types
                .as_ref()
                .map(|types| types.iter().map(|t| t.to_string().to_lowercase()).collect());

            let mut scored: Vec<(CandidateRow, f32)> = candidates
                .into_iter()
                .filter_map(|(candidate, embedding_bytes)| {
                    // PERF-27: Use HashSet::contains with lowercased value instead of
                    // linear scan with eq_ignore_ascii_case. Values in the sets are
                    // already lowercased at construction above.
                    if let Some(ref langs) = lang_set {
                        if !langs.contains(&candidate.language.to_lowercase()) {
                            return None;
                        }
                    }

                    if let Some(ref types) = type_set {
                        if !types.contains(&candidate.chunk_type.to_lowercase()) {
                            return None;
                        }
                    }

                    let embedding = embedding_slice(&embedding_bytes, self.dim)?;

                    let score = score_candidate(
                        embedding,
                        Some(&candidate.name),
                        &candidate.origin,
                        &scoring_ctx,
                    )?;

                    Some((candidate, score))
                })
                .collect();

            scored.sort_by(|a, b| b.1.total_cmp(&a.1));

            let scored: Vec<(String, f32)> =
                scored.into_iter().map(|(c, score)| (c.id, score)).collect();

            self.finalize_results(
                scored,
                &filter.query_text,
                use_rrf,
                limit,
                filter.path_pattern.as_deref(),
            )
            .await
        })
    }

    /// Unified search with optional vector index.
    ///
    /// Returns code-only results (SQ-9: notes removed from search pipeline).
    /// When an HNSW index is provided, uses O(log n) candidate retrieval.
    pub fn search_unified_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<crate::store::UnifiedResult>, StoreError> {
        if limit == 0 {
            return Ok(vec![]);
        }

        let _span = tracing::info_span!("search_unified", limit, threshold = %threshold).entered();

        let code_results =
            self.search_filtered_with_index(query, filter, limit, threshold, index)?;

        let unified: Vec<crate::store::UnifiedResult> = code_results
            .into_iter()
            .map(crate::store::UnifiedResult::Code)
            .collect();

        Ok(unified)
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{ChunkType, Language};
    use crate::store::helpers::SearchFilter;
    use crate::test_helpers::{mock_embedding, setup_store};
    use std::path::PathBuf;

    /// Constructs a mock `Chunk` with the provided metadata and a placeholder function body.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the function chunk.
    /// * `file` - The file path where the chunk is located.
    /// * `lang` - The programming language of the chunk.
    /// * `chunk_type` - The type classification of the chunk.
    ///
    /// # Returns
    ///
    /// A new `Chunk` struct with a generated ID based on the file path and content hash, mock function signature and content, and default values for other fields.
    fn make_chunk(
        name: &str,
        file: &str,
        lang: Language,
        chunk_type: ChunkType,
    ) -> crate::parser::Chunk {
        let content = format!("fn {}() {{ /* body */ }}", name);
        let hash = blake3::hash(content.as_bytes()).to_hex().to_string();
        crate::parser::Chunk {
            id: format!("{}:1:{}", file, &hash[..8]),
            file: PathBuf::from(file),
            language: lang,
            chunk_type,
            name: name.to_string(),
            signature: format!("fn {}()", name),
            content,
            doc: None,
            line_start: 1,
            line_end: 5,
            content_hash: hash,
            parent_id: None,
            window_idx: None,
            parent_type_name: None,
        }
    }

    #[test]
    fn test_search_filtered_language_filter() {
        let (store, _dir) = setup_store();

        let rust_chunk = make_chunk("rust_fn", "src/lib.rs", Language::Rust, ChunkType::Function);
        let py_chunk = make_chunk(
            "py_fn",
            "src/main.py",
            Language::Python,
            ChunkType::Function,
        );
        let emb = mock_embedding(1.0);

        store
            .upsert_chunks_batch(
                &[(rust_chunk, emb.clone()), (py_chunk, emb.clone())],
                Some(12345),
            )
            .unwrap();

        let filter = SearchFilter {
            languages: Some(vec![Language::Rust]),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.language, Language::Rust);
    }

    #[test]
    fn test_search_filtered_chunk_type_filter() {
        let (store, _dir) = setup_store();

        let fn_chunk = make_chunk("my_fn", "src/a.rs", Language::Rust, ChunkType::Function);
        let struct_chunk = make_chunk("MyStruct", "src/b.rs", Language::Rust, ChunkType::Struct);
        let emb = mock_embedding(1.0);

        store
            .upsert_chunks_batch(
                &[(fn_chunk, emb.clone()), (struct_chunk, emb.clone())],
                Some(12345),
            )
            .unwrap();

        let filter = SearchFilter {
            chunk_types: Some(vec![ChunkType::Struct]),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.chunk_type, ChunkType::Struct);
    }

    #[test]
    fn test_search_filtered_path_pattern() {
        let (store, _dir) = setup_store();

        let src_chunk = make_chunk("src_fn", "src/lib.rs", Language::Rust, ChunkType::Function);
        let test_chunk = make_chunk(
            "test_fn",
            "tests/test.rs",
            Language::Rust,
            ChunkType::Function,
        );
        let emb = mock_embedding(1.0);

        store
            .upsert_chunks_batch(
                &[(src_chunk, emb.clone()), (test_chunk, emb.clone())],
                Some(12345),
            )
            .unwrap();

        let filter = SearchFilter {
            path_pattern: Some("src/**".to_string()),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.name, "src_fn");
    }

    #[test]
    fn test_search_filtered_combined_filters() {
        let (store, _dir) = setup_store();

        let rust_src = make_chunk("rs_src", "src/a.rs", Language::Rust, ChunkType::Function);
        let py_src = make_chunk("py_src", "src/b.py", Language::Python, ChunkType::Function);
        let rust_test = make_chunk("rs_test", "tests/t.rs", Language::Rust, ChunkType::Function);
        let emb = mock_embedding(1.0);

        store
            .upsert_chunks_batch(
                &[
                    (rust_src, emb.clone()),
                    (py_src, emb.clone()),
                    (rust_test, emb.clone()),
                ],
                Some(12345),
            )
            .unwrap();

        let filter = SearchFilter {
            languages: Some(vec![Language::Rust]),
            path_pattern: Some("src/**".to_string()),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk.name, "rs_src");
    }

    #[test]
    fn test_search_filtered_rrf_hybrid() {
        let (store, _dir) = setup_store();

        let chunk = make_chunk(
            "handleError",
            "src/err.rs",
            Language::Rust,
            ChunkType::Function,
        );
        let emb = mock_embedding(1.0);
        store
            .upsert_chunks_batch(&[(chunk, emb.clone())], Some(12345))
            .unwrap();

        let filter = SearchFilter {
            enable_rrf: true,
            query_text: "error handling".to_string(),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert!(!results.is_empty(), "RRF hybrid should return results");
    }

    #[test]
    fn test_search_filtered_name_boost() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk(
            "parseConfig",
            "src/a.rs",
            Language::Rust,
            ChunkType::Function,
        );
        let c2 = make_chunk("renderUI", "src/b.rs", Language::Rust, ChunkType::Function);
        let emb = mock_embedding(1.0);

        store
            .upsert_chunks_batch(&[(c1, emb.clone()), (c2, emb.clone())], Some(12345))
            .unwrap();

        // With name_boost, parseConfig should rank higher for query "parse"
        let filter = SearchFilter {
            name_boost: 0.3,
            query_text: "parseConfig".to_string(),
            ..Default::default()
        };
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert!(!results.is_empty());
        // The chunk whose name matches query text should rank first
        assert_eq!(results[0].chunk.name, "parseConfig");
    }

    #[test]
    fn test_search_filtered_empty_store() {
        let (store, _dir) = setup_store();
        let emb = mock_embedding(1.0);
        let filter = SearchFilter::default();
        let results = store.search_filtered(&emb, &filter, 10, 0.0).unwrap();
        assert!(results.is_empty());
    }

    /// TC-7: Verify HNSW-guided path produces RRF results when enable_rrf is true.
    ///
    /// The search_by_candidate_ids path must apply the same RRF fusion as
    /// search_filtered, combining cosine-scored candidates with FTS keyword hits.
    #[test]
    fn test_search_by_candidate_ids_rrf() {
        let (store, _dir) = setup_store();

        // Insert chunks with content that FTS can match by keyword
        let mut c_error = make_chunk(
            "handleError",
            "src/err.rs",
            Language::Rust,
            ChunkType::Function,
        );
        c_error.content = "fn handleError() { log_error(\"error handling failed\"); }".to_string();
        let mut c_parse = make_chunk(
            "parseConfig",
            "src/cfg.rs",
            Language::Rust,
            ChunkType::Function,
        );
        c_parse.content = "fn parseConfig() { read_toml(\"config.toml\"); }".to_string();
        let emb1 = mock_embedding(1.0);
        let emb2 = mock_embedding(0.9);

        store
            .upsert_chunks_batch(
                &[(c_error.clone(), emb1.clone()), (c_parse.clone(), emb2)],
                Some(12345),
            )
            .unwrap();

        // Search by candidate IDs with RRF enabled — FTS should boost "handleError"
        // for the query text "error handling"
        let candidate_ids: Vec<&str> = vec![&c_error.id, &c_parse.id];
        let filter = SearchFilter {
            enable_rrf: true,
            query_text: "error handling".to_string(),
            ..Default::default()
        };

        let results = store
            .search_by_candidate_ids(&candidate_ids, &emb1, &filter, 10, 0.0)
            .unwrap();

        assert!(
            !results.is_empty(),
            "RRF in candidate path should return results"
        );
        // "handleError" should rank first because it matches both semantically
        // and via FTS keyword "error"
        assert_eq!(
            results[0].chunk.name, "handleError",
            "FTS+RRF should boost the keyword-matching chunk"
        );

        // Compare with non-RRF path to verify RRF actually changes behavior
        let filter_no_rrf = SearchFilter {
            enable_rrf: false,
            query_text: "error handling".to_string(),
            ..Default::default()
        };
        let results_no_rrf = store
            .search_by_candidate_ids(&candidate_ids, &emb1, &filter_no_rrf, 10, 0.0)
            .unwrap();
        assert!(
            !results_no_rrf.is_empty(),
            "Non-RRF candidate path should also return results"
        );
    }

    #[test]
    fn test_search_filtered_respects_threshold() {
        let (store, _dir) = setup_store();

        let c1 = make_chunk("fn_a", "src/a.rs", Language::Rust, ChunkType::Function);
        let emb_opposite = mock_embedding(-1.0);
        store
            .upsert_chunks_batch(&[(c1, emb_opposite)], Some(12345))
            .unwrap();

        let query = mock_embedding(1.0);
        let filter = SearchFilter::default();
        let results = store.search_filtered(&query, &filter, 10, 0.99).unwrap();
        assert!(
            results.is_empty(),
            "Opposite embedding should not meet 0.99 threshold"
        );
    }

    #[test]
    fn test_search_filtered_respects_limit() {
        let (store, _dir) = setup_store();

        for i in 0..10 {
            let c = make_chunk(
                &format!("fn_{}", i),
                &format!("src/{}.rs", i),
                Language::Rust,
                ChunkType::Function,
            );
            let emb = mock_embedding(1.0 + i as f32 * 0.001);
            store.upsert_chunks_batch(&[(c, emb)], Some(12345)).unwrap();
        }

        let query = mock_embedding(1.0);
        let filter = SearchFilter::default();
        let results = store.search_filtered(&query, &filter, 3, 0.0).unwrap();
        assert_eq!(results.len(), 3);
    }
}
