# Project Continuity

## Right Now

**v1.39.1 shipped** — 2026-05-07 (same day as v1.39.0). Patch release on crates.io. One PR (#1584) plus a docs-only follow-up (#1582) for reranker closeout.

**The fix**: CAGRA enforces `itopk_size >= k` and `itopk_size <= itopk_max(n_vectors)`, where `itopk_max = (log2(n_vectors) * 32).clamp(128, 4096)` — 441 at 14k chunks, 532 at 100k. A search request with `k > itopk_max` returned an empty `Vec` from `cagra::search_impl` with a comment claiming the caller would fall back to HNSW. `search_filtered_with_index` did fall back via the brute-force escape hatch, but `search_hybrid` (the SPLADE-fusion path used by every production query) did not — empty dense leg + α-weighted sum collapsed every fused score to `1.0·0 + 0.0·s = 0` at α=1.0.

**How it surfaced**: User asked "is the k*2 floor too high? Probe lower ef_search values." Sweeps across `CQS_HNSW_EF_SEARCH` produced byte-identical R@K because hnsw_rs internally floors ef to k (line 1450 `let ef = ef_arg.max(knbn)`). Pivoted to `CQS_SEARCH_CANDIDATE_FLOOR` sweep, found a sharp cliff at floor=442 in dense-only (R@5 0.66 → 0.17). Traced back to CAGRA's itopk_max for our 14k-chunk corpus = `floor(log2(14181) * 32) = 441`.

**Fix shape**: `VectorIndex::max_k() -> Option<usize>` declares per-call backend capacity. `cap_k_to_backend(idx, k)` trims `k` to the cap with a debug log tagging the trimmed backend. Both dispatch sites (`search_hybrid`, `search_filtered_with_index`) call it before `search_with_filter` / `search`. CAGRA returns `Some(itopk_max)` honouring `CQS_CAGRA_ITOPK_MAX`. CAGRA's existing `return Vec::new()` branch retained as defense-in-depth.

**Empirical impact** on 14k-chunk corpora at default floor=500 (from #1583):
- Hybrid (router α): R@5 0.5963 → 0.7156 (+12pp restored on v3.v2 test 109q)
- Dense-only (α=1.0): R@5 0.1651 → 0.6606 (+49pp; capped at max_k=441, no cliff)
- Dev set (109q): flat across floor=500..1000 in hybrid mode — confirms floor=500 still the right default

**Pacing lesson**: The cliff was technically present from the moment #1583 bumped the floor 100→500, because the new floor=500 immediately exceeded itopk_max=441 on our own corpus. v1.39.0 shipped with this regression. The fix went out as a patch on the same day, but lesson generalizes: **a recall-floor bump is a load-bearing change that needs a paired-eval sanity check on the actual production stack** (not just the formula's contract). The formula was correct; the interaction with CAGRA's per-backend k limit wasn't on the radar.

**Other v1.39.1 mini-cleanup**: Closed #1582 (reranker V2 closeout docs — README + ROADMAP confirm rerankers stay net-negative on v3.v2 post-v1.39.0; future revisit gated on v4-scale fixture or a 5× bigger base). Closed #1583 as superseded — its `candidate_count_for` floor=500 helper was already on main via #1584's squash (the cliff fix branched from #1583's branch).

---

**v1.39.0 shipped** — 2026-05-07. Minor release. 88 commits since v1.38.0 across three threads:
- v1.38.0-cohort audit follow-ups (#1487–#1511)
- post-v1.38 audit cycle of 154 findings catalogued in PR #1515 — ~64 closed across ~33 cluster PRs (#1514–#1570)
- post-cycle hardening of the watch/reindex path (#1572, #1575, #1577)

**Headline operator-visible changes** (v1.39.0):
- Daemon stops SIGFPE'ing on EmbeddingGemma reindex (#1577 TRT-incompatibility blocklist; root cause #1576 upstream). Pre-fix observed 4 daemon crashes/day.
- Cross-project commands (`trace`, `callers`, `deps`, `impact`, `test_map`) work again on slot-migrated projects since #1105 (#1564 fix).
- Atomic per-file reindex (#1575) — mid-batch crash leaves no asymmetric state between `function_calls` and chunks/FTS.
- `cqs dead` noise rate cut from ~80% to ~30% (#1572 — Property + doc-extension filters).
- Graph commands now reject `--limit 0` at parse time (#1569 LimitArg fan-out).

**Operational lesson from v1.39.0 release**: #1495's `cqs-macros` workspace split (which landed AFTER v1.38.0 was tagged) had `publish = false`, blocking `cargo publish -p cqs`. Fixed in #1579: dropped publish=false, filled in standard Cargo.toml metadata, then published cqs-macros 0.1.0 first followed by cqs 1.39.0. **Going forward both crates need version bumps coordinated whenever cqs-macros's surface changes.**

## Audit umbrellas — current state

- ✅ **#1463 (P4 design-level)** — ~64 of 154 findings closed across the v1.38 cycle. Truly remaining (all genuinely big or platform-blocked):
  - **API-V1.38-6** (top-level Cli flag → subcommand parity) — clap conflict on duplicate flag definitions; needs SearchArgs locals removed AND every search-wrapping handler rewritten to read from cli.*. Lib `cqs::scout()` doesn't accept filter knobs at all.
  - **DS-V1.38-4 deeper hazard** (HNSW half-renamed-set under a lock-then-rename window) — needs bundle-into-single-file refactor + migration path. Easy mitigation already shipped in #1570.
  - **PL-V1.38-2** (SPLADE Windows umask) — needs Windows test runner.
  - **TC-HAP-V1.38-3** (`enrichment_pass` itself untested) — needs real embedder load (~91 MB).
  - 12 P4 carry-overs all tracked separately (#1512 Windows daemon, #1461 Windows ACL, etc.).
- ⏳ **#1459 (P3 API design)** — 7 of 8 sub-items shipped. Item 2 (project/ref verb consolidation) remains; user investigation found ref + project are genuinely distinct primitives.
- ✅ **#1460, #1461, #1462** — closed in v1.38.0
- ✅ **#1366** (proc-macro CLI derive) — closed by #1495
- ✅ **#1452** (skip first-pass embed) — closed by #1497
- ✅ **#1453** (per-slot SPLADE α) — closed by #1472
- ✅ **#1458** (TC Happy 5 tests) — closed in v1.39.0 cycle

## Open issues (re-verified 2026-05-07)

All 15 open issues are still relevant — none stale.

| # | Status | Why open |
|---|---|---|
| #106 | tier-3 | ort 2.0-rc.12 stable release blocked on upstream pykeio |
| #255 | tier-3 | Pre-built reference packages — signing/registry design (infra, not code) |
| #717 | tier-3 | HNSW mmap — needs lib swap to hnswlib-rs (nightly-only) |
| #916 | tier-2 | SPLADE mmap — audit-deprioritized (59 MB transient) |
| #1043 | platform | Windows network drives — needs Windows test runner |
| #1139 | enhancement | structural_matchers shared library — partially landed (per-language data exists; cross-language sharing remains) |
| #1140 | enhancement | Embedder preset extras map — explicitly skipped per autopilot directive |
| #1350 | architecture | apply_scoring_pipeline hand-coded — P4-14 deferred |
| #1351 | architecture | HNSW DistCosine type-baked — needs persist migration |
| #1391 | enhancement | NVRTX (TensorRT-RTX) — blocked on ORT Linux platform gate |
| #1459 | umbrella | API design — 1 of 8 items remaining (project/ref verb consolidation) |
| #1463 | umbrella | P4 — see audit umbrella state above |
| #1512 | platform | Windows daemon named pipes — needs Windows runner |
| #1573 | new | cqs dead tier 2/3 false-positive sources (filed during v1.39.0 cycle) |
| #1576 | upstream | TensorRT 10 SIGFPE during ONNX engine compilation for Gemma — filed against NVIDIA |

## Recent release history (compressed)

- **v1.39.0** (2026-05-07) — 88-commit minor release. v1.38 audit cycle + post-cycle hardening (atomic reindex #1575, TRT blocklist #1577, cqs dead noise filter #1572). Schema unchanged at v27.
- **v1.38.0** (2026-05-06) — 13 audit-driven PRs closing #1460/#1461/#1462. Per-slot SPLADE α tables (#1472), TOML overlays for FTS synonyms + classifier vocab, `cqs serve` concurrent-request cap (#1477), daemon socket TOCTOU hardening (#1478). No schema bump.
- **v1.37.0** (2026-05-05) — v1.36.2 audit close-out (#1456): 120/163 findings addressed. Dim-scaled batch sizes (#1464). Promoted `cqs::limits` to `pub`. `RerankerMode::Llm` removed.
- **v1.36.2** (2026-05-04) — critical fix (#1451): long-running `cqs index` no longer crashes with SQLITE_BUSY when concurrent `cqs` invocations overlap. busy_timeout 5s → 30s.
- **v1.36.1** (2026-05-04) — qwen3-embedding-4b preset (#1441/#1442) — 7.4 GB FP16, 2560-dim, 4096 max-seq.
- **v1.36.0** (2026-05-03) — schema v25→v26. Per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge. Net agg lift R@5 +3.7pp. 13 audit-followup fixes including critical readonly-migration bug (#1413).
- **v1.35.0** (2026-05-02) — default embedder swap BGE-large → EmbeddingGemma-300m + tokenizer-truncation correctness fix (#1384) for fine-tuned BERT-family presets.
- **v1.34.0** (2026-05-02) — post-v1.33.0 audit close-out (24 fix PRs, 129 findings) + EmbeddingGemma preset.
- **v1.33.0** (2026-05-02) — eval-matcher drift fix (#1284), placeholder-cache 30s startup tax fix (#1288, CI 38min→6min), `bge-large-ft` LoRA preset.

## Schema state

- **v27** (post-#1497, v1.38.0+) — `chunks.needs_embedding INTEGER NOT NULL DEFAULT 0` plus partial index. Drives `--llm-summaries` skip-first-pass embed: chunks land with zero-vec sentinel + `needs_embedding=1`; HNSW build and search hide them until `enrichment_pass` clears the flag.
- v27 migration backfills `needs_embedding=1` for any pre-v27 row with `embedding_base IS NULL` so legacy chunks repopulate the base-HNSW on the next index pass.
- HNSW build, `Store::search_by_name`, `Store::search_fts_only` all filter `WHERE needs_embedding = 0`.

## Adding a top-level CLI command (post-#1495)

Declare the variant with `#[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]` on `Commands` (definitions.rs), implement the handler in `commands/<area>/`, add a small `cmd_<snake>_dispatch` shim in `commands/dispatch_shims.rs`. The shim destructures the variant out of `&Commands` and forwards to the handler. Cfg-gated variants get `#[cfg(feature = "...")]` next to `#[cqs_cmd(...)]` and the derive forwards it to every emitted arm.

## Operational pitfalls (rolling forward)

- **Main is protected** — `git push` to main is rejected. Always create a branch + PR.
- **Always use `--body-file` for `gh pr create`** — never inline heredocs (PowerShell mangles + Claude Code captures the whole multiline as a permission entry, corrupting `settings.local.json`).
- **WSL git credential helper** — `git push` from `~/training-data` needs `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Already configured globally for cqs.
- **Squash-merge + rebase trap** — when a PR is squash-merged and a follow-up branch was based off it, rebase fails. Cherry-pick onto fresh main.
- **Auto-merge disabled** — `gh pr merge --auto` returns "auto merge is not allowed". Watch CI manually + merge when green.
- **`cargo publish --features gpu-index` fails verification** — the workspace `[patch.crates-io]` cuvs-patched fork doesn't ship in the package. Use plain `cargo publish` (no features); gpu-index is feature-gated.
- **cqs-macros must publish first** — when bumping cqs that depends on cqs-macros, publish cqs-macros to crates.io first or `cargo publish -p cqs` errors with "no matching package named cqs-macros".
- **`cargo publish` 413 errors** = excludes missing. `evals/` etc. are in `Cargo.toml`'s `exclude` list.
- **`enumerate_files` returns relative paths** — joining with project root before `parse_file()` is mandatory; otherwise the parser resolves against cargo's CWD.
- **`type_edges` parser tracks signature-level uses only** — params, returns, fields. Not expression-level (`let x = T::new()`). Test assertions on "who uses type T?" must check signature users.
- **Daemon GPU "activity" is misleading** — ORT keeps the CUDA context warm; A6000 sits at P2/1800MHz/84W with 0 actual compute work. True idle (P8) requires stopping the daemon.
- **CI cqs test job runs ~6-12 min** post-#1288/#1302 (was 38 min). Fixed-interval `/loop` heartbeats > 60min should go to cloud schedule (`/schedule`).
- **HF preset tokenizers may ship `truncation: {max_length: 512}` baked into `tokenizer.json`** — affects bge-large-ft, v9-200k, coderank. Cqs windowing/counting must clone-and-disable truncation before counting tokens. See PR #1384. When adding a new preset, check `python -c "from tokenizers import Tokenizer; print(Tokenizer.from_file('tokenizer.json').get_truncation())"` first.
- **Triage-flip durability** (audit-cycle lesson): force-pushed rebases naively resolve triage-row conflicts using older agents' pre-flip snapshots. Mitigation: keep triage flips append-only OR move each PR's triage update into a separate narrow PR per cluster.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines

Canonical slate: `evals/queries/v3_test.v2.json` (109q) + `evals/queries/v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109).

**Baseline (v3.v2 218q dual-judge, post-v1.39.0 default — embeddinggemma-300m + per-cat α + Unknown=0.80):**

| Metric | Test | Dev |
|---|---:|---:|
| R@1 | 44.0% | 55.0% |
| R@5 | 67.9% / 69.7% | 78.0% / 80.7% |
| R@20 | 80.7% / 84.4% | 91.7% / 92.7% |

The R@5/R@20 ranges reflect the natural variance from one query rank-shifting at the boundary; both numbers comfortably above their canonical baselines. v4 fixtures (1526/split, 14× v3 N) exist for any A/B that needs tighter noise floors.

**Strategic frontier candidates** (when redirected): wire USearch / SIMD brute-force as `IndexBackend` candidates (#1131 trait scaffolding already in); HyDE on v3 dev with index-time per-category routing (never properly tested at v3 N); knowledge-augmented retrieval (call/type graph as structured filter; multi_step queries weakest at 28-43% R@1); expand v3 → v4 fixture scale (1526q/split — current 109q is data-bound for per-category sweeps).

**Reranker V2 closed** — 2026-05-07 re-eval against post-v1.39.0 stack confirmed all four reranker variants (off-the-shelf MiniLM + 3 in-domain UniXcoder retrains) remain net-negative on v3.v2 (test R@5 -10 to -16pp, dev R@5 -16 to -26pp). Gap actually widened on dev as stage-1 strengthened. R@20 within 1-4pp of baseline across all four — gold is in the pool; every reranker demotes it. Bottleneck is fixture-size (109q × 30 candidates too thin for 125M cross-encoder) + stage-1 already strong; not a tunable knob. Future revisit gated on v4-scale labelled fixture OR a 5× bigger base (bge-reranker-large at ~3× latency). README now documents the regression at v1.39.0.
