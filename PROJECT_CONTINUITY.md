# Project Continuity

## Right Now

**v1.35.0 released 2026-05-02.** **Default embedder swaps from BGE-large to EmbeddingGemma-300m.** PR #1385 merged (3 follow-up fixes during CI iteration: doctor test default-derivation, embedder ONNX external-data sidecar download, embedder_dim_mismatch test default-derivation). Tag pushed; binaries via `release.yml`; crates.io publish in flight.

5-slot apples-to-apples eval after every slot was reindexed `--force --llm-summaries` on the truncation-fixed binary (#1384):

| Slot | Agg R@1 | Agg R@5 | Agg R@20 |
|---|---:|---:|---:|
| **embeddinggemma-300m (new default)** | **49.1%** | 72.5% | **86.2%** |
| bge-large-ft | 47.7% | **73.4%** | **86.2%** |
| BGE-large (former default) | 47.2% | 72.0% | 84.4% |
| v9-200k | 45.0% | 68.8% | 80.7% |
| nomic-coderank | 45.0% | 67.9% | 78.9% |

Existing slot indexes keep their stored model — only fresh slots / fresh `cqs index` runs pick up the new default. BGE-large remains a first-class preset (`CQS_EMBEDDING_MODEL=bge-large`).

**Side fix (#1385): ONNX external-data sidecar.** EmbeddingGemma's >2GB FP32 weights live in `model.onnx_data` next to `model.onnx`. The downloader was only fetching the graph file, not the weights file. Without the sidecar, ORT init fails on a fresh runner with `cannot get file size: No such file or directory [.../onnx/model.onnx_data]`. Fix: after the existing model.onnx + tokenizer.json fetches, also try `<onnx_path>_data`; ignore 404 since most presets don't have one.

**v1.34.0 released 2026-05-02 (same day as v1.33.0).** Bundled the post-v1.33.0 audit close-out (24 fix PRs) + pre-audit feature work (EmbeddingGemma-300m preset, `cqs eval --reranker`, slow-tests Phase 2, ci-slow.yml stabilization). On crates.io. Tag pushed (binaries via `release.yml`).

**Open in flight: PR #1384 — `fix(embedder): bypass tokenizer truncation in windowing/count paths`.** Apples-to-apples eval comparison surfaced a real correctness bug. **bge-large-ft and v9-200k tokenizers ship `truncation: {max_length: 512}` baked into `tokenizer.json`** (HF's `optimum-cli` default). cqs `split_into_windows`/`token_count` rely on `encode().get_ids().len()` to count tokens — the silent truncation cap meant long markdown sections were chunked at "fits in 1-2 windows" when they actually needed 12, so ~90% of section content was never embedded. Surgical fix: clone the tokenizer Arc and disable truncation for counting paths only (inference paths still need the cap to clamp at max_seq).

Reproducer on `docs/audit-findings-v1.30.0.md` (15.5k chars, 5358 real tokens):
- BGE-large default tokenizer (no truncation): 5358 tokens → 12 windows
- bge-large-ft tokenizer (truncation=512): 512 tokens reported → 2 windows

**Pending after #1384 merges:** rebuild binary, install, restart daemon, re-reindex bge-ft + v9 (their indexes are missing real content), re-eval all 4 slots for the clean apples-to-apples comparison. The earlier "matching coverage" eval is contaminated — bge-ft and v9 numbers were measured against indexes missing ~90% of long-section content.

**v1.33.0 audit close-out (earlier in this session, all merged):** 16-category audit produced 167 findings; triaged P1=47/P2=41/P3=56/P4=23. **24 fix PRs landed**; 25 medium-effort items filed as tracking issues (#1337-#1377). Coverage: 129 ✅ closed / 15 🎫 issue-tracked / 0 ⬜ open. PR #1334 (daemon-aware `cqs index`) closed 98% of telemetry CLI error rate. PR #1380 recovered 112 lost ✅ flips after a cascade-rebase pattern silently rolled back triage updates.

**Eval results pre-tokenizer-fix (apples-to-apples, all 4 slots reindexed --force --llm-summaries):**

| Preset | Agg R@1 | Agg R@5 | Agg R@20 | Note |
|--------|---:|---:|---:|------|
| BGE-large (default) | 47.7% | 71.6% | 83.9% | clean — full-coverage tokenizer |
| **embeddinggemma-300m** | **49.1%** | **72.5%** | **86.2%** | clean — full-coverage tokenizer |
| bge-large-ft | 45.4% | 72.9% | 86.2% | **CONTAMINATED** — truncation bug |
| v9-200k | 44.5% | 69.7% | 81.7% | **CONTAMINATED** — truncation bug |

Take only the BGE-large vs Gemma comparison as valid: **embeddinggemma-300m wins agg R@1 by 1.4pp, R@5 by 0.9pp, R@20 by 2.3pp**. v1.35.0 default-candidate at 308M params / 768 dim. The bge-ft and v9 numbers will likely jump after the fix lands and they're reindexed; re-run before any roadmap conclusion.

**Coverage now:**
- ✅ 129 closed (all P1, all in-scope P2/P3 batches, plus 17 new tests)
- 🎫 15 issue-tracked (P4 batches: #1337-#1359; deferred: #1365, #1366, #1369, #1370-#1377)
- ⬜ 0 open

**One surprising operational lesson:** PR #1380 was needed to **recover 112 lost ✅ flips** in `docs/audit-triage.md`. The cascade of force-pushed rebases — when multiple branches conflict on the same triage rows — caused older agents' pre-flip snapshots to silently override newer merged ✅s when conflicts were resolved naively. Source-code fixes were unaffected; only the bookkeeping rolled back. **Mitigation for future audits: keep triage flips append-only OR move each PR's triage update into a separate narrow PR per cluster.** See `feedback_agent_worktrees.md` and the new note added below for the full pattern.

**Eval refresh (PR #1381 in flight, closes #1369):** all 5 materialized presets re-run against v3.v2 218q dual-judge:

| Preset | Agg R@1 | Agg R@5 | Agg R@20 |
|--------|---:|---:|---:|
| BGE-large (default) | 46.8% | **73.9%** | **85.3%** |
| embeddinggemma-300m | **48.2%** | 73.4% | 85.3% |
| bge-large-ft | 45.9% | 72.0% | 83.0% |
| v9-200k | 44.0% | 69.3% | 81.7% |
| nomic-coderank | 45.0% | 68.8% | 80.7% |

The "R@5 regression discovered 2026-05-01" noted in MEMORY.md was *not* a regression — BGE-large agg R@5 is 73.9% now vs 73.4% in the original TL;DR claim. The drop was a stale measurement, not a code change. **embeddinggemma-300m beats BGE-large on agg R@1 by 1.4pp** — that's the v1.34.0 angle if anyone wants to ship it as a default candidate. The 296-query "Fixture eval" table was dropped from the README (the fixture itself isn't in the repo, can't be regenerated).

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

## Open issues (~37 total — 12 long-running + 25 new from v1.33.0 audit)

The audit's P4 + deferred-medium tier all got tickets so they're tracked rather than forgotten. None are time-sensitive.

**v1.33.0 audit issues filed today** (medium-effort, no fix in this session):

| Range | Theme |
|-------|-------|
| #1337-#1359 | P4 batch (23 issues) — security defense-in-depth, RM eviction/idle-state, Extensibility refactors, Platform Behavior on Windows, missing e2e smoke tests |
| #1365 | P3-27: clap `--slot` help-text mismatch on slot/cache subcommands |
| #1366 | P3-49: structural CLI registry — top-level command needs three coordinated edits |
| #1369 | P1-5/P1-6: README eval numbers (closing via PR #1381) |
| #1370 | P2-9: HNSW M/ef defaults static — auto-scale with corpus |
| #1371 | P2-37: SQLite chunks missing composite index `(source_type, origin)` |
| #1372 | P2-14: `--rerank` (bool) on search vs `--reranker <mode>` on eval |
| #1373 | P2-13: `--depth` flag four defaults across five commands |
| #1374 | P2-4: `IndexBackend` trait uses `anyhow::Result` instead of `thiserror` |
| #1375 | P3-52: `lib.rs` wildcard `pub use diff::* / gather::* / ...` |
| #1376 | P2-8: `serve` async handlers duplicate ~15-20 LOC × 6 |
| #1377 | Umbrella: P2-36, P3-53, P3-54, P3-55 — perf micro-opts |

**Pre-existing tier-3 issues:**

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
- **HF preset tokenizers may ship `truncation: {max_length: 512}` baked into `tokenizer.json`** (HF's `optimum-cli` enables it by default on export). Affects bge-large-ft and v9-200k locally. cqs windowing/counting paths must clone-and-disable truncation before counting tokens, otherwise long sections silently chunk into 1-2 windows when they need 12+ — see PR #1384. Inference paths intentionally keep truncation. When adding a new preset, sanity-check `python -c "from tokenizers import Tokenizer; print(Tokenizer.from_file('tokenizer.json').get_truncation())"` before relying on token counts.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines (for regression comparison)

`v3_test.v2.json` (109q) and `v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109) — gold chunks re-pinned to current line numbers to absorb v1.29.x audit drift.

**Refreshed 2026-05-02 against current slot states (post-audit binary):**

| Config | test R@1 | test R@5 | test R@20 | dev R@1 | dev R@5 | dev R@20 |
|---|---|---|---|---|---|---|
| canonical (post-v1.28.3, 2026-04-20) | 41.3% | 68.8% | 85.3% | 45.0% | 78.0% | 88.1% |
| **default = BGE-large** | 44.0% | **72.5%** | **83.5%** | **49.5%** | **75.2%** | **87.2%** |
| embeddinggemma-300m | **47.7%** | 71.6% | 83.5% | 48.6% | **75.2%** | **87.2%** |
| bge-large-ft | 45.0% | **73.4%** | 83.5% | 46.8% | 70.6% | 82.6% |
| v9-200k | 42.2% | 67.9% | 79.8% | 45.9% | 70.6% | 83.5% |
| nomic-coderank | 42.2% | 67.9% | 79.8% | 47.7% | 69.7% | 81.7% |

Per-slot state at measurement time was uneven (default 19,857 chunks 48% summaries; bge-ft 14,460 0% summaries; gemma 13,118 90%; v9 14,468 82%; coderank 12,393 0%) — direct cross-model comparison is qualitative; tighter A/B requires reindex-from-shared-summary-set per slot. The earlier "post-v1.28.3 → refreshed" 3.7-5.5pp R@5 gap that was attributed to corpus drift was actually noise; current BGE-large numbers are *higher* than the canonical row across most metrics. The v3.v2 fixture is still the canonical slate; v4 fixtures (1526/split, 14× v3 N) exist for any future A/B that needs tighter noise floors.

**Apples-to-apples reindex pass (2026-05-02, post-summary-equalization):** all 4 slots reindexed `--force --llm-summaries` to identical 90%+ coverage. Results in Right Now; bge-ft + v9 contaminated by truncation bug (PR #1384). Re-run pending after #1384 merges.

The `research/models.md` file (committed in #1270) is the inaugural retrieval-research log. Future A/B writeups append there.
