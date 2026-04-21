# Fused Alpha + Classifier Head — Contrastive Ranking

**Status:** Proposed
**Author:** opus 4.7 + jjohnson
**Date:** 2026-04-20

## Problem

The current SPLADE-α router quantizes queries into 8 categories, then looks up a hardcoded α per category:

```
identifier_lookup → α=1.00      multi_step      → α=0.10
structural        → α=0.90      negation        → α=0.80
behavioral        → α=0.80      type_filtered   → α=1.00
conceptual        → α=0.70      cross_language  → α=0.10
```

Three failure modes follow from this design:

1. **Asymmetric error cost.** A single misclassification between two distant categories (multi_step → behavioral) flips α from 0.10 to 0.80, a catastrophic 8× SPLADE-weight swing. Phase 1.4 measured this empirically: the distilled head at 79.8% accuracy delivered ±0pp R@5 because the misclassified queries lost more than the correctly-classified ones gained.

2. **Convex-hull constraint.** Soft routing (blending defaults via the softmax distribution) can only land α in the convex hull of {1.0, 0.9, 0.8, 0.7, 0.1}. If the truly optimal α for a query is 0.55, no convex combination of defaults can produce it.

3. **Category granularity is wrong-sized for α.** The 8 categories were chosen for routing decisions (which index, which strategy). α is a continuous knob that may have variance *within* a category.

## Goal

Replace the hardcoded per-category α defaults with a learned head that emits α directly from the query embedding, **trained against the actual retrieval objective via contrastive ranking** rather than against a regression target. Keep the categorization head for routing decisions that are discrete (index selection, search strategy).

**Success criterion:** test R@5 ≥ +3pp over the v1.28.3 baseline (rule + centroid, no head) on `v3_test.v2.json` (109 queries), with no R@5 regression on `v3_dev.v2.json` (109 queries).

## Non-goals

- Replacing the rule classifier or centroid classifier. Both stay; the fused head sits between them in the resolution order, same slot the distilled head occupies today.
- Online training or RLHF-style updates. Training is offline batch.
- Touching CAGRA / HNSW / SPLADE base models. Only the α resolution path changes.
- Optimizing R@1 directly (use R@5 as the proxy — it's a smoother gradient signal and matches the production tuning target).

## Decision: distilled head is replaced, not deprecated

The distilled head (Phase 1.4 / 1.4b) was implemented on `feat/distilled-query-classifier` but never shipped to main. Both A/B measurements landed ±0pp R@5 (test ±0.0, dev +0.9 with 88.1% val accuracy after Phase 1.3 label expansion). The fused head replaces it on a clean basis:

- The distilled head code (`src/classifier_head.rs`, `CQS_DISTILLED_CLASSIFIER*` env vars, `reclassify_with_distilled_head` call sites, the v1 ONNX artifact) does not enter main. The branch stays around as a research reference.
- The fused head's PR introduces `src/fused_head.rs`, `src/corpus_fingerprint.rs`, and `reclassify_with_fused_head` against current main — no parallel-head deletion needed because nothing was shipped.
- Roadmap entry for the distilled-head arc gets a `(superseded by fused head before ship)` annotation.

Justification: cqs has no external users, and the distilled head's null result means the code carries no production value worth preserving. The contrastive fused head is a strict superset functionally — same categorization output, plus continuous α conditioned on corpus fingerprint.

## Why contrastive ranking over regression

Regression on a "best α per query" label requires:
- A brute-force α sweep over the corpus to generate labels (~65 min compute).
- Decisions on plateau handling (median? argmin? center of mass?).
- A loss function (Huber/MSE) that optimizes label fit, not actual ranking quality.

Contrastive ranking sidesteps all three:
- Labels are the existing (query, gold_chunk) pairs — already in the v3 + synthetic corpus.
- No plateau handling — the model learns α that *separates gold from distractors*, which is the actual objective.
- Loss is differentiable in α and operates on retrieval scores directly. The model can pick any α ∈ [0, 1], including values no per-category default ever produced.

The cost: requires pre-computing per-(query, candidate) sparse and dense scores at training time, and accepting a controlled simulator/production mismatch in the scoring function (see "Train/test mismatch" below).

## Design

### Architecture

```
query_embedding [1, 1024]    corpus_fingerprint [1, 1024]
                ↓                       ↓
                concat → [1, 2048]
                        ↓
trunk: Linear(2048, 64) + ReLU + Dropout(0.1)
                        ↓
     ├── classifier head: Linear(64, 8)  → softmax → category
     └── alpha head:      Linear(64, 1)  → sigmoid → α ∈ [0, 1]
```

Total params: ~131K. Inference cost: still <0.1ms on CPU.

Two design choices baked in:

1. **Shared trunk.** Lets the alpha head benefit from the categorization signal during training without forcing alpha into a quantized output space.
2. **Corpus fingerprint as trunk input.** The trunk learns `(query, corpus) → α` rather than `(query) → α`. A query like "find the parser" wants a different α in a Rust monorepo than in a polyglot data pipeline; the fingerprint conditions the head on which kind of corpus it's serving. Locked in from v1 so the input contract doesn't change when multi-corpus training lands later.

### Corpus fingerprint

A **single 1024-dim vector that summarizes the corpus's semantic distribution**:

```
fingerprint = normalize( mean( bge_embed(chunk) for chunk in corpus.chunks ) )
```

Computed once per corpus, cached on disk, loaded at daemon startup, concatenated with every query embedding before the trunk forward pass.

**Properties:**
- Independent of query — single per-corpus computation.
- No per-query cost — concat is O(2048) memcpy.
- Generalizes to arbitrary corpora — no lookup table, no registered set.
- Captures the global semantic centroid of the codebase, which proxies "what kind of repo is this."

**Storage:** `~/.local/share/cqs/corpus_fingerprint.v1.bin` (4 KB raw float32).

**Computation:** lazy on first fused-head invocation — scans `chunk_embeddings` from the store, computes mean, normalizes, caches both in-memory and to disk. Subsequent daemon restarts read from disk (fast). Invalidated by index regeneration: `cqs index` deletes the cache to force recompute on next use.

**MVP simplification:** training set is single-corpus (cqs's own 15991 chunks). The corpus channel sees a constant fingerprint during training, which means the model can't yet learn per-corpus variation — but the input slot exists, so adding multi-corpus training later requires only collecting more (query, corpus, gold) triplets, not changing the architecture.

### Loss

```
L = L_cls + λ_α · L_α
L_cls = CrossEntropy(category_logits, gemma_label)
L_α   = ContrastiveRanking(α_pred, sparse_scores, dense_scores, gold_idx)
```

`λ_α = 1.0` to start.

#### Contrastive ranking loss definition

For each training query `q`:

- `sparse_scores[K+1]` — sparse-only scores for `[gold, distractor_1, ..., distractor_K]`
- `dense_scores[K+1]`  — dense-only scores for the same
- `α_pred ∈ [0, 1]`    — single scalar from the alpha head

Combined score per candidate:

```
combined_i(α) = α · sparse_scores[i] + (1 − α) · dense_scores[i]
```

Differentiable cross-entropy ranking loss with temperature `τ`:

```
L_α = −log( exp(combined_gold(α) / τ) /
            Σ_i exp(combined_i(α) / τ) )
```

Gradient flows from the loss back through `combined_i(α)` (linear in α) into the sigmoid output of the alpha head.

`τ = 0.1` to start. Lower τ sharpens the ranking pressure; higher τ smooths it. Sweep [0.05, 0.1, 0.2] if training is unstable.

#### Distractor selection

For each (query, gold) pair, build the candidate pool by:

1. Run a single baseline retrieval (α=0.7) and take top 50 candidates.
2. Remove the gold chunk if present.
3. Sample K=15 distractors stratified by score quantile (5 each from top/middle/bottom thirds). This forces the model to discriminate against both easy and hard negatives.
4. If gold is missing from the top 50, append it at position 0 — it must always be in the candidate set since the loss is gold-vs-everyone-else.

Pool size 16 (1 gold + 15 distractors) keeps per-step compute tractable while exposing the model to a realistic retrieval-time decision.

#### Training data prep

For each query in the 4376-query corpus (544 v3 + 3833 synthetic):

```
embedding   = bge_embed(query)         (1024 floats)
gemma_label = cached_gemma_classify    (1 of 8 categories)
candidates  = top_50(α=0.7, splade_on) (50 chunk IDs + gold_idx)
sparse_scores[i] = splade_score(query, candidates[i])
dense_scores[i]  = cosine(bge_embed(query), bge_embed(candidates[i]))
```

Plus the corpus fingerprint computed once for the cqs corpus:

```
fingerprint = normalize( mean( bge_embed(chunk) for chunk in cqs.chunks ) )
```

The fingerprint is constant across the entire training set (single corpus). Each training example feeds `(query_embedding, fingerprint)` to the trunk.

Pre-compute once into a `.npz` shard. Estimated size: 4376 queries × (16 candidates × 2 score arrays + 1024-dim query_embedding) × 4 bytes + 1024-dim fingerprint ≈ 18 MB. Still trivial.

This pre-compute is the only "α-aware" step at training time. The model never sees α targets — only the score components, and it learns which α reranks gold to position 1, conditioned on `(query, corpus)`.

### Inference path

Two new files: `src/fused_head.rs` (ORT inference) and `src/corpus_fingerprint.rs` (lazy compute + cache).

```rust
// src/corpus_fingerprint.rs
pub fn load_or_compute(store: &Store<ReadOnly>) -> Option<Arc<Vec<f32>>>;
// Reads ~/.local/share/cqs/corpus_fingerprint.v1.bin if present;
// otherwise scans store, computes normalized mean of chunk embeddings,
// writes the cache, returns the vector.

// src/fused_head.rs
pub struct FusedHead {
    session: Mutex<ort::Session>,
    fingerprint: Arc<Vec<f32>>,
    threshold: f32,
}

pub struct FusedClassification {
    pub category: QueryCategory,
    pub category_confidence: f32,
    pub alpha: f32,
}

pub fn classify_with_fused_head(query_embedding: &[f32]) -> Option<FusedClassification>;
```

The fingerprint is loaded once at head construction (inside `FusedHead::load`) and concatenated with the query embedding inside `classify` before the ORT forward pass. Call sites don't need to know about the fingerprint.

`src/search/router.rs` adds:

```rust
pub fn reclassify_with_fused_head(
    classification: Classification,
    embedding: &[f32],
) -> (Classification, Option<f32>) {  // returns (cls, override_alpha)
    if let Some(out) = crate::fused_head::classify_with_fused_head(embedding) {
        if out.category_confidence >= threshold {
            classification.category = out.category;
            return (classification, Some(out.alpha));
        }
    }
    (classification, None)
}
```

Call sites (search/query.rs, batch/handlers/search.rs, eval/runner.rs) replace `reclassify_with_distilled_head` with `reclassify_with_fused_head` and use the returned `Option<f32>` to override `splade_alpha` when present.

**Cache invalidation:** `cqs index` deletes the fingerprint cache after a successful index update, forcing the next fused-head load to recompute. Hooked into the existing index-completion path in `src/cli/commands/index.rs`.

### Env vars

| Var | Default | Effect |
|-----|---------|--------|
| `CQS_FUSED_HEAD` | `0` | Set `1` to enable. Supersedes `CQS_DISTILLED_CLASSIFIER` when both are set. |
| `CQS_FUSED_HEAD_PATH` | — | Override on-disk model path (test/dev). |
| `CQS_FUSED_HEAD_THRESHOLD` | `0.4` | Min category-softmax to trust the head. Below threshold falls back to centroid. |
| `CQS_FUSED_HEAD_ALPHA_FLOOR` | `0.0` | Clamp predicted α to ≥ this value. Set non-zero only if eval surfaces queries collapsing to α=0. |

### File layout

```
src/fused_head.rs                                     (new — ORT inference)
src/corpus_fingerprint.rs                             (new — lazy compute + cache)
src/lib.rs                                             (pub mod fused_head, corpus_fingerprint)
src/search/router.rs                                   (reclassify_with_fused_head)
src/cli/commands/index.rs                              (invalidate fingerprint cache on reindex)
src/cli/commands/search/query.rs                       (wire fused_head call site)
src/cli/batch/handlers/search.rs                       (wire fused_head call site)
src/cli/commands/eval/runner.rs                        (wire fused_head call site)
evals/build_contrastive_shards.py                     (new — pre-computes per-query
                                                        sparse/dense scores + candidate IDs)
evals/train_fused_head.py                             (new — trains both heads with
                                                        CE + contrastive ranking)
evals/fused_head_ab_eval.py                           (new — A/B vs distilled head + baseline)
evals/fused_head/                                      (new artifact dir — model + meta)
evals/fused_head/contrastive_shards.npz               (pre-computed training shards)
~/.local/share/cqs/fused_head.v1.onnx                 (deployed model)
~/.local/share/cqs/corpus_fingerprint.v1.bin         (lazy-computed corpus fingerprint, 4 KB)
```

## Train/test mismatch

The training-time scoring function is `α · sparse + (1-α) · dense`. Production scoring uses RRF fusion with α-weighted RRF score components. The two functions agree on direction (higher α → more weight on sparse) but disagree on magnitude.

Three options for handling the mismatch:

1. **Accept it.** Linear blend is a strict simplification. The model learns the *direction* of α correctly even if magnitude is off; the empirical R@5 on production scoring is what gates ship.
2. **Match exactly.** Re-implement RRF in PyTorch. Doable — RRF is `1 / (k + rank)` and rank can be approximated via softmax temperature on raw scores. Adds ~30 min of training-loop code.
3. **Calibrate post-hoc.** Train with linear blend, then sweep an α scaling factor at validation time so the deployed α matches the production retrieval characteristics.

Default: option 1 (accept) for the MVP. If validation A/B shows the model learns wrong-direction α (rare but possible), pivot to option 2.

## Validation

Three-cell A/B on `v3_test.v2.json` and `v3_dev.v2.json`:

1. **Baseline:** rule + centroid. v1.28.3 production.
2. **Distilled head (Phase 1.4b):** the current single-head model. Whatever Phase 1.4b A/B measured.
3. **Fused head (this spec):** trunk + classifier + alpha heads, alpha used directly.

Decision matrix:

| Result | Action |
|--------|--------|
| Cell 3 R@5 ≥ baseline + 3pp on test, no regression on dev | Ship as v1.28.4. Flip `CQS_FUSED_HEAD=1` default ON. |
| Cell 3 R@5 ≥ cell 2 + 1pp, but < baseline + 3pp | Re-run with τ ∈ {0.05, 0.2} and K ∈ {7, 31}. Sweep distractor sampling strategies (all-hard vs stratified). |
| Cell 3 R@5 ≤ cell 2 | Inspect predicted α distribution. If collapsed to a single mode, increase trunk capacity to Linear(1024, 128). If wildly variant, revisit τ. |
| Cell 3 R@5 catastrophic regression | Inspect train/test scoring mismatch — pivot to option 2 (RRF in PyTorch). |

Per-category R@5 must also be reported in the eval output to spot the asymmetric-cost failure mode if it persists.

Additional diagnostic: histogram of predicted α across the test+dev queries, broken out by Gemma category. The fused model's α output should:
- Concentrate near 1.0 for identifier_lookup
- Concentrate near 0.1 for multi_step
- Spread across [0.4, 0.9] for behavioral / conceptual / structural

If the histogram shows the model collapsing all categories to a single α mode, the contrastive loss isn't producing useful gradient — investigate before A/B is meaningful.

## Open questions

1. **Multi-corpus training data collection.**
   The corpus channel is plumbed from v1, but it's inert until trained on multiple corpora. Need to pick 4-5 codebases that span variety along the axes that should drive α: language mix, repo size, domain (web/systems/data/embedded). Concrete candidates to evaluate:

   | Corpus | Why | Approx chunks |
   |--------|-----|---------------|
   | django/django | Python web, mature | ~30K |
   | tokio-rs/tokio | Rust async, contrasts cqs's sync-ish Rust | ~8K |
   | facebook/react | TypeScript/JS frontend | ~25K |
   | torvalds/linux (subdir like `kernel/` or `mm/`) | C systems, large | ~50K (if scoped) |
   | apache/spark | Scala/Java enterprise | ~40K |
   | bevyengine/bevy | Rust game engine, ECS-heavy, very different from cqs | ~12K |

   Pick 4 from this list. Need: (a) v3-style query+gold pairs per corpus (~500 each via `evals/generate_from_chunks.py` against each indexed corpus), (b) corpus fingerprint per repo, (c) joint training loop that mixes batches across corpora. Spec the data collection in a follow-on plan once this MVP demonstrates the mechanism on cqs alone.

2. **Cache-invalidation race — durable solution.**
   The "daemon may hold stale fingerprint" issue needs a real fix, not just deferral. Three mechanisms, layered:

   - **Push (primary):** `cqs index` sends a `fingerprint_invalidated` message over the existing daemon Unix socket on successful index completion. Daemon's IPC handler clears `Arc<Vec<f32>>` and the next query triggers recompute. Reliable across WSL / NTFS / network filesystems where inotify is unreliable.
   - **Pull (defense in depth):** daemon stats the fingerprint file's mtime on every Nth query (e.g., N=100). If mtime is newer than the cached load time, reload. Cheap (one stat call per N queries) and catches the case where the push message is missed (daemon down during reindex, cold-start with stale cache).
   - **Inotify (optimistic):** if the daemon's existing `cqs watch` inotify is active, add the fingerprint file path to the watch set. Reload on `IN_DELETE` or `IN_MODIFY`. Skipped on WSL where inotify is known unreliable on `/mnt/c/`.

   MVP: implement push + pull. Inotify is bonus on Linux native. Document expected behavior per platform.

   Still deferred: the v1 spec ships push only. Pull layer goes in if a stale-fingerprint regression is observed in the wild.

3. **Single-corpus training risk.**
   Even with mitigations (fingerprint dropout p=0.2 + Gaussian jitter σ=0.01), single-corpus training fundamentally limits what the corpus channel can learn. The honest answer is open-question #1 — collect more corpora. Mitigations are a stopgap to let v1 ship with the input contract intact.

## Ablations

These are not blocking design decisions — they're knob settings to sweep during training and report alongside the headline result. None require architectural changes.

| Knob | MVP setting | Sweep range | Hypothesis being tested |
|------|-------------|-------------|-------------------------|
| Temperature `τ` | 0.1 constant | {0.05, 0.1, 0.2}; also annealed 0.5 → 0.05 | Lower τ sharpens ranking pressure; annealing may help late-stage convergence |
| Distractor sampling | Stratified (5 per quantile third) | + pure-hard (top-K-1), + pure-random, + curriculum (random → hard over epochs) | Hard mining pushes harder but risks overfitting to easy flips |
| Score normalization | Raw scores | + per-component z-score, + min-max [0,1], + log1p on SPLADE only | If α collapses to {0,1}, normalization is the cause |
| Trunk hidden dim | 64 | {32, 64, 128, 256} | Capacity bottleneck check; 64 chosen for inference cost |
| `λ_α` (loss weight) | 1.0 | {0.1, 0.5, 1.0, 2.0, 5.0} | Balance categorization vs alpha objectives |
| Distractor pool size K | 15 | {7, 15, 31, 63} | Larger pool = harder ranking; diminishing returns expected past 31 |
| Fingerprint dropout p | 0.2 | {0.0, 0.1, 0.2, 0.5} | Prevents trunk from absorbing constant fingerprint into bias terms |
| Fingerprint jitter σ | 0.0 | {0.0, 0.005, 0.01, 0.02} | Simulates corpus variation in single-corpus training |

Report all ablations as R@5 deltas vs the MVP setting on `v3_test.v2.json`. Pick the best per-knob setting before final A/B.

## Alternative considered: regression on per-query α targets

The earlier draft of this spec used regression with brute-force α sweep labels. Rejected because:
- The labeling step adds 65 min of compute and a class of judgment calls (plateau handling, grid granularity) before training can begin.
- The loss target is a proxy for what we actually want (R@5). Contrastive ranking is the objective itself.
- Regression bounds the alpha head to whatever resolution the sweep grid uses (0.1 spacing). Contrastive learns continuous α with no quantization.

Kept here as the documented fallback if contrastive training proves unstable across multiple temperature/distractor sweeps.

## Future work

**Multi-corpus joint training.**
The corpus channel is plumbed from v1, but training data is single-corpus (cqs only). To activate the conditional structure, collect (query, gold) pairs from 3-5 additional corpora — candidates: a Python data pipeline, a TypeScript frontend monorepo, an embedded C codebase, a Java enterprise repo. Each corpus contributes its own fingerprint, and training mixes batches across corpora so the trunk learns to condition α on corpus identity rather than memorize a single fingerprint. Spec the data collection separately; the model architecture is unchanged.

**Richer corpus fingerprint.**
The MVP fingerprint is a single 1024-dim mean. Two upgrades to consider once multi-corpus training is in flight:
- **Per-category centroids:** stack the 8 centroids from the existing centroid classifier → 8192-dim fingerprint. Captures within-corpus variation (a polyglot repo's identifier_lookup centroid differs from its conceptual_search centroid).
- **Compressed multi-feature:** PCA-down `[mean, top-K cluster centroids, language-distribution histogram, chunk-count log-bucket]` to 128 dims. More information, smaller input.

**End-to-end RRF surrogate.**
Replace the linear blend in the contrastive loss with a differentiable RRF approximation (rank-via-softmax-temperature). Eliminates the train/test mismatch entirely. Worth building once the contrastive MVP demonstrates the basic mechanism works.

**Multi-stage routing.**
Currently α is the only learned routing knob. The same contrastive framework could learn:
- SPLADE pool size (currently fixed at 100)
- Dense pool size (currently fixed at 100)
- Reranker on/off (currently rule-based on category)
- Reranker pool cap

Each is a continuous (or low-cardinality discrete) decision that maps the same way: `(query, corpus) → routing parameter → measured downstream R@5`. Spec separately when α is shipped.
