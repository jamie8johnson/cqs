# Project Continuity

## Right Now

**9th audit: 43/110 fixed, PR #737 CI running. (2026-03-31 17:30 CDT)**

Branch: `fix/v1.13-audit-p1-p3`

### Session summary
- v1.13.0 released (crates.io + GitHub) — 8 PRs merged (#728-736), 4 issues closed
- IEC 61131-3 Structured Text (52nd language) — grammar forked, extended, merged
- Paper v0.9 — thesis revised: enrichment dominates, model differences compress
- 13-model expanded eval (296q, 7 langs) — v5 ties top tier, GPU variance discovery
- v9-200k full CoIR: 45.02 (10 tasks), testq CSN: 0.622, all raw MRRs filled
- 9th audit: 110 findings across 15 categories, 43 fixed (all 15 P1s)
- Windowing artifact: MAX_TOKENS_PER_WINDOW now uses ModelConfig::max_seq_length
- Contrastive cap: 15K→30K + CQS_MAX_CONTRASTIVE_CHUNKS env override
- NL char budget: scales with CQS_MAX_SEQ_LENGTH env var
- 153GB disk freed (debug artifacts + unused HF models)
- Claude Code source indexed (19,648 chunks) — explored feature flags, memory system
- Audit skill updated: 2 batches of 8, TC split into happy/adversarial
- CLAUDE.md: "never suggest ending a session"

### PR #737 — audit fixes (CI running)
All P1s fixed (15): panics, correctness, scaling, security, docs
P2 fixes (6): NL budget, PERF-3 batch type edges, PB-1 lock, PERF-5 batch delete, PERF-4 HashMap
P3 fixes (22): spans, dead code, docs, migration idempotency, token_pack cap

### Remaining unfixed (67)
- 8 P2s: DS-38 (process lock), SEC-3 (ONNX symlink), PERF-2 (FTS batch), PERF-6 (clone), RM-5 (1.6GB alloc), CQ-2 (test-map duplication), RB-5 (ONNX shape panic), EX-3 (duplicate of SHL-1, already fixed)
- 36 P3s: mostly easy but low impact
- 23 P4s: hard refactors or cosmetic

### Key discoveries
- **Enrichment compresses model differences** — BGE-large 90.5%, v9-200k 90.2%, v5 89.5% all within GPU noise (~1.4pp) on 296-query eval
- **Windowing artifact** — MAX_TOKENS_PER_WINDOW=480 handicapped all large-context model evals (GTE-Qwen2, nomic, E5-mistral). Prior results invalid. Fixed.
- **v9-200k CoIR 45.02** — sharpest benchmark-product split: best pipeline, worst CoIR among non-KeyDAC LoRAs

### Next session
1. Merge PR #737 after CI
2. Fix remaining P2s (DS-38 process lock, SEC-3, PERF-2)
3. Re-eval GTE-Qwen2 and nomic with correct windowing
4. BGE-large full CoIR run
5. Fine-tune BGE-large on 200K CG-filtered data

### OpenClaw — 7 PRs, 6 issues
Tracker: `docs/openclaw-contributions.md`.

## Parked
- Dart language support
- hnswlib-rs migration
- DXF Phase 1 (P&ID → PLC function block mapping)
- Openclaw variant for PLC process control (long horizon)
- Blackwell GPU upgrade
- Publish 500K/1M datasets to HF

## Open Issues (cqs)
- #717 RM-40 (HNSW fully in RAM, no mmap)
- #389 (upstream cuVS CAGRA memory)
- #255, #106, #63 (upstream deps)

## Architecture
- Version: 1.13.0
- Languages: 52 (IEC 61131-3 ST)
- Presets: BGE-large (default, 1024d), E5-base (768d), v9-200k (768d)
- Metrics: 90.5% R@1 BGE-large, 90.2% v9-200k, 89.5% v5 (same-session, 296q)
- CoIR: v9-200k 45.02, v7 49.19, base E5 49.47
- Tests: ~1540
- Hooks: Pre-Edit (module context), Pre-Bash (git commit → cqs review)
- 9 audits, 43/110 findings fixed in latest
