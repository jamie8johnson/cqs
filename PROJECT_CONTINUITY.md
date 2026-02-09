# Project Continuity

## Right Now

**Rename `.cq/` → `.cqs/` (issue #260).** 2026-02-08. Branch: `fix/rename-cq-to-cqs`.

Consistency rename — index directory `.cq/` → `.cqs/` to match binary name, config dir, config file.
Auto-migration: `resolve_index_dir()` in `src/lib.rs` renames `.cq/` → `.cqs/` on first access.

### What's done
- Central `INDEX_DIR` constant + `resolve_index_dir()` migration helper in `src/lib.rs`
- All ~40 hardcoded `.cq` references in src/ and tests/ updated
- Variable renames: `cq_dir` → `cqs_dir` throughout
- Dual fallback in `project.rs` (cross-project search works with unmigrated projects)
- Docs updated: SECURITY.md, PRIVACY.md, skills (migrate, troubleshoot, bootstrap)
- All 302 lib + 233 integration tests pass, clippy clean

### Pending
- Commit, PR, merge
- Release binary update after merge
- `.cqs.toml` — untracked, has aveva-docs reference config

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references (substring matching of chunk names across docs)
- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255
- **`.cq` rename to `.cqs`** — in progress on branch `fix/rename-cq-to-cqs`

## Open Issues

### Reference index bugs (new)
- #318: ref update silently prunes all chunks when binary lacks language support
- #319: ref remove leaves stale metadata, blocking re-add with same name

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Multi-index follow-ups
- #255: Pre-built reference packages
- #256: Cross-store dedup
- #257: Parallel search + shared Runtime

### Remaining audit items
- #269: Brute-force search loads all embeddings (P4)
- #270: HNSW LoadedHnsw unsafe transmute (P4)

### P4 Deferred (v0.5.1 audit, still open)
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #240: embedding_batches cursor pagination

## Architecture

- Version: 0.9.6
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- 302 lib + 233 integration tests (with gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (also available as CLI commands now)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
