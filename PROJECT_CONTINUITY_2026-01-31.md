# cqs - Project Continuity

Updated: 2026-01-31T23:30Z

## Current State

**v0.1.10 IN PROGRESS. Chunk-level incremental indexing + RRF hybrid search implemented.**

- Published to crates.io: `cargo install cqs` (v0.1.9)
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.9
- ~4600 lines across 9 modules
- 42 tests passing, 5 doctests, clippy clean

## Implemented (pending release)

### Chunk-Level Incremental Indexing (PR pending)

**Problem:** Editing one function re-embeds entire file. Wastes GPU/CPU time.

**Solution:** Use `content_hash` (BLAKE3) to lookup existing embeddings. Skip re-embedding unchanged chunks.

**Implementation:**
- `Store::get_embeddings_by_hashes()` for batch lookup
- Indexing loop checks cache first, only embeds new chunks
- Stats output: "Indexed X chunks (Y cached, Z embedded)"
- Verified 80-90% cache hit rate

### RRF Hybrid Search (PR pending)

**Problem:** Semantic search misses exact identifier matches.

**Solution:** Combine semantic + FTS5 keyword search with Reciprocal Rank Fusion.

**Implementation:**
- FTS5 virtual table `chunks_fts` for full-text search
- `normalize_for_fts()`: splits camelCase/snake_case → "words"
  - Example: `parseConfigFile` → "parse config file"
- RRF fusion: `score = Σ 1/(k + rank)` where k=60
- Enabled by default in CLI and MCP
- Schema version bumped from 1 to 2

**Key Files Changed:**
- `src/schema.sql`: Added FTS5 virtual table
- `src/store.rs`: Added normalize_for_fts(), search_fts(), rrf_fuse(), enable_rrf in SearchFilter
- `src/cli.rs`: Enable RRF by default
- `src/mcp.rs`: Enable RRF by default
- `tests/store_test.rs`: 12 tests including FTS and RRF tests

## Next Steps

1. Push branch and create PR for RRF (feat/rrf-hybrid-search)
2. Merge after CI passes
3. Add C and Java language support (if desired)
4. Release v0.1.10

## Future Work

### C and Java Languages

- Add tree-sitter-c, tree-sitter-java to Cargo.toml
- Add query definitions:
  - C: `function_definition`, `struct_specifier`
  - Java: `method_declaration`, `class_declaration`, `interface_declaration`
- Add to Language enum and file extension mapping

## Previous Session Notes

### v0.1.9 Release

- HNSW-guided filtered search (10-100x faster)
- SIMD cosine similarity via simsimd (2-4x faster)
- Shell completions, config file, lock PID, error hints
- CHANGELOG.md, rustdoc

## Hunches

- hnsw_rs lifetime forces reload (~1-2ms overhead) - library limitation
- FTS5 tokenization with preprocessing works well for code identifiers
