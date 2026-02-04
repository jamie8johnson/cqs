# Project Continuity

## Right Now

**Post-Audit Cleanup** (2026-02-04)

### Just Merged
- #128: MCP concurrency fix (CRITICAL - read locks)
- #115-#120: First audit fixes

### PRs Needing Rebase
- #127: name_match_score + ensure_embedder (conflicts with #128)
- #129: Error path tests

### Open Issues (22 total)
Tracking in #130: https://github.com/jamie8johnson/cqs/issues/130

**Deferred to v0.3.0:**
- #103: O(n) notes search
- #107: Memory (all_embeddings)
- #125: Store refactor
- #106: ort stable release
- #122: Embedder session lock (documented limitation)

## Key Changes This Session

1. **MCP Concurrency** (#128)
   - `McpServer.embedder`: `Option` → `OnceLock`
   - `McpServer.audit_mode`: direct → `Mutex`
   - `handle_request(&mut self)` → `handle_request(&self)`
   - HTTP handler: `write()` → `read()` lock
   - Embedder methods: `&mut self` → `&self`

2. **Audit PRs Merged** (#115-#120)
   - Glob compiled once (was per-chunk)
   - Off-by-one line numbers fixed
   - CAGRA mutex poison recovery
   - Config parse errors logged
   - Batch INSERT for calls
   - deny.toml added
   - CagraIndex resources behind Mutex
   - HNSW safety tests
   - ChunkType consolidated
   - Parser unit tests (21)

## Architecture

- 769-dim embeddings, E5-base-v2
- VectorIndex: CAGRA (GPU) > HNSW (CPU)
- Schema v10, SQLite WAL mode
- MCP: concurrent via interior mutability

## Next Steps

1. Rebase #127, #129 and merge
2. Work on remaining open issues
3. Update CHANGELOG for merged PRs
