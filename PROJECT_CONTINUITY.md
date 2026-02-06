# Project Continuity

## Right Now

**20-category audit complete, triage done, ready to fix P1** (2026-02-06)

Branch: `main` — PR #258 merged (multi-index).

### Audit state
- Full 20-category audit completed (4 batches × 5 parallel agents)
- 193 raw findings in `docs/audit-findings.md`, ~120 unique after dedup
- Triage done: 12 P1, 6 P2, 15 P3, 15 P4
- Previous audit P4 issues (#231-241) cross-checked — overlapping findings noted

### P1 fixes needed (easy + high impact)
1. Reference name path traversal (security)
2. Glob filter wrong path extraction in brute-force search (correctness)
3. Pipeline wrong file_mtime across batched files (correctness)
4. StoreError messages say `cq` not `cqs` (user-facing)
5. Language param silently defaults to Rust (user-facing)
6. SECURITY.md stale (docs)
7. `tagged_score()` redundant (dead code)
8. Reference threshold before vs after weight (correctness)
9. SSE endpoint missing origin validation (security)
10. 5 stale doc fixes (search.rs, note.rs, language/mod.rs, store/mod.rs, CONTRIBUTING.md)
11. lib.rs Quick Start unnecessary `mut` (docs)
12. serde_json unwrap_or_default in notes (error handling)

### Uncommitted files
- `docs/audit-findings.md` — full audit findings
- `docs/notes.toml` — groomed (4 removed, 10 mentions updated)
- `PROJECT_CONTINUITY.md` — this file

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

- Version: 0.5.3
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 388 tests (no GPU), 0 warnings, clippy clean
