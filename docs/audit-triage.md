# Audit Triage — v0.19.2

Generated: 2026-02-27
Total findings: 117 across 14 categories (3 batches)
Informational/well-designed: 3 (RM-7, RM-9, RM-10) — no action needed

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | **AC-5**: Stale `partial_cmp` in drift test — missed by v0.19.1 sweep | Algorithm | drift.rs:167 | ✅ PR #501 |
| 2 | **DOC-1**: lib.rs quick start example uses invalid SearchFilter (fails at runtime) | Documentation | src/lib.rs:35 | ✅ PR #501 |
| 3 | **DOC-2**: CONTRIBUTING lists C++/Kotlin/Swift as future work — already implemented | Documentation | CONTRIBUTING.md:70 | ✅ PR #501 |
| 4 | **DOC-3**: CONTRIBUTING lists phantom `src/deps.rs` — file doesn't exist | Documentation | CONTRIBUTING.md:172 | ✅ PR #501 |
| 5 | **DOC-4**: README says `cqs project add` — actual command is `register` | Documentation | README.md:402 | ✅ PR #501 |
| 6 | **DOC-5**: README HNSW ef_search listed as fixed 100 — adaptive since v0.19.2 | Documentation | README.md:463 | ✅ PR #501 |
| 7 | **DOC-6/SEC-3**: `open_readonly` skips `PRAGMA quick_check` — SECURITY.md claims every open | Security/Doc | store/mod.rs:279, SECURITY.md:20 | ✅ PR #501 |
| 8 | **DOC-7**: CONTRIBUTING lists `source/` as active — it's dead code | Documentation | CONTRIBUTING.md:108 | ✅ PR #501 |
| 9 | **RB-1/EH-1**: `serde_json::to_string().unwrap()` in batch REPL — 6 NaN panic sites | Robustness | batch/mod.rs:288+ | ✅ PR #501 |
| 10 | **RB-5**: `ChunkOutput` serialization `.expect()` panics on NaN scores | Robustness | batch/handlers.rs:34,152 | ✅ PR #501 |
| 11 | **RB-7**: `diff_parse.rs` unwrap after starts_with on external input | Robustness | diff_parse.rs:50 | ✅ PR #501 |
| 12 | **RB-8**: `onboard.rs` NaN score-to-u64 cast produces garbage ordering | Robustness | onboard.rs:182 | ✅ PR #501 |
| 13 | **SEC-1**: SQLite URL constructed from unescaped path — `?`/`#` injection | Security | store/mod.rs:180,286 | ✅ PR #501 |
| 14 | **OB-2**: `parse_notes()` errors swallowed in read command (2 sites) | Observability | read.rs:280,312 | ✅ PR #501 |
| 15 | **OB-3**: `search_by_names_batch()` error swallowed in read --focus | Observability | read.rs:223 | ✅ PR #501 |
| 16 | **EH-3**: gc.rs HNSW file deletion silently ignores failures | Error Handling | gc.rs:64-67 | ✅ PR #501 |
| 17 | **EH-4**: `cmd_ref_add` bare `?` on Store::open — no path context | Error Handling | reference.rs:104-105 | ✅ PR #501 |
| 18 | **EH-5**: `cmd_diff` bare `?` on Store::open for 3 stores — no context | Error Handling | diff.rs:46,55,75 | ✅ PR #501 |
| 19 | **PB-1**: `path_matches_mention` no backslash normalization — notes lose boost | Platform | note.rs:311 | ✅ PR #501 |
| 20 | **PB-2**: `find_dead_code` inline path filter diverges from `is_test_chunk` | Platform | calls.rs:814-819 | ✅ PR #501 |
| 21 | **PB-7**: `find_stale_mentions` backslash paths cause false-positive staleness | Platform | suggest.rs:192 | ✅ PR #501 |
| 22 | **AD-1**: `GatherDirection` raw string instead of clap `ValueEnum` | API Design | cli/mod.rs:506 | ✅ PR #501 |
| 23 | **AD-2**: `audit-mode` state is `Option<String>` not enum | API Design | cli/mod.rs:541 | ✅ PR #501 |
| 24 | **AD-9**: `SearchFilter::new()` duplicates `Default::default()` | API Design | helpers.rs:406 | ✅ PR #501 |
| 25 | **CQ-3**: HNSW extension list duplicated with mismatch (3 vs 4 files) | Code Quality | persist.rs:15, watch.rs:217 | ✅ PR #501 |
| 26 | **CQ-5**: Reference lookup boilerplate duplicated 6 times | Code Quality | 6 sites | ✅ PR #501 |
| 27 | **CQ-7**: `GatheredChunk` 11-field construction repeated 4 times — no From impl | Code Quality | gather.rs:244+ | ✅ PR #501 |
| 28 | **AC-3**: `onboard` uses embedding-only search — missing RRF | Algorithm | onboard.rs:114 | ✅ PR #501 |
| 29 | **AC-4**: Parent dedup reduces results below limit after RRF | Algorithm | search.rs:598 | ✅ PR #501 |
| 30 | **PF-3**: `score_name_match` redundant `to_lowercase()` in inner loop | Performance | helpers.rs:594 | ✅ PR #501 |
| 31 | **PF-8**: `NOT IN (subquery)` anti-pattern in dead code Phase 1 | Performance | calls.rs:714 | ✅ PR #501 |
| 32 | **EX-4**: `callable_sql_list()` duplicates `is_callable()` — manual sync | Extensibility | language/mod.rs:244 | ✅ PR #501 |
| 33 | **EX-7**: NL `type_word` duplicates ChunkType::Display | Extensibility | nl.rs:338 | ✅ PR #501 |
| 34 | **SEC-4**: `convert_directory` walk doesn't filter symlinks | Security | convert/mod.rs:345 | ✅ PR #501 |
| 35 | **SEC-6**: `CQS_PDF_SCRIPT` override only warns at tracing level | Security | convert/pdf.rs:57 | ✅ PR #501 |
| 36 | **PB-5**: `is_wsl()` private to config.rs — not reusable | Platform | config.rs:14 | ✅ PR #501 |
| 37 | **PB-6**: Watch mode no WSL inotify unreliability warning | Platform | watch.rs:69 | ✅ PR #501 |
| 38 | **OB-4**: `get_call_graph()` silent truncation at 500K edges | Observability | calls.rs:469 | ✅ PR #501 |
| 39 | **OB-9**: `find_dead_code()` no span or result count logging | Observability | calls.rs:707 | ✅ PR #501 |
| 40 | **RB-6**: `Parser::new()` expect doesn't identify offending language | Robustness | parser/mod.rs:62 | ✅ PR #501 |
| 41 | **TC-5**: `rel_display` pure utility — zero tests | Test Coverage | lib.rs:223 | ✅ PR #501 |
| 42 | **DS-4**: GC prune operations not atomic — crash leaves orphans | Data Safety | gc.rs:41-56 | ✅ PR #501 |
| 43 | **RM-5**: `merge_results()` hashes all before truncating | Resource Mgmt | reference.rs:186 | ✅ PR #501 |
| 44 | **PF-2**: Pipeline `needs_reindex` per-chunk not per-file | Performance | pipeline.rs:362 | ✅ PR #501 |
| 45 | **PF-4**: `note_boost` O(notes×mentions) per chunk in inner loop | Performance | search.rs:266 | ✅ PR #501 |
| 46 | **PF-6**: `analyze_impact` loads test chunks redundantly | Performance | analysis.rs:27 | ✅ PR #501 |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | **DS-1**: Watch mode never acquired index lock (claimed fixed, wasn't) | Data Safety | watch.rs | ✅ PR #502 |
| 2 | **DS-2**: Watch mode chunks+calls not atomic (claimed fixed, wasn't) | Data Safety | watch.rs:394-427 | ✅ PR #502 |
| 3 | **AC-1**: HNSW path skips RRF fusion — search differs by index presence | Algorithm | search.rs:664 | ✅ PR #502 |
| 4 | **AC-2**: BFS depth overwrite — deeper depth replaces shallower | Algorithm | gather.rs:202 | ✅ PR #502 |
| 5 | **AD-6**: ScoutResult/TaskResult/GatherResult etc missing Serialize | API Design | multiple | ✅ PR #502 |
| 6 | **AD-8**: cmd_diff/cmd_drift duplicate reference resolution ~30 lines | API Design | diff.rs, drift.rs | ✅ PR #502 |
| 7 | **AD-10**: `StoreError::Runtime` catch-all masks unrelated errors | API Design | helpers.rs:34 | ✅ PR #502 |
| 8 | **EH-6**: `AnalysisError::Embedder(String)` discards typed error chain | Error Handling | lib.rs:148 | ✅ PR #502 |
| 9 | **OB-1**: Store module zero tracing spans on 8+ critical operations | Observability | store/*.rs | ✅ PR #502 |
| 10 | **DS-3**: OnceLock notes cache never invalidated in long-lived Store | Data Safety | store/mod.rs:170 | ✅ PR #502 |
| 11 | **DS-5**: HNSW copy fallback not atomic — crash loses index | Data Safety | persist.rs:225 | ✅ PR #502 |
| 12 | **DS-6**: `prune_missing` per-batch transactions — partial prune on crash | Data Safety | chunks.rs:474 | ✅ PR #502 |
| 13 | **PB-3**: 30+ sites manual `.replace('\\', "/")` — no centralized function | Platform | 15+ files | ✅ PR #509 |
| 14 | **PF-1**: N+1 SELECT for content hash snapshotting in upsert | Performance | chunks.rs:64 | ✅ PR #502 |
| 15 | **PF-5**: HNSW search loads full content for all 500 candidates before scoring | Performance | chunks.rs:1235, search.rs:694 | deferred |
| 16 | **PF-7**: `get_call_graph` called 15 times with no caching | Performance | calls.rs:469 | ✅ PR #502 |
| 17 | **PF-10**: `find_test_chunks` LIKE content scan — 50MB, called 13 times | Performance | calls.rs:946 | ✅ PR #502 |
| 18 | **EX-1**: ChunkType Display/FromStr still manual — macro never created | Extensibility | language/mod.rs:268 | ✅ PR #502 |
| 19 | **EX-2**: Structural patterns no language-specific dispatch hooks | Extensibility | structural.rs:64 | ✅ PR #502 |
| 20 | **EX-8**: Test heuristics hardcoded, not connected to language system | Extensibility | calls.rs:83 | ✅ PR #502 |
| 21 | **CQ-1**: cmd_query repeats boilerplate across 5 code paths | Code Quality | query.rs:16 | ✅ PR #502 |
| 22 | **CQ-4**: cmd_watch 9 indent levels, duplicated embedder init | Code Quality | watch.rs:39 | ✅ PR #502 |
| 23 | **CQ-6**: find_dead_code 233 lines with inline struct, 3 phases | Code Quality | calls.rs:707 | ✅ PR #502 |
| 24 | **TC-1**: `search_across_projects` zero tests | Test Coverage | project.rs:155 | ✅ PR #502 |
| 25 | **TC-2**: Schema migration never executed in tests | Test Coverage | migrations.rs:29 | ✅ PR #502 |
| 26 | **TC-7**: HNSW search path no RRF behavior test | Test Coverage | search.rs:664 | ✅ PR #502 |
| 27 | **TC-10**: `index_notes` zero tests | Test Coverage | lib.rs:247 | ✅ PR #502 |
| 28 | **RM-2**: gather/scout load full call graph per CLI call — no _with_resources | Resource Mgmt | gather.rs:325 | ✅ PR #502 |
| 29 | **RM-6**: BatchContext no idle timeout for embedder/reranker | Resource Mgmt | batch/mod.rs:46 | ✅ PR #502 |
| 30 | **PB-4**: HNSW advisory locking silently ineffective on WSL — no warning | Platform | persist.rs:119 | ✅ PR #502 |
| 31 | **DS-7**: `rewrite_notes_file` lock on read-only fd | Data Safety | note.rs:185 | ✅ PR #502 |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | **AD-3**: Inconsistent positional arg naming (task/query/concept) | API Design | cli/mod.rs | ✅ PR #504 |
| 2 | **AD-4**: 4 commands have both --format and --json flags | API Design | cli/mod.rs | ✅ PR #504 |
| 3 | **AD-5**: 7 handlers accept `_cli: &Cli` but never use it | API Design | multiple | ✅ PR #504 |
| 4 | **AD-7**: `suggest_placement` 4 API variants | API Design | where_to_add.rs:101 | ✅ PR #504 |
| 5 | **RB-2/EH-2**: BatchContext OnceLock unwrap — should use get_or_init | Robustness | batch/mod.rs | ✅ PR #504 |
| 6 | **RB-3**: search_filtered unwrap with non-local invariant | Robustness | search.rs:523,583 | ✅ PR #504 |
| 7 | **RB-4**: Embedder/Reranker session guard unwrap | Robustness | embedder.rs:523 | ✅ PR #504 |
| 8 | **RB-9**: convert FORMAT_TABLE expect — runtime panic for compile-time invariant | Robustness | convert/mod.rs:191 | ✅ PR #504 |
| 9 | **RB-10/EH-8**: onboard.rs unwrap with non-local early-return invariant | Robustness | onboard.rs:128 | ✅ PR #504 |
| 10 | **EH-7**: impact swallows search_by_names_batch errors — no degraded flag | Error Handling | analysis.rs:72 | ✅ PR #504 |
| 11 | **OB-5**: Store::open/open_readonly lack timing span | Observability | store/mod.rs:175 | ✅ PR #504 |
| 12 | **OB-6**: search_across_projects missing entry span | Observability | project.rs:155 | ✅ PR #504 |
| 13 | **OB-7**: gather() doesn't log BFS expansion stats | Observability | gather.rs | ✅ PR #504 |
| 14 | **OB-8**: HNSW build_batched no per-batch progress logging | Observability | hnsw/build.rs | ✅ PR #504 |
| 15 | **SEC-2**: Temp file PID+nanos — low entropy, use RandomState | Security | 5 sites | ✅ PR #504 |
| 16 | **SEC-5**: FTS query safety depends on undocumented sanitization ordering | Security | store/mod.rs:598 | ✅ PR #504 |
| 17 | **EX-3**: ENTRY_POINT_NAMES/TRAIT_METHOD_NAMES not connected to languages | Extensibility | calls.rs:44 | ✅ PR #504 |
| 18 | **EX-5**: PIPEABLE_COMMANDS manually maintained | Extensibility | pipeline.rs:15 | ✅ PR #504 |
| 19 | **EX-6**: NAME_ARRAY_FIELDS manually maintained | Extensibility | pipeline.rs:84 | ✅ PR #504 |
| 20 | **PF-9**: search_filtered rebuilds identical SQL per cursor batch | Performance | search.rs:489 | ✅ PR #504 |
| 21 | **TC-3**: check_origins_stale batch boundary untested | Test Coverage | chunks.rs:638 | ✅ PR #504 |
| 22 | **TC-4**: resolve_index_dir migration zero tests | Test Coverage | lib.rs:168 | ✅ PR #504 |
| 23 | **TC-6**: suggest_placement only trivial empty test | Test Coverage | where_to_add.rs:101 | ✅ PR #504 |
| 24 | **TC-8**: review_diff note matching never tested with actual notes | Test Coverage | review_test.rs | ✅ PR #504 |
| 25 | **TC-9**: enumerate_files zero tests | Test Coverage | lib.rs:312 | ✅ PR #504 |
| 26 | **PB-8**: ChunkSummary.file PathBuf semantics mismatch — needs doc comment | Platform | helpers.rs:99 | ✅ PR #504 |
| 27 | **RM-1**: count_vectors reads entire HNSW id map as string | Resource Mgmt | persist.rs:389 | ✅ PR #504 |
| 28 | **RM-3**: find_dead_code loads full ChunkSummary just for names | Resource Mgmt | calls.rs:768 | ✅ PR #504 |
| 29 | **RM-8**: Verify cmd_index routes to build_batched for >50K | Resource Mgmt | hnsw/build.rs | ✅ PR #504 |

## P4: Hard or Low Impact — Defer/Create Issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | **CQ-2**: `run_index_pipeline` 458 lines — 6 concerns, 5 threads | Code Quality | pipeline.rs:238 | ✅ PR #506 |
| 2 | **CQ-8**: `search_filtered` 219 lines mixing SQL, scoring, RRF, fetch | Code Quality | search.rs:414 | ✅ PR #506 |
| 3 | **RM-4**: Store mmap 256MB×4 = 1GB virtual — benign, needs doc | Resource Mgmt | store/mod.rs:214 | ✅ PR #506 |

## Cross-References

- RB-1 ≈ EH-1 (same finding: batch REPL NaN panic)
- RB-2 ≈ EH-2 (same finding: BatchContext OnceLock unwrap)
- RB-10 ≈ EH-8 (same finding: onboard unwrap)
- DOC-6 ≈ SEC-3 (same finding: open_readonly skips quick_check)
- DS-1/DS-2 are regressions — v0.19.0 triage marked fixed but code was never applied
- TC-7 would catch AC-1 (HNSW RRF skip)
- PB-3 subsumes PB-1, PB-7 (centralized normalize_path fixes both)
