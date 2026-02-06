# Project Continuity

## Right Now

**Clean state** (2026-02-06)

Branch: main, synced with remote. No pending work.

### Recently merged
- PR #244: Brute-force notes + update/remove MCP tools (closes #230)
- PR #246: `cqs notes list` CLI + 7 Claude Code skills (closes #245)

### Skills
7 skills in `.claude/skills/`. Should appear in `/` after restart.
If they don't show in autocomplete, try removing `disable-model-invocation: true` from frontmatter.

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
