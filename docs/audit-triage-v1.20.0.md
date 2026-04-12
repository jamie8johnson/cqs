# Audit Triage — v1.20.0

~80 findings across 14 categories (both batches). 2026-04-08.

## P1: Easy + High Impact (fix immediately)

| ID | Finding | Status |
|----|---------|--------|
| RB-15 | `from_config` panics instead of returning Err (= CQ-4, EH-2) | fixing |
| RB-17 | 8 post_process functions panic on multi-byte UTF-8 at byte boundary | fixing |
| RB-18 | `find_insertion_point` OOB panic on stale index data | fixing |
| DS-2 | INSERT OR REPLACE wipes enrichment_hash on every re-index | fixing |
| DS-3 | GC ignores set_hnsw_dirty failure, crash leaves stale HNSW | fixing |
| DS-4 | Migration hardcodes 768 dims, corrupts BGE-large installs | fixing |
| EH-1 | Cache evict avg-entry query failure silently swallowed | fixing |
| EH-3 | get_all_summaries_full failure silently discards all LLM summaries | fixing |
| EH-4 | chunk_type_language_map silently drops chunks with unrecognized type | fixing |
| CQ-9 | mem::forget(dir) leaks temp directories in tests | fixing |
| RB-16 | reranker::score_passages panics on zero ONNX outputs | fixing |
| AC-4 | paired_bootstrap p-value exceeds 1.0 | fixing |
| AC-8 | SPLADE hybrid sort uses partial_cmp instead of total_cmp | fixing |

## P2: Medium Effort + High Impact (fix in batch)

| ID | Finding | Status |
|----|---------|--------|
| CQ-1 | --cross-project silently falls back on 4 commands (= CQ-2) | fixing |
| CQ-3 | analyze_impact_cross returns empty file/line for all callers | fixing |
| DS-1 | Orphan sparse_vectors after prune_missing | fixing |
| DS-6 | prune_all omits sparse_vectors — split atomicity | fixing |
| SEC-1 | Temp file world-readable before set_permissions | fixing |
| SEC-2 | fs::copy fallback creates world-readable temp | fixing |
| PF-3 | compute_risk_and_tests N separate reverse_bfs calls | defer |
| PF-9 | suggest_tests reverse_bfs per direct caller | defer |
| PF-4 | find_contrastive_neighbors clones Vec 12K times | defer |
| PF-6 | search_by_names_batch full deserialization before name match | defer |
| AD-3 | CrossProjectCallee.line vs line_start JSON inconsistency | defer |
| AD-8 | CallerWithContext.line missing serde rename | defer |
| PB-1 | prune_all missing absolute/relative path suffix fallback | defer |

## P3: Easy + Low Impact (fix if time)

| ID | Finding | Status |
|----|---------|--------|
| CQ-5 | _local parameter ignored, duplicate store opened (= PF-5) | fixing |
| CQ-6 | include_types silently ignored in analyze_impact_cross | fixing |
| CQ-8 | Duplicate make_named_store test helpers | fixing |
| PF-8 | Inline placeholder duplicates make_placeholders | fixing |
| PF-10 | GatherOptions::default reads env var every time | fixing |
| SEC-3 | llm_api_base logged verbatim at debug level | fixing |
| SHL-26 | llm_max_tokens capped at 4096 below model limits | fixing |
| SHL-27 | ENRICH_EMBED_BATCH ignores CQS_EMBED_BATCH_SIZE | fixing |
| SHL-28 | MAX_REFERENCES=20 no env override | fixing |
| OB-7 | search_hybrid silently falls back when SPLADE unavailable | fixing |
| OB-8 | collect_events in watch zero tracing | fixing |
| OB-9 | pending_files overflow silently drops events | fixing |
| OB-10 | search_single_project has no span | fixing |
| OB-12 | load_single_reference has no span | fixing |
| AC-5 | bootstrap_ci lower bound wrong index | fixing |
| AC-6 | bfs_expand false expansion_capped | fixing |
| RM-4 | Cache uses wrong tokio runtime | fixing |
| RM-5 | Cache pool missing idle_timeout | fixing |
| PB-2 | list_stale_files missing macOS case-fold | defer |
| PB-3 | WSL watch auto-poll hardcodes /mnt/ | defer |
| PB-4 | atomic_write non-atomic fallback on Windows | defer |

## P4: Hard or Low Impact (create issues / defer)

| ID | Finding | Status |
|----|---------|--------|
| CQ-7 | ScoringConfig::with_overrides dead code | defer |
| DS-5 | DEFERRED transactions → SQLITE_BUSY (recurring) | issue |
| SEC-4 | Reference path accepts any filesystem path | issue |
| SEC-5 | FTS5 operator injection (low severity) | defer |
| EXT-3 | human_name() no compile-time guard | defer |
| EXT-4 | Language count hardcoded in 5+ docs | defer |
| EXT-5 | rrf_k not in ScoringOverrides | defer |
| SHL-25 | 25 env vars undocumented in README | issue |
| SHL-29 | Pipeline channel depths not configurable | defer |
| SHL-30 | HNSW ID map limit not env-readable | defer |
| PF-1 | get_neighbors Vec<String> in BFS | defer |
| PF-2 | bfs_expand double-clones seeds | defer |
| PF-7 | cached_notes_summaries Mutex on warm read | defer |
| RM-3 | SpladeEncoder can't be freed during idle | defer |
| RM-6 | load_all_sparse_vectors 3x peak memory | defer |
| AC-7 | VectorIndex default 3x over-fetch under-returns | defer |
| AC-9 | test_reachability equivalence class over-counts | defer |
| PB-5 | libc::atexit allocates via Mutex (UB) | issue |
| AD-1 | Store::open_light misleading name | defer |
| AD-2 | --include-type vs --include-types collision | issue |
| AD-4 | Batch mode missing 8 CLI flags | issue |
| AD-5 | SearchFilter.chunk_types naming (= #844) | existing #844 |
| AD-6 | storedproc/configkey/typealias squashed names | defer |
| AD-7 | Write commands bypass CommandContext | defer |
| DOC-32 | CHANGELOG empty for shipped PRs | fixing |
| DOC-33 | ROADMAP unchecked items | fixing |
| DOC-34 | CONTRIBUTING.md missing cross_project.rs | fixing |
| DOC-35 | README shows trace --cross-project as working | fixing |
| DOC-36 | 8 post_process functions undocumented | fixing |
| DOC-37 | No "Adding a Chunk Type" section in CONTRIBUTING | defer |
| DOC-38 | callees --cross-project not shown in README | fixing |
| DOC-39 | README "How It Works" lists only 7 types | fixing |
| OB-11 | watch reindex_files no cache hit/miss log | defer |
| TC-27–TC-36 | 10 test coverage gaps | batch later |

## Notes

- Duplicates collapsed: CQ-4=RB-15=EH-2, CQ-2⊂CQ-1, CQ-5=PF-5
- DS-1+DS-6 fixed together (sparse_vectors in prune transaction)
- "fixing" = dispatched to fix agents
- "defer" = low impact, tracked for future
- "issue" = create GitHub issue
- PF-3/4/6/9 deferred — performance optimizations need benchmarking first
- PB-2/3/4 deferred — platform-specific, need platform testing
- TC items batched separately as test-only commit
