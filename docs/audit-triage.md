# Audit Triage — v1.19.0

54 findings across 8 categories. Triaged by impact × effort.

## P1 — Easy + High Impact (fix immediately)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| SEC-10 | Security | evict() negative size wraps u64, deletes entire cache | ✅ #841 |
| SEC-7 | Security | Cache opens SQLite via unencoded path URL | ✅ #840 |
| SEC-8 | Security | Cache DB created world-readable | ✅ #840 |
| SEC-9 | Security | query_log.jsonl created world-readable | ✅ #841 |
| RB-11 | Robustness | format_timestamp panics on negative created_at | ✅ #841 |
| RB-12 | Robustness | splade_alpha NaN/OOR silently corrupts hybrid scores | ✅ #841 |
| DS-45 | Data Safety | INSERT OR IGNORE retains stale entry on fingerprint fallback | ✅ #842 |
| EH-16 | Error Handling | prune_orphan_sparse_vectors defined but never called | ✅ #842 |

## P2 — Medium + High Impact (fix in batch)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| CQ-5 | Code Quality | Batch search missing --include-type/--exclude-type | ✅ #842 |
| CQ-7 | Code Quality | resolve_parent_context dedup uses child ID, never fires | ✅ #842 |
| PF-5 | Performance | SPLADE single-threaded, encode_batch never used | issue #843 |
| PF-12 | Performance | chunk_type_language_map scans all chunks per search | ✅ #842 |
| PF-14 | Performance | SPLADE encode copies full logits tensor (15.6MB/call) | ✅ #840 |
| DS-47 | Data Safety | Cache pool missing busy_timeout | ✅ #840 |
| DS-49 | Data Safety | evict() measures physical pages, not logical data | ✅ #842 |
| DS-50 | Data Safety | Concurrent block_on serializes GPU/CPU cache access | ✅ #840 |
| RB-10 | Robustness | SPLADE session panics on poisoned Mutex | ✅ #840 |
| RB-13 | Robustness | SPLADE encode no input length cap | ✅ #840 |

## P3 — Easy + Low Impact (fix if time)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| OB-1 | Observability | stats() swallows 5 SQLite failures | ✅ #841 |
| OB-2 | Observability | evict() PRAGMA failure silent | ✅ #841 |
| OB-3 | Observability | read_batch blob mismatch silent | ✅ #841 |
| OB-4 | Observability | log_query open failure silent | ✅ #841 |
| OB-5 | Observability | dispatch() missing tracing span | ✅ #841 |
| OB-6 | Observability | cache subcommand helpers missing spans | ✅ #841 |
| EH-13 | Error Handling | SPLADE encode error silent in batch | ✅ #841 |
| EH-14 | Error Handling | ensure_splade_index silently ignores DB error | ✅ #841 |
| EH-15 | Error Handling | get_chunk_with_embedding errors silent in neighbors | ✅ #841 |
| PF-6 | Performance | write_batch allocates per-embedding Vec<u8> | ✅ #841 |
| PF-7 | Performance | read_batch rebuilds SQL placeholder per sub-batch | ✅ #841 |
| PF-8 | Performance | prepare_for_embedding collects hash list twice | ✅ #841 |
| PF-9 | Performance | GPU embed 3 passes for one debug log | ✅ #841 |
| PF-10 | Performance | as_slice().to_vec() clones embeddings for cache write | ✅ #841 |
| PF-11 | Performance | upsert_sparse_vectors one DELETE per chunk | ✅ #841 |
| PF-13 | Performance | SpladeIndex bounds-checked get in hot loop | ✅ #841 |
| CQ-1/DS-48/TC-19 | Multiple | VerifyReport dead type (3 findings, 1 fix) | ✅ #840 |
| CQ-2 | Code Quality | SearchFilter::new() dead method | ✅ #841 |
| CQ-3 | Code Quality | test_eviction duplicates open internals | ✅ #841 |
| CQ-4 | Code Quality | cache_path_display recomputes default_path | ✅ #841 |
| CQ-6 | Code Quality | Double rerank guard unreachable | ✅ #841 |
| CQ-8 | Code Quality | search_hybrid bare unwrap | ✅ #841 |
| DS-44 | Data Safety | write_batch no dim validation | ✅ #841 |
| DS-46 | Data Safety | Negative dim wraps to usize | ✅ #841 |
| RB-14 | Robustness | token_id negative wraps to u32 | ✅ #841 |

## P4 — Test Coverage (add tests)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| TC-17 | Test Coverage | HnswIndex::search_filtered zero unit tests | ✅ #841 |
| TC-18 | Test Coverage | find_rank matches name only, file unused | ✅ #841 |
| TC-20 | Test Coverage | read_batch >100 hashes path untested | ✅ #841 |
| TC-21 | Test Coverage | NaN/Inf embeddings untested | ✅ #841 |
| TC-22 | Test Coverage | log_query zero tests | ✅ #841 |
| TC-23 | Test Coverage | New chunk types missing from filter tests | ✅ #841 |
| TC-24 | Test Coverage | prune edge cases untested | ✅ #841 |
| TC-25 | Test Coverage | Eval harness helpers untested | ✅ #841 |
| TC-26 | Test Coverage | write_batch duplicate hash untested | ✅ #841 |

## Summary

- **P1**: 8 findings (all easy, security + robustness + data safety)
- **P2**: 10 findings (medium effort, performance + data safety + robustness)
- **P3**: 25 findings (easy, observability + error handling + performance + code quality)
- **P4**: 9 findings (test coverage gaps)
- **Total**: 52 unique findings (3 duplicates merged: CQ-1 = DS-48 = TC-19)

## Batch 2 — 7 remaining categories (2026-04-08)

19 findings across Documentation, API Design, Scaling, Algorithm Correctness, Extensibility, Platform Behavior, Resource Management.

### P1

| ID | Category | Title | Status |
|----|----------|-------|--------|
| AC-2 | Algorithm | Watch mode absolute-path chunk IDs | ✅ #842 |
| EXT-1 | Extensibility | New ChunkType silently excluded from search | ✅ #842 |

### P2

| ID | Category | Title | Status |
|----|----------|-------|--------|
| AC-1 | Algorithm | evict() minimum 100 deletions over-evicts | ✅ #842 |
| AC-3 | Algorithm | bootstrap_ci weak PRNG seed | ✅ #842 |
| PB-1 | Platform | open_light() 4 connections on 1-thread runtime | ✅ #842 |
| RM-2 | Resource | Cache pool max_connections(2) but only 1 used | ✅ #842 |

### P3

| ID | Category | Title | Status |
|----|----------|-------|--------|
| DOC-27 | Documentation | Language count 53→54 | ✅ #842 |
| DOC-28 | Documentation | Chunk type count 27→25 | ✅ #842 |
| DOC-29 | Documentation | README eval baselines stale | ✅ #842 |
| DOC-30 | Documentation | with_query() stale doc comment | ✅ #842 |
| DOC-31 | Documentation | lib.rs batch size comment wrong | ✅ #842 |
| PB-2 | Platform | idle_timeout WAL lock retention | ✅ #842 |
| SHL-22 | Scaling | SPLADE threshold hardcoded | ✅ #842 |
| SHL-24 | Scaling | Env vars undocumented in README | ✅ #842 |

### P4 (deferred)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| AD-19 | API Design | chunk_types/exclude_types asymmetric naming | issue #844 |
| EXT-2 | Extensibility | CLI/batch dual registration, no sync test | design issue |
| RM-1 | Resource | Two tokio runtimes during index | low priority |
| SHL-23 | Scaling | Channel depths not configurable | low priority |

### Batch 2 Summary

- **P1**: 2 fixed
- **P2**: 4 fixed
- **P3**: 8 fixed
- **P4**: 4 deferred (1 tracked issue, 3 low priority)
- **Total**: 14 fixed, 4 deferred, 1 tracked issue
