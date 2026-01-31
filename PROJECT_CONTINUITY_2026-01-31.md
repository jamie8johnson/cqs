# cqs - Project Continuity

Updated: 2026-01-31T22:00Z

## Current State

**v0.1.9 RELEASED. Planning v0.1.10 with chunk-level incremental indexing.**

- Published to crates.io: `cargo install cqs`
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.9
- ~4400 lines across 9 modules
- 38 tests passing, 5 doctests, clippy clean

## In Progress: Chunk-Level Incremental Indexing

**Problem:** Editing one function re-embeds entire file. Wastes GPU/CPU time.

**Solution:** Use `content_hash` (BLAKE3) to lookup existing embeddings. Skip re-embedding unchanged chunks.

### Implementation Plan (approved, not yet implemented)

**Step 1: Add batch hash lookup to Store** (`src/store.rs`)
```rust
pub fn get_embeddings_by_hashes(&self, hashes: &[&str]) -> HashMap<String, Embedding>
```
- Query: `SELECT content_hash, embedding FROM chunks WHERE content_hash IN (...)`
- Returns map of hash → embedding for reuse

**Step 2: Modify indexing loop** (`src/cli.rs:634-659`)
- Before embedding batch, check which hashes already exist
- Split into cached (reuse embedding) vs to_embed (need new embedding)
- Only call `embedder.embed_documents()` for new chunks
- Upsert all (cached + new) to DB

**Step 3: Add cache hit stats**
- Track cached vs embedded counts
- Print: "Indexed X chunks (Y cached, Z embedded)"

### Key Files to Change

| File | Change |
|------|--------|
| `src/store.rs` | Add `get_embeddings_by_hashes()` after line 647 |
| `src/cli.rs` | Modify indexing loop at lines 634-659 |

### Expected Impact

- 80-90% cache hit rate on re-index (per ck-search benchmarks)
- Near-instant re-index when content unchanged (only mtime changed)

## Future Work (After v0.1.10)

### 1. RRF Hybrid Search (biggest quality improvement)

Combine semantic + keyword search with Reciprocal Rank Fusion:
- Add FTS5 virtual table for keyword search
- Preprocess identifiers: `parseConfigFile` → "parse config file" (split camelCase/snake_case)
- Run semantic + FTS5 independently
- Fuse: `score = Σ 1/(k + rank)` where k=60

**Note:** Default FTS5 tokenizer won't split on underscore or camelCase. Need preprocessing.

### 2. C and Java Languages

- Add tree-sitter-c, tree-sitter-java to Cargo.toml
- Add query definitions:
  - C: `function_definition`, `struct_specifier`
  - Java: `method_declaration`, `class_declaration`, `interface_declaration`
- Add to Language enum and file extension mapping

## Previous Session - v0.1.9 Release

### PR #16: Performance and UX improvements (MERGED)

- HNSW-guided filtered search (10-100x faster)
- SIMD cosine similarity via simsimd (2-4x faster)
- Shell completions, config file, lock PID, error hints
- CHANGELOG.md, rustdoc

### PR #17-19: Doc updates (MERGED)

## Hunches

- hnsw_rs lifetime forces reload (~1-2ms overhead) - library limitation
- FTS5 tokenization needs preprocessing for code identifiers
