# cqs - Project Continuity

Updated: 2026-01-31T05:05Z

## Current State

**Phase 1 MVP functional. Two fixes applied, restart to activate MCP.**

- All 6 modules implemented (~1800 lines)
- 21 tests passing (13 parser, 8 store)
- End-to-end pipeline verified: init → index → search
- Published to crates.io as `cqs` v0.1.0
- GitHub repo public at github.com/jamie8johnson/cqs
- Index has 121 chunks (100 Rust, 6 Python, 6 TypeScript, 5 Go, 4 JavaScript)
- CLI search works: `cqs "parse files"` returns relevant results (0.79 similarity)
- MCP server works when tested directly with correct args

## MCP Status

**Two fixes applied this session:**

1. **Logging to stderr** - ort was logging to stdout, polluting JSON-RPC stream. Fixed in `main.rs` with `tracing_subscriber::fmt().with_writer(std::io::stderr).init()`

2. **Config in ~/.claude.json** - Added `--project /mnt/c/projects/cq` to args. The config in `.mcp.json` wasn't being read; Claude Code uses `~/.claude.json`.

**After restart:**
- MCP tools should work via Claude Code interface
- Server returns clean JSON-RPC (tested manually)

## Recent Changes (This Session)

- Fixed ort logging going to stdout (polluted JSON-RPC) - now logs to stderr
- Fixed MCP config in `~/.claude.json` (not .mcp.json) to add `--project` arg
- Verified MCP server works when called directly: stats and search both return clean JSON

## Previous Session Changes

- Fixed embedder for nomic-embed-text-v1.5 ONNX model (i64 inputs, token_type_ids, mean pooling)
- Tested full pipeline on CPU (~20ms per embedding)
- Added MCP server config, GPU setup docs, SECURITY.md, PRIVACY.md

## Next Steps

1. **Restart Claude Code** - MCP tools will work after restart
2. Test `cqs_search` in conversation - semantic code search via MCP
3. Test `cqs_stats` - index statistics via MCP
4. Write eval suite - 10 queries per language
5. Publish v0.1.1 with all fixes

## Blockers / Open Questions

- None - restart will activate MCP

## Decisions Made

- **MCP project path**: Must be explicit in config since cwd is unpredictable
- **Logging to stderr**: Required for clean JSON-RPC over stdio
- **ONNX inputs**: Use i64, include token_type_ids
- **Pooling**: Mean pooling over last_hidden_state
- **CUDA**: Not working on this box, CPU fallback is fine
