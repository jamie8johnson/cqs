# Project Continuity

## Right Now

**Clean state** (2026-02-06)

Branch: main. Commit `9be4453` (notes grooming) not yet pushed.

### This session
- Groomed notes.toml: 40 → 31 notes (removed 9 stale/duplicate/superseded)

## Parked

- **Phase 6**: Security (index encryption, rate limiting)
- **Multi-index**: reference codebases (after model question settled)
- **P4 issues**: #231-#241 (file locking, CAGRA guard, CJK, etc.)

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### P4 Deferred
- #231: Notes file locking
- #232: CAGRA RAII guard pattern
- #233: Cache parsed notes.toml in MCP server
- #234: search.rs / store::helpers refactor
- #235: Dual tokio runtimes in HTTP mode
- #236: HNSW-SQLite freshness validation
- #237: TOML manual escaping → serializer
- #238: CJK tokenization
- #239: Test coverage gaps (low-priority)
- #240: embedding_batches cursor pagination
- #241: Config permission checks

## Architecture

- Version: 0.5.1
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 381 tests (no GPU), 0 warnings, clippy clean
