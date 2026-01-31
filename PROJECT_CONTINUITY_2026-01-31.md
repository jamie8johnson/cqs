# cqs - Project Continuity

Updated: 2026-01-31T18:30Z

## Current State

**Phase 3 complete. Ready for v0.1.3 release.**

- All modules implemented (~2800 lines)
- 21 tests passing (13 parser, 8 store)
- Published to crates.io as `cqs` v0.1.2
- GitHub repo public at github.com/jamie8johnson/cqs
- Build: 0 warnings

### New in v0.1.3

1. **.gitignore support** - `ignore` crate replaces `walkdir`
2. **Watch mode** - `cqs watch` with debounce, auto-reindex on file changes
3. **HTTP transport** - MCP Streamable HTTP spec 2025-03-26
4. **CLI restructured** - query as positional arg, flags work anywhere
5. **Compiler warnings fixed** - 0 warnings
6. **Checksum constants renamed** - MODEL_BLAKE3/TOKENIZER_BLAKE3

### Dependencies Added

- `ignore` 0.4 (replaces walkdir, adds .gitignore support)
- `notify` 6 (file watching)
- `axum` 0.7, `tower` 0.4, `tower-http` 0.5 (HTTP transport)

## This Session

### Phase 3 Implementation

All 7 items from plan completed:
1. Fixed 5 compiler warnings
2. Renamed checksum constants (SHA256 → BLAKE3)
3. Restructured CLI (trailing_var_arg removed)
4. Added .gitignore support (ignore crate)
5. Implemented watch mode
6. Implemented HTTP transport
7. Full MD file review and updates

### MCP Spec Update

Discovered SSE transport deprecated in MCP spec 2025-03-26. Implemented Streamable HTTP instead:
- Single `/mcp` endpoint
- POST for requests, GET for SSE stream (future)
- Session management via `Mcp-Session-Id` header

### Documentation Updates

- README.md: Added watch mode, HTTP transport, indexing sections
- ROADMAP.md: Phase 3 complete, Phase 4 current
- SECURITY.md: HTTP transport security notes
- PRIVACY.md: Updated MCP section
- CLAUDE.md: Added watch mode to workflow

## MCP Status

**Working.** Tools available:
- `cqs_search` - semantic search with filters, name_boost
- `cqs_stats` - index statistics

Transports:
- `stdio` - default, for Claude Code
- `http` - for web integrations (port 3000 default)

## Next Steps

1. Bump version to 0.1.3
2. Publish to crates.io
3. Phase 4: HNSW for scale (>50k chunks)

## Blockers

None.

## Decisions Made

- **SSE → HTTP**: MCP SSE transport deprecated, use Streamable HTTP
- **ignore crate**: Replaces walkdir, handles .gitignore automatically
- **localhost only**: HTTP transport binds to 127.0.0.1 for security
- **Flags anywhere**: Removed trailing_var_arg, query is positional
