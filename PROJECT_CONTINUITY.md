# Project Continuity

## Right Now

**v1.0.7 audit + fixes (2026-03-15).** Branch: `main` (uncommitted).

### Done this session
- v1.0.6 released (PR #588 SQ-2, PR #589)
- v1.0.7 released (PR #590 SQ-4, PR #591)
- Full 14-category audit running (Batch 1+2 complete, Batch 3 in progress)
- P1 fixes: EH-9 assert, EH-11 drain-after-write, CQ-12 tests, CQ-7 dead code, CQ-8 doc
- P2 fixes: AD-14 rename, EH-8 progress guard, EH-10 rows_affected, EH-12 expect, CQ-9 doc
- RB-B1: skip ambiguous names in enrichment (prevents `new`/`parse` caller merging)
- AC-B1/B2: comment fix, page_size→const
- DOC-1: CONTRIBUTING.md + vue.rs/aspx.rs
- AD-15: update_embeddings_batch doc comment

### Still needs
- Batch 3 findings (running)
- Commit all audit fixes
- PR and release as v1.0.8

## Pending Changes

Uncommitted audit fixes:
- `src/cli/pipeline.rs` — EH-8/9/11, RB-B1, AC-B1/B2, rename callee_caller_counts
- `src/store/chunks.rs` — CQ-7 dead code removed, EH-10, AD-15 doc
- `src/store/calls.rs` — AD-14 rename
- `src/nl.rs` — CQ-8 doc fix, CQ-9 stale comment, CQ-12 tests
- `src/lib.rs` — export updates
- `CONTRIBUTING.md` — DOC-1
- `docs/audit-findings.md`, `docs/audit-triage.md`

## Parked

- **SQ-1: Adaptive name_boost** — sweep proved ineffective. Dead end.
- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT, fine-tuned E5
- **SQ-4 tuning** — callers-only vs both, max_callers/max_callees sweep, cqs self-eval
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

- Version: 1.0.7
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only (notes use brute-force SQLite search)
- Multi-index: separate Store+HNSW per reference, parallel rayon search, blake3 dedup
- 51 languages
- 16 ChunkType variants
- Tests: 1562 pass, 0 failures
- CLI-only (MCP server removed in PR #352)
- Eval: E5-base-v2 81.8% R@1, 0.904 MRR on 143-query holdout eval
- CUDA: 13 (cuVS/rapidsai) + 12 (ORT CUDA provider) symlinked into conda lib dir
- NVIDIA env: CUDA 13.1, Driver 582.16, libcuvs 26.02, cuDNN 9.19.0
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- SQ-4: Two-pass enrichment — 63% of chunks enriched with call context
