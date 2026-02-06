//! Search algorithms and scoring functions
//!
//! This module contains the SearchEngine for code and note search,
//! as well as helper functions for similarity scoring.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use sqlx::Row;

use crate::embedder::Embedding;
use crate::index::VectorIndex;
use crate::math::cosine_similarity;
use crate::nl::normalize_for_fts;
use crate::nl::tokenize_identifier;
use crate::store::helpers::{
    clamp_line_number, embedding_slice, ChunkRow, ChunkSummary, SearchFilter, SearchResult,
};
use crate::store::{Store, StoreError};

/// Pre-tokenized query for efficient name matching in loops
///
/// Create once before iterating over search results, then call `score()` for each name.
/// Avoids re-tokenizing the query for every result.
pub struct NameMatcher {
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

/// Compute name match score for hybrid search
///
/// For repeated calls with the same query, use `NameMatcher::new(query).score(name)` instead.
pub fn name_match_score(query: &str, name: &str) -> f32 {
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
            let glob_matcher = filter.path_pattern.as_ref().and_then(|p| {
                match globset::Glob::new(p) {
                    Ok(g) => Some(g.compile_matcher()),
                    Err(e) => {
                        tracing::warn!(pattern = %p, error = %e, "Invalid glob pattern, ignoring filter");
                        None
                    }
                }
            });

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
                    // Extract file path from chunk ID (format: "path:start-end").
                    // Use rfind(':') to handle paths with colons (e.g., Windows "C:\...")
                    let file_part = id.rfind(':').map(|i| &id[..i]).unwrap_or(&id);
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
            let placeholders: String = (1..=ids.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, parent_id
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let detail_rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in &ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            let rows_map: HashMap<String, ChunkRow> = detail_rows
                .into_iter()
                .map(|row| {
                    let chunk_row = ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: clamp_line_number(row.get::<i64, _>(8)),
                        line_end: clamp_line_number(row.get::<i64, _>(9)),
                        parent_id: row.get(10),
                    };
                    (chunk_row.id.clone(), chunk_row)
                })
                .collect();

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
        if candidate_ids.is_empty() {
            return Ok(vec![]);
        }

        let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();

        self.rt.block_on(async {
            let placeholders: String = (1..=candidate_ids.len())
                .map(|i| format!("?{}", i))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT id, origin, language, chunk_type, name, signature, content, doc, line_start, line_end, embedding, parent_id
                 FROM chunks WHERE id IN ({})",
                placeholders
            );

            let rows: Vec<_> = {
                let mut q = sqlx::query(&sql);
                for id in candidate_ids {
                    q = q.bind(*id);
                }
                q.fetch_all(&self.pool).await?
            };

            // Compile glob pattern once outside the loop (not per-chunk).
            // Note: Invalid patterns are logged and silently ignored (returns all results).
            // Callers should validate patterns upfront via SearchFilter::validate() if they
            // want to reject invalid patterns. This lenient behavior is intentional to allow
            // partial searches when users provide malformed patterns interactively.
            let glob_matcher = filter.path_pattern.as_ref().and_then(|p| {
                match globset::Glob::new(p) {
                    Ok(g) => Some(g.compile_matcher()),
                    Err(e) => {
                        tracing::warn!(pattern = %p, error = %e, "Invalid glob pattern, ignoring filter");
                        None
                    }
                }
            });

            // Pre-tokenize query for name matching (avoids re-tokenizing per result)
            let name_matcher = if use_hybrid {
                Some(NameMatcher::new(&filter.query_text))
            } else {
                None
            };

            let mut scored: Vec<(ChunkRow, f32)> = rows
                .into_iter()
                .filter_map(|row| {
                    let chunk_row = ChunkRow {
                        id: row.get(0),
                        origin: row.get(1),
                        language: row.get(2),
                        chunk_type: row.get(3),
                        name: row.get(4),
                        signature: row.get(5),
                        content: row.get(6),
                        doc: row.get(7),
                        line_start: clamp_line_number(row.get::<i64, _>(8)),
                        line_end: clamp_line_number(row.get::<i64, _>(9)),
                        parent_id: row.get(11),
                    };
                    let embedding_bytes: Vec<u8> = row.get(10);

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
        let (code_results, note_results) = if let Some(idx) = index {
            // Query HNSW for candidates (both chunks and notes)
            let candidate_count = (limit * 5).max(100);
            let index_results = idx.search(query, candidate_count);

            if index_results.is_empty() {
                tracing::info!("Index returned no candidates, falling back to brute-force search (performance may degrade)");
                (
                    self.search_filtered(query, filter, limit, threshold)?,
                    self.search_notes(query, limit, threshold)?,
                )
            } else {
                // Partition candidates by note: prefix
                let mut chunk_ids: Vec<&str> = Vec::new();
                let mut note_ids: Vec<&str> = Vec::new();

                for result in &index_results {
                    if let Some(id) = result.id.strip_prefix("note:") {
                        note_ids.push(id);
                    } else {
                        chunk_ids.push(&result.id);
                    }
                }

                tracing::debug!(
                    "Index returned {} chunk candidates, {} note candidates",
                    chunk_ids.len(),
                    note_ids.len()
                );

                let code =
                    self.search_by_candidate_ids(&chunk_ids, query, filter, limit, threshold)?;
                let notes = self.search_notes_by_ids(&note_ids, query, limit, threshold)?;

                (code, notes)
            }
        } else {
            (
                self.search_filtered(query, filter, limit, threshold)?,
                self.search_notes(query, limit, threshold)?,
            )
        };

        let min_code_slots = (limit * 3) / 5;
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
}
