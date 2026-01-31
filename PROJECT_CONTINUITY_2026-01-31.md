# cqs - Project Continuity

Updated: 2026-01-31T23:30Z

## Current State

**v0.1.8 ready to commit: HNSW + P0 audit fixes.**

- ~3900 lines across 8 modules (added hnsw.rs)
- 32 tests passing (13 parser + 8 store + 8 MCP + 3 HNSW)
- Clippy clean
- v0.1.7 published to crates.io
- GitHub repo: github.com/jamie8johnson/cqs
- CI workflow running - all passing
- Branch ruleset active

### v0.1.8 Changes (ready to commit)

**HNSW Implementation (Phase 4):**
- `src/hnsw.rs` - NEW: HNSW index wrapper using hnsw_rs crate
- `src/lib.rs` - Added hnsw module export
- `src/store.rs` - Added `all_embeddings()` and `get_chunk_by_id()` methods
- `src/cli.rs` - Integrated HNSW build after index, HNSW search in query
- `Cargo.toml` - Added `hnsw_rs = "0.3"` dependency
- `CONTRIBUTING.md` - Updated architecture, added contribution ideas

**P0 Audit Fixes:**
- `src/mcp.rs` - Fixed RwLock panic risk in HTTP handler, added query length validation
- `src/embedder.rs` - Fixed LRU cache lock poisoning risk
- `src/store.rs` - Added embedding byte length validation with warning

### Version History This Session

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore, CLI restructure |
| v0.1.4 | MCP 2025-11-25 compliance (Origin, Protocol-Version headers) |
| v0.1.5 | GET /mcp SSE stream support, full spec compliance |
| v0.1.6 | Phase B audit fixes, lru vulnerability fix, dependency updates |
| v0.1.7 | Phase C audit fixes (error handling, graceful shutdown, byte limits) |
| v0.1.8 | (pending) HNSW index for O(log n) search + P0 audit fixes |

## Features Complete

### Core
- Semantic code search (5 languages)
- GPU acceleration (CUDA) with CPU fallback
- .gitignore support
- Watch mode with debounce
- Connection pooling (r2d2-sqlite, 4 concurrent connections)
- Query embedding cache (LRU, 100 entries)
- Graceful HTTP shutdown (Ctrl+C)
- **HNSW index for O(log n) search** (pending commit)

### MCP
- stdio transport (default)
- HTTP transport (Streamable HTTP 2025-11-25)
- Tools: cqs_search, cqs_stats

### Security
- SQL parameterized queries
- Secure UUID generation
- Request body limit (1MB)
- Chunk byte limit (100KB)
- Query length validation (8KB max)
- Lock poisoning recovery

## HNSW Design Decisions

- **Crate**: hnsw_rs v0.3.3 (pure Rust, no C++ deps)
- **Parameters**: M=24, max_layer=16, ef_construction=200, ef_search=100
- **Persistence**: .cq/index.hnsw.{graph,data,ids}
- **Search fallback**: Uses brute-force when filters active or HNSW unavailable
- **Limitation**: No delete support (rebuild required for removals)

## Next Steps

1. **Commit v0.1.8** - HNSW + P0 fixes
2. **Publish and release**
3. **Optimize filtered search** - use HNSW candidates with Store filtering

## Blockers

None.
