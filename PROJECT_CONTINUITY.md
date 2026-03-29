# Project Continuity

## Right Now

**8th full audit (v1.9.0+) in progress. P1 fixes applied. OpenClaw contributions complete. (2026-03-29)**

### Audit v1.9.0+ — 88 findings, 14 categories
- Batch 1 (5 categories): 37 findings. Done.
- Batch 2 (5 categories): 27 findings. Done.
- Batch 3 (4 categories): 24 findings. Done.
- **P1 (12 items): ALL FIXED.** Compiles clean, 1490/1491 tests pass (1 failure under investigation).
- **P2 (15 items): Fix prompts generated, review agent running.**
- **P3 (39 items): Fix prompts generated, review pending.**
- **P4 (25 items): Issue descriptions generated. Trivial ones to be fixed inline.**
- Audit skill updated with prompt-generation + review steps (steps 8-9).
- Key P1 fixes: dim=0 panic guard, empty-name zero-vector guard, SQLite 999-param temp table, chunked-encoding size cap, dunce on CQS_ONNX_DIR, 9 stale E5-base/768 references.

### OpenClaw Contributions (tracking: `docs/openclaw-contributions.md`)
19 contributions (9 PRs, 9 issues, 1 comment). Six Greptile 5/5 scores.
OpenClaw clone at `/home/user001/openclaw`, fork at `/mnt/c/Projects/openclaw-fork`.

### Training
- v9-200k-1.5ep: ~55% complete, ~1.5h remaining on A6000.
- Future experiments added to ROADMAP + research_log: test-derived queries, contrastive training pairs, type-aware negatives, f64 cosine fix.

### Paper v0.6
Updated `~/training-data/paper/draft.md` — thesis: training signal quality > model capacity.

### Uncommitted (cqs repo — large diff)
- P1 audit fixes: hnsw/build.rs, nl/mod.rs, embedder/mod.rs, store/chunks/crud.rs, llm/batch.rs, store/migrations.rs, cli/definitions.rs, lib.rs, README.md, SECURITY.md
- Audit infrastructure: audit-findings.md, audit-triage.md, audit skill update
- Prior session carryover: pipeline_eval.rs, openclaw-contributions.md, openclaw-security-contribution.md
- ROADMAP.md (future experiments), PROJECT_CONTINUITY.md, notes.toml

### Pending
1. Fix 1 test failure from P1 changes
2. Apply P2 fixes after review
3. Apply P3 fixes
4. Fix trivial P4s, file issues for hard P4s
5. Commit + PR all audit fixes
6. Publish 500K/1M datasets to HF

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1
- Blackwell GPU upgrade

## Open Issues
- #389, #255, #106, #63 (upstream)
- #694-697, #700 (audit P4)
- #711 RT-RES-9

## Architecture
- Version: 1.9.0, BGE-large default (1024-dim)
- v9-200k LoRA: 94.5% pipeline, 70.9% raw (110M = 335M on pipeline)
- HF dataset: https://huggingface.co/datasets/jamie8johnson/cqs-code-search-200k
- OpenClaw PR: https://github.com/openclaw/openclaw/pull/56278
- Tests: 1491
