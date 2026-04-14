# Roadmap

## Current: v1.24.0

54 languages. 29 chunk types. 265-query v2 eval. BGE-large = best config. Adaptive retrieval Phases 1-5 shipped. **Daemon mode** (`cqs watch --serve`, 3-19ms queries). Per-category SPLADE alpha routing. GPU-native CAGRA bitset filtering (patched cuvs 26.4). Enrichment ablation + router update (type_filtered + multi_step → base).

### Eval Baselines

| Eval | Model | R@1 | Notes |
|------|-------|-----|-------|
| Fixture (296q) | BGE-large FT | 91.9% | Synthetic fixtures |
| Fixture (296q) | BGE-large | 91.2% | Production model |
| Real code (100q) | BGE-large | 50.0% | R@5 = 73% (agent-relevant) |
| V2 (265q, live) | BGE-large | 42.3% | 8 categories, clean A/B (2026-04-12) |
| V2 (265q, live) | + SPLADE α=0.9 | 43.4% | +1.1pp — global optimum from 11-point sweep |
| V2 (265q, live) | Base-only (no summaries) | 42.3% | 2026-04-13 enrichment ablation |
| V2 (265q, live) | Enriched + CAGRA filter | 42.6% | 2026-04-13 enrichment ablation |
| V2 (265q, live) | Oracle routing (theoretical) | **43.8%** | Best arm per category |
| V2 (265q, live) | **v1.24.0 fully routed** | **41.5%** | Router + CAGRA filter, net zero — CAGRA regression on enriched |

---

## Active

### GPU Lane
- [x] ~~SPLADE v1~~ — **NULL**. 0pp R@1. Weak reg, wrong vocab, wrong negatives.
- [x] ~~SPLADE v2 data mining~~ — 199,998 token-overlap pairs.
- [x] ~~SPLADE v2 training~~ — Token-overlap negs + reg_weight 1e-3.
- [x] ~~SPLADE v2 eval~~ — **NULL**. 0pp R@1. 110M BERT capacity ceiling confirmed.
- [x] ~~SPLADE v3 / v4 (reg sweep, CodeBERT vocab)~~ — Cancelled. Capacity is the bottleneck.
- [x] ~~**SPLADE-Code 0.6B eval**~~ — **Flag-driven is a net loss.** 2026-04-11 re-run: 41.8% R@1 (+SPLADE) vs 42.4% R@1 (baseline) on 165q, N=165. Reverses the 2026-04-09 +1.2pp headline. cross_language is the only category where SPLADE pays off (+10pp, N=10). conceptual_search −3.7pp and multi_step −4.6pp from cross-category R@5 displacement. Full breakdown in `~/training-data/research/sparse.md` § SPLADE-Code 0.6B Eval Re-run. Next: selective routing (below).
- [ ] **Reranker V2** — code-trained cross-encoder (ms-marco was catastrophic)

### CPU Lane (next up)
- [x] ~~**Adaptive retrieval** Phases 1-4~~ — classifier + routing + telemetry shipped in v1.22.0
- [x] ~~**Adaptive retrieval** Phase 5~~ — dual embeddings (base + enriched HNSW) shipped in PR #876 + #877 + #878
- [x] ~~**SPLADE alpha sweep + ship defaults**~~ — 11-point sweep + single-category verification. Per-category optimal alphas shipped: identifier 0.9, structural 0.7, conceptual 0.9, type_filtered 0.9, behavioral 0.1, rest 1.0. Expected **+4.9pp R@1** (47.2% vs 42.3%). Cross-language excluded (N=21 noise). Plan: `docs/plans/2026-04-12-selective-splade-routing.md`
- [x] ~~**Enrichment ablation + routing update**~~ — 2-arm eval at 78% summary coverage with SPLADE. Oracle routing = 43.8% R@1 (+1.9pp). Updated router: type_filtered/multi_step → DenseBase (previously enriched). Research: `~/training-data/research/enrichment.md`.
- [x] ~~**CAGRA native bitset filtering**~~ — GPU-side type/language filtering during graph traversal, replacing 3x over-fetch + post-filter. +0.7pp R@1 (42.6% vs 41.9%) on enriched. Structural +3.7pp, negation +3.4pp, behavioral +2.2pp. Patched cuvs crate (upstream PR rapidsai/cuvs#2019). Shipped in v1.24.0.
- [ ] **Investigate CAGRA filtering regression on enriched index** — fully-routed eval (v1.24.0) showed conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release baseline. CAGRA bitset and HNSW traversal-time filtering return different candidate sets on the enriched index. Hypothesis: CAGRA graph walk strands in filtered-out regions when the graph was built on the full dataset. Options to try: (1) use HNSW for enriched + CAGRA only for base, (2) increase CAGRA `itopk_size` for filtered queries, (3) build separate CAGRA graphs per common filter bitmask. Blocks further R@1 gains.
- [ ] **Query-time HyDE for structural queries** — old data shows HyDE helps structural +14pp, type_filtered +12pp but hurts conceptual −22pp, behavioral −15pp. Instead of a third index column (`embedding_hyde`), do it at query time: router classifies structural → LLM generates synthetic code snippet → embed that → search. No index change, ~500ms-1s latency per structural query. Per-category by design since router already classifies. Need fresh eval with SPLADE active (old data is pre-SPLADE, pre-AC-1).
- [ ] **Config file support** — `[splade.alpha]` per-category overrides in `.cqs.toml`
- [ ] **Phase 6: Explainable search** — depends on SPLADE-Code being the production default. Spec: `docs/plans/adaptive-retrieval.md`
- ~~**OpenRCT2 → Rust dual-trail experiment**~~ — parked. Spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish + adaptive retrieval results
- [x] ~~**Cross-project: wire remaining commands**~~ — impact, trace, test-map wired in #864. Deps local-only.
- [x] ~~**Agent adoption: telemetry analysis**~~ — mined 16,731 invocations across all sessions. Finding: main conversation uses search (60%) + context (28%). Subagents use the full toolkit (impact, callers, test-map). The gap is in the main conversation, not subagents.
- [x] ~~**Agent adoption: pre-edit impact hook**~~ — PreToolUse hook on Edit, runs `cqs impact`, injects caller/test/risk as additionalContext. Implemented in `.claude/hooks/pre-edit-impact.py`, wired in `settings.json`.
- [ ] **Agent adoption: slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + "see `cqs --help`". Measure with telemetry before/after.
- [ ] **Agent adoption: composite search results** — `cqs search` returns mini-impact (caller count, test count) alongside each result. One call instead of search + impact.
- [ ] **Move language** — blocked: no tree-sitter grammar on crates.io
- [x] ~~**`PRAGMA quick_check` opt-in**~~ — flipped to opt-in via `CQS_INTEGRITY_CHECK=1` (#924/#911). Default: skip. Saves ~40s on WSL `/mnt/c` write opens.
- [x] ~~**Persistent daemon**~~ — `cqs watch --serve` (#926). Unix socket, dedicated query thread, 3-19ms graph queries. Plan: `docs/plans/2026-04-12-persistent-daemon.md`.
- [x] ~~**Persistent query cache**~~ — `~/.cache/cqs/query_cache.db` (#928). Disk-backed query embeddings, 7-day eviction. Saves ~500ms on repeated queries.
- [x] ~~**Shared runtime support**~~ — `Store::open_readonly_pooled_with_runtime()` + `EmbeddingCache::open_with_runtime()` (#929). Optional runtime injection, backward compatible.
- [x] ~~**AC-1 fusion rewrite**~~ — `apply_scoring_pipeline()` preserves fused scores through scoring (#910). Alpha knob now functional.
- [x] ~~**Audit mega-batch**~~ — 28 findings + 10 tests + 13 issues (#911). All P1s addressed (88 total).
- [x] ~~**SPLADE re-eval**~~ — done via 11-point alpha sweep (PR #932). AC-1 fusion fix confirmed working: α=0.9 is +1.1pp R@1 globally, per-category optimal alphas shipped in `resolve_splade_alpha()`.
- [ ] **Daemon: full CLI parity** — batch parser subset differs from CLI (missing some flags). Need either unified parser or more comprehensive arg translation.
- [ ] **Daemon: incremental SPLADE in watch mode** — watch currently skips SPLADE encoding for new/changed chunks. Needs: (1) keep SPLADE ONNX model loaded in daemon, (2) encode only new chunks, (3) incremental insert into in-memory `SpladeIndex` (current impl rebuilds from scratch ~18s for 68k chunks). Without this, `cqs index` is still required after edits for full SPLADE coverage.
- [x] ~~**cuVS bump: cuvs 26.2→26.4**~~ — PR #935 merged. conda libcuvs=26.04, CUDA 13 JIT support, non-consuming search, CAGRA persistence fix (rapidsai/cuvs#1800). Fixed daemon CAGRA segfault.
  - [x] ~~**Simplify cagra.rs**~~ — v1.24.0 removed `IndexRebuilder` / `Mutex<Option<Index>>` / cached `dataset`. Net −357 lines. Fixed daemon SIGABRT under sustained load.
- [x] ~~**cuVS filtered CAGRA search**~~ — GPU-native bitset filtering shipped. Local patched cuvs 26.4 via `[patch.crates-io]`. Upstream PR rapidsai/cuvs#2019 pending review. When merged, switch back to crates.io version. PCA preprocessor and float16 are C++/Python only.

### Agent Adoption — Telemetry Data (2026-04-09)

16,731 cqs invocations across all sessions. Two distinct usage profiles:

**Main conversation (3,889 invocations via telemetry):**
| Command | % | Count |
|---------|---|-------|
| search | 60.1% | 2336 |
| context | 27.8% | 1080 |
| notes | 1.9% | 74 |
| batch | 1.8% | 70 |
| review | 1.2% | 46 |
| scout | 0.7% | 26 |
| health | 0.4% | 14 |
| impact | **0.2%** | 6 |
| callers | **0.2%** | 7 |

**Subagents (12,842 invocations via conversation log mining):**
| Command | Count |
|---------|-------|
| impact | 825 |
| callers | 589 |
| test-map | 457 |
| dead | 693 |
| gather | 403 |
| review | 370 |
| scout | 377 |

Key insight: impact/callers/test-map are used heavily by subagents but almost never by the main conversation. The pre-edit hook bridges this gap by running impact automatically.

### Cross-Project Architecture

Current implementation: N-project via `[[reference]]` entries in `.cqs.toml` → `CrossProjectContext { stores: Vec<NamedStore> }`.

| Approach | Status | Used for | How |
|----------|--------|----------|-----|
| Per-store BFS | **shipped** | callers, callees, impact, trace | Walk call graph in each store, merge by name. Cross-boundary edges matched by exact function name. |
| Per-store search + merge | **shipped** | search | Independent embedding search per store, RRF-merge by score. No cross-boundary awareness. |
| Unified index | not implemented | — | Single HNSW spanning all projects. Best recall, needs shared model + reindex. |
| Federated query | not implemented | — | Query fan-out with coordinator, filtering/reranking across merged set. |

**Limitation:** Cross-project BFS connects functions by exact name match only. If project-1 calls `utils::parse` and project-2 defines it, the edge connects. But wrapper functions, re-exports, or name mismatches are invisible.

**Future improvements:**
- [ ] Type-signature matching for cross-boundary edges (same signature + same callers → likely same function)
- [ ] Import-graph resolution (parse `use`/`import` to resolve re-exports across projects)
- [ ] Cross-project search with unified scoring (not just per-store RRF merge)
- [ ] `analyze_impact_cross` resolve file/line from CallGraph (currently returns empty paths — CQ-3)
- [ ] Cross-project dead code detection (function with zero callers across all referenced projects)

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26
- **Astro, ERB, EEx/HEEx** — need tree-sitter grammars
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork
- **ArchestrA QuickScript** — needs custom grammar from scratch

---

## Parked

- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`
- Wiki system — spec revised (agent-first), parked for review
- SSD fine-tuning experiments — spec ready, 5 experiments
- MCP server — re-add when CLI solid
- Pre-built reference packages (#255)
- Blackwell RTX 6000 (96GB)
- L5X files from plant
- KD-LoRA distillation (CodeSage→E5)
- ColBERT late interaction
- Enrichment-mismatch mining (Exp #4)
- Lock/fork-aware training weights (Exp #5)
- Ladder logic (RLL) parser
- DXF, Openclaw PLC

---

## Open Issues

| # | Finding | Difficulty |
|---|---------|-----------|
| #853 | DS-5: DEFERRED transactions → SQLITE_BUSY | medium |
| #854 | SEC-4: Reference path containment | medium |
| #855 | SHL-25: 25 env vars undocumented in README | easy |
| #856 | PB-5: atexit Mutex UB | hard |
| #857 | ~~AD-2: --include-type naming~~ | closed |
| #858 | ~~AD-4: Batch missing flags~~ | closed |
| #849 | SHL-23: Channel depth env overrides | done in #863 |
| #848 | RM-1: Reduce tokio threads | easy |
| #847 | EXT-2: CLI/batch parity test | easy |

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.24.0 | GPU-native CAGRA bitset filtering (upstream PR rapidsai/cuvs#2019), daemon stability (CAGRA non-consuming search fixes SIGABRT under load), cagra.rs simplified −357 lines, batch/daemon base index routing, router update (type_filtered + multi_step → base), cuVS 26.4 |
| v1.23.0 | **Daemon mode** (`cqs watch --serve`, 3-19ms queries), per-category SPLADE alpha routing + 11-point sweep, persistent query cache, shared runtime, AC-1 fusion fix, 90 audit findings |
| v1.22.0 | Adaptive retrieval Phases 1-5, SPLADE-Code 0.6B eval chain, SPLADE index persistence (#895), v19/v20 migrations (#898/#899), read-only batch store (#919), Store::clear_caches (#918), 13 issues created (#912-#925) |
| v1.21.0 | Cross-project call graph (#850), 4 new chunk types to 29 (#851), chunk type coverage across 15 languages (#852), 14-category audit 40+ fixes (#859), API renames + 8 batch flags (#860), remaining audit sweep (#863), paper v1.0, docs refresh |
| v1.20.0 | 14-category audit (71 findings, 69 fixed), Elm (54th), batch --include-type/--exclude-type, SPLADE code training (null), env var docs, README eval rewrite |
| v1.19.0 | `--include-type`/`--exclude-type`, Java/C# test+endpoint, batch `--rrf`, capture list unification, Phase 2 chunks, 265q eval, store dim check |
| v1.18.0 | Embedding cache, 5 chunk types, v2 eval harness, batch query logging |
| v1.17.0 | SPLADE sparse-dense hybrid, schema v17, HNSW traversal filtering, ConfigKey, CAGRA itopk fix |
| v1.16.0 | Language macro v2, Dart (53rd), Impl chunk type |
| v1.15.2 | 10th audit 103/103, typed JSON output structs, 35 PRs |
| v1.15.1 | JSON schema migration, batch/CLI unification |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT |
| v1.14.0 | `--format text|json`, ImpactOptions, scoring config |
| v1.13.0 | 296-query eval, 9th audit, 16 commands |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes |
