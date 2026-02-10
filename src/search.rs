//! Search algorithms and name matching
//!
//! Implements search methods on Store for semantic, hybrid, and index-guided
//! search. See `math.rs` for similarity scoring.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use sqlx::Row;

use crate::embedder::Embedding;
use crate::index::VectorIndex;
use crate::math::cosine_similarity;
use crate::nl::normalize_for_fts;
use crate::nl::tokenize_identifier;
use crate::store::helpers::{embedding_slice, ChunkRow, ChunkSummary, SearchFilter, SearchResult};
use crate::store::{Store, StoreError, UnifiedResult};

// ============ Target Resolution ============

/// Parse a target string into (optional_file_filter, function_name).
///
/// Supports formats:
/// - `"function_name"` -> (None, "function_name")
/// - `"path/to/file.rs:function_name"` -> (Some("path/to/file.rs"), "function_name")
pub fn parse_target(target: &str) -> (Option<&str>, &str) {
    if let Some(pos) = target.rfind(':') {
        let file = &target[..pos];
        let name = &target[pos + 1..];
        if !file.is_empty() && !name.is_empty() {
            return (Some(file), name);
        }
    }
    (None, target.trim_end_matches(':'))
}

/// Resolve a target string to a ChunkSummary.
///
/// Uses search_by_name with optional file filtering.
/// Returns the best-matching chunk or an error if none found.
pub fn resolve_target(
    store: &Store,
    target: &str,
) -> Result<(ChunkSummary, Vec<SearchResult>), StoreError> {
    let (file_filter, name) = parse_target(target);
    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        return Err(StoreError::Runtime(format!(
            "No function found matching '{}'. Check the name and try again.",
            name
        )));
    }

    let matched = if let Some(file) = file_filter {
        results.iter().position(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        })
    } else {
        None
    };

    let idx = matched.unwrap_or(0);
    let chunk = results[idx].chunk.clone();
    Ok((chunk, results))
}

// ============ Name Matching ============

/// Pre-tokenized query for efficient name matching in loops
///
/// Create once before iterating over search results, then call `score()` for each name.
/// Avoids re-tokenizing the query for every result.
pub(crate) struct NameMatcher {
    query_lower: String,
    query_words: Vec<String>,
}

impl NameMatcher {
    /// Create a new matcher with pre-tokenized query
    pub fn new(query: &str) -> Self {
        Self {
            query_lower: query.to_lowercase(),
            query_words: tokenize_identifier(query)
                .into_iter()
                .map(|w| w.to_lowercase())
                .collect(),
        }
    }

    /// Compute name match score against pre-tokenized query
    pub fn score(&self, name: &str) -> f32 {
        let name_lower = name.to_lowercase();

        // Exact match
        if name_lower == self.query_lower {
            return 1.0;
        }

        // Name contains query as substring
        if name_lower.contains(&self.query_lower) {
            return 0.8;
        }

        // Query contains name as substring
        if self.query_lower.contains(&name_lower) {
            return 0.6;
        }

        // Word overlap scoring
        if self.query_words.is_empty() {
            return 0.0;
        }

        // Trade-off: Building name_words Vec per result adds allocation overhead,
        // but pre-indexing names would require storing tokenized names in the DB
        // (increasing schema complexity and storage ~20%). Given name_words are
        // typically 1-5 words and this only runs for top-N results after filtering,
        // the per-result allocation is acceptable.
        let name_words: Vec<String> = tokenize_identifier(name)
            .into_iter()
            .map(|w| w.to_lowercase())
            .collect();

        if name_words.is_empty() {
            return 0.0;
        }

        // Fast path: build HashSet for O(1) exact match lookup
        let name_word_set: HashSet<&str> = name_words.iter().map(String::as_str).collect();

        // O(m*n) substring matching trade-off:
        // - m = query words (typically 1-5), n = name words (typically 1-5)
        // - Worst case: ~25 comparisons per name, but short-circuits on exact match
        // - Alternative (pre-indexing substring tries) would add complexity for minimal gain
        //   since names are short and search results are already capped by limit
        let overlap = self
            .query_words
            .iter()
            .filter(|w| {
                // Fast path: exact word match
                if name_word_set.contains(w.as_str()) {
                    return true;
                }
                // Slow path: substring matching (only if no exact match)
                // Intentionally excludes equal-length substrings: if lengths are equal
                // but strings differ, they're not substrings of each other (would need
                // exact match, handled above). This avoids redundant contains() calls.
                name_words.iter().any(|nw| {
                    // Short-circuit: check length before expensive substring search
                    (nw.len() > w.len() && nw.contains(w.as_str()))
                        || (w.len() > nw.len() && w.contains(nw.as_str()))
                })
            })
            .count() as f32;
        let total = self.query_words.len().max(1) as f32;

        (overlap / total) * 0.5 // Max 0.5 for partial word overlap
    }
}

/// Compile a glob pattern into a matcher, logging and ignoring invalid patterns.
///
/// Returns `None` if the pattern is `None` or invalid (with a warning logged).
fn compile_glob_filter(pattern: Option<&String>) -> Option<globset::GlobMatcher> {
    pattern.and_then(|p| match globset::Glob::new(p) {
        Ok(g) => Some(g.compile_matcher()),
        Err(e) => {
            tracing::warn!(pattern = %p, error = %e, "Invalid glob pattern, ignoring filter");
            None
        }
    })
}

/// Compute name match score for hybrid search
///
/// For repeated calls with the same query, use `NameMatcher::new(query).score(name)` instead.
#[cfg(test)]
pub(crate) fn name_match_score(query: &str, name: &str) -> f32 {
    NameMatcher::new(query).score(name)
}

/// Bounded min-heap for maintaining top-N search results by score.
///
/// Uses a min-heap internally so the smallest score is always at the top,
/// allowing O(log N) eviction when the heap is full. This bounds memory to
/// O(limit) instead of O(total_chunks) for the scoring phase.
struct BoundedScoreHeap {
    heap: BinaryHeap<Reverse<(OrderedFloat, String)>>,
    capacity: usize,
}

/// Wrapper for f32 that implements Ord for use in BinaryHeap.
/// Uses total_cmp for consistent ordering (NaN sorts to the end).
#[derive(Clone, Copy, PartialEq)]
struct OrderedFloat(f32);

impl Eq for OrderedFloat {}

impl PartialOrd for OrderedFloat {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedFloat {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl BoundedScoreHeap {
    fn new(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity + 1),
            capacity,
        }
    }

    /// Push a scored result. If at capacity, evicts the lowest score.
    fn push(&mut self, id: String, score: f32) {
        if !score.is_finite() {
            tracing::warn!("BoundedScoreHeap: ignoring non-finite score");
            return;
        }

        // If below capacity, always insert
        if self.heap.len() < self.capacity {
            self.heap.push(Reverse((OrderedFloat(score), id)));
            return;
        }

        // At capacity - only insert if better than current minimum
        if let Some(Reverse((OrderedFloat(min_score), _))) = self.heap.peek() {
            if score > *min_score {
                self.heap.pop();
                self.heap.push(Reverse((OrderedFloat(score), id)));
            }
        }
    }

    /// Drain into a sorted Vec (highest score first).
    fn into_sorted_vec(self) -> Vec<(String, f32)> {
        let mut results: Vec<_> = self
            .heap
            .into_iter()
            .map(|Reverse((OrderedFloat(score), id))| (id, score))
            .collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }
}

impl Store {
    /// Search for similar chunks (two-phase for memory efficiency)
    pub fn search(
        &self,
        query: &Embedding,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        self.search_filtered(query, &SearchFilter::default(), limit, threshold)
    }

    /// Search with filters
    pub fn search_filtered(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<SearchResult>, StoreError> {
        let _span = tracing::info_span!("search_filtered", limit = limit, rrf = filter.enable_rrf)
            .entered();

        self.rt.block_on(async {
            // Build WHERE clause from filter
            let mut conditions = Vec::new();
            let mut bind_values: Vec<String> = Vec::new();

            if let Some(ref langs) = filter.languages {
                let placeholders: Vec<_> = (0..langs.len())
                    .map(|i| format!("?{}", bind_values.len() + i + 1))
                    .collect();
                conditions.push(format!("language IN ({})", placeholders.join(",")));
                for lang in langs {
                    bind_values.push(lang.to_string());
                }
            }

            if let Some(ref types) = filter.chunk_types {
                let placeholders: Vec<_> = (0..types.len())
                    .map(|i| format!("?{}", bind_values.len() + i + 1))
                    .collect();
                conditions.push(format!("chunk_type IN ({})", placeholders.join(",")));
                for ct in types {
                    bind_values.push(ct.to_string());
                }
            }

            let where_clause = if conditions.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", conditions.join(" AND "))
            };

            let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();
            let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();
            let semantic_limit = if use_rrf { limit * 3 } else { limit };

            let sql = if use_hybrid {
                format!("SELECT id, embedding, name FROM chunks{}", where_clause)
            } else {
                format!("SELECT id, embedding FROM chunks{}", where_clause)
            };

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for val in &bind_values {
                    q = q.bind(val);
                }
                q.fetch_all(&self.pool).await?
            };

            // Compile glob pattern once outside the loop (not per-chunk).
            // Note: Invalid patterns are logged and silently ignored (returns all results).
            // Callers should validate patterns upfront via SearchFilter::validate() if they
            // want to reject invalid patterns. This lenient behavior is intentional to allow
            // partial searches when users provide malformed patterns interactively.
            let glob_matcher = compile_glob_filter(filter.path_pattern.as_ref());

            // Pre-tokenize query for name matching (avoids re-tokenizing per result)
            let name_matcher = if use_hybrid {
                Some(NameMatcher::new(&filter.query_text))
            } else {
                None
            };

            // Use bounded heap to maintain only top-N results during iteration.
            // This bounds memory to O(semantic_limit) instead of O(total_chunks).
            let mut score_heap = BoundedScoreHeap::new(semantic_limit);

            for row in &rows {
                let id: String = row.get(0);
                let embedding_bytes: Vec<u8> = row.get(1);
                let name: Option<String> = if use_hybrid { row.get(2) } else { None };

                let Some(embedding) = embedding_slice(&embedding_bytes) else {
                    continue;
                };
                let Some(embedding_score) = cosine_similarity(query.as_slice(), embedding) else {
                    continue;
                };

                let score = if let Some(ref matcher) = name_matcher {
                    let n = name.as_deref().unwrap_or("");
                    let name_score = matcher.score(n);
                    (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                } else {
                    embedding_score
                };

                if let Some(ref matcher) = glob_matcher {
                    // Extract file path from chunk ID (format: "path:line_start:hash_prefix").
                    // Strip ":hash_prefix" then ":line_start" with two rfind calls.
                    let file_part = id
                        .rfind(':')
                        .and_then(|i| id[..i].rfind(':'))
                        .map(|i| &id[..i])
                        .unwrap_or(&id);
                    if !matcher.is_match(file_part) {
                        continue;
                    }
                }

                if score >= threshold {
                    score_heap.push(id, score);
                }
            }

            let mut scored = score_heap.into_sorted_vec();

            // Normalize query text once for FTS (not twice - search_fts is a separate code path)
            let normalized_query = if use_rrf {
                Some(normalize_for_fts(&filter.query_text))
            } else {
                None
            };

            let final_scored: Vec<(String, f32)> = if use_rrf {
                let fts_ids = {
                    let normalized_query = normalized_query.as_ref().unwrap();
                    if normalized_query.is_empty() {
                        vec![]
                    } else {
                        let fts_rows: Vec<(String,)> = sqlx::query_as(
                            "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
                        )
                        .bind(normalized_query)
                        .bind(semantic_limit as i64)
                        .fetch_all(&self.pool)
                        .await?;
                        fts_rows.into_iter().map(|(id,)| id).collect()
                    }
                };
                let semantic_ids: Vec<String> = scored.iter().map(|(id, _)| id.clone()).collect();
                Self::rrf_fuse(&semantic_ids, &fts_ids, limit)
            } else {
                scored.truncate(limit);
                scored
            };

            if final_scored.is_empty() {
                return Ok(vec![]);
            }

            // Phase 2: Fetch full content only for top-N results
            let ids: Vec<&str> = final_scored.iter().map(|(id, _)| id.as_str()).collect();
            let rows_map = self.fetch_chunks_by_ids_async(&ids).await?;

            let mut seen_parents: HashSet<String> = HashSet::new();
            let results: Vec<SearchResult> = final_scored
                .into_iter()
                .filter_map(|(id, score)| {
                    rows_map.get(&id).and_then(|row| {
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
                })
                .collect();

            Ok(results)
        })
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

        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        self.rt.block_on(async {
            let rows = self
                .fetch_chunks_with_embeddings_by_ids_async(candidate_ids)
                .await?;

            // Compile glob pattern once outside the loop (not per-chunk).
            let glob_matcher = compile_glob_filter(filter.path_pattern.as_ref());

            // Pre-tokenize query for name matching (avoids re-tokenizing per result)
            let name_matcher = if use_hybrid {
                Some(NameMatcher::new(&filter.query_text))
            } else {
                None
            };

            let mut scored: Vec<(ChunkRow, f32)> = rows
                .into_iter()
                .filter_map(|(chunk_row, embedding_bytes)| {
                    if let Some(ref langs) = filter.languages {
                        let row_lang: Result<crate::parser::Language, _> =
                            chunk_row.language.parse();
                        if let Ok(lang) = row_lang {
                            if !langs.contains(&lang) {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    }

                    if let Some(ref types) = filter.chunk_types {
                        let row_type: Result<crate::parser::ChunkType, _> =
                            chunk_row.chunk_type.parse();
                        if let Ok(ct) = row_type {
                            if !types.contains(&ct) {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    }

                    if let Some(ref matcher) = glob_matcher {
                        if !matcher.is_match(&chunk_row.origin) {
                            return None;
                        }
                    }

                    let embedding = match embedding_slice(&embedding_bytes) {
                        Some(e) => e,
                        None => return None,
                    };
                    let embedding_score = cosine_similarity(query.as_slice(), embedding)?;

                    let score = if let Some(ref matcher) = name_matcher {
                        let name_score = matcher.score(&chunk_row.name);
                        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    if score >= threshold {
                        Some((chunk_row, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            let mut seen_parents: HashSet<String> = HashSet::new();
            let results: Vec<SearchResult> = scored
                .into_iter()
                .filter_map(|(row, score)| {
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
                .take(limit)
                .collect();

            Ok(results)
        })
    }

    /// Unified search with optional vector index
    ///
    /// When an HNSW index is provided, uses O(log n) search for both chunks and notes.
    /// Note IDs in HNSW are prefixed with `note:` to distinguish from chunk IDs.
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

        // note_only: return only notes, skip code search entirely
        if filter.note_only {
            let note_results = self.search_notes(query, limit, threshold)?;
            return Ok(note_results.into_iter().map(UnifiedResult::Note).collect());
        }

        // Skip note search entirely when note_weight is effectively zero
        let skip_notes = filter.note_weight <= 0.0;

        // Notes always use brute-force search from SQLite (capped at 1000).
        // This ensures notes added via MCP are immediately searchable without
        // waiting for an HNSW rebuild. HNSW is only used for chunks (10k-100k+).
        let note_results = if skip_notes {
            vec![]
        } else {
            self.search_notes(query, limit, threshold)?
        };

        let code_results = if let Some(idx) = index {
            // Query HNSW for chunk candidates only
            let candidate_count = (limit * 5).max(100);
            let index_results = idx.search(query, candidate_count);

            if index_results.is_empty() {
                tracing::info!("Index returned no candidates, falling back to brute-force search (performance may degrade)");
                self.search_filtered(query, filter, limit, threshold)?
            } else {
                // Filter to chunk IDs only (skip any legacy note: prefixed entries)
                let chunk_ids: Vec<&str> = index_results
                    .iter()
                    .filter_map(|r| {
                        if r.id.starts_with("note:") {
                            None
                        } else {
                            Some(r.id.as_str())
                        }
                    })
                    .collect();

                tracing::debug!("Index returned {} chunk candidates", chunk_ids.len());

                self.search_by_candidate_ids(&chunk_ids, query, filter, limit, threshold)?
            }
        } else {
            self.search_filtered(query, filter, limit, threshold)?
        };

        // Slot allocation: reserve minimum 60% for code results, up to 40% for notes.
        // This prevents notes from dominating while still surfacing relevant observations.
        let min_code_slots = ((limit * 3) / 5).max(1);
        let code_count = code_results.len().min(limit);
        let reserved_code = code_count.min(min_code_slots);
        let note_slots = limit.saturating_sub(reserved_code);

        let mut unified: Vec<crate::store::UnifiedResult> = code_results
            .into_iter()
            .take(limit)
            .map(crate::store::UnifiedResult::Code)
            .collect();

        // Apply note_weight to attenuate note scores before merging
        let notes_to_add: Vec<crate::store::UnifiedResult> = note_results
            .into_iter()
            .take(note_slots)
            .map(|mut r| {
                r.score *= filter.note_weight;
                crate::store::UnifiedResult::Note(r)
            })
            .collect();
        unified.extend(notes_to_add);

        unified.sort_by(|a, b| {
            b.score()
                .partial_cmp(&a.score())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        unified.truncate(limit);

        Ok(unified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // cosine_similarity tests are in src/math.rs

    // ===== name_match_score tests =====

    #[test]
    fn test_name_match_exact() {
        assert_eq!(name_match_score("parse", "parse"), 1.0);
    }

    #[test]
    fn test_name_match_contains() {
        assert_eq!(name_match_score("parse", "parseConfig"), 0.8);
    }

    #[test]
    fn test_name_match_contained() {
        assert_eq!(name_match_score("parseConfigFile", "parse"), 0.6);
    }

    #[test]
    fn test_name_match_partial_overlap() {
        let score = name_match_score("parseConfig", "configParser");
        assert!(score > 0.0 && score <= 0.5);
    }

    #[test]
    fn test_name_match_no_match() {
        assert_eq!(name_match_score("foo", "bar"), 0.0);
    }

    // ===== min_code_slots tests =====

    #[test]
    fn test_min_code_slots_limit_1() {
        // With limit=1, (1*3)/5 = 0 which starved code results.
        // After fix: .max(1) ensures at least 1 code slot.
        let limit = 1;
        let min_code_slots = ((limit * 3) / 5).max(1);
        assert_eq!(min_code_slots, 1);
    }

    #[test]
    fn test_min_code_slots_limit_5() {
        let limit = 5;
        let min_code_slots = ((limit * 3) / 5).max(1);
        assert_eq!(min_code_slots, 3);
    }

    // ===== compile_glob_filter tests =====

    #[test]
    fn test_compile_glob_filter_none() {
        assert!(compile_glob_filter(None).is_none());
    }

    #[test]
    fn test_compile_glob_filter_valid() {
        let pattern = "src/**/*.rs".to_string();
        let matcher = compile_glob_filter(Some(&pattern));
        assert!(matcher.is_some());
        let m = matcher.unwrap();
        assert!(m.is_match("src/cli/mod.rs"));
        assert!(!m.is_match("tests/foo.py"));
    }

    #[test]
    fn test_compile_glob_filter_invalid() {
        let pattern = "[invalid".to_string();
        assert!(compile_glob_filter(Some(&pattern)).is_none());
    }
}
