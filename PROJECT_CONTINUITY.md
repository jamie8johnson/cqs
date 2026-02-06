# Project Continuity

## Right Now

**20-Category Audit — Fix Phase** (2026-02-05)

### Current Task
Fixing P1 audit findings. Plan at `~/.claude/plans/serialized-enchanting-moon.md`.

### Audit Status
- **Collection complete**: All 4 batches (20 categories) done
- **Findings**: ~120 raw, ~85 actionable after dedup
- **Findings file**: `docs/audit-findings.md` (full details)
- **Triage**: P1 (12 items), P2 (23), P3 (12), P4 (~12 deferred)
- **Fix progress**: Starting P1

### P1 Items (in progress)
1. Documentation fixes (6 items: lib.rs, README, CHANGELOG, ROADMAP, CONTRIBUTING x2)
2. Deduplicate `strip_unc_prefix` → shared `path_utils.rs`
3. MCP stats: use `count_vectors()` not full HNSW load
4. Deduplicate `load_hnsw_index` → `HnswIndex::try_load()`
5. MCP JSON-RPC types visibility → `pub(crate)`
6. Regex caching in `sanitize_error_message` → `LazyLock`
7. Error propagation: 6 silent-swallow fixes
8. `cli::run()` dead code removal
9. `EMBEDDING_DIM` consolidation → single constant in lib.rs
10. `split_into_windows` assert → Result
11. Glob filter BEFORE heap (algorithm correctness bug)
12. Windows path extraction bug in brute-force search

### Open PRs
None.

### GPU Build
```bash
bash ~/gpu-test.sh test --features gpu-search  # all env vars set
bash ~/gpu-test.sh build --features gpu-search
```
Needs: CUDA 13.1, conda base env (miniforge3), libcuvs 25.12

## Completed This Session
- **20-category audit collection**: 4 batches × 5 parallel agents
- Previous session: v0.5.0 release, C/Java, model eval, parser refactor, 50 tests, NL templates

## Parked

- **Phase 5**: Multi-index (deferred for audit)

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
