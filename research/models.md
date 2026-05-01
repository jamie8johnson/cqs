# Retrieval research log

A append-only log of retrieval experiments — what was tried, what the eval said, and whether the change shipped. Each entry is dated and tied to the issue / PR that triggered the experiment so the rationale stays attached to the diff (or the absence of one).

The format is loose on purpose. Add: hypothesis, change shape, eval setup, headline numbers, per-category notes, verdict, and the issue / PR link. Don't bury the verdict — it is the most-read line.

---

## 2026-05-01 — SPLADE fusion: RRF vs. linear-α blend

**Issue:** #1176 (phase 2 of #1130). Phase 1 (#1175) shipped `Store::rrf_fuse_n` so dense + FTS + sparse can plug into a single fusion call. The open question was whether SPLADE's dense + sparse blend (today linear-α with min-max normalization on the sparse leg, in `search_hybrid` at `src/search/query.rs:548-636`) should migrate to RRF on top of `rrf_fuse_n`.

**Change tested:** drop the linear-α + min-max blend on dense/sparse score maps and replace with `Self::rrf_fuse_n(&[&dense_ids, &sparse_ids], candidate_count)`. Same fusion primitive `search_filtered_with_index` already uses for semantic + FTS. Diff was ~80 lines net deletion (`splade_alpha`, the `alpha <= 0` P2.53 dampening, and the manual min-max normalization all collapse into the rank-only RRF path).

**Eval setup:**
- Fixture: `evals/queries/v3_test.v2.json` (109 queries, gating split) + `evals/queries/v3_dev.v2.json` (109 queries, advisory)
- Model: BAAI/bge-large-en-v1.5 (default, 1024-dim)
- Index: project default slot, schema v25, post-#1175
- Both runs back-to-back on the same index state, no enrichment between, `--no-require-fresh`
- Eval runner already engages SPLADE for classified queries via `enable_splade: true` (`src/cli/commands/eval/runner.rs:295`), so SPLADE participation rate is identical between A and B

**Headline (overall, N=109 each):**

| split | fusion    |   R@1 |   R@5 |  R@20 |
|-------|-----------|------:|------:|------:|
| test  | linear-α  | 22.0% | 39.4% | 49.5% |
| test  | RRF       | 22.9% | 38.5% | 47.7% |
| **Δ** |           | **+0.9pp** | **−0.9pp** | **−1.8pp** |
| dev   | linear-α  | 28.4% | 49.5% | 54.1% |
| dev   | RRF       | 24.8% | 45.0% | 56.0% |
| **Δ** |           | **−3.7pp** | **−4.6pp** | **+1.8pp** |

**Per-category test R@5 (sample of larger swings):**

| category          |   N | linear-α |   RRF |     Δ |
|-------------------|----:|---------:|------:|------:|
| structural_search |   8 |    25.0% | 37.5% | **+12.5pp** |
| conceptual_search |  13 |    15.4% | 23.1% | **+7.7pp**  |
| behavioral_search |  16 |    56.2% | 56.2% | 0pp |
| identifier_lookup |  18 |    66.7% | 66.7% | 0pp |
| cross_language    |  11 |    36.4% | 36.4% | 0pp |
| multi_step        |  14 |    50.0% | 42.9% | **−7.1pp**  |
| negation          |  16 |    25.0% | 18.8% | **−6.2pp**  |
| type_filtered     |  13 |    23.1% | 15.4% | **−7.7pp**  |

The redistribution shape is consistent across both splits: lexical-heavy categories (`type_filtered`, `negation`, `multi_step`) lose recall under RRF, while categories where dense and sparse disagree on the right answer (`structural_search`, `conceptual_search`) gain. R@1 in `cross_language` jumped 9.1% → 27.3% on test (+18.2pp) — RRF is rewarding overlap exactly where you want it for that class.

**Why linear-α leads on the precision metrics, mechanically:** the linear-α blend on a normalized sparse score in `[0, 1]` and a cosine in `[-1, 1]` lets a high-confidence dense match dominate any sparse vote. RRF only sees ranks, so a 0.95-cosine dense top-1 and a SPLADE top-1 with normalized sparse 0.001 contribute the same rank-1 weight. For categories where SPLADE's top-1 is noisy (the type-filtered and negation classes — small N of high-confidence sparse matches), RRF inherits that noise instead of the linear blend's score-weighted dampening.

**Gate evaluation:**
- ±3pp test R@5: −0.9pp ✓ (within tolerance)
- ±3pp test R@20: −1.8pp ✓ (within tolerance)
- Test gate: **passes** (within noise band)
- Dev R@5: −4.6pp (outside ±3pp tolerance, advisory only)

**Verdict — don't ship.** The test gate is a tie within the ±3pp noise floor, but linear-α is consistently ahead on R@1 and R@5 across both splits. The R@20 gain on dev (+1.8pp) is the only RRF win, and R@20 is the least-important precision tier for an agent-facing tool that reads top-5. The category redistribution is interesting but not directional — RRF helps where dense and sparse converge, hurts where they disagree noisily; the linear blend's score-weighted dampening turns out to encode useful confidence information that pure rank fusion drops on the floor.

The phase 1 primitive (`rrf_fuse_n`) stays — it is still the right shape for fusing more than two signals (FTS + dense + sparse + name + future). Phase 2 was a fusion-strategy question on the dense + sparse pair specifically; the answer is "linear-α with min-max normalization is the right blend for SPLADE on cqs queries, given the current model and corpus."

**Follow-ups left open:**
- The `alpha <= 0` "pure re-rank" path (the P2.53 dampening) keeps living in `query.rs` — that's fine. Anyone reading "but RRF would have removed it" will find this entry.
- If a future model changes the score-distribution shape (e.g. ColBERT-style late interaction with a different top-1 confidence profile), this conclusion may not hold — re-run the same A/B against that model.
- Dev R@1/R@5 dropping ~4-5pp under RRF deserves a second look: it could be split-specific structure rather than a general RRF weakness. Out of scope for #1176.

**Closing:** issue #1176 closed without merging the fusion swap. The branch `feat/splade-rrf-fusion` carried the change for the eval and is being thrown away. Eval JSONs at `/tmp/eval-{baseline,rrf}-{test,dev}.json` (local — not checked in; numbers above are the canonical record).
