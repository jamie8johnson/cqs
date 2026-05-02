# Project Continuity

## Right Now

**v1.33.0 released 2026-05-02.** Tag `v1.33.0` pushed (triggers `release.yml` for binary artifacts); `cqs 1.33.0` published to crates.io after token rotation (the prior token returned 403 — root cause not fully diagnosed; new token generated with `publish-update` scope). No schema bump.

Five themes (full detail in CHANGELOG.md):

1. **Eval matcher drift fix** (#1284, big). Strict `(file, name, line_start)` was eating ~38% of gold chunks as misses after audit-driven line-shifts. Loosened to `(file, name)`. Today's BGE-base v3.v2 numbers under corrected matcher: R@1=44.5% / R@5=73.4% / R@20=84.9% (218 queries aggregate). The v9-200k "retired" verdict was 95% fixture-side artifact and the model is back as opt-in.
2. **Placeholder-cache 30-second startup tax fix** (#1288, big). Eager `LazyLock<Vec<String>>` build of 32,466 SQL placeholder strings was ~30s on first DB write per process. Lazy per-`OnceLock<String>`. **CI test job ~38min → ~6min.**
3. **Chunk orphan pipeline prune** (#1283). `cqs index --force` now cleans up old-format chunks left behind by chunker-version bumps. ~2% accumulated orphan rates clear automatically on next reindex.
4. **`bge-large-ft` embedder preset** (#1289). LoRA fine-tune of BGE-large; **best test R@5 (73.4%) of any model in the 5-way A/B**, trades dev R@5 by 6.5pp. Opt-in (`CQS_EMBEDDING_MODEL=bge-large-ft`); default stays at BGE-base for the dev R@5 hedge.
5. **Daemon test refactor + nightly CI workflow** (#1292, #1286 Phase 1). Thread-local override replaces unsafe `set_var` (eliminates the libc env-mutex deadlock that hung CI for hours); `.github/workflows/ci-slow.yml` runs `cargo test --include-ignored` on a daily cron with auto-issue-on-failure.

### Post-v1.32.0 sweep — folded into v1.33.0 (2026-05-02)

| PR | Closes | Theme |
|---|---|---|
| #1268 | — | Fix flaky `llm::validation::tests::from_env_strict` race (ENV_LOCK hoisted). Restored main green. |
| #1269 | — | `cqs notes list --kind <kind>` filter (#1133 follow-up). |
| #1270 | #1176 | SPLADE phase 2 negative-result A/B writeup → `research/models.md`. RRF dense+sparse trails linear-α by ~1pp R@5/R@20 on test, ~4-5pp R@1/R@5 on dev. Linear-α stays. |
| #1271 | #1230 | `process_file_changes` zero-files no-op test. |
| #1272 | #1217 | `write_slot_model` round-trips through `SlotConfigFile` with flatten extras — preserves unrelated TOML sections. |
| #1273 | #1215 | `daemon_request<T>` helper extraction; `daemon_ping/status/reconcile` collapse to thin shims. -110 net lines. |
| #1275 | #1218 | `AuthChannel` trait + 3 impls (`Bearer`/`Cookie<'a>`/`QueryParam`). `check_request` collapses to a registry walk. |
| #1276 | #1220 | `Reranker` trait + `OnnxReranker` (renamed) / `NoopReranker` / `LlmReranker` (skeleton). Holders cache `Arc<dyn Reranker>`. |
| #1278 | #1133 | `cqs notes update --new-kind <kind>` flag — closes the kind taxonomy lifecycle. |
| #1279 | #1226 | `run_daemon_reconcile_with_walk(...)` accepts pre-computed disk walk; idle-tick dispatcher does one walk when both gc + reconcile fire. |
| #1274 / #1277 | — | Continuity updates. |

**Eight issues closed by this sweep:** #1133 (already), #1176, #1217, #1218, #1220, #1226, #1230, #1215.

**State:** main at `766115af` (post-#1308). Local in sync. Daemon was rebuilt + restarted post-#1303 (carries the eval `--reranker` flag); needs another rebuild + restart to pick up #1308's test-only soft-skip changes (no behavior change for production code).

**Conda/pip cleanup pass (2026-05-01):** safe-tier upgrades across base, cqs-train, onnx-export, vllm-serve. Bumped: `anthropic` 0.86→0.97 (used by SQ-6), `onnxruntime` 1.24.4→1.25.1, `onnx` 1.20→1.21, `sentence-transformers` 5.1→5.4, `peft` 0.18→0.19, `datasets` 4.8.4→4.8.5, `pip` 26.0.1→26.1, plus ~15 utility minors per env. Held: `transformers` 4→5, `protobuf` 6→7, `cudnn` 9.19→9.21, `cuda-version` 13.1→13.2, `vllm` 0.19→0.20, `torch` 2.9→2.11, all `nvidia-*` (torch-pinned). Rolled back: `mpmath` (sympy cap), `setuptools` (torch cap), `fsspec` (datasets cap), `flashinfer-cubin/python`/`lark` (vllm exact pins). Pre-existing latent conflicts surfaced (not caused by upgrades): `pylate` pins `st==5.1.1`/`ujson==5.10.0` in base; `optimum-onnx` pins `transformers<4.58` in cqs-train; `vllm 0.19` pins `transformers<5` but env has 5.6.0.dev0; `coir-eval` missing `faiss-cpu`.

### Post-v1.33.0 follow-up sweep (2026-05-02)

| PR | Closes | Theme |
|---|---|---|
| #1300 | — | ROADMAP cleanup (header v1.32.0→v1.33.0; folded "Post-v1.32.0 sweep" into v1.33.0 row). |
| #1301 | — | EmbeddingGemma-300m preset (`PoolingStrategy::Identity` for projection-head pooling, `CQS_DISABLE_TENSORRT` knob). Stash from earlier session shipped. |
| #1302 | — | #1286 Phase 2 — gate `onboard_test` (~6.6 min) + `eval_subcommand_test` (~5.3 min) behind `slow-tests`; new `slow-tests-feature` job in `ci-slow.yml`. ~12 min off PR-time CI. |
| #1303 | — | `cqs eval --reranker <none\|onnx\|llm>` — wires #1276's Reranker trait into the eval harness. Default `none` preserves baselines. |
| HF dataset README | #1290 | Fixed HF viewer CastError (sidecar `processing_manifest.jsonl` was being ingested as data; added `configs` block scoping train split to the dataset file only). |
| #1306 | — | Disabled ci-slow.yml schedule cron — the first manual run failed; daily cron would auto-file an issue every 06:00 UTC. workflow_dispatch still wired up. |
| #1307 | half of #1305 | `cli_doctor_fix_test` was checking legacy `.cqs/index.db` path; PR #1105 (per-project slots) moved it to `.cqs/slots/default/index.db`. Fixed by switching to `cqs::resolve_index_db(&cqs_dir)`. Tests pass locally and on CI. |
| #1308 | other half of #1305 | 9 model-loading tests (8× SPLADE + 1× embedder, plus `tests/embedding_test.rs` + `tests/model_eval.rs` + `tests/eval_test.rs` + reranker integration test) panic'd on the GitHub-hosted runner because anonymous HF downloads return error pages. All `expect()` on model-load now `match` to soft-skip with a one-line diagnostic. Tests still load + run locally where models are cached. |

**Issues closed in sweep:** #1290 (HF dataset viewer); #1286 Phase 2 fully addressed (Phase 3 deferred per user direction); #1305 fully addressed via #1307 + #1308.

**Up next (no active task — user-direct):**
- Manual `gh workflow run ci-slow.yml -f include_ignored=true` is in flight (run `25247620924`); if it goes green, **re-enable the ci-slow.yml schedule cron** by uncommenting the `schedule:` block (single revert of #1306).
- v1.32.0 audit eligible (16-category audit).
- Embedder eval queue: ceiling probes Qwen3-Embedding-8B + NV-Embed-v2.
- `LlmReranker` production wiring against `BatchProvider` (skeleton in `src/reranker.rs`; eval flag now consumes it from `cqs::LlmReranker::new()` so the wire-up is the only missing piece).

### Outstanding follow-ups (small, optional)

- Retroactive vendored / kind tagging for pre-v25 rows — operator can `cqs index --force` if they want immediate flagging.
- `cuvs` crate update — upstream PRs #1840 (serialize/deserialize) + #2019 (search_with_filter) both merged into rapidsai/cuvs; `[patch.crates-io]` entry on `jamie8johnson/cuvs-patched` becomes redundant once a new cuvs crate publishes (RAPIDS ~2-month cadence).

## Open issues (12 total)

All P1/P2 closed. Refactor frontier (#1215, #1217, #1218, #1220, #1226) cleared. Remaining are tier-3 or have specific blockers.

| # | Title | Why open |
|---|---|---|
| 106 | ort dependency is pre-release RC | Blocks on upstream pykeio cutting a stable 2.0 |
| 255 | Pre-built reference packages | Signing/registry design (infra, not code) |
| 717 | perf: HNSW fully loaded into RAM | Needs lib swap to hnswlib-rs (nightly-only) |
| 916 | perf: mmap SPLADE index | Audit-deprioritized — 59 MB peak transient, dominated by parse-side allocations |
| 1043 | `is_slow_mmap_fs` ignores Windows network drives | Linux/WSL unaffected; needs Windows test runner |
| 1139 | EX-V1.30-3: structural_matchers shared library | Touches 50+ language modules; explicitly skipped per autopilot directive |
| 1140 | EX-V1.30-4: Embedder preset extras map | Skipped per autopilot directive |
| 1216 | EX-V1.30.1-2: BatchCmd dispatch macro table | 33-handler refactor; current dispatch already exhaustive |
| 1228 | RM-2: wait_for_fresh persistent connection | Daemon side reads one request per connection — option (a) is bigger than the issue's "30-line" estimate |
| 1229 | RM-5: stream enumerate_files walk | Real win at 1M-file repos; needs `enumerate_files_iter` API + batched SQL lookup |
| 1244 | RM-4: HNSW snapshot 17 MB | Audit's "240×" claim assumed nonexistent u32 chunk_ids; actual win ~1 MB via `[u8; 32]` repr |
| 1286 | Overnight CI workflow Phase 3 | Phase 1 (#1293) + Phase 2 (#1302) shipped; Phase 3 (CLI subprocess test binary collapse) lower priority after #1288's PR-time CI win |

## Parked

Strategic frontier candidates if redirected:

- **#1131 follow-on** — wire USearch / SIMD brute-force as `IndexBackend` candidates (trait scaffolding from #1131 already in).
- **EmbeddingGemma-300m, Qwen3-Embedding-8B, NV-Embed-v2** — embedder eval queue, all eval-required.
- **HyDE on v3 dev** — most promising untested representation lever. Per-category routing required. Killed at v1.28.3 attempt; index-time variant never properly retested at v3 N.
- **Reranker V2 properly retrained** — Phase 3 attempt failed (-24pp R@5 full pool). Three fixes in post-mortem (TIE labels, domain-shifted hard negatives, pool cap), ~1-2 weeks work. Re-attempt only with 10× more queries OR bge-reranker-large.
- **Knowledge-augmented retrieval** — call/type graph as structured filter. multi_step queries weakest at 28-43% R@1.

## Operational pitfalls (rolling forward)

- **Main is protected** — `git push` to main is rejected. Always create a branch + PR. `git push origin main` wastes a round trip.
- **Always use `--body-file` for `gh pr create`** — never inline heredocs (PowerShell mangles + Claude Code captures the whole multiline as a permission entry, corrupting `settings.local.json`).
- **WSL git credential helper** — `git push` from `~/training-data` needs `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Already configured globally for cqs.
- **Squash-merge + rebase trap** — when a PR is squash-merged and a follow-up branch was based off it, rebase fails (commits ≠ squash). Cherry-pick the follow-up's commits onto a fresh branch from main.
- **Auto-merge disabled on this repo** — `gh pr merge --auto` returns "auto merge is not allowed". Watch CI manually + merge when green.
- **Cargo publish 413** = "exclude" list missing. `evals/` etc. now in `Cargo.toml` exclude list.
- **Always confirm test wins on dev before declaring** — single-split A/B is noisy at N=109. ColBERT 2-stage taught this.
- **Smoke-test against real producer output** — synthetic fixtures only catch what you anticipate.
- **No time estimates in specs** — wall-time predictions are unreliable. Use compute units / step counts / size anchors instead.
- **`enumerate_files` returns relative paths** — joining with project root before `parse_file()` is mandatory; otherwise the parser resolves against cargo's CWD and parses the wrong tree.
- **`type_edges` parser tracks signature-level uses only** — params, returns, fields. Not expression-level (`let x = T::new()`). Test assertions on "who uses type T?" must check signature users.
- **Daemon GPU "activity" is misleading** — ORT keeps the CUDA context warm; A6000 sits at P2/1800MHz/84W with 0 actual compute work. True idle (P8) requires stopping the daemon.
- **CI cqs test job runs ~30-40 min** serialised on a single GPU runner. Fixed-interval `/loop` heartbeats > 60min should go to cloud schedule (`/schedule`).
- **vllm 0.19 has tight pins** on `flashinfer==0.6.6` and `lark==1.2.2`. Bumping these without bumping vllm itself breaks the Gemma server. The vllm-serve env runs a `transformers 5.6.0.dev0` build that vllm theoretically rejects — tolerated at runtime, fragile if vllm ever bumps.
- **`pylate 1.4.0` pins `sentence-transformers==5.1.1` exactly** with no newer pylate available. Conflict is dormant unless ColBERT eval is run; cleanest fix would be a dedicated `colbert-eval` env.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines (for regression comparison)

`v3_test.v2.json` (109q) and `v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109) — gold chunks re-pinned to current line numbers to absorb v1.29.x audit drift.

| Config | test R@1 | test R@5 | test R@20 | dev R@1 | dev R@5 | dev R@20 |
|---|---|---|---|---|---|---|
| canonical (post-v1.28.3, 2026-04-20) | 41.3% | 68.8% | 85.3% | 45.0% | 78.0% | 88.1% |
| **current (refreshed fixture, BGE-large)** | 36.7% | **63.3%** | **80.7%** | 42.2% | **74.3%** | **87.2%** |
| current (CodeRankEmbed, opt-in via #1110) | 37.6% | **67.0%** | 78.9% | 45.0% | 69.7% | 79.8% |
| current (v9-200k, retired) | 22.9% | 38.5% | 47.7% | 20.2% | 40.4% | 52.3% |

The 3.7-5.5pp gap between canonical and refreshed-current is real corpus-drift attrition (5,413 new chunks since 2026-04-20, ~30% of corpus). Not a search regression. The v3.v2 fixture is the canonical eval slate; v4 fixtures (1526/split, 14× v3 N) exist for any future A/B that needs tighter noise floors. Long-term inoculation against fixture drift would be relaxing eval gold-match to `(file, name, chunk_type)` only — out of scope for this round.

The `research/models.md` file (committed in #1270) is the inaugural retrieval-research log. Future A/B writeups append there.
