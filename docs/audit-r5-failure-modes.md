# R@5 Failure-Mode Audit — v3 test

**Run:** 2026-07-05 08:48:05  
**Config:** current shipping config (hybrid RRF, per-category α, `--splade` leg on; see `cqs --version`)
**Queries:** 109 (loaded from `v3_test.v2.json`)

## Top-line

| Metric | strict % | permissive % | Δ |
|---|---|---|---|
| R@1  | 9.2% (10) | 47.7% (52) | +38.5pp |
| R@5  | 19.3% (21) | 72.5% (79) | +53.2pp |
| R@20 | 24.8% (27) | 85.3% (93) | +60.6pp |

- **Strict** = exact `(origin, name, line_start)` match — what `cqs eval` reports.
- **Permissive** = also accept `(basename(origin), name, line_start)` and `name`-only matches. The gap reveals stale gold paths (worktree carve-outs, doc-to-code rename).

### Match-kind composition

| match_kind | N |
|---|---|
| `name` | 66 |
| `strict` | 27 |
| `none` | 16 |

The strict R@5 → R@20 gap is **5.5pp**. 
This audit decomposes the 14 queries that landed in rank [6, 20].

### Baseline drift note

ROADMAP records v1.27.0 baseline as **R@1 42.2 / R@5 64.2 / R@20 78.9** on v3 test. 
This audit run measures lower (see Top-line above). The corpus has grown since the 
baseline measurement (~14.9k → 16.6k chunks); some near-misses below are likely 
genuinely new chunks competing with gold. Re-baselining after this audit is advisable.

## Failure modes (rank 6-20)

Each near-miss query is tagged with one or more failure modes. Counts overlap.

| Mode | N | % of near-misses |
|---|---|---|
| `near_dup_crowding` | 10 | 71.4% |
| `unexplained` | 4 | 28.6% |
| `wrong_abstraction_top_too_small` | 2 | 14.3% |
| `eval_artifact_docs` | 2 | 14.3% |
| `truncated_gold` | 1 | 7.1% |

### Mode definitions

- **eval_artifact_worktree** — gold lives under `.claude/worktrees/`. Origin path won't survive worktree cleanup; counts as miss against the parent index.
- **eval_artifact_docs** — gold lives in `docs/` (often code-block targets in superseded plan files). Real production code with the same name may rank well; gold path is the artifact.
- **classifier_misroute** — router predicted a non-Unknown category that disagrees with the gold label. SPLADE alpha applied was wrong for the actual query type.
- **near_dup_crowding** — top-5 contains ≥3 chunks from the same file or with the same function name. Diversity-starved retrieval — MMR target.
- **wrong_abstraction_top_too_big** — top-5 median chunk is ≥2x longer than gold (large orchestrators crowd out the targeted detail).
- **wrong_abstraction_top_too_small** — top-5 median chunk is <1/3 of gold (small detail chunks crowd out the gold orchestrator).
- **truncated_gold** — gold chunk is <5 lines. Likely a thin signature/struct that loses content-vector signal vs longer matches.
- **unexplained** — no heuristic fires. Likely embedding-space lexical mismatch; needs LLM-driven analysis or cross-encoder reranking.

## Per-category breakdown

| Category | N | `eval_artifact_docs` | `near_dup_crowding` | `truncated_gold` | `unexplained` | `wrong_abstraction_top_too_small` |
|---|---|---|---|---|---|---|
| behavioral_search | 4 | 1 | 2 | 0 | 1 | 0 |
| conceptual_search | 1 | 0 | 1 | 0 | 0 | 0 |
| cross_language | 3 | 0 | 2 | 0 | 1 | 0 |
| negation | 5 | 0 | 3 | 0 | 1 | 1 |
| structural_search | 2 | 0 | 1 | 1 | 0 | 0 |
| type_filtered | 4 | 1 | 1 | 0 | 1 | 1 |

## Near-miss queries (rank 6-20)

### `[rank 6]` `cross_language` — how to define a table with a composite primary key in SQL vs MongoDB

- **Gold:** `sparse_vectors` in `src/schema.sql` (L175-181, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `metadata` in `src/schema.sql` (L66-69) score=0.463
  2. `metadata` in `docs/DESIGN_SPEC_27k_tokens.md` (L937-940) score=0.461
  3. `type_edges` in `src/schema.sql` (L206-213) score=0.457
  4. `notes` in `src/schema.sql` (L219-235) score=0.424
  5. `sql_table_with_blank_separated_comment_keeps_doc_none` in `src/parser/chunk.rs` (L2540-2560) score=0.423

### `[rank 6]` `negation` — table definition for chunks that is not a view

- **Gold:** `chunks` in `src/schema.sql` (L41-71, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `schema_sql_chunks_every_create_table` in `src/parser/chunk.rs` (L1195-1230) score=0.593
  2. `sql_tables_around_error_recovery_all_chunk` in `src/parser/chunk.rs` (L1240-1271) score=0.563
  3. `sql_table_with_blank_separated_comment_keeps_doc_none` in `src/parser/chunk.rs` (L2540-2560) score=0.547
  4. `ChunkClass` in `src/language/mod.rs` (L784-792) score=0.543
  5. `ChunkType` in `src/language/mod.rs` (L794-892) score=0.537

### `[rank 6]` `type_filtered` — methods for onboarding commands

- **Gold:** `cmd_onboard` in `src/cli/commands/search/onboard.rs` (L8-141, `function`, `rust`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `dispatch_onboard` in `src/cli/batch/handlers/info.rs` (L212-238) score=0.593
  2. `Commands` in `src/cli/definitions.rs` (L407-1111) score=0.569
  3. `Commands` in `src/cli/definitions.rs` (L1118-1284) score=0.567
  4. `cmd_onboard_dispatch` in `src/cli/commands/dispatch_shims.rs` (L597-615) score=0.558
  5. `build_batch_cmd` in `src/cli/batch/json_args.rs` (L243-603) score=0.556

### `[rank 7]` `cross_language` — how to implement a multi-part string hash in Python vs Go

- **Gold:** `_hash` in `evals/llm_client.py` (L103-108, `function`, `python`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `python_hash_comment_does_not_change_canonical_hash` in `src/parser/chunk.rs` (L1419-1423) score=0.640
  2. `python_code_change_does_change_canonical_hash` in `src/parser/chunk.rs` (L1426-1430) score=0.625
  3. `canonical_hash_fallback` in `src/parser/chunk.rs` (L179-183) score=0.577
  4. `hash_file` in `src/hnsw/persist.rs` (L211-228) score=0.521
  5. `_hash` in `evals/claude_client.py` (L47-52) score=0.512

### `[rank 8]` `conceptual_search` — composite primary key table definition

- **Gold:** `llm_summaries` in `src/schema.sql` (L208-215, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `metadata` in `docs/DESIGN_SPEC_27k_tokens.md` (L937-940) score=0.408
  2. `type_edges` in `src/schema.sql` (L206-213) score=0.394
  3. `metadata` in `src/schema.sql` (L66-69) score=0.380
  4. `file_registry` in `src/schema.sql` (L139-145) score=0.374
  5. `sparse_vectors` in `src/schema.sql` (L252-258) score=0.372

### `[rank 8]` `negation` — LLMClient validate method that is not synchronous

- **Gold:** `LLMClient` in `evals/llm_client.py` (L111-286, `class`, `python`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `LlmClient` in `src/llm/mod.rs` (L629-656) score=0.697
  2. `LlmClient` in `src/llm/batch.rs` (L26-367) score=0.696
  3. `LlmClient` in `src/llm/prompts.rs` (L189-326) score=0.688
  4. `LlmClient` in `src/llm/batch.rs` (L369-435) score=0.677
  5. `LlmClient` in `src/llm/mod.rs` (L623-627) score=0.632

### `[rank 8]` `structural_search` — enum variants for different kinds of code chunks

- **Gold:** `CODE_KINDS` in `evals/generate_from_chunks.py` (L51-52, `constant`, `python`)
- **Failure modes:** `near_dup_crowding`, `truncated_gold`
- **Top-5:**
  1. `ChunkType` in `docs/DESIGN_SPEC_27k_tokens.md` (L177-181) score=0.871
  2. `ChunkClass` in `src/language/mod.rs` (L784-792) score=0.869
  3. `ChunkType` in `src/language/mod.rs` (L794-892) score=0.740
  4. `code_types` in `src/language/mod.rs` (L859-861) score=0.732
  5. `define_chunk_types` in `src/language/mod.rs` (L553-699) score=0.718

### `[rank 9]` `behavioral_search` — how does train_pairs extract NL description and code pairs for embedding fine-tuning

- **Gold:** `load_pairs` in `docs/superpowers/plans/2026-03-20-code-reranker.md` (L43-92, `function`, `python`)
- **Failure modes:** `eval_artifact_docs`, `near_dup_crowding`
- **Top-5:**
  1. `build_nl_description` in `src/cli/commands/train/train_pairs.rs` (L23-50) score=0.545
  2. `TrainPair` in `src/cli/commands/train/train_pairs.rs` (L14-20) score=0.542
  3. `extract_params_nl` in `src/nl/mod.rs` (L387-428) score=0.488
  4. `build_nl_without_doc` in `src/cli/commands/train/train_pairs.rs` (L203-208) score=0.486
  5. `cmd_train_pairs` in `src/cli/commands/train/train_pairs.rs` (L82-165) score=0.483

### `[rank 10]` `behavioral_search` — evict old entries from the global embedding cache by size limit

- **Gold:** `evict` in `src/cache/query_cache.rs` (L114-192, `method`, `rust`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `EmbeddingCache` in `src/cache/embedding_cache.rs` (L26-789) score=0.728
  2. `evict_embeddings_cache_with_runtime` in `src/cli/batch/mod.rs` (L333-401) score=0.658
  3. `embedding_cache_evict_lock_is_process_global` in `src/cache/embedding_cache.rs` (L2155-2167) score=0.656
  4. `EmbeddingCache` in `src/cache/embedding_cache.rs` (L15-24) score=0.603
  5. `evict` in `src/cache/embedding_cache.rs` (L438-519) score=0.581

### `[rank 10]` `type_filtered` — method implementations on the Store struct

- **Gold:** `Store` in `docs/DESIGN_SPEC_27k_tokens.md` (L979-1035, `impl`, `rust`)
- **Failure modes:** `eval_artifact_docs`, `near_dup_crowding`, `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `Store` in `src/cli/pipeline/chunkloss_interleaving_model.rs` (L155-158) score=0.740
  2. `Store` in `src/cli/batch/tasks/publication_interleaving_model.rs` (L166-168) score=0.738
  3. `Store` in `docs/DESIGN_SPEC_27k_tokens.md` (L928-930) score=0.695
  4. `advance_store` in `src/hnsw/persist.rs` (L2997-3023) score=0.592
  5. `Store` in `docs/AUDIT_2026-01-31.md` (L115-122) score=0.588

### `[rank 13]` `negation` — load vectors from directory not using a database

- **Gold:** `load_with_dim` in `src/hnsw/persist.rs` (L686-954, `method`, `rust`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `load_vectors` in `examples/exp_level_scale.rs` (L134-144) score=0.617
  2. `count_vectors` in `src/hnsw/persist.rs` (L1394-1477) score=0.592
  3. `try_load_base_with_ef` in `src/hnsw/persist.rs` (L1498-1504) score=0.583
  4. `try_load_with_ef` in `src/hnsw/persist.rs` (L1484-1490) score=0.577
  5. `try_load_named` in `src/hnsw/persist.rs` (L1507-1545) score=0.548

### `[rank 15]` `cross_language` — SQL equivalent of a TypeScript interface for a code chunk table

- **Gold:** `chunks` in `src/schema.sql` (L41-71, `struct`, `sql`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `ChunkType` in `src/language/mod.rs` (L794-892) score=0.706
  2. `type_edges` in `src/schema.sql` (L206-213) score=0.702
  3. `schema_sql_chunks_every_create_table` in `src/parser/chunk.rs` (L1195-1230) score=0.647
  4. `sql_tables_around_error_recovery_all_chunk` in `src/parser/chunk.rs` (L1240-1271) score=0.623
  5. `ChunkType` in `docs/DESIGN_SPEC_27k_tokens.md` (L177-181) score=0.621

### `[rank 16]` `negation` — load HNSW index from disk not build from scratch

- **Gold:** `try_load_with_ef` in `src/hnsw/persist.rs` (L1068-1074, `method`, `rust`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `HnswIndex` in `src/hnsw/build.rs` (L8-283) score=0.711
  2. `HnswIndex` in `src/hnsw/persist.rs` (L682-1546) score=0.697
  3. `build_hnsw_index_owned_round_trips_through_disk` in `docs/audit-fix-scratch-v1.33.0/p2-b.md` (L2414-2422) score=0.693
  4. `build_hnsw_index_owned_round_trips_through_disk` in `docs/audit-fix-prompts-v1.30.0.md` (L5798-5806) score=0.677
  5. `build_hnsw_index` in `src/cli/commands/index/build.rs` (L1511-1517) score=0.669

### `[rank 18]` `behavioral_search` — retry with exponential backoff

- **Gold:** `record_failure` in `src/cli/watch/rebuild.rs` (L136-145, `method`, `rust`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `backoff_after_failure_delays_retry` in `src/cli/watch/tests.rs` (L155-164) score=0.517
  2. `backoff_initial_state_allows_retry` in `src/cli/watch/tests.rs` (L149-152) score=0.508
  3. `retry_with_backoff` in `tests/fixtures/eval_python.py` (L11-21) score=0.507
  4. `DEFAULT_RETRY_BACKOFFS_MS` in `src/llm/local.rs` (L51-51) score=0.482
  5. `RetryWithBackoff` in `tests/fixtures/eval_go.go` (L18-32) score=0.450

## Strategy implications

Each lever's *expected R@5 lift on v3 test* is a back-of-envelope ceiling — assumes the lever fully solves every query in its mode. Real lift will be a fraction of that, and modes overlap (a single query can be both `near_dup_crowding` AND `wrong_abstraction_*`), so additive ceilings are wrong.

| Mode | Near-misses | R@5 ceiling if fully solved | Lever |
|---|---|---|---|
| `near_dup_crowding` | 10/109 | +9.2pp | MMR re-rank on top-K pool (λ≈0.5-0.7). Cheap, no model change. |
| `unexplained` | 4/109 | +3.7pp | Reranker V2 (Phase 2 in flight) — catches lexical mismatch via cross-encoder. |
| `wrong_abstraction_top_too_small` | 2/109 | +1.8pp | Boost orchestrators when query verbs/nouns suggest top-down (e.g. 'workflow', 'pipeline'). |
| `eval_artifact_docs` | 2/109 | +1.8pp | Eval-data fix or re-judging: gold-in-docs is often a superseded plan target. Check if production code with same name exists; if so, swap gold. |
| `truncated_gold` | 1/109 | +0.9pp | Chunker fix: pad short chunks with leading docstring/comment block. Schema-level lift. |

**Recommended ordering** (by effort/impact):

1. **Eval-data hygiene first** (2 queries). Re-baselining without fixing eval artifacts means we're chasing noise.
2. **MMR for `near_dup_crowding`** (10 queries — biggest single mode). 1-2 day implementation, no model change. Sanity-check on v3 dev before merging.
3. **Reranker V2 for `unexplained`** (4 queries). Already in flight (Phase 2 corpus build). Wait for trained model.
4. **Chunker tuning for `truncated_gold`** (1 queries). Bigger lift; gated on training-data signal that it's worth a reindex.
5. **Skip `classifier_misroute` standalone work**. Centroid pilot proved this lever is harder than it looks (−4.6pp). Better lift comes from removing the router entirely once Reranker V2 lands.

