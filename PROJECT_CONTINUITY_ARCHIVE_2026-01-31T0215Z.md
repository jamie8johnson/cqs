# cq - Archive

Session log and detailed notes.

---

## Session: 2026-01-31

### Bootstrap

Ran bootstrap per CLAUDE.md instructions:
- Created docs/ directory
- Created SESSION_CONTEXT.md, HUNCHES.md from templates
- Created ROADMAP.md from template
- Created tear files (this file and PROJECT_CONTINUITY)
- Scaffolded Cargo.toml per DESIGN.md dependencies section
- Created GitHub repo

Design doc version: 0.6.1-draft

Key architecture decisions from design doc:
- tree-sitter for parsing (not syn) - multi-language support
- ort + tokenizers for embeddings (not fastembed-rs) - GPU control
- nomic-embed-text-v1.5 model (768-dim, 8192 context)
- SQLite with WAL mode for storage
- Brute-force search initially, HNSW in Phase 4

---

## Session: 2026-01-31 (Continued - Design Refinement)

### Audit Rounds

Ran 7 comprehensive audit rounds on DESIGN.md:

1. **v0.6.1 → v0.6.2**: Fixed ort execution provider imports, model size (547MB)
2. **v0.6.2 → v0.7.0**: Security overhaul (path validation, symlinks, file size limits, file lock, UTF-8 handling, SIGINT)
3. **v0.7.0 → v0.8.0**: Fixed compilation errors (mutability, type parsing), added missing types
4. **v0.8.0 → v0.9.0**: Added comprehensive MCP Integration section (~300 lines)
5. **v0.9.0 → v0.10.0**: Added helper functions, FromStr/Display impls, Store struct
6. **v0.10.0 → v0.11.0**: Two-phase search, Parser query caching, API implementations
7. **v0.11.0 → v0.12.0**: Complete Store API (search_filtered, stats, etc.)
8. **v0.12.0 → v0.13.0**: MCP moved to Phase 1, Testing Strategy added

### Key Fixes

- `upsert_chunks_batch`: `&self` → `&mut self` (rusqlite transaction requires mut)
- `needs_reindex`: removed invalid `.flatten()` on `Option<i64>`
- `check_schema_version`: TEXT → parse as i32
- Added `file_mtime INTEGER` column to schema
- Two-phase search: Phase 1 loads id+embedding only, Phase 2 fetches content for top-N
- Parser caches compiled tree-sitter queries

### MCP Integration

Added full MCP server design:
- `cq serve` command with stdio/SSE transports
- Tools: `cq_search`, `cq_similar`, `cq_stats`, `cq_index`
- Full JSON schemas, error handling, type definitions
- Claude Code configuration examples
- Moved to Phase 1 per user request

### Testing Strategy

Added comprehensive testing section:
- Unit tests: Parser, Embedder, Store modules
- Integration tests: Full pipeline
- Eval suite: 10 golden queries per language, 80% recall@5 target

### Decisions

- MCP in Phase 1 (not Phase 3) - user wants Claude Code integration early
- Name `cq` confirmed available on crates.io
- Testing: all three tiers (unit, integration, eval)

Design doc now at v0.13.0, implementation ready.

---
