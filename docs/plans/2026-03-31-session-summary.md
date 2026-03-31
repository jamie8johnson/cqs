# Session Summary — 2026-03-31

## Releases & PRs
- **v1.13.0 released** (crates.io + GitHub)
- **9 PRs merged** (#728-737), PR #738 in review, **4 issues** closed (#694, #695, #697, #718)
- IEC 61131-3 Structured Text shipped (52nd language)
- Grammar repo created: `jamie8johnson/tree-sitter-structured-text`

## Research
- **Paper v0.9** — thesis revised: enrichment compresses model differences above a quality threshold
- **13-model expanded eval** (296 queries, 7 languages) — BGE-large 90.5%, v9-200k 90.2%, v5 89.5-90.9% (varies across runs, ~1.4pp GPU non-determinism for LoRAs)
- **v5 discovery**: simple MNR (no CG filtering) ties the top tier on enriched pipeline. CG filtering still produces the best raw embeddings (70.9% vs 54.5%) and lowest variance. The 55-query eval exaggerated the enriched pipeline gap (5.4pp) — the 296-query eval shows ~1pp.
- **Windowing artifact**: MAX_TOKENS_PER_WINDOW=480 handicapped all large-context model evals. Fixed. Prior GTE-Qwen2/nomic/E5-mistral results suspected invalid — not yet re-tested.
- **v9-200k full CoIR**: 45.02 — sharpest benchmark-product split (best pipeline, worst CoIR among non-KeyDAC LoRAs)
- All metric gaps filled: raw MRR for 5 models, testq CSN (0.622)

## Audit
- **9th audit**: 132 findings across 16 categories (including new Research Extensibility)
- **43 fixes merged** (PR #737 — all 15 P1s + P2s + P3s), **4 more in review** (PR #738)
- Key fixes: 2 panics on user input, 3 correctness bugs, windowing scaling, contrastive cap 15K→30K, security permissions, flaky LLM test ENV_MUTEX, batch FTS (22K→batched SQL), ONNX symlink containment
- **22 research extensibility findings** — roadmap for transforming cqs into a research platform

## Infrastructure
- **153GB disk freed** (123GB debug artifacts + 30GB unused HF models)
- **Pre-commit hook**: `cqs review` on git commit (replaced Stop hook)
- **Audit skill updated**: 2 batches of 8, TC split into happy/adversarial
- **CLAUDE.md**: "never suggest ending a session"
- Claude Code source indexed (19,648 chunks) — explored feature flags, memory system, undercover mode

## Key Strategic Insight
The enrichment stack is the dominant contributor to pipeline quality. Above ~54% raw R@1, enrichment compresses top model differences to ~1pp — comparable to GPU non-determinism. CG filtering's value is in raw embedding quality (70.9%, +21.8pp over base) and lower variance, not enriched pipeline advantage over simpler training. The 55-query eval that drove the paper's thesis was a 3-query artifact.

## Next
- Phase 1-3 of the coordinated audit fix plan (12 agents: configurable constants → tests → shared serialization)
- Re-eval GTE-Qwen2 and nomic with correct windowing
- BGE-large CoIR run
- Research platform tier: A/B testing, enrichment ablation, per-query diagnostics
