# cqs - Project Continuity

Updated: 2026-01-31T19:30Z

## Current State

**v0.1.6 published. All audit Phase A+B merged. Dependencies updated. 29 tests passing.**

- ~3400 lines across 7 modules
- 29 tests passing (13 parser + 8 store + 8 MCP)
- Published v0.1.3 through v0.1.6 to crates.io
- GitHub repo: github.com/jamie8johnson/cqs
- All Dependabot PRs resolved
- CI workflow running (build, test, clippy, fmt) - all passing
- Branch ruleset active (main requires PR + CI, blocks force push)

### Version History This Session

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore, CLI restructure |
| v0.1.4 | MCP 2025-11-25 compliance (Origin, Protocol-Version headers) |
| v0.1.5 | GET /mcp SSE stream support, full spec compliance |
| v0.1.6 | Phase B audit fixes, lru vulnerability fix, dependency updates |

## Features Complete

### Core
- Semantic code search (5 languages)
- GPU acceleration (CUDA) with CPU fallback
- .gitignore support
- Watch mode with debounce
- Connection pooling (r2d2-sqlite, 4 concurrent connections)
- Query embedding cache (LRU, 100 entries)

### MCP
- stdio transport (default)
- HTTP transport (Streamable HTTP 2025-11-25)
  - POST /mcp - JSON-RPC requests
  - GET /mcp - SSE stream for server messages
  - Origin validation, request body limit (1MB)
- Tools: cqs_search, cqs_stats

### Security
- SQL parameterized queries
- Secure UUID generation (timestamp + random)
- Request body limit
- Branch protection enforced

## This Session Summary

1. **Audit Phase A** (PR #7 merged): SQL params, globset, fs4, MCP tests, community docs
2. **Audit Phase B** (PR #8 merged): Connection pooling, RwLock, UUID, rate limiting, LRU cache
3. **v0.1.6 published**: Phase B fixes + lru vulnerability fix (0.12→0.16)
4. **Dependencies updated** (PR #11): axum 0.8, tower-http 0.6, toml 0.9, tree-sitter-go 0.25
5. **Dependabot PRs closed** (#2, #3, #4, #5 superseded by #11)
6. **CLAUDE.md updated**: gh CLI workaround, branch protection workflow, gh pr checks quirk

## Next Steps

1. **Phase C audit remediation** - error handling, robustness items
2. **Pre-commit hook** - cargo fmt check
3. **Phase 4: HNSW** - scale to >50k chunks

## Blockers

None.
