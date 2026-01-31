# cqs - Project Continuity

Updated: 2026-01-31T23:50Z

## Current State

**v0.1.8 published and released.**

- ~3900 lines across 8 modules (added hnsw.rs)
- 32 tests passing (13 parser + 8 store + 8 MCP + 3 HNSW)
- v0.1.8 published to crates.io
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.8
- CI workflow running - all passing

### Version History This Session

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore, CLI restructure |
| v0.1.4 | MCP 2025-11-25 compliance (Origin, Protocol-Version headers) |
| v0.1.5 | GET /mcp SSE stream support, full spec compliance |
| v0.1.6 | Phase B audit fixes, lru vulnerability fix, dependency updates |
| v0.1.7 | Phase C audit fixes (error handling, graceful shutdown, byte limits) |
| v0.1.8 | HNSW index for O(log n) search + P0 audit fixes |

## Features Complete

### Core
- Semantic code search (5 languages)
- GPU acceleration (CUDA) with CPU fallback
- .gitignore support
- Watch mode with debounce
- Connection pooling (r2d2-sqlite, 4 concurrent connections)
- Query embedding cache (LRU, 100 entries)
- Graceful HTTP shutdown (Ctrl+C)
- HNSW index for O(log n) search

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

1. **Optimize filtered search** - use HNSW candidates with Store filtering
2. **More languages** - C, C++, Java, Ruby
3. **More chunk types** - classes, structs, interfaces

## Blockers

None.
