# Project Continuity

## Right Now

**v0.4.5 released** (2026-02-05)

20-category audit complete and published. Nothing active.

### Session Summary (2026-02-05)
- Completed P4 audit triage (PR #209)
- Created 7 GitHub issues (#202-208) for deferred items
- Released v0.4.5 to GitHub and crates.io
- All docs verified up to date

### Recent PRs
- #211: Release v0.4.5
- #210: Docs update (continuity, roadmap)
- #209: P4 triage completion
- #201: P3 code fixes

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
