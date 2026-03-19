# Project Continuity

## Right Now

**SQ-9 complete + audit P1-P4 done (2026-03-19).** Notes removed from search, 769→768-dim, schema v15. See ROADMAP.md "v1.1.0 Release Plan" for remaining work.

### Done this session (2026-03-18/19)
- Full 14-category audit: 99 findings, 88 unique after consolidation
- P1: 15/15 fixed (PR #614)
- P2: 17/17 fixed (PR #615)
- P3: 31/39 fixed (PR #616), 7 deferred, 1 wontfix
- P4 test coverage: 49 new tests (PR #617), 1706 total
- Search defaults to project-only, `--include-refs` for cross-index (PR #618)
- Comprehensive docs overhaul (PR #619)
- SQ-9 Phase 1a dispatched (in progress)

### Work Order (see ROADMAP.md for details)
1. ~~SQ-9~~ ✅ DONE
2. P3 deferred (EX-6/7, CQ-13, PERF-11/13/16)
3. P4 refactors (search.rs split, enrichment/ORT extraction, PERF-12, CQ-11, EX-8)
4. Release v1.1.0

### Branch
`sq9-notes-simplification` — Phase 1a agent running

## Pending Changes

ROADMAP.md has v1.1.0 release plan (uncommitted on current branch).

## Parked

- **SQ-3: Code-specific embedding model** — UniXcoder, CodeBERT
- **SQ-7: LoRA fine-tune E5 on A6000** — Training data: hard eval + holdout + synthetic pairs
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

- Version: 1.0.13 (v1.1.0 in progress)
- MSRV: 1.93
- Schema: v15 (768-dim, notes embedding deprecated)
- Embeddings: 768-dim (E5-base-v2, sentiment dimension removed in SQ-9)
- HNSW index: chunks only
- 51 languages, 16 ChunkType variants
- Tests: 1706 pass (with gpu-index)
- ORT: 1.24.2 (ort crate 2.0.0-rc.12)
- Error types: ProjectError, LlmError, ConfigError (thiserror in library, anyhow in CLI)
- Search: project-only by default, --include-refs for references
- CUDA: 13 (cuVS) + 12 (ORT)
- Release targets: Linux x86_64, macOS ARM64, Windows x86_64
