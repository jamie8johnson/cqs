# SPLADE↔dense viz — locked design (query-anchored mechanism view)

Locked 2026-06-25 after a 4-critic adversarial review of the v2 static-divergence design (which was found **fatally** mis-framed: it measured the wrong object and mislabeled a difference as a value). This is the reframe; it must survive a second adversarial pass before build.

> **Update — second adversarial pass resolved the open questions → v3 (build spec).** Verdict: REFINE-AND-BUILD (frame survives, no FATAL). The four §"Open questions" below were each answered against this doc's hopeful framing: (1) feasibility — `cqs serve` holds no models and the daemon *discards* the three legs, so it's a real cross-surface feature (additive `legs` output on the retrieval core + a daemon `search_legs` verb + serve as a daemon-client), NOT a thin endpoint; (2) representativeness — the eval-gold driver is hybrid-flattering (0–36pp by category; 2 of 8 earn ~0), so the default is a **stratified tour with a pinned per-category anchor** that shows the zero-uplift categories too; (3) legibility — one hue can't carry 4-set Venn, so **step-through on a dimmed base** + the win number rides **text**, not color; (4) honesty — the per-chunk +/− "win" was the v2 sin re-imported (and the fusion math here was misstated: dense is *raw* cosine `[-1,1]`, only sparse is min-max'd, `d=0` is *injected* for SPLADE-only chunks), so the win signal is the **query-level R@K delta (fused vs dense-alone)**, attributable to the fusion not isolable to SPLADE per-chunk. Build is staged: Stage 1 = the `search_legs` backend; Stage 2 = the frontend (step-through, stratified tour, text-win, deck.gl renderer).

## The correction that forced the reframe
Production hybrid is **query-centric**, not chunk-centric, and the SPLADE↔dense fusion is **linear interpolation**, not RRF:
- `query.rs:642` dense = HNSW(query→chunks); `:654` sparse = `SpladeIndex::search`(query→chunks); `:747` fuses `α·dense + (1−α)·sparse` over min-max-normalized scores (`CQS_SPLADE_ALPHA`). RRF (`rrf_fuse`) is the separate semantic+FTS leg; the SPLADE leg never touches it.
- v2's `1−Jaccard@k(dense-kNN, splade-kNN)` was chunk→chunk — a different object than production's query→chunk fusion, and a *difference* (not a *value*): "gold = optimum" was a lie the color would tell.

## The frame
**Position = the dense semantic map; the query drives the action.** Don't summarize the relationship in a static scalar — *show the mechanism*.

- **Base layer (unchanged):** the existing `cqs serve` cluster view — dense-768 → UMAP → `chunks.umap_x/umap_y`, rendered by `cluster-3d.js` (x=umap_x, z=umap_y). This is honest "semantic context" *only*; it is explicitly NOT a divergence geography. Document on-surface that the plane is dense-cosine UMAP (stochastic, locally faithful, globally distorted, NOT metric) and that SPLADE structure is intentionally non-positional.
- **Interaction (the new core):** enter a query → highlight three overlaid point-sets on the map:
  - **dense top-k** (HNSW leg),
  - **SPLADE top-k** (`SpladeIndex::search` leg),
  - the **α-fused result** (what production returns).
  You watch the fusion pull in SPLADE's lexical matches that dense ranked low. Optionally draw on-demand edges from a fused-result chunk to its SPLADE-leg origin when it's dense-far — the "hybrid earns its keep" relation shown as an explicit link across the distorted plane, not implied by proximity.
- **"Hybrid wins" made honest:** the chunks the fused rank **elevates over dense-alone** are the candidates; overlay the **eval gold** and the elevated-AND-relevant ones light up as *genuine* wins (helpful, +) vs elevated-but-irrelevant (harmful, −). That signed-against-gold quantity is the only thing that may be labeled "where hybrid earns its keep" — answering the "optimum point" question truthfully.
- **Teaching layer (mandatory, from the interpretive critic):** click a chunk → surface the specific SPLADE-leg neighbors dense missed + the dominant shared **tokens** (decode `top_k_token_ids` via the tokenizer). Turns "color = mystery scalar" into "SPLADE pulled THIS toward THESE on THESE tokens dense ignored" — the two-complementary-spaces-fused-at-retrieval lesson, made visible.

## The static divergence map → optional secondary "explore" mode
v2's `1−Jaccard@k` is demoted to an optional, honestly-LABELED *descriptive* overlay ("lexical-vs-semantic neighborhood divergence; descriptive, NOT a hybrid-win signal"), gated on a saturation check (sample ~500 chunks, k-sweep, drop if the divergence distribution pins near 1.0), with exact brute-force dense-kNN (not ef-bounded HNSW). It is never the headline and never labeled a value/wins map.

## Build (lightest principled path)
Reuse the retrieval that already exists — no new persisted column, no schema bump for the headline mode.
- **OPEN (re-review must resolve):** does `cqs serve` expose the dense / SPLADE / fused legs **separately**, or only the final fused result? The mechanism view needs all three per query. If only fused is exposed, add a debug/observe endpoint that returns the three rankings for a query (the daemon already computes them in `query.rs`; surface them rather than recompute).
- Frontend: extend `cluster-3d.js` — a query box (search-highlight likely already exists) → three highlight styles + the click-inspector + optional edges. Y-axis: in the divergence *explore* mode, lift Y by the scalar (don't keep n_callers as Y while coloring by divergence); in the query mode, keep the dense plane.
- The eval-gold overlay needs the eval query set (`evals/queries/*.json`) loaded as the "show me the wins" driver.

## Open questions for the second adversarial pass
1. **Feasibility:** the three-legs-exposed question above — is it a small serve addition or a deeper change?
2. **Query representativeness:** whose queries drive the view — the eval set, user-typed, or both? A curated set risks cherry-picking; user-typed risks unrepresentative reads.
3. **Three-way highlight legibility:** can dense/SPLADE/fused overlaid on one map be read, or does it need small-multiples / a toggle?
4. **Honesty of the gold overlay:** does "elevated-and-relevant" hold up, or does min-max normalization / the α value distort what "elevated over dense" means?
