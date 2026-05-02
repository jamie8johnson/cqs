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

**Caveat (added 2026-05-01):** the absolute numbers above were collected under a buggy eval matcher that required strict `(file, name, line_start)` for gold-chunk matching. After 5 days of audit-driven line-shift drift, ~38% of gold chunks were "invisible" to the matcher even when search returned them — see the matcher-loosen entry below. The relative comparison (linear-α vs RRF) was redone under the corrected matcher in the third entry below ("SPLADE phase 2 redo"); verdict is the same — linear-α wins — with even more decisive numbers (test R@5 gap grew from 0.9pp to 3.6pp; dev R@5 gap grew from 4.5pp to 6.4pp).

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

**v9-200k bare vs. v9-200k + summaries (2026-05-01 follow-up).** Cross-slot copied the 9,505 default-slot summaries to the v9 slot by content_hash, then ran `cqs index --slot v9 --force --llm-summaries` (which generated 2,333 fresh summaries via Anthropic Batches API to bring chunks_with_summary from 6,572 → 9,081, ~63% coverage). Eval against the corrected matcher:

| split | metric | v9-200k bare | v9-200k + LLM summaries | Δ |
|-------|--------|-------------:|------------------------:|--:|
| test  | R@1    | 45.9%        | 39.4%                   | -6.5 |
| test  | R@5    | 70.6%        | 69.7%                   | -0.9 |
| test  | R@20   | 80.7%        | 80.7%                   |  0.0 |
| dev   | R@1    | 46.8%        | 45.0%                   | -1.8 |
| dev   | R@5    | 68.8%        | 67.9%                   | -0.9 |
| dev   | R@20   | 81.7%        | 86.2%                   | +4.5 |

Summaries hurt v9-200k's strongest signal (test R@1 down 6.5pp), are a wash on R@5 across both splits, and only help on dev R@20. The original v9-200k retirement-note claim — "bare-vs-enriched gives identical numbers — summaries can't rescue what the dense channel doesn't surface" — was directionally right under the broken matcher and continues to hold under the corrected matcher (the small-pp shifts go in the *wrong* direction for the headline metrics, not toward summaries-rescuing-the-model).

This matches the EmbeddingGemma+summ result from earlier in this entry (bare and with-summaries both at test R@5=42.2%) — the dense channels of these code-specialist or code-tuned models already capture what summaries would add. BGE-large gets a small enrichment win because its pre-training distribution is broader and the summary text adds genuinely new signal. **Practical advice: pass `--llm-summaries` for BGE-large; skip it for v9-200k and EmbeddingGemma.** Eval JSONs at `/tmp/eval-v9-summ-{test,dev}.json`.

**Takeaway 1 — v9-200k un-retired.** The 2026-04-25 verdict was "30pp behind, retire" but it turns out to be ~95% fixture artifact. v9-200k actually marginally beats BGE-large on test R@5 (70.6% vs 69.7%) and trails by 8.3pp on dev R@5 (68.8% vs 77.1%). For 1/3 the dim, 1/3 the params, and already fine-tuned on cqs's own call-graph data, that's a strong showing. Decision (per ROADMAP entry): keep BGE-large as default for now (dev R@5 hedge against unknown query types, broader pre-training base) but un-retire v9-200k as an opt-in preset.

**Takeaway 2 — EmbeddingGemma is competitive but doesn't dethrone BGE.** Edges out on R@1 (likely the projection-head's task-aware pooling helping top-1 precision) but loses on R@5 and R@20 across both splits. Summary-pass enrichment didn't materially shift the numbers (bare Gemma was already at test R@5=42.2% in the broken-matcher run — same as with-summaries). Keep EmbeddingGemma as an opt-in preset; not the right default for cqs's query mix.

**Takeaway 3 — the lesson the user pushed back on.** "If a benchmark number drops by 25pp overnight, that's bug-shaped, not model-shaped." The April 25 fixture-line-drift post-mortem (in PROJECT_CONTINUITY.md tears) was specifically about this exact failure pattern. We hit the same drift 5 days later and almost retired a viable model. The harness fix lands now so future sessions don't repeat it.

**Two related correctness bugs surfaced during the same investigation:**
- **#1281** — pipeline `total_calls` over-counts when the final flush of deferred chunk_calls fails FK validation. Cosmetic-but-misleading: the success log claims more graph rows than landed. Fix included in this PR.
- **#1282** — `cqs index` rebuilds HNSW but never invalidates `index.cagra`, which is selected over HNSW at chunk_count ≥ CQS_CAGRA_THRESHOLD; a stale `index.cagra` from a prior run silently returns pre-rebuild results. Fix (delete-on-rebuild) included in this PR.
- **#1283** — chunks table accumulates ~2% orphan rows because per-file delete-before-insert keys on literal chunk IDs, and chunker ID format has changed several times. Filed as separate issue; not fixed here.

**Closing:** matcher fix lands as PR #TBD. v9-200k un-retired in ROADMAP. EmbeddingGemma preset (Identity pooling + `CQS_DISABLE_TENSORRT` env knob for TRT-incompatible graphs) kept as a separate follow-up PR.

---

## 2026-05-01 — SPLADE phase 2 redo: RRF vs. linear-α under the corrected matcher

**Issue:** redo of #1176 (SPLADE phase 2) under the eval matcher loosening from the previous entry. The original phase 2 was run on 2026-04-30 against an eval matcher that mis-counted ~38% of gold chunks as misses due to v1.30.x line-start drift. The relative comparison (linear-α vs RRF) probably held — both arms ate the same drift — but the user pushed back: "if a benchmark number drops by 25pp overnight, that's bug-shaped, not model-shaped" applies just as much to negative results. Re-run cleanly, confirm or refute, document.

**Change tested:** identical to phase 1 of #1176 — drop the linear-α + min-max blend on dense/sparse score maps in `search_hybrid` (`src/search/query.rs:548-636`) and replace with `Self::rrf_fuse_n(&[&dense_ids, &sparse_ids], candidate_count)`. Same primitive `search_filtered_with_index` already uses for semantic + FTS. ~80 lines net deletion (`splade_alpha`, the `alpha <= 0` P2.53 dampening, the manual min-max normalization all collapse into the rank-only RRF path).

**Eval setup:**
- Fixture: `evals/queries/v3_test.v2.json` (109, gating) + `evals/queries/v3_dev.v2.json` (109, advisory)
- Model: BAAI/bge-large-en-v1.5 (default, 1024-dim) — same default slot as the matcher-loosen entry above
- Index: schema v25, post-#1175, post-PR #1284 (matcher loosen + CAGRA delete-on-rebuild + total_calls fix already in branch)
- Both arms back-to-back on the same index state, no enrichment between, `--no-require-fresh`, `CQS_NO_DAEMON=1`, n=20 limit
- Matcher: loose `(file, name)` per the previous entry — gold-chunk drift can't contaminate the comparison

**Headline numbers:**

| split | fusion    |   R@1 |   R@5 |  R@20 |
|-------|-----------|------:|------:|------:|
| test  | linear-α  | 43.1% | **69.7%** | **83.5%** |
| test  | RRF       | **45.0%** | 66.1% | 78.0% |
|       | Δ         | +1.9  | **-3.6**  | **-5.5**  |
| dev   | linear-α  | **45.9%** | **75.2%** | **88.1%** |
| dev   | RRF       | 41.3% | 68.8% | 87.2% |
|       | Δ         | -4.6  | **-6.4**  | -0.9  |

(Bold = fusion winner per row. Eval JSONs at `/tmp/eval-splade-{linalpha,rrf}-{test,dev}.json`.)

**Comparison to the original phase 2 (broken-matcher) numbers:**

| metric         | original (broken matcher) | redo (corrected matcher) |
|----------------|--------------------------:|--------------------------:|
| test R@5 Δ     | -0.9pp (linear wins)      | -3.6pp (linear wins)      |
| dev R@5 Δ      | -4.5pp (linear wins)      | -6.4pp (linear wins)      |

The relative direction is unchanged — linear-α wins R@5 on both splits in both versions. **But the gap *grew* under the corrected matcher** (test 0.9→3.6, dev 4.5→6.4). The original write-up framed test R@5 as "tied within ±3pp tolerance" — under the corrected matcher it's clearly outside the noise floor on test, and was always outside on dev. Linear-α isn't just "slightly preferred"; it's the right call.

**Why linear-α's edge grew under the corrected matcher:** when 38% of gold chunks were mis-matched, both arms were credited with the same artificial misses and the ranking-level differences were attenuated by the high baseline noise. Cleaning the matcher exposed the true difference between fusion strategies.

**Per-category test R@5 delta (RRF minus linear-α, sample of larger swings):**

| category            | n  | linear-α R@5 | RRF R@5 | Δ      |
|---------------------|---:|-------------:|--------:|-------:|
| identifier_lookup   | 18 | 66.7%        | 50.0%   | -16.7  |
| type_filtered       | 13 | 69.2%        | 53.8%   | -15.4  |
| negation            | 16 | 56.2%        | 50.0%   | -6.2   |
| structural_search   |  8 | 75.0%        | 75.0%   |   0.0  |
| conceptual_search   | 13 | 76.9%        | 76.9%   |   0.0  |
| behavioral_search   | 16 | 75.0%        | 75.0%   |   0.0  |
| cross_language      | 11 | 81.8%        | 81.8%   |   0.0  |
| multi_step          | 14 | 64.3%        | 78.6%   | +14.3  |

(Hand-computed from `/tmp/eval-splade-{linalpha,rrf}-test.json` per-category sections; numbers above are the rounded eval output.)

The shape is consistent with the original phase 2 finding: lexical-heavy categories (`identifier_lookup`, `type_filtered`, `negation`) lose recall under RRF, while categories where dense and sparse converge gain or break even. **Multi-step** swings the other way under the corrected matcher (+14.3pp under RRF), which is interesting — multi-step queries combine multiple constraints, which is exactly where rank-fusion's "rewards overlap" property should help. But the negative magnitude on identifier_lookup + type_filtered swamps the gain.

**Verdict — same as original, with stronger evidence: don't ship RRF for SPLADE blending.** Linear-α with min-max normalization on the sparse leg keeps the score-weighted dampening that lets a high-confidence dense match dominate noisy sparse votes. Pure rank fusion drops that signal on the floor.

**Why the rank-only fusion loses on identifier_lookup (worst category):** identifier_lookup is exactly where SPLADE's top-1 sparse score is high-confidence (the literal token matches the chunk's name). Linear-α weights that signal by its normalized score, so a 0.95 dense + 0.99 sparse dominates a 0.05 dense + 0.001 sparse. RRF only sees ranks: a dense top-1 with cosine 0.05 contributes 1/61 = 0.0164, and a SPLADE top-1 with sparse 0.001 also contributes 1/61. The high-confidence SPLADE match doesn't get to swamp the weak dense one in RRF. That's the exact "score-weighted dampening" the linear-α path encodes.

**Closing:** code change reverted; this branch carries only the docs update. The branch `research/splade-rrf-redo` carried the change for the eval and is being thrown away (same as the original phase 2 branch was). Issue #1176 stays closed. The phase 2 entry above is updated with this caveat-note pointing at this entry; the absolute numbers in the original entry are now superseded by the corrected-matcher numbers here.

**Lesson — meta:** when a relative comparison feels "barely in tolerance," check the harness before accepting "tie within noise." The 0.9pp test R@5 gap in the original phase 2 was real — it was just compressed by matcher noise. A 3.6pp gap on the corrected matcher would have been an obvious "linear-α wins" without any "barely" caveat. Compressed-by-noise differences are a known pitfall of small benchmarks; loosening the matcher made the eval's resolution match what the underlying question deserved.

---

## 2026-05-02 — BGE-large LoRA fine-tune A/B vs base

**Issue:** #1289. The HF repo `jamie8johnson/bge-large-v1.5-code-search` (LoRA fine-tune of BGE-large on `cqs-code-search-200k`) had been published since the original training pass but never made it into the v3.v2 production-fixture comparison. Question: is the FT model strictly better than base?

**Setup:** Added `bge-large-ft` preset (same architecture as `bge-large` — 1024-dim, 512 max_seq, mean pooling, BGE prefix). Tokenizer fetch failed on first attempt because `tokenizer.json` lives in `onnx/` not at repo root for this HF layout — fixed `tokenizer_path = "onnx/tokenizer.json"`. Fresh slot, reindex to 14,460 chunks, then eval against test + dev under the corrected matcher.

**Headline numbers:**

| split | metric | BGE-large (base) | **BGE-large + LoRA** | Δ |
|-------|--------|-----------------:|---------------------:|--:|
| test  | R@1    | 43.1%            | **45.0%**            | +1.9 |
| test  | R@5    | 69.7%            | **73.4%**            | **+3.7** |
| test  | R@20   | 83.5%            | 83.5%                |  0.0 |
| dev   | R@1    | 45.9%            | **46.8%**            | +0.9 |
| dev   | R@5    | **77.1%**        | 70.6%                | **−6.5** |
| dev   | R@20   | **86.2%**        | 82.6%                | −3.6 |

**Updated best-per-metric across all 5 models tested today (BGE-base, BGE-FT, EmbeddingGemma+summ, v9-200k, nomic-coderank):**

| metric         | winner               | runner-up                                              |
|----------------|----------------------|--------------------------------------------------------|
| test R@1       | Gemma / v9 (tie 45.9%) | BGE-FT 45.0%                                         |
| **test R@5**   | **BGE-FT 73.4%**     | v9-200k 70.6%, BGE-base 69.7%                          |
| test R@20      | BGE-base / BGE-FT 83.5% (tied) | Gemma 82.6%                                |
| dev R@1        | Gemma / coderank 47.7% | BGE-FT / v9 46.8%                                    |
| **dev R@5**    | **BGE-base 77.1%**   | Gemma 71.6%, BGE-FT 70.6%                              |
| dev R@20       | **BGE-base 86.2%**   | Gemma 85.3%, BGE-FT 82.6%                              |

**The trade-off, mechanistically.** LoRA fine-tuning on a curated code-pair distribution drags the embedding space toward that distribution. Test split is closer to it (queries look more like training pairs); dev split is deliberately broader (more natural-language reasoning, more open-ended exploration). Result: BGE-FT moves +3.7pp on test R@5 and -6.5pp on dev R@5. Canonical in-vs-out-of-distribution fine-tune trade-off — not a bug.

**Verdict:** ship as `bge-large-ft` opt-in preset; do NOT promote to default. Reasoning:
1. Dev R@5 is the more conservative signal for cqs's deployment context — agents don't always issue cleanly code-shaped queries (onboarding / exploratory questions). Losing 6.5pp there means worse generalization.
2. The test R@5 win is real but capped — at 73.4% it's the best we've seen, but BGE-base at 69.7% was already in the same band, and the test gate has been historically noisy.
3. Cost is identical — same architecture, same 1.3GB ONNX bundle, same inference latency. Opt-in preset is the right shape.

cqs default stays at BGE-base. BGE-FT joins the opt-in preset list alongside `v9-200k`, `nomic-coderank`, `embeddinggemma-300m`.

**Caveat on chunk counts.** Default slot has 19,476 chunks; the new `bge-ft` slot has 14,460. Partly real chunker output, partly accumulated orphan rows in the default slot from prior chunker-ID-format changes (#1283 — fixed in PR #1295, just not yet rerun against default). The eval matcher finds the right `(file, name)` chunk if indexed, so this doesn't bias the headline numbers, but a follow-up rerun of BGE-base from a freshly-pruned default slot would tighten the comparison.

**Open follow-up:** per-category breakdown of the dev R@5 gap (6.5pp) — likely concentrated in `negation` / `multi_step` / `conceptual_search` where dev queries deliberately stress NL reasoning. Tracked as a footnote in the BGE-FT HF model card.

**Closing:** preset added; HF model card updated with v3.v2 results + trade-off framing; default-model decision unchanged. Eval JSONs at `/tmp/eval-bge-ft-{test,dev}.json`. Issue #1289 closed.
