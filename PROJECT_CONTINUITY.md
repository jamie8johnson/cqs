# Project Continuity

## Right Now

**Error path tests complete** (2026-02-04)

Added tests for #126 (Error path coverage):
- `tests/hnsw_test.rs` - 6 tests for corruption/error detection
- `tests/store_test.rs` - 3 tests for schema/model validation
- `tests/mcp_test.rs` - 7 new edge case tests

Closed 6 issues that were already fixed by Store refactor (#133):
#142, #143, #144, #145, #146, #148

## Session Summary

### This Session
- Closed 6 stale issues (already fixed by Store refactor)
- Added 16 new error path tests across 3 files
- Test count: 169 total (up from 153)

### Issues Closed (Already Fixed)
| # | Title | Why Fixed |
|---|-------|-----------|
| #142 | Glob pattern per-chunk | search.rs:140 - compiled outside loop |
| #143 | Off-by-one line | parser.rs:532 - correct calculation |
| #144 | CAGRA mutex expect() | cagra.rs - uses unwrap_or_else |
| #145 | Silent config errors | config.rs - has tracing::warn |
| #146 | Parser unit tests | parser.rs:819+ - tests exist |
| #148 | N+1 calls insert | store/calls.rs - uses QueryBuilder batch |

## Open Issues (8)

### Medium (1-4 hr)
- #126: Error path tests (IN PROGRESS - tests added, needs PR)

### Hard (1+ day)
- #147: Duplicate types
- #103: O(n) note search
- #107: Memory OOM
- #139: Deferred housekeeping

### External/Waiting
- #106: ort stable
- #63: paste dep
- #130: Tracking issue

## Architecture

- 769-dim embeddings (768 + sentiment)
- Store: split into focused modules (6 files)
- Schema v10, WAL mode
- tests/common/mod.rs for test fixtures
