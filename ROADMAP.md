# Roadmap

## Current: v1.26.0 + PR #1010

54 languages. 29 chunk types. v3 eval canonical (544 dual-judge queries, train/dev/test 326/109/109). Daemon mode (`cqs watch --serve`, 3-19ms queries). Per-category SPLADE alpha routing. GPU-native CAGRA bitset filtering (patched cuvs 26.4).

**v1.26.0** shipped 2026-04-15: watch-mode hardening + alpha re-fit + `--splade` CLI fix. 162/236 audit findings closed across v1.25.0 + v1.26.0.

**PR #1010** (2026-04-16, will land as v1.26.1 or v1.27.0):
- cqs batch RefCell panic in `invalidate_mutable_caches` (try_borrow_mut + deferred retry).
- Reranker `token_type_ids` bug: zeroed segment IDs silently broke fine-tuned BERT-family rerankers.
- `CQS_RERANKER_MODEL` accepts absolute local directory paths alongside HF repo ids.
- Cross_language α 1.00 → 0.10 (v3 sweep finding; +1.8pp R@1 on v3 test).
- Centroid classifier infrastructure (disabled by default; `CQS_CENTROID_CLASSIFIER=1`).
- `tests/classifier_audit.rs` integration test: confusion matrix vs v3 consensus labels.

### Eval baselines on v3 test (production router, 3-trial stable)

| Config | R@1 | R@5 | R@20 |
|---|---|---|---|
| v1.26.0 alphas | 40.4% | 64.2% | 80.7% |
| **v1.26.0 + xlang=0.10 (shipping)** | **42.2%** | 64.2% | 78.9% |
| Full v3-swept per-category α | 41.3% | 63.3% | 78.9% |

Single-trial v3 test readings drift ±1pp; always confirm over 3 trials. Forced-α (no strategy router) tops out around 48% — the ceiling if the rule-based classifier perfectly routed every query. Breakeven simulation shows per-category α routing on Unknown queries (~48% of traffic) is net-negative at *any* classifier accuracy. Real reachable tuning ceiling is ~1-3pp above 42.2%. Further R@1 requires representation changes (HyDE, reranker V2 at scale, embedder switch).

---

## Active

### GPU Lane

- [ ] **Reranker V2 — code-trained cross-encoder.** Pilot (2270 v3 triples, ms-marco-MiniLM-L-6-v2 fine-tune) landed net-negative. Default ms-marco without fine-tuning: 28.4% R@1. Full pipeline verified end-to-end (training, ONNX export, local-path loading, `--rerank` integration).

  **Prerequisites to ship — all four required:**
  1. Scale — 200k+ Gemma-labeled pairs (pipeline built 2026-04-15: vLLM Gemma 4 31B serving, blake3-cached prompts, Haiku fallback for hard tail).
  2. Code-pretrained base — CodeBERT, CodeT5+-110M-embedding, or UniXcoder. MS-MARCO on web passages doesn't transfer to code.
  3. RRF fusion — combine reranker logit with hybrid score rather than replacing it. Preserves SPLADE signal.
  4. Don't over-retrieve — keep reranker input = top-K, not 4×K. Prevents R@20 drop.

  Bi-encoder alternative is blocked by the research/models.md "basin" result: v9-200k, v9-200k-hn, v9-200k-testq, v9-175k, v9-500k, v9-mini, v8, contrastive-B all land 81-82% R@1 on 296q regardless of training variation. Architectural ceiling of E5-base, not a training gap.

  Gating: idle GPU window + decision on LLM-judged vs click-signal labels. Calibration gate (≥85% Gemma↔Haiku agreement on 1k gold) before committing to local-only labeling. Full design in `~/training-data/research/models.md`.

### CPU Lane

**Retrieval quality:**
- [ ] **HyDE for structural queries — most promising untested lever.** v2-era data: +14pp structural, +12pp type_filtered, −22pp conceptual, −15pp behavioral. Router → LLM generates synthetic code → embed → search, per-category by design. Treat v2 numbers as motivation, not promise: this session saw several wins vanish through the full router (centroid, reranker v2, full alpha sweep). Design the experiment to hold the production router fixed and vary only the query embedding source. Prereqs already built (Gemma 4 31B via vLLM, BGE embedder, v3 eval harness).
- [ ] **BGE → E5 v9-200k.** Clean-index eval ties on R@1, slight edge on R@5/R@20, 1/3 the embedding dim (768 vs 1024). Gated on [#949](https://github.com/jamie8johnson/cqs/issues/949) (embedder abstraction) + v3 re-run to rule out noise.
- [ ] **CAGRA filtering regression on enriched index.** Fully-routed v1.24.0: conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release. Hypothesis: CAGRA graph walk strands in filtered-out regions. Concrete proposal in [#962](https://github.com/jamie8johnson/cqs/issues/962).
- [~] **Classifier accuracy — SCOPE REDUCED 2026-04-16.** The "4.5pp oracle gap" was an illusion. Breakeven simulation on v3 dev shows per-category α on Unknown queries is net-negative at *any* classifier accuracy (p=1.00 → −9.1pp). Root cause: alphas were tuned on queries the rule-based classifier was already confident about — a population with different retrieval characteristics than Unknown queries, which want α=1.0 (pure SPLADE weighting).

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

  Overall: 38.5% accurate, 49.5% fall to Unknown, 13.8% fire wrong. Dead paths: centroid matching (−4.6pp, infra preserved), logistic regression / fine-tuned MiniLM / LLM classify (same failure mode), negation idiom rule-fix (measured no R@1 change). Low-risk/low-value remainder: rule expansion for multi_step (`X AND Y`) and conceptual (better abstract-noun coverage) — wait for a larger eval set where 1pp is above noise. Details in `~/training-data/research/models.md`.

**Testing infrastructure:**
- [ ] **Rewrite slow CLI test binaries to in-process fixtures** ([#980](https://github.com/jamie8johnson/cqs/issues/980)). `cli_batch_test`, `cli_graph_test`, `cli_commands_test`, `cli_test`, `cli_health_test` gated behind `slow-tests` feature (PR #988) because each shells out to `cqs` and cold-loads the ONNX/HNSW/SPLADE stack per test case (~118 min combined on PR CI). Follow the `cli_notes_test` + `router_test` pattern: one `Store` + `CommandContext` per binary, call `cmd_*` handlers directly. Un-gates the feature and retires the nightly `slow-tests.yml` workflow.

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

## Open Issues (pre-audit)

| # | Finding | Difficulty |
|---|---------|-----------|
| #853 | DS-5: DEFERRED transactions → SQLITE_BUSY | medium |
| #854 | SEC-4: Reference path containment | medium |
| #855 | SHL-25: 25 env vars undocumented in README | easy |
| #856 | PB-5: atexit Mutex UB | hard |
| #848 | RM-1: Reduce tokio threads | easy |
| #847 | EXT-2: CLI/batch parity test | easy (subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947)) |

Audit-era issues under the `audit-v1.25.0` label: `gh issue list --label audit-v1.25.0`. Wave G backlog: #955, #958, #959, #960, #966, #969, #971, #974, #975.

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
