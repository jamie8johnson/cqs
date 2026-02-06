# Project Continuity

## Right Now

**Clean state** (2026-02-06)

Branch: main, synced with remote. No pending work.

### Done this session
- PR #252 merged: CJK tokenization for FTS search (closed #238)
- PR #253 merged: Store boundary refactor + TOML serializer (closed #234, #237)
- Closed #235 (dual runtimes) as not_planned — accepted, documented
- Added "update the roadmap" to CLAUDE.md completion checklist
- Marked note grooming (#245) as done in ROADMAP.md
- Groomed notes: 62 → 60

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 6**: Security (index encryption, rate limiting)
- **Multi-index**: reference codebases (model eval done, ready to build)

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

- Version: 0.5.2
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 383 tests (no GPU), 0 warnings, clippy clean
