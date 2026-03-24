# Project Continuity

## Right Now

**All evals and refactoring complete (2026-03-24). Contrastive summaries plan written. Ready for next experiment.**

### Final Model Results

| Eval | v7 | v7b | Base |
|------|-----|------|------|
| Hard eval (raw, 268 chunks) | 89.1% R@1 | 89.1% | 89.1% |
| CoIR overall (9 tasks) | **49.19** | 49.03 | 49.48 |
| CoIR CSN (6 langs) | **0.707** | 0.702 | 0.627 |
| Full pipeline (6,867 chunks) | 65.4% R@1 | — | — |

**v7 unbalanced (200k) is the best model.** Shipped in v1.3.1. v7b balanced didn't improve.

### What's merged since v1.3.1
- PR #656: 5 issue fixes (--json alias, light runtime, chunks.rs split, batch cache invalidation)
- PR #662: Extension ChunkType (Swift/ObjC/F#/Scala) + coverage gaps (7 langs)
- PR #663: 4 file splits (llm/calls/handlers/scoring) + Constructor ChunkType (10 langs) + R/Lua improvements
- 5 dependabot PRs (#657-661)

### Next experiments (prioritized)
1. **Contrastive discriminating summaries** — plan written at `docs/superpowers/plans/2026-03-24-contrastive-summaries.md`. ~1.5h implementation. Brute-force cosine neighbors, contrastive prompt.
2. **KeyDAC query augmentation** (free) — keyword-preserving training data augmentation
3. **KD-LoRA distillation** — CodeSage-large (1.3B) → E5-base (110M). ~12h on A6000.

### Pending Changes
- `ROADMAP.md` — updated with ChunkType status, refactoring done, literature survey refs
- `docs/superpowers/plans/2026-03-24-contrastive-summaries.md` — new plan
- `tests/full_pipeline_eval.sh` — new eval script

## Parked
- Paper revision — after next training improvement
- Verified HF eval results — needs CoIR benchmark registration
- v7b epoch 2 — deprioritized (v7b didn't improve)
- Full-pipeline hard eval with doc comments — costs API credits

## Open Issues
- #389: CAGRA memory retention (blocked on upstream cuVS)
- #255: Pre-built reference packages (enhancement)
- #106: ort pre-release RC
- #63: paste crate warning (monitoring)

## Architecture
- Version: 1.4.0
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR, 89.1% hard eval)
- ChunkType: 20 variants (Extension: 4 langs, Constructor: 10 langs)
- 4 large files split into submodules (llm, calls, handlers, scoring)
- store/chunks.rs also split (PR #656)
- Tests: 1867 pass
- Telemetry: CQS_TELEMETRY=1
