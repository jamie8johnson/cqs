# Roadmap

## Current: v1.29.1 (v1.29.0 audit close-out)

54 languages. 29 chunk types. v3 eval canonical, regenerated to v2 fixture 2026-04-17/20 (109 test / 109 dev). Daemon mode (`cqs watch --serve`, 99ms graph p50 / 200ms search-warm p50). Per-category SPLADE alpha routing — re-swept against R@5 in v1.28.3 (was R@1-tuned). **Centroid classifier active by default** (test R@5 +3.7pp from category-aware routing; opt-out via `CQS_CENTROID_CLASSIFIER=0`). GPU-native CAGRA bitset filtering (patched cuvs 26.4). Schema v22 (umap_x/umap_y, opt-in via `cqs index --umap`). MSRV 1.95.

**v1.29.1** shipped 2026-04-24: patch release — v1.29.0 audit close-out. 147 findings triaged; 142 fixed. No new commands, no schema bump, no reindex. Cagra SIGSEGV root-caused (missing `Drop` on `GpuState`) + fixed; `cqs serve` security hardened (host allowlist, SQL caps, HTML escape, loopback `--open`); transaction integrity fixes (staleness / metadata / cache / HNSW persist); 13 new `CQS_*` env var knobs for thresholds (additive); `rustls-webpki` GHSA-high patch; reranker + daemon-socket + cross-project test coverage. Remaining 5 audit items (SEC-7 serve auth, EX-1 Commands trait, EX-3 LlmProvider trait, PF-9 suggest_tests BFS, RM-10 socket BufReader) split to issues #1096/#1097/#1098 + umbrella #1095. Full detail in `CHANGELOG.md`.

**v1.29.0** shipped 2026-04-23: feature release bundling three arcs.

- **`cqs serve` web UI** with four interactive views (2D / 3D / hierarchy / embedding cluster). Spec `docs/plans/2026-04-22-cqs-serve-3d-progressive.md`. Perf pass took first paint from ~60s → ~3-4s on the cqs corpus (SQL-side `max_nodes` cap, default 300 nodes, `cose` layout, gzip middleware, lazy 3D bundle). Cluster view requires `cqs index --umap` to populate UMAP coordinates (Python umap-learn, embedded `scripts/run_umap.py`).
- **`.cqsignore` mechanism** — cqs-specific exclusions on top of `.gitignore`. Drops the cqs corpus from 18,954 → 15,488 chunks (vendor minified JS + eval JSON), zero "Dropped oversized" parser warnings.
- **Slow-tests cron eliminated** — 5 of 16 subprocess CLI test binaries (113 tests, ~130 min nightly) converted to in-process `InProcessFixture`-based tests + 15-test `cli_surface_test.rs` for things that genuinely need a binary spawn. Net: ~2 min added to every PR instead of ~130 min nightly. `slow-tests.yml` workflow deleted; `slow-tests` Cargo feature kept for the 11 remaining stragglers (convert opportunistically).

Plus 2 Dependabot security bumps (openssl 0.10.78, rand 0.8.6).

**v1.28.3** shipped 2026-04-20: per-category SPLADE α re-sweep targeting R@5 (the 2026-04-15 sweep optimized R@1 — different optima in many categories). Two alpha changes ship — `behavioral` 0.00 → 0.80, `multi_step` 1.00 → 0.10 — both essentially flipping a category from one extreme to the other. Production net: test R@5 +0.9pp, dev R@5 ±0, no regressions. The per-category sweep predicted +14pp held-out lift; ~8× dilution from classifier accuracy explains the gap (full analysis in `~/training-data/research/models.md`). Plus README cleanup (PR #1065).

**v1.28.2** shipped 2026-04-20: four correctness fixes from the Reranker V2 retrain arc — windowing fix (`chunks.content` was lossy WordPiece-decoded text for 7228/15616 chunks; PR #1060), `cqs index --force` fail-fast vs running daemon (#1061), `cqs notes list` daemon dispatch (#1062), `cli_review_test` `--format` → `--json` migration miss (#1063, fixes 2-day-red slow-tests nightly). Plus: reranker pool cap default 100→20, centroid classifier flipped default-on after isolated A/B (test R@5 +3.7pp), `notes_boost_factor` measured (zero impact, default unchanged). Reindex required to refresh stored content from raw source.

### Eval baselines on v3.v2 (post-v1.28.3, 2026-04-20)

| Split | R@1 | R@5 | R@20 | Notes |
|---|---|---|---|---|
| **test (n=109), v1.28.3 stage-1 + classifier ON** | 40.4% | **64.2%** | 82.6% | 16,026-chunk corpus, post-incremental-drift |
| test (n=109), v1.28.2 reverted alphas (same corpus) | 40.4% | 63.3% | 82.6% | A/B baseline |
| **dev (n=109), v1.28.3 stage-1 + classifier ON** | 40.4% | 73.4% | 87.2% | same corpus |
| dev (n=109), v1.28.2 reverted alphas (same corpus) | 41.3% | 73.4% | 87.2% | A/B baseline |
| canonical (v1.27.0 shipping) test / dev R@5 | – | 63.3% / 74.3% | 80.7% / 86.2% | for delta reference |

Net v1.28.3 Δ vs canonical: test R@5 **+0.9pp** (smaller than v1.28.2 morning baseline of 67.0% because of incremental index drift; see "Per-Category SPLADE Alpha Re-Sweep — Target R@5" in `models.md`), dev R@5 −0.9pp (drift, not config). The v1.28.3 alpha changes are net-positive on test, neutral on dev when measured on the same drifted corpus.

### Per-category R@5 sweep — what didn't ship and why

The R@5 re-sweep also surfaced direction-stable but small-magnitude moves on `cross_language` (0.10 → 0.40, n=11 per held-out split — small N + the current 0.10 is already well-placed by the prior R@1 sweep). And inconsistent / noisy optima on `identifier_lookup`, `conceptual`, `negation`, `structural`, `type_filtered`. None of those clear the cross-split-agreement + magnitude bar that `behavioral` and `multi_step` did. The dilution analysis (sweep gain × classifier accuracy) suggests per-category α tuning is now bottlenecked by classifier accuracy, not the alpha grid — future R@5 work should target classifier accuracy first.

---

## Active

### GPU Lane

- [x] **Reranker V2 — code-trained cross-encoder, 2026-04-17/18 pass.** Phase 1 calibration: 1k Gemma+Claude triples → 98.3% inter-rater agreement → GEMMA_ONLY decision (PR #1031). Phase 2: 200k Stack v2 hard-negative triples labeled by Gemma 4 31B AWQ on A6000 (12h45m wall, 95.31% overall agreement, 0 parse errors, balanced across 9 langs). Phase 3: trained `microsoft/unixcoder-base` + BCE on 382k pointwise rows. **Result: −24pp R@5 on v3.v2 test.** Even at smallest pool: −4.6pp R@5. Weights stay local at `~/training-data/reranker-v2-unixcoder/`.

  **Post-mortem (full detail `~/training-data/research/reranker.md`):**
  1. TIE labels were dropped from pointwise → trained on binary, weaker signal than BiXSE assumes
  2. Domain shift: trained on raw Stack v2 chunks, deployed on cqs's enriched chunks (NL desc + signature + content + doc)
  3. Pool-size brittleness: `(limit*4).min(100)` over-retrieves; weak rerankers get amplified

  All three are fixable but combined ~1-2 weeks. Not currently top priority. The "ms-marco net negative" result still stands for off-the-shelf rerankers; we now also have the matching result for in-domain-trained rerankers when domain isn't actually matched.

- [x] **ColBERT 2-stage rerank — tested 2026-04-17/18.** `mixedbread-ai/mxbai-edge-colbert-v0-32m` (Apache-2.0, 32M, beats ColBERTv2 on BEIR) via PyLate. Three modes (pure replacement, RRF fusion, alpha sweep). **Test α=0.9: R@5 +2.8pp; dev α=0.9: R@5 +0.9pp.** Test gain didn't fully replicate on dev; only R@20 improves consistently. Eval tool shipped (PR #1037), default OFF in production. Rust integration deferred — gains too marginal/inconsistent to justify the work.

- [x] **Chunker doc fallback for short chunks — landed 2026-04-18 (PR #1040).** `extract_doc_fallback_for_short_chunk` in `src/parser/chunk.rs` plus blank-line tolerance in `extract_doc_comment` close the `truncated_gold` failure mode (chunks <5 lines that ship without leading comment context). 10 happy/sad-path tests; reindex required. **Test R@5 +3.7pp vs canonical (63.3% → 67.0%); dev R@5 −2.7pp** (74.3% → 71.6%) — interlocked with LLM summary regen (5,486 → 7,018 cached, 47.7% coverage). The dev regression and R@20 movement on both splits are partly corpus-pruning artifact (16,095 → 14,734 chunks during reindex); follow-up A/B with a third reindex would isolate.

- [x] **Reranker V2 retrain with post-mortem fixes — tested 2026-04-20, PARKED.** Executed all three fixes: hard-negatives mined from cqs's own v3_pools (9175 cqs-domain graded training rows), TIE labels preserved as 0.5, pool cap lowered default 100→20. Trained UniXcoder three ways: pointwise BCE unweighted, pointwise BCE with auto `pos_weight=3.28`, pairwise `MarginRankingLoss(margin=0.3)`. **All three converged on −5 to −9pp R@5** (unweighted: −6.4 test / −7.3 dev; weighted: −5.5 / −9.2; pairwise: −5.5 / −9.2). R@20 unchanged across all three — gold IS in pool, weak score head consistently demotes it. Pairwise hit 98% train accuracy → fits train pairs perfectly, doesn't generalize. Conclusion: 326 queries × ~30 candidates is too thin to fine-tune a 125M cross-encoder against hard stage-1 negatives. Bottleneck is corpus size + base strength, not loss choice. Shippable wins (windowing + pool cap default + eval tooling) landed in v1.28.2 (PR #1060). Tooling kept: `evals/label_reranker_v3.py`, `evals/rerank_ab_eval.py`, `evals/train_reranker_v2_pairwise.py`. Next attempt would need 10x more queries (Gemma-augmented synthetic) or 5x bigger base (bge-reranker-large at ~3x latency).

### CPU Lane

**Retrieval quality:**
- [x] **Query-time HyDE — tested 2026-04-20, CATASTROPHIC.** Per `evals/hyde_per_category_eval.py`: generate synthetic Rust code via Gemma 4 31B per query, search with synthetic as the query string. **R@5 = 0.0% across all 8 categories on both test and dev splits** (vs baseline 65-95% per category). Inspecting samples: synthetic code is generic Rust/SQL with zero cqs-specific identifiers (e.g. for "table named notes AND columns with NOT NULL constraint" Gemma generated a generic `CREATE TABLE notes (id INTEGER PRIMARY KEY, ...)` — has nothing in common with cqs's actual schema chunks). Search returns generic-looking chunks; gold is never matched. The v2-era HyDE result that motivated the experiment was index-time, not query-time, so we tested the wrong direction.

  **Index-time HyDE re-eval still open.** cqs already has `cqs index --hyde-queries` that adds LLM-generated "queries that would find me" strings to each chunk at index time. The 2026-04-08 measurement on v2_300q showed +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral — net negative on R@1 in a single-config measurement. Per-category routing (only enable hyde-augmented chunks for queries where the v3 sweep says it helps) was never tried. Properly testing this requires: (1) regenerate HyDE for all chunks via the existing Claude Batches pipeline, (2) reindex with `--hyde-queries`, (3) per-category A/B harness that toggles the hyde-augmented embedding column. ~1 day. Lower expected lift than the categorization improvements above; promote only if classifier/regression work plateaus.

- [ ] **Expand the v3 label set with Gemma-generated synthetic queries.** Current v3 train + dev + test = 544 queries. Categorical optimization (alpha sweep, distilled classifier, per-query α regression) is data-bound past 50-100q per category. Generate ~5-10k more via the existing chunk-driven pipeline (`evals/generate_from_chunks.py`), classified self-consistently via Gemma. Bias generation toward thin categories (`conceptual` 0% rule fire, `negation` small-N noise on test). Prerequisite for the distilled classifier and per-query α regression items above. ~1 day of compute (Gemma already up via vLLM); negligible engineering since the pipeline is already working.

- [ ] **Context-aware classification.** Currently the router classifies the query in isolation. Add features available at query time: index language distribution (Rust-heavy vs Python-heavy vs polyglot), project category if known, top-N most-recently-searched terms. The intuition: same query in different project shapes might want different α (e.g., "function with retry" in a Go project routes to behavioral, in a Rust project might route to structural because Rust queries are more often structural in nature). Cheap to add as additional input dims to the distilled classifier or per-query α regression heads (no separate model needed). Effort: ~1 day after the distilled head is in place. Speculative ceiling — could be 0pp if context doesn't predict, or +3-5pp if there's signal we're not using. Also unlocks better behavior when an index spans heterogeneous projects (refs).

- [ ] **Soft routing — distribution over categories instead of argmax.** Today the classifier returns a single `QueryCategory`, the router picks `α(category)`, and a marginal misclassification fully swaps the alpha (e.g., behavioral=0.80 vs structural=0.90 — close enough, but multi_step=0.10 vs structural=0.90 if the classifier puts a multi_step query in `structural` is catastrophic). Soft routing: classifier outputs `P(c)` per category, effective α = `Σ P(c) × α(c)`. A query that's 60% behavioral / 30% structural / 10% multi_step gets α = 0.6×0.80 + 0.3×0.90 + 0.1×0.10 = 0.79.

  **Why now**: this whole arc is fundamentally a classification-and-routing problem. Hard routing throws away the classifier's confidence — even today's centroid classifier internally has soft cosine scores per category, but we softmax → argmax → pick one. Soft routing reuses that signal end-to-end.

  **Compatible with everything**: works on rule+centroid (use centroid cosines as the soft distribution), works on the distilled head (softmax outputs natural), works on the per-query α regression (the regression IS already producing a soft α). Probably a half-day in `src/search/router.rs` to wire centroid-based soft routing today; the rest follows for free when the distilled head lands.

  **Risk**: mixing alphas may attenuate their effect — if behavioral wants 0.80 and structural wants 0.90, mixing gives 0.85, which might be in the "neither helps much" middle ground. Worth measuring with a synthetic test where we know the true category from fixture metadata.

  **Pairs particularly well with the per-query α regression**: train on a soft target (R@5-weighted distribution over categories) instead of a hard one-hot, which gives the model nuanced training signal.
- [ ] **BGE → E5 v9-200k.** Clean-index eval ties on R@1, slight edge on R@5/R@20, 1/3 the embedding dim (768 vs 1024). Embedder abstraction gate ([#949](https://github.com/jamie8johnson/cqs/issues/949)) closed — `ModelConfig` now carries `input_names` / `output_name` / `pooling` / `tokenizer`; adding a model is config-only. Pending the embeddings cache + slots infrastructure (`docs/plans/2026-04-24-embeddings-cache-and-slots.md`) which makes paired-reindex A/Bs cheap. **Protocol note (2026-04-21):** PR #1071 measured HNSW reconstruction noise at ~4pp R@5 on v4 N=1526. Any embedder swap A/B must use paired-reindex baselines or fixed-seed HNSW to isolate signal — without that protocol, embedder-swap deltas under 5pp are noise.
- [ ] **Index-time HyDE re-eval** — never tested at proper N. v2-era single-config measurement showed +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral. Per-category routing (only enable hyde-augmented chunks for categories where sweep says it helps) was never tried. Properly testing requires (1) regenerate HyDE for all chunks via Claude Batches pipeline, (2) reindex with `--hyde-queries`, (3) per-category A/B harness toggling the hyde-augmented embedding column. Lower expected lift than the now-exhausted classifier work; cheap if revisited.

### Empirically closed: alpha-routing arc (2026-04-20 → 2026-04-21)

The classifier-accuracy / per-query-α-regression / soft-routing / fused-head family was systematically tested at proper N (v3 + v4, v4 = 1526 per split). Definitive null result across all variants. Documented in PR #1069 (research artifacts + post-mortems) and PR #1071 (long-chunk doc-aware windowing post-mortem with HNSW-noise meta-finding). Highlights:

| Lever | v3 R@5 (n=109) | v4 R@5 (n=1526) | Verdict |
|---|---|---|---|
| Distilled head (88.1% val acc, retrained on v3+synth) | test ±0 / dev +0.9 | test -0.3 / dev ±0 | parked |
| Fused head (continuous α + corpus fingerprint, contrastive ranking) | test ±0 / dev -0.9 | test -0.4 / dev +0.2 | parked |
| HyDE query-time | test -12.8 / dev -22.0 | test -10.7 / dev -9.8 | killed |
| Long-chunk doc-aware windowing | n/a | test +0.2 / dev +0.1 | neutral (within noise) |

Core finding: R@5 is alpha-insensitive on this corpus state across [0, 1]. The Oracle test's +9.2pp ceiling came from category-driven per-category default flips, not continuous-α refinement. **Routing-side levers (which α, which category, which routing weight) are exhausted.** Future R@5 work should target signal-side levers (chunking, embedder, multi-granularity index) under paired-reindex protocol.

**Embedder swap workflow (repeatable model A/B):**
- [x] **Index-aware embedder resolution.** Shipped — `src/cli/store.rs:138` `model_config()` reads `Store::stored_model_name()` and overrides env/CLI via `ModelConfig::resolve_for_query`. Closes the `CQS_EMBEDDING_MODEL=foo` foot-gun where queries against a `bar`-model index silently returned zero results.
- [x] **Embedder abstraction.** [#949](https://github.com/jamie8johnson/cqs/issues/949) closed. `ModelConfig` carries `input_names` / `output_name` / `pooling` / `tokenizer`; non-BERT models (Jina v3, Stella, GTE, custom) can be added as config entries rather than encoder forks.
- [ ] **Content-keyed embeddings cache + named slots** — single spec `docs/plans/2026-04-24-embeddings-cache-and-slots.md`. Adds `.cqs/embeddings_cache.db` keyed by `(chunk_hash, model_id)` + project-level `.cqs/slots/<name>/` directories + `cqs slot {list,create,promote,remove,active}` subcommand + `--slot` / `CQS_SLOT` on every major command + one-shot migration of existing `.cqs/index.db`. Single PR; ~38 new tests. Unblocks cheap repeated embedder A/Bs and side-by-side model comparison.

**Daemon:**
- [ ] **Daemon: full CLI parity** — subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification.

**Features (queued, no immediate work):**
- [ ] **Temporal search — `cqs history`** — query by author + time range, ranks by how little a chunk's been touched since. Uses git log + file/line mapping.
- [ ] **Author-weighted search** — `cqs search "..." --author X --boost 0.5`. Complements temporal search.
- [ ] **Auto-notes on commit** — post-commit hook runs `cqs notes add` with message + changed chunk names. Sentiment inferred from heuristics with override flag.
- [ ] **Config file support** — `[splade.alpha]` per-category overrides in `.cqs.toml`.
- [ ] **Phase 6: Explainable search** — depends on SPLADE-Code being production default. Spec: `docs/plans/adaptive-retrieval.md`.
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish + adaptive retrieval results.

**Agent adoption:**
- [ ] **Slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + pointer to `cqs --help`. Measure with telemetry before/after.
- [ ] **Composite search results** — `cqs search` returns mini-impact (caller + test counts) alongside each result. One call instead of search + impact.
- [ ] **`cqs trace "query"`** — show every routing decision: classifier → category → strategy → α → SPLADE top-K → dense top-K → RRF fusion → final ranking. Today understanding why a query ranked X requires `RUST_LOG=debug` + log grepping. Bigger lift; design separately. The agent-facing version of "explain my retrieval".
- [ ] **`cqs repl`** — interactive prompt (sqlite-shell-style) for iterating queries + ad-hoc exploration without `cqs batch` JSONL friction. Persistent connection to daemon, command history, in-line config tweaks. Replaces the heredoc-into-batch dance for exploratory work.

### Cross-Project Architecture

N-project via `[[reference]]` entries → `CrossProjectContext { stores: Vec<NamedStore> }`. Per-store BFS (callers, callees, impact, trace) matches cross-boundary edges by exact name only — wrappers, re-exports, and name mismatches are invisible.

- [ ] Type-signature matching for cross-boundary edges
- [ ] Import-graph resolution (parse `use`/`import` to resolve re-exports)
- [ ] Cross-project search with unified scoring (not just per-store RRF merge)
- [ ] `analyze_impact_cross` resolve file/line from CallGraph (currently empty paths — CQ-3)
- [ ] Cross-project dead code detection

### Agent Adoption — Telemetry

49,242 cqs invocations at `~/.cache/cqs/query_log.jsonl` since 2026-04-06 (snapshot 2026-04-16). 328 unique real queries (99% duplicate rate); 99.9% are `search`.

Historical split (2026-04-09, 16,731 invocations): **main conversation** uses `search` (60%) + `context` (28%) heavily, `impact`/`callers` almost never (0.2% each). **Subagents** drive nearly all `impact`/`callers`/`test-map`/`dead`/`gather` usage. Pre-edit hook bridges the gap by running `impact` automatically.

---

## Open Issues

Re-audited 2026-04-21 against actual GitHub state. **All Tier 1 + Tier 2 issues from the 2026-04-16 audit have shipped** (across PRs #1041, #1045, #1046 in the v1.27 → v1.28 audit-fix waves). Remaining work is the 6 P4 deferrals from the post-v1.27.0 audit + 5 hard-blocked Tier 3 items.

**P4 deferrals (small effort, low impact — opportunistic):**

| # | Finding | Notes |
|---|---------|-------|
| [#1042](https://github.com/jamie8johnson/cqs/issues/1042) | `WINDOW_OVERHEAD` doesn't scale with embedder prefix length | constant tuning |
| [#1047](https://github.com/jamie8johnson/cqs/issues/1047) | `ChunkType::human_name` catch-all hides multi-word variant omissions | compile-time enforcement |
| [#1048](https://github.com/jamie8johnson/cqs/issues/1048) | `try_daemon_query` strict-string output parsing | future-proof refactor |
| [#1049](https://github.com/jamie8johnson/cqs/issues/1049) | Pin `fallback_does_not_mix_comment_styles` with explicit test | tiny test pin |
| [#1043](https://github.com/jamie8johnson/cqs/issues/1043) | `is_slow_mmap_fs` ignores Windows network drives + reparse points | Windows-specific edge case |
| [#1044](https://github.com/jamie8johnson/cqs/issues/1044) | Native Windows `cqs watch` cannot stop cleanly — DB corruption risk | Windows-specific signal handling |

**Tier 3 (blocked on external factors):**

| # | Finding | Blocker |
|---|---------|---------|
| [#956](https://github.com/jamie8johnson/cqs/issues/956) | ExecutionProvider: CoreML/ROCm decouple | needs non-Linux CI |
| [#255](https://github.com/jamie8johnson/cqs/issues/255) | Pre-built reference packages | signing/registry design |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | HNSW mmap | needs lib swap to hnswlib-rs (nightly-only) |
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | mmap SPLADE body | smaller win than originally claimed |
| [#106](https://github.com/jamie8johnson/cqs/issues/106) | ort 2.0-rc.12 stable release | upstream (pykeio) |

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26.
- **Astro, ERB, EEx/HEEx, Move** — no tree-sitter grammar on crates.io.
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork.
- **ArchestrA QuickScript** — needs custom grammar from scratch.

---

## Parked

- **Code-specific embedder A/B — `nomic-ai/CodeRankEmbed` + `nomic-ai/nomic-embed-code`.** Two open-weight code-specialized embedders from nomic-ai. Both use the same query prefix (`"Represent this query for searching relevant code: "`) and no document prefix; same training corpus (CoRNStack, ~21M pairs). cqs already has separate `embed_query` / `embed_documents` paths so wiring is a model preset + prefix metadata.
  - **CodeRankEmbed** — 137M params, MIT, base = Snowflake Arctic Embed M Long, 768-dim (matches v9-200k schema, no migration), 8192-token context (long-chunk win for free). Released late 2024. Headline: 77.9 MRR on CSN, 60.1 NDCG@10 on CoIR — best at 137M class. Right-shape default candidate; ~2-hr A/B against v9-200k on the v3 fixture.
  - **nomic-embed-code** — 7B params, Apache 2.0, base = Qwen2.5-Coder-7B, GGUF quantizations available. Released March 2025. Beats Voyage Code 3 + OpenAI Embed 3 Large on CodeSearchNet. ~14 GB VRAM at FP16 (fine on A6000, painful on consumer cards); embedding is probably 3584-dim (Qwen2.5-Coder hidden size = 4.5× our current storage). Right shape for an opt-in "best quality, big GPU" preset, not the default.
  - Pre-req: nothing (wiring is independent of perf / slow-tests work). Run after current GPU/CPU lane items + perf + slow-tests are clear.
- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`.
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`.
- Wiki system (agent-first), MCP server (re-add when CLI solid), pre-built reference packages (#255), Blackwell RTX 6000 (96GB), L5X files from plant, KD-LoRA distillation (CodeSage→E5), ColBERT late interaction, enrichment-mismatch mining (Exp #4), lock/fork-aware training weights (Exp #5), ladder logic (RLL) parser, DXF/Openclaw PLC, SSD fine-tuning experiments.

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.29.0 | **`cqs serve` + `.cqsignore` + slow-tests cron killed.** Interactive web UI for the call graph with 4 views — 2D Cytoscape, 3D force-directed, hierarchy (Y axis = BFS depth), embedding cluster (X/Z = UMAP, Y = caller count). Schema bumps v21→v22 for `umap_x`/`umap_y` columns; opt-in via `cqs index --umap` (Python umap-learn). Serve perf pass: ~60s → ~3-4s first paint (SQL-side max_nodes cap, default 300 nodes, `cose` layout, gzip, lazy 3D bundle). New `.cqsignore` mechanism layered on `.gitignore` (drops 18,954 → 15,488 indexed chunks on the cqs corpus, all noise). 5 of 16 slow-test binaries converted to in-process `InProcessFixture`-based tests; nightly `slow-tests.yml` cron deleted. Two Dependabot security bumps (openssl 0.10.78, rand 0.8.6). |
| v1.28.0 | **Post-audit release.** Closes the post-v1.27.0 16-category audit: 150 findings landed across PRs #1041 (P1, 26) / #1045 (P2, 47) / #1046 (P3, 69); 6 deferred items filed as issues #1042-#1044, #1047-#1049. **BREAKING:** uniform JSON envelope across CLI/batch/daemon-socket (PR #1038). Schema v21 adds `parser_version` column on chunks (PR #1040 + P2 #29). 17 new env-var knobs. Daemon defaults tuned. Eval bumps: PR #1040 chunker doc fallback for short chunks → test R@5 63.3% → 67.0% (vs canonical). PR #1037 ColBERT 2-stage eval tool (default OFF, marginal/inconsistent gain). PR #1039 rustls-webpki CVE bumps. |
| v1.26.0 | **Watch + SPLADE hardening + Wave D–F audit batch.** `cqs watch` respects `.gitignore` (#1002, PR #1006). Incremental SPLADE in watch (#1004, PR #1007) — 100% coverage stays. Per-category α re-fit on clean 14,882-chunk index (PR #1005, +1.8pp R@1 on v2). `--splade` CLI flag respects router (PR #1008). `Store::open_readonly_after_init` replaces unsafe `into_readonly` (#986, PR #998). **Refactor lane** #946–#950 all closed (PRs #981–#985): Store typestate, Commands/BatchCmd unification, `cqs::fs::atomic_replace` helper, embedder model abstraction, CAGRA persistence. **Quick-wins lane**: WSL 9P/NTFS mmap auto-detect + CAGRA itopk envs + reranker batch chunking (#961/#962/#963, PR #979). **Wave D–F batch**: Aho-Corasick language_names (#964, PR #992), dispatch_search content tests (#973, PR #997), shared `Arc<Runtime>` (#968, PR #1000), migration fs-backup (#953, PR #996), NameMatcher ASCII fast path (#965, PR #990), `open_readonly_small` (#970, PR #993), reindex drain-owned chunks (#967, PR #991), `INDEX_DB_FILENAME` constant (#923, PR #994), CAGRA sentinel `INVALID_DISTANCE` (#952, PR #995), daemon `try_daemon_query` test scaffold (#972, PR #999). **Eval expansion**: v3 consensus dataset (544 dual-judge queries, train/dev/test 326/109/109, every category N≥23). |
| v1.25.0 | **11th full audit** (16 categories, 236 findings). Per-category SPLADE α defaults from clean 21-point sweep. Multi_step router fix (`"how does"` → not Behavioral, +0.7pp). Eval output to `~/.cache/cqs/evals/` (#943, root cause of 2 days of eval drift). Notes daemon-bypass routing (#945). Determinism fixes across 15+ sort sites + GC suffix-match bug (81% chunks orphaned, root cause of v1.24.0 → v1.25.0 R@1 inflation). |
| v1.24.0 | GPU-native CAGRA bitset filtering (upstream PR rapidsai/cuvs#2019), daemon stability (CAGRA non-consuming search fixes SIGABRT under load), cagra.rs simplified −357 lines, batch/daemon base index routing, router update (type_filtered + multi_step → base), cuVS 26.4. |
| v1.23.0 | **Daemon mode** (`cqs watch --serve`, 3-19ms queries), per-category SPLADE α routing + 11-point sweep, persistent query cache, shared runtime, AC-1 fusion fix, 90 audit findings. |
| v1.22.0 | Adaptive retrieval Phases 1-5 (classifier + routing + dual base/enriched HNSW), SPLADE-Code 0.6B eval chain (null result), SPLADE index persistence (#895), v19/v20 migrations (#898/#899), read-only batch store (#919), `Store::clear_caches` (#918). |
| v1.21.0 | Cross-project call graph (#850), 4 new chunk types to 29 (#851), chunk type coverage across 15 languages (#852), 14-category audit 40+ fixes (#859), API renames + 8 batch flags (#860), paper v1.0, docs refresh. |
| v1.20.0 | 14-category audit (71 findings, 69 fixed), Elm (54th), batch `--include-type`/`--exclude-type`, SPLADE code training (null), env var docs, README eval rewrite. |
| v1.19.0 | Include/exclude-type, Java/C# test+endpoint, batch `--rrf`, capture list unification, Phase 2 chunks, 265q eval, store dim check. |
| v1.18.0 | Embedding cache, 5 chunk types, v2 eval harness, batch query logging. |
| v1.17.0 | SPLADE sparse-dense hybrid, schema v17, HNSW traversal filtering, ConfigKey, CAGRA itopk fix. |
| v1.16.0 | Language macro v2, Dart (53rd), Impl chunk type. |
| v1.15.2 | 10th audit 103/103, typed JSON output structs, 35 PRs. |
| v1.15.1 | JSON schema migration, batch/CLI unification. |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT. |
| v1.14.0 | `--format text\|json`, ImpactOptions, scoring config. |
| v1.13.0 | 296-query eval, 9th audit, 16 commands. |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap. |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes. |
