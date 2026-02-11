# Project Continuity

## Right Now

**Releasing v0.11.0.** 2026-02-11.

Features since v0.10.2:
1. **Proactive hints** (PR #362) — `cqs explain` and `cqs read --focus` show caller/test counts.
2. **`cqs impact-diff`** (PR #362) — diff-aware impact analysis in one call.
3. **Table-aware Markdown chunking** (PR #361) — row-wise table splitting, `--expand` parent retrieval.
4. **Markdown RAG improvements** (PR #360) — richer embeddings, cross-doc links.
5. **Suppress ort warning** (PR #363) — filter benign ONNX Runtime log noise.

### Known limitations
- T-SQL triggers (`CREATE TRIGGER ON table AFTER INSERT`) not supported by grammar
- `type_map` field in LanguageDef is defined but never read (dead code)

## Parked

- **AVEVA docs reference testing** — 5662 chunks from 39 markdown files, 38 cross-referenced docs still missing. User converting more PDFs.
- **VB.NET language support** — parked, VS2005 project delayed
- **Post-index name matching** — follow-up PR for fuzzy cross-doc references
- **Phase 8**: Security (index encryption)
- **ref install** — deferred from Phase 6, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Feature
- #255: Pre-built reference packages

### Audit
- #270: HNSW LoadedHnsw uses unsafe transmute (upstream hnsw_rs)

## Architecture

- Version: 0.11.0
- MSRV: 1.93
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 9 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown)
- CLI-only (MCP server removed in PR #352)
- Source layout: parser/ and hnsw/ are directories (split from monoliths in v0.9.0)
- SQL grammar: tree-sitter-sequel-tsql v0.4.2 (crates.io)
- Build target: `~/.cargo-target/cq/` (Linux FS)
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02 (conda/rapidsai), cuDNN 9.19.0 (conda/conda-forge)
