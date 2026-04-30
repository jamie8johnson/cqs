# Roadmap

## Current: v1.31.0 (released 2026-04-30)

Tag `v1.31.0` pushed; `cqs 1.31.0` published to crates.io; GitHub Release workflow building prebuilt binaries. Minor bump because of the **schema v22 → v23 migration** (auto-migrating; new `source_size` + `source_content_hash` columns on `FileFingerprint` for the reconcile cluster fix). Theme: post-v1.30.2 bug drain across watch reconcile, sparse-vector index, LLM redirect policy, slot lifecycle, native-Windows shutdown, and coarse-mtime filesystems. Reindex not required, but the v23 binary will refuse to open a v22 index until the auto-migration step runs on first start.

**Bundle table:**

| PR | Closes | Theme |
|---|---|---|
| #1248 | #1219 #1245 #1231 #1227 | watch reconcile cluster — content-hash fingerprint + path dedup + force-rotation guard. **Schema v22→v23.** |
| #1249 | #1212 | sparse-upsert chunked sub-transactions (`CQS_SPARSE_CHUNKS_PER_TX`, default 5000). |
| #1250 | #1224 #1225 | coarse-mtime FS handling (HFS+, SMB, NFS, CIFS, FAT32 on plain Linux/macOS) + WSL `cqs serve --open` browser opener. |
| #1251 | #1222 #1223 | reqwest same-origin redirect policy + `cqs ref add --source` symlink-redirect warning. |
| #1252 | #1232 | `cqs slot remove` refuses if daemon is serving (probes `daemon_status`, matches `WatchSnapshot.active_slot`). |
| #1253 | #1044 | native-Windows `cqs watch` clean shutdown via `ctrlc` termination feature. |
| #1255 | tracks #1254 | agent definitions: worktree-leakage warning bullet across all 6 `.claude/agents/*.md` files. |

**#1254 worktree leakage** — `git worktree add` doesn't create `.cqs/`; cqs errors; agents fall back to absolute paths under main's tree → edits leak into parent tree. Agent-side guard shipped via #1255; cqs-side fix (`.git/commondir` auto-discovery + `worktree_stale: bool` JSON envelope flag) deferred.

**Deferred indefinitely:**

- **#1134-#1136** — `cqs serve` P4 auth bugs from the v1.30.0 audit. Need shaping decisions; not blocking.
- **#1139** — `structural_matchers` shared library. Touches 50+ language modules.
- **#1140** — embedder preset extras map. Revisit when preset count pressures the current hand-rolled match.

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
- [x] **BGE → E5 v9-200k — tested 2026-04-25, RETIRED on v3.v2.** Hypothesis was "clean-index eval ties on R@1, slight edge on R@5/R@20, 1/3 the embedding dim". Result on the actual v3.v2 fixture (refreshed): dev R@5 **40.4%** vs BGE 74.3% (-33.9pp), test R@5 **38.5%** vs 63.3% (-24.8pp). Bare-vs-enriched gives identical numbers — summaries can't rescue what the dense channel doesn't surface. Per-category collapse: every NL→code semantic category drops to 16-25% R@5; only identifier_lookup holds (FTS+name-boost path doesn't need strong dense semantics). Distribution mismatch confirmed: v9-200k optimized for CSN/Stack curated pairs (90.5% R@1 on 296q synthetic fixture, BGE-parity), but doesn't generalize to v3.v2's LLM-generated + telemetry queries — consistent with the prior CoIR finding (worst CoIR despite best fixture R@1). Centroid classifier additionally no-ops on v9-200k (1024-dim BGE-prefixed file vs 768-dim E5 query embedding → dim guard returns None) but that's only ~0-3pp of the 30pp gap. **Protocol note (2026-04-21):** PR #1071 measured HNSW reconstruction noise at ~4pp R@5 on v4 N=1526; under a 30pp gap that noise floor is irrelevant. Full numbers in `~/training-data/research/models.md`.
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
- [ ] **EmbeddingGemma-300m A/B vs BGE-large — production candidate.** Google's [embeddinggemma-300m](https://huggingface.co/google/embeddinggemma-300m) (Sep 2025), 308M params — same size class as BGE-large (340M) so inference cost is comparable and the local-embedder advantage holds. Bidirectional-attention head on the Gemma3 backbone, trained on 320B tokens, 100+ langs, reported #1 multilingual under 500M on MTEB at release. Plausible wins over BGE-large: **2K context** (vs BGE's 512 — chunks longer than 512 tokens currently truncate, an obvious miss), **MRL truncation** to 768/512/256/128, and the size-matched code-vs-NL trade vs CodeRankEmbed-137M Phase 1. Pre-quantized ONNX variants already published in [onnx-community/embeddinggemma-300m-ONNX](https://huggingface.co/onnx-community/embeddinggemma-300m-ONNX): q4 (197 MB), q4f16 (175 MB), fp16 (617 MB), int8 (309 MB) — no DIY quantization step. Apache 2.0 license. Plan: clone the ONNX repo, `cqs slot create gemma --model embeddinggemma-300m`, copy `llm_summaries` cross-slot by `content_hash`, reindex, eval on v3.v2. ~1 hour engineering + cold reindex. Most leverage at 768-dim (drops from 1024 → 768 storage, −25%). Treat as **Phase 3** of the embedder A/B sequence (Phase 1 = CodeRankEmbed-137M, Phase 2 = parked nomic-embed-code).
- [ ] **Qwen3-Embedding-8B ceiling probe — research, not production.** Even though 8B inference defeats the local-embedder advantage on the RTX 4000 inference card, we want to know **what the practical R@K ceiling looks like** when size isn't the constraint. The 8B Qwen3 embedder is #1 MTEB multilingual (70.58, June 2025); if it can't beat BGE-large by a meaningful margin on v3.v2, then the gap to "best available" isn't a model-size problem and we stop chasing scale. If it does, we have the magnitude of the lift waiting on the agentic-batching side. **The interesting outcome is no-improvement.** A flat or slightly-worse 8B vs 340M result would tell us the v3.v2 corpus is the ceiling — meaning further retrieval-quality work should redirect from "bigger embedder" toward corpus expansion, harder negatives, query rewriting, or rerankers. A 24× model-size delta giving ≤ 2pp R@5 lift would be one of the cleanest signals we could get to retire embedder-scale as a knob. Two community ONNX exports exist: [onnx-community/Qwen3-Embedding-8B-ONNX](https://huggingface.co/onnx-community/Qwen3-Embedding-8B-ONNX) (FP32 only, 30.27 GB single weights file) and [Maxi-Lein/Qwen3-Embedding-8B-onnx](https://huggingface.co/Maxi-Lein/Qwen3-Embedding-8B-onnx). Native BF16, up to 4096-dim with MRL truncation to 32–4096, 32K context. Plan: A6000 only (RTX 4000 won't fit); `cqs slot create qwen3-8b --model qwen3-embedding-8b` at 1024-dim MRL truncation for storage parity with BGE-large, copy `llm_summaries` cross-slot, reindex (slow — expect tens of minutes for the cqs corpus), eval on v3.v2. The ONNX is FP32-only in onnx-community so we either pay the 30 GB cold load or run our own `optimum-cli onnxruntime quantize` to FP16/INT8 first. Result becomes the upper bound entry in `~/training-data/research/models.md`.
- [ ] **NV-Embed-v2 ceiling probe (NVIDIA, Aug 2024) — research, not production.** Companion to the Qwen3 ceiling probe. [`nvidia/NV-Embed-v2`](https://huggingface.co/nvidia/NV-Embed-v2) was MTEB #1 at release with **72.31 across 56 tasks** and **62.65 on the 15 retrieval tasks** — a hair above Qwen3-Embedding-8B's 70.58 on a different snapshot. 8B params, base = Mistral-7B-v0.1, 4096-dim, 32K context. Two caveats vs Qwen3 that change the engineering profile: (1) **non-standard Latent-Attention pooling head** — cqs's ONNX path assumes mean / CLS / EOS pooling, so wiring NV-Embed-v2 needs a custom pooling implementation OR running via PyTorch outside cqs and importing the per-chunk embeddings as a stored vector; (2) **no community ONNX exists** — would need our own `optimum-cli` export. License is **CC-BY-NC-4.0** which is a commercial-use blocker if cqs ever ships it as a default — not a problem for a measurement-only run on the local A6000 (the user has confirmed eval-only use is fine, since this never ships). The retrieval-specialized sibling [`nvidia/NV-Retriever-v1`](https://huggingface.co/nvidia/NV-Retriever-v1) (60.9 retrieval) and the multimodal [`nvidia/omni-embed-nemotron-3b`](https://huggingface.co/nvidia/omni-embed-nemotron-3b) (3B, vision+audio+text) are noted but lower priority — pick NV-Embed-v2 first since it's the strongest text-only scorer. Smaller commercial-OK NeMo Retriever option for the production lane: [`nvidia/llama-3.2-nv-embedqa-1b-v2`](https://huggingface.co/nvidia/llama-3.2-nv-embedqa-1b-v2) (1B, Matryoshka 384–2048-dim, 8K context, 26 langs, NVIDIA Open Model License + Llama 3.2 license, commercial-OK) — a plausible alternative to EmbeddingGemma if Phase 3 surfaces interesting tradeoffs.

**Daemon:**
- [ ] **Daemon: full CLI parity** — subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification.

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

Re-audited 2026-04-25 against actual GitHub state. **The v1.29.0 audit P4 backlog is now empty** — every numbered finding (1042/1047/1048/1049/1091/1107/1108) closed via PRs #1112, #1117, #1119 (the latter two from the post-v1.29.1 audit close-out batch). Remaining open issues split into "Windows-specific (need test env)" and "external-blocked".

**Windows-specific (need Windows test environment):**

| # | Finding | Blocker |
|---|---------|---------|
| [#1043](https://github.com/jamie8johnson/cqs/issues/1043) | `is_slow_mmap_fs` ignores Windows network drives + reparse points | Linux/WSL unaffected; needs Windows runner |
| [#1044](https://github.com/jamie8johnson/cqs/issues/1044) | Native Windows `cqs watch` cannot stop cleanly — DB corruption risk | Closing via PR #1253 (`ctrlc` termination feature) |

**Tier 2 / 3 (external-blocked or scaffolding-only):**

| # | Finding | Status |
|---|---------|--------|
| [#956](https://github.com/jamie8johnson/cqs/issues/956) | ExecutionProvider: CoreML/ROCm decouple | **Phase A in PR #1120** (cargo feature split + cfg-gated enum variants + restructured probe) — Phase B (CoreML, GHA macOS runner) and Phase C (ROCm, AMD hardware) both deferred to contributors with the matching test environment |
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | mmap SPLADE body (PF-11) | smaller win than originally claimed |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | HNSW mmap (RM-40) | needs lib swap to hnswlib-rs (nightly-only) |
| [#255](https://github.com/jamie8johnson/cqs/issues/255) | Pre-built reference packages | signing/registry design (infra, not code) |
| [#106](https://github.com/jamie8johnson/cqs/issues/106) | ort 2.0-rc.12 stable release | blocked upstream (pykeio) |

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
