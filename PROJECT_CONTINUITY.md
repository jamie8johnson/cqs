# Project Continuity

## Right Now

**Store Refactor Complete** (2026-02-04)

Issue #125 implemented. Pending PR.

## Session Summary

### Completed This Session
- #125: Store god object refactor (1,916 lines → 6 files, largest 531)

### Merged PRs (Previous)
- #132: embedder.rs + cli.rs unit tests (#62)
- #128: MCP concurrency (CRITICAL)
- #115-#120: First audit batch
- #131: name_match_score + config tests

### Open Issues (8)
| Issue | Description | Status |
|-------|-------------|--------|
| #130 | Tracking | Keep |
| #126 | Error tests | Partial |
| #125 | Store refactor | **DONE** |
| #107 | Memory | v0.3.0 |
| #106 | ort stable | External |
| #103 | O(n) notes | v0.3.0 |
| #70 | Cleanup | Low |
| #63 | paste dep | External |

## Store Refactor (#125)

Split `src/store.rs` (1,916 lines) into:
```
src/store/
  mod.rs       468 lines  (Store struct, open/init, FTS, RRF)
  chunks.rs    352 lines  (chunk CRUD)
  notes.rs     197 lines  (note CRUD)
  calls.rs     220 lines  (call graph)
  helpers.rs   245 lines  (types, embedding conversion)
src/search.rs  531 lines  (search algorithms, scoring)
```

Largest file: 531 lines (down from 1,916, 3.6x reduction)

## Architecture

- 769-dim embeddings (768 model + 1 sentiment)
- CAGRA (GPU) > HNSW (CPU)
- Schema v10, WAL mode
- MCP: interior mutability
- Store: split into focused modules

## Next

1. Create PR for #125
2. CHANGELOG
3. v0.3.0: memory (#107), notes HNSW (#103)
