# cqs - Project Continuity

Updated: 2026-01-31T23:45Z

## Current State

**main branch ready for v0.1.10 release.**

- Published to crates.io: `cargo install cqs` (v0.1.9)
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.9
- ~4600 lines across 9 modules
- 42 tests passing, 5 doctests, clippy clean

## Implemented (pending release as v0.1.10)

### 1. Chunk-Level Incremental Indexing (merged)

**Problem:** Editing one function re-embeds entire file. Wastes GPU/CPU time.

**Solution:** Use `content_hash` (BLAKE3) to lookup existing embeddings.

**Implementation:**
- `Store::get_embeddings_by_hashes()` for batch lookup
- Indexing loop checks cache first, only embeds new chunks
- Stats output: "Indexed X chunks (Y cached, Z embedded)"
- Verified 80-90% cache hit rate

### 2. RRF Hybrid Search (PR #24, merged)

**Problem:** Semantic search misses exact identifier matches.

**Solution:** Combine semantic + FTS5 keyword search with Reciprocal Rank Fusion.

**Implementation:**
- FTS5 virtual table `chunks_fts` for full-text search
- `normalize_for_fts()`: splits camelCase/snake_case → words
  - Example: `parseConfigFile` → "parse config file"
- RRF fusion: `score = Σ 1/(k + rank)` where k=60
- Enabled by default in CLI and MCP
- Schema version bumped from 1 to 2

**Key Files:**
- `src/schema.sql` - FTS5 virtual table
- `src/store.rs` - normalize_for_fts, search_fts, rrf_fuse, enable_rrf
- `src/cli.rs`, `src/mcp.rs` - RRF enabled by default
- `tests/store_test.rs` - 12 tests including FTS and RRF

## Next Steps

1. **Release v0.1.10** - Package incremental indexing + RRF
2. **Optional: C and Java language support**
   - Add tree-sitter-c, tree-sitter-java to Cargo.toml
   - C: `function_definition`, `struct_specifier`
   - Java: `method_declaration`, `class_declaration`, `interface_declaration`

## Recent PRs

- PR #24: RRF hybrid search (merged)
- PR #23: Cleanup - tests, warnings, pre-commit hooks (merged)
- PR #22: Chunk-level incremental indexing (merged)

## Hunches

- hnsw_rs lifetime forces reload (~1-2ms overhead) - library limitation
- FTS5 tokenization with preprocessing works well for code identifiers (resolved)
