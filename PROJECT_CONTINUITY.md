# Project Continuity

## Right Now

**Parser/registry refactor complete** (2026-02-05)

Consolidated parser.rs duplication with language/ registry. parser.rs: 1469 → 1056 lines (28% reduction).

### Completed This Session
- **PR triage**: Merged #52, #54, #53, #51, #220. Closed #164, #50.
- **Phase 1**: Model eval — E5-base-v2 stays (100% Recall@5). PR #221 merged.
- **Phase 2**: Skipped (E5 wins).
- **Phase 3**: C and Java language support. PR #222 merged.
- **Refactor**: Parser/registry consolidation. Language enum moved to language/mod.rs. Query constants deleted from parser.rs. Methods delegate to REGISTRY via Language::def(). infer_chunk_type data-driven via LanguageDef fields.

### What's Next (per approved plan)
- **Phase 4**: Template experiments in nl.rs
- **Phase 5**: Multi-index (5 sub-phases)

### Open PRs
None. (Refactor uncommitted — needs PR.)

## Parked

Nothing active.

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

## Architecture

- Version: 0.4.6
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- Unified HNSW index (chunks + notes with prefix)
- Language enum + LanguageDef registry in language/mod.rs (source of truth)
- Parser re-exports Language, ChunkType from language module
- Store: split into focused modules (7 files including migrations)
- CLI: mod.rs + display.rs + watch.rs + pipeline.rs
- 326+ tests (including CLI, server, stress tests)
