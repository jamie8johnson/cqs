# Project Continuity

## Right Now

**Session complete** (2026-02-05)

### Completed This Session
- **PR triage**: Merged #52, #54, #53, #51, #220. Closed #164, #50.
- **Phase 1**: Model eval — E5-base-v2 stays (100% Recall@5). PR #221 merged.
- **Phase 2**: Skipped (E5 wins).
- **Phase 3**: C and Java language support. PR #222 merged.
- **Refactor**: Parser/registry consolidation. PR #223 merged. parser.rs: 1469 → 1056 lines (28% reduction).
- **GPU setup**: CUDA 13.1 toolkit + conda + libcuvs 25.12 installed. `gpu-search` feature builds and passes all tests. Wrapper at `~/gpu-test.sh`.
- **Test coverage**: 50 new tests across 6 modules. PR #224 merged. 375 tests (GPU) / 364 (no GPU).

### What's Next
- **Phase 4**: Template experiments in nl.rs
- **Phase 5**: Multi-index (5 sub-phases)

### Open PRs
None.

### GPU Build
```bash
bash ~/gpu-test.sh test --features gpu-search  # all env vars set
bash ~/gpu-test.sh build --features gpu-search
```
Needs: CUDA 13.1, conda base env (miniforge3), libcuvs 25.12

## Parked

Nothing active.

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## Architecture

- Version: 0.5.0
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Language enum + LanguageDef registry in language/mod.rs (source of truth)
- Parser re-exports Language, ChunkType from language module
- Store: split into focused modules (7 files including migrations)
- CLI: mod.rs + display.rs + watch.rs + pipeline.rs
- 375 tests with GPU / 364 without (including CLI, server, stress tests)
