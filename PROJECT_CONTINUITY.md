# Project Continuity

## Right Now

**v1.4.0 audit complete (2026-03-24). 70/74 findings fixed. Ready to merge.**

### Audit Results (v1.4.0)
- 14-category audit, 3 batches, 74 unique findings
- P1: 10/10 fixed (crashes, security, data loss)
- P2: 15/15 fixed (correctness, performance, duplication)
- P3: 37/37 fixed (docs, derives, spans, tests, small improvements)
- P4: 8/12 fixed, 4 deferred (2 informational, 2 upstream-blocked)
- Tests: 1916 pass (up from 1095 pre-audit), 0 failures, 0 warnings
- Key fixes: Windows path separator bug, UTF-8 panic, API key exfiltration warning, LLM batch dedup (-215 lines), HNSW rollback safety, BFS batching, 29 new tests

### Uncommitted changes
All audit fixes on `main` working tree — needs branch + PR + merge.

### Next experiments (prioritized)
1. **Contrastive discriminating summaries** — plan at `docs/superpowers/plans/2026-03-24-contrastive-summaries.md`. ~1.5h implementation.
2. **KeyDAC query augmentation** (free) — keyword-preserving training data augmentation
3. **KD-LoRA distillation** — CodeSage-large (1.3B) → E5-base (110M). ~12h on A6000.

### Next session
1. **Merge audit PR** if not merged yet
2. **Execute contrastive summaries** — Rust changes in `src/llm/summary.rs`
3. **Execute KeyDAC augmentation** — plan at `docs/superpowers/plans/2026-03-24-keydac-augmentation.md`

## Parked
- Paper revision — after next training improvement
- Verified HF eval results — needs CoIR benchmark registration
- v7b epoch 2 — deprioritized (v7b didn't improve)
- Full-pipeline hard eval with doc comments — costs API credits

## Open Issues
- #665: RM-23 enrichment_pass ~105MB memory (deferred)
- #666: DS-17/DS-18 GC transaction windows (informational)
- #389: CAGRA memory retention (blocked on upstream cuVS)
- #255: Pre-built reference packages (enhancement)
- #106: ort pre-release RC
- #63: paste crate warning (monitoring)

## Architecture
- Version: 1.4.0 (released, tagged, published to crates.io)
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR, 89.1% hard eval)
- ChunkType: 20 variants (Extension: 4 langs, Constructor: 10 langs)
- Tests: 1916 pass (post-audit)
- 5th full audit (v0.5.3, v0.12.3, v0.19.2, v1.0.13, v1.4.0)
