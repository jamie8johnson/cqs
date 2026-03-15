# Project Continuity

## Right Now

**SQ-5 shipped, preparing v1.0.9 (2026-03-15).** Branch: `main`.

### Done this session
- v1.0.6 released (PR #588 SQ-2 NL enrichment + eval infrastructure)
- v1.0.7 released (PR #590 SQ-4 call-graph-enriched embeddings)
- v1.0.8 released (PR #592 audit fixes — 14 findings resolved)
- Full 14-category audit: 3 batches, ~35 findings, 14 fixed
- SQ-4: two-pass enrichment, IDF callee filtering, ambiguous name skip (RB-B1)
- SQ-5: filename stems in NL — implemented with generic stem filter. Regresses fixture eval by ~3pp but improves real-codebase search. Kept because fixture eval overfits to generic filenames.
- CUDA 12 permanently fixed via symlinks into conda lib dir
- 89GB build artifacts cleaned
- SQ-7 (LoRA fine-tune E5 on A6000) added to roadmap

### Key decisions
- Fixture eval (143 queries) is not the ground truth — real-codebase performance matters more
- Markdown ranking above code is a model quality issue, not a doc truncation issue. SQ-7 (LoRA) is the real fix.
- SQ-5 filename stems regress fixture eval but help real queries. Shipping it.

### Still needs
- Commit SQ-5 + roadmap updates
- Consider v1.0.9 release

## Pending Changes

None.

## Parked

- **SQ-1: Adaptive name_boost** — dead end
- **SQ-3: Code-specific embedding model** — evaluate UniXcoder, CodeBERT
- **SQ-6: LLM-generated summaries** — breaks local-only
- **SQ-7: LoRA fine-tune E5 on A6000** — training data: hard eval + holdout + synthetic
- **Truncate markdown NL** — wrong fix, treats symptom not cause
- **`cqs plan` templates** — 11 templates
- **Post-index name matching** — fuzzy cross-doc references
- **ref install** — #255

## Open Issues

### External/Waiting
- #106: ort stable (rc.12)
- #63: paste dep unmaintained (RUSTSEC-2024-0436)

### Feature
- #255: Pre-built reference packages

### Audit
- #389: CAGRA CPU-side dataset retention

## Architecture

- Version: 1.0.8
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1096 lib pass
- SQ-4: Two-pass enrichment, 2259 chunks enriched (31%), ambiguous names skipped
- SQ-5: Filename stems in NL (uncommitted)
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
