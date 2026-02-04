//! Search algorithms and scoring functions
//!
//! This module contains the SearchEngine for code and note search,
//! as well as helper functions for similarity scoring.

use std::collections::{HashMap, HashSet};

use sqlx::Row;

use crate::embedder::Embedding;
use crate::index::VectorIndex;
use crate::nl::normalize_for_fts;
use crate::nl::tokenize_identifier;
use crate::store::helpers::{
    clamp_line_number, embedding_slice, ChunkRow, ChunkSummary, SearchFilter, SearchResult,
};
use crate::store::{Store, StoreError};

/// Cosine similarity for L2-normalized vectors (just dot product)
/// Uses SIMD acceleration when available (2-4x faster on AVX2/NEON)
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "Embedding dimension mismatch");
    debug_assert_eq!(a.len(), 769, "Expected 769-dim embeddings");
    use simsimd::SpatialSimilarity;
    f32::dot(a, b).unwrap_or_else(|| {
        // Fallback for unsupported architectures
        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>() as f64
    }) as f32
}

/// Compute name match score for hybrid search
pub fn name_match_score(query: &str, name: &str) -> f32 {
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();

    // Exact match
    if name_lower == query_lower {
        return 1.0;
    }

    // Name contains query as substring
    if name_lower.contains(&query_lower) {
        return 0.8;
    }

    // Query contains name as substring
    if query_lower.contains(&name_lower) {
        return 0.6;
    }

    // Word overlap (split on camelCase, snake_case)
    // Tokenize original (with casing), then lowercase each token
    let query_words: Vec<String> = tokenize_identifier(query)
        .into_iter()
        .map(|w| w.to_lowercase())
        .collect();
    let name_words: Vec<String> = tokenize_identifier(name)
        .into_iter()
        .map(|w| w.to_lowercase())
        .collect();

    if query_words.is_empty() || name_words.is_empty() {
        return 0.0;
    }

    let overlap = query_words
        .iter()
        .filter(|w| {
            name_words.iter().any(|nw| {
                // Short-circuit: check length before expensive substring search
                (nw.len() >= w.len() && nw.contains(w.as_str()))
                    || (w.len() >= nw.len() && w.contains(nw.as_str()))
            })
        })
        .count() as f32;
    let total = query_words.len().max(1) as f32;

    (overlap / total) * 0.5 // Max 0.5 for partial word overlap
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

            // Compile glob pattern once outside the loop (not per-chunk)
            let glob_matcher = filter
                .path_pattern
                .as_ref()
                .and_then(|p| globset::Glob::new(p).ok())
                .map(|g| g.compile_matcher());

            let mut scored: Vec<(String, f32)> = rows
                .iter()
                .filter_map(|row| {
                    let id: String = row.get(0);
                    let embedding_bytes: Vec<u8> = row.get(1);
                    let name: Option<String> = if use_hybrid { row.get(2) } else { None };

                    let embedding = embedding_slice(&embedding_bytes)?;
                    let embedding_score = cosine_similarity(query.as_slice(), embedding);

                    let score = if use_hybrid {
                        let n = name.as_deref().unwrap_or("");
                        let name_score = name_match_score(&filter.query_text, n);
                        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    if let Some(ref matcher) = glob_matcher {
                        let file_part = id.split(':').next().unwrap_or("");
                        if !matcher.is_match(file_part) {
                            return None;
                        }
                    }

                    if score >= threshold {
                        Some((id, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            scored.truncate(semantic_limit);

            let final_scored: Vec<(String, f32)> = if use_rrf {
                let fts_ids = {
                    let normalized_query = normalize_for_fts(&filter.query_text);
                    if normalized_query.is_empty() {
                        vec![]
                    } else {
                        let fts_rows: Vec<(String,)> = sqlx::query_as(
                            "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
                        )
                        .bind(&normalized_query)
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
                tracing::debug!("Index returned no candidates, falling back to brute-force");
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

            // Compile glob pattern once outside the loop (not per-chunk)
            let glob_matcher = filter
                .path_pattern
                .as_ref()
                .and_then(|p| globset::Glob::new(p).ok())
                .map(|g| g.compile_matcher());

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
                    let embedding_score = cosine_similarity(query.as_slice(), embedding);

                    let score = if use_hybrid {
                        let name_score = name_match_score(&filter.query_text, &chunk_row.name);
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

    /// Unified search across code chunks and notes
    pub fn search_unified(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
    ) -> Result<Vec<crate::store::UnifiedResult>, StoreError> {
        let code_results = self.search_filtered(query, filter, limit, threshold)?;
        let note_results = self.search_notes(query, limit, threshold)?;

        let min_code_slots = (limit * 3) / 5;
        let code_count = code_results.len().min(limit);
        let reserved_code = code_count.min(min_code_slots);
        let note_slots = limit.saturating_sub(reserved_code);

        let mut unified: Vec<crate::store::UnifiedResult> = code_results
            .into_iter()
            .take(limit)
            .map(crate::store::UnifiedResult::Code)
            .collect();

        let notes_to_add: Vec<crate::store::UnifiedResult> = note_results
            .into_iter()
            .take(note_slots)
            .map(crate::store::UnifiedResult::Note)
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

    /// Unified search with optional vector index
    pub fn search_unified_with_index(
        &self,
        query: &Embedding,
        filter: &SearchFilter,
        limit: usize,
        threshold: f32,
        index: Option<&dyn VectorIndex>,
    ) -> Result<Vec<crate::store::UnifiedResult>, StoreError> {
        let code_results =
            self.search_filtered_with_index(query, filter, limit, threshold, index)?;
        let note_results = self.search_notes(query, limit, threshold)?;

        let min_code_slots = (limit * 3) / 5;
        let code_count = code_results.len().min(limit);
        let reserved_code = code_count.min(min_code_slots);
        let note_slots = limit.saturating_sub(reserved_code);

        let mut unified: Vec<crate::store::UnifiedResult> = code_results
            .into_iter()
            .take(limit)
            .map(crate::store::UnifiedResult::Code)
            .collect();

        let notes_to_add: Vec<crate::store::UnifiedResult> = note_results
            .into_iter()
            .take(note_slots)
            .map(crate::store::UnifiedResult::Note)
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

    // ===== cosine_similarity tests =====

    fn make_embedding(val: f32) -> Vec<f32> {
        vec![val; 769]
    }

    fn make_unit_embedding(idx: usize) -> Vec<f32> {
        let mut v = vec![0.0; 769];
        v[idx] = 1.0;
        v
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = make_embedding(0.5);
        let sim = cosine_similarity(&a, &a);
        // Identical vectors should have high similarity
        assert!(sim > 0.99, "Expected ~1.0, got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = make_unit_embedding(0);
        let b = make_unit_embedding(1);
        let sim = cosine_similarity(&a, &b);
        // Orthogonal unit vectors should have 0 similarity
        assert!(sim.abs() < 0.01, "Expected ~0, got {}", sim);
    }

    #[test]
    fn test_cosine_similarity_symmetric() {
        let a: Vec<f32> = (0..769).map(|i| (i as f32) / 769.0).collect();
        let b: Vec<f32> = (0..769).map(|i| 1.0 - (i as f32) / 769.0).collect();
        let sim_ab = cosine_similarity(&a, &b);
        let sim_ba = cosine_similarity(&b, &a);
        assert!((sim_ab - sim_ba).abs() < 1e-6, "Should be symmetric");
    }

    #[test]
    fn test_cosine_similarity_range() {
        // Random-ish vectors
        let a: Vec<f32> = (0..769).map(|i| ((i * 7) % 100) as f32 / 100.0).collect();
        let b: Vec<f32> = (0..769).map(|i| ((i * 13) % 100) as f32 / 100.0).collect();
        let sim = cosine_similarity(&a, &b);
        // Cosine similarity for non-normalized vectors can exceed [-1, 1]
        // but for typical embeddings should be reasonable
        assert!(sim.is_finite(), "Should be finite");
    }

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
