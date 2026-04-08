# Audit Triage — v1.19.0

54 findings across 8 categories. Triaged by impact × effort.

## P1 — Easy + High Impact (fix immediately)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| SEC-10 | Security | evict() negative size wraps u64, deletes entire cache | |
| SEC-7 | Security | Cache opens SQLite via unencoded path URL | |
| SEC-8 | Security | Cache DB created world-readable | |
| SEC-9 | Security | query_log.jsonl created world-readable | |
| RB-11 | Robustness | format_timestamp panics on negative created_at | |
| RB-12 | Robustness | splade_alpha NaN/OOR silently corrupts hybrid scores | |
| DS-45 | Data Safety | INSERT OR IGNORE retains stale entry on fingerprint fallback | |
| EH-16 | Error Handling | prune_orphan_sparse_vectors defined but never called | |

## P2 — Medium + High Impact (fix in batch)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| CQ-5 | Code Quality | Batch search missing --include-type/--exclude-type | |
| CQ-7 | Code Quality | resolve_parent_context dedup uses child ID, never fires | |
| PF-5 | Performance | SPLADE single-threaded, encode_batch never used | |
| PF-12 | Performance | chunk_type_language_map scans all chunks per search | |
| PF-14 | Performance | SPLADE encode copies full logits tensor (15.6MB/call) | |
| DS-47 | Data Safety | Cache pool missing busy_timeout | |
| DS-49 | Data Safety | evict() measures physical pages, not logical data | |
| DS-50 | Data Safety | Concurrent block_on serializes GPU/CPU cache access | |
| RB-10 | Robustness | SPLADE session panics on poisoned Mutex | |
| RB-13 | Robustness | SPLADE encode no input length cap | |

## P3 — Easy + Low Impact (fix if time)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| OB-1 | Observability | stats() swallows 5 SQLite failures | |
| OB-2 | Observability | evict() PRAGMA failure silent | |
| OB-3 | Observability | read_batch blob mismatch silent | |
| OB-4 | Observability | log_query open failure silent | |
| OB-5 | Observability | dispatch() missing tracing span | |
| OB-6 | Observability | cache subcommand helpers missing spans | |
| EH-13 | Error Handling | SPLADE encode error silent in batch | |
| EH-14 | Error Handling | ensure_splade_index silently ignores DB error | |
| EH-15 | Error Handling | get_chunk_with_embedding errors silent in neighbors | |
| PF-6 | Performance | write_batch allocates per-embedding Vec<u8> | |
| PF-7 | Performance | read_batch rebuilds SQL placeholder per sub-batch | |
| PF-8 | Performance | prepare_for_embedding collects hash list twice | |
| PF-9 | Performance | GPU embed 3 passes for one debug log | |
| PF-10 | Performance | as_slice().to_vec() clones embeddings for cache write | |
| PF-11 | Performance | upsert_sparse_vectors one DELETE per chunk | |
| PF-13 | Performance | SpladeIndex bounds-checked get in hot loop | |
| CQ-1/DS-48/TC-19 | Multiple | VerifyReport dead type (3 findings, 1 fix) | |
| CQ-2 | Code Quality | SearchFilter::new() dead method | |
| CQ-3 | Code Quality | test_eviction duplicates open internals | |
| CQ-4 | Code Quality | cache_path_display recomputes default_path | |
| CQ-6 | Code Quality | Double rerank guard unreachable | |
| CQ-8 | Code Quality | search_hybrid bare unwrap | |
| DS-44 | Data Safety | write_batch no dim validation | |
| DS-46 | Data Safety | Negative dim wraps to usize | |
| RB-14 | Robustness | token_id negative wraps to u32 | |

## P4 — Test Coverage (add tests)

| ID | Category | Title | Status |
|----|----------|-------|--------|
| TC-17 | Test Coverage | HnswIndex::search_filtered zero unit tests | |
| TC-18 | Test Coverage | find_rank matches name only, file unused | |
| TC-20 | Test Coverage | read_batch >100 hashes path untested | |
| TC-21 | Test Coverage | NaN/Inf embeddings untested | |
| TC-22 | Test Coverage | log_query zero tests | |
| TC-23 | Test Coverage | New chunk types missing from filter tests | |
| TC-24 | Test Coverage | prune edge cases untested | |
| TC-25 | Test Coverage | Eval harness helpers untested | |
| TC-26 | Test Coverage | write_batch duplicate hash untested | |

## Summary

- **P1**: 8 findings (all easy, security + robustness + data safety)
- **P2**: 10 findings (medium effort, performance + data safety + robustness)
- **P3**: 25 findings (easy, observability + error handling + performance + code quality)
- **P4**: 9 findings (test coverage gaps)
- **Total**: 52 unique findings (3 duplicates merged: CQ-1 = DS-48 = TC-19)
