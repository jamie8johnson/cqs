# cqs - Project Continuity

Updated: 2026-01-31T24:00Z

## Current State

**v0.1.10 released.**

- Published to crates.io: `cargo install cqs` (v0.1.10)
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.10
- ~4800 lines across 9 modules
- 45 tests passing (12 store tests including FTS/RRF), clippy clean

## Released in v0.1.10

### 1. Chunk-Level Incremental Indexing

**Problem:** Editing one function re-embeds entire file. Wastes GPU/CPU time.

**Solution:** Use `content_hash` (BLAKE3) to lookup existing embeddings.

**Implementation:**
- `Store::get_embeddings_by_hashes()` for batch lookup
- Indexing loop checks cache first, only embeds new chunks
- Stats output: "Indexed X chunks (Y cached, Z embedded)"
- Verified 80-90% cache hit rate

### 2. RRF Hybrid Search

**Problem:** Semantic search misses exact identifier matches.

**Solution:** Combine semantic + FTS5 keyword search with Reciprocal Rank Fusion.

**Implementation:**
- FTS5 virtual table `chunks_fts` for full-text search
- `normalize_for_fts()`: splits camelCase/snake_case → words
- RRF fusion: `score = Σ 1/(k + rank)` where k=60
- Enabled by default in CLI and MCP
- Schema version bumped from 1 to 2

## Next Steps

1. **Optional: C and Java language support**
   - Add tree-sitter-c, tree-sitter-java to Cargo.toml
   - C: `function_definition`, `struct_specifier`
   - Java: `method_declaration`, `class_declaration`, `interface_declaration`

## Recent PRs

- PR #28: Mark resolved hunches (merged)
- PR #27: Document RRF in README (merged)
- PR #26: Release v0.1.10 (merged)
- PR #25: Update docs after RRF merge (merged)
- PR #24: RRF hybrid search (merged)
- PR #23: Cleanup - tests, warnings, pre-commit hooks (merged)
- PR #22: Chunk-level incremental indexing (merged)

## Upgrade Note

Users upgrading from v0.1.9 or earlier need to rebuild their index:
```bash
cqs index --force
```
This rebuilds the FTS5 keyword index for RRF hybrid search.
