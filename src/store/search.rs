//! Search methods for the Store (FTS, name search, RRF fusion).

use std::collections::HashMap;

use super::helpers::{self, ChunkRow, SearchResult};
use super::{sanitize_fts_query, ChunkSummary, Store, StoreError};
use crate::nl::normalize_for_fts;
use crate::search::scoring::knob;

/// Push a `[scoring]` section's knob overrides into the shared resolver.
/// Must be called before the first search; subsequent calls are no-ops
/// (delegates to [`knob::set_overrides_from_config`], which is OnceLock).
pub fn set_rrf_k_from_config(overrides: &crate::config::ScoringOverrides) {
    knob::set_overrides_from_config(&overrides.knobs);
}

/// PF-2: RRF constant K. Resolved via the shared knob table — see
/// `src/search/scoring/knob.rs` for the resolution order
/// (config → `CQS_RRF_K` env → default 60.0).
fn rrf_k() -> f32 {
    knob::resolve_knob("rrf_k")
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

        self.rt.block_on(async {
            let rows: Vec<(String,)> = sqlx::query_as(
                "SELECT id FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY bm25(chunks_fts) LIMIT ?2",
            )
            .bind(&normalized_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.into_iter().map(|(id,)| id).collect())
        })
    }

    /// Search for chunks by name (definition search).
    ///
    /// Searches the FTS5 name column for exact or prefix matches.
    /// Use this for "where is X defined?" queries instead of semantic search.
    ///
    /// # Limit cap (P3 #101)
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

        // Pre-lowercase query once for score_name_match_pre_lower (PF-3)
        let lower_name = name.to_lowercase();

        // Search name column specifically using FTS5 column filter
        // Use * for prefix matching (e.g., "parse" matches "parse_config")
        // SEC-10: Runtime guard — sanitize_fts_query strips `"` but defense-in-depth
        // prevents FTS5 injection if sanitization logic ever changes.
        if normalized.contains('"') {
            tracing::warn!(
                name = %name,
                "FTS injection guard: double quote in sanitized name, returning empty"
            );
            return Ok(vec![]);
        }
        let fts_query = format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized);

        self.rt.block_on(async {
            let rows: Vec<_> = sqlx::query(
                "SELECT c.id, c.origin, c.language, c.chunk_type, c.name, c.signature, c.content, c.doc, c.line_start, c.line_end, c.content_hash, c.parent_id, c.parent_type_name
                 FROM chunks c
                 JOIN chunks_fts f ON c.id = f.id
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts, 10.0, 1.0, 1.0, 1.0) -- Heavy weight on name column
                 LIMIT ?2",
            )
            .bind(&fts_query)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;

            let mut results = rows
                .into_iter()
                .map(|row| {
                    let chunk = ChunkSummary::from(ChunkRow::from_row(&row));
                    let name_lower = chunk.name.to_lowercase();
                    let score = helpers::score_name_match_pre_lower(&name_lower, &lower_name);
                    SearchResult { chunk, score }
                })
                .collect::<Vec<_>>();

            // Re-sort by name-match score (FTS bm25 ordering may differ).
            // P3 #122: chunk id sorts line numbers lexicographically
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

    /// Compute RRF (Reciprocal Rank Fusion) scores for combining N ranked lists.
    ///
    /// Generalizes the historical 2-list `rrf_fuse(semantic, fts, ...)` so any
    /// number of ranked sources can contribute on a single, uniform pipeline
    /// (semantic embedding + FTS keyword + SPLADE sparse + name-fingerprint +
    /// future signals all become slice elements). Each list is deduped
    /// independently — duplicates within a list collapse to first-occurrence
    /// rank, but a chunk that appears in multiple lists still accumulates one
    /// RRF contribution per list, which is the desired "rewards overlap"
    /// property that #1130 phase 1 preserves bit-for-bit.
    ///
    /// Pre-allocates the HashMap with the total candidate budget across all
    /// inputs (PERF-28). PF-V1.25-2: uses `BoundedScoreHeap` for the final
    /// top-`limit` extraction instead of full-sorting every candidate, with
    /// the id tie-breaker for deterministic ordering.
    pub(crate) fn rrf_fuse_n(ranked_lists: &[&[&str]], limit: usize) -> Vec<(String, f32)> {
        // K=60 is the standard RRF constant from the Cormack et al. (2009)
        // paper, originally tuned for web search. For code search with smaller
        // corpora (10k-100k chunks), the optimal K may differ. Empirically,
        // K=60 performs well on our eval set (90.9% R@1). Override via
        // CQS_RRF_K env var.
        let k = rrf_k();

        let total_capacity: usize = ranked_lists.iter().map(|l| l.len()).sum();
        let mut scores: HashMap<&str, f32> = HashMap::with_capacity(total_capacity);

        for list in ranked_lists {
            // AC-9: per-list dedup — duplicates within a list collapse to
            // first-occurrence rank (best score). Cross-list overlap is
            // intentional and gets the "rewards overlap" boost.
            let mut seen = std::collections::HashSet::with_capacity(list.len());
            for (rank, id) in list.iter().enumerate() {
                if !seen.insert(*id) {
                    continue;
                }
                // RRF formula: 1 / (K + rank). The + 1.0 converts the
                // 0-indexed `enumerate()` to 1-indexed ranks (first result
                // = rank 1, not rank 0).
                let contribution = 1.0 / (k + rank as f32 + 1.0);
                *scores.entry(*id).or_insert(0.0) += contribution;
            }
        }

        let mut heap = crate::search::scoring::BoundedScoreHeap::new(limit);
        for (id, score) in scores {
            heap.push(id.to_string(), score);
        }
        heap.into_sorted_vec()
    }

    /// Backward-compatible 2-list wrapper for the historical semantic + FTS
    /// pairing. New call sites should target [`rrf_fuse_n`] directly so they
    /// can plug additional signals without touching this signature.
    pub(crate) fn rrf_fuse(
        semantic_ids: &[&str],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        let fts_refs: Vec<&str> = fts_ids.iter().map(|s| s.as_str()).collect();
        Self::rrf_fuse_n(&[semantic_ids, &fts_refs], limit)
    }

    /// Exposed for property testing only
    #[cfg(test)]
    pub(crate) fn rrf_fuse_test(
        semantic_ids: &[String],
        fts_ids: &[String],
        limit: usize,
    ) -> Vec<(String, f32)> {
        let refs: Vec<&str> = semantic_ids.iter().map(|s| s.as_str()).collect();
        Self::rrf_fuse(&refs, fts_ids, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ReadOnly;
    use crate::test_helpers::setup_store;
    use proptest::prelude::*;

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

    /// P3 #122 regression: when two chunks share a name in the same file but
    /// at different line numbers, the result for `cqs --name-only build`
    /// must list the *earlier* line first. The previous chunk-id-only
    /// tie-breaker sorted "file.rs:10:..." before "file.rs:2:..." because
    /// `"1" < "2"` lexicographically, so the line-10 stub beat the line-2
    /// real definition. Tuple key fixes it: `(file, line_start, id)`.
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

    // ===== Property-based tests for RRF =====

    proptest! {
        /// Property: RRF scores are always positive
        #[test]
        fn prop_rrf_scores_positive(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::<ReadOnly>::rrf_fuse_test(&semantic, &fts, limit);
            for (_, score) in &result {
                prop_assert!(*score > 0.0, "RRF score should be positive: {}", score);
            }
        }

        /// Property: RRF scores are bounded
        /// Note: Duplicates in input lists can accumulate extra points.
        /// Max theoretical: sum of 1/(K+r+1) for all appearances across both lists.
        #[test]
        fn prop_rrf_scores_bounded(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..20),
            fts in prop::collection::vec("[a-z]{1,5}", 0..20),
            limit in 1usize..50
        ) {
            let result = Store::<ReadOnly>::rrf_fuse_test(&semantic, &fts, limit);
            // Conservative upper bound: sum of first N terms of 1/(K+r+1) for both lists
            // where N is max list length (20). With duplicates, actual max is ~0.3
            let max_possible = 0.5; // generous bound accounting for duplicates
            for (id, score) in &result {
                prop_assert!(
                    *score <= max_possible,
                    "RRF score {} for '{}' exceeds max {}",
                    score, id, max_possible
                );
            }
        }

        /// Property: RRF respects limit
        #[test]
        fn prop_rrf_respects_limit(
            semantic in prop::collection::vec("[a-z]{1,5}", 0..30),
            fts in prop::collection::vec("[a-z]{1,5}", 0..30),
            limit in 1usize..20
        ) {
            let result = Store::<ReadOnly>::rrf_fuse_test(&semantic, &fts, limit);
            prop_assert!(
                result.len() <= limit,
                "Result length {} exceeds limit {}",
                result.len(), limit
            );
        }

        /// Property: RRF results are sorted by score descending
        #[test]
        fn prop_rrf_sorted_descending(
            semantic in prop::collection::vec("[a-z]{1,5}", 1..20),
            fts in prop::collection::vec("[a-z]{1,5}", 1..20),
            limit in 1usize..50
        ) {
            let result = Store::<ReadOnly>::rrf_fuse_test(&semantic, &fts, limit);
            for window in result.windows(2) {
                prop_assert!(
                    window[0].1 >= window[1].1,
                    "Results not sorted: {} < {}",
                    window[0].1, window[1].1
                );
            }
        }

        /// Property: Items appearing in both lists get higher scores
        /// Note: Uses hash_set to ensure unique IDs - duplicates in input lists
        /// accumulate scores which can violate the "overlap wins" property.
        #[test]
        fn prop_rrf_rewards_overlap(
            common_id in "[a-z]{3}",
            only_semantic in prop::collection::hash_set("[A-Z]{3}", 1..5),
            only_fts in prop::collection::hash_set("[0-9]{3}", 1..5)
        ) {
            let mut semantic = vec![common_id.clone()];
            semantic.extend(only_semantic);
            let mut fts = vec![common_id.clone()];
            fts.extend(only_fts);

            let result = Store::<ReadOnly>::rrf_fuse_test(&semantic, &fts, 100);

            let common_score = result.iter()
                .find(|(id, _)| id == &common_id)
                .map(|(_, s)| *s)
                .unwrap_or(0.0);

            let max_single = result.iter()
                .filter(|(id, _)| id != &common_id)
                .map(|(_, s)| *s)
                .fold(0.0f32, |a, b| a.max(b));

            prop_assert!(
                common_score >= max_single,
                "Common item score {} should be >= single-list max {}",
                common_score, max_single
            );
        }
    }

    // ===== Direct rrf_fuse_n tests (#1130 phase 1) =====
    //
    // The 2-list rrf_fuse path is already exhaustively covered by the
    // proptests above (it now delegates to rrf_fuse_n). These tests exercise
    // the new generic surface with shapes the wrapper doesn't reach: empty
    // input list, a single list, three lists, and "rewards overlap" across
    // 3+ sources.

    #[test]
    fn rrf_fuse_n_empty_input_returns_empty() {
        let r = Store::<ReadOnly>::rrf_fuse_n(&[], 10);
        assert!(r.is_empty());
    }

    #[test]
    fn rrf_fuse_n_each_list_independently_dedupes() {
        // A list with the same id at rank 1 and rank 5 should contribute
        // exactly the rank-1 score, not their sum.
        let list: &[&str] = &["a", "b", "c", "d", "a", "e"];
        let r = Store::<ReadOnly>::rrf_fuse_n(&[list], 10);
        let a_score = r.iter().find(|(id, _)| id == "a").map(|(_, s)| *s).unwrap();
        // RRF formula: 1 / (K + 1) where K = rrf_k() (default 60). Should NOT
        // include the second occurrence's contribution.
        let k = rrf_k();
        let expected = 1.0 / (k + 1.0);
        assert!(
            (a_score - expected).abs() < 1e-6,
            "a should score {} (rank-1 only, dedup wins), got {}",
            expected,
            a_score
        );
    }

    #[test]
    fn rrf_fuse_n_three_lists_cumulative_overlap() {
        // A chunk that appears at rank 1 in all three lists should score
        // 3× the rank-1 contribution. Single-list participants score 1×.
        let l_sem: &[&str] = &["common", "x", "y"];
        let l_fts: &[&str] = &["common", "z"];
        let l_splade: &[&str] = &["common", "w"];

        let r = Store::<ReadOnly>::rrf_fuse_n(&[l_sem, l_fts, l_splade], 10);
        let k = rrf_k();
        let single = 1.0 / (k + 1.0);
        let triple = 3.0 * single;

        let common_score = r
            .iter()
            .find(|(id, _)| id == "common")
            .map(|(_, s)| *s)
            .unwrap();
        let x_score = r.iter().find(|(id, _)| id == "x").map(|(_, s)| *s).unwrap();

        assert!(
            (common_score - triple).abs() < 1e-6,
            "common at rank 1 in 3 lists should score {} (3× single), got {}",
            triple,
            common_score
        );
        assert!(
            (x_score - 1.0 / (k + 2.0)).abs() < 1e-6,
            "x at rank 2 in semantic-only should score {}, got {}",
            1.0 / (k + 2.0),
            x_score
        );
        // Common must outrank single-list participants — preserves the
        // "rewards overlap" property at N=3.
        assert!(common_score > x_score);
    }

    #[test]
    fn rrf_fuse_n_respects_limit_with_many_lists() {
        let l1: &[&str] = &["a", "b"];
        let l2: &[&str] = &["c", "d"];
        let l3: &[&str] = &["e", "f"];
        let l4: &[&str] = &["g", "h"];
        let r = Store::<ReadOnly>::rrf_fuse_n(&[l1, l2, l3, l4], 3);
        assert_eq!(r.len(), 3);
        // Sorted descending — first ≥ second ≥ third.
        assert!(r[0].1 >= r[1].1);
        assert!(r[1].1 >= r[2].1);
    }
}
