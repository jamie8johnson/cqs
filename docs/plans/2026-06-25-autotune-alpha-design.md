# Autotune-α — per-codebase fusion-weight tuning (design spec, PARKED)

**Status: PARKED — design only, not scheduled.** Captured 2026-06-25. The end goal of the retrieval-quality program: cqs auto-tunes the dense↔sparse fusion weight α to each codebase it encounters, with no manual sweep — so retrieval is calibrated to *this* corpus's lexical/semantic character automatically.

## Goal

When cqs first indexes a codebase, it produces a fusion-weight α calibrated to that corpus, persisted per-slot, with zero manual eval work. Replaces the hand-run per-category α-sweeps with an automatic, per-codebase, self-supervised tune.

## Why (the case for it over a fixed α)

- **α is corpus- and query-dependent, and the variance is large.** The v3 alpha sweeps show per-category best-α from ~0.05 (multi_step, SPLADE-dominant) to ~0.95 (type_filtered, dense-dominant); a single fixed α is a weighted compromise that serves no category well.
- **Manual sweeps are noisy and don't adapt.** Per-category sweeps run at n=8–14 single-gold queries — below the noise floor (the reason retunes require test+dev agreement). And whatever α we ship is tuned on *our* corpus, not the user's.
- **A different codebase has a different optimum.** An identifier-dense codebase wants dense-heavy α; a prose/concept-heavy one wants more SPLADE. Only a per-corpus tune captures that.

## The first-encounter pipeline

```
index → embed → llm-summaries enrich (2-pass) → auto-generate pseudo-eval → sweep α on the ENRICHED index → persist per-corpus α
```

### Critical sequencing gate: α tunes AFTER the summaries are APPLIED

α tunes the dense↔sparse balance, and the LLM summaries change the **dense** leg (they enrich the embedded text). Tuning α before the summaries land calibrates against embeddings cqs will not serve — the enrichment then shifts the dense scores out from under the chosen α.

**"Filled" means the two-pass enrichment is *applied*, not merely stored.** `--llm-summaries` stores summaries (Batches lands after the embed pass); a follow-up `--force` re-embeds with them. The dense leg only reflects the summaries after pass 2. The α-tune must gate on pass-2 completion, ideally on the `0 chunks-with-summary-unenriched` invariant — the same trap that made a recall gate on a partially-enriched index *understate*. An α-tune on a partially-enriched index mis-tunes identically.

## The pseudo-eval (self-supervised gold)

No user codebase ships with a labeled eval set, so the tune must generate one:
- **Queries from the code** — LLM generates queries from chunk docstrings/signatures/comments (the existing fixture-generation path), source chunk = pseudo-gold. Reuses the query-gen machinery; no new science.
- **Gold model:** single-gold-pseudo (source chunk) is the cheap first version; **multi-gold-pseudo** (pooled + judged, see below) is the richer one that un-saturates the metric.

## Metric: nDCG, not R@K

α is a **rank-shuffling** knob — it moves a result from rank 8 to rank 3, it rarely flips in/out of the top-20. R@K is a step function blind to within-K rank order, so it gives α almost no gradient. **nDCG** (rank-discounted) responds to the shuffle. With multi-gold, nDCG/MAP also stay discriminative past R@20 (more relevant items → ranking quality matters deeper). Tuning a ranking weight demands a ranking metric.

## Multi-gold fixture (the prototype of the pseudo-eval's gold model)

Build + validate on *our* corpus first, then port the gold model to the auto-generated per-codebase pseudo-eval:
- **Pooling:** judge the union of top-K from *diverse* configs.
- **Judge:** the existing dual-judge / local-LLM relevance harness (graded 0–3 → nDCG; binary → multi-gold R@K/MAP).
- **α-circularity caveat (load-bearing for this use case):** if the pool is built only from α-fusion configs and you then tune α against it, you score α among the very configs that *defined* "relevant" — circular. **The pool MUST include legs that are not α-fusion** — dense-only, sparse-only, FTS/BM25 — so the gold set is not defined by the knob being turned.

## Design forks (decide when un-parked)

1. **LLM-query-gen dependency.** Needs a local model on the user's box (fits local-first). Required, or graceful-skip "run `cqs autotune` when a model is available"? — lean graceful-skip; never block indexing on it.
2. **Eager vs background.** First-index must not block on a sweep. Background pass after the index (and enrichment) settles — recommended.
3. **Per-corpus α now, per-query-type classifier later.** The variance is mostly per-query-*type*, so a single per-corpus α leaves gain on the table. End-state is likely **per-corpus baseline + a query-type classifier routing each query to its α** (the `evals/classifier_ab_eval.py` direction). Design the persisted-α path so the classifier can layer on; do NOT treat the single corpus α as terminal.
4. **Single- vs multi-gold pseudo-eval.** Multi-gold is richer but carries the pooling-incompleteness cost; ship single-gold-pseudo first, upgrade to multi-gold-pseudo once the fixture methodology is validated.

## Risks / open questions

- **Pooling incompleteness (TREC's classic):** the gold set is only as complete as the pooled systems; a relevant chunk no config retrieved is a silent false-negative. Mitigate with deep, diverse pools — bound, not eliminate. The multi-gold set is *additive* to single-gold, not a replacement.
- **α-circularity** (above) — the sharpest risk specific to tuning α against a self-built pool.
- **Compute cost** of query-gen + sweep at first-encounter — bounded by the background-pass framing + a query-count budget.
- **Pseudo-gold quality** caps tune quality — LLM-generated queries from chunks may be unrepresentative of real agent queries; validate against the human-labeled fixture before trusting the auto path.

## Program sequence (stages, each gated)

1. **Multi-gold fixture + α-under-nDCG on our corpus.** Gate: pooled+judged multi-gold set built (with non-α legs in the pool); α swept under nDCG shows a smoother, lower-variance curve than single-gold R@5; the located α agrees test+dev.
2. **Auto-generated per-codebase pseudo-eval.** Gate: query-gen produces a pseudo-eval on an arbitrary corpus; its α-optimum correlates with the human-labeled optimum on our corpus (validation of the auto path).
3. **Autotune-α pass.** Gate: a background pass, gated on post-enrichment (`0 unenriched`), sweeps + persists per-corpus α per-slot; graceful-skip without a local model; first-index unaffected.
4. **Query-type classifier (optional terminal).** Gate: per-query α routing beats the single per-corpus α on the multi-gold nDCG, by more than the noise floor on test+dev.

## Relationship to other work

Shares the gold model + sweep harness with the multi-gold eval work; the SPLADE↔dense viz (in build) shows the same fusion mechanism the tune optimizes. All one program: **multi-gold fixture → auto-pseudo-eval → autotune-α → (classifier).**
