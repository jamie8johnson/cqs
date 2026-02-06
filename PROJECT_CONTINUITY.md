# Project Continuity

## Right Now

**Implementing multi-index support** (2026-02-06)

Branch: main (will create feature branch). Plan approved: `/home/user001/.claude/plans/stateful-sparking-swing.md`

### Active: Multi-index (Phase 5 final item)
- Plan: 7 steps, 14 files (2 new), ~30 tests
- Step 1: Config (ReferenceConfig, override_with, read-modify-write helpers)
- Step 2: reference.rs (ReferenceIndex, load_references, merge_results, TaggedResult)
- Step 3: MCP integration (server + search multi-search path)
- Step 4: MCP stats with references
- Step 5: CLI ref commands, display, query, name_only, sources filter, doctor
- Step 6-7: Tests, docs, cleanup

### Done this session
- Released v0.5.3 (prior session)
- Planned multi-index feature with two fresh-eyes reviews
- Found & fixed: store.init() needs ModelInfo, ref update was missing HNSW rebuild

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 6**: Security (index encryption, rate limiting)
- **Multi-index**: reference codebases — **now in progress** (plan approved)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### P4 Deferred (7 remaining)
- #231: Notes file locking
- #232: CAGRA RAII guard pattern
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #239: Test coverage gaps (low-priority)
- #240: embedding_batches cursor pagination
- #241: Config permission checks

## Architecture

- Version: 0.5.3
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 383 tests (no GPU), 0 warnings, clippy clean
