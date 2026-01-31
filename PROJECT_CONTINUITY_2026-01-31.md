# cqs - Project Continuity

Updated: 2026-01-31T12:50Z

## Current State

**Phase 1 MVP functional. MCP configured, needs restart to test.**

- All 6 modules implemented (~1800 lines)
- 21 tests passing (13 parser, 8 store)
- End-to-end pipeline verified: init → index → search
- Published to crates.io as `cqs` v0.1.0
- GitHub repo public at github.com/jamie8johnson/cqs
- MCP server configured in Claude Code (needs restart to activate)

## What's Ready to Test After Restart

MCP server `cqs` is configured in `~/.claude.json` for project `/mnt/c/projects/cq`:
- Command: `/home/user001/.cargo-target/cq/debug/cqs serve`
- Tools: `cqs_search`, `cqs_stats`
- After restart, run `/mcp` to verify, then test search in conversation

## Recent Changes (This Session)

- Fixed embedder for nomic-embed-text-v1.5 ONNX model:
  - Changed i32 → i64 for input tensors
  - Added token_type_ids input (required by model)
  - Changed from sentence_embedding to last_hidden_state + mean pooling
- Tested full pipeline on CPU (~20ms per embedding):
  - `cqs init` ✓
  - `cqs index` ✓ (121 chunks)
  - `cqs "query"` ✓ (semantic search working)
  - `cqs doctor` ✓
- Investigated CUDA/GPU in WSL2:
  - Installed NVIDIA CUDA repo, cuDNN 9
  - GPU visibility intermittent in WSL2 - documented
  - CPU fallback works reliably
- Added MCP server config via `claude mcp add cqs`
- Added GPU setup instructions to README
- Added SECURITY.md and PRIVACY.md

## Files Changed

- `src/embedder.rs` - i64 inputs, token_type_ids, mean pooling
- `README.md` - GPU setup instructions
- `SECURITY.md` - new
- `PRIVACY.md` - new
- `.mcp.json` - project MCP config (also in ~/.claude.json)

## Next Steps (After Restart)

1. Run `/mcp` - verify cqs server is connected
2. Test `cqs_search` in conversation - semantic code search via MCP
3. Test `cqs_stats` - index statistics via MCP
4. If GPU needed, restart WSL to restore nvidia-smi
5. Write eval suite - 10 queries per language
6. Publish v0.1.1 with embedder fixes

## Blockers / Open Questions

- None - just need restart to test MCP

## Decisions Made

- **ONNX inputs**: Use i64, include token_type_ids
- **Pooling**: Mean pooling over last_hidden_state
- **CUDA**: Document as optional, CPU works well
- **MCP config**: Added via `claude mcp add` to project scope
