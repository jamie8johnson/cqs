# Project Continuity

## Right Now

**v1.29.0 shipped 2026-04-23 (PR #1092 squashed at 22:57 UTC).** crates.io published, GitHub Release workflow building binaries (~13 min), local binary at 1.29.0. No active PRs.

### Open PRs

None.

### v1.29.0 contents

Feature release bundling three arcs (23 commits since v1.28.3):
- **`cqs serve`** with 4 views (2D / 3D / hierarchy / embedding cluster) + perf pass (~60s → ~3-4s first paint). Schema v22 (umap_x/umap_y); opt-in via `cqs index --umap`.
- **`.cqsignore`** mechanism for cqs-only exclusions.
- **Slow-tests cron killed**: 5 of 16 subprocess CLI test binaries converted to in-process; `slow-tests.yml` workflow deleted.
- 2 Dependabot security bumps (openssl 0.10.78, rand 0.8.6).

Spec docs:
- `docs/plans/2026-04-22-cqs-serve-3d-progressive.md`
- `docs/plans/2026-04-22-cqs-serve-perf.md`
- `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`

### Pre-flight cleanups that landed alongside the release

Things clippy 1.95 caught when running with `-D warnings`:
- **Restored `slow-tests` Cargo feature gate** — 11 subprocess test files still reference `#![cfg(feature = "slow-tests")]` (not in original conversion scope): `cli_blame_test`, `cli_brief_test`, `cli_chat_completer_test`, `cli_chat_format_test`, `cli_doctor_fix_test`, `cli_drift_diff_test`, `cli_envelope_test`, `cli_neighbors_test`, `cli_reconstruct_test`, `cli_review_test`, `cli_train_review_test`. Feature kept alive (gate without an executor — the cron is gone). Convert opportunistically when re-touching their code.
- **`tests/common/mod.rs`** — file-level `#![allow(dead_code, clippy::type_complexity)]` since harness items used by different test-binary subsets.
- **`src/search/scoring/candidate.rs`** — `SearchFilter::default()` + field reassignment → struct literal.

### Just-shipped arcs (2026-04-22 → 2026-04-23)

**`cqs serve` 3D progressive rollout (#1077, #1078, #1079, #1081):**
- Step 1: renderer abstraction + 2D/3D toggle (Three.js + 3d-force-graph, lazy-loaded)
- Step 2: hierarchy view (Y axis = BFS depth from selected root)
- Step 3: embedding cluster view (X/Z = UMAP, Y = caller count) — adds `cqs index --umap` flag, schema v22 (umap_x/umap_y columns), Python `scripts/run_umap.py` embedded in binary
- Perf pass: SQL-side `max_nodes` cap, default 300 (was 1500), `cose` layout (was dagre), gzip middleware, lazy 3D bundle. **First paint ~60s → ~3-4s on cqs corpus.**
- Spec: `docs/plans/2026-04-22-cqs-serve-3d-progressive.md` + `2026-04-22-cqs-serve-perf.md`

**`.cqsignore` mechanism (#1080):** Layered on top of `.gitignore`. Excludes vendor minified JS + eval JSON fixtures. Index 18,954 → 15,488 chunks. Zero "Dropped oversized" parser warnings.

**Slow-tests elimination (#1082-#1088, full sweep):** All 5 subprocess CLI test binaries (cli_health, cli_test, cli_graph, cli_commands, cli_batch — 113 tests, ~130 min nightly cron) converted to in-process `InProcessFixture`-based tests (60 tests across 4 binaries) + 15-test `cli_surface_test.rs` for things that genuinely need a binary. **`slow-tests.yml` workflow deleted, `slow-tests` Cargo feature removed.** Now ~2 min added to every PR instead of ~130 min nightly. Spec: `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`. Issue #980 closed.

**Dependabot security alerts: 12 → 0 open.**
- #1086: openssl 0.10.75 → 0.10.78 (medium, several CVE-adjacent fixes) — merged
- #1089: rand 0.8.5 → 0.8.6 (low, custom-logger soundness) — open, awaiting CI

### Newly-filed issues (2026-04-23)

- **#1090** — `cqs watch` does a full HNSW rebuild on every file change (~15-30s of CUDA churn per save). `hnsw_rs` doesn't support incremental insert into a loaded index. Four candidate fixes ranked.
- **#1091** — WSL `cqs watch` poll-watcher walks entire tree at 1500ms intervals over the 9P bridge → 8% sustained CPU. Easy win: configurable `CQS_WATCH_POLL_MS` with longer default.

### Architecture state

- **Version:** v1.29.0 (crates.io published 2026-04-23 22:58 UTC; GitHub Release workflow #24863038721 building binaries; tag `v1.29.0` at commit `21b5f6e6`)
- **Local binary:** `~/.cargo/bin/cqs` at 1.29.0; daemon restarted post-install
- **Index:** 15,488 chunks across 598 files, **schema v22** (umap_x/umap_y columns, opt-in via `cqs index --umap`)
- **Tests:** ~2963 pass + 51 ignored locally without `gpu-index` (matches CI invocation). Library + integration suite all in regular CI in ~2 min added (was ~130 min nightly cron)
- **Production R@5 on v3.v2 test:** 68.8% (from v1.28.3 baseline; no retrieval changes in v1.29.0)
- **cqs-watch daemon:** running v1.29.0 release binary; CUDA context warm in P2 (~2-3 GB VRAM, expected idle floor with model resident)
- **`cqs serve`:** 4 views available — `2d` / `3d` / `hierarchy` / `cluster`. Run `cqs index --umap` first to populate the cluster view's coords.

### Roadmap parked (highest-value)

- **`nomic-ai/CodeRankEmbed` + `nomic-ai/nomic-embed-code` A/B** — open-weight code-specialized embedders. CodeRankEmbed is 137M (smaller than BGE-large), MIT, 768-dim, 8192-token context, asymmetric prefix. nomic-embed-code is 7B (Apache, Qwen2.5-Coder-based), GGUF quantizations available. ~2-hour A/B against v9-200k on the v3 fixture would tell us if CodeRankEmbed earns the default slot.

### Local cleanup pending

None. Working tree clean post-release.

### Roadmap follow-ups added during the release

- **11 slow-test-binary stragglers** (cli_blame, cli_brief, cli_chat_completer, cli_chat_format, cli_doctor_fix, cli_drift_diff, cli_envelope, cli_neighbors, cli_reconstruct, cli_review, cli_train_review) still gated by `slow-tests` feature with no executor (cron is dead). Convert opportunistically when re-touching the code paths they cover. Pattern documented in `docs/plans/2026-04-22-cqs-slow-tests-elimination.md`.
- **Issue #1090** — HNSW full rebuild on every save (15-30s CUDA churn per file change in `cqs watch`)
- **Issue #1091** — WSL poll-watcher 8% sustained CPU (1500ms interval, walks entire tree over 9P)

## What's parked

- **HyDE on v3 dev** — most promising untested representation lever. Per-category routing required. Killed at v1.28.3 attempt.
- **Reranker V2 properly retrained** — Phase 3 attempt failed (-24pp R@5 full pool). Three fixes in post-mortem (TIE labels, domain-shifted hard negatives, pool cap), ~1-2 weeks work. Re-attempt only with 10x more queries OR bge-reranker-large.
- **ColBERT integration with per-token index** — eval tool exists, default off; full integration multi-week.
- **Knowledge-augmented retrieval** — call/type graph as structured filter. Multi_step queries weakest at 28-43% R@1.
- **Code-aware embedder switch (older candidates)** — CodeBERT, CodeT5+-110M, UniXcoder all untested on v3. v9-200k didn't help. CodeRankEmbed (above) is the better bet now.

## Operational pitfalls (rolling forward)

- **WSL git credential helper** — out-of-the-box, `git push` from `~/training-data` fails with "could not read Username." Fix: `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Already configured globally.
- **Squash-merge + rebase trap** — when a PR is squash-merged and a follow-up branch was based off it, rebase fails because individual commits ≠ squash. Fix: cherry-pick the follow-up's commits onto a fresh branch from main. Hit this 4 times during the cqs serve arc.
- **Auto-merge disabled on this repo** — `gh pr merge --auto` returns "auto merge is not allowed". Watch CI manually + merge when green.
- **`gh pr create` requires `--head` + `--base`** when branch name on local differs from origin (rebased branches).
- **Always use `--body-file` for PR/issue bodies** — never inline heredocs (PowerShell mangles + Claude Code captures whole multiline as a permission entry).
- **Cargo publish 413 = "exclude" list missing** — `evals/` etc. now in `Cargo.toml` exclude list.
- **Always confirm test wins on dev before declaring** — single-split A/B is noisy at N=109. ColBERT 2-stage taught this.
- **Smoke-test against real producer output** — synthetic fixtures only catch what you anticipate.
- **No time estimates in specs** — wall-time predictions are unreliable. Use compute units / step counts / size anchors instead.
- **`enumerate_files` returns relative paths** — joining with project root before `parse_file()` is mandatory; otherwise the parser resolves against cargo's CWD and parses the wrong tree. Caught during InProcessFixture phase 1.
- **`type_edges` parser tracks signature-level uses only** — params, returns, fields. Not expression-level (`let x = T::new()`). Test assertions on "who uses type T?" must check signature users.
- **Audit-mode tests must `mkdir .cqs/` first** — `TestStore::new()` puts `index.db` in the tempdir root, not in a `.cqs/` subdirectory; `cqs::audit::save_audit_state` writes to `cqs_dir.join(...)` which 404s without the dir.
- **Daemon GPU "activity" is misleading** — ORT keeps the CUDA context warm; A6000 sits at P2/1800MHz/84W with 0 actual compute work. True idle (P8) requires stopping the daemon.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines (for regression comparison)

`v3_test.v2.json` (109q) and `v3_dev.v2.json` (109q):

| Config | test R@1 | test R@5 | test R@20 | dev R@1 | dev R@5 | dev R@20 |
|---|---|---|---|---|---|---|
| **current (post-v1.28.3, 2026-04-20)** | 41.3% | **68.8%** | **85.3%** | 45.0% | **78.0%** | **88.1%** |
| canonical pre-v1.28.0 | 41.3% | 63.3% | 80.7% | 41.3% | 74.3% | 86.2% |
| Δ | 0.0 | **+5.5** | **+4.6** | **+3.7** | **+3.7** | **+1.9** |

The v3.v2 fixture is the canonical eval slate. v4 fixtures (1526/split, 14× v3 N) exist for any future A/B that needs tighter noise floors.

## Open issues (13 open)

| # | Title | Tier |
|---|---|---|
| 1091 | WSL poll-watcher 8% CPU | performance |
| 1090 | HNSW rebuild every save (15-30s CUDA) | performance |
| 1049 | Pin fallback_does_not_mix_comment_styles test | testing, tier-3 |
| 1048 | try_daemon_query strict-string parsing | enhancement, tier-3 |
| 1047 | ChunkType::human_name catch-all hides variants | enhancement, tier-3 |
| 1044 | Windows cqs watch can't stop cleanly | bug, data-integrity, tier-3 |
| 1043 | is_slow_mmap_fs ignores Windows network drives | performance, tier-3 |
| 1042 | WINDOW_OVERHEAD doesn't scale with prefix length | enhancement, tier-3 |
| 956 | ExecutionProvider — decouple gpu-index from CUDA | refactor, tier-2 |
| 916 | mmap SPLADE index | tier-2 |
| 717 | HNSW fully in RAM, no mmap | tier-3 |
| 255 | Pre-built reference packages | enhancement, tier-3 |
| 106 | ort dependency is pre-release RC | tier-3 |
