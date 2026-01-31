# cqs - Project Continuity

Updated: 2026-01-31T21:00Z

## Current State

**v0.1.7 published. Phase C audit complete. Pre-commit hook active. 29 tests passing.**

- ~3500 lines across 7 modules
- 29 tests passing (13 parser + 8 store + 8 MCP)
- Published v0.1.3 through v0.1.7 to crates.io
- GitHub repo: github.com/jamie8johnson/cqs
- CI workflow running (build, test, clippy, fmt) - all passing
- Branch ruleset active (main requires PR + CI, blocks force push)
- Pre-commit hook configured (cargo fmt check)

### Version History This Session

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore, CLI restructure |
| v0.1.4 | MCP 2025-11-25 compliance (Origin, Protocol-Version headers) |
| v0.1.5 | GET /mcp SSE stream support, full spec compliance |
| v0.1.6 | Phase B audit fixes, lru vulnerability fix, dependency updates |
| v0.1.7 | Phase C audit fixes (error handling, graceful shutdown, byte limits) |

## Features Complete

### Core
- Semantic code search (5 languages)
- GPU acceleration (CUDA) with CPU fallback
- .gitignore support
- Watch mode with debounce
- Connection pooling (r2d2-sqlite, 4 concurrent connections)
- Query embedding cache (LRU, 100 entries)
- Graceful HTTP shutdown (Ctrl+C)

### MCP
- stdio transport (default)
- HTTP transport (Streamable HTTP 2025-11-25)
  - POST /mcp - JSON-RPC requests
  - GET /mcp - SSE stream for server messages
  - Origin validation, request body limit (1MB)
  - Graceful shutdown on Ctrl+C
- Tools: cqs_search, cqs_stats

### Security
- SQL parameterized queries
- Secure UUID generation (timestamp + random)
- Request body limit (1MB)
- Branch protection enforced
- Chunk byte limit (100KB max)

## This Session Summary

1. **Audit Phase A** (PR #7 merged): SQL params, globset, fs4, MCP tests, community docs
2. **Audit Phase B** (PR #8 merged): Connection pooling, RwLock, UUID, rate limiting, LRU cache
3. **v0.1.6 published**: Phase B fixes + lru vulnerability fix (0.12→0.16)
4. **Dependencies updated** (PR #11): axum 0.8, tower-http 0.6, toml 0.9, tree-sitter-go 0.25
5. **Dependabot PRs closed** (#2, #3, #4, #5 superseded by #11)
6. **Pre-commit hook**: .githooks/pre-commit runs cargo fmt check
7. **Phase C audit fixes** (v0.1.7):
   - Removed Parser::default() panic
   - Added logging for silent DB errors in search
   - Clarified unwrap with .expect() in embedder
   - Added logging for parse errors in watch mode
   - Added 100KB byte limit for chunks (handles minified files)
   - Added graceful HTTP shutdown with Ctrl+C
   - Fixed protocol version constant consistency

## Next Steps

1. **Phase 4: HNSW** - scale to >50k chunks
2. **More languages** - C, C++, Java, Ruby

## Blockers

None.
