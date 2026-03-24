# Project Continuity

## Right Now

**v1.4.1 released + contrastive summaries implemented (2026-03-24).**

### Contrastive Summaries (SQ-10)
- Brute-force cosine neighbors precomputed from embeddings during summary pass
- Top-3 nearest neighbors passed to LLM prompt: "This function is similar to but different from: X, Y, Z"
- Doc-comment shortcut removed — all callable chunks go through contrastive API path (~$0.38 one-time)
- FTS path filter bug fixed — `--path` glob now applies to FTS keyword results too
- Full-pipeline eval: **92.7% R@1, 96.3% R@5, 0.9478 NDCG@10** (55 queries, 5 languages)
- 2 remaining misses: TS helper-function confusion (genuine hard cases)

### Uncommitted changes
- Contrastive summary implementation (`src/llm/summary.rs`, `src/llm/prompts.rs`, `src/llm/batch.rs`)
- FTS path filter fix (`src/search/query.rs`)
- Improved eval script (`tests/full_pipeline_eval.sh` — path scoping, R@1/R@5/R@10/NDCG metrics)
- Audit skill update (mandatory `cqs health`/`cqs dead` first steps)

### Next experiments (prioritized)
1. **KeyDAC query augmentation** — plan at `docs/superpowers/plans/2026-03-24-keydac-augmentation.md`. ~1h code + 14-21h train.
2. **KD-LoRA distillation** — CodeSage-large (1.3B) → E5-base (110M). ~12h on A6000.

### Next session
1. **Merge contrastive summaries PR** if not merged yet
2. **Execute KeyDAC augmentation**
3. **Re-run hard eval** with contrastive summaries in fixture embeddings (requires adding summary injection to eval test harness)

## Parked
- Paper revision — after next training improvement
- Verified HF eval results — needs CoIR benchmark registration
- v7b epoch 2 — deprioritized (v7b didn't improve)

## Open Issues
- #665: RM-23 enrichment_pass ~105MB memory (deferred)
- #666: DS-17/DS-18 GC transaction windows (informational)
- #389: CAGRA memory retention (blocked on upstream cuVS)
- #255: Pre-built reference packages (enhancement)
- #106: ort pre-release RC
- #63: paste crate warning (monitoring)

## Architecture
- Version: 1.4.1 (released, tagged, published to crates.io)
- Current model: LoRA v7 (200k 9-lang, GIST+Matryoshka, 0.707 CSN, 49.19 CoIR, 89.1% hard eval raw)
- Full-pipeline: 92.7% R@1, 0.9478 NDCG@10 (with contrastive summaries + enrichment)
- ChunkType: 20 variants (Extension: 4 langs, Constructor: 10 langs)
- Tests: 1916 pass
- 5th full audit (v0.5.3, v0.12.3, v0.19.2, v1.0.13, v1.4.0)
