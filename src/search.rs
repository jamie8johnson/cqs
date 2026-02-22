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
use crate::note::path_matches_mention;
use crate::store::helpers::{
    embedding_slice, ChunkRow, ChunkSummary, NoteSummary, SearchFilter, SearchResult,
};
use crate::store::sanitize_fts_query;
use crate::store::{Store, StoreError, UnifiedResult};

/// Result of resolving a target name to a concrete chunk.
///
/// Contains the best-matching chunk and any alternative matches
/// found during resolution (useful for disambiguation UIs).
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    /// The resolved chunk (best match for the target name)
    pub chunk: ChunkSummary,
    /// Other candidates found during resolution, ordered by match quality
    pub alternatives: Vec<SearchResult>,
}

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

/// Resolve a target string to a [`ResolvedTarget`].
///
/// Uses search_by_name with optional file filtering.
/// Returns the best-matching chunk and alternatives, or an error if none found.
pub fn resolve_target(store: &Store, target: &str) -> Result<ResolvedTarget, StoreError> {
    let (file_filter, name) = parse_target(target);
    let results = store.search_by_name(name, 20)?;
    if results.is_empty() {
        return Err(StoreError::Runtime(format!(
            "No function found matching '{}'. Check the name and try again.",
            name
        )));
    }

    let idx = if let Some(file) = file_filter {
        let matched = results.iter().position(|r| {
            let path = r.chunk.file.to_string_lossy();
            path.ends_with(file) || path.contains(file)
        });
        match matched {
            Some(i) => i,
            None => {
                let found_in: Vec<_> = results
                    .iter()
                    .take(3)
                    .map(|r| r.chunk.file.to_string_lossy().to_string())
                    .collect();
                return Err(StoreError::Runtime(format!(
                    "No function '{}' found in file matching '{}'. Found in: {}",
                    name,
                    file,
                    found_in.join(", ")
                )));
            }
        }
    } else {
        0
    };
    let chunk = results[idx].chunk.clone();
    Ok(ResolvedTarget {
        chunk,
        alternatives: results,
    })
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

/// Extract file path from a chunk ID.
///
/// Standard format: `"path:line_start:hash_prefix"` (3 segments from right)
/// Windowed format: `"path:line_start:hash_prefix:wN"` (4 segments)
///
/// The hash_prefix is always 8 hex chars. Windowed chunk IDs append `:wN` where
/// N is a small integer (0-99). We detect windowed IDs by checking if the last
/// segment starts with 'w' followed by digits.
fn extract_file_from_chunk_id(id: &str) -> &str {
    // Strip last segment
    let Some(last_colon) = id.rfind(':') else {
        return id;
    };
    let last_seg = &id[last_colon + 1..];

    // Determine how many segments to strip from the right:
    // - Standard: 2 (hash_prefix, line_start)
    // - Windowed: 3 (wN, hash_prefix, line_start)
    // Window suffix format: "w0", "w1", ..., "w99"
    let segments_to_strip = if !last_seg.is_empty()
        && last_seg.starts_with('w')
        && last_seg.len() <= 3
        && last_seg[1..].bytes().all(|b| b.is_ascii_digit())
    {
        3
    } else {
        2
    };

    let mut end = id.len();
    for _ in 0..segments_to_strip {
        if let Some(i) = id[..end].rfind(':') {
            end = i;
        } else {
            break;
        }
    }
    &id[..end]
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

/// Multiplicative boost factor for note-matched code chunks.
///
/// A note with sentiment +1 boosts the chunk's score by 15%.
/// A note with sentiment -1 reduces it by 15%.
const NOTE_BOOST_FACTOR: f32 = 0.15;

/// Compute the note-based score boost for a chunk.
///
/// Checks if any note's mentions match the chunk's file path or name.
/// When multiple notes match, takes the strongest absolute sentiment
/// (preserving sign) to avoid averaging away strong signals.
///
/// Returns a multiplier: `1.0 + sentiment * NOTE_BOOST_FACTOR`
fn note_boost(file_path: &str, chunk_name: &str, notes: &[NoteSummary]) -> f32 {
    let mut strongest: Option<f32> = None;
    for note in notes {
        for mention in &note.mentions {
            if path_matches_mention(file_path, mention) || chunk_name == mention {
                match strongest {
                    Some(prev) if note.sentiment.abs() > prev.abs() => {
                        strongest = Some(note.sentiment);
                    }
                    None => {
                        strongest = Some(note.sentiment);
                    }
                    _ => {}
                }
                break; // This note already matched, check next note
            }
        }
    }
    match strongest {
        Some(s) => 1.0 + s * NOTE_BOOST_FACTOR,
        None => 1.0,
    }
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

        // At capacity - only insert if strictly better than current minimum.
        // Using > (not >=) gives first-indexed stability: when scores are equal,
        // earlier items are kept. This prevents last-wins bias where later-indexed
        // chunks systematically replace earlier ones at equal scores.
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
        results.sort_by(|a, b| b.1.total_cmp(&a.1));
        results
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

        // Load notes once for note-boosted ranking (cheap — no embeddings)
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                &[]
            }
        };

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

            let use_hybrid = filter.name_boost > 0.0 && !filter.query_text.is_empty();
            let use_rrf = filter.enable_rrf && !filter.query_text.is_empty();
            let semantic_limit = if use_rrf { limit * 3 } else { limit };

            // Select columns: always id + embedding, optionally name for hybrid scoring
            let columns = if use_hybrid {
                "rowid, id, embedding, name"
            } else {
                "rowid, id, embedding"
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

            // Cursor-based batching: load embeddings in batches of 5000 instead of
            // all at once. This bounds memory to O(batch_size) instead of O(total_chunks).
            // Uses the same cursor pattern as EmbeddingBatchIterator in store/chunks.rs.
            const BRUTE_FORCE_BATCH_SIZE: i64 = 5000;
            let mut last_rowid: i64 = 0;
            loop {
                let rowid_condition = format!("rowid > ?{}", bind_values.len() + 1);
                let limit_param = format!("?{}", bind_values.len() + 2);

                let batch_where = if conditions.is_empty() {
                    format!(" WHERE {} ORDER BY rowid ASC LIMIT {}", rowid_condition, limit_param)
                } else {
                    format!(
                        " WHERE {} AND {} ORDER BY rowid ASC LIMIT {}",
                        conditions.join(" AND "),
                        rowid_condition,
                        limit_param
                    )
                };

                let sql = format!("SELECT {} FROM chunks{}", columns, batch_where);
                let batch: Vec<_> = {
                    let mut q = sqlx::query(&sql);
                    for val in &bind_values {
                        q = q.bind(val);
                    }
                    q = q.bind(last_rowid);
                    q = q.bind(BRUTE_FORCE_BATCH_SIZE);
                    q.fetch_all(&self.pool).await?
                };

                if batch.is_empty() {
                    break;
                }
                last_rowid = batch.last().unwrap().get::<i64, _>("rowid");

                for row in &batch {
                    let id: String = row.get("id");
                    let embedding_bytes: Vec<u8> = row.get("embedding");
                    let name: Option<String> = if use_hybrid { row.get("name") } else { None };

                    let Some(embedding) = embedding_slice(&embedding_bytes) else {
                        continue;
                    };
                    let Some(embedding_score) = cosine_similarity(query.as_slice(), embedding)
                    else {
                        continue;
                    };

                    let base_score = if let Some(ref matcher) = name_matcher {
                        let n = name.as_deref().unwrap_or("");
                        let name_score = matcher.score(n);
                        (1.0 - filter.name_boost) * embedding_score
                            + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    let file_part = extract_file_from_chunk_id(&id);

                    if let Some(ref matcher) = glob_matcher {
                        if !matcher.is_match(file_part) {
                            continue;
                        }
                    }

                    // Apply note-based boost: notes mentioning this chunk's file or name
                    // adjust its score by up to ±15%
                    let chunk_name = name.as_deref().unwrap_or("");
                    let score = base_score * note_boost(file_part, chunk_name, notes);

                    if score >= threshold {
                        score_heap.push(id, score);
                    }
                }
            }

            let mut scored = score_heap.into_sorted_vec();

            // Normalize + sanitize query text for FTS5 MATCH (defense-in-depth)
            let normalized_query = if use_rrf {
                let normalized = normalize_for_fts(&filter.query_text);
                Some(sanitize_fts_query(&normalized))
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

        // Load notes once for note-boosted ranking
        let notes = match self.cached_notes_summaries() {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to load notes for search boosting");
                &[]
            }
        };

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

                    let base_score = if let Some(ref matcher) = name_matcher {
                        let name_score = matcher.score(&chunk_row.name);
                        (1.0 - filter.name_boost) * embedding_score + filter.name_boost * name_score
                    } else {
                        embedding_score
                    };

                    // Apply note-based boost
                    let score = base_score * note_boost(&chunk_row.origin, &chunk_row.name, notes);

                    if score >= threshold {
                        Some((chunk_row, score))
                    } else {
                        None
                    }
                })
                .collect();

            scored.sort_by(|a, b| b.1.total_cmp(&a.1));

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
        // This ensures notes are immediately searchable without
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
        // When code results are sparse, cap notes to the proportional amount (40%)
        // rather than letting them fill all remaining slots.
        let min_code_slots = ((limit * 3) / 5).max(1);
        let code_count = code_results.len().min(limit);
        let note_slots = if code_count >= min_code_slots {
            limit.saturating_sub(code_count)
        } else {
            // Code is sparse — still cap notes to proportional amount
            limit.saturating_sub(min_code_slots)
        };

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

    // ===== note_boost tests =====

    fn make_note(sentiment: f32, mentions: &[&str]) -> NoteSummary {
        NoteSummary {
            id: "note:test".to_string(),
            text: "test note".to_string(),
            sentiment,
            mentions: mentions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_note_boost_no_notes() {
        let boost = note_boost("src/lib.rs", "my_fn", &[]);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_no_match() {
        let notes = vec![make_note(-0.5, &["other.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert_eq!(boost, 1.0);
    }

    #[test]
    fn test_note_boost_file_match_negative() {
        let notes = vec![make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_file_match_positive() {
        let notes = vec![make_note(1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_name_match() {
        let notes = vec![make_note(0.5, &["my_fn"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.075).abs() < 0.001,
            "Expected ~1.075, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_wins() {
        // Two notes: weak positive and strong negative. Strong negative should win.
        let notes = vec![make_note(0.5, &["lib.rs"]), make_note(-1.0, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 0.85).abs() < 0.001,
            "Expected ~0.85, got {}",
            boost
        );
    }

    #[test]
    fn test_note_boost_strongest_absolute_preserves_sign() {
        // Two notes: strong positive and weak negative. Strong positive should win.
        let notes = vec![make_note(1.0, &["lib.rs"]), make_note(-0.5, &["lib.rs"])];
        let boost = note_boost("src/lib.rs", "my_fn", &notes);
        assert!(
            (boost - 1.15).abs() < 0.001,
            "Expected ~1.15, got {}",
            boost
        );
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

    // ===== extract_file_from_chunk_id tests =====

    #[test]
    fn test_extract_file_standard_chunk_id() {
        // Standard: "path:line_start:hash_prefix"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id() {
        // Windowed: "path:line_start:hash_prefix:wN"
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w3"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_nested_path() {
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef"),
            "src/cli/commands/mod.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/cli/commands/mod.rs:42:deadbeef:w1"),
            "src/cli/commands/mod.rs"
        );
    }

    #[test]
    fn test_extract_file_windowed_chunk_id_w_prefix() {
        // Windowed IDs use "wN" format (not bare digits)
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w0"),
            "src/foo.rs"
        );
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:abc12345:w12"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_hash_not_confused_with_window() {
        // 8-char hex hash should NOT be mistaken for a window index
        assert_eq!(
            extract_file_from_chunk_id("src/foo.rs:10:deadbeef"),
            "src/foo.rs"
        );
    }

    #[test]
    fn test_extract_file_no_colons() {
        assert_eq!(extract_file_from_chunk_id("justanid"), "justanid");
    }

    // ===== BoundedScoreHeap tests =====

    #[test]
    fn test_bounded_heap_equal_scores() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("a".to_string(), 0.5);
        heap.push("b".to_string(), 0.5);
        heap.push("c".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        // First-indexed stability: equal scores don't replace existing entries,
        // so "a" and "b" are kept, "c" is rejected.
        assert!(results.iter().any(|(id, _)| id == "a"));
        assert!(results.iter().any(|(id, _)| id == "b"));
    }

    // ===== BoundedScoreHeap additional tests =====

    #[test]
    fn test_bounded_heap_evicts_lowest() {
        let mut heap = BoundedScoreHeap::new(2);
        heap.push("low".to_string(), 0.1);
        heap.push("mid".to_string(), 0.5);
        heap.push("high".to_string(), 0.9);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "high");
        assert_eq!(results[1].0, "mid");
    }

    #[test]
    fn test_bounded_heap_ignores_non_finite() {
        let mut heap = BoundedScoreHeap::new(5);
        heap.push("nan".to_string(), f32::NAN);
        heap.push("inf".to_string(), f32::INFINITY);
        heap.push("neginf".to_string(), f32::NEG_INFINITY);
        heap.push("ok".to_string(), 0.5);
        let results = heap.into_sorted_vec();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "ok");
    }

    #[test]
    fn test_bounded_heap_empty() {
        let heap = BoundedScoreHeap::new(5);
        let results = heap.into_sorted_vec();
        assert!(results.is_empty());
    }

    // ===== search_filtered integration tests (TC4) =====

    mod search_filtered_tests {
        use crate::embedder::Embedding;
        use crate::parser::{ChunkType, Language};
        use crate::store::helpers::{ModelInfo, SearchFilter};
        use crate::store::Store;
        use std::path::PathBuf;

        fn setup_store() -> (Store, tempfile::TempDir) {
            let dir = tempfile::TempDir::new().unwrap();
            let db_path = dir.path().join("index.db");
            let store = Store::open(&db_path).unwrap();
            store.init(&ModelInfo::default()).unwrap();
            (store, dir)
        }

        fn mock_embedding(seed: f32) -> Embedding {
            let mut v = vec![seed; 768];
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            v.push(0.0);
            Embedding::new(v)
        }

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

            let rust_chunk =
                make_chunk("rust_fn", "src/lib.rs", Language::Rust, ChunkType::Function);
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
            let struct_chunk =
                make_chunk("MyStruct", "src/b.rs", Language::Rust, ChunkType::Struct);
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
            let rust_test =
                make_chunk("rs_test", "tests/t.rs", Language::Rust, ChunkType::Function);
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
}
