# Project Continuity

## Right Now

**Clean main, moving to A6000 machine for SQ-7 (2026-03-17).** Build artifacts cleaned.

### Done this session (2026-03-16)
- v1.0.11: RT-DATA-2/4/6 data integrity fixes + #555 where_to_add 43 languages
- v1.0.12: `cqs plan` command — 11 task-type templates
- v1.0.13: SQ-6 LLM summaries (schema v14, Batches API, cached by content_hash)
- PR #605: Batches API + llm-summaries in default features
- PR #606: Stress eval A/B results

### SQ-6 eval results (CRITICAL for SQ-7 planning)
- Fixture eval baseline: 85.5% R@1, 0.914 MRR (ceiling-bound, no room)
- Stress eval baseline (no summaries, 3727 chunks): R@1 58.0%, MRR 0.653
- Stress eval WITH SQ-6 (3727 chunks): R@1 55.9%, MRR 0.630 (**-2.1pp, -0.023**)
- Python MRR: 0.457 → 0.300 (-0.157). TS/JS improved slightly.
- Rust MRR: 0.000 in both cases
- **Verdict: E5-base-v2 treats summaries same as prose. SQ-7 (LoRA) is the fix.**
- SQ-6 + SQ-7 expected to outperform SQ-7 alone (richer NL + trained discrimination)

### Key decisions made
- Dirty flag over generation counter for RT-DATA-6
- Batches API over sequential calls for SQ-6 (Tier 1 = 50 RPM, too low for sequential)
- Doc comment shortcut: skip API for documented functions
- enrichment_hash extended to include summary (no new column)
- llm-summaries is default feature now (reqwest always compiled)

## Pending Changes

None. Clean main.

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-7: LoRA fine-tune E5 on A6000** — NEXT. Training data: hard eval + holdout + synthetic pairs
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
- RT-DATA-3: HNSW orphan accumulation in watch mode (medium — no deletion API)
- RT-DATA-5: Batch OnceLock stale cache (medium — by design, restart to refresh)

## Architecture

- Version: 1.0.13
- MSRV: 1.93
- Schema: v14 (llm_summaries table)
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1095 lib pass
- SQ-6: LLM summaries via Claude Batches API, cached by content_hash
- `cqs plan` command: 11 task-type templates
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 122 indexed
- Red team: 21+ protections verified, 10 findings fixed, 2 deferred
