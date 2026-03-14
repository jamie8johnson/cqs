# Project Continuity

## Right Now

**SQ-2 + CUDA fixes (2026-03-14).** Branch: `main` (uncommitted).

### Done this session
- Cross-encoder reranking tested → dead end (ms-marco-MiniLM-L-6-v2 useless for code)
- `rerank_with_passages` method added to reranker
- 143-query holdout eval set built (eval_common.rs)
- Stress eval infrastructure (real codebases as noise: cqs, Flask, Express, Chi = 3970 chunks)
- name_boost sweep → dead end (near-zero effect at scale)
- NL enrichment: field names for structs/enums/classes + dir-only file context
  - Hard eval: 83.6→87.3% R@1 (+3.7pp)
  - Stress: 37.1→37.8% R@1, JS MRR +12.4pp
- CUDA 12+13 side-by-side: pip `nvidia-cublas-cu12`, `nvidia-cuda-runtime-cu12`, `nvidia-cufft-cu12`
- `.bashrc` updated with ORT_CUDA12_LIBS paths
- `.cargo/config.toml`: added `rustdocflags` — fixes doc test linker errors
- All doc tests pass (8/8), all unit tests pass (190), all integration tests pass
- SQ-1 through SQ-4 on ROADMAP.md
- Dead code warning fixed (make.rs `calls` → `_calls`)

### Still needs
- Commit all changes
- Consider shipping as 1.0.6

## Pending Changes

Uncommitted in working tree:
- `src/nl.rs` — field names + file context enrichment for Compact template
- `src/reranker.rs` — `rerank_with_passages` method
- `src/store/helpers.rs` — `From<&ChunkSummary> for Chunk`
- `src/search.rs` — search changes
- `src/embedder.rs` — reverted CUDA guard (no longer needed)
- `src/language/make.rs` — dead code warning fix
- `tests/eval_common.rs` — 143 holdout eval cases
- `tests/pipeline_eval.rs` — holdout + stress eval tests
- `tests/model_eval.rs` — model eval additions
- `.cargo/config.toml` — rustdocflags for doc tests
- `~/.bashrc` — CUDA 12 lib paths for ORT
- `ROADMAP.md` — SQ-1 through SQ-4

## Parked

- **SQ-1: Adaptive name_boost** — sweep proved ineffective. Dead end.
- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT, fine-tuned E5
- **SQ-4: Call-graph-enriched embeddings** — two-pass index with caller/callee context
- **`cqs plan` templates** — 11 templates; add more as patterns emerge
- **Post-index name matching** — fuzzy cross-doc references
- **ref install** — deferred, tracked in #255

## Open Issues

### External/Waiting
- #106: ort stable (currently on rc.12, waiting for 2.0 stable)
- #63: paste dep unmaintained (RUSTSEC-2024-0436) — transitive via `tokenizers`

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA CPU-side dataset retention (~146MB at 50k chunks)

## Architecture

- Version: 1.0.5
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 51 languages
- 16 ChunkType variants
- Tests: 1534 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Eval: E5-base-v2 87.3% R@1, 0.920 MRR on 55-query hard eval (enriched NL)
- CUDA: 13 (cuVS/rapidsai) + 12 (ORT CUDA provider) side by side
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02, cuDNN 9.19.0
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
