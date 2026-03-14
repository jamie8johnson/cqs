# Project Continuity

## Right Now

**SQ-4: Call-graph-enriched embeddings (2026-03-14).** Branch: `main` (uncommitted).

### Goal
Two-pass indexing: after the main pipeline builds the call graph, re-embed each chunk with caller/callee context baked into the NL description for better search discrimination.

### Done this session
- v1.0.6 released (PR #588 SQ-2 NL enrichment, PR #589 version bump)
- Published to crates.io, GitHub release with binaries
- Scouted NL pipeline, embedding flow, index build, call graph storage
- Implemented SQ-4 two-pass enrichment:
  - `nl.rs`: `CallContext` struct + `generate_nl_with_call_context()` — appends "Called by: X, Y" and "Calls: A, B" to base Compact NL, with IDF-based callee filtering (>10% = stopword)
  - `store/chunks.rs`: `update_embeddings_batch()` — lightweight embedding-only UPDATE (no FTS rebuild), `chunks_paged()` — cursor-based full-chunk iterator
  - `store/calls.rs`: `callee_document_frequencies()` — callee name → distinct caller count for IDF
  - `pipeline.rs`: `enrichment_pass()` — post-pipeline second pass, pages through all chunks, batch-fetches callers/callees, skips leaf nodes, re-embeds in batches of 64
  - `commands/index.rs`: wires enrichment pass after main pipeline, before HNSW build
- Fixed embedding dimension mismatch: `embed_documents()` returns 768-dim, store needs 769. Added `.with_sentiment(0.0)`.
- Fixed CUDA 12 runtime loading permanently: symlinked pip CUDA 12 libs into conda lib dir (already in binary rpath). Cargo `[env]` LD_LIBRARY_PATH doesn't work for `dlopen`.
- Tested end-to-end: 7233 chunks → 4584 enriched (63%) with call graph context

### Still needs
- Run holdout + stress eval to measure search quality impact
- Sweep: callers-only vs both, max_callers/max_callees tuning
- Commit and PR
- Consider shipping as v1.0.7 or v1.1.0

## Pending Changes

Uncommitted in working tree:
- `src/nl.rs` — `CallContext`, `generate_nl_with_call_context()`
- `src/lib.rs` — export new nl types
- `src/store/chunks.rs` — `update_embeddings_batch()`, `chunks_paged()`
- `src/store/calls.rs` — `callee_document_frequencies()`
- `src/cli/pipeline.rs` — `enrichment_pass()`, `flush_enrichment_batch()`
- `src/cli/mod.rs` — re-export `enrichment_pass`
- `src/cli/commands/index.rs` — wire enrichment pass into index command
- `.cargo/config.toml` — `[env]` LD_LIBRARY_PATH (also CUDA 12 symlinks in conda lib dir, not tracked)
- `PROJECT_CONTINUITY.md`

## Parked

- **SQ-1: Adaptive name_boost** — sweep proved ineffective. Dead end.
- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT, fine-tuned E5
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

- Version: 1.0.6
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 51 languages
- 16 ChunkType variants
- Tests: 1562 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Eval: E5-base-v2 87.3% R@1, 0.920 MRR on 55-query hard eval (enriched NL)
- CUDA: 13 (cuVS/rapidsai) + 12 (ORT CUDA provider) side by side, symlinked into conda lib dir
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02, cuDNN 9.19.0
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- SQ-4: Two-pass enrichment — 4584/7233 chunks enriched with call context (63%)
