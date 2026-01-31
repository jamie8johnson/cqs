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

## Session: 2026-01-31 (Implementation & Testing)

### Implementation Complete

Implemented all 6 Phase 1 modules (~1800 lines):
- `src/parser.rs` (~320 lines) - tree-sitter parsing, 5 languages
- `src/store.rs` (~400 lines) - SQLite, two-phase search
- `src/embedder.rs` (~280 lines) - ort + tokenizers, CUDA/CPU
- `src/cli.rs` (~500 lines) - all commands
- `src/mcp.rs` (~300 lines) - JSON-RPC server, stdio transport
- `src/schema.sql` - database schema

### Rename cq → cqs

- Original name `cq` was taken on crates.io
- Renamed throughout: Cargo.toml, imports, MCP tool names
- Published v0.1.0 to crates.io as `cqs`
- Renamed GitHub repo to `jamie8johnson/cqs`
- Made repo public

### Embedder Fixes (Integration Testing)

Original embedder didn't work with actual ONNX model:
1. **i32 → i64**: Model expects int64 inputs, not int32
2. **token_type_ids**: Model requires this input (all zeros)
3. **Mean pooling**: Model outputs `last_hidden_state`, not `sentence_embedding`

After fixes, full pipeline works:
- `cqs init` - downloads model, creates .cq/
- `cqs index` - 121 chunks from cqs codebase
- `cqs "query"` - semantic search returns relevant results (0.65-0.73 scores)

### CUDA/GPU Investigation

Attempted GPU acceleration in WSL2:
- Installed NVIDIA CUDA repo, cuDNN 9
- cuDNN version mismatch (ort needs v9, Ubuntu had v8) - fixed
- WSL2 GPU visibility dropped during testing
- CPU fallback works reliably (~20ms per embedding)
- Documented in README as optional

### MCP Configuration

Added cqs as MCP server for Claude Code:
```bash
claude mcp add cqs -e LD_LIBRARY_PATH="..." -- /path/to/cqs serve
```
Config stored in `~/.claude.json` under project scope.
Needs Claude Code restart to activate.

### Files Added/Changed

- `SECURITY.md`, `PRIVACY.md` - new
- `README.md` - GPU setup, MCP config
- `.mcp.json` - project MCP config
- `.env.example` - credentials template
- `tests/` - parser and store tests (21 total)
- `tests/fixtures/` - sample files for 5 languages

---
