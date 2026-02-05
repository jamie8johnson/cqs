# Project Continuity

## Right Now

**v0.4.4 releasing** (2026-02-05)

**Audit complete.** Codebase in good shape after fresh-eyes review found and fixed dead code.

### Session Summary (2026-02-05)
Post-audit cleanup:
- **CAGRA streaming** - GPU index now streams embeddings like HNSW (PR #180)
- **Dead code removed** - `search_unified()` was never called (PR #182)
- **`note_weight` parameter** - tune how prominently notes appear in results (PR #183)

### Key Changes in v0.4.4
- `--note-weight 0.5` to make notes rank below code
- CAGRA streams from SQLite, includes notes with `note:` prefix
- Cleaner search.rs after dead code removal

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

Nothing actionable until upstream releases.

## Architecture

- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Store: split into focused modules (6 files)
- CLI: mod.rs + display.rs + watch.rs
- Schema v10, WAL mode
- 280+ tests
