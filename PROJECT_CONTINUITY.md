# Project Continuity

## Right Now

**Releasing v0.6.0** (2026-02-06)

Branch: `main` — multi-index + all audit fixes merged.

### What's in v0.6.0
- Multi-index search (PR #258)
- P1 audit fixes — 12 items (PR #259)
- P2 audit fixes — 5 items (PR #261)
- P3 audit fixes — 11 items (PRs #262, #263)
- Remaining P3/P4 tracked in issues #264-270

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 6**: Security (index encryption, rate limiting)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Multi-index follow-ups
- #255: Pre-built reference packages
- #256: Cross-store dedup
- #257: Parallel search + shared Runtime

### P4 Deferred (7 remaining from v0.5.1 audit)
- #231: Notes file locking
- #232: CAGRA RAII guard pattern
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #239: Test coverage gaps (low-priority)
- #240: embedding_batches cursor pagination
- #241: Config permission checks

## Architecture

- Version: 0.6.0
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 418 tests (no GPU), 0 warnings, clippy clean
