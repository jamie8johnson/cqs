# Project Continuity

## Right Now

**20-category audit complete** (2026-02-05)

All P1-P4 items addressed. Future work tracked in issues #202-208.

### Audit Summary
- **~243 findings** across 20 categories
- **P1 (~93)**: ✅ Complete - critical fixes merged
- **P2 (~79)**: ✅ Complete - documentation and code hygiene
- **P3 (~41)**: ✅ Complete - PR #201 (FTS error handling, limits, progress)
- **P4 (~30)**: ✅ Complete - PR #209 (19 OK, 11 → issues)

### PRs This Session
- #199: P1 fixes
- #200: P2 documentation
- #201: P3 code fixes
- #209: P4 triage completion

## Open Issues

### P4 Deferred (from audit)
- #202: API key security (env visibility, memory sanitization)
- #203: Note search O(n) optimization
- #204: Pipeline resource sharing
- #205: Server function tests
- #206: CLI integration tests
- #207: Stress tests
- #208: Lazy grammar loading

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
