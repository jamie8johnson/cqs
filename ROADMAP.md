# Roadmap

## Current: v1.36.2 (cut 2026-05-04)

Patch release. No schema bump.

**v1.36.2 (2026-05-04):** critical fix — long-running `cqs index` runs no longer crash with `(code: 5) database is locked` when a concurrent short-lived `cqs` invocation overlaps the indexer's writes (#1451 `Store::drop` checkpoint TRUNCATE → PASSIVE; the indexer's WAL contention with `cqs stats` / similar polling was surfacing fatal mid-transaction `SQLITE_BUSY`). Plus `busy_timeout` 5s → 30s defense-in-depth (#1450) and 5 dependency bumps from dependabot.

**v1.36.1 (2026-05-04):** `qwen3-embedding-4b` preset (#1441/#1442) — 7.4 GB FP16, 2560-dim, 4096 max-seq. Production-ready first-class preset alongside the v1.36.0 8B ceiling probe.

**v1.36.0 (2026-05-03):** schema v25 → v26 (composite `(source_type, origin)` index on `chunks`; auto-migrated on first read-write open). Headline: per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge — v1.35.0 shipped EmbeddingGemma as the new default but inherited per-category α defaults that were tuned for BGE-large (2026-04-15/16). A fresh sweep on the gemma slot landed different optima: `Structural` 0.90→0.60, `Behavioral` 0.80→1.00, `Conceptual` 0.70→0.80, `TypeFiltered` 1.00→0.00, `CrossLanguage` 0.10→0.70, plus `Unknown` 1.00→0.80. Net agg lift: R@1 +1.8pp, R@5 +3.7pp, R@20 +2.4pp. Plus 13 audit-followup fixes including a critical bug catch (#1413): readonly opens with stale schema were attempting to migrate and failing with SQLite "attempt to write a readonly database" errors. Fixed by surfacing `SchemaMismatch` on stale-schema readonly opens.

**v1.35.0 (released 2026-05-02):** default embedder swap BGE-large → EmbeddingGemma-300m (308M, 768-dim, 2K context). Plus tokenizer-truncation correctness fix (#1384) that affected fine-tuned BERT-family presets (bge-large-ft, v9-200k, coderank).

**v1.34.0 (2026-05-02):** bundled the post-v1.33.0 audit close-out (24 fix PRs, 129 findings closed) plus pre-audit feature work — EmbeddingGemma-300m preset (#1301), `cqs eval --reranker` (#1303), `slow-tests` Phase 2 (#1302), ci-slow.yml stabilization.

**v1.33.0 (2026-05-02):** eval-matcher drift fix (#1284, ~38% of gold chunks were going invisible after audit-driven line shifts), placeholder-cache 30s startup tax fix (#1288, CI 38min→6min), chunk-orphan pipeline prune (#1283), `bge-large-ft` LoRA preset (#1289), daemon test refactor + nightly CI workflow (#1292, #1286 Phase 1).

**Eval baseline (v3.v2 218q dual-judge):**

| Config | Agg R@1 | Agg R@5 | Agg R@20 |
|---|---:|---:|---:|
| **embeddinggemma-300m + v1.36 α (current default)** | **50.9%** | **76.2%** | **88.6%** |
| embeddinggemma-300m + v1.35 α (BGE-tuned) | 49.1% | 72.5% | 86.2% |
| bge-large-ft (pre-retune) | 47.7% | 73.4% | 86.2% |
| BGE-large (pre-retune) | 47.2% | 72.0% | 84.4% |
| v9-200k (pre-retune) | 45.0% | 68.8% | 80.7% |
| nomic-coderank (pre-retune) | 45.0% | 67.9% | 78.9% |

Other rows are pre-retune; a 5-slot rerun under the new alphas is queued. Per-split numbers + per-category breakdowns + sweep methodology in `~/training-data/research/models.md` and `/tmp/gemma-alpha-sweep/`.

(Older release detail is in the Done table at the bottom + CHANGELOG.md.)

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
- [x] **BGE → E5 v9-200k — UN-RETIRED 2026-05-01.** Original 2026-04-25 verdict was "30pp behind, retired" but it turned out to be ~95% fixture-side artifact, not a model regression. The eval matcher in `eval/runner.rs` required strict `(file, name, line_start)` to score a gold chunk as matched, and v1.30.x audit waves had shifted line numbers in 42/109 test golds + 40/109 dev golds since the Apr 25 fixture refresh — so search returned the right chunks and the matcher counted them as misses. After loosening the matcher to `(file, name)` (this PR), v9-200k posts test R@1=45.9% R@5=70.6% R@20=80.7% / dev R@1=46.8% R@5=68.8% R@20=81.7% — **ties or marginally beats BGE-large on test R@5** (BGE 69.7% → v9 70.6%, +0.9pp), trails by ~8pp on dev R@5 (BGE 77.1% → v9 68.8%). For a model that's 1/3 the dim (768 vs 1024), 1/3 the params (~110M vs 335M), faster to embed, and already fine-tuned on cqs's call-graph data, it's back in serious contention. **Decision (2026-05-01): keep BGE-large as default.** Dev R@5 is the more reliable signal (advisory, not gating, but the larger gap there suggests v9 generalizes worse out-of-distribution), and BGE has years of upstream pre-training as a hedge against unknown query types. v9-200k stays available as an opt-in preset; it's the right model when memory or embed latency dominates over a few percentage points of R@5.
  - **Lesson (the user pushed back on this):** "if a benchmark number drops by 25pp overnight, that's bug-shaped, not model-shaped." Trust the prior baseline; investigate the harness before retiring the candidate. The fixture-line-drift symptom was already documented in PR #1109's post-mortem (Apr 25); we forgot the lesson and ate the same drift again 5 days later.
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

**Index backends (signal-side, recall-leaning):**
- [ ] **USearch backend** — same algorithm class as HNSW, but its tuning lets you push `ef_search` higher cheaply. We're at limit=20 with eval R@5 = 63.3% test / 74.3% dev; if recall is leaving points on the table (vs ranking), bumping recall via `ef_search` is the cheapest knob. USearch makes that knob less expensive. Plugs into the `IndexBackend` trait introduced by [#1131](https://github.com/jamie8johnson/cqs/issues/1131).
- [ ] **SIMD brute-force fast path under ~5K chunks** — exact cosine, recall = 1.0 by construction. Wouldn't move our 17K-chunk eval, but would close the small-project gap and remove an index-build variable from per-slot A/Bs. Plugs into `IndexBackend` as a higher-priority backend that fires when `chunk_count < threshold`.

**Embedder swap workflow (repeatable model A/B):**
- [x] **Index-aware embedder resolution.** Shipped — `src/cli/store.rs:138` `model_config()` reads `Store::stored_model_name()` and overrides env/CLI via `ModelConfig::resolve_for_query`. Closes the `CQS_EMBEDDING_MODEL=foo` foot-gun where queries against a `bar`-model index silently returned zero results.
- [x] **Embedder abstraction.** [#949](https://github.com/jamie8johnson/cqs/issues/949) closed. `ModelConfig` carries `input_names` / `output_name` / `pooling` / `tokenizer`; non-BERT models (Jina v3, Stella, GTE, custom) can be added as config entries rather than encoder forks.
- [x] **Content-keyed embeddings cache + named slots — shipped 2026-04-25 (PR #1105).** `.cqs/embeddings_cache.db` keyed by `(content_hash, model_id)` + project-level `.cqs/slots/<name>/` directories + `cqs slot {list,create,promote,remove,active}` + `cqs cache {stats,clear,prune,compact}` + `--slot`/`CQS_SLOT` on every major command + one-shot migration of `.cqs/index.db` → `.cqs/slots/default/`. Spec at `docs/plans/2026-04-24-embeddings-cache-and-slots.md`. Post-merge wiring fix added `cqs::resolve_index_db()` helper to handle 8 callers that built `.cqs/index.db` paths directly.
  - **Follow-ups filed:** [#1107](https://github.com/jamie8johnson/cqs/issues/1107) (`cqs slot create --model` validates but doesn't persist; must pass `--model` globally on every invocation), [#1108](https://github.com/jamie8johnson/cqs/issues/1108) (5 hot search SELECTs omit `content_hash`, ~2,180 warnings/eval, surfaced when running A/B evals on the new infra).
  - **A/B technique that works:** copy `llm_summaries` rows cross-slot by `content_hash` before `cqs index --llm-summaries`. Summary text is model-independent (NL describing the chunk); only the embedding into the slot's HNSW changes. Reduced API spend on coderank A/B prep to ~$0.03 (894 new summaries) and v9-200k to $0 (full overlap on eligible chunks).
- [x] **EmbeddingGemma-300m promoted to default in v1.35.0 (2026-05-02).** PR #1385 swapped the `define_embedder_presets!` `default = true` annotation from `bge_large` to `embeddinggemma_300m`. Apples-to-apples eval (after #1384 truncation fix): agg R@1=49.1% / R@5=72.5% / R@20=86.2%, beats BGE-large on R@1 by +1.9pp at 308M params / 768 dim / 2K context. BGE-large remains a first-class preset (`CQS_EMBEDDING_MODEL=bge-large`); existing slot indexes keep their stored model. TRT-RTX wiring is still a prerequisite for FP16 — TRT 10 fails the engine build on Gemma3's bidirectional-attention head plugin op; `CQS_DISABLE_TENSORRT=1` knob (#1301) falls through to CUDA EP at FP32. Full A/B writeup in `research/models.md`.
- [x] **Qwen3-Embedding-4B ceiling probe — closed 2026-05-04. Result: gemma-300m wins.** Probed Qwen3-Embedding-**4B** as a tractable proxy for the 8B (8B mmap risks on WSL still load-bearing, but 4B carries the same architecture: decoder-only, last-token pooling, instruct-prefix). Full enrichment + per-cat α tuning lifted qwen3-4b to test R@5 69.7% / dev R@5 77.1%, **still 2.7-2.8pp below gemma-300m's 72.5% / 79.8%** despite 13× the params and 3.3× the dim. The probe paid for itself in engineering — DB-lock root cause (#1451), FP16 dispatch (#1442), per-model α-set proof (#1453) — but the architecture finding is decisive: the Qwen3-Embedding family's MTEB-strong general-retrieval profile does not transfer to code search on this fixture. **Embedder-scale retired as a knob for cqs.** Full results in PROJECT_CONTINUITY.md "Right Now". Sweep artifacts: `/tmp/qwen3-sweep/{test,dev}-*.json` (capture before reboot).
  - 8B not run: same architecture, would consume another full day for a finding that's already conclusive at 4B. If it's ever revisited, the engineering envelope is now tested clean (FP16 dispatch + DB-lock fix + batch=1 + 30s busy_timeout + `Store::drop` PASSIVE all merged).
- [x] **NV-Embed-v2 ceiling probe — dropped 2026-05-04 by transitivity.** The Qwen3-4B result above generalizes: large general-purpose retrieval embedders underperform code-specialized small embedders on cqs's fixture. NV-Embed-v2 (8B Mistral base, 4096-dim, MTEB #1 at release) faces the same architecture-vs-domain mismatch plus higher engineering cost (custom pooling head, no community ONNX, our own export). Skip unless evidence emerges that NV-Embed-v2's Latent-Attention pooling specifically helps code retrieval — currently no such signal.
- [x] **llama-3.2-nv-embedqa-1b-v2 — considered, dropped 2026-05-03.** NVIDIA's commercial-OK Llama-based embedder (1B, Matryoshka 384–2048-dim, 8K context). Investigation found: no ONNX in the repo, custom `LlamaBidirectionalModel` architecture (`model_type: llama_bidirec`) needs `trust_remote_code=True`, would require authoring our own ONNX export including pooling + L2-normalize wrapper. 1-2 days of engineering with risk that bidirectional-attention ops don't export cleanly. Value proposition was "commercial-OK alternative to gemma" — but Gemma's restrictions (no weapons / biometric ID / dangerous infrastructure) don't bite cqs's code-search use case. Dropped. Resurrect only if a real Gemma-license blocker appears. Full writeup in `research/models.md`.

**Daemon:**
- [x] **Daemon: full CLI parity** — closed via [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification (shipped in v1.30.x).

**Watch mode:**

The biggest gap between cqs and similar code-intelligence tools: *easy to index, hard to keep indexed between turns*. IDEs solve "between keystrokes" (continuous time, editor consumer); Sourcegraph solves "between pushes" (discrete time, server consumer); cqs needs to solve "between turns" (discrete time, agent consumer). Different consistency models for different consumers — and cqs is the first tool in the space whose primary consumer is the agent, so it's the first that needs the turn-shaped consistency model. Items below are ordered by leverage.

- [x] **#1182 — perfect watch mode (3-layer reconciliation).** Filed 2026-04-28. The closing-the-gap item. Three layers compose: (1) `.git/hooks/post-{checkout,merge,rewrite}` post a `reconcile` message to the daemon socket, (2) periodic full-tree fingerprint reconciliation every `CQS_WATCH_RECONCILE_SECS` (default 30s) catches what hooks + inotify miss, (3) `cqs status --watch-fresh --wait` exposes a freshness contract — eval-runner just calls `--wait` and stops caring. Promise: bounded eventual consistency, agent can either trust `fresh` or block. **Positioning differentiator. Layers 1-4 shipped #1189/#1191/#1193/#1194; 47-file bulk-delta acceptance test landed in #1196.**
- [ ] **Adaptive debounce — idle-flush instead of fixed window.** Today: 500ms (1500ms on WSL/poll) regardless of event burstiness. Replace with "flush after N ms of no events." A bulk `git checkout` (200 files in 50ms) gets one cycle; a slow typist on one file still gets snappy ~500ms. Pre-drain decision in `src/cli/watch/events.rs::collect_events` — `process_file_changes` already drains atomically.
- [ ] **`cqs status --watch`.** Daemon already tracks queue depth, last-reindex latency, cache hit rate, HNSW dirty duration, in-flight clients, last error — operators currently grep `journalctl --user-unit cqs-watch` for them. Expose as a single status command via the existing daemon socket. (Pairs with #1182's freshness API; same command with `--watch-fresh` flag.)
- [ ] **Whitespace/comment-canonical hash for cache lookup.** `content_hash` covers the whole chunk text today, so reformatting or doc-comment churn re-embeds. Hash a normalized form (whitespace collapsed, comments stripped) for the cache lookup; keep the full hash for store identity. Comment-only edits become free; a `cargo fmt` run no longer re-embeds.
- [ ] **Parallel reindex across slots.** A save event today reindexes only the active slot. Slots share `content_hash` and the global embedding cache (#1129), so parallel reindex of all slots is near-free with a warm cache — keeps inactive slots from rotting between A/Bs and removes the manual `cqs index --slot` step.
- [ ] **Kill the periodic full HNSW rebuild.** Today, after `hnsw_rebuild_threshold` inserts the watch loop spawns a background thread to rebuild from scratch (`PendingRebuild` + delta replay in `src/cli/watch/rebuild.rs`). Implement true delete-and-update on the HNSW (mark stale entries, prune in-place) so the rebuild path goes away. Bigger refactor, but the entire content-hash-aware-drain (#1124) machinery exists *only* to bridge this gap.

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

Re-audited 2026-05-02 post-v1.33.0-audit close. The v1.33.0 cycle and post-release audit together closed 134 audit findings; 25 medium-effort items filed as tracking issues (#1337-#1377). #1286 + #1290 closed during the audit chain. Remaining issues split below.

**v1.33.0 audit follow-ups (filed 2026-05-02, all medium-effort):**

| Range | Theme | Count |
|-------|-------|------:|
| [#1337-#1359](https://github.com/jamie8johnson/cqs/issues/1337) | P4 batch — security defense-in-depth, RM eviction, Extensibility refactors, Platform Behavior on Windows, missing e2e smoke tests | 23 |
| [#1365](https://github.com/jamie8johnson/cqs/issues/1365) | P3-27: clap `--slot` help-text mismatch on slot/cache subcommands | 1 |
| [#1366](https://github.com/jamie8johnson/cqs/issues/1366) | P3-49: structural CLI registry — top-level command needs three coordinated edits | 1 |
| [#1370](https://github.com/jamie8johnson/cqs/issues/1370) | P2-9: HNSW M/ef defaults static — auto-scale with corpus | 1 |
| [#1371](https://github.com/jamie8johnson/cqs/issues/1371) | P2-37: SQLite chunks missing composite index `(source_type, origin)` | 1 |
| [#1372](https://github.com/jamie8johnson/cqs/issues/1372) | P2-14: `--rerank` (bool) on search vs `--reranker <mode>` on eval | 1 |
| [#1373](https://github.com/jamie8johnson/cqs/issues/1373) | P2-13: `--depth` flag four defaults across five commands | 1 |
| [#1374](https://github.com/jamie8johnson/cqs/issues/1374) | P2-4: `IndexBackend` public-lib trait uses `anyhow::Result` instead of `thiserror` | 1 |
| [#1375](https://github.com/jamie8johnson/cqs/issues/1375) | P3-52: `lib.rs` wildcard `pub use` leaks internal API surface | 1 |
| [#1376](https://github.com/jamie8johnson/cqs/issues/1376) | P2-8: `serve` async handlers duplicate ~15-20 LOC × 6 — extract helper | 1 |
| [#1377](https://github.com/jamie8johnson/cqs/issues/1377) | Umbrella: P2-36 + P3-53/54/55 perf micro-opts cluster | 1 |

**Perf tier-3 (real wins, but each ≥1hr):**

| # | Finding | Status |
|---|---------|--------|
| [#1244](https://github.com/jamie8johnson/cqs/issues/1244) | RM-4: Reduce build_hnsw_index_owned 17 MB content_hash snapshot | Audit's "240×" claim assumed nonexistent u32 chunk_ids; actual win ~1 MB via `[u8; 32]` repr |
| [#1229](https://github.com/jamie8johnson/cqs/issues/1229) | RM-5: Stream enumerate_files walk + per-file SQL lookup | Real win at 1M-file repos; needs `enumerate_files_iter` API + batched 1k-row SQL |
| [#1228](https://github.com/jamie8johnson/cqs/issues/1228) | RM-2: wait_for_fresh persistent connection | Daemon-side reads one request per connection — option (a) needs daemon-side loop change too |
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | perf: mmap SPLADE body (PF-11) | Audit-deprioritized — 59 MB peak transient, dominated by parse-side allocations |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | perf: HNSW fully loaded into RAM (RM-40) | Needs lib swap to hnswlib-rs (nightly-only) |

**Refactor tier-3 (architectural debt, no user-visible impact):**

| # | Finding | Status |
|---|---------|--------|
| [#1216](https://github.com/jamie8johnson/cqs/issues/1216) | EX: Drive BatchCmd dispatch from macro table (33-arm match) | Current dispatch already exhaustive; win is hypothetical-future-regression-prevention |
| [#1140](https://github.com/jamie8johnson/cqs/issues/1140) | EX: Embedder preset extras map | Explicitly skipped per autopilot directive |
| [#1139](https://github.com/jamie8johnson/cqs/issues/1139) | EX: structural_matchers shared library | Touches 50+ language modules; explicitly skipped |

**Blocked on Windows test env or upstream:**

| # | Finding | Blocker |
|---|---------|---------|
| [#1043](https://github.com/jamie8johnson/cqs/issues/1043) | `is_slow_mmap_fs` ignores Windows network drives + reparse points | Linux/WSL unaffected; needs Windows runner |
| [#106](https://github.com/jamie8johnson/cqs/issues/106) | ort 2.0-rc.12 stable release | Blocked upstream (pykeio); no stable release yet |

**Feature scaffolding deferred:**

| # | Finding | Status |
|---|---------|--------|
| [#255](https://github.com/jamie8johnson/cqs/issues/255) | Pre-built reference packages | Signing/registry design (infra, not code) |

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26.
- **Astro, ERB, EEx/HEEx, Move** — no tree-sitter grammar on crates.io.
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork.
- **ArchestrA QuickScript** — needs custom grammar from scratch.

---

## Parked

- **`nomic-ai/nomic-embed-code` (7B) — Phase 2 of the code-specific embedder A/B, deferred 2026-04-25.** Apache 2.0, base = Qwen2.5-Coder-7B, GGUF quantizations available. Released March 2025; reported to beat Voyage Code 3 + OpenAI Embed 3 Large on CodeSearchNet. ~14 GB VRAM at FP16; embedding is 3584-dim (4.5× current storage). Skipped because at 7B params, inference cost approaches an LLM call — defeats the local-embedder advantage. Would need agentic batching to amortize. Revisit only if Phase 1 (CodeRankEmbed-137M, opt-in via #1110) shows the code-specialist trade-off is worth pushing further at scale.
- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`.
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`.
- Wiki system (agent-first), MCP server (re-add when CLI solid), pre-built reference packages (#255), Blackwell RTX 6000 (96GB), L5X files from plant, KD-LoRA distillation (CodeSage→E5), ColBERT late interaction, enrichment-mismatch mining (Exp #4), lock/fork-aware training weights (Exp #5), ladder logic (RLL) parser, DXF/Openclaw PLC, SSD fine-tuning experiments.

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.35.0 | **Default embedder swap: BGE-large → EmbeddingGemma-300m + tokenizer truncation fix.** No schema bump. PR #1385 moves the `define_embedder_presets!` `default = true` annotation from `bge_large` to `embeddinggemma_300m`; all four downstream constants (`ModelConfig::DEFAULT_REPO`, `ModelConfig::DEFAULT_DIM`, `embedder::DEFAULT_MODEL_REPO`, `EMBEDDING_DIM`) update via the macro. Wins agg R@1 +1.9pp over BGE-large at half the params, 4× context. PR #1384 fixes the tokenizer truncation cap leaking into windowing/count paths — bge-large-ft / v9-200k / coderank tokenizers ship `truncation: max_length=512`, silently capping `encode().get_ids().len()` at 512 even on 5k+ token inputs and dropping ~90% of long-section content from the index. Surgical fix clones the tokenizer and disables truncation for counting paths only. Affected slots gained ~32% chunks on reindex (bge-ft 14,704 → 19,463; v9 14,718 → 19,506). Also: ONNX external-data sidecar download (#1385 fixup), serde_helpers `ignore` → `text` doctest (#1388 closes #1387), `test_prune_zero_days` flake fix (#1390 closes #1389). |
| v1.34.0 | **Audit close-out + EmbeddingGemma preset + reranker eval flag.** Same day as v1.33.0. Bundled 24 fix PRs from the post-v1.33.0 audit (167 findings, 129 closed, 25 medium-effort filed as tracking issues #1337-#1377). Highlights: daemon-aware `cqs index` (#1334, kills 98% of telemetry error rate), 11 unbounded `fs::read_to_string` sites capped (#1328), 5 SQLite legacy batch sizes → `max_rows_per_statement(N)` (#1324, 30-65× round-trip reduction), HNSW partial-save verification (#1325). Plus pre-audit feature work: EmbeddingGemma-300m preset (#1301), `cqs eval --reranker <none\|onnx\|llm>` (#1303 wires #1276's Reranker trait), `slow-tests` Phase 2 (#1302 gates `onboard_test` + `eval_subcommand_test` behind feature; ~12 min off PR-time CI), ci-slow.yml stabilization (#1306-#1308). |
| v1.33.0 | **Eval correctness + indexing performance + new fine-tuned preset.** No schema bump. Release themes: #1284 eval-matcher line-start drift fix (~38% of gold chunks were going invisible after audit-driven line shifts), #1288 placeholder-cache 30s startup-tax fix (CI ~38min → ~6min), #1283 chunk-orphan pipeline prune, #1289 `bge-large-ft` LoRA preset, #1292 daemon thread-local socket-dir override + #1286 Phase 1 nightly CI workflow. Plus 7 internal refactors (Reranker trait #1276, AuthChannel trait #1275, daemon_request<T> #1273, shared enumerate_files walk #1279, write_slot_model flatten #1272, notes kind lifecycle #1278/#1269, ENV_LOCK hoist #1268). |
| v1.32.0 | **HNSW load-phase flock self-deadlock fix + structural-trust + watch-correctness bundle.** Schema v23→v25 (chained, additive). Five themes: #1261 watch-mode flock fix, #1221 three-tier `trust_level: vendored-code`, #1254 worktree → main-index discovery, #1133 note kind taxonomy, #1260 TC-ADV reconcile coverage + persistent TRT engine cache. Post-release sweep landed 13 PRs closing 8 issues — refactor frontier (#1215/#1217/#1218/#1220/#1226) cleared, kind taxonomy fully wired (add/list/update). |
| v1.31.0 | **Schema v22→v23 + watch reconcile cluster + post-v1.30.2 bug drain.** Bundle: #1248 reconcile content-hash fingerprint + path dedup + force-rotation guard, #1249 sparse-upsert chunked sub-transactions, #1250 coarse-mtime FS handling + WSL `cqs serve --open` browser opener, #1251 reqwest same-origin redirect policy, #1252 `cqs slot remove` daemon-aware, #1253 native-Windows `cqs watch` clean shutdown via `ctrlc`, #1255 agent worktree-leakage guard. |
| v1.30.2 | **#1181 mistrust posture + #1182 perfect watch mode + v1.30.1 audit-fix wave.** Default-on `CQS_TRUST_DELIMITERS`, `_meta.handling_advice` on every JSON envelope, per-chunk `injection_flags`. Watch-mode Layers 1-4 closed: `cqs hook install` git hooks, periodic full-tree reconciliation, `cqs status --watch-fresh --wait` API, `cqs eval --require-fresh` gate. v1.30.1 audit-fix omnibus across 19 PRs (P1+P2+P3+P4 trivials, 121 of 144 findings; 18 hard P4s tracked). |
| v1.30.1 | **Indirect-prompt-injection hardening + v1.30.0 audit-fix wave + watch-mode reliability.** Cluster #1166-#1170 + threat model #1171: trust labelling on chunk JSON, `CQS_TRUST_DELIMITERS` opt-in, first-encounter shared-notes prompt on `cqs index`, `CQS_SUMMARY_VALIDATION` for prose summaries before caching, `--improve-docs` review-gated by default, threat model in `SECURITY.md`. Audit-fix omnibus #1141 (152 of 170 P1+P2+P3 findings). Five watch-mode correctness fixes (#1124-#1129): content-hash-aware drain, restore_from_backup pool ordering, summary-write coalescing, daemon mutex hold-time, embedding-cache `purpose` plumbing. Plus four refactors enabling cleaner extension points (#1130 RRF generalize, #1131 IndexBackend trait, #1132 scoring-knob resolver, **#1137 + #1138** registry tables for batch / LLM provider) and 12 dependabot bumps. Schema unchanged from v1.30.0; no reindex required. |
| v1.30.0 | **Cache+slots + three-way embedder A/B + v1.29.0 audit close-out + #956 Phase A scaffolding.** Cache+slots infra (#1105): `.cqs/embeddings_cache.db` keyed on (content_hash, model_id) + project-level `.cqs/slots/<name>/` directories + per-slot `cqs slot {list,create,promote,remove,active}` and `cqs cache {stats,clear,prune,compact}` commands. Three-way embedder A/B (#1109 #1110): fixture refresh absorbed v1.29.x line-start drift; BGE-large stays default; CodeRankEmbed-137M added as opt-in preset; v9-200k retired from production candidacy on the v3.v2 distribution. v1.29.0 audit close-out batch (#1112 #1113 #1114 #1117 #1118 #1119): every umbrella finding from #1095 closed. #956 Phase A scaffolding (#1120): `gpu-index` → `cuda-index` cargo feature rename (legacy alias preserved); `ep-coreml` / `ep-rocm` features added; `ExecutionProvider` enum gains cfg-gated `CoreML` and `ROCm { device_id }` variants. CUDA path byte-identical at runtime. |
| v1.29.1 | **v1.29.0 audit close-out** (147 findings triaged; 142 fixed). No new commands, no schema bump, no reindex. CAGRA SIGSEGV root-caused (missing `Drop` on `GpuState`) + fixed; `cqs serve` security hardened (host allowlist, SQL caps, HTML escape, loopback `--open`); transaction integrity fixes (staleness / metadata / cache / HNSW persist); 13 new `CQS_*` env var knobs for thresholds (additive); `rustls-webpki` GHSA-high patch. Remaining 5 audit items split to issues #1095/#1096/#1097/#1098. |
| v1.29.0 | **`cqs serve` + `.cqsignore` + slow-tests cron killed.** Interactive web UI for the call graph with 4 views — 2D Cytoscape, 3D force-directed, hierarchy (Y axis = BFS depth), embedding cluster (X/Z = UMAP, Y = caller count). Schema bumps v21→v22 for `umap_x`/`umap_y` columns; opt-in via `cqs index --umap` (Python umap-learn). Serve perf pass: ~60s → ~3-4s first paint (SQL-side max_nodes cap, default 300 nodes, `cose` layout, gzip, lazy 3D bundle). New `.cqsignore` mechanism layered on `.gitignore` (drops 18,954 → 15,488 indexed chunks on the cqs corpus, all noise). 5 of 16 slow-test binaries converted to in-process `InProcessFixture`-based tests; nightly `slow-tests.yml` cron deleted. Two Dependabot security bumps (openssl 0.10.78, rand 0.8.6). |
| v1.28.3 | **Per-category SPLADE α re-sweep targeting R@5** (the 2026-04-15 sweep optimized R@1 — different optima in many categories). Two alpha changes ship: `behavioral` 0.00 → 0.80, `multi_step` 1.00 → 0.10. Production net: test R@5 +0.9pp, dev R@5 ±0, no regressions. |
| v1.28.2 | **Reranker V2 retrain follow-ups.** Windowing fix (`chunks.content` was lossy WordPiece-decoded text for 7228/15616 chunks; PR #1060), `cqs index --force` fail-fast vs running daemon (#1061), `cqs notes list` daemon dispatch (#1062), `cli_review_test` `--format` → `--json` migration miss (#1063, fixes 2-day-red slow-tests nightly). Plus reranker pool cap default 100→20, centroid classifier flipped default-on after isolated A/B (test R@5 +3.7pp), `notes_boost_factor` measured (zero impact). Reindex required. |
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
