# Project Continuity

## Right Now

**v0.4.3 released** (2026-02-05)

**Audit complete.** All P2-P4 items fixed or verified as design choices.

### Session Summary (2026-02-05)
Fixed the two "hard" deferred issues:
- **#107 Memory OOM** - Streaming HNSW build (PR #176)
- **#103 O(n) note search** - Notes in unified HNSW (PR #177)

Then shipped v0.4.3 (PR #178) to GitHub + crates.io.

### Key Changes in v0.4.3
- `Store::embedding_batches()` - streams in 10k batches via LIMIT/OFFSET
- `HnswIndex::build_batched()` - incremental build, O(batch_size) memory
- Notes in HNSW with `note:` prefix - O(log n) search
- `Store::note_embeddings()` and `search_notes_by_ids()`
- HNSW build moved after note indexing
- Index output: `HNSW index: 879 vectors (811 chunks, 68 notes)`

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
