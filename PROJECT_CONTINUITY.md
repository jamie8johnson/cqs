# Project Continuity

## Right Now

**v1.0.10 released (2026-03-15).** Clean main, nothing in progress.

### Done this session
- v1.0.6: SQ-2 NL enrichment + eval infrastructure
- v1.0.7: SQ-4 call-graph-enriched embeddings (two-pass, IDF callee filtering)
- v1.0.8: 14-category audit fixes (14 findings resolved)
- v1.0.9: SQ-5 module-level context (filename stems with generic filter)
- v1.0.10: Red team security fixes (HNSW ID desync, PDF script injection, path traversal)
- Full 14-category audit + 4-category red team audit
- Notes groomed (117→119), docs reviewed (all current)
- 89GB build artifacts cleaned
- CUDA 12 permanently fixed (symlinks into conda lib dir)

### Key decisions made
- Fixture eval (143 queries) overfits to test file naming — real-codebase performance matters more
- Markdown ranking above code is a model quality issue — SQ-7 (LoRA) is the real fix, not truncating docs
- SQ-5 filename stems regress fixture eval by ~3pp but help real queries — shipped anyway
- SQ-1 (name_boost), SQ-3 (code model swap) are dead ends
- No standard code search benchmark exists for repo-aware search

## Pending Changes

None (notes.toml edited but not git-tracked).

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-6: LLM-generated summaries** — breaks local-only
- **SQ-7: LoRA fine-tune E5 on A6000** — the real fix for code-vs-doc ranking. Training data: hard eval + holdout + synthetic. Upload merged ONNX to HuggingFace.
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

### Red Team (unfixed, accepted/deferred)
- RT-DATA-2: Enrichment no idempotency marker (medium — needs schema change)
- RT-DATA-3: HNSW orphan accumulation in watch mode (medium — no deletion API)
- RT-DATA-5: Batch OnceLock stale cache (medium — by design, restart to refresh)
- RT-DATA-6: SQLite/HNSW crash desync (medium — needs generation counter)
- RT-DATA-4: Notes file lock vs rename race (low)

## Architecture

- Version: 1.0.10
- MSRV: 1.93
- Schema: v12
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1096 lib pass
- SQ-4: Two-pass enrichment, ~2259 chunks enriched, ambiguous names skipped
- SQ-5: Filename stems in NL (generic stems filtered)
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 119 indexed
- Red team: 21+ protections verified, 7 findings fixed
