# Project Continuity

## Right Now

**SQ-6 metrics testing (2026-03-16).** Batches API merged (#605), stress eval running.

### Done this session
- v1.0.11: RT-DATA-2/4/6 data integrity fixes + #555 where_to_add 43-language coverage
- v1.0.12: `cqs plan` command — 11 task-type templates
- v1.0.13: SQ-6 LLM summaries (schema v14, llm_summaries table)
- PR #605: Batches API + llm-summaries in default features (merged)
- Live test: 1465 summaries via batch ($0.59), 669 doc-comment, 2637 cached
- Fixture eval baseline: 85.5% R@1, 0.914 MRR (ceiling-bound)
- Stress eval running: baseline (no summaries) against cqs+Flask+Zod+Express+Chi

### Stress eval baseline (no summaries, 3727 chunks)
- R@1: 58.0%, MRR: 0.653, R@5: 73.4%
- Rust MRR: 0.000, Python: 0.457, TS: 0.925, JS: 0.888, Go: 0.948
- name_boost has no effect (0.0 = 0.2 = same results)

### Stress eval WITH summaries (3727 chunks, 143 queries)
- R@1: 55.9% (baseline 58.0%, **-2.1pp**)
- MRR: 0.630 (baseline 0.653, **-0.023**)
- Python MRR: 0.300 (baseline 0.457, **-0.157**)
- TS/JS improved slightly (+0.023/+0.055), Rust still 0.000
- **Verdict: SQ-6 doesn't help with E5-base-v2.** Model treats summaries same as prose.
- SQ-7 (LoRA) remains the real fix — teach model that summaries > prose.

### Still needs to happen
- Decision: keep SQ-6 available for SQ-7 or shelve?
- Docs review (README, CONTRIBUTING)
- Release decision

### Key decisions made
- Batches API over sequential calls (no RPM limit, 50% discount, Tier 1 50 RPM was too low)
- llm-summaries now default feature
- Deduplicate batch items by content_hash
- Doc comment shortcut: first sentence as summary, skip API
- enrichment_hash extended to include summary text (no new column)
- Enrichment pass loads summaries internally (no signature change)
- ChunkSummary extended with content_hash + window_idx fields

## Pending Changes

None (PR #605 merged, no release yet).

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-7: LoRA fine-tune E5 on A6000** — the real fix for code-vs-doc ranking
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

- Version: 1.0.13 (v1.0.14 pending after metrics)
- MSRV: 1.93
- Schema: v14 (llm_summaries table)
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1095 lib pass
- SQ-6: LLM summaries via Claude Batches API, cached by content_hash
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 122 indexed
- Red team: 21+ protections verified, 10 findings fixed, 2 deferred
