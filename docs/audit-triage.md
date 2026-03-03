# Audit Triage — v0.19.4+

Triaged 2026-03-02. 75 findings across 14 categories, 3 batches.

## P1 — Easy + High Impact (fix immediately)

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| RB-1 | SQLite 999-param limit on `fetch_candidates_by_ids_async` / `fetch_chunks_by_ids_async` — PF-5 regression | Robustness | src/store/chunks.rs:1366, 1326 | ✅ fixed |
| AC-1 | `emit_empty_results` JSON injection — query string not escaped via raw `format!` | Algorithm | src/cli/commands/query.rs:29, similar.rs:99 | ✅ fixed |
| DS-2 | `acquire_index_lock` truncate(true) races with concurrent PID read — stale lock unrecoverable | Data Safety | src/cli/files.rs:57, 99 | ✅ fixed |
| DS-7 | `cmd_index --force` removes old DB before pipeline completes — interruption loses entire index | Data Safety | src/cli/commands/index.rs:69-76 | ✅ fixed |
| SEC-2 | FTS query safety depends on `debug_assert` — compiled out in release builds | Security | src/store/mod.rs:609, chunks.rs:1199 | ✅ fixed |
| CQ-1 | Dead `source/` module (~250 lines, zero callers) + stale CONTRIBUTING.md entry (DOC-1) | Code Quality | src/source/, lib.rs:81, CONTRIBUTING.md:109 | ✅ fixed |

## P2 — Medium Effort + High Impact (fix in batch)

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| TC-1 | 5 newest languages (Bash, HCL, Kotlin, Swift, ObjC) have zero parser integration tests | Test Coverage | tests/parser_test.rs, tests/fixtures/ | ✅ fixed |
| TC-2 | `NoteBoostIndex` has zero tests — search scoring hot path | Test Coverage | src/search.rs:300-371 | ✅ fixed |
| TC-3 | PF-5 `search_by_candidate_ids` language/chunk_type filter branches untested | Test Coverage | src/search.rs:883-905 | ✅ fixed (7 filter set unit tests) |
| AD-3 | Core store types (`ChunkSummary`, `SearchResult`, `CallerInfo`, etc.) lack `Serialize` — manual `to_json()` everywhere | API Design | src/store/helpers.rs:128-330 | ✅ fixed |

## P3 — Easy + Low Impact (fix if time)

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| OB-1 | `resolve_target` has no tracing span | Observability | src/search.rs:57 | ✅ fixed |
| OB-2 | `delete_by_origin` / `replace_file_chunks` no tracing spans | Observability | src/store/chunks.rs:194, 222 | ✅ fixed |
| OB-3 | `search_filtered` exits without logging result count | Observability | src/search.rs:793 | ✅ fixed |
| OB-4 | `Store::init` — no span on schema initialization | Observability | src/store/mod.rs:355 | ✅ fixed |
| OB-5 | `warn_stale_results` missing entry span | Observability | src/cli/staleness.rs:19 | ✅ fixed |
| OB-6 | `cmd_watch` no entry span | Observability | src/cli/watch.rs:54 | ✅ fixed |
| OB-7 | `get_caller_counts_batch` / `get_callee_counts_batch` no spans | Observability | src/store/calls.rs:1147, 1163 | ✅ fixed |
| AD-1 | `BatchCmd::Gather` direction is stringly-typed — should use `GatherDirection` enum | API Design | src/cli/batch/commands.rs:109 | ✅ fixed |
| AD-2 | `get_callers()` is dead public API — zero callers | API Design | src/store/calls.rs:252 | ✅ fixed (deleted, tests updated to get_callers_full) |
| AD-4 | `BlameEntry` / `BlameData` use manual JSON assembly (depends on AD-3) | API Design | src/cli/commands/blame.rs:18-155 | ✅ fixed (Serialize derive) |
| AD-6 | 9 CLI command handlers accept unused `_cli: &Cli` parameter | API Design | blame.rs, where_cmd.rs, test_map.rs, etc. | ✅ fixed (6 handlers) |
| EH-1 | `build_blame_data` discards `StoreError` chain via `.map_err` stringify | Error Handling | src/cli/commands/blame.rs:46 | ✅ fixed |
| EH-2 | 7 bare `Store::open()` calls without path context | Error Handling | diff.rs, drift.rs, index.rs, reference.rs, watch.rs | ✅ fixed |
| EH-3 | `SearchFilter::validate()` returns `&'static str` not proper error type | Error Handling | src/store/helpers.rs:513 | ✅ fixed (returns String with values) |
| EH-4 | `dispatch_onboard` swallows `get_chunks_by_names_batch` error silently | Error Handling | src/cli/batch/handlers.rs:912 | acceptable (has tracing::warn) |
| EH-5 | `GatherDirection::FromStr` uses `String` error type (moot if AD-1 fixed) | Error Handling | src/gather.rs:97 | moot (AD-1 fixed) |
| EH-6 | `schema_version` silently defaults to 0 on parse failure | Error Handling | src/store/chunks.rs:873 | ✅ fixed (tracing::warn) |
| DOC-2 | PRIVACY.md deletion instructions miss `config.toml` | Documentation | PRIVACY.md:46-51 | ✅ fixed |
| DOC-3 | SECURITY.md symlink mitigation description is inaccurate (understates scope) | Documentation | SECURITY.md:94 | ✅ fixed |
| DOC-4 | lib.rs doc comment omits Web Help format | Documentation | src/lib.rs:13 | ✅ fixed |
| DOC-5 | `cqs dead --min-confidence` undocumented in README and CLAUDE.md | Documentation | README.md:243, CLAUDE.md:71 | ✅ fixed |
| CQ-2 | Gather JSON assembly duplicated between CLI and batch handler | Code Quality | gather.rs:114, handlers.rs:406 | ✅ fixed (serde Serialize, removed to_json) |
| EX-1 | `pipeable_command_names()` manually duplicates pipeable variants — stale on new commands | Extensibility | src/cli/batch/pipeline.rs:31-113 | ✅ fixed (PIPEABLE_NAMES const + sync test) |
| EX-2 | `name_boost: 0.2` hardcoded — no shared constant with CLI default | Extensibility | src/cli/batch/handlers.rs:83 | ✅ fixed (DEFAULT_NAME_BOOST const) |
| EX-3 | `HNSW_EXTENSIONS` / `HNSW_ALL_EXTENSIONS` overlap with no sync enforcement | Extensibility | src/hnsw/persist.rs:31,34 | ✅ fixed |
| EX-5 | Note/code slot ratio `(limit * 3) / 5` inline formula repeated in tests | Extensibility | src/search.rs:1061, 1216, 1223 | ✅ fixed (min_code_slot_count fn) |
| RB-3 | `where_to_add` core panics via `.expect()` on `query_embedding` | Robustness | src/where_to_add.rs:170 | ✅ fixed (AnalysisError::Phase) |
| RB-4 | Blame passes inverted line ranges to git silently | Robustness | src/cli/commands/blame.rs:86 | ✅ fixed (swap + warn) |
| RB-5 | Reranker/embedder `outputs[0]` panics if ONNX returns empty | Robustness | src/reranker.rs:140 | informational (SessionOutputs API) |
| AC-3 | `token_pack` O(n²) `keep.iter().any()` in packing loop | Algorithm | src/cli/commands/mod.rs:134 | ✅ fixed (kept_any bool) |
| AC-4 | `cap_scores` uses `u64::MAX - x` inversion trick (correct but fragile) | Algorithm | src/onboard.rs:175 | ✅ fixed (std::cmp::Reverse) |
| TC-4 | `ChatHelper::complete` tab-completion logic untested | Test Coverage | src/cli/chat.rs:26-49 | ✅ fixed (4 tests) |
| TC-6 | Batch pipeline error propagation for malformed mid-pipeline input untested | Test Coverage | src/cli/batch/pipeline.rs | ✅ fixed (6 tests) |
| PB-2 | `notes_path` falls back to non-canonical path if file missing at watch start | Platform | src/cli/watch.rs:97-105 | acceptable (canonical after first event) |
| PB-4 | Lock file open code duplicated; NTFS ignores `0o600` silently | Platform | src/cli/files.rs:52-115 | acceptable (already extracted to open_lock_file) |
| PB-5 | `is_wsl()` has no `#[cfg(unix)]` guard | Platform | src/config.rs:15-25 | ✅ fixed |
| SEC-1 | `CQS_PDF_SCRIPT` env var allows arbitrary script execution (defense-in-depth) | Security | src/convert/pdf.rs:56-68 | acceptable (documented in SECURITY.md) |
| SEC-3 | `convert_directory` walk has no depth/file count limit | Security | src/convert/mod.rs:345 | ✅ fixed (max_depth 50) |
| SEC-4 | HNSW index files written with no permission restriction (inconsistent with DB) | Security | src/hnsw/persist.rs | acceptable (0o600 set in persist) |
| SEC-6 | `run_git_diff` can allocate unbounded memory — no size limit | Security | src/cli/commands/mod.rs:166-188 | ✅ fixed (50 MB limit) |
| DS-3 | `extract_relationships` not atomic with chunk upserts — crash leaves stale call graph | Data Safety | src/cli/commands/index.rs:120-133 | acceptable (reindex recovers) |
| DS-5 | `notes_summaries_cache` invalidation is caller-responsibility — fragile | Data Safety | src/store/mod.rs:746, notes.rs:122,224 | non-issue (all paths call invalidate) |
| DS-6 | `bytes_to_embedding` silently skips corrupted embeddings — no aggregate signal | Data Safety | src/store/helpers.rs:696-725 | ✅ fixed (warn level logging) |
| PF-1 | `search_by_candidate_ids` parses language/chunk_type strings per candidate (pre-build HashSet) | Performance | src/search.rs:884-905 | ✅ fixed |
| PF-2 | `search_filtered` clones all semantic IDs to separate Vec for `rrf_fuse` | Performance | src/search.rs:757 | ✅ fixed (Vec<&str>) |
| PF-3 | `search_by_name` re-lowercases query per result (use `score_name_match_pre_lower`) | Performance | src/store/mod.rs:646 | ✅ fixed |
| PF-4 | `score_confidence` clones all candidate IDs to separate Vec | Performance | src/store/calls.rs:881 | ✅ fixed (Vec<&str>) |
| PF-5 | `fetch_active_files` uses `IN (subquery)` where JOIN is more efficient | Performance | src/store/calls.rs:841-843 | ✅ fixed (INNER JOIN) |
| PF-6 | `build_batched` two-pass loop (validation then collection) | Performance | src/hnsw/build.rs:140-157 | ✅ fixed (single pass) |
| RM-1 | CHM/webhelp converters accumulate all pages then `join()` — 2× peak memory | Resource Mgmt | src/convert/chm.rs:119, webhelp.rs:93 | ✅ fixed (incremental String) |
| RM-2 | `html_file_to_markdown` / `markdown_passthrough` load entire file with no size guard | Resource Mgmt | src/convert/html.rs:32, mod.rs:113 | ✅ fixed (100 MB limit) |

## P4 — Hard or Low Impact (create issues)

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| AD-5 | `dispatch_search` takes 9 individual parameters instead of a struct | API Design | src/cli/batch/handlers.rs:33-42 | |
| CQ-3 | `token_pack_unified` / `token_pack_tagged` near-identical functions | Code Quality | src/cli/commands/query.rs:333-398 | |
| CQ-4 | `cmd_query` at 287 lines — multiple concerns in one function | Code Quality | src/cli/commands/query.rs:39-325 | |
| CQ-5 | 11 functions suppress `clippy::too_many_arguments` | Code Quality | multiple files | |
| EX-4 | `extract_from_scout_groups` bespoke extractor — new output shapes need new extractors | Extensibility | src/cli/batch/pipeline.rs:116-210 | |
| RB-2 | `CandidateRow::from_row` / `ChunkRow::from_row` use panicking `row.get()` | Robustness | src/store/helpers.rs:75-121 | informational |
| RB-6 | `Language::def()` panics on disabled feature flags | Robustness | src/language/mod.rs:549, 570 | informational |
| RB-7 | `Parser::new()` panics on registry/enum mismatch | Robustness | src/parser/mod.rs:62-67 | informational |
| AC-2 | FTS not scoped to HNSW candidates — design tension, improves recall | Algorithm | src/search.rs:934-942 | by-design |
| AC-5 | `bfs_shortest_path` uses empty-string sentinel for predecessor tracking | Algorithm | src/cli/commands/trace.rs:203-221 | informational |
| TC-5 | `build_blame_data` only tested through components — no end-to-end with mock git | Test Coverage | src/cli/commands/blame.rs:36-68 | |
| PB-1 | `cmd_watch` uses inotify on WSL; PollWatcher would be more reliable on `/mnt/` | Platform | src/cli/watch.rs:61-89 | existing behavior |
| PB-3 | Forward-slash path in blame `-L` spec latently incompatible with native Windows git | Platform | src/cli/commands/blame.rs:50, 86 | latent |
| SEC-5 | `find_7z` / `find_python` search PATH without validation | Security | src/convert/chm.rs:168, pdf.rs:95 | |
| DS-1 | HNSW save partial rename leaves inconsistent index on mid-loop failure | Data Safety | src/hnsw/persist.rs:241-272 | |
| DS-4 | `prune_stale_calls` executes outside GC's index lock scope after chunk pruning | Data Safety | src/cli/commands/gc.rs:44-59 | |
| RM-3 | `BatchContext::refs` loaded references accumulate with no eviction | Resource Mgmt | src/cli/batch/mod.rs:60 | acceptable |
| RM-4 | Pipeline channel depth same for parse (light) and embed (heavy) payloads | Resource Mgmt | src/cli/pipeline.rs:37 | |
| RM-5 | Watch `last_indexed_mtime` grows unbounded; `retain` runs O(files) `exists()` calls | Resource Mgmt | src/cli/watch.rs:117, 307-311 | |

## Summary

| Priority | Count | Action |
|----------|-------|--------|
| P1 | 6 | Fix immediately |
| P2 | 4 | Fix in batch |
| P3 | 50 | Fix if time |
| P4 | 19 | Create issues / informational |
| **Total** | **75** | |
