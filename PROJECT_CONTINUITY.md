# Project Continuity

## Right Now

**Audit cleanup batch in progress** (2026-02-08). Branch `fix/audit-cleanup-batch`. 6 issues done, not yet committed/PRed.

### Current batch — all code changes done, needs commit + PR
- #265: search_reference returns Result (was swallowing errors)
- #264: Config load_file returns Result (was silently ignoring parse errors)
- #241: Config validation — clamps limit/threshold/name_boost, Unix permission check
- #267: Module boundaries — 8 modules now pub(crate), re-exports for CLI
- #239: Test coverage — 13 new tests (Store::close, FTS edge cases, HNSW batch, C/Java fixtures)
- #232: CAGRA IndexRebuilder RAII guard (behind gpu-search feature)

### Build status
- 0 warnings, 0 clippy, 496 tests passing (default features)
- GPU build (`--features gpu-search`): needs env vars, currently testing
- Fresh-eyes review: done, 0 issues

### GPU build env vars (for `--features gpu-search`)
```bash
export CUDA_PATH=/usr/local/cuda
export CPATH=/usr/local/cuda/include
export LIBRARY_PATH=/home/user001/miniforge3/lib:/usr/local/cuda/lib64
export LD_LIBRARY_PATH=/home/user001/miniforge3/lib:/usr/local/cuda/lib64:$LD_LIBRARY_PATH
```
These are needed because: cuvs-sys uses bindgen (needs CPATH for headers), cmake (needs CUDA_PATH), and linking needs LIBRARY_PATH for libcuvs_c.so (conda) and libcudart.so.

### Recent merges
- PR #307: Language extensibility via define_languages! macro (#268)
- PR #306: v0.9.3 release
- PR #305: Fix gather/cross-project search to use RRF hybrid

### P4 audit items tracked in issues
- #300: Search/algorithm edge cases (5 items)
- #301: Observability gaps (5 items)
- #302: Test coverage gaps (4 items)
- #303: Polish/docs (3 items)

### Dev environment
- `~/.bashrc`: `LD_LIBRARY_PATH` for ort CUDA libs
- `~/.config/systemd/user/cqs-watch.service`: auto-starts `cqs watch` on WSL boot

## Parked

- **Phase 8**: Security (index encryption, rate limiting)
- **ref install** — deferred from Phase 6, tracked in #255
- **Relevance feedback** — deferred indefinitely (low impact)
- **`.cq` rename to `.cqs`** — breaking change needing migration, no issue filed yet

## Open Issues

### External/Waiting
- #106: ort stable (currently 2.0.0-rc.11)
- #63: paste dep (via tokenizers)

### Multi-index follow-ups
- #255: Pre-built reference packages
- #256: Cross-store dedup
- #257: Parallel search + shared Runtime

### Remaining audit items (v0.6.0 audit)
- #266: embedding_to_bytes should validate dimensions (P3)
- #269: Brute-force search loads all embeddings (P4)
- #270: HNSW LoadedHnsw unsafe transmute (P4)

### P4 Deferred (v0.5.1 audit, still open)
- #233: Cache parsed notes.toml in MCP server
- #236: HNSW-SQLite freshness validation
- #240: embedding_batches cursor pagination

## Architecture

- Version: 0.9.3
- Schema: v10
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, score-based merge with weight
- 7 languages (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- 271 lib + 225 integration tests (+ GPU tests behind gpu-search), 0 warnings, clippy clean
- MCP tools: 20 (note_only, summary, mermaid added as params in v0.9.2+)
- Source layout: parser/ and hnsw/ are now directories (split from monoliths in v0.9.0)
