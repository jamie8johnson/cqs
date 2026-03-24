# Project Continuity

## Right Now

**v1.4.0 released (2026-03-24). Audit pending. Next: KeyDAC augmentation or contrastive summaries.**

### Final Model Results

| Eval | v7 | v7b | Base |
|------|-----|------|------|
| Hard eval (raw, 268 chunks) | 89.1% R@1 | 89.1% | 89.1% |
| CoIR overall (9 tasks) | **49.19** | 49.03 | 49.48 |
| CoIR CSN (6 langs) | **0.707** | 0.702 | 0.627 |
| Full pipeline (6,867 chunks) | 65.4% R@1 | — | — |

**v7 unbalanced (200k) is the best model.** Shipped in v1.3.1. v7b balanced didn't improve.

### Released in v1.4.0 (PRs #656-664)
- 5 issue fixes (--json alias, light runtime, chunks.rs split, batch cache invalidation)
- Extension ChunkType (Swift/ObjC/F#/Scala) + coverage gaps (7 langs)
- 4 file splits (llm/calls/handlers/scoring) + Constructor ChunkType (10 langs) + R/Lua improvements
- 5 dependabot bumps. Public docs reviewed (README, CONTRIBUTING, CLAUDE.md, SECURITY.md).

### Next experiments (prioritized)
1. **Contrastive discriminating summaries** — plan written at `docs/superpowers/plans/2026-03-24-contrastive-summaries.md`. ~1.5h implementation. Brute-force cosine neighbors, contrastive prompt.
2. **KeyDAC query augmentation** (free) — keyword-preserving training data augmentation
3. **KD-LoRA distillation** — CodeSage-large (1.3B) → E5-base (110M). ~12h on A6000.

### Next session
1. **Run `/audit all`** — 14-category code audit. Needs fresh context. Archive existing `docs/audit-findings.md` and `docs/audit-triage.md` as `*-v1.3.0.md` first.
2. **Execute KeyDAC augmentation** — plan at `docs/superpowers/plans/2026-03-24-keydac-augmentation.md`. ~1h code + 14-21h train.
3. **Execute contrastive summaries** — plan at `docs/superpowers/plans/2026-03-24-contrastive-summaries.md`. ~1.5h code (Rust, modifies `src/llm/summary.rs`).
4. **Unused import warning fix** — committed on main (post-release). One commit ahead of v1.4.0 tag.

### Pending Changes
- `pr_body.md` deleted (cleanup)
- One post-release commit: unused test import fix (4155610)

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
- Version: 1.4.0 (released, tagged, published to crates.io)
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR, 89.1% hard eval)
- ChunkType: 20 variants (Extension: 4 langs, Constructor: 10 langs)
- 4 large files split into submodules (llm, calls, handlers, scoring)
- store/chunks.rs also split (PR #656)
- Tests: 1867 pass
- Telemetry: CQS_TELEMETRY=1
