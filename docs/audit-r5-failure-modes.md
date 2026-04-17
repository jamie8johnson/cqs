# R@5 Failure-Mode Audit — v3 test

**Run:** 2026-04-17 02:42:28  
**Config:** v1.27.0 shipping (cross_language α=0.10, BGE-large, no centroid classifier)
**Queries:** 109 (loaded from `v3_test.json`)

## Top-line

| Metric | strict % | permissive % | Δ |
|---|---|---|---|
| R@1  | 33.0% (36) | 42.2% (46) | +9.2pp |
| R@5  | 50.5% (55) | 63.3% (69) | +12.8pp |
| R@20 | 67.9% (74) | 81.7% (89) | +13.8pp |

- **Strict** = exact `(origin, name, line_start)` match — what `cqs eval` reports.
- **Permissive** = also accept `(basename(origin), name, line_start)` and `name`-only matches. The gap reveals stale gold paths (worktree carve-outs, doc-to-code rename).

### Match-kind composition

| match_kind | N |
|---|---|
| `strict` | 74 |
| `none` | 20 |
| `name` | 13 |
| `basename` | 2 |

The strict R@5 → R@20 gap is **17.4pp**. 
This audit decomposes the 20 queries that landed in rank [6, 20].

### Baseline drift note

ROADMAP records v1.27.0 baseline as **R@1 42.2 / R@5 64.2 / R@20 78.9** on v3 test. 
This audit run measures lower (see Top-line above). The corpus has grown since the 
baseline measurement (~14.9k → 16.6k chunks); some near-misses below are likely 
genuinely new chunks competing with gold. Re-baselining after this audit is advisable.

## Failure modes (rank 6-20)

Each near-miss query is tagged with one or more failure modes. Counts overlap.

| Mode | N | % of near-misses |
|---|---|---|
| `near_dup_crowding` | 11 | 55.0% |
| `wrong_abstraction_top_too_big` | 5 | 25.0% |
| `wrong_abstraction_top_too_small` | 5 | 25.0% |
| `unexplained` | 3 | 15.0% |
| `truncated_gold` | 3 | 15.0% |
| `eval_artifact_worktree` | 1 | 5.0% |
| `eval_artifact_docs` | 1 | 5.0% |

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

| Category | N | `eval_artifact_docs` | `eval_artifact_worktree` | `near_dup_crowding` | `truncated_gold` | `unexplained` | `wrong_abstraction_top_too_big` | `wrong_abstraction_top_too_small` |
|---|---|---|---|---|---|---|---|---|
| behavioral_search | 4 | 1 | 0 | 1 | 0 | 1 | 0 | 1 |
| conceptual_search | 3 | 0 | 0 | 1 | 0 | 1 | 1 | 0 |
| cross_language | 3 | 0 | 0 | 2 | 0 | 0 | 1 | 0 |
| identifier_lookup | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 1 |
| multi_step | 7 | 0 | 1 | 3 | 1 | 0 | 2 | 0 |
| negation | 5 | 0 | 0 | 2 | 0 | 1 | 1 | 1 |
| type_filtered | 6 | 0 | 0 | 2 | 2 | 0 | 0 | 2 |

## Near-miss queries (rank 6-20)

### `[rank 6]` `behavioral_search` — filter configuration for search queries with language and path constraints

- **Gold:** `SearchFilter` in `src/store/helpers/search_filter.rs` (L16-59, `struct`, `rust`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `search_with_filter` in `src/hnsw/mod.rs` (L356-363) score=0.859
  2. `HnswIndex` in `src/hnsw/mod.rs` (L175-188) score=0.824
  3. `tc17_search_filtered_accepts_only_matching_ids` in `src/hnsw/build.rs` (L469-502) score=0.819
  4. `tc17_search_filtered_all_rejected_returns_empty` in `src/hnsw/build.rs` (L505-521) score=0.797
  5. `classify_query` in `src/search/router.rs` (L524-666) score=0.786

### `[rank 6]` `multi_step` — classify function AND async definition OR query argument

- **Gold:** `classify` in `evals/llm_client.py` (L157-220, `method`, `python`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `classify_query` in `src/search/router.rs` (L524-666) score=0.845
  2. `bench_classify_query_throughput` in `src/search/router.rs` (L1449-1503) score=0.818
  3. `classify_query` in `docs/plans/adaptive-retrieval.md` (L120-120) score=0.772
  4. `classify` in `src/search/router.rs` (L1000-1029) score=0.768
  5. `resolve_splade_alpha` in `src/search/router.rs` (L413-519) score=0.755

### `[rank 6]` `multi_step` — functions that search for H1 headings AND return a stripped title string

- **Gold:** `extract_document_title` in `scripts/clean_md.py` (L20-27, `function`, `python`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_big`
- **Top-5:**
  1. `HEADING_TAGS_VUE` in `src/language/languages.rs` (L7729-7729) score=0.850
  2. `HEADING_TAGS_SVELTE` in `src/language/languages.rs` (L6773-6773) score=0.839
  3. `HEADING_TAGS_RAZOR` in `src/language/languages.rs` (L5647-5647) score=0.802
  4. `extract_doc_title` in `src/convert/cleaning.rs` (L142-152) score=0.764
  5. `extract_text_content_razor` in `src/language/languages.rs` (L5601-5628) score=0.722

### `[rank 6]` `type_filtered` — method implementations on the Store struct

- **Gold:** `Store` in `src/store/search.rs` (L37-218, `impl`, `rust`)
- **Failure modes:** `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `LANG_RUST` in `src/language/languages.rs` (L6127-6286) score=0.823
  2. `fmt` in `src/store/calls/cross_project.rs` (L26-31) score=0.808
  3. `DeadFunction` in `src/store/calls/mod.rs` (L29-34) score=0.793
  4. `search_by_name` in `src/store/search.rs` (L86-149) score=0.779
  5. `TRAIT_IMPL_RE` in `src/store/calls/mod.rs` (L121-122) score=0.769

### `[rank 7]` `conceptual_search` — schema for tracking function call graph

- **Gold:** `function_calls` in `src/schema.sql` (L78-85, `struct`, `sql`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `CallContext` in `src/nl/mod.rs` (L32-37) score=0.860
  2. `FunctionCallStats` in `src/store/calls/mod.rs` (L110-117) score=0.831
  3. `CallStats` in `src/store/calls/mod.rs` (L101-106) score=0.804
  4. `type_edges` in `src/schema.sql` (L95-102) score=0.801
  5. `find_hotspots` in `src/impact/hints.rs` (L258-272) score=0.794

### `[rank 7]` `negation` — markdown cleaning utility that is not a library

- **Gold:** `main` in `scripts/clean_md.py` (L349-393, `function`, `python`)
- **Failure modes:** `unexplained`
- **Top-5:**
  1. `definition_markdown` in `src/language/languages.rs` (L4246-4248) score=0.852
  2. `LANG_MARKDOWN` in `src/language/languages.rs` (L4167-4244) score=0.794
  3. `pdf_to_markdown` in `src/convert/pdf.rs` (L13-47) score=0.677
  4. `clean_markdown` in `src/convert/cleaning.rs` (L109-139) score=0.615
  5. `clean_markdown_file` in `scripts/clean_md.py` (L257-319) score=0.611

### `[rank 7]` `type_filtered` — impl blocks for ModelConfig

- **Gold:** `ModelConfig` in `src/embedder/models.rs` (L147-383, `impl`, `rust`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `model_config` in `src/embedder/mod.rs` (L340-342) score=0.843
  2. `ensure_model` in `src/embedder/mod.rs` (L897-967) score=0.792
  3. `Cli` in `src/cli/definitions.rs` (L269-290) score=0.787
  4. `Embedder` in `src/embedder/mod.rs` (L257-894) score=0.770
  5. `model_config` in `src/cli/definitions.rs` (L285-289) score=0.751

### `[rank 7]` `type_filtered` — delegate and event handler type definitions

- **Gold:** `StructuralMatcherFn` in `src/language/mod.rs` (L191-191, `typealias`, `rust`)
- **Failure modes:** `near_dup_crowding`, `truncated_gold`
- **Top-5:**
  1. `post_process_swift_swift` in `src/language/languages.rs` (L7020-7095) score=0.683
  2. `Language` in `src/language/mod.rs` (L1003-1057) score=0.676
  3. `extract_method_receiver_type` in `src/parser/chunk.rs` (L330-343) score=0.673
  4. `LANG_FSHARP` in `src/language/languages.rs` (L1593-1710) score=0.662
  5. `LANG_RUST` in `src/language/languages.rs` (L6127-6286) score=0.662

### `[rank 8]` `behavioral_search` — parse function signatures

- **Gold:** `extract_signature` in `src/parser/chunk.rs` (L135-167, `function`, `rust`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `SignatureStyle` in `src/language/mod.rs` (L432-444) score=0.929
  2. `extract_return_powershell` in `src/language/languages.rs` (L4971-4974) score=0.836
  3. `extract_return_scala` in `src/language/languages.rs` (L6328-6350) score=0.831
  4. `extract_return_javascript` in `src/language/languages.rs` (L3213-3217) score=0.828
  5. `extract_return_solidity` in `src/language/languages.rs` (L6407-6424) score=0.826

### `[rank 9]` `behavioral_search` — how does train_pairs extract NL description and code pairs for embedding fine-tuning

- **Gold:** `load_pairs` in `docs/superpowers/plans/2026-03-20-code-reranker.md` (L43-92, `function`, `python`)
- **Failure modes:** `eval_artifact_docs`, `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `TrainPair` in `src/cli/commands/train/train_pairs.rs` (L14-20) score=0.813
  2. `embed_batch` in `src/embedder/mod.rs` (L751-893) score=0.800
  3. `extract_return_nl` in `src/nl/mod.rs` (L433-435) score=0.796
  4. `generate_nl_with_call_context_and_summary` in `src/nl/mod.rs` (L65-154) score=0.791
  5. `make_hidden` in `src/embedder/mod.rs` (L1219-1225) score=0.788

### `[rank 9]` `cross_language` — SQL equivalent of a TypeScript interface for a code chunk table

- **Gold:** `chunks` in `src/schema.sql` (L22-45, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_big`
- **Top-5:**
  1. `ChunkType` in `src/language/mod.rs` (L676-751) score=1.117
  2. `LANG_VUE` in `src/language/languages.rs` (L7852-7949) score=0.979
  3. `type_edges` in `src/schema.sql` (L95-102) score=0.959
  4. `LANG_TYPESCRIPT` in `src/language/languages.rs` (L7334-7442) score=0.852
  5. `post_process_zig_zig` in `src/language/languages.rs` (L8091-8146) score=0.812

### `[rank 9]` `multi_step` — functions that build a query set AND take an existing path

- **Gold:** `build_query_set` in `evals/generate_queries.py` (L270-316, `function`, `python`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `QUERIES_DIR` in `evals/alpha_sweep_v3.py` (L44-44) score=0.744
  2. `QUERIES_DIR` in `evals/audit_r5_failure_modes.py` (L39-39) score=0.744
  3. `QUERIES_DIR` in `evals/build_pools.py` (L28-28) score=0.744
  4. `QUERIES_DIR` in `evals/build_reranker_train.py` (L27-27) score=0.744
  5. `QUERIES_DIR` in `evals/centroid_classifier.py` (L29-29) score=0.744

### `[rank 10]` `type_filtered` — struct definitions in src/cli/commands/infra

- **Gold:** `BatchInput` in `src/cli/batch/commands.rs` (L25-28, `struct`, `rust`)
- **Failure modes:** `truncated_gold`
- **Top-5:**
  1. `Cli` in `src/cli/definitions.rs` (L136-267) score=0.822
  2. `ModelListEntry` in `src/cli/commands/infra/model.rs` (L88-93) score=0.778
  3. `Wrapper` in `src/cli/commands/eval/mod.rs` (L219-222) score=0.768
  4. `EnvVar` in `src/cli/commands/infra/doctor.rs` (L446-449) score=0.767
  5. `Wrapper` in `src/cli/commands/eval/mod.rs` (L202-205) score=0.764

### `[rank 12]` `conceptual_search` — composite primary key table definition

- **Gold:** `llm_summaries` in `src/schema.sql` (L167-174, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_big`
- **Top-5:**
  1. `migrate_v15_to_v16` in `src/store/migrations.rs` (L353-384) score=0.753
  2. `migrate_v16_to_v17` in `src/store/migrations.rs` (L392-417) score=0.711
  3. `llm_summaries_v2` in `docs/superpowers/specs/2026-03-19-improve-docs-design.md` (L38-45) score=0.697
  4. `metadata` in `src/schema.sql` (L17-20) score=0.677
  5. `migrate_v12_to_v13` in `src/store/migrations.rs` (L281-295) score=0.664

### `[rank 12]` `cross_language` — how to define a table with foreign key constraints in SQLite vs PostgreSQL

- **Gold:** `type_edges` in `src/schema.sql` (L95-102, `struct`, `sql`)
- **Failure modes:** `near_dup_crowding`
- **Top-5:**
  1. `migrate_v15_to_v16` in `src/store/migrations.rs` (L353-384) score=0.747
  2. `definition_sql` in `src/language/languages.rs` (L6631-6633) score=0.731
  3. `CandidateRow` in `src/store/helpers/rows.rs` (L27-40) score=0.730
  4. `migrate_v10_to_v11` in `src/store/migrations.rs` (L234-257) score=0.728
  5. `migrate_v11_to_v12` in `src/store/migrations.rs` (L263-272) score=0.719

### `[rank 12]` `negation` — audit mode that forces direct code examination without cached notes

- **Gold:** `invalidate_mutable_caches` in `src/cli/batch/mod.rs` (L373-383, `method`, `rust`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_big`
- **Top-5:**
  1. `main` in `evals/audit_r5_failure_modes.py` (L212-446) score=0.759
  2. `audit_state` in `src/cli/batch/mod.rs` (L865-868) score=0.753
  3. `invalidate_mutable_caches` in `src/cli/batch/mod.rs` (L384-427) score=0.720
  4. `notes` in `src/cli/batch/mod.rs` (L871-894) score=0.709
  5. `OUT_MD` in `evals/audit_r5_failure_modes.py` (L43-43) score=0.679

### `[rank 13]` `multi_step` — structs that have a project String AND flatten CallerInfo

- **Gold:** `CrossProjectCaller` in `.claude/worktrees/agent-a7cedd3c/src/store/calls/cross_project.rs` (L36-41, `struct`, `rust`)
- **Failure modes:** `eval_artifact_worktree`, `wrong_abstraction_top_too_big`
- **Top-5:**
  1. `CallersArgs` in `src/cli/args.rs` (L281-292) score=0.755
  2. `CallersArgs` in `src/cli/args.rs` (L253-259) score=0.749
  3. `CrossProjectContext` in `src/store/calls/cross_project.rs` (L64-68) score=0.748
  4. `post_process_solidity_solidity` in `src/language/languages.rs` (L6512-6530) score=0.716
  5. `post_process_cpp_cpp` in `src/language/languages.rs` (L273-306) score=0.715

### `[rank 13]` `negation` — training data generator that is not for non-git repositories

- **Gold:** `generate_training_data` in `src/train_data/mod.rs` (L96-367, `function`, `rust`)
- **Failure modes:** `near_dup_crowding`, `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `git_log_rejects_non_repo` in `src/train_data/git.rs` (L535-542) score=0.799
  2. `validate_git_repo_rejects_non_repo` in `src/train_data/git.rs` (L500-514) score=0.786
  3. `git_log` in `src/train_data/git.rs` (L65-126) score=0.765
  4. `validate_git_repo` in `src/train_data/git.rs` (L25-46) score=0.763
  5. `Embedder` in `src/embedder/mod.rs` (L217-249) score=0.745

### `[rank 17]` `multi_step` — virtual table using fts5 AND tokenize unicode61

- **Gold:** `notes_fts` in `src/schema.sql` (L123-127, `struct`, `sql`)
- **Failure modes:** `truncated_gold`
- **Top-5:**
  1. `extract_name_fallback_with_unicode_before_keyword` in `src/parser/chunk.rs` (L986-990) score=0.681
  2. `TYPE_HINT_TABLE` in `src/search/router.rs` (L850-855) score=0.678
  3. `v` in `src/daemon_translate.rs` (L157-159) score=0.666
  4. `v` in `src/daemon_translate.rs` (L292-294) score=0.666
  5. `map_code_language_latex` in `src/language/languages.rs` (L3763-3788) score=0.662

### `[rank 18]` `identifier_lookup` — Store::open_readonly_small reference index mmap size

- **Gold:** `load_references` in `src/reference.rs` (L189-224, `function`, `rust`)
- **Failure modes:** `wrong_abstraction_top_too_small`
- **Top-5:**
  1. `open_readonly` in `docs/superpowers/plans/2026-04-02-command-context-refactor.md` (L34-37) score=0.756
  2. `store` in `src/cli/batch/mod.rs` (L486-489) score=0.746
  3. `store` in `src/cli/batch/mod.rs` (L552-555) score=0.746
  4. `DEFAULT_SPLADE_MAX_INDEX_BYTES` in `src/splade/index.rs` (L56-56) score=0.739
  5. `open_readonly_small` in `src/store/mod.rs` (L766-778) score=0.721

## Strategy implications

Each lever's *expected R@5 lift on v3 test* is a back-of-envelope ceiling — assumes the lever fully solves every query in its mode. Real lift will be a fraction of that, and modes overlap (a single query can be both `near_dup_crowding` AND `wrong_abstraction_*`), so additive ceilings are wrong.

| Mode | Near-misses | R@5 ceiling if fully solved | Lever |
|---|---|---|---|
| `near_dup_crowding` | 11/109 | +10.1pp | MMR re-rank on top-K pool (λ≈0.5-0.7). Cheap, no model change. |
| `wrong_abstraction_top_too_big` | 5/109 | +4.6pp | Chunk-type aware boost when query intent demands detail (e.g. `extract_*` queries → leaf functions). |
| `wrong_abstraction_top_too_small` | 5/109 | +4.6pp | Boost orchestrators when query verbs/nouns suggest top-down (e.g. 'workflow', 'pipeline'). |
| `truncated_gold` | 3/109 | +2.8pp | Chunker fix: pad short chunks with leading docstring/comment block. Schema-level lift. |
| `unexplained` | 3/109 | +2.8pp | Reranker V2 (Phase 2 in flight) — catches lexical mismatch via cross-encoder. |
| `eval_artifact_worktree` | 1/109 | +0.9pp | Eval-data fix: rebuild v3 test fixture from current corpus (gold paths drift when worktrees come and go). |
| `eval_artifact_docs` | 1/109 | +0.9pp | Eval-data fix or re-judging: gold-in-docs is often a superseded plan target. Check if production code with same name exists; if so, swap gold. |

**Recommended ordering** (by effort/impact):

1. **Eval-data hygiene first** (2 queries). Re-baselining without fixing eval artifacts means we're chasing noise.
2. **MMR for `near_dup_crowding`** (11 queries — biggest single mode). 1-2 day implementation, no model change. Sanity-check on v3 dev before merging.
3. **Reranker V2 for `unexplained`** (3 queries). Already in flight (Phase 2 corpus build). Wait for trained model.
4. **Chunker tuning for `truncated_gold`** (3 queries). Bigger lift; gated on training-data signal that it's worth a reindex.
5. **Skip `classifier_misroute` standalone work**. Centroid pilot proved this lever is harder than it looks (−4.6pp). Better lift comes from removing the router entirely once Reranker V2 lands.

