# Project Continuity

## Right Now

**Session Complete** (2026-02-04)

Housekeeping done. Deferred items tracked in #139.

## Session Summary

### Merged This Session
- #133: Store god object refactor (#125 closed)
- #134: CHANGELOG update
- #135: Lock poisoning DEBUG logs (#70)
- #136: MODEL_NAME constant (#70)
- #137: Doc comments + test helper (#70 closed)
- #138: Final housekeeping (mut warnings, ExitCode, ServeConfig)

## Deferred Items (#139)

| Item | Difficulty | Notes |
|------|------------|-------|
| r2d2 pool size tuning | 2 hr | Needs benchmarking |
| Schema migrations | 1 day | Implement incremental migration |
| hnsw_rs lifetime fix | 1 day+ | Upstream or major refactor |
| bincode replacement | 2 days | Breaking change, mitigated |

## Open Issues (7)

| Issue | Description | Status |
|-------|-------------|--------|
| #139 | Deferred housekeeping | Tracking |
| #130 | Tracking | Keep |
| #126 | Error tests | Partial |
| #107 | Memory | v0.3.0 |
| #106 | ort stable | External |
| #103 | O(n) notes | v0.3.0 |
| #63 | paste dep | External |

## Architecture

- 769-dim embeddings (768 + sentiment)
- Store: split into focused modules (6 files)
- Schema v10, WAL mode
- tests/common/mod.rs for test fixtures
