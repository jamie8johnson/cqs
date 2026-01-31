# Roadmap

## Current Phase: 1 (MVP)

### Status: Design Complete, Implementation Ready

### Done

- [x] Design document (v0.13.0) - architecture, API, all implementations specified
- [x] Audits - 7 rounds, 0 critical/high issues remaining
- [ ] Parser - tree-sitter extraction, all 5 languages
- [ ] Embedder - ort + tokenizers, CUDA/CPU detection, model download
- [ ] Store - sqlite with WAL, BLOB embeddings, two-phase search
- [ ] CLI - init, doctor, index, query, stats, serve, --lang filter
- [ ] MCP - cq serve with stdio, cq_search + cq_stats tools
- [ ] Tests - unit tests, integration tests, eval suite (10 queries/lang)

### Exit Criteria

- `cargo install cq` works
- GPU used when available, CPU fallback works
- 8/10 eval queries return relevant result in top-5 per language
- Index survives Ctrl+C during indexing
- MCP works with Claude Code

## Phase 2: Polish

- More chunk types (classes, structs, interfaces)
- More languages (C, C++, Java, Ruby)
- Hybrid search (embedding + name match)
- Watch mode, stale file detection
- MCP extras: cq_similar, cq_index, progress notifications

## Phase 3: Integration

- `--context N` for surrounding code
- VS Code extension
- SSE transport for MCP

## Phase 4: Scale

- HNSW index for >50k chunks
- Incremental embedding updates
- Index sharing (team sync)
