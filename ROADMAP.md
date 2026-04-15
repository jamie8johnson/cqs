# Roadmap

## Current: v1.25.0 (audit-fixes batch in prep)

54 languages. 29 chunk types. 265-query v2 eval. **Daemon mode** (`cqs watch --serve`, 3-19ms queries). Per-category SPLADE alpha routing. GPU-native CAGRA bitset filtering (patched cuvs 26.4). Enrichment ablation + router update (type_filtered + multi_step → base).

**v1.25.0 shipped 2026-04-14:** eval output writes to `~/.cache/cqs/evals/` (#943, fixes watch-reindex contamination), clean 21-point alpha re-sweep, new per-category defaults (identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05, rest 1.0), multi_step router fix (removed over-broad `"how does"` Behavioral pattern). Tag pushed, crates.io published, GH release with Linux/macOS/Windows binaries.

**Audit-fixes batch in prep:** ~80 fixes from the 11th full audit (16 categories, 2 batches × 8 parallel opus auditors → 236 findings → 49 P1 + 47 P2 + 97 P3 + 16 P4-trivial-inline + 32 P4-issues). Branch `audit/p1-fixes-wave1` accumulating. Issues #946–#975 filed for refactors and quick wins.

### Eval Baselines (post-clean-index, 2026-04-14)

The pre-2026-04-14 numbers were measured against an index that was 81% worktree/cuvs duplicates (root cause: GC `prune_all` suffix-match bug, fixed wave 1). Duplicate-name chunks inflated R@1 and crowded R@5/R@20. Current honest numbers:

| Eval | Model | R@1 | R@5 | R@20 | Notes |
|------|-------|-----|-----|------|-------|
| Fixture (296q) | BGE-large FT | 91.9% | — | — | Synthetic fixtures, model-agnostic to GC bug |
| Fixture (296q) | BGE-large baseline | 91.2% | — | — | Production model |
| Real code (100q) | BGE-large | 50.0% | 73.0% | — | Identifier-slice subset of v2 |
| V2 (265q clean) | BGE-large | 37.4% | 55.8% | 77.4% | Fully routed v1.25.0, post-GC fix (this is the honest number) |
| V2 (265q clean) | E5 v9-200k | 37.4% | 56.6% | 78.1% | Ties BGE on R@1, slight edge on R@5/R@20 — at 1/3 the embedding size |
| V2 (265q clean) | Oracle per-category α | 49.4% | — | — | Theoretical ceiling — gated on classifier accuracy (~22% non-identifier today) |

**Caveat:** v1.25.0 per-category alpha defaults were tuned on the *dirty* index. Alphas may need re-fitting against the clean index numbers above. Tracked in CPU Lane.

---

## Active

### Refactoring Lane (post-audit, 2026-04-14)

High-leverage refactors that close entire bug classes — surfaced by the v1.25.0 audit. Each is its own GitHub issue.

- [ ] **`Store` typestate** — issue [#946](https://github.com/jamie8johnson/cqs/issues/946). Closes `gc-in-daemon`, `notes-in-daemon`, `suggest --apply` write-on-readonly-store class (audit API-V1.25-3, API-V1.25-5).
- [ ] **`Commands` / `BatchCmd` unification** — issue [#947](https://github.com/jamie8johnson/cqs/issues/947). 47 vs 36 variants drift produced 8 silent-fail commands through the daemon (audit EX-V1.25-1, API-V1.25-1/2/4/6, CQ-V1.25-1/2). Half-day refactor.
- [ ] **`cqs::fs::atomic_replace` shared helper** — issue [#948](https://github.com/jamie8johnson/cqs/issues/948). `std::fs::rename` cross-device fallback duplicated 4× with divergent semantics; two were missing fsync until the audit (PB-V1.25-6, DS-V1.25-1, DS-V1.25-4).
- [ ] **Embedder model abstraction** — issue [#949](https://github.com/jamie8johnson/cqs/issues/949). `ModelConfig::input_names`/`output_name`/`pooling` so non-BERT models are config entries, not code edits. Pre-req for BGE → E5 v9-200k default switch.
- [ ] **CAGRA persistence** — issue [#950](https://github.com/jamie8johnson/cqs/issues/950). `CagraIndex::save`/`load` via cuVS native serialize. Cuts daemon hot-restart from ~30s to <5s.

### Quick-wins Lane (Tier-1 ROI from audit issues)

- [ ] **WSL 9P/NTFS mmap auto-detect** — issue [#961](https://github.com/jamie8johnson/cqs/issues/961). Fixes daily WSL `/mnt/c` slow-eval pain. Detect 9P/NTFS at Store open, set `mmap_size=0`. ~50 LOC.
- [ ] **CAGRA itopk + graph_degree env overrides** — issue [#962](https://github.com/jamie8johnson/cqs/issues/962). Concrete proposal for the CAGRA-filtering-regression investigation. Env vars + corpus-size scaling formula.
- [ ] **Reranker batch chunking** — issue [#963](https://github.com/jamie8johnson/cqs/issues/963). Reranker OOMs on large top-K + shared GPU. Add `CQS_RERANKER_BATCH` and chunk the input.
- [ ] **Daemon `try_daemon_query` test scaffold** — issue [#972](https://github.com/jamie8johnson/cqs/issues/972). Closes the largest test-coverage gap in one file.

Full list: 25 issues #951–#975, all labeled `audit-v1.25.0`. See `gh issue list --label audit-v1.25.0`.

### GPU Lane

- [ ] **Reranker V2** — code-trained cross-encoder (ms-marco was catastrophic). SPLADE work all `[x]` historically (cqs-side null result; full breakdown in `~/training-data/research/sparse.md`).

### CPU Lane

**Eval & retrieval quality:**
- [ ] **Classifier accuracy investigation** — the 4.5pp gap between deployed per-category routing (44.9%) and oracle (49.4%) is *entirely* in `classify_query()` accuracy, not alpha picks. Today: negation 100%, identifier 84%, structural 19%, type_filtered 4%, behavioral 5%, conceptual 3%, cross_language 0%. Most queries fall to `Unknown` → α=1.0.

  **Options (local-first first):**
  1. **Rule expansion** — mine eval queries for missed phrasings ("how is X built", "X that uses Y"). Cheap, brittle, +5-10pp.
  2. **Centroid matching on BGE embeddings (zero-train)** — mean labeled embeddings per category → 8 centroids (~32 KB). Query embedding already computed for retrieval. Sub-µs inference. Likely 60-80% on non-identifier/negation. **Recommended starting point.**
  3. **Logistic regression on BGE embeddings (trivial train)** — same input, proper linear head, ~40 KB weights. Approaches 85-90%.
  4. **Fine-tuned MiniLM (training effort)** — 23 MB ONNX, ~1 ms inference. Highest local accuracy.
  5. **LLM classify + cache (non-local)** — Haiku, query-hash cached. Highest accuracy, violates local-first.

  **Data caveat:** 265 labeled queries *are* the eval set — train/eval leakage. Need leave-one-out CV + held-out partition. Coupled with eval expansion below.

- [ ] **Re-fit per-category alphas on clean index** — current v1.25.0 defaults were tuned on the dirty (81% worktree-dup) index. Real optima on the clean index are unknown and likely shift. Re-run the 21-point sweep with the clean index + record new per-category curves.
- [ ] **Eval expansion: grow small categories** — N=21 cross_language and N=24 type_filtered are too noisy for reliable per-category decisions (±4.5pp sampling floor). Target every category N≥40 (≤2.5pp). Rename `v2_300q.json` to actual count (265).
- [ ] **Investigate CAGRA filtering regression on enriched index** — fully-routed v1.24.0 showed conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release baseline. Hypothesis: CAGRA graph walk strands in filtered-out regions. Concrete proposal in [#962](https://github.com/jamie8johnson/cqs/issues/962) (Quick-wins Lane).
- [ ] **Query-time HyDE for structural queries** — old data: HyDE +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral. Router classifies structural → LLM generates synthetic code → embed → search. Per-category by design. Need fresh eval with SPLADE active (old data is pre-SPLADE, pre-AC-1).
- [ ] **Switch production default BGE → E5 v9-200k** — clean-index eval shows ties on R@1 + slight edge on R@5/R@20 + 1/3 the embedding dimension (768 vs 1024). Gated on Embedder model abstraction ([#949](https://github.com/jamie8johnson/cqs/issues/949)) and a confirmation re-run to rule out 1-query noise.

**Daemon & data:**
- [ ] **Daemon: full CLI parity** — batch parser subset differs from CLI. Subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification.
- [ ] **Daemon: incremental SPLADE in watch mode** — watch currently skips SPLADE encoding for new/changed chunks. Keep ONNX model in daemon, encode only new chunks, incremental insert into in-memory `SpladeIndex` (current rebuild ≈18s for 68k chunks).

**Features (queued, no immediate work):**
- [ ] **Temporal search — `cqs history`** — query by author + time range, returns recently-touched chunks ranked by how little they've been touched since. Uses git log + chunk file/line mapping.
- [ ] **Author-weighted search** — `cqs search "..." --author X --boost 0.5` biases results toward an author. Complements temporal search.
- [ ] **Auto-notes on commit** — post-commit hook runs `cqs notes add` with commit message + changed chunk names. Sentiment inferred from commit-message heuristics with override flag.
- [ ] **Config file support** — `[splade.alpha]` per-category overrides in `.cqs.toml`.
- [ ] **Phase 6: Explainable search** — depends on SPLADE-Code being the production default. Spec: `docs/plans/adaptive-retrieval.md`.
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish + adaptive retrieval results.

**Agent adoption:**
- [ ] **Slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + "see `cqs --help`". Measure with telemetry before/after.
- [ ] **Composite search results** — `cqs search` returns mini-impact (caller count, test count) alongside each result. One call instead of search + impact.

**Languages:**
- [ ] **Move language** — blocked: no tree-sitter grammar on crates.io.

### Agent Adoption — Telemetry Data (2026-04-09)

16,731 cqs invocations across all sessions. Two distinct profiles:

**Main conversation (3,889 invocations):**
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

**Subagents (12,842 invocations):**
| Command | Count |
|---------|-------|
| impact | 825 |
| callers | 589 |
| dead | 693 |
| test-map | 457 |
| gather | 403 |
| review | 370 |
| scout | 377 |

**Insight:** impact/callers/test-map are heavy in subagents but almost unused by main. The pre-edit hook bridges this gap by running impact automatically.

### Cross-Project Architecture

Current: N-project via `[[reference]]` entries in `.cqs.toml` → `CrossProjectContext { stores: Vec<NamedStore> }`.

| Approach | Status | Used for | How |
|----------|--------|----------|-----|
| Per-store BFS | shipped | callers, callees, impact, trace | Walk call graph in each store, merge by name. Cross-boundary edges matched by exact name. |
| Per-store search + merge | shipped | search | Independent embedding search per store, RRF-merge by score. No cross-boundary awareness. |
| Unified index | not implemented | — | Single HNSW spanning all projects. Best recall, needs shared model + reindex. |
| Federated query | not implemented | — | Query fan-out + coordinator, filtering/reranking across merged set. |

**Limitation:** Cross-project BFS connects functions by exact name match only. Wrapper functions, re-exports, and name mismatches are invisible.

**Future improvements:**
- [ ] Type-signature matching for cross-boundary edges (same signature + same callers → likely same function)
- [ ] Import-graph resolution (parse `use`/`import` to resolve re-exports)
- [ ] Cross-project search with unified scoring (not just per-store RRF merge)
- [ ] `analyze_impact_cross` resolve file/line from CallGraph (currently empty paths — CQ-3)
- [ ] Cross-project dead code detection

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
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`

---

## Open Issues

Pre-audit issues. New audit issues are tracked under the `audit-v1.25.0` GitHub label (`gh issue list --label audit-v1.25.0` or in the Refactoring + Quick-wins lanes above).

| # | Finding | Difficulty |
|---|---------|-----------|
| #853 | DS-5: DEFERRED transactions → SQLITE_BUSY | medium |
| #854 | SEC-4: Reference path containment | medium |
| #855 | SHL-25: 25 env vars undocumented in README | easy |
| #856 | PB-5: atexit Mutex UB | hard |
| #848 | RM-1: Reduce tokio threads | easy |
| #847 | EXT-2: CLI/batch parity test | easy (subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947)) |

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.25.0 | **11th full audit** (16 categories, 236 findings, fix waves in flight). Per-category SPLADE alpha defaults from clean 21-point sweep (identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05). Multi_step router fix (`"how does"` → not Behavioral, +0.7pp). Eval output to `~/.cache/cqs/evals/` (#943, fixed watch-reindex contamination — root cause of 2 days of eval drift). Notes daemon-bypass routing (#945). Determinism fixes across 15+ sort sites + GC suffix-match bug (81% chunks orphan, root cause of v1.24.0 → v1.25.0 R@1 inflation). Refactor lane queued: #946–#950. Quick-wins lane: #961–#975. |
| v1.24.0 | GPU-native CAGRA bitset filtering (upstream PR rapidsai/cuvs#2019), daemon stability (CAGRA non-consuming search fixes SIGABRT under load), cagra.rs simplified −357 lines, batch/daemon base index routing, router update (type_filtered + multi_step → base), cuVS 26.4 |
| v1.23.0 | **Daemon mode** (`cqs watch --serve`, 3-19ms queries), per-category SPLADE alpha routing + 11-point sweep, persistent query cache, shared runtime, AC-1 fusion fix, 90 audit findings |
| v1.22.0 | Adaptive retrieval Phases 1-5 (classifier + routing + dual base/enriched HNSW), SPLADE-Code 0.6B eval chain (null result), SPLADE index persistence (#895), v19/v20 migrations (#898/#899), read-only batch store (#919), Store::clear_caches (#918), 13 issues created (#912-#925) |
| v1.21.0 | Cross-project call graph (#850), 4 new chunk types to 29 (#851), chunk type coverage across 15 languages (#852), 14-category audit 40+ fixes (#859), API renames + 8 batch flags (#860), remaining audit sweep (#863), paper v1.0, docs refresh |
| v1.20.0 | 14-category audit (71 findings, 69 fixed), Elm (54th), batch --include-type/--exclude-type, SPLADE code training (null), env var docs, README eval rewrite |
| v1.19.0 | `--include-type`/`--exclude-type`, Java/C# test+endpoint, batch `--rrf`, capture list unification, Phase 2 chunks, 265q eval, store dim check |
| v1.18.0 | Embedding cache, 5 chunk types, v2 eval harness, batch query logging |
| v1.17.0 | SPLADE sparse-dense hybrid, schema v17, HNSW traversal filtering, ConfigKey, CAGRA itopk fix |
| v1.16.0 | Language macro v2, Dart (53rd), Impl chunk type |
| v1.15.2 | 10th audit 103/103, typed JSON output structs, 35 PRs |
| v1.15.1 | JSON schema migration, batch/CLI unification |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT |
| v1.14.0 | `--format text\|json`, ImpactOptions, scoring config |
| v1.13.0 | 296-query eval, 9th audit, 16 commands |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes |
