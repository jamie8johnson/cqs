# Roadmap

## Current: v1.26.0 + PR #1010 post-release fixes

54 languages. 29 chunk types. **v3 eval dataset** canonical (544 high-confidence dual-judge queries, train/dev/test 326/109/109). **Daemon mode** (`cqs watch --serve`, 3-19ms queries). Per-category SPLADE alpha routing. GPU-native CAGRA bitset filtering (patched cuvs 26.4).

**v1.26.0 shipped 2026-04-15:** watch-mode hardening + alpha re-fit on clean index + `--splade` CLI bug fix. 162 of 236 audit findings now closed across v1.25.0 + v1.26.0.

**Post-release fixes in PR #1010 (2026-04-16, will land as v1.26.1 or v1.27.0):**
- cqs batch RefCell panic in `invalidate_mutable_caches` (try_borrow_mut + deferred retry).
- Reranker `token_type_ids` bug: zeroed segment IDs silently broke fine-tuned BERT-family rerankers. Fixed to populate from tokenizer encoding.
- `CQS_RERANKER_MODEL` accepts absolute local directory paths alongside HF repo ids.
- Cross_language α 1.00 → 0.10 (v3 sweep finding; +1.8pp R@1 on v3 test).
- Centroid classifier infrastructure (disabled by default; `CQS_CENTROID_CLASSIFIER=1` to experiment, alpha floor wired).
- `tests/classifier_audit.rs` integration test: reports confusion matrix vs v3 consensus labels on every invocation.

### Eval baselines on v3 test (2026-04-16, production router, 3-trial stable)

| Config | R@1 | R@5 | R@20 |
|---|---|---|---|
| v1.26.0 alphas | 40.4% | 64.2% | 80.7% |
| **v1.26.0 + xlang=0.10 (shipping)** | **42.2%** | 64.2% | 78.9% |
| Full v3-swept per-category α | 41.3% | 63.3% | 78.9% |

**Measurement caveat.** Single-trial eval runs fluctuate ±1 query (~1pp) on v3 test. Always confirm over 3 trials before citing a delta. Single-trial readings earlier in this roadmap (e.g. "44.0% dev baseline", "45.0% test R@1") were noise; the stable numbers are above.

**Upper-bound analysis.** Forced-α sweep (bypassing strategy routing) tops out around 48% R@1 on v3 — the ceiling if the rule-based classifier perfectly routed every query. But the breakeven simulation on v3 showed per-category α routing on Unknown queries (~48% of traffic) is net-negative at *any* classifier accuracy. The real reachable ceiling from alpha + classifier tuning is ~1–3pp above the current 42.2%. Further R@1 progress requires representation changes (HyDE, reranker V2 at scale, embedder switch), not tuning.

### Historical reference numbers (pre-v3, not comparable to above)

v2 (265q) under v1.26.0 alphas: 39.2% R@1 / 58.8% R@5 / 78.6% R@20. v2 and v3 differ in query distribution and gold-chunk provenance, so v2 → v3 deltas shouldn't be read as improvements. BGE-large FT fixture eval (296q synthetic): 91.9% R@1 — different task entirely (chunk-to-description retrieval, not code-search).

---

## Active

### Refactoring Lane (post-audit, 2026-04-14)

High-leverage refactors that close entire bug classes — surfaced by the v1.25.0 audit. Each is its own GitHub issue.

- [x] **`Store` typestate** — #946 closed by PR #982 (merged 2026-04-15). Follow-up #986 tracks `open_readonly_after_init` replacement for `into_readonly()`.
- [x] **`Commands` / `BatchCmd` unification** — #947 closed by PR #981.
- [x] **`cqs::fs::atomic_replace` shared helper** — #948 closed by PR #983.
- [x] **Embedder model abstraction** — #949 closed by PR #984.
- [x] **CAGRA persistence** — #950 closed by PR #985.

### Quick-wins Lane (Tier-1 ROI from audit issues)

- [x] **WSL 9P/NTFS mmap auto-detect** — #961 closed by PR #979.
- [x] **CAGRA itopk + graph_degree env overrides** — #962 closed by PR #979.
- [x] **Reranker batch chunking** — #963 closed by PR #979.
- [x] **Daemon `try_daemon_query` test scaffold** — #972 closed by PR #999 (Wave D1, 2026-04-15).

### Waves D–F (Tier-1/2/3 batch closed 2026-04-15)

- [x] **Aho-Corasick + LazyLock language_names (#964)** — Wave D2, PR #992. 1.31× on type_filtered classifier path. Prereq for classifier accuracy investigation.
- [x] **dispatch_search content-asserting tests (#973)** — Wave E2, PR #997. Closes silent-regression test-gap class.
- [x] **Shared Arc<Runtime> (#968)** — Wave E3, PR #1000. Daemon consolidates to one `cqs-shared-rt` worker pool.
- [x] **Migration fs-backup (#953)** — Wave E1, PR #996. Data-integrity: restore DB byte-identical on migration failure.
- [x] **NameMatcher ASCII fast path (#965)** — Wave F1, PR #990. 1.2-1.5× on search hot path.
- [x] **`open_readonly_small` (#970)** — Wave F2, PR #993. 256MB → 16MB mmap per reference store.
- [x] **Reindex drain-owned chunks (#967)** — Wave F3, PR #991. ~180MB / ~1.4M allocs saved per 20-file watch burst.
- [x] **INDEX_DB_FILENAME constant (#923)** — Wave F4, PR #994. 56 literal sites unified.
- [x] **CAGRA sentinel INVALID_DISTANCE (#952)** — Wave F5, PR #995.
- [x] **`open_readonly_after_init` + drop unsafe `into_readonly` (#986)** — Wave F6, PR #998. *Merged 2026-04-15 (v1.26.0).*

### Watch-mode + SPLADE hardening (v1.26.0, 2026-04-15)

- [x] **`cqs watch` respects `.gitignore` (#1002)** — PR #1006. `.claude/worktrees/*` no longer polluting the index.
- [x] **Incremental SPLADE in `cqs watch` (#1004)** — PR #1007. Tier-1. Dense+sparse inline, no more coverage drift.
- [x] **`--splade` flag no longer bypasses router** — PR #1008. CLI-level semantic bug from pre-routing era; `splade_alpha: Option<f32>` + unified match arm.
- [x] **Per-category alpha re-fit on clean index** — PR #1005. +1.8pp R@1 on v2 eval (39.2% vs 37.4% corrected baseline).

Full list: 25 issues #951–#975, all labeled `audit-v1.25.0`. See `gh issue list --label audit-v1.25.0`. Remaining tier-2/3 form the Wave G backlog: #955, #958, #959, #960, #966, #969, #971, #974, #975 + upstream-blocked.

### GPU Lane

- [ ] **Reranker V2** — code-trained cross-encoder. Pilot experiment 2026-04-16 on v3 dev showed the small-data approach is net-negative: fine-tuning `ms-marco-MiniLM-L-6-v2` on 2270 v3 pool triples gave R@1=38.5% vs single-trial baseline of 44.0% (pre-stabilization reading — the stable baseline is ~41-42%). Default ms-marco without fine-tuning: 28.4%. Either way, net-negative. Full pipeline now verified end-to-end (training, ONNX export, cqs local-path loading, `--rerank` flag integration).

  **Why the pilot failed and what it teaches:**
  1. Hybrid dense+SPLADE retrieval is already well-calibrated — a cross-encoder scoring on (query, chunk_text) alone has strictly less signal than the hybrid scorer.
  2. Over-retrieval (4× when `--rerank` is set) pushes gold chunks beyond rank 20 even when the model is right most of the time. R@20 drops too, not just R@1.
  3. 2270 examples + MS-MARCO base model is insufficient for code queries.

  **Prerequisites to make V2 net-positive (must address all three):**
  1. **Scale: 200k+ Gemma-labeled pairs.** Pipeline built 2026-04-15 (vLLM Gemma 4 31B serving, claude_client.py fallback, blake3-cached prompts). Pairwise preferences across `augmented_200k_keydac` or generated-from-chunks corpus.
  2. **Base: code-pretrained encoder.** CodeBERT, CodeT5+-110M-embedding, or UniXcoder. MS-MARCO on web passages doesn't transfer.
  3. **Fusion: RRF instead of replace.** Combine reranker logit with original hybrid score via reciprocal rank fusion rather than using the reranker alone. Preserves SPLADE signal.
  4. **Don't over-retrieve.** Keep reranker input = top-K, not 4×K. Prevents the R@20 drop.

  **Why not the bi-encoder instead:** research/models.md "basin" result — v9-200k, v9-200k-hn, v9-200k-testq, v9-175k, v9-500k, v9-mini, v8, contrastive-B all land 81-82% R@1 on 296q regardless of training variation. That's the architectural ceiling for E5-base, not a training gap. Further preference data on the bi-encoder won't move the basin.

  **Bug fix prerequisite (DONE):** `src/reranker.rs` was zeroing `token_type_ids` before ORT inference. BERT-family rerankers use segment IDs to distinguish query (0) from passage (1). Default ms-marco was robust to all-zeros; fine-tuned models break catastrophically. Fixed 2026-04-16 (in PR #1010). Any future reranker upgrade needs this fix.

  **Data pipeline is the long pole:**
  1. **Local-LLM-judged pairwise preferences (primary)** — `Gemma 4 31B Dense` at Q4_K_M via vLLM on the A6000 scores `(query, chunk_A, chunk_B)` across augmented_200k_keydac or combined_9lang_hard_negs. Apache 2.0, ~20GB VRAM at Q4, ~28GB headroom left for KV cache + context. Gemma 4 release (2026-04-02) leads open-weights coding benches — 80.0% LiveCodeBench v6, 2150 Codeforces ELO. Cost: $0 per pass. Throughput estimate: ~500-1000 tok/s batched → 200k labels in ~5h. Cached by content hash (same pattern as contrastive summaries SQ-10b). Fallback tier: `Gemma 4 26B MoE` (3.8B active/token, ~2-3× throughput, ~2 point lower LiveCodeBench) for bulk clear-cut pairs.
  2. **Claude for the hard tail (secondary)** — Haiku or Sonnet on the subset where Gemma's per-pair log-prob confidence is low (< threshold) or where a calibration run shows <70% agreement vs. a 1k gold subset. Haiku batch rate ~0.3¢/query; sized to cost pennies if Gemma handles 80%+ of the corpus.
  3. **Click-signal from agent telemetry** — 16k+ cqs invocations logged per ROADMAP "Agent Adoption" section. Sequences where `search` → `gather`/`context` on a specific chunk within N turns are implicit positive signals. Cheap, noisy vs. explicit LLM judgments; best used to validate/backfill the LLM-judged labels.

  **Calibration gate before full run:** label a 1k-query gold subset with both Gemma 4 31B and Haiku, compute inter-model agreement. ≥85% → local-only is fine. 70-85% → hybrid Gemma-then-Claude for low-confidence pairs. <70% → Claude-only (Gemma not tracking judgment quality).

  **Training budget:** ~1-2 days on the A6000 once data exists. Architecture likely `jinaai/jina-reranker-v2-base` or similar 100-300M param cross-encoder — small enough to ONNX-export and ship inside `~/.local/share/cqs/` like the current reranker. SPLADE work is unrelated (all `[x]` historically, cqs-side null result; full breakdown in `~/training-data/research/sparse.md`).

  **Gating:** dedicated project, not a drive-by. Waiting on (a) an idle GPU window and (b) decision on LLM-judged vs. click-signal corpus.

### CPU Lane

**Eval & retrieval quality:**
- [~] **Classifier accuracy investigation — SCOPE REDUCED 2026-04-16.** The "4.5pp oracle gap" was an illusion. A breakeven simulation on v3 dev showed that per-category alpha routing on Unknown queries is net-negative at ANY classifier accuracy, including p=1.00 (−9.1pp R@1 at perfect accuracy). Root cause: the per-category alphas were tuned on queries the rule-based classifier was already confident about — a population with different retrieval characteristics than Unknown queries. Unknown queries want α=1.0 (pure SPLADE scoring weight) because dense embeddings don't capture their semantics well.

  **Audit data (v3 dev, 109 queries; `cargo test --test classifier_audit`):**

  | v3 label | N | Fire rate | Correct | Precision when fires |
  |---|---|---|---|---|
  | negation | 17 | 100% | 100% | 100% ✓ |
  | cross_language | 11 | 82% | 82% | 100% ✓ |
  | identifier_lookup | 18 | 61% | 61% | 100% — recall gap, α=1.0 already optimal |
  | structural | 8 | 50% | 38% | 75% |
  | type_filtered | 13 | 46% | 8% | 17% — misfires into structural/conceptual |
  | multi_step | 14 | 43% | 0% | 0% — "AND" conjunctions get caught by structural first |
  | behavioral | 16 | 19% | 6% | 33% |
  | conceptual | 12 | 0% | 0% | 0% — abstract-noun patterns don't match v3 phrasings |

  Overall: 38.5% accurate, 49.5% fall to Unknown, 13.8% fire wrong.

  **What's dead:**
  - Option 2 (centroid matching): measured at 76% accuracy → −4.6pp R@1 on v3 dev. Disabled but infra preserved (`CQS_CENTROID_CLASSIFIER=1` to experiment).
  - Option 3 (logistic regression): would fail the same way — simulation proved accuracy gains can't overcome the per-category alpha mismatch.
  - Option 4 (fine-tuned MiniLM) and 5 (LLM classify): same.
  - Option 1 rule-fix pilot (negation idiom guards: `not null`, `avoid`, etc.) measured 2026-04-16: eliminated 2 misfires, no R@1 change within noise. Reverted. Cheap classifier precision improvements don't move R@1 because queries that fall to Unknown get α=1.0, which is close to optimal for most of them.

  **What's still worth doing (low-value but low-risk):**
  - Rule expansion for multi_step (catch "X AND Y" patterns) and conceptual (better abstract-noun coverage) — only worth doing when we have a larger eval set where 1pp is above noise.

  Details and breakeven simulation in `~/training-data/research/models.md`.

- [x] **Re-fit per-category alphas on clean index** — **Done 2026-04-15 (v1.26.0, PR #1005).** ident 0.90→1.00, struct 0.60→0.90, concept 0.85→0.70, behav 0.05→0.00 (dense-only), neg 1.00→0.80 (explicit arm). Fully-routed R@1 lands at 39.2% (+1.8pp over v1.25.0 corrected baseline; +3.4pp over dense-only).
- [x] **Eval expansion: v3 consensus dataset** — **Done 2026-04-15.** 544 high-confidence dual-judge queries (Claude Haiku + Gemma 4 31B consensus). Train/dev/test 326/109/109, stratified. Every category N≥23 (was N=1 for multi_step in v2). Pipeline: telemetry mining (328 real) + chunk-seeded generation (522) + pooled retrieval (3 cqs variants) + dual LLM judge. Non-tautological gold: generated seeds fixed before retrieval; telemetry gold from pooled candidates + independent LLM validation. Details in `~/training-data/research/models.md`.
- [ ] **Investigate CAGRA filtering regression on enriched index** — fully-routed v1.24.0 showed conceptual −5.5pp, structural −3.8pp, identifier −2pp vs pre-release baseline. Hypothesis: CAGRA graph walk strands in filtered-out regions. Concrete proposal in [#962](https://github.com/jamie8johnson/cqs/issues/962) (Quick-wins Lane).
- [ ] **Query-time HyDE for structural queries — most promising untested lever.** Old v2-era data: HyDE +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral. Router classifies structural → LLM generates synthetic code → embed → search. Per-category by design, attacks representation directly. Treat the v2 numbers as motivation, not promise — this session's experience with wins-that-vanish-through-the-router (centroid, reranker v2, full alpha sweep) argues for measuring on v3 dev before declaring it a win. Prerequisites already built: Gemma 4 31B via vLLM for generation, BGE embedder, v3 eval harness. Design the experiment to hold the full-router path fixed and vary only the query embedding source.
- [ ] **Switch production default BGE → E5 v9-200k** — clean-index eval shows ties on R@1 + slight edge on R@5/R@20 + 1/3 the embedding dimension (768 vs 1024). Gated on Embedder model abstraction ([#949](https://github.com/jamie8johnson/cqs/issues/949)) and a confirmation re-run to rule out 1-query noise.

**Daemon & data:**
- [ ] **Daemon: full CLI parity** — batch parser subset differs from CLI. Subsumed by [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification.
- [x] **Daemon: incremental SPLADE in watch mode** — **Done 2026-04-15 (v1.26.0, #1004, PR #1007).** Watch now encodes sparse vectors for changed files inline alongside dense, batches at `CQS_SPLADE_BATCH` (default 32), kill-switch `CQS_WATCH_INCREMENTAL_SPLADE=0`.

**Testing infrastructure:**
- [ ] **Rewrite slow CLI test binaries to in-process fixtures** — issue [#980](https://github.com/jamie8johnson/cqs/issues/980). `cli_batch_test`, `cli_graph_test`, `cli_commands_test`, `cli_test`, `cli_health_test` are gated behind the `slow-tests` feature (PR #988) because each shells out to `cqs` and cold-loads the full ONNX/HNSW/SPLADE stack per test case (~118 min combined on PR CI). Follow the `cli_notes_test` + `router_test` pattern: open one `Store` + `CommandContext` per binary, call `cmd_*` handlers directly. Un-gates the feature and retires the nightly `slow-tests.yml` workflow.

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

### Agent Adoption — Telemetry Data

**49,242 cqs invocations** logged at `~/.cache/cqs/query_log.jsonl` since 2026-04-06 (snapshot 2026-04-16). Only 328 unique real queries (99% duplicate rate); 99.9% are `search`. The distribution below is from the 2026-04-09 analysis which split main-conversation vs subagent invocations and still reflects the relative command mix.

Historical profile (2026-04-09, 16,731 invocations), two distinct profiles:

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
| v1.26.0 | **Watch-mode + SPLADE hardening batch.** `cqs watch` respects `.gitignore` (#1002, PR #1006) — ends worktree pollution. Incremental SPLADE encoding in watch (#1004, PR #1007) — coverage stays 100% during active dev. Per-category α re-fit on the genuinely-clean 14,882-chunk index (PR #1005, +1.8pp R@1 to 39.2%). `--splade` CLI flag no longer bypasses the router (PR #1008) — a pre-routing-era bug surfaced during the "phantom regression" investigation. `Store::open_readonly_after_init` closure-based constructor replaces unsafe `into_readonly` (#986, PR #998). Closes 4 additional audit findings on top of v1.25.0. |
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
