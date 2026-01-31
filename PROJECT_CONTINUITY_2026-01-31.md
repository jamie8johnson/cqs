# cqs - Project Continuity

Updated: 2026-01-31T12:40Z

## Current State

**Phase 1 MVP functional. Full pipeline tested and working on CPU.**

- All 6 modules implemented (~1800 lines)
- 21 tests passing (13 parser, 8 store)
- End-to-end pipeline verified: init → index → search
- Published to crates.io as `cqs` v0.1.0
- GitHub repo public at github.com/jamie8johnson/cqs

## Recent Changes (This Session)

- Fixed embedder for nomic-embed-text-v1.5 ONNX model:
  - Changed i32 → i64 for input tensors
  - Added token_type_ids input (required by model)
  - Changed from sentence_embedding to last_hidden_state + mean pooling
- Tested full pipeline:
  - `cqs init` - downloads model, creates .cq/
  - `cqs index` - parsed 121 chunks from cqs codebase
  - `cqs "query"` - semantic search returns relevant results (0.65-0.73 similarity)
  - `cqs doctor` - all checks pass
- Investigated CUDA/GPU setup in WSL2:
  - cuDNN version mismatch (ort needs v9, Ubuntu had v8) - fixed
  - Installed NVIDIA CUDA repo, libcudnn9-cuda-12
  - WSL2 GPU visibility intermittent - documented in README
- Added GPU setup instructions to README
- Added SECURITY.md and PRIVACY.md

## What Works

- `cqs init` ✓
- `cqs doctor` ✓
- `cqs index` ✓ (121 chunks, ~2.5s on CPU)
- `cqs "query"` ✓ (semantic search working)
- `cqs stats` ✓
- `cqs --help` ✓
- CPU fallback ✓ (~20ms per embedding)

## What Needs Testing

- `cqs serve` - MCP server with Claude Code (next)
- Ctrl+C handling during index
- GPU acceleration (after WSL restart)

## Blockers / Open Questions

- WSL2 GPU visibility dropped during CUDA setup - restart may fix
- Need to test MCP integration with Claude Code

## Next Steps

1. **Test MCP** - Connect cqs serve to Claude Code
2. **Restart WSL** - Verify GPU comes back
3. **Write eval suite** - 10 queries per language
4. **Publish v0.1.1** - With embedder fixes

## Decisions Made

- **Name**: `cqs` (cq was taken on crates.io)
- **ONNX inputs**: Use i64, include token_type_ids
- **Pooling**: Mean pooling over last_hidden_state (model has no sentence_embedding output)
- **CUDA**: Document as optional, CPU works well enough for typical projects
