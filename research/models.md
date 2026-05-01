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

**Caveat (added 2026-05-01):** the absolute numbers above were collected under a buggy eval matcher that required strict `(file, name, line_start)` for gold-chunk matching. After 5 days of audit-driven line-shift drift, ~38% of gold chunks were "invisible" to the matcher even when search returned them — see the next section. The relative comparison (linear-α vs RRF) probably holds, since both arms ate the same drift, but reproducing the experiment under the loosened matcher would tighten the conclusion. Open as a follow-up if the question becomes load-bearing again.

---

## 2026-05-01 — Eval matcher loosening: drop `line_start` from gold-chunk match key

**Issue:** discovered while running an EmbeddingGemma A/B against BGE-large. Test R@5 came in at 39.4% — exactly matching the 2026-04-30 SPLADE phase 2 baseline above, but ~24pp below the canonical 63.3% recorded in `PROJECT_CONTINUITY.md` and `ROADMAP.md` after PR #1109 (2026-04-25). Initial reaction was "the default slot must have something corrupt" or "EmbeddingGemma broke the search path." Both wrong.

**Symptom diagnosis:**

```
PROJECT_CONTINUITY.md (2026-04-25): BGE-large test R@5 = 63.3%, dev R@5 = 74.3%
SPLADE phase 2 baseline (2026-04-30):                test R@5 = 39.4%, dev R@5 = 49.5%
EmbeddingGemma A/B re-run (2026-05-01):              test R@5 = 39.4%, dev R@5 = 49.5%
```

The same code, same fixture, same model — different R@5 across 5 days. The 2026-04-30 numbers were already regressed by the time SPLADE phase 2 ran; nobody noticed because the *relative* comparison (linear-α vs RRF) was internally consistent and that was the question being asked.

**Root cause:** `eval/runner.rs:355-358` matched gold chunks on strict `(file, name, line_start)`. `line_start` is fragile — every audit wave shifts function definitions up or down by a few lines as code moves. After v1.30.0 (147 fixes) + v1.30.1 (91 fixes) + v1.30.2 + v1.31.0 + v1.32.0 lines were shifted in a meaningful fraction of the corpus.

Counted directly against the v3.v2 fixture and the current default-slot index:

```
test split:
  strict match (file+name+line_start):  65/109 (59.6%)
  drifted line_start (file+name still match): 42/109 (38.5%)
  absent (file or name renamed/removed):       2/109  (1.8%)

dev split:
  strict match:    66/109 (60.6%)
  drifted:         40/109 (36.7%)
  absent:           3/109  (2.8%)
```

For ~38% of queries the search engine could return the correct chunk and the matcher would still count it as a miss because the chunk was at line 125 instead of 121. PR #1109 (2026-04-25) fixed this exact symptom with a one-shot fixture re-pin. 5 days of audit work re-introduced it. **The fix-by-data approach is a treadmill; the fix-by-code approach is one-shot.**

**Change:** drop `line_start` from the matcher (`eval/runner.rs`):

```rust
// Before:
if file_str == target_file
    && sr.chunk.name == gold.name
    && sr.chunk.line_start == gold.line_start  // ← removed
{
    return Ok(Some(i + 1));
}

// After:
if file_str == target_file && sr.chunk.name == gold.name {
    return Ok(Some(i + 1));
}
```

Where multiple chunks share `(file, name)` (overloaded names, sub-chunks of windowed sections like CHANGELOG headings), the first ranked match wins. That's the most generous interpretation of "did search find this," which is exactly what R@K is asking.

**Re-eval against the corrected matcher (4-way model A/B):**

| split | metric | BGE-large | EmbeddingGemma+summ | v9-200k | nomic-coderank |
|-------|--------|----------:|--------------------:|--------:|---------------:|
| test  | R@1    | 43.1%     | **45.9%**           | **45.9%** | 42.2%        |
| test  | R@5    | 69.7%     | 67.9%               | **70.6%** | 67.9%        |
| test  | R@20   | **83.5%** | 82.6%               | 80.7%   | 79.8%          |
| dev   | R@1    | 45.9%     | **47.7%**           | 46.8%   | **47.7%**      |
| dev   | R@5    | **77.1%** | 71.6%               | 68.8%   | 69.7%          |
| dev   | R@20   | **86.2%** | 85.3%               | 81.7%   | 81.7%          |

(All runs: `--no-require-fresh`, `CQS_NO_DAEMON=1`, `CQS_CAGRA_THRESHOLD=999999` for HNSW parity, n=20 limit. Bold = best per row. Eval JSONs saved at `/tmp/eval-{bge,gemma,v9,coderank}-{test,dev}-loose.json`.)

**Was nomic-coderank affected by the matcher bug?** No — the canonical CodeRankEmbed numbers in `PROJECT_CONTINUITY.md` (test R@5=67.0%, dev R@5=69.7%, recorded 2026-04-25 in PR #1110) were collected the *same day* as PR #1109's fixture re-pin, before any drift had accumulated. Today's loosened-matcher numbers (test R@5=67.9%, dev R@5=69.7%) reproduce those within ~1pp. The verdict at the time (CodeRankEmbed beats BGE on test R@5, loses on dev R@5, ships as opt-in) holds. The matcher bug bites evals run *after* several audit waves accumulate; coderank was lucky on timing.

**Takeaway 1 — v9-200k un-retired.** The 2026-04-25 verdict was "30pp behind, retire" but it turns out to be ~95% fixture artifact. v9-200k actually marginally beats BGE-large on test R@5 (70.6% vs 69.7%) and trails by 8.3pp on dev R@5 (68.8% vs 77.1%). For 1/3 the dim, 1/3 the params, and already fine-tuned on cqs's own call-graph data, that's a strong showing. Decision (per ROADMAP entry): keep BGE-large as default for now (dev R@5 hedge against unknown query types, broader pre-training base) but un-retire v9-200k as an opt-in preset.

**Takeaway 2 — EmbeddingGemma is competitive but doesn't dethrone BGE.** Edges out on R@1 (likely the projection-head's task-aware pooling helping top-1 precision) but loses on R@5 and R@20 across both splits. Summary-pass enrichment didn't materially shift the numbers (bare Gemma was already at test R@5=42.2% in the broken-matcher run — same as with-summaries). Keep EmbeddingGemma as an opt-in preset; not the right default for cqs's query mix.

**Takeaway 3 — the lesson the user pushed back on.** "If a benchmark number drops by 25pp overnight, that's bug-shaped, not model-shaped." The April 25 fixture-line-drift post-mortem (in PROJECT_CONTINUITY.md tears) was specifically about this exact failure pattern. We hit the same drift 5 days later and almost retired a viable model. The harness fix lands now so future sessions don't repeat it.

**Two related correctness bugs surfaced during the same investigation:**
- **#1281** — pipeline `total_calls` over-counts when the final flush of deferred chunk_calls fails FK validation. Cosmetic-but-misleading: the success log claims more graph rows than landed. Fix included in this PR.
- **#1282** — `cqs index` rebuilds HNSW but never invalidates `index.cagra`, which is selected over HNSW at chunk_count ≥ CQS_CAGRA_THRESHOLD; a stale `index.cagra` from a prior run silently returns pre-rebuild results. Fix (delete-on-rebuild) included in this PR.
- **#1283** — chunks table accumulates ~2% orphan rows because per-file delete-before-insert keys on literal chunk IDs, and chunker ID format has changed several times. Filed as separate issue; not fixed here.

**Closing:** matcher fix lands as PR #TBD. v9-200k un-retired in ROADMAP. EmbeddingGemma preset (Identity pooling + `CQS_DISABLE_TENSORRT` env knob for TRT-incompatible graphs) kept as a separate follow-up PR.
