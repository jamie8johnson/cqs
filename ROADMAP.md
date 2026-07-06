# Roadmap

v1.x release history, shipped/closed roadmap items, and experiment post-mortems: [docs/roadmap-archive.md](docs/roadmap-archive.md) (the v0.x phases live there too); fuller per-release detail: [CHANGELOG.md](CHANGELOG.md).

## Current: v1.54.0 (2026-07-03, crates.io + GitHub Release)

*(Paragraph reconstructed from CHANGELOG — the release cut without a roadmap entry.)* **Slot-promote hot-reload, both stages (#2128 closed)** — a same-model AND a foreign-model `cqs slot promote` hot-reload a running daemon within a tick, store + embedder + `model_config` swapped in lock-step, no restart; slot pinning fixed on both daemon sides so an env- or flag-pinned daemon no longer splits query/write across slots after a promote (#2186/#2172). **MCP outputSchema closure (#2125/#2203)** — every advertised tool publishes a field-level result contract, with full-surface out-of-process stdio trial coverage. **Telemetry honesty family (#2197/#2198/#2211/#2212)** — daemon/MCP-served traffic finally counted, exactly-one-invoke booking under relay degradation, redaction split-brain closed, dashboard attribution fixed. **Worktree-overlay truth completions** — the CLI-direct diff commands emit the two-axis partial-skip pair (#2185), the pure-graph four emit `skipped-no-daemon`, the overlay diff frames to the SERVED root (repo-subdir projects stop reading "no impact"), and a worktree-added caller survives `--limit`/token caps. Plus daemon error taxonomy + honest relay budgets (#2178/#2179), reranker preamble-calibration fix (#2127), `plan --tokens` real JSON budgeting (#2138), and the any-length leading-comment `doc` fallback (#2126).

## Previous: v1.53.0 (2026-07-02, crates.io + GitHub Release)

**HNSW manifest-generation persistence** (#2165 — generation-suffixed artifacts behind an atomic manifest pointer; the post-crash save-wedge is GONE, pinned RED; live-migrated in production with the full audit battery: fable crash-matrix review held every cell + caught the model-show sizing straggler, sweep-audit confirmed code uniformity + fixed the SECURITY.md footprint straggler + landed the layout guard #2166, legacy-state audit landed six frozen-artifact guards #2168 + filed the dot-metric boundary finding #2167). Around it, the 27-PR post-v1.52.0 idle-loop batch: **the `--force` race family closed** (#2141 stale-sidecar guards, #2143 quiet-repo store reopen, #2153 orphaned-WAL replay + close-under-lock at the source), **worktree-overlay honesty campaign completed** (#2145 seed-family degradation markers, #2149 qualified-path merge, #2151 --ref marker gate, #2160 origin-aware def freshness, #2162 impact kind classification), **cross-project six completed** (#2152 deps was the last stub), `plan` text-mode token budget (#2139), write-time SPLADE scrubbing (#2146), insert_batch borrowed ids (#2150), JSON-noise re-decay guards CLI+lib (#2158/#2164), slow-tests lint gate (#2161), onnx reranker coverage (#2155). **Recall gate PASSED with `--require-fresh` ON** (the #2133-fixed freshness API's first release gate) — agg **45.9/71.1/85.8**; `src/search/` diff vs v1.52.0 EMPTY (scoring byte-identical; movement = live-migration rebuild noise + churn). Cross-build dry-run all-3-green before tagging.

---

## Active

### Retrieval quality

- [ ] **Expand the retrieval-INDEPENDENT gold set (PR-mined, off-self).** Replaces the culled synthetic-query expansion item (2026-07-06, archive): the v3/synthetic fixtures are retrieval-SEEDED — gold reached via the retriever, so R@K is inflated by construction — while the retrieval-independent number (PR-mined behavioral gold, n=2 cross-lang) sits near ~21% R@20. More seeded synthetic queries grow the flattering fixture, not the honest one. The valuable expansion mines independent gold from merged PRs at enough per-category N to make the independent number load-bearing for release gates; numbers live in ~/training-data/RESULTS.md.

- [ ] **Index-time HyDE re-eval** — never tested at proper N. v2-era single-config measurement showed +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral. Per-category routing (only enable hyde-augmented chunks for categories where sweep says it helps) was never tried. Properly testing requires (1) regenerate HyDE for all chunks via Claude Batches pipeline, (2) reindex with `--hyde-queries`, (3) per-category A/B harness toggling the hyde-augmented embedding column. Lower expected lift than the now-exhausted classifier work; cheap if revisited.

**Index backends (signal-side, recall-leaning):**
- [ ] **SIMD brute-force fast path under ~5K chunks** — still relevant, but small-project-only. Exact cosine, recall = 1.0 by construction. Wouldn't move cqs's own 14k-chunk eval, but would close the small-project gap and remove an index-build variable from per-slot A/Bs. Plugs into `IndexBackend` as a higher-priority backend that fires when `chunk_count < threshold`. Lower priority than the recall-saturated USearch finding (below) suggests; promote only if a small-project user reports a recall gap not explained by the `candidate_count` floor.

**Empirically closed — reopen only on new evidence (full post-mortems in [docs/roadmap-archive.md](docs/roadmap-archive.md)):**
- **Rerankers** — off-the-shelf ms-marco, the in-domain-trained V2 family (three losses), and the post-v1.39.0 re-verification: every variant net-negative at the rank-5 boundary (R@20 stays flat — gold is in the pool; the cross-encoder demotes it). Stays opt-in via `--rerank`. Revisit gates: v4-scale labelled fixture, a ≥5× base, or off-corpus proof of lift.
- **Query-time HyDE** — R@5 = 0.0% across all categories on both splits; killed. (The index-time re-eval above is a different direction and stays open.)
- **Alpha-routing arc** — distilled/fused heads and per-query α regression tested at proper N (v4, 1526/split): R@5 is alpha-insensitive across [0, 1] on this corpus; routing-side levers exhausted. Future R@5 work targets signal-side levers (chunking, embedder, multi-granularity index) under paired-reindex protocol. The routing-side items that predated this closure (soft routing, context-aware classification, seeded synthetic label expansion) were culled 2026-07-06 — full text in the archive.
- **USearch backend** — HNSW is already recall-saturated at the candidate-pool layer (`CQS_HNSW_EF_SEARCH` 1000→8000: byte-identical R@K); the headroom lived in the `candidate_count` floor (#1583).
- **Embedder scale** — the Qwen3-4B ceiling probe finished 2.7-2.8pp below gemma-300m at 13× the params; NV-Embed-v2 / llama-nv-embedqa dropped by transitivity. Scale retired as a knob for cqs.
- **v9-200k vs the default** — v9 un-retired once the eval-matcher line-drift artifact was fixed; stays an opt-in preset for memory/latency-bound cases. Default remains EmbeddingGemma-300m (since v1.35.0).

**Features (queued, no immediate work):**
- [ ] **Temporal search — `cqs history`** — query by author + time range, ranks by how little a chunk's been touched since. Uses git log + file/line mapping.
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish. Unblocked 2026-07-06: the adaptive-retrieval results it awaited now exist as definitive nulls (rerankers / routing / embedder scale) plus the seeded-vs-independent eval finding — a negative-results story.

**Agent adoption:**
- [ ] **Slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + pointer to `cqs --help`. Measure with telemetry before/after.
- [ ] **Composite search results** — `cqs search` returns mini-impact (caller + test counts) alongside each result. One call instead of search + impact. *(In flight 2026-07-06 — implementation lane running.)*
- [ ] **`cqs why "query"`** (né `cqs trace "query"` — the trace name is taken by the graph command) — show every routing decision: classifier → category → strategy → α → legs → fusion → finalize → final ranking. Today this requires `RUST_LOG=debug` + log grepping, and several decisions (RRF internals, boost mutations, drop counts, centroid declines) are invisible at any log level. The agent-facing version of "explain my retrieval". *(Design locked 2026-07-06: `docs/plans/2026-07-06-cqs-why-query-trace-design.md` — same-execution side-channel capture riding the search-legs core; per-token SPLADE attribution deferred to v2.)*

### Cross-Project Architecture

N-project via `[[reference]]` entries → `CrossProjectContext { stores: Vec<NamedStore> }`. Per-store BFS (callers, callees, impact, trace) matches cross-boundary edges by exact name only — wrappers, re-exports, and name mismatches are invisible.

- [ ] Type-signature matching for cross-boundary edges
- [ ] Import-graph resolution (parse `use`/`import` to resolve re-exports)
- [ ] Cross-project search with unified scoring (not just per-store RRF merge)
- [ ] `analyze_impact_cross` resolve file/line from CallGraph (currently empty paths — CQ-3)
- [ ] Cross-project dead code detection

---

## Open Issues

Re-audited 2026-06-10. The v1.39–v1.41 audit cycles closed nearly everything filed from v1.33.0, and the 2026-06-10 implementation campaign closed the last two medium-effort tracking issues: #1350 (ScoreSignal trait, PR #1719) and #1351 (DistanceMetric, PR #1722). Perf tier-3 lost #1244/#1229/#1228 (closed); refactor tier-3 lost #1216 (closed).

**v1.33.0 audit follow-ups: all closed** (#1350 via PR #1719, #1351 via PR #1722, both 2026-06-10).

**v1.40.0 audit cycle — deferred P1s (rationale in CHANGELOG v1.41.0):**

| Finding | Status |
|---------|--------|
| DS-V1.40-1: daemon Store cache invalidation via `PRAGMA data_version` | **FIXED** in PR #1718 (2026-06-10) — dedicated probe connection, WAL-write-no-checkpoint test |
| DS-V1.40-7: sentiment column CHECK constraint (schema v28) | Single-user discrete-value compliance is reliable; migration cost > benefit unless telemetry shows misuse |

**Perf tier-3 (real wins, but each ≥1hr):**

| # | Finding | Status |
|---|---------|--------|
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | perf: mmap SPLADE body (PF-11) | Audit-deprioritized — 59 MB peak transient, dominated by parse-side allocations |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | perf: HNSW fully loaded into RAM (RM-40) | Needs lib swap to hnswlib-rs (nightly-only) |

**Refactor tier-3 (architectural debt, no user-visible impact):**

| # | Finding | Status |
|---|---------|--------|
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

**Issue-state check (2026-07-06):** of the idle-loop backlog carried in past release notes, only #1804 and the #1459 tier-3 umbrella remain open; #1935 / #1937 / #1888 / #1573 / #1463 and the v1.49.0 carried rows #2043 / #2044 have all closed since.

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26.
- **Astro, ERB, EEx/HEEx, Move** — no tree-sitter grammar on crates.io.
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork.
- **ArchestrA QuickScript** — needs custom grammar from scratch.

---

## Parked

- **`nomic-ai/nomic-embed-code` (7B) — Phase 2 of the code-specific embedder A/B, deferred 2026-04-25.** Apache 2.0, base = Qwen2.5-Coder-7B, GGUF quantizations available. Released March 2025; reported to beat Voyage Code 3 + OpenAI Embed 3 Large on CodeSearchNet. ~14 GB VRAM at FP16; embedding is 3584-dim (4.5× current storage). Skipped because at 7B params, inference cost approaches an LLM call — defeats the local-embedder advantage. Would need agentic batching to amortize. Revisit only if Phase 1 (CodeRankEmbed-137M, opt-in via #1110) shows the code-specialist trade-off is worth pushing further at scale.
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`.
- Wiki system (agent-first), Blackwell RTX 6000 (96GB), L5X files from plant, KD-LoRA distillation (CodeSage→E5), ColBERT late interaction, enrichment-mismatch mining (Exp #4), lock/fork-aware training weights (Exp #5), ladder logic (RLL) parser, DXF/Openclaw PLC, SSD fine-tuning experiments.
