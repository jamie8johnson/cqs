# Roadmap

## Current Phase: 1 (MVP)

### Done

- [ ] Parser - tree-sitter extraction, all 5 languages
- [ ] Embedder - ort + tokenizers, CUDA/CPU detection, model download
- [ ] Store - sqlite with WAL, BLOB embeddings, brute-force search
- [ ] CLI - init, doctor, index, query, stats, --lang filter
- [ ] Eval - 10 queries per language, measure recall

### Exit Criteria

- `cargo install cq` works
- GPU used when available, CPU fallback works
- 8/10 test queries return relevant results per language
- Index survives Ctrl+C during indexing

## Phase 2: Polish

- More chunk types (classes, structs, interfaces)
- More languages (C, C++, Java, Ruby)
- Path filtering
- Hybrid search (embedding + name match)
- Watch mode
- Stale file detection in doctor

## Phase 3: Integration

- MCP tool for Claude Code
- `--context N` for surrounding code
- VS Code extension

## Phase 4: Scale

- HNSW index for >50k chunks
- Incremental embedding updates
- Index sharing (team sync)
