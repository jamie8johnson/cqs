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
use crate::parser::ChunkType;
use crate::store::helpers::{
    embedding_slice, CandidateRow, ChunkSummary, SearchFilter, SearchResult,
};
use crate::store::sanitize_fts_query;
use crate::store::{NoteSummary, Store, StoreError};

use super::scoring::{
    apply_parent_boost, apply_scoring_pipeline, build_filter_sql, compile_glob_filter,
    extract_file_from_chunk_id, score_candidate, BoundedScoreHeap, NameMatcher, NoteBoostIndex,
    ScoringContext,
};
use super::synonyms::expand_query_for_fts;

/// Default multiplicative boost applied to chunks whose type matches the
/// router-provided type hints. Phase 5 placeholder; never empirically swept.
pub(crate) const DEFAULT_TYPE_BOOST_FACTOR: f32 = 1.2;

/// Resolve the type-boost factor used by `finalize_results` Step 4b.
///
/// Reads `CQS_TYPE_BOOST` from the environment if set; otherwise falls back
/// to [`DEFAULT_TYPE_BOOST_FACTOR`] (1.2x). Invalid values (non-numeric,
/// non-finite, ≤ 0) log a warning and fall back to the default — we never
/// want a typo'd env var to multiply scores by zero or NaN.
///
/// Re-reads the env var on every call (env::var is a single syscall and we
/// hit this at most once per search). This is the contract that
/// `evals/run_sweep.py` relies on: spawn a fresh `cqs` invocation per value
/// of `CQS_TYPE_BOOST`, no process-level caching to defeat the sweep.
pub(crate) fn type_boost_factor() -> f32 {
    let raw = match std::env::var("CQS_TYPE_BOOST") {
        Ok(v) => v,
        Err(_) => {
            tracing::debug!(
                factor = DEFAULT_TYPE_BOOST_FACTOR,
                "CQS_TYPE_BOOST unset, using default type boost"
            );
            return DEFAULT_TYPE_BOOST_FACTOR;
        }
    };
    match raw.parse::<f32>() {
        Ok(v) if v.is_finite() && v > 0.0 => {
            tracing::debug!(
                factor = v,
                source = "CQS_TYPE_BOOST",
                "Type boost factor set from env var"
            );
            v
        }
        Ok(v) => {
            tracing::warn!(
                raw = %raw,
                parsed = v,
                fallback = DEFAULT_TYPE_BOOST_FACTOR,
                "CQS_TYPE_BOOST is non-finite or non-positive — using default"
            );
            DEFAULT_TYPE_BOOST_FACTOR
        }
        Err(e) => {
            tracing::warn!(
                raw = %raw,
                error = %e,
                fallback = DEFAULT_TYPE_BOOST_FACTOR,
                "CQS_TYPE_BOOST not parseable as f32 — using default"
            );
            DEFAULT_TYPE_BOOST_FACTOR
        }
    }
}

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
        let _span =
            tracing::info_span!("search_filtered", limit, threshold, rrf = filter.enable_rrf)
                .entered();
        // Load notes once for note-boosted ranking (cheap — no embeddings)
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                std::sync::Arc::new(Vec::new())
            }
        };
        self.search_filtered_with_notes(query, filter, limit, threshold, &notes)
    }

    /// Inner implementation of `search_filtered` that accepts pre-loaded notes.
    fn search_filtered_with_notes(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        notes: &[NoteSummary],
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_filtered", limit = limit, rrf = filter.enable_rrf)
            .entered();

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
            let note_index = NoteBoostIndex::new(notes);

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

                    let embedding = match embedding_slice(&embedding_bytes, self.dim) {
                        Ok(e) => e,
                        Err(_) => continue,
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
                    filter.type_boost_types.as_deref(),
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
        type_boost_types: Option<&[ChunkType]>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        // Step 1: RRF fusion with FTS keyword search, or plain truncate
        let final_scored: Vec<(String, f32)> = if use_rrf {
            let normalized = normalize_for_fts(query_text);
            let sanitized = sanitize_fts_query(&normalized);
            let expanded = expand_query_for_fts(&sanitized);
            let fts_query = if expanded.is_empty() {
                sanitized.clone()
            } else {
                expanded
            };
            let fts_ids = if fts_query.is_empty() {
                vec![]
            } else {
                tracing::debug!(fts_query = %fts_query, "FTS MATCH query");
                let fts_rows: Vec<(String,)> = sqlx::query_as(
                    "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
                )
                .bind(&fts_query)
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
        let mut rows_map = self.fetch_chunks_by_ids_async(&ids).await?;

        // Step 3: Parent dedup — keep first occurrence per parent_id.
        // Use remove() instead of get()+clone() to avoid copying 10+ Strings per result (PERF-6).
        let mut seen_parents: HashSet<String> = HashSet::new();
        let mut results: Vec<SearchResult> = final_scored
            .into_iter()
            .filter_map(|(id, score)| {
                let row = rows_map.remove(&id)?;
                let dedup_key = row.parent_id.clone().unwrap_or_else(|| row.id.clone());
                if seen_parents.insert(dedup_key) {
                    Some(SearchResult {
                        chunk: ChunkSummary::from(row),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Step 4: Boost container chunks when multiple child methods appear
        apply_parent_boost(&mut results);

        // Step 4b: Type boost from adaptive routing.
        //
        // Default 1.2x for matching types, overridable via CQS_TYPE_BOOST env
        // var so we can sweep this knob without rebuilding the binary. The
        // 1.2x default is a Phase 5 placeholder — see
        // docs/plans/adaptive-retrieval.md and the open question
        // "Should type boost factor be configurable? (Later — hardcode 1.2x for v1)".
        // Empirical sweep is queued in the roadmap.
        //
        // Boost is multiplicative (not additive) so it stays scale-invariant
        // across cosine [0,1] and re-ranker scores [-inf, inf]. Boost == 1.0
        // is the no-op default for callers that haven't opted in via
        // type_boost_types.
        if let Some(boost_types) = type_boost_types {
            let boost = type_boost_factor();
            for result in &mut results {
                if boost_types.contains(&result.chunk.chunk_type) {
                    result.score *= boost;
                }
            }
            // Re-sort after boost
            results.sort_by(|a, b| b.score.total_cmp(&a.score));
        }

        // Step 5: Truncate back to requested limit after parent dedup
        results.truncate(limit);

        Ok(results)
    }

    /// Search with optional vector index for O(log n) candidate retrieval
    /// Search with optional SPLADE sparse-dense fusion.
    ///
    /// When `splade` is Some and `filter.enable_splade` is true, fuses dense
    /// (cosine) and sparse (SPLADE) results via linear interpolation.
    pub fn search_hybrid(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
        splade: Option<(
            &crate::splade::index::SpladeIndex,
            &crate::splade::SparseVector,
        )>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        // If SPLADE is not enabled or not available, delegate to standard path
        if !filter.enable_splade || splade.is_none() {
            return self.search_filtered_with_index(query, filter, limit, threshold, index);
        }

        let (splade_index, sparse_query) = splade.unwrap();
        let _span = tracing::info_span!("search_hybrid", limit, enable_splade = true).entered();

        // Load notes once for all paths
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                std::sync::Arc::new(Vec::new())
            }
        };

        let candidate_count = (limit * 5).max(100);

        // Build chunk filter predicate
        let meta = self.chunk_type_language_map()?;
        let include_types = filter.include_types.as_ref();
        let exclude_types = filter.exclude_types.as_ref();
        let languages = filter.languages.as_ref();
        let predicate = |chunk_id: &str| -> bool {
            if include_types.is_none() && exclude_types.is_none() && languages.is_none() {
                return true;
            }
            if let Some((ct, lang)) = meta.get(chunk_id) {
                let type_ok = include_types.is_none_or(|types| types.contains(ct));
                let exclude_ok = exclude_types.is_none_or(|types| !types.contains(ct));
                let lang_ok = languages.is_none_or(|langs| langs.contains(lang));
                type_ok && exclude_ok && lang_ok
            } else {
                false
            }
        };

        // Dense results from vector index (HNSW or CAGRA)
        let dense_results = if let Some(idx) = index {
            idx.search_with_filter(query, candidate_count, &predicate)
        } else {
            tracing::warn!("No vector index available for dense leg of hybrid search");
            Vec::new()
        };

        // Sparse results from SPLADE inverted index
        let sparse_results =
            splade_index.search_with_filter(sparse_query, candidate_count, &predicate);

        tracing::debug!(
            dense = dense_results.len(),
            sparse = sparse_results.len(),
            "Hybrid search: fusing results"
        );

        // Normalize sparse scores to [0, 1] via min-max
        let max_sparse = sparse_results
            .iter()
            .map(|r| r.score)
            .fold(0.0f32, f32::max);

        // Build score maps
        let mut dense_scores: std::collections::HashMap<&str, f32> =
            std::collections::HashMap::new();
        for r in &dense_results {
            dense_scores.insert(&r.id, r.score);
        }
        let mut sparse_scores: std::collections::HashMap<&str, f32> =
            std::collections::HashMap::new();
        for r in &sparse_results {
            let normalized = if max_sparse > 0.0 {
                r.score / max_sparse
            } else {
                0.0
            };
            sparse_scores.insert(&r.id, normalized);
        }

        // Union of all candidate IDs
        let mut all_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for r in &dense_results {
            all_ids.insert(&r.id);
        }
        for r in &sparse_results {
            all_ids.insert(&r.id);
        }

        // Fuse with linear interpolation: final = α * dense + (1-α) * sparse
        let alpha = filter.splade_alpha;
        tracing::debug!(
            alpha,
            dense = dense_scores.len(),
            sparse = sparse_scores.len(),
            "SPLADE fusion"
        );
        let mut fused: Vec<crate::index::IndexResult> = all_ids
            .iter()
            .map(|id| {
                let d = dense_scores.get(id).copied().unwrap_or(0.0);
                let s = sparse_scores.get(id).copied().unwrap_or(0.0);
                let score = if alpha <= 0.0 {
                    // Pure re-rank mode: SPLADE score for chunks it found,
                    // cosine score (demoted) for chunks it didn't.
                    // This preserves cosine ordering for SPLADE-unknown chunks
                    // while letting SPLADE override when it has signal.
                    if s > 0.0 {
                        1.0 + s
                    } else {
                        d
                    }
                } else {
                    alpha * d + (1.0 - alpha) * s
                };
                crate::index::IndexResult {
                    id: id.to_string(),
                    score,
                }
            })
            .collect();
        fused.sort_by(|a, b| b.score.total_cmp(&a.score));
        fused.truncate(candidate_count);

        tracing::debug!(fused = fused.len(), alpha, "Hybrid fusion complete");

        let fused_map: std::collections::HashMap<String, f32> =
            fused.iter().map(|r| (r.id.clone(), r.score)).collect();
        let candidate_ids: Vec<&str> = fused.iter().map(|r| r.id.as_str()).collect();
        self.search_by_candidate_ids_with_notes(
            &candidate_ids,
            query,
            filter,
            limit,
            threshold,
            &notes,
            Some(&fused_map),
        )
    }

    pub fn search_filtered_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        // PERF-44: Load notes once for all search paths
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                std::sync::Arc::new(Vec::new())
            }
        };

        if let Some(idx) = index {
            let _span = tracing::info_span!("search_index_guided", limit = limit).entered();

            let candidate_count = (limit * 5).max(100);
            let has_type_or_lang_filter = filter.include_types.is_some()
                || filter.exclude_types.is_some()
                || filter.languages.is_some();

            let index_results = if has_type_or_lang_filter {
                // Build traversal-time filter from chunk metadata
                let meta = self.chunk_type_language_map()?;
                let include_types = filter.include_types.as_ref();
                let exclude_types = filter.exclude_types.as_ref();
                let languages = filter.languages.as_ref();
                let predicate = |chunk_id: &str| -> bool {
                    if let Some((ct, lang)) = meta.get(chunk_id) {
                        let type_ok = include_types.is_none_or(|types| types.contains(ct));
                        let exclude_ok = exclude_types.is_none_or(|types| !types.contains(ct));
                        let lang_ok = languages.is_none_or(|langs| langs.contains(lang));
                        type_ok && exclude_ok && lang_ok
                    } else {
                        false
                    }
                };
                idx.search_with_filter(query, candidate_count, &predicate)
            } else {
                idx.search(query, candidate_count)
            };

            if index_results.is_empty() {
                tracing::info!("Index returned no candidates, falling back to brute-force search (performance may degrade)");
                return self.search_filtered_with_notes(query, filter, limit, threshold, &notes);
            }

            tracing::debug!("Index returned {} candidates", index_results.len());

            let candidate_ids: Vec<&str> = index_results.iter().map(|r| r.id.as_str()).collect();
            return self.search_by_candidate_ids_with_notes(
                &candidate_ids,
                query,
                filter,
                limit,
                threshold,
                &notes,
                None,
            );
        }

        self.search_filtered_with_notes(query, filter, limit, threshold, &notes)
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
        // Load notes once for note-boosted ranking
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                std::sync::Arc::new(Vec::new())
            }
        };
        self.search_by_candidate_ids_with_notes(
            candidate_ids,
            query,
            filter,
            limit,
            threshold,
            &notes,
            None,
        )
    }

    /// Inner implementation of `search_by_candidate_ids` that accepts pre-loaded notes
    /// and optional pre-fused scores from hybrid search.
    ///
    /// When `fused_scores` is `Some`, candidates with a fused score entry use that
    /// score as the base (replacing cosine similarity) while still applying name
    /// boost, note boost, demotion, and threshold filtering.
    #[allow(clippy::too_many_arguments)]
    fn search_by_candidate_ids_with_notes(
        &self,
        candidate_ids: &[&str],
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        notes: &[NoteSummary],
        fused_scores: Option<&std::collections::HashMap<String, f32>>,
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

        // AC-24: Reuse flag computation from build_filter_sql to stay consistent
        let flags = build_filter_sql(filter);
        let use_hybrid = flags.use_hybrid;
        let use_rrf = flags.use_rrf;

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
            let note_index = NoteBoostIndex::new(notes);

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
                .include_types
                .as_ref()
                .map(|types| types.iter().map(|t| t.to_string().to_lowercase()).collect());

            let mut scored: Vec<(CandidateRow, f32)> = candidates
                .into_iter()
                .filter_map(|(candidate, embedding_bytes)| {
                    // v1.22.0 audit PF-7: previously called `.to_lowercase()`
                    // per candidate (500+ String allocations per search). DB
                    // values are already canonical lowercase from
                    // Language::to_string / ChunkType::to_string, so use
                    // direct contains on the pre-lowercased set.
                    if let Some(ref langs) = lang_set {
                        if !langs.contains(&candidate.language) {
                            return None;
                        }
                    }

                    if let Some(ref types) = type_set {
                        if !types.contains(&candidate.chunk_type) {
                            return None;
                        }
                    }

                    let score =
                        if let Some(&fused) = fused_scores.and_then(|fs| fs.get(&candidate.id)) {
                            apply_scoring_pipeline(
                                fused,
                                Some(&candidate.name),
                                &candidate.origin,
                                &scoring_ctx,
                            )?
                        } else {
                            let embedding = embedding_slice(&embedding_bytes, self.dim).ok()?;
                            score_candidate(
                                embedding,
                                Some(&candidate.name),
                                &candidate.origin,
                                &scoring_ctx,
                            )?
                        };

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
                filter.type_boost_types.as_deref(),
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
    use super::{type_boost_factor, DEFAULT_TYPE_BOOST_FACTOR};
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
            include_types: Some(vec![ChunkType::Struct]),
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
            enable_rrf: true, // Test needs RRF on
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
            enable_rrf: true, // Test needs RRF on
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

    // ===== type_boost_factor() tests =====

    use std::sync::Mutex;
    /// Process-wide lock for env-touching tests. CQS_TYPE_BOOST is global
    /// state — parallel tests would race if they didn't serialize.
    static TYPE_BOOST_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Default fallback when env var is unset.
    #[test]
    fn test_type_boost_factor_default_when_unset() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        std::env::remove_var("CQS_TYPE_BOOST");
        assert_eq!(type_boost_factor(), DEFAULT_TYPE_BOOST_FACTOR);
    }

    /// Valid float values are honored.
    #[test]
    fn test_type_boost_factor_valid_value() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        for valid in &["1.0", "1.05", "1.5", "2.0", "0.5"] {
            std::env::set_var("CQS_TYPE_BOOST", valid);
            let parsed: f32 = valid.parse().unwrap();
            assert!(
                (type_boost_factor() - parsed).abs() < 1e-6,
                "CQS_TYPE_BOOST={valid} should produce {parsed}",
            );
        }
        std::env::remove_var("CQS_TYPE_BOOST");
    }

    /// Empty string env var is treated as a parse error → default fallback.
    /// (Bash gotcha: `export CQS_TYPE_BOOST=` shouldn't break the search.)
    #[test]
    fn test_type_boost_factor_empty_falls_back() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_TYPE_BOOST", "");
        assert_eq!(type_boost_factor(), DEFAULT_TYPE_BOOST_FACTOR);
        std::env::remove_var("CQS_TYPE_BOOST");
    }

    /// Garbage values fall back to the default rather than poisoning scoring.
    #[test]
    fn test_type_boost_factor_invalid_falls_back() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        for garbage in &["abc", "1.2x", "true", "--", "1.0e", "1,2"] {
            std::env::set_var("CQS_TYPE_BOOST", garbage);
            assert_eq!(
                type_boost_factor(),
                DEFAULT_TYPE_BOOST_FACTOR,
                "CQS_TYPE_BOOST={garbage:?} should fall back to default",
            );
        }
        std::env::remove_var("CQS_TYPE_BOOST");
    }

    /// Non-positive and non-finite values must NOT silently zero out scores.
    /// This is the load-bearing safety property — without it, a typo'd
    /// env var like `CQS_TYPE_BOOST=0` would multiply matching chunks
    /// to score 0 and effectively *exclude* them, which would silently
    /// destroy recall.
    #[test]
    fn test_type_boost_factor_rejects_zero_negative_nan_inf() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        for unsafe_val in &["0", "0.0", "-1.0", "-0.5", "NaN", "nan", "inf", "-inf"] {
            std::env::set_var("CQS_TYPE_BOOST", unsafe_val);
            assert_eq!(
                type_boost_factor(),
                DEFAULT_TYPE_BOOST_FACTOR,
                "CQS_TYPE_BOOST={unsafe_val:?} must be rejected — \
                 a non-positive or non-finite boost would corrupt scoring",
            );
        }
        std::env::remove_var("CQS_TYPE_BOOST");
    }

    /// The function re-reads the env var on every call (no caching) so a
    /// process can vary the boost across calls. Critical for tests but
    /// also for any future code that wants to use the boost factor in
    /// a long-running process.
    #[test]
    fn test_type_boost_factor_reads_env_on_each_call() {
        let _guard = TYPE_BOOST_ENV_LOCK.lock().unwrap();
        std::env::set_var("CQS_TYPE_BOOST", "1.3");
        let first = type_boost_factor();
        std::env::set_var("CQS_TYPE_BOOST", "1.7");
        let second = type_boost_factor();
        std::env::remove_var("CQS_TYPE_BOOST");
        assert!((first - 1.3).abs() < 1e-6);
        assert!((second - 1.7).abs() < 1e-6);
    }
}
