# Roadmap

## Current: v1.27.0 (audit-wave release)

54 languages. 29 chunk types. v3 eval canonical, regenerated to v2 fixture 2026-04-17 (109 test / 109 dev with strict==permissive matching after `regenerate_v3_test.py`). Daemon mode (`cqs watch --serve`, 99ms graph p50 / 200ms search-warm p50). Per-category SPLADE alpha routing via compile-enforced `define_query_categories!` macro. GPU-native CAGRA bitset filtering (patched cuvs 26.4). MSRV 1.95 (bumped from 1.93).

**v1.27.0** shipped 2026-04-16: closes 13 of 18 open issues from the post-v1.26.1 audit. Major perf wins (#917 streaming SPLADE, #966 stream-hash enrichment, #969 recency-based watch prune) + the `AuxModelConfig` preset registry (#957) that makes SPLADE-Code 0.6B a one-line config switch. See [`docs/audit-open-issues-2026-04-16.md`](docs/audit-open-issues-2026-04-16.md) for the audit ledger.

### Eval baselines on v3.v2 (canonical, 2026-04-17/18)

| Split | R@1 | R@5 | R@20 | Notes |
|---|---|---|---|---|
| **test (n=109), post-#1040 (chunker doc fallback + LLM regen)** | **41.3%** | **67.0%** | 75.2% | reindex 2026-04-18, 14,734 chunks, 47.7% LLM coverage |
| test (n=109), v1.27.0 shipping config | 41.3% | 63.3% | 80.7% | 2026-04-17 regen, 16,095 chunks |
| dev (n=109), post-#1040 | 40.4% | 71.6% | 79.8% | same reindex |
| dev (n=109), v1.27.0 shipping config | 41.3% | 74.3% | 86.2% | 2026-04-17 regen |

#1040 (chunker doc fallback for short chunks) plus an LLM summary regen lifts test R@5 to 67.0% (+3.7pp vs canonical) but drops dev R@5 to 71.6% (−2.7pp) and both R@20 by 5-6pp. Part of the dev / R@20 movement is corpus-pruning in the reindex (16,095 → 14,734 chunks) rather than the fix itself; a third reindex would help isolate. **R@5 ceiling moved up — the "cheap-lever well dry" claim from earlier in the session arc was wrong.** Subsequent A/B should always quote both test AND dev; N=109 per split is noisy at ±2-3pp single-trial.

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

- [ ] **Reranker V2 retrain with post-mortem fixes — open path.** Mine hard negatives against cqs's *own* index (~16k chunks) for domain match, keep TIE labels in pointwise as 0.5, cap reranker pool at 20. ~1-2 weeks. Plausibly lands where the Stack-v2-trained version didn't.

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
| post-v1.27.0 (unreleased) | **Cheap-lever sweep + Tier 3 chunker fix.** PR #1037 ColBERT 2-stage eval tool (default OFF). PR #1038 uniform JSON envelope across all CLI/batch/daemon-socket commands (Task #17). PR #1039 rustls-webpki 0.103.10→0.103.12 (Dependabot #7/#8). PR #1040 chunker doc fallback for short chunks (truncated_gold lever, test R@5 +3.7pp). |
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
