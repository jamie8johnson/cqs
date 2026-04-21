# Roadmap

## Current: v1.28.3 (R@5-targeted alpha tweaks for behavioral + multi_step)

54 languages. 29 chunk types. v3 eval canonical, regenerated to v2 fixture 2026-04-17/20 (109 test / 109 dev). Daemon mode (`cqs watch --serve`, 99ms graph p50 / 200ms search-warm p50). Per-category SPLADE alpha routing — re-swept against R@5 in v1.28.3 (was R@1-tuned). **Centroid classifier active by default** (test R@5 +3.7pp from category-aware routing; opt-out via `CQS_CENTROID_CLASSIFIER=0`). GPU-native CAGRA bitset filtering (patched cuvs 26.4). MSRV 1.95.

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
- [ ] **HyDE for structural queries — most promising untested lever.** v2-era data: +14pp structural, +12pp type_filtered, −22pp conceptual, −15pp behavioral. Router → LLM generates synthetic code → embed → search, per-category by design. Treat v2 numbers as motivation, not promise: this session saw several wins vanish through the full router (centroid, reranker v2, full alpha sweep). Design the experiment to hold the production router fixed and vary only the query embedding source. Prereqs already built (Gemma 4 31B via vLLM, BGE embedder, v3 eval harness).
- [ ] **BGE → E5 v9-200k.** Clean-index eval ties on R@1, slight edge on R@5/R@20, 1/3 the embedding dim (768 vs 1024). Gated on [#949](https://github.com/jamie8johnson/cqs/issues/949) (embedder abstraction) + v3 re-run to rule out noise.
- [ ] **CAGRA filtering regression on enriched index.** Fully-routed v1.24.0: conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release. Hypothesis: CAGRA graph walk strands in filtered-out regions. Concrete proposal in [#962](https://github.com/jamie8johnson/cqs/issues/962).
- [~] **Classifier accuracy — SCOPE REOPENED 2026-04-20.** The earlier "SCOPE REDUCED" call was correct in a world where per-category α tuning was R@1-targeted (and gave small gains). The 2026-04-20 R@5 re-sweep changed the picture: per-category α has substantial latent R@5 signal (sweep predicted +14pp held-out lift) but ~8× of it dilutes through the rule-based + centroid classifier (production reality: +0.9pp test, ±0 dev). **Classifier accuracy is now the bottleneck on R@5, not the alpha grid.**

  **Audit data (v3 dev, 109 queries; `cargo test --test classifier_audit`):**

  | v3 label | N | Fire% | Correct% | Prec-when-fires |
  |---|---|---|---|---|
  | negation | 17 | 100% | 100% | 100% ✓ |
  | cross_language | 11 | 82% | 82% | 100% ✓ |
  | identifier_lookup | 18 | 61% | 61% | 100% (recall gap, α=1.0 already optimal) |
  | structural | 8 | 50% | 38% | 75% |
  | type_filtered | 13 | 46% | 8% | 17% (misfires into structural/conceptual) |
  | multi_step | 14 | 43% | 0% | 0% ("AND" caught by structural first) |
  | behavioral | 16 | 19% | 6% | 33% |
  | conceptual | 12 | 0% | 0% | 0% (abstract-noun patterns don't match v3) |

  Overall: 38.5% accurate, 49.5% fall to Unknown, 13.8% fire wrong. Dead paths from prior attempts: centroid matching (now active by default after alpha-floor fix in v1.28.2), logistic regression, fine-tuned MiniLM, runtime LLM classify (latency-prohibitive at 300-500ms/query), negation idiom rule-fix (no R@1 change). Details in `~/training-data/research/models.md`.

  **Next attempt: distilled query classifier on the BGE query embedding.** Use Gemma 4 31B Dense (already deployed via vLLM for the Reranker V2 labeling pipeline) as a teacher to label ~5-10k v3 queries (mix of telemetry + synthetic). Train a tiny classifier head — a single linear layer + softmax mapping the 1024-dim BGE query embedding to 9 categories (~10K params). Inference: one matmul on the embedding cqs already computes for search, <0.1ms additional latency. No new runtime dependencies.

  **Prerequisite measurement:** how well does Gemma 4 31B itself classify v3 queries? We've never measured it directly against v3 ground-truth labels. Run the 544 v3 queries through `evals/llm_client.py::classify`, compare to fixture labels, get per-category accuracy. This caps the distillation ceiling. ~5 min compute (cached).

  **Plausible accuracy projection** — DistilBERT-canon shows distilled students retain ~97% of teacher accuracy on NLP classification ([source](https://www.geeksforgeeks.org/nlp/introduction-to-distilbert-model/)). If the Gemma 31B baseline lands at 85% on v3 (likely range 75-90% — the v3 taxonomy has hard boundaries between negation/structural and multi_step/structural), the distilled head should hit ~80-83%. Vs current production classifier at 38% accuracy.

  **Estimated R@5 lift** — at ~80% classifier accuracy, the v1.28.3 alpha changes alone would compound from the current +0.9pp test R@5 to roughly **+8-10pp test R@5** (sweep-predicted +14pp × 0.6-0.7 dilution from 80% accuracy, vs current ~8× dilution from 38% accuracy). Plus opens the door for retuning the 5 categories whose sweep optima were noisy partly because of bad classifier-routed training data.

  **Smaller Gemma 4 variants — E2B (Effective 2B), E4B, 26B MoE** — could be runtime classifiers if latency budget allows. E2B at ~30-60ms via vLLM is still 10-20× the daemon hot path. Distillation to a head on cqs's existing BGE embedding is the more aggressive path: <0.1ms and no new runtime dependency.

  **Estimated effort:** 1-2 days (Gemma 31B already up via vLLM; labeling pipeline already proven from Reranker V2 calibration; training head is ~50 LOC of PyTorch; eval harness in place via `evals/classifier_ab_eval.py`).

- [ ] **Per-query α regression — skip the taxonomy entirely.** Alternative or companion to the distilled classifier above. The 9-way category taxonomy is a lossy compression of an underlying continuous α surface — categories were a human-designed clustering of query types, but the model doesn't actually need them. Train a tiny MLP (BGE 1024-dim query embedding → α ∈ [0, 1]) directly. Prediction: one matmul + sigmoid, <0.1ms additional latency. Same runtime cost as the distilled head, but skips the categorical bottleneck.

  **Training data already exists.** The v1.28.3 R@5 sweep produced 11,424 labeled examples (544 queries × 21 alphas, R@5 outcome per (query, α) pair) sitting in `evals/queries/v3_alpha_sweep_r5{,_test,_dev}.json`. Per-query "best α" is `argmax_α R@5(query, α)`. For queries where multiple alphas tie at the optimum, regress against the centroid of the tied range to bias toward stable predictions.

  **Why this might beat the distilled head:** category boundaries are fuzzy. A query that's 60% behavioral / 30% structural / 10% multi_step gets routed to one bucket and gets that bucket's α — which is wrong for queries near the boundary. Direct α regression sees the query and picks the right point on the [0, 1] surface without intermediate quantization.

  **Why it might not:** single-query R@5 outcomes are noisy (per-query R@5 ∈ {0/n, 1/n, ..., n/n} for tiny per-query sample size). The category abstraction provides natural smoothing across a population. The regression target may need windowing or smoothing to be learnable.

  **Combine with the distilled head?** Possibly — train both on the same Gemma-labeled dataset, ensemble. Distilled head gives interpretable category routing for telemetry/debugging; α regression captures fine-grained tuning. Or use the regression as a tie-breaker when the distilled head's top-1 vs top-2 confidence margin is small.

  **Estimated effort:** 1-2 days, shares infrastructure with the distilled head (same labeling pass would label both targets simultaneously). ~75 LOC of PyTorch.

**Testing infrastructure:**
- [ ] **Rewrite slow CLI test binaries to in-process fixtures** ([#980](https://github.com/jamie8johnson/cqs/issues/980)). `cli_batch_test`, `cli_graph_test`, `cli_commands_test`, `cli_test`, `cli_health_test` gated behind `slow-tests` feature (PR #988) because each shells out to `cqs` and cold-loads the ONNX/HNSW/SPLADE stack per test case (~118 min combined on PR CI). Follow the `cli_notes_test` + `router_test` pattern: one `Store` + `CommandContext` per binary, call `cmd_*` handlers directly. Un-gates the feature and retires the nightly `slow-tests.yml` workflow.

**Embedder swap workflow (repeatable model A/B):**
- [ ] **Content-keyed embeddings cache.** New SQLite table `embeddings_cache(chunk_hash BLOB, model_id TEXT, embedding BLOB, PRIMARY KEY (chunk_hash, model_id))`. Index time: check cache before invoking the embedder. Re-embedding the same corpus with a different model only pays for cache misses. Disk cost: ~4KB × #chunks × #cached_models (~150MB for 2 models on cqs-sized projects). Turns "swap embedder + re-eval" from 20 min into ~30s on second/subsequent swaps.
- [ ] **Index-aware embedder resolution.** Trust `index.db`'s recorded model at query time instead of re-checking `CQS_EMBEDDING_MODEL`. The env var would only matter at `cqs index` time. Eliminates a class of "I changed the env, now everything's broken" foot-guns. ~1 day, almost zero new surface.
- [ ] **Named index slots** — `cqs index --slot v9-200k --model v9-200k` builds at `.cqs/slots/v9-200k/`. `cqs --slot v9-200k "query"` queries that slot. `cqs slot promote v9-200k` swaps the active pointer. Build only after the cache lands — without it, slot-switching still pays the reindex cost.

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

Audited 2026-04-16 post-v1.26.1 — see [`docs/audit-open-issues-2026-04-16.md`](docs/audit-open-issues-2026-04-16.md) for full findings + fix prompts amended onto each issue body.

**Tier 1 (ship next, HIGH or MEDIUM/EASY):**

| # | Finding | Impact | Difficulty |
|---|---------|--------|-----------|
| [#917](https://github.com/jamie8johnson/cqs/issues/917) | Streaming SPLADE serialize (~60-100MB peak drop) | HIGH | MEDIUM |
| [#974](https://github.com/jamie8johnson/cqs/issues/974) | onboard + where: assert retrieval content | HIGH | MEDIUM |
| [#975](https://github.com/jamie8johnson/cqs/issues/975) | Always-on recall + staleness mtime semantics | HIGH | MEDIUM |
| [#954](https://github.com/jamie8johnson/cqs/issues/954) | Grammar-less parser dispatch via LanguageDef | MEDIUM | EASY |
| [#959](https://github.com/jamie8johnson/cqs/issues/959) | Collapse notes dispatch into single handler | MEDIUM | EASY |
| [#966](https://github.com/jamie8johnson/cqs/issues/966) | Stream-hash enrichment (blake3) | MEDIUM | EASY |
| [#969](https://github.com/jamie8johnson/cqs/issues/969) | Recency-based `last_indexed_mtime` prune | MEDIUM | EASY |
| [#971](https://github.com/jamie8johnson/cqs/issues/971) | HNSW self-heal dirty-flag integration test | MEDIUM | EASY |
| [#951](https://github.com/jamie8johnson/cqs/issues/951) | Re-benchmark README Performance table | MEDIUM | EASY |

**Tier 2 (bundle into audit-v1.26.0 wave, MEDIUM/MEDIUM):**

| # | Finding |
|---|---------|
| [#955](https://github.com/jamie8johnson/cqs/issues/955) | Compile-enforced ChunkType type-hint patterns |
| [#958](https://github.com/jamie8johnson/cqs/issues/958) | `define_query_categories!` macro — single source of truth |
| [#960](https://github.com/jamie8johnson/cqs/issues/960) | Per-LanguageDef structural pattern matchers |
| [#957](https://github.com/jamie8johnson/cqs/issues/957) | SPLADE/reranker preset registry |

**Tier 3 (deferred / blocked):**

| # | Finding | Blocker |
|---|---------|---------|
| [#956](https://github.com/jamie8johnson/cqs/issues/956) | ExecutionProvider: CoreML/ROCm decouple | needs non-Linux CI |
| [#255](https://github.com/jamie8johnson/cqs/issues/255) | Pre-built reference packages | signing/registry design |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | HNSW mmap | needs lib swap |
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | mmap SPLADE body | smaller win than claimed; depriorotized behind #917 |
| [#106](https://github.com/jamie8johnson/cqs/issues/106) | ort 2.0-rc.12 stable release | upstream (pykeio) |

**Closed during this audit:** #63 (audit.toml ignore already in place), #921 (watch-loop claim invalid; subsumed by #917).

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26.
- **Astro, ERB, EEx/HEEx, Move** — no tree-sitter grammar on crates.io.
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork.
- **ArchestrA QuickScript** — needs custom grammar from scratch.

---

## Parked

- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`.
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`.
- Wiki system (agent-first), MCP server (re-add when CLI solid), pre-built reference packages (#255), Blackwell RTX 6000 (96GB), L5X files from plant, KD-LoRA distillation (CodeSage→E5), ColBERT late interaction, enrichment-mismatch mining (Exp #4), lock/fork-aware training weights (Exp #5), ladder logic (RLL) parser, DXF/Openclaw PLC, SSD fine-tuning experiments.

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
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
