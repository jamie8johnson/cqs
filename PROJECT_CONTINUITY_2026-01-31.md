# cqs - Project Continuity

Updated: 2026-01-31T09:00Z

## Current State

**Phase 1 implementation complete. Published to crates.io as `cqs` v0.1.0.**

- All 6 modules implemented (~1800 lines)
- 21 tests passing (13 parser, 8 store)
- CLI working with all commands
- MCP server implemented with stdio transport
- Renamed from `cq` to `cqs` (original name taken on crates.io)

## Recent Changes (This Session)

- Completed rename from `cq` to `cqs` throughout codebase
- Published v0.1.0 to crates.io to claim name
- Set up crates.io API token (90 day expiration)
- Added `.env` support for credentials (gitignored)
- Updated CLAUDE.md with environment/credentials section

## What Works

- `cqs --help` - CLI responds correctly
- `cargo test` - 21 tests pass
- `cargo build` - compiles with only warnings
- Published to crates.io

## What Needs Testing

- `cqs init` - downloads model (~547MB)
- `cqs doctor` - checks setup
- `cqs index` - indexes a project
- `cqs "query"` - semantic search
- `cqs serve` - MCP server with Claude Code
- GPU detection and fallback
- Ctrl+C handling during index

## Blockers / Open Questions

None. Ready for integration testing.

## Next Steps

1. **Test with real model** - Run `cqs init` to download model
2. **Index a test project** - Verify embedding + storage works
3. **Test MCP** - Connect to Claude Code
4. **Write eval suite** - 10 queries per language
5. **Fix any issues found** - Then tag v0.1.1

## Decisions Made

- **Name**: `cqs` (cq was taken on crates.io)
- **Token storage**: Use `cargo login` (credentials in ~/.cargo/)
- **MCP tool names**: `cqs_search`, `cqs_stats`
