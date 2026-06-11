## Algorithm Correctness

#### `extract_file_from_chunk_id` mis-parses window indices ≥ 100
- **Difficulty:** easy
- **Location:** src/search/scoring/filter.rs:44-53 (the `wN` arm of `is_window_suffix`)
- **Description:** `is_window_suffix` accepts `wN` only when the suffix length is `≤ 3` (so `w0`..`w99`). `apply_windowing` (`src/cli/pipeline/windowing.rs:65`) and the legacy chunker (`src/cli/pipeline/mod.rs:483`) format ids as `format!("{}:w{}", parent_id, window_idx)` where `window_idx` is `u32` and uncapped. A chunk whose tokenized length needs ≥ 100 windows therefore produces an id ending in `:w100` (length 4), which `is_window_suffix` rejects. `extract_file_from_chunk_id` then strips only the standard `(line, hash)` pair — the result is `path:line:hash` instead of `path`, corrupting every downstream consumer that joins by file or de-dups across windows: SPLADE fusion `dense_results`/`sparse_results` ID set, `extract_file_from_chunk_id` callers in `search/query.rs:246` / `:335`, scout's same-file scoring, glob filtering on file path, etc. With BGE/E5 (~480 token windows) the threshold is ~50,000-token chunks, but the markdown / image / data-file pipelines can comfortably exceed that, and the `tNwM` form is similarly capped via the inner-digit `bytes.len() >= 4` lower bound only — `t100w0` etc. still parse, but the generic-window arm is the easy regression. The `:t12w99` test is the largest case asserted today.
- **Suggested fix:** Drop the `bytes.len() <= 3` ceiling in the generic `wN` arm — the only structural requirement is `'w'` followed by 1+ ASCII digits to end-of-segment. The current upper bound is purely incidental (it was sized for the v0 100-window cap that no longer exists). Add a regression test for `path:10:abc12345:w100` and `path:10:abc12345:w999`.

#### `+ Inf` note sentiment silently zeroes the boosted chunk's score
- **Difficulty:** easy
- **Location:** src/search/scoring/note_boost.rs:131-134, 204-220 (final `1.0 + s * factor`); pipeline at src/search/scoring/candidate.rs:328
- **Description:** `note_stats` documents that `±Inf` sentiment round-trips through SQLite (`store/notes.rs:492`, asserted by `test_upsert_notes_infinity_sentiment_roundtrips`). Neither `NoteBoostIndex::boost` nor `OwnedNoteBoostIndex::boost` checks finiteness before computing `1.0 + s * note_boost_factor`, so an `+Inf` mention produces a `+Inf` multiplier. In `apply_scoring_pipeline` (`candidate.rs:328`), `base_score.max(0.0) * +Inf == +Inf` (or `NaN` when base is exactly 0.0), the `score >= threshold` guard accepts it, and the candidate flows up to `BoundedScoreHeap::push` (`candidate.rs:213`) where the `is_finite()` check drops it on the floor. Symmetrically, `-Inf` produces `-Inf` and fails `score >= threshold`, returning `None`. Either way, a single mention with extreme sentiment hides every chunk it boosts from search results — exactly opposite to the intent of "boost". (`0.0 * Inf = NaN` is also a separate hit on identical-vector cases.)
- **Suggested fix:** Either (a) clamp sentiment to `[-1.0, 1.0]` (the documented discrete value range from CLAUDE.md) inside `NoteBoostIndex::new` / `OwnedNoteBoostIndex::new`, or (b) reject non-finite sentiments in `notes::upsert_*` so the storage layer enforces the invariant. (a) is a one-line change and matches the existing `clamp(0.0, 1.0)` defense-in-depth pattern in `apply_scoring_pipeline:311`.

#### `compute_scores_opt` per-chunk empty-tokenization fallback emits 0.5 in mixed cohort
- **Difficulty:** medium
- **Location:** src/reranker.rs:299-311
- **Description:** `run_chunk` checks `max_len == 0` and returns `vec![sigmoid(0.0); batch_size]` (= 0.5 per passage) when *this chunk's* longest encoding tokenized empty. The aggregate guard in `compute_scores_opt:267` only short-circuits when the *entire* input is empty. So with a mix of empty + non-empty chunks (e.g. one batch of 32 happens to be all-empty after `take(stage1_limit)` lands on candidates whose passages tokenize to nothing), the empty chunk's 32 results all get score 0.5, and the non-empty chunks return their cross-encoder sigmoid scores in [0, 1]. After `apply_rerank_scores` sorts by score, the 0.5 cohort sits in the middle of the cross-encoder distribution rather than at the tail where missing-data should land. Anything the cross-encoder rated below 0.5 is now ranked behind passages it never saw.
- **Suggested fix:** Return `None` (or a sentinel) for empty chunks and have `compute_scores_opt` fall back to skipping the rerank call rather than producing zero-information scores. If preserving order matters, use the input cosine score (which the candidates already carry as `SearchResult.score`) for empty-tokenization rows so the surviving cohort stays homogeneous.

#### `select_negatives` `take(k)` upstream of empty-content drop yields fewer than k
- **Difficulty:** easy
- **Location:** src/train_data/bm25.rs:155-184
- **Description:** The pipeline is `filter(hash != positive) → filter(content_hash != positive_content_hash) → take(k) → filter_map(drop empty content)`. When some of the top-k entries returned by `score()` map to an empty-content row (rare but possible: pre-content-hash data, or rows where the content was rewritten to empty between BM25 build and select call), the empty rows count toward `k` *before* being dropped, so the caller sees fewer than `k` negatives even when more candidates exist. The docstring promises "top-k negatives".
- **Suggested fix:** Move the empty-content `filter_map` ahead of `take(k)`, or convert the whole chain to a `for` loop that skips empty rows and continues until `k` non-empty negatives are accumulated.

#### `weight as f32` strips NaN guard on sparse vector load
- **Difficulty:** easy
- **Location:** src/store/sparse.rs:381-401
- **Description:** Sparse vector load reads each row's `weight: f64`, applies a `token_id` range check, and pushes `(token_id as u32, weight as f32)` into the per-chunk vector with no finiteness check. SPLADE encoding is non-negative by design (ReLU on logits), but a corrupted row, a future encoder switch, or a manual SQL update can land a `NaN`/`±Inf` weight. Downstream `splade.search_with_filter → IndexResult::score` then carries the bad value into `search/query.rs:543` min-max normalization (`fold(0.0f32, f32::max)` will silently swallow `NaN` because `NaN.max(x) == x` for the `f32::max` total-order convention but `>` returns false on NaN — the normalization branch then divides finite scores by 0.0 and returns 0.0 for everything else). Either path quietly degrades hybrid fusion.
- **Suggested fix:** Filter (with a `tracing::warn!`) or coerce non-finite weights to 0.0 in the load loop, mirroring the `BoundedScoreHeap` is_finite invariant. Same guard belongs at `store/sparse.rs:400` next to the existing token_id range check.

#### CAGRA env-knob defaults accept `0` (parallel to triaged HNSW issue P1-45)
- **Difficulty:** easy
- **Location:** src/cagra.rs:191-203 (`CQS_CAGRA_GRAPH_DEGREE`, `CQS_CAGRA_INTERMEDIATE_GRAPH_DEGREE`)
- **Description:** P1-45 (triaged ✅ #1326) closed the same hole for HNSW `M`/`ef_construction`/`ef_search`, but the CAGRA branch went unmodified: `std::env::var("CQS_CAGRA_GRAPH_DEGREE").ok().and_then(|v| v.parse().ok()).unwrap_or(64)` accepts a literal `"0"` and forwards it to `set_graph_degree(0)`. cuVS treats `graph_degree=0` as "use library default" on some versions and as an error on others, so the user-visible behavior depends on the cuvs pin — exactly the silent-misconfig scenario P1-45 was filed against. `cagra_max_bytes` / `cagra_stream_batch_size` already route through `parse_env_usize` and inherit its `> 0` guard; only these two knobs slipped through.
- **Suggested fix:** Replace the inline `parse().ok().unwrap_or(64)` calls with `crate::limits::parse_env_usize(...)` which already filters non-positive values and logs the rejection.

#### SPLADE min-max normalization collapses everything to 0.0 on negative-only sparse cohort
- **Difficulty:** medium
- **Location:** src/search/query.rs:543-572
- **Description:** `max_sparse = sparse_results.iter().map(|r| r.score).fold(0.0f32, f32::max)`. With a negative-bearing sparse cohort (no real SPLADE input today, but any future sparse signal that is not non-negative — learned dot-product retrievers, contrastive scores, BM25-like deltas) the fold's seed `0.0` dominates, `max_sparse == 0.0`, and the `if max_sparse > 0.0` branch sends every sparse score to `0.0`. The hybrid fuse then degenerates to `alpha * dense + 0.0` — i.e., dense-only retrieval — without any tracing/warning that the sparse leg was suppressed. Even within today's SPLADE-only path, a query whose entire candidate set scores exactly 0.0 (degenerate empty intersection) hits the same branch.
- **Suggested fix:** Initialize the fold from the first element (`iter().map(|r| r.score).reduce(f32::max)`) so the seed can't dominate, and skip normalization (use scores as-is, or warn) when `max_sparse <= 0.0`. Alternatively, log a `tracing::warn!` when `max_sparse == 0.0` so eval/CI catch silent collapse.

#### `chunk.line_end + 1` on `u32` can overflow in `where_to_add` placement suggestion
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:223
- **Description:** `(chunk.name.clone(), chunk.line_end + 1)` adds 1 to a `u32` line number with no saturation. `line_end == u32::MAX` is unreachable for real source files but reachable for fuzzed/corrupted input or the synthetic L5X paths flagged in P1-41. In debug builds this panics; in release it wraps to 0 and silently produces a placement suggestion at line 0, which then breaks any caller relying on `1 ≤ line ≤ file_lines`.
- **Suggested fix:** `chunk.line_end.saturating_add(1)`. Same mechanical fix as the parser's other line-arithmetic guards (`parser/mod.rs:723`, `cache.rs:986`, etc.).

#### `reverse_bfs` (single-source) lacks the stale-queue guard `reverse_bfs_multi` has
- **Difficulty:** medium
- **Location:** src/impact/bfs.rs:50-87 vs 161-215, 229-292
- **Description:** Single-source `reverse_bfs` is correct *today* because BFS visits depths in non-decreasing order and the `!ancestors.contains_key` guard means the first-insertion depth is the minimum. But the moment the function is reused in a loop that pushes back already-seen nodes (e.g. a hypothetical "expand if newer evidence found shorter path" tweak), or merged with `reverse_bfs_multi` for a future caller that wants both single- and multi-source semantics, the missing stale-entry skip block (`if ancestors.get(&current).is_some_and(|&stored| d > stored) { continue; }`) silently regresses to the bug `test_reverse_bfs_multi_stale_queue_entry` was filed against. The two implementations have diverged enough that a reasonable refactor toward "use `reverse_bfs_multi` everywhere" would have to re-discover the property.
- **Suggested fix:** Either (a) reimplement `reverse_bfs(target, depth)` as a thin wrapper over `reverse_bfs_multi(&[target], depth)` so the two share one verified path, or (b) port the stale-entry skip into `reverse_bfs` defensively. (a) eliminates the duplication and the divergence risk in one move.

#### `total_cmp` tie-break on rerank batch fallback puts 0.5 in the middle of cross-encoder cohort
- **Difficulty:** medium
- **Location:** src/reranker.rs:872-908 + 305-311 interaction
- **Description:** Companion to the empty-tokenization finding above, but the algorithm-correctness angle is in `apply_rerank_scores`: the comment justifying the cohort-homogeneous truncation when `scores.len() < results.len()` (`AC-V1.33-9`) is correct, but the symmetric case — `scores.len() == results.len()` with some scores being the 0.5 fallback from `run_chunk` empty-batch — is not addressed. The sort comparator `b.score.total_cmp(&a.score)` then interleaves true cross-encoder scores (in [0, 1]) with synthetic 0.5 fallbacks within the same cohort, and the deterministic id-tiebreak gives them stable but meaningless ranking. Worst case: a true low-relevance score of 0.3 sits below a 0.5 fallback for a passage the encoder never saw.
- **Suggested fix:** Surface the empty-tokenization rows back to `apply_rerank_scores` (e.g., return `Vec<Option<f32>>` from `compute_scores_opt`) and have it either drop those rows (matching the `< n` cohort-trim policy) or fall back to the input cosine score that survived stage 1. Both options keep the comparator on a single homogeneous distribution.

DONE
