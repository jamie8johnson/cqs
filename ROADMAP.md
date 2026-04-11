# Roadmap

## Current: v1.21.0

54 languages. 29 chunk types. 265-query v2 eval. BGE-large = best config.

### Eval Baselines

| Eval | Model | R@1 | Notes |
|------|-------|-----|-------|
| Fixture (296q) | BGE-large FT | 91.9% | Synthetic fixtures |
| Fixture (296q) | BGE-large | 91.2% | Production model |
| Real code (100q) | BGE-large | 50.0% | R@5 = 73% (agent-relevant) |
| V2 (265q, live) | BGE-large | 48.5% | 8 categories, bootstrap CIs |
| V2 (265q, live) | + LLM summaries | 48.5% | +18pp multi_step, -15pp conceptual, net 0pp |

---

## Active

### GPU Lane
- [x] ~~SPLADE v1~~ — **NULL**. 0pp R@1. Weak reg, wrong vocab, wrong negatives.
- [x] ~~SPLADE v2 data mining~~ — 199,998 token-overlap pairs.
- [x] ~~SPLADE v2 training~~ — Token-overlap negs + reg_weight 1e-3.
- [x] ~~SPLADE v2 eval~~ — **NULL**. 0pp R@1. 110M BERT capacity ceiling confirmed.
- [x] ~~SPLADE v3 / v4 (reg sweep, CodeBERT vocab)~~ — Cancelled. Capacity is the bottleneck.
- [ ] **SPLADE-Code 0.6B eval** — perf blockers cleared (PRs #881, #884, #886, #889, #891). Encoding pipeline now runs end-to-end with proper vocab probe, batched encoder, constant-padding arena reuse, and SQLite-limit-derived sparse insert batches. Reindex in flight; eval runs immediately after. Previous result on a different corpus state: +1.2pp R@1, +20pp cross_language. This eval reproduces or refutes that on the current corpus.
- [ ] **Reranker V2** — code-trained cross-encoder (ms-marco was catastrophic)

### CPU Lane (next up)
- [x] ~~**Adaptive retrieval** Phases 1-4~~ — classifier + routing + telemetry shipped in v1.22.0
- [x] ~~**Adaptive retrieval** Phase 5~~ — dual embeddings (base + enriched HNSW) shipped in PR #876 + #877 + #878
- [ ] **Selective SPLADE routing** — `classify_query` should pick `SearchStrategy::DenseWithSplade` for `QueryCategory::CrossLanguage`. **Now mandatory, not optional**: the 2026-04-11 re-run measured flag-driven SPLADE-Code 0.6B at **−0.6pp R@1 net** on the 165q eval (41.8% vs 42.4% baseline). Per-category: cross_language +10pp (30 → 40%, N=10), conceptual −3.7pp, multi_step −4.6pp, others flat. R@5 damage bigger (conceptual −7.4pp, type_filtered −6.2pp, negation −5.6pp). Flag-driven SPLADE displaces good dense hits at positions 2-5 on categories where lexical expansion isn't the missing signal. Routing SPLADE to cross_language only:
  - Predicted: +10pp on cross_language stays, −3.7pp conceptual and −4.6pp multi_step disappear, total 41.8% → ~43.0% (net **+1.2pp** vs always-on, **+0.6pp** vs baseline)
  - Strict improvement vs both "always off" and "always on"
  - Encoder is already lazy-loaded on first SPLADE query — sessions with no cross-language queries never pay the load
  - Persisted SpladeIndex (shipped this session) means the cross_language queries that do activate SPLADE load in ~5 s from disk instead of rebuilding for 45 s
  - Code: derive `want_splade = cli.splade || matches!(c.category, CrossLanguage)`, plumb through encoder + index loading, graceful fallback when encoder unavailable
  - Open: should the routed strategy compose with `DenseBase` for cross-language + negation queries? Probably not in v1 — keep enums mutually exclusive, revisit if data demands
  - Validate: re-run just cross_language + conceptual + multi_step cells with the router patched in, compare against 2026-04-11 v3 numbers
- [ ] **Phase 6: Explainable search** — depends on SPLADE-Code being the production default. Spec: `docs/plans/adaptive-retrieval.md`
- [ ] **OpenRCT2 → Rust dual-trail experiment** — substrate for measuring whether structural code intelligence augmentation improves agent-directed translation in a sustained, real-world task. Two parallel translations on the same upstream commit, one with cqs in the loop, one without. Pre-registered metrics (regression bugs, tokens, wall clock). Publishable after three modules in both trails. Spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish + adaptive retrieval results
- [x] ~~**Cross-project: wire remaining commands**~~ — impact, trace, test-map wired in #864. Deps local-only.
- [x] ~~**Agent adoption: telemetry analysis**~~ — mined 16,731 invocations across all sessions. Finding: main conversation uses search (60%) + context (28%). Subagents use the full toolkit (impact, callers, test-map). The gap is in the main conversation, not subagents.
- [ ] **Agent adoption: pre-edit impact hook** — PreToolUse hook that runs `cqs impact` on every Edit, injects caller/test/risk as additionalContext. Prototype in `.claude/hooks/pre-edit-impact.sh`. Needs session restart to test.
- [ ] **Agent adoption: slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + "see `cqs --help`". Measure with telemetry before/after.
- [ ] **Agent adoption: composite search results** — `cqs search` returns mini-impact (caller count, test count) alongside each result. One call instead of search + impact.
- [ ] **Move language** — blocked: no tree-sitter grammar on crates.io
- [ ] **`PRAGMA quick_check` on write opens is 40 s on 1.1 GB DB / WSL /mnt/c** — the read-only path already skips it (shipped in #893). Write opens still pay it on every `cqs notes add`, `cqs index`, and `cqs-watch` batch. Options: skip entirely on WSL, make it opt-in via `CQS_INTEGRITY_CHECK=1`, run it once per session (cached via sentinel file), or off-thread it. Low risk of corruption on a rebuildable index — "off by default, opt-in" is probably right.

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
