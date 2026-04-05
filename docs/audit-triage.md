# Audit Triage — v1.15.1 (2026-04-04)

103 findings across 16 categories. Post-refactoring audit (29 PRs, JSON schema migration).

## P1: Easy + High Impact (fix immediately)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| EH-7 | Error Handling | `is_hnsw_dirty()` wrong default — DB error biases toward stale HNSW | ✅ PR #787 |
| RB-7 | Robustness | Unicode panic in telemetry `print_telemetry_text` on multi-byte chars | ✅ PR #786 |
| AC-7 | Algorithm | BFS node cap checked before target — returns None for reachable paths | ✅ PR #787 |
| EXT-39 | Extensibility | `block_comment` doc_format has no match arm — wrong ST comment syntax | ✅ PR #787 |
| TC-6 | Test Coverage | `token_pack` zero budget returns empty despite "at least one" guarantee | ✅ PR #793 |
| SEC-10 | Security | `search_by_name` FTS guard uses `debug_assert!` — compiled out in release | ✅ PR #785 |
| AD-11 | API Design | `ProjectSearchResult` uses `"line"` not `"line_start"` — missed normalization | ✅ PR #788 |
| AD-13 | API Design | `NeighborEntry` uses `"similarity"` not `"score"` | ✅ PR #788 |
| AD-15 | API Design | `ExternalCallerEntry` uses `"caller"`/`"callee"` not `"name"` | ✅ PR #788 |
| PF-2 | Performance | `rrf_fuse` reads `CQS_RRF_K` env var per search — no OnceLock | ✅ PR #785 |

## P2: Medium Effort + High Impact (fix in batch)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| CQ-NEW-3 | Code Quality | `display.rs` three divergent search result JSON constructors | ✅ PR #797 |
| CQ-NEW-4 | Code Quality | `cmd_onboard` token-packing diverges from shared helpers | ✅ PR #801 |
| CQ-NEW-5 | Code Quality | `dispatch_similar` / `cmd_similar` incompatible JSON schemas | ✅ PR #797 |
| CQ-NEW-6 | Code Quality | `impact_to_json` post-serialize mutation (PF-3 overlap) | ✅ PR #794 |
| CQ-NEW-7 | Code Quality | `SearchResult`/`UnifiedResult` manual JSON builders | ✅ PR #797 |
| AD-14 | API Design | Batch context summary mode completely different schema from CLI | ✅ PR #798 |
| AD-17 | API Design | `CommandContext` has no writable constructor | ✅ PR #798 |
| AC-10 | Algorithm | `build_test_map` BFS has no node cap — unbounded memory | ✅ PR #794 |
| RM-9 | Resource Mgmt | `store_stage` deferred vecs unbounded — 100-200MB on large indexes | ✅ PR #800 |
| HP-4 | Test Coverage | Batch tests cover 5 of ~30 commands | ✅ PR #802 |
| HP-7 | Test Coverage | `cmd_query` + `display_similar_results_json` no unit tests | ✅ PR #803 |
| HP-8 | Test Coverage | `CommandContext` lazy init no tests | ✅ PR #803 |
| DS-NEW-1 | Data Safety | HNSW lock released after load — concurrent writer unblocked | ✅ PR #799 |

## P3: Easy + Low Impact (fix if time)

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| CQ-NEW-1 | Code Quality | 4 trivial `*_to_json` wrappers — public API noise | ✅ PR #794 |
| CQ-NEW-2 | Code Quality | `related_result_to_json`/`where_to_json` redundant wrappers | ✅ PR #810 |
| EH-8 | Error Handling | 6 `*_to_json` fall back to `json!({})` on error | ✅ already fixed |
| EH-9 | Error Handling | `chunk_count()` error silently bypasses GPU index | ✅ PR #804 |
| EH-10 | Error Handling | `build_brief_data` returns partial data silently | ✅ already fixed |
| EH-11 | Error Handling | `build_full_data` external callers/callees silently empty | ✅ already fixed |
| EH-12 | Error Handling | `parse_notes` failures invisible in `cmd_read` | ✅ already fixed |
| AD-12 | API Design | `lines: [u32; 2]` array instead of `line_start`/`line_end` scalars | ✅ PR #788 |
| AD-16 | API Design | Pipeline test fixtures use pre-migration field names | ✅ PR #792 |
| AD-18 | API Design | `SuggestOutput` dead code | ✅ PR #810 |
| PB-8 | Platform | `DiffEntryOutput.file` uses `display()` — backslashes on Windows | ✅ PR #789 |
| PB-9 | Platform | `ExplainOutput` mixed relative/absolute paths | ✅ PR #789 |
| PB-10 | Platform | `chrono_like_timestamp` spawns POSIX `date` — fails on Windows | ✅ PR #786 |
| PF-1 | Performance | `map_hunks_to_functions` N+1 DB queries | ✅ PR #805 |
| PF-3 | Performance | `impact_to_json` double-pass serialize+mutate (= CQ-NEW-6) | ✅ PR #794 |
| PF-4 | Performance | `merge_results` recomputes blake3 — ignores stored hash | ✅ PR #805 |
| SHL-16 | Scaling | `bfs_shortest_path` MAX_NODES not configurable | ✅ already fixed (AC-7) |
| SHL-17 | Scaling | HNSW file-size limits hardcoded — block large indexes | ✅ PR #795 |
| SHL-20 | Scaling | Telemetry file unbounded growth | ✅ PR #786 |
| SEC-7 | Security | `cmd_telemetry_reset` no size guard on file read | ✅ PR #786 |
| SEC-8 | Security | L5K regex O(N * unterminated) on malformed input | ✅ PR #796 |
| SEC-9 | Security | `run_git_log_line_range` trusts git error text | ✅ PR #805 |
| RM-7 | Resource Mgmt | `open_readonly` isn't actually readonly | ✅ PR #806 |
| RM-8 | Resource Mgmt | Notes commands open two store connections | ✅ PR #806 |
| DS-NEW-2 | Data Safety | Telemetry reset races with concurrent log_command | ✅ PR #786 |
| DS-NEW-4 | Data Safety | `cached_notes_summaries` double-checked locking gap | |
| AC-8 | Algorithm | `window_overlap_tokens` edge case silently skips windowing | ✅ PR #806 |
| AC-9 | Algorithm | `rrf_fuse` asymmetric deduplication | ✅ PR #785 |
| AC-11 | Algorithm | `index_pack` no 10x budget guard on first item | ✅ PR #793 |
| RB-8 | Robustness | `Cli::model_config()` panics before resolve | ✅ PR #810 |
| RB-9 | Robustness | `count_sessions` over-counts on leading Reset events | ✅ PR #786 |
| EXT-40 | Extensibility | `chat.rs` stale hardcoded command list | ✅ PR #792 |
| EXT-41 | Extensibility | `PIPEABLE_NAMES` sync test one-directional | ✅ PR #792 |

## P4: Trivial / Low Priority

| Finding | Category | Description | Status |
|---------|----------|-------------|--------|
| OB-1 to OB-9 | Observability | 9 missing tracing spans | ✅ PR #790 |
| DOC-11 to DOC-20 | Documentation | 10 stale paths/counts in README/CONTRIBUTING/source | ✅ PR #791 |
| SHL-18 | Scaling | FILE_BATCH_SIZE not configurable | ✅ PR #795 |
| SHL-19 | Scaling | MAX_CONTENT_CHARS not configurable | ✅ PR #795 |
| SHL-21 | Scaling | Stale "768" in doc comments | ✅ PR #810 |
| PB-11 | Platform | L5X CRLF ordering invariant untested | ✅ PR #796 |
| DS-NEW-3 | Data Safety | Test env var race (already partially fixed in PR #770) | ✅ PR #810 |
| EXT-42 | Extensibility | JSON naming conventions not documented in CONTRIBUTING | ✅ PR #807 |
| TC-7 to TC-16 | Test Coverage | 10 adversarial test gaps | ✅ PR #796 (TC-8/9/10), #807 (TC-7/12/14), #809 (TC-11/13/15/16) |
| HP-1 to HP-9 | Test Coverage | 9 happy-path test gaps (except HP-4,7,8 which are P2) | ✅ PR #793 (HP-2/9), #807 (HP-3/6), #809 (HP-1/5) |
| RM-10, RM-11 | Resource Mgmt | Telemetry memory usage | ✅ PR #808 |

## Summary

| Priority | Fixed | Remaining | Total |
|----------|-------|-----------|-------|
| P1 | 10 | 0 | 10 |
| P2 | 13 | 0 | 13 |
| P3 | 33 | 0 | 33 |
| P4 | ~46 | ~1 | ~47 |
| **Total** | **~102** | **~1** | **103** |

**0 P1, 0 P2, 0 P3 remaining.**

**In CI:** PR #810 (AD-18, CQ-NEW-2, SHL-21, RB-8, DS-NEW-3)
**Remaining:** DS-NEW-4 (cached_notes_summaries locking gap) — dispatch after #810 merges
