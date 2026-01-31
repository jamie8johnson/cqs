# Roadmap

## Current Phase: 1 (MVP)

### Status: Complete

### Done

- [x] Design document (v0.13.0) - architecture, API, all implementations specified
- [x] Audits - 7 rounds, 0 critical/high issues remaining
- [x] Parser - tree-sitter extraction, all 5 languages (13 tests passing)
- [x] Embedder - ort + tokenizers, CUDA/CPU detection, model download
- [x] Store - sqlite with WAL, BLOB embeddings, two-phase search (8 tests passing)
- [x] CLI - init, doctor, index, query, stats, serve, --lang filter, --path filter
- [x] MCP - cqs serve with stdio, cqs_search + cqs_stats tools
- [x] Published to crates.io as `cqs` v0.1.0
- [x] End-to-end testing - init, index, search all working
- [x] MCP integration tested with Claude Code
- [x] Path pattern filtering fixed (relative paths)
- [x] Invalid language error handling
- [ ] Eval suite - 10 queries/lang, measure recall@5

### Exit Criteria

- [x] `cargo install cqs` works (published v0.1.0)
- [x] CPU fallback works (~20ms per embedding)
- [x] GPU works when available (CUDA tested)
- [ ] 8/10 eval queries return relevant result in top-5 per language
- [x] Index survives Ctrl+C during indexing
- [x] MCP works with Claude Code

## Phase 2: Polish

- More chunk types (classes, structs, interfaces)
- More languages (C, C++, Java, Ruby)
- Hybrid search (embedding + name match)
- Watch mode, stale file detection
- MCP extras: cqs_similar, cqs_index, progress notifications

## Phase 3: Integration

- `--context N` for surrounding code
- VS Code extension
- SSE transport for MCP

## Phase 4: Scale

- HNSW index for >50k chunks
- Incremental embedding updates
- Index sharing (team sync)
