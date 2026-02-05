# Project Continuity

## Right Now

**20-category audit in progress** (2026-02-05)

PR #190 addresses P1/P2 findings. Waiting for CI.

### Session Summary (2026-02-05)
20-category audit execution:
- **202 findings** collected across 4 batches (in `docs/audit-findings.md`)
- **Most easy items already fixed** in prior sessions
- **P1/P2 fixes** in PR #190:
  - Transaction wrappers for delete operations
  - `debug_assert` → `assert` for dimension checks
  - Logging for swallowed `.ok()?` patterns
  - CHANGELOG historical note about model change
- **P3/P4 deferred** to issues #186-189

### Previous Session
- CAGRA streaming (PR #180)
- Dead `search_unified()` removed (PR #182)
- `note_weight` parameter (PR #183)
- v0.4.4 released

## Open Issues

### Audit Follow-up
- #186: Non-atomic HNSW writes (P3)
- #187: File permissions (P3)
- #188: Schema migrations (P4)
- #189: Test coverage (P4)

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## Architecture

- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Store: split into focused modules (6 files)
- CLI: mod.rs + display.rs + watch.rs
- Schema v10, WAL mode
- 280+ tests
