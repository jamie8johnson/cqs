# Project Continuity

## Right Now

**Reranker V2 Phase 1 done — verdict GEMMA_ONLY** (Task #18, 2026-04-17). 1000 sampled triples from `~/training-data/augmented_200k_keydac.jsonl` labeled by both Gemma 4 31B (vLLM local) and Claude Haiku. Result: **98.3% inter-rater agreement, kappa 0.9663**, 0 parse errors, ground-truth agreement 98.79% (Gemma) / 98.30% (Claude). Clears 85% threshold by 13.3pp → use Gemma alone for the 200k pass. **PR #1031 open, in CI** (`feat/reranker-v2-phase1-calibration`).

**Reranker V2 Phase 2 in flight** (Task #19, 2026-04-17). Agent `a499dc706d1e2e055` building Stack v2 corpus + Gemma labeling. Phase 1 calibration source was 100% Python/CodeSearchNet — Phase 2 switches to `bigcode/the-stack-v2-dedup`, 9 languages (rust/py/js/ts/go/java/cpp/ruby/php), ~25k chunks/lang, function-level w/ docstring, BGE-large embed → HNSW → 7 hard negatives per (query, positive) → sample 200k triples → Gemma label via `--gemma-only` (Phase 1 decision). Final output: `evals/reranker_v2_train_200k.jsonl`. Wall ~32h on A6000 vLLM. Phase 3 (training) gated on corpus quality review.

**R@5 audit + strategy + gc fix + MMR experiment landed on the same branch** (Tasks #3, #4, #21, #22). Audit shows permissive R@5 = **64.2%** (matches documented v1.27.0 baseline exactly after the gc cleanup). Strict R@5 = 51.4% — the 13pp gap is v3-fixture drift, not retrieval drift. Failure modes: `near_dup_crowding` 60%, `wrong_abstraction` 45%, `unexplained` 15%. Strategy doc: `docs/r5-strategy-2026-04-17.md`.

**MMR result: negative.** Surface-feature MMR (file/dir/name similarity) regressed R@5 at every λ < 1.0 in the v3-test sweep, even after calibrating same-file penalty 1.0 → 0.4. Root cause: pool expansion re-triggered type-boost re-sort, shifting top-1. Code shipped as inert opt-in infrastructure (`CQS_MMR_LAMBDA` env / `SearchFilter.mmr_lambda`) for future embedding-MMR experiments. **Type-boost calibration (CQS_TYPE_BOOST sweep):** noise window ±1pp; default 1.2 stays. **GC fix #21:** dropped 522 worktree+gitignored chunks the daemon was missing because `origin_exists` had a `Path::exists()` fallback that kept any extant file regardless of indexer ownership.

**Branch:** `feat/reranker-v2-phase1-calibration`. PR #1031 expanded scope: calibration + audit + strategy + gc fix + MMR infra. CI is in flight (clippy + fmt + test fixes pushed at ad1be3d).

**Phase 2 status:** Stage A done (225k chunks, 9 langs, ~56 min). Stage B (BGE embed) running at 67 chunks/sec on A6000, 97% GPU, ~24 min ETA. Stage C (HNSW + hard-neg mining) next, then Gemma labeling pass.

### What landed this session (after v1.27.0)

| PR | Closes | Highlight |
|---|---|---|
| #1023 | release v1.27.0 | Audit-wave + MSRV bump 1.93→1.95 |
| #1024 | tears | v1.27.0 ROADMAP refresh + embedder swap workflow plan |
| #1025 | publish 413 fix | Excluded `evals/`, `samples/`, `tools/`, `cuvs-fork-push/` from package |
| #1026 | #6, #7 | Embedder hygiene (index-aware resolution + dim-mismatch error) + proactive GC (startup + retroactive gitignore + idle-time periodic) |
| #1027 | #11, #13, #14, #9 | `cqs stats` field expansion + `cqs doctor --verbose` + `cqs ping` + `cqs eval` subcommand |
| #1028 | #12, #8 | `--limit` standardization + `--json` propagation through batch |
| #1029 | #10 | `.gitattributes` + LF renormalize (closed CRLF tax) |
| #1030 | #15, #16 | `cqs model swap` + `cqs eval --baseline` regression gate (merged) |
| #1031 | Reranker V2 Phase 1 | Calibration gate → GEMMA_ONLY (98.3% agreement, kappa 0.97) |

**v1.27.0 published on crates.io 2026-04-16.** GitHub Release with binaries built. crates.io was 503-ing initially; resolved after PR #1025 fixed 413 (eval datasets pushed package over 10MB).

### v9-200k embedder eval result (2026-04-16)

Completed. **Verdict: don't switch.** v9-200k v3 test = R@1 28.4% / R@5 49.5% / R@20 71.6% vs BGE-large 42.2% / 64.2% / 78.9%. Net −13.8/−14.7/−7.3pp. Confound: `--force` reindex dropped chunks 15.5k → 10.7k (stale-row cleanup), but the gap is too large for that alone to explain. v9-200k is fine-tuned E5-base; not a true code-aware model. The ROADMAP "ties on R@1" claim was on 296q fixture (chunk-to-description), not v3 real-code search.

### v3 test baselines (still current)

| Config | R@1 | R@5 | R@20 |
|---|---|---|---|
| **v1.27.0 shipping (xlang=0.10)** | **42.2%** | 64.2% | 78.9% |
| Forced-α ceiling (no router) | ~48% | (untested) | (untested) |

**R@5 ceiling on v3 is unmeasured.** Forced-α R@5 ceiling is the next sanity check before committing to Reranker V2.

## What's queued

Strategy ordering per `docs/r5-strategy-2026-04-17.md` (Tier 1 ships independent of Phase 2):

- **Tier 1.1 — Eval-data hygiene** — regenerate `v3_test.json` against current corpus + Gemma re-judge of name-only matches (~14 queries). Half-day. No R@5 change but pre-requisite for trusting future lever measurements.
- ~~**Tier 1.2 — MMR re-rank**~~ — tested, **negative result**. Surface-feature MMR regressed R@5 at every tested λ. Code shipped inert as opt-in (`CQS_MMR_LAMBDA`). Embedding-MMR is the obvious follow-up if revisited.
- ~~**Tier 1.3 — Chunk-type aware boost**~~ — partial test via `CQS_TYPE_BOOST` sweep (1.3 → 2.0). Within ±1pp noise window of default 1.2. Already wired via `extract_type_hints` + Aho-Corasick. Default stands; no action.
- **Tier 2 — Reranker V2 Phase 2 in flight + Phase 3** (Tasks #19, #20). Catches `unexplained` (15%) plus likely improves wrong_abstraction. +5-10pp realistic. ~5d to deployment.
- **Tier 3 — chunker fix for `truncated_gold`** (3 queries). Reindex cost; smaller lift.
- **JSON output schema standardization** (Task #17) — bigger refactor, runs whenever ergonomics work resumes.

## R@5 90% target — analysis

Honest framing (per session discussion):
- **80% R@5 is the sharp goal.** Achievable with audit + Section-2 stack (MMR, larger pool, α resweep, chunk-type boost) + multi-query + per-category HyDE. Lower variance, infrastructure mostly exists. Estimated P(hit) ≥ 70%.
- **90% R@5 is stretch.** Requires Reranker V2 done right with all 4 prereqs OR ColBERT (1-3 month architectural lift) OR both. Estimated P(hit) 30-40% even with full stack.
- **Wins-vanish-through-router** is the recurring structural risk (centroid: −4.6pp, reranker v2 pilot: −5.5pp, full α sweep: only xlang transferred). Each phase must validate end-to-end on v3 dev.

## Architecture state

- **Version:** v1.27.0 (live on crates.io + GitHub Releases with binaries)
- **MSRV:** 1.95
- **Local binary:** built from main; reinstall after merge with `cargo build --release --features gpu-index && systemctl --user stop cqs-watch && cp ~/.cargo-target/cqs/release/cqs ~/.cargo/bin/cqs && systemctl --user start cqs-watch`
- **Index:** ~14.9k chunks (BGE-large; stale-row cleanup after the v9-200k experiment may have shifted count slightly)
- **Production R@1 baseline on v3 test:** 42.2% / R@5 64.2% / R@20 78.9%
- **Open PRs:** #1031 (Phase 1 calibration, in CI)
- **Open issues:** 5 — all tier-3 deferred or external-blocked: #106 (ort upstream RC), #717 (HNSW lib swap), #916 (mmap SPLADE — depriorotized behind #917 which shipped), #956 (CoreML/ROCm needs non-Linux CI), #255 (pre-built ref packages design)
- **cqs-watch daemon:** running latest binary
- **CRLF tax:** killed by #1029 (`.gitattributes` + `* text=auto eol=lf`)

## Operational pitfalls (rolling forward)

- **Agent worktree leak via absolute paths** — `isolation: "worktree"` is *soft* isolation; agents using absolute paths in tool calls write to parent tree. Add explicit path-discipline text to every parallel-agent prompt. **Filed as Anthropic feedback this session.**
- **Cargo publish 413 = "exclude" list missing** — `evals/queries/v3_*.json` pushed package over 10MB. `Cargo.toml` exclude list now blocks `evals/`, `samples/`, `tools/`, `cuvs-fork-push/`. Re-check after adding any new heavy dir.
- **Cargo publish 503 = transient CDN** — auto-retry within minutes resolves it. Don't bump version chasing 503s.
- **3-way `git merge-file` works well** for parallel-agent aggregation when files don't overlap line-for-line. Pattern: `git merge-file <current> <main_base> <other_worktree>` produces clean merge for non-overlapping clap variant additions.
- **systemd doesn't inherit shell env** — `CQS_EMBEDDING_MODEL=v9-200k systemctl start cqs-watch` doesn't propagate; use `systemctl --user set-environment KEY=VAL` then start. (Mostly obsolete now since #1026 made model resolution index-aware.)
- **Always run `cqs eval --baseline` after retrieval changes** — the regression gate from #1030 catches per-category R@K drops automatically. Save baselines per release: `evals/baseline-v1.27.0.json` etc.
- **Reranker V2 prereqs are non-negotiable**: 200k+ Gemma pairs, code-pretrained base (NOT MS-MARCO), RRF fusion (don't replace), top-K input (no over-retrieval). Pilot violated all 4 → −5pp. The 200k corpus build (Task #18-20) is the path to satisfying #1.
- **No time estimates in specs** (per user feedback this session) — they're systemically too long. Frame in compute units / GPU hours / step counts.

## What's parked

- **HyDE on v3 dev** — most promising untested representation lever. Per-category routing required (v2-era data: +14pp structural, −22pp conceptual). Treat v2 numbers as motivation only — wins-vanish risk is real.
- **ColBERT-class late interaction** — biggest single lever for code retrieval (+10-25pp R@5 on benchmarks). Architectural rebuild. Plan: add `Reranker` trait → ColBERT impl as 2-stage re-ranker first → only do full per-token index if it wins.
- **Code-aware embedder switch** — CodeBERT, CodeT5+-110M-embedding, UniXcoder all untested on v3. v9-200k didn't help but it's not a true code-aware model.
- **Knowledge-augmented retrieval** — use the call/type graph as a structured filter. Multi_step queries currently weakest at 28-43% R@1; KG could help.
- **Meta-routing** — current router commits to one strategy; ensemble of strategies with learned weights could stop the wins-vanishing pattern.
