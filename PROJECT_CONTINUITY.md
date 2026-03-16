# Project Continuity

## Right Now

**SQ-6: LLM-generated function summaries (2026-03-16).** Plan approved, about to implement.

Branch: not yet created
Plan: `/home/administrator/.claude/plans/cheeky-sauteeing-abelson.md`

### Done this session
- v1.0.11: RT-DATA-2/4/6 data integrity fixes + #555 where_to_add 43-language coverage
- v1.0.12: `cqs plan` command — 11 task-type templates with keyword classification
- Roadmap fresh-eyes review: stale versions, missing injections, red team section, #555 tracked
- Both releases include binaries (Linux, macOS, Windows)

### Key decisions made
- SQ-6 design: reqwest::blocking (not async — avoids nested tokio panic)
- Separate llm_summaries table keyed by content_hash (survives --force)
- Schema/store NOT feature-gated (cross-build index compat), only API code is gated
- Doc comment shortcut: skip API for documented functions (save 30-50%)
- Only callable chunks (Function/Method/Macro), min 50 chars, window_idx=0 only
- Enrichment pass loads summaries internally (no signature change)
- enrichment_hash extended to include summary (no new column)
- --llm-summaries is opt-in but noisy on failure (bail!, not warn)
- Existing cached summaries always used in enrichment, even without the flag

## Pending Changes

None.

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

- Version: 1.0.12
- MSRV: 1.93
- Schema: v13 (v14 pending for SQ-6)
- 769-dim embeddings (768 E5-base-v2 + 1 sentiment)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1090 lib pass
- SQ-4: Two-pass enrichment with idempotency hash (RT-DATA-2)
- SQ-5: Filename stems in NL (generic stems filtered)
- HNSW dirty flag for crash detection (RT-DATA-6)
- Notes lock file for concurrent write safety (RT-DATA-4)
- `cqs plan` command: 11 task-type templates
- CUDA: 13 (cuVS) + 12 (ORT) symlinked into conda lib dir
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
- Notes: 120 indexed
- Red team: 21+ protections verified, 10 findings fixed, 2 deferred
