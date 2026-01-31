# cqs - Project Continuity

Updated: 2026-02-01T00:10Z

## Current State

**v0.1.8 published. Phase 4 (Scale) complete. Discoverability in progress.**

- ~3900 lines across 8 modules
- 32 tests passing
- v0.1.8 on crates.io
- GitHub release: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.8

## This Session

### Completed

1. **HNSW Implementation** (Phase 4)
   - Added src/hnsw.rs using hnsw_rs v0.3.3
   - O(log n) search for >50k chunks
   - Auto-build after indexing, disk persistence
   - Brute-force fallback for filtered searches

2. **P0 Audit Fixes**
   - RwLock poison recovery in HTTP handler
   - LRU cache poison recovery in embedder
   - Query length validation (8KB max)
   - Embedding byte validation with warning

3. **Published v0.1.8**
   - Merged PR #14 (HNSW + P0 fixes)
   - Published to crates.io
   - Created GitHub release

4. **Documentation Updates** (PR #15)
   - Updated README with Claude Code integration section
   - Added TL;DR and "Why use cqs?" section
   - Added CLAUDE.md example for projects
   - Updated Cargo.toml and GitHub repo descriptions
   - Added `--watch` note to CLAUDE.md for PR checks

5. **Discoverability**
   - Added GitHub topics: claude-code, mcp-server, semantic-search, code-search, embeddings, rust
   - PR to punkpeye/awesome-mcp-servers: https://github.com/punkpeye/awesome-mcp-servers/pull/1783
   - mcpservers.org submission pending (manual form)

### Version History

| Version | Changes |
|---------|---------|
| v0.1.3 | Watch mode, HTTP transport, .gitignore |
| v0.1.4 | MCP 2025-11-25 compliance |
| v0.1.5 | SSE stream support |
| v0.1.6 | Phase B audit fixes, dependency updates |
| v0.1.7 | Phase C audit fixes (error handling, graceful shutdown) |
| v0.1.8 | HNSW index + P0 audit fixes |

## Features Complete

- Semantic code search (5 languages: Rust, Python, TypeScript, JavaScript, Go)
- HNSW index for O(log n) search
- GPU acceleration (CUDA) with CPU fallback
- Watch mode with debounce
- MCP server (stdio + HTTP transport)
- Connection pooling, query caching
- Graceful shutdown, lock poisoning recovery

## Next Steps

1. **Await PR merge** - awesome-mcp-servers #1783
2. **Submit to mcpservers.org** - manual form at https://mcpservers.org/submit
3. **Consider MCP Registry** - once they support non-npm packages
4. **Future work** - filtered HNSW search, more languages, encryption

## Blockers

None.
