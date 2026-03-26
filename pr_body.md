## Summary

- Embedder detects embedding dimension from ONNX model output at runtime via OnceLock
- `cosine_similarity` validates dimension agreement instead of hardcoded 768
- HNSW search removes hardcoded dimension check (library validates internally)
- `EMBEDDING_DIM` constant remains as default for E5-base-v2
- Batch size constants documented with SQLite 999-param formula (#683)

Closes #682, closes #683

## Test plan

- [x] `cargo clippy --features gpu-index -- -D warnings` — zero warnings
- [x] 67 embedding/cosine/hnsw/store tests pass
- [ ] CI validation

🤖 Generated with [Claude Code](https://claude.com/claude-code)
