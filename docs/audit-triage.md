# Audit Triage — v1.15.1 (2026-04-04)

103 findings across 16 categories. Post-refactoring audit (29 PRs, JSON schema migration).

## P1: Easy + High Impact (fix immediately)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| EH-7 | Error Handling | `is_hnsw_dirty()` wrong default — DB error biases toward stale HNSW | |
| RB-7 | Robustness | Unicode panic in telemetry `print_telemetry_text` on multi-byte chars | |
| AC-7 | Algorithm | BFS node cap checked before target — returns None for reachable paths | |
| EXT-39 | Extensibility | `block_comment` doc_format has no match arm — wrong ST comment syntax | |
| TC-6 | Test Coverage | `token_pack` zero budget returns empty despite "at least one" guarantee | |
| SEC-10 | Security | `search_by_name` FTS guard uses `debug_assert!` — compiled out in release | |
| AD-11 | API Design | `ProjectSearchResult` uses `"line"` not `"line_start"` — missed normalization | |
| AD-13 | API Design | `NeighborEntry` uses `"similarity"` not `"score"` | |
| AD-15 | API Design | `ExternalCallerEntry` uses `"caller"`/`"callee"` not `"name"` | |
| PF-2 | Performance | `rrf_fuse` reads `CQS_RRF_K` env var per search — no OnceLock | |

## P2: Medium Effort + High Impact (fix in batch)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| CQ-NEW-3 | Code Quality | `display.rs` three divergent search result JSON constructors | |
| CQ-NEW-4 | Code Quality | `cmd_onboard` token-packing diverges from shared helpers | |
| CQ-NEW-5 | Code Quality | `dispatch_similar` / `cmd_similar` incompatible JSON schemas | |
| CQ-NEW-6 | Code Quality | `impact_to_json` post-serialize mutation (PF-3 overlap) | |
| CQ-NEW-7 | Code Quality | `SearchResult`/`UnifiedResult` manual JSON builders | |
| AD-14 | API Design | Batch context summary mode completely different schema from CLI | |
| AD-17 | API Design | `CommandContext` has no writable constructor | |
| AC-10 | Algorithm | `build_test_map` BFS has no node cap — unbounded memory | |
| RM-9 | Resource Mgmt | `store_stage` deferred vecs unbounded — 100-200MB on large indexes | |
| HP-4 | Test Coverage | Batch tests cover 5 of ~30 commands | |
| HP-7 | Test Coverage | `cmd_query` + `display_similar_results_json` no unit tests | |
| HP-8 | Test Coverage | `CommandContext` lazy init no tests | |
| DS-NEW-1 | Data Safety | HNSW lock released after load — concurrent writer unblocked | |

## P3: Easy + Low Impact (fix if time)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| CQ-NEW-1 | Code Quality | 4 trivial `*_to_json` wrappers — public API noise | |
| CQ-NEW-2 | Code Quality | `related_result_to_json`/`where_to_json` redundant wrappers | |
| EH-8 | Error Handling | 6 `*_to_json` fall back to `json!({})` on error | |
| EH-9 | Error Handling | `chunk_count()` error silently bypasses GPU index | |
| EH-10 | Error Handling | `build_brief_data` returns partial data silently | |
| EH-11 | Error Handling | `build_full_data` external callers/callees silently empty | |
| EH-12 | Error Handling | `parse_notes` failures invisible in `cmd_read` | |
| AD-12 | API Design | `lines: [u32; 2]` array instead of `line_start`/`line_end` scalars | |
| AD-16 | API Design | Pipeline test fixtures use pre-migration field names | |
| AD-18 | API Design | `SuggestOutput` dead code | |
| PB-8 | Platform | `DiffEntryOutput.file` uses `display()` — backslashes on Windows | |
| PB-9 | Platform | `ExplainOutput` mixed relative/absolute paths | |
| PB-10 | Platform | `chrono_like_timestamp` spawns POSIX `date` — fails on Windows | |
| PF-1 | Performance | `map_hunks_to_functions` N+1 DB queries | |
| PF-3 | Performance | `impact_to_json` double-pass serialize+mutate (= CQ-NEW-6) | |
| PF-4 | Performance | `merge_results` recomputes blake3 — ignores stored hash | |
| SHL-16 | Scaling | `bfs_shortest_path` MAX_NODES not configurable | |
| SHL-17 | Scaling | HNSW file-size limits hardcoded — block large indexes | |
| SHL-20 | Scaling | Telemetry file unbounded growth | |
| SEC-7 | Security | `cmd_telemetry_reset` no size guard on file read | |
| SEC-8 | Security | L5K regex O(N * unterminated) on malformed input | |
| SEC-9 | Security | `run_git_log_line_range` trusts git error text | |
| RM-7 | Resource Mgmt | `open_readonly` isn't actually readonly | |
| RM-8 | Resource Mgmt | Notes commands open two store connections | |
| DS-NEW-2 | Data Safety | Telemetry reset races with concurrent log_command | |
| DS-NEW-4 | Data Safety | `cached_notes_summaries` double-checked locking gap | |
| AC-8 | Algorithm | `window_overlap_tokens` edge case silently skips windowing | |
| AC-9 | Algorithm | `rrf_fuse` asymmetric deduplication | |
| AC-11 | Algorithm | `index_pack` no 10x budget guard on first item | |
| RB-8 | Robustness | `Cli::model_config()` panics before resolve | |
| RB-9 | Robustness | `count_sessions` over-counts on leading Reset events | |
| EXT-40 | Extensibility | `chat.rs` stale hardcoded command list | |
| EXT-41 | Extensibility | `PIPEABLE_NAMES` sync test one-directional | |

## P4: Trivial / Low Priority

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| OB-1 to OB-9 | Observability | 9 missing tracing spans | |
| DOC-11 to DOC-20 | Documentation | 10 stale paths/counts in README/CONTRIBUTING/source | |
| SHL-18 | Scaling | FILE_BATCH_SIZE not configurable | |
| SHL-19 | Scaling | MAX_CONTENT_CHARS not configurable | |
| SHL-21 | Scaling | Stale "768" in doc comments | |
| PB-11 | Platform | L5X CRLF ordering invariant untested | |
| DS-NEW-3 | Data Safety | Test env var race (already partially fixed in PR #770) | |
| EXT-42 | Extensibility | JSON naming conventions not documented in CONTRIBUTING | |
| TC-7 to TC-16 | Test Coverage | 10 adversarial test gaps | |
| HP-1 to HP-9 | Test Coverage | 9 happy-path test gaps (except HP-4,7,8 which are P2) | |
| RM-10, RM-11 | Resource Mgmt | Telemetry memory usage | |

## Summary

| Priority | Count | Description |
|----------|-------|-------------|
| P1 | 10 | Easy + high impact — fix now |
| P2 | 13 | Medium effort + high impact — batch fix |
| P3 | 33 | Easy + low impact — fix if time |
| P4 | ~47 | Trivial / low priority |
| **Total** | **103** | |

**0 P1 bugs from the JSON schema migration itself.** The P1s are pre-existing (EH-7, AC-7) or normalization misses (AD-11, AD-13, AD-15). The migration introduced no regressions.
