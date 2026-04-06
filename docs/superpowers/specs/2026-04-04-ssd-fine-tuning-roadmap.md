# Fine-Tuning Roadmap: SSD-Inspired Recipes

Motivated by "Embarrassingly Simple Self-Distillation Improves Code Generation" (Zhang et al., Apple, April 2026, arxiv 2604.01193). SSD's structural insight — self-derived training signal needs a structural filter to break the fixed point, and the filter contributes more than the signal itself — validates the CG filter pattern and suggests next experiments.

**Status (2026-04-06):** Experiments 1-3 complete. All null for E5-base. E5-base ceiling confirmed at ~81% pipeline R@1. BGE-large at 91.2% is the production model. Experiments 4-5 remain untried but face diminishing returns.

---

## Background: What Transferred

SSD trains a code LLM on its own unfiltered outputs. Naive self-training is a fixed point (Appendix B.2). Truncation breaks the fixed point by reshaping the distribution — suppressing distractor tails at "lock" positions (syntax-determined) while preserving diversity at "fork" positions (multiple valid continuations).

The parallel to cqs embedding training:

| SSD (generative) | cqs (contrastive) |
|---|---|
| Naive self-training = fixed point | FAISS hard negative mining = basin at ~81% (296q) |
| Truncation breaks fixed point | CG filter breaks fixed point |
| Near-gibberish data still helps | Enrichment dominates model choice (+15pp on 296q) |
| Context-dependent reshaping | BM25 handles locks, HNSW handles forks, RRF fuses |

---

## Current Baselines (re-baselined 2026-04-06)

| Model | Params | Pipeline R@1 (296q) | MRR | Raw R@1 (55q) |
|-------|--------|---------------------|-----|---------------|
| **BGE-large FT** | 335M | **91.9%** | **0.955** | 66.2% |
| BGE-large | 335M | 91.2% | 0.951 | 61.8% |
| v9-200k | 110M | 81.4% | 0.898 | 70.9% |
| E5-base | 110M | ~75% | ~0.87 | 49.1% |

---

## Experiments

### ~~1. Similarity-Band Negative Mining~~ — NULL RESULT (2026-04-05)

**Result:** 81.1% pipeline R@1 vs 81.4% v9-200k baseline. Band [20,50) negatives from v9-200k embedding space, CG-filtered, margin=0.05.

Band-selected negatives (ranks 20-50 instead of top-k) don't help. The model already learns everything it can from the CG-filtered data regardless of which negatives are selected from the filtered pool.

---

### ~~2. GIST Margin Sweep~~ — NULL RESULT (2026-04-05)

**Result:** All margins (0.01-0.10) land in 80-83% pipeline R@1. Margin=0.03 gives +1.8pp raw R@1 (repeatable, deterministic) but pipeline is within training seed variance. Default 0.05 confirmed correct.

Note: the original spec proposed an InfoNCE temperature sweep. We swept the GIST margin instead, which serves the same purpose — controls the false-negative filtering aggressiveness. The finding: CG filter + GIST margin overlap; adjusting one doesn't compensate for the other.

---

### ~~3. Iterative Self-Distillation~~ — NULL RESULT (2026-04-06)

**Result:** 70.9% raw R@1 — identical to v9-200k baseline. Exact fixed point.

Mining from v9-200k's own embedding space (top-k, CG-filtered) and retraining produces the identical model. SSD's prediction holds: self-training without a *new* structural filter is a fixed point. CG filter broke the fixed point once (base → v9-200k); the same filter applied to v9-200k's space doesn't break it again.

The **risk identified in the original spec was correct**: "The 89.1% basin might reassert." It did — the CG filter is a one-shot escape, not an iterative one.

---

### 4. Enrichment-Mismatch Mining — NOT RUN

**Cost:** Medium-high. Requires dual-index (raw and enriched) for the training corpus.

**Idea:** Mine negatives from raw embeddings (without enrichment stack), but train with enriched representations. Functions that look similar without doc comments but dissimilar with them are exactly the negatives the model needs to learn from. The enrichment stack has already solved the discrimination — use enrichment-aware gaps as training signal to teach the model to internalize what enrichment provides externally.

**Concretely:**
1. Build a second index of the training corpus using raw embeddings (no doc comments, no file context, no call graph context, no type signatures)
2. Mine hard negatives from the raw index (with CG filter)
3. Train using enriched representations as usual

**Why this is different from 1-3:** Experiments 1-3 all operated within the same embedding space (v9-200k enriched). This experiment creates a *mismatch* between the mining space and training space. The gradient comes from the gap between what the raw model confuses and what enrichment can distinguish.

**Expected effect:** The model learns the distinctions that currently require enrichment to make. If successful, enrichment contribution should decrease (model internalizes it) while raw model quality increases. The enrichment ablation showed v9-200k loses 16.6pp without doc comments vs BGE-large's 6.8pp — reducing that gap would be a real advance.

**Honest assessment:** Given three null results on E5-base, there's a real possibility this also hits the ceiling. The E5-base architecture (110M params, 768-dim) may simply not have the capacity to internalize what enrichment provides. BGE-large (335M, 1024-dim) already does this naturally — its enrichment dependency is 2-3x lower.

---

### 5. Lock/Fork-Aware Training Weights — NOT RUN

**Cost:** Medium. Requires per-chunk entropy estimation + weighted loss.

**Idea:** Weight training pairs by anchor entropy: fork-like anchors (dense neighborhoods) get upweighted, lock-like anchors (isolated) get downweighted.

**Infrastructure required:**
- A. Per-chunk k-NN density estimation (~minutes for 200K chunks)
- B. Join entropy with training pairs via content_hash
- C. Per-anchor weight scaling in loss function
- D. Three-bin weighting: low entropy = 0.5, medium = 1.0, high entropy = 2.0

**Honest assessment:** Three null results suggest the E5-base training signal is saturated. Reweighting the same signal is unlikely to break through alone. Might stack with #4 if #4 shows signal. Low priority.

---

## Experiment Order (revised)

```
#1 (band mining)      — DONE — null
#2 (margin sweep)     — DONE — null
#3 (iterative)        — DONE — null, exact fixed point
#4 (mismatch mining)  — next if pursuing E5-base, independent
#5 (entropy weights)  — lowest priority, run only if #4 shows signal
```

---

## Revised Success Criteria

The original target ("break above 91% fixture R@1") is unreachable for E5-base. Three experiments confirmed the ceiling at ~81%.

**For E5-base (experiments 4-5):**
- Any improvement above 82% pipeline R@1 on the 296q fixture eval
- OR: reduction in enrichment dependency (doc ablation <-12pp, currently -16.6pp)

**For the project:**
- BGE-large FT at 91.9% is the production ceiling
- Further improvement requires a different base architecture (ColBERT, larger model) or enrichment stack improvements
- Time may be better spent on features (wiki, embedding cache, cross-project call graph) than training experiments

---

## References

- Zhang et al. "Embarrassingly Simple Self-Distillation Improves Code Generation." arXiv 2604.01193, April 2026.
- cqs RESULTS.md — authoritative eval numbers
- cqs ROADMAP.md — experiment tracking
