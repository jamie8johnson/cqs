## Test Coverage (happy path)

#### TC-HAP-V1.36-1: `serve::data::build_stats` has no positive test
- **Difficulty:** easy
- **Location:** src/serve/data.rs:1109 — `build_stats` (1 caller: `handlers::stats`)
- **Description:** All sibling builders in `serve/data.rs` (`build_graph`, `build_chunk_detail`, `build_hierarchy`, `build_cluster`) have populated-store positive tests under the `TC-HAP-1.29-1` block in `src/serve/tests.rs:1010-1190`. `build_stats` is the only one without a direct positive test — only the `/api/stats` HTTP layer exercises it, and only against a tiny fixture without verifying the four numeric fields (`total_chunks`, `total_files`, `call_edges`, `type_edges`). Schema regressions (`call_edges` vs prior `total_call_edges`, missing `type_edges` after the type-edge migration) would slip through.
- **Suggested fix:** Add `build_stats_returns_correct_counts_for_populated_store` next to `build_chunk_detail_returns_callers_callees_tests`. Insert N chunks across M distinct origin files, upsert K function_calls and L type_edges, then assert `(total_chunks, total_files, call_edges, type_edges) == (N, M, K, L)`.

#### TC-HAP-V1.36-2: `Store::get_callers_with_context` has no direct unit test
- **Difficulty:** easy
- **Location:** src/store/calls/query.rs:150 — `get_callers_with_context` (callers: `impact::analysis::analyze_impact`, plus `_batch` variant in `impact::diff`)
- **Description:** The function joins call-edges with chunks/snippets to return `CallerInfo` with `call_line` and snippet context — load-bearing for `cqs impact`. Only indirectly tested via `tests/impact_test.rs::analyze_impact`. A regression in JOIN order, snippet truncation, or `call_line` extraction would reach `cqs impact` JSON output without anyone catching it. The simpler `get_callers_full` at line 14 has test coverage in `tests/store_calls_test.rs:215`; this richer variant doesn't.
- **Suggested fix:** Add to `tests/store_calls_test.rs`: insert two chunks A and B with calls A→B at known line numbers, call `store.get_callers_with_context("B")`, assert the returned `CallerInfo` has the expected `name`, `file`, `line`, `call_line`, and a non-empty `snippet`.

#### TC-HAP-V1.36-3: `get_callers_full_batch` and `get_callees_full_batch` untested
- **Difficulty:** easy
- **Location:** src/store/calls/query.rs:239 and :294 (callers: `cli::enrichment::enrich_chunks`, `cli::commands::io::context::build_full_data`, `tests/pipeline_eval.rs`)
- **Description:** Two `pub` batch variants on a hot path (page-render and context-pack stages). `tests/pipeline_eval.rs` calls them with `unwrap_or_default()`, so silent regressions like "returns empty map for one of N names" never trip a test. The non-batch `get_callers_full` has explicit tests in `tests/store_calls_test.rs`; the batch versions don't.
- **Suggested fix:** In `tests/store_calls_test.rs`, after the `store.upsert_function_calls` fixture, call both batch fns with a `Vec<&str>` of three names where one is unknown. Assert the unknown name maps to `Vec::new()` (not missing key) and the others have the expected callers/callees.

#### TC-HAP-V1.36-4: `cli::commands::io::context::build_compact_data` and `build_full_data` untested
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:25 (`build_compact_data`) and :130 (`build_full_data`) — both `pub(crate)`, exclusively used by `cmd_context`
- **Description:** `cmd_context` is exercised only at the JSON-shape level (`compact_to_json`, `full_to_json`, `summary_to_json` have `hp1_*` tests), but the data-fetching builders that populate those shapes from the store have zero direct tests. Both contain non-trivial logic: path normalization (PB-V1.29-1), `bail!` on empty origin, `get_caller_counts_batch`/`get_callee_counts_batch` reduction. A breakage in the normalization or the empty-path guard would only surface as a `cqs context` regression in production.
- **Suggested fix:** Add a `tc_hap_build_compact_data_*` test that opens an in-memory store, upserts two chunks under `src\\foo.rs` (Windows-style backslashes), calls `build_compact_data(&store, "src\\foo.rs")`, and asserts both chunks come back with non-zero caller/callee counts wired up. Mirror for `build_full_data`.

#### TC-HAP-V1.36-5: `apply_ci_token_budget` (`pub(crate)` entry point) has no direct test
- **Difficulty:** easy
- **Location:** src/cli/commands/review/ci.rs:64 — `apply_ci_token_budget` (caller: `cli::batch::handlers::analysis:206`)
- **Description:** `apply_ci_token_budget` is a `pub(crate)` shim over the local `apply_token_budget(_, _, json=true)`. The sibling `apply_token_budget_public` in `diff_review.rs:70` has tests at `:388` and `:407`, but the CI variant — used by the batch pipeline for `cqs ci --tokens N` — has none. The `json=true` branch enables `JSON_OVERHEAD_PER_RESULT` which inflates per-item token cost; a regression in that constant would silently fit fewer items into the budget for batch CI but not for review.
- **Suggested fix:** Add `tests` mod to `ci.rs` with `test_apply_ci_token_budget_truncates_callers_and_tests` and `test_apply_ci_token_budget_zero_returns_zero_items` mirroring the `diff_review.rs:test_apply_token_budget_*` shape, but pinning `json=true` accounting.

#### TC-HAP-V1.36-6: `HnswIndex::search` (unfiltered) has no direct unit test
- **Difficulty:** easy
- **Location:** src/hnsw/search.rs:23 — `HnswIndex::search` (callers: `Store::search_*` family)
- **Description:** `search_filtered` has TC-17 tests (referenced at `src/hnsw/build.rs:487`) and dim-mismatch tests in `tests/embedder_dim_mismatch_test.rs`. The unfiltered `search` entry point has no direct test — it's only reached through the higher-level `Store::search` which mixes RRF, BM25, and reranker concerns. The empty-query and dim-mismatch early-returns at lines 53 and 65, and the non-finite filter at line 82, deserve dedicated coverage at the HNSW layer rather than smuggled in through 6 wrapper layers.
- **Suggested fix:** Add to `src/hnsw/mod.rs` tests: `test_hnsw_search_empty_index_returns_empty`, `test_hnsw_search_dim_mismatch_returns_empty`, `test_hnsw_search_nonfinite_query_returns_empty`, `test_hnsw_search_returns_top_k_in_score_order`. Use `make_test_embedding` already imported at `:700`.

#### TC-HAP-V1.36-7: `cli::commands::io::context::pack_by_relevance` has no test
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:349 — `pack_by_relevance` (caller: `cmd_context` token-budgeting path)
- **Description:** Token-budget packing for the `cqs context --tokens N` flag. The companion `build_token_pack` private at `:448` is exercised through the `cmd_context` integration test, but `pack_by_relevance` — which applies the relevance-weighted ordering — has no direct test. Score ordering or saturation bugs in the relevance heuristic land in production output without being caught.
- **Suggested fix:** Add `pack_by_relevance_orders_by_score` test in `context.rs` `tests` mod. Build a `Vec<ChunkSummary>` with three chunks (high/mid/low scores), call `pack_by_relevance`, assert the high-score chunk is first and the low-score is last (or dropped if budget excludes it).

#### TC-HAP-V1.36-8: `cli::pipeline::embedding::prepare_for_embedding` has no test despite being a major orchestrator
- **Difficulty:** medium
- **Location:** src/cli/pipeline/embedding.rs:26 — `prepare_for_embedding` (callers: `gpu_embed_stage:247`, `cpu_embed_stage:461`)
- **Description:** 130-line function that does five logical steps (windowing, global cache, store cache, partition, NL description). The sibling `create_embedded_batch` at `:159` has four positive tests in `src/cli/pipeline/mod.rs:277-360`. `prepare_for_embedding` has zero direct tests — silent regressions in the cache hit/miss split or the windowing→hash chain only surface as eval-recall regressions. (Note: the pre-existing R@5 regression noted in MEMORY.md between 2026-04-25 and 2026-04-30 lives somewhere in this region of code.)
- **Suggested fix:** Add `test_prepare_for_embedding_separates_cached_and_uncached` and `test_prepare_for_embedding_uses_global_cache_when_available` next to the `create_embedded_batch` tests. Use a fake/in-memory `EmbeddingCache` with one pre-seeded `(content_hash, model_fp)` entry; assert that one chunk lands in `cached` and the other in `to_embed`.

#### TC-HAP-V1.36-9: Daemon GC entry points untested
- **Difficulty:** medium
- **Location:** src/cli/watch/gc.rs:114 (`run_daemon_startup_gc`) and :218 (`run_daemon_periodic_gc`)
- **Description:** Both functions are `pub(super)` and called from `cli/watch/mod.rs:1051` and `:1450`. The lower-level `prune_last_indexed_mtime` is well-tested in `cli/watch/tests.rs:688-803`. The two big GC drivers — which orchestrate `Pass 1: drop chunks for missing files` + `Pass 2: drop chunks for now-gitignored paths` and the periodic origin-cap walker (`DAEMON_PERIODIC_GC_CAP_DEFAULT=1000`) — have no direct test. Bugs in the cap honoring, the gitignore-matcher integration, or the `CQS_DAEMON_STARTUP_GC=0` opt-out only surface in production daemon logs.
- **Suggested fix:** Add to `cli/watch/tests.rs`: `test_run_daemon_startup_gc_prunes_missing_files` (insert 5 chunks for files A,B,C,D,E; delete A and B from disk; run startup GC; assert chunk count drops to 3) and `test_run_daemon_periodic_gc_honors_cap` (set `CQS_DAEMON_PERIODIC_GC_CAP=2`, verify only 2 origins per tick).

#### TC-HAP-V1.36-10: `train_data::cmd_train_data` and `cmd_plan` untested directly
- **Difficulty:** medium
- **Location:** src/cli/commands/train/train_data.rs:7 and src/cli/commands/train/plan.rs:7
- **Description:** `cli_train_review_test.rs` covers `cmd_plan` (P2 #46 (a)) but `cmd_train_data` has only an eval-fixture mention and no integration test. The function calls `cqs::train_data::generate_training_data` and prints a six-field summary; if the underlying generator changes its `TrainingDataStats` shape (rename `commits_skipped` → `commits_filtered`, etc.) the print format silently drifts from what the spec promises and from `cqs train-data --help`.
- **Suggested fix:** Add `tests/cli_train_data_test.rs` (subprocess pattern, like `cli_train_review_test.rs`): set up a tiny git repo with 2 commits, run `cqs train-data --output /tmp/x.jsonl`, assert exit code 0 and that stdout matches `Generated \d+ triplets from 1 repos \(\d+ commits processed, \d+ skipped\)`.

DONE
