# Fine-Tuning Roadmap: SSD-Inspired Recipes

Motivated by "Embarrassingly Simple Self-Distillation Improves Code Generation" (Zhang et al., Apple, April 2026, arxiv 2604.01193). SSD's structural insight — self-derived training signal needs a structural filter to break the fixed point, and the filter contributes more than the signal itself — validates the CG filter pattern and suggests next experiments.

---

## Background: What Transferred

SSD trains a code LLM on its own unfiltered outputs. Naive self-training is a fixed point (Appendix B.2). Truncation breaks the fixed point by reshaping the distribution — suppressing distractor tails at "lock" positions (syntax-determined) while preserving diversity at "fork" positions (multiple valid continuations).

The parallel to cqs embedding training:

| SSD (generative) | cqs (contrastive) |
|---|---|
| Naive self-training = fixed point | FAISS hard negative mining = basin at ~82% (296q) |
| Truncation breaks fixed point | CG filter breaks fixed point |
| Near-gibberish data still helps | Enrichment dominates model choice (+15pp on 296q) |
| Context-dependent reshaping | BM25 handles locks, HNSW handles forks, RRF fuses |

---

## Experiments (priority order)

### 1. Similarity-Band Negative Mining

**Cost:** Low. Data pipeline change only, no new infrastructure.

**Idea:** Don't take the absolute hardest negatives (closest in embedding space). Those are the most likely false negatives even after CG filtering — functions that are genuinely similar but not in the call graph. Take negatives from a similarity band: hard enough to be informative, but not the hardest.

**Concretely:** Instead of top-5 nearest non-CG neighbors, take top-20-to-top-50. The CG filter catches structural false negatives; the band catches semantic false negatives the call graph doesn't know about.

**SSD analog:** SSD's truncation doesn't just cut the bottom of the distribution — it removes both tails. Very highest probability tokens pass through unchanged; very lowest are removed. Similarity-band mining is the same shape: remove both extremes (too close = likely false negative, too far = uninformative).

**Prior evidence:** v9-200k-hn used FAISS top-k (hardest negatives) and regressed from 90.5% to 82.4%. v9-200k used CG-filtered only with no similarity ranking. The band idea sits between these — the experiment we should have run instead of jumping to FAISS.

**Test:** Train v10 with band-mined negatives, compare against v9-200k on fixture (296q) and real-code (100q) evals.

---

### 2. GIST Margin / InfoNCE Temperature Sweep

**Cost:** Lowest. Hyperparameter change only.

**Idea:** Standard contrastive loss uses a fixed temperature τ on similarity scores. SSD's insight: training-time temperature interacts with eval-time behavior — the optimal training temperature isn't necessarily the default. The CG filter changes which negatives survive, which changes the optimal τ. If CG filter was added without re-tuning τ, there may be headroom.

**Note:** We use CachedGISTEmbedLoss with margin=0.05. GIST's margin parameter acts similarly to temperature — sweep both: GIST margin ∈ {0.01, 0.03, 0.05, 0.08, 0.1, 0.15} and if switching to InfoNCE, τ ∈ {0.01, 0.02, 0.05, 0.07, 0.1, 0.15, 0.2}.

**Test:** Grid search, each run is cheap (same data, same architecture, different scalar).

---

### 3. Iterative Self-Distillation

**Cost:** Medium. Multiple training rounds, each ~3h (E5-base) or ~13h (BGE-large) on A6000.

**Idea:** v9-200k was one round: train on base model's embedding space negatives with CG filter. Use v9-200k's embedding space to mine negatives (with CG filter) → train v10. The reshaped embedding space produces different negatives. The CG filter prevents collapse (fixed-point breaker preserved across rounds).

**Concretely:**
1. Index training corpus with v9-200k model
2. Mine negatives from v9-200k's embedding space (with CG filter, possibly with band from experiment #1)
3. Train v10 on new negatives
4. Evaluate. If improved, repeat.

**Risk:** The ~82% basin might reassert. The CG filter breaks the one-round fixed point, but multi-round dynamics could converge to a different basin. Monitor for collapse — if round 2 matches round 1, stop.

---

### 4. Enrichment-Mismatch Mining

**Cost:** Medium-high. Requires dual-index (raw and enriched) for the training corpus.

**Idea:** Mine negatives from raw embeddings (without enrichment stack), but train with enriched representations. Functions that look similar without doc comments but dissimilar with them are exactly the negatives the model needs to learn from. The enrichment stack has already solved the discrimination — use enrichment-aware gaps as training signal to teach the model to internalize what enrichment provides externally.

**Concretely:**
1. Build a second index of the training corpus using raw embeddings (no doc comments, no file context, no call graph context, no type signatures)
2. Mine hard negatives from the raw index (with CG filter)
3. Train using enriched representations as usual

**Expected effect:** The model learns the distinctions that currently require enrichment to make. If successful, enrichment contribution should decrease (model internalizes it) while raw model quality increases. Long-term: potentially reduce enrichment dependency, which is currently the fragile bottleneck (v9-200k collapsed from 49% to 26% R@1 after file restructuring because path-dependent enrichment changed).

**SSD analog:** This inverts the relationship between training data quality and model capability, similar to SSD's "bad data, good results" finding. The training data isn't better — it's worse (raw negatives are noisier) — but the mismatch between mining space and training space creates a gradient the model can learn from.

---

### 5. Lock/Fork-Aware Training Weights

**Cost:** Medium. Requires new infrastructure (per-chunk entropy estimation), then a training pipeline change.

**Idea:** Some code chunks are "lock-like" — unique signatures, isolated in embedding space. Others are "fork-like" — generic utilities, dense neighborhoods, many close neighbors. Weight training pairs by anchor entropy: fork-like anchors get upweighted because that's where the model needs finer discrimination. Lock-like anchors are already well-separated.

**SSD analog:** Context-dependent reshaping is SSD's core mechanism. Uniform sharpening hurts (kills fork diversity). Context-dependent sharpening helps (cleans lock tails, preserves fork spread). Entropy-weighted training is the contrastive equivalent.

#### Infrastructure Required

**A. Per-chunk k-NN density estimation.**
One query per training chunk against existing HNSW index. ~minutes for 200K chunks. Could be a `cqs density` command or `--entropy` flag on `train-data`.

**B. Mapping to training pairs.**
Join entropy scores with training data via content_hash (already the primary key).

**C. Weighted loss.**
Per-anchor weight scaling. Most contrastive loss implementations accept sample weights.

**D. Weight function.**
Three bins to start: low entropy (lock) = 0.5, medium = 1.0, high entropy (fork) = 2.0.

Total new code: ~100 lines in `train-data` for entropy estimation, ~10 lines in training loop for weighting.

---

## Experiment Order and Dependencies

```
#2 (margin/τ sweep)   — no dependencies, cheapest, run first
#1 (band mining)      — no dependencies, data pipeline only
#3 (iterative)        — depends on best result from #1/#2
#4 (mismatch mining)  — independent, needs dual-index build
#5 (entropy weights)  — needs infrastructure, run after #1/#2 establish baseline
```

#2 and #1 can run in parallel. #3 uses whatever #1/#2 found works best. #4 is independent and can run anytime. #5 is the most novel but has the highest setup cost.

---

## Success Criteria

The target to beat: v9-200k at 90.5% fixture R@1 (296q), BGE-large FT at 91.6%. Fine-tuned BGE-large at 50% real-code R@1 (100q expanded eval).

Any recipe that breaks above 92% on fixtures — or improves real-code eval by 2+pp above 50% — is a meaningful advance.

Secondary goal: reduce enrichment dependency. v9-200k loses 16.6pp without doc comments vs BGE-large's 7.5pp. A recipe that achieves comparable fixture R@1 with fewer enrichment layers active means the model internalized what enrichment was providing.

---

## References

- Zhang et al. "Embarrassingly Simple Self-Distillation Improves Code Generation." arXiv 2604.01193, April 2026.
- cqs research log: Exp 18-27, enrichment ablation, basin analysis
- v9-200k deep analysis: `ROADMAP.md` section "Future — Deep Analysis: Why v9-200k Escapes the Basin"
