# Project Continuity

## Right Now

**Multi-hour autopilot sweep of open issue fixes** — 2026-05-03 evening. User-directed: keep going on small bounded fixes from the open-issue queue until interrupted. Each fix gets its own branch + PR, no waiting on CI between them.

**Sweep order (smallest, most contained first):**
1. **#1107** — `cqs slot create --model` validates the arg but doesn't persist it. Workaround: pass `--model` globally on every invocation. Real fix: write the model name to `slot.toml` during creation.
2. **#1108** — `content_hash` missing from 5 hot SELECTs in `src/store/{search.rs, chunks/async_helpers.rs, chunks/query.rs}`. Caused ~2,180 warnings/eval; reference.rs:333 falls back to recomputing blake3 per result for dedup. Fix is mechanical: add the column.
3. **#1395** — replace the char-count GPU-routing heuristic with token-count (or remove the branch entirely now that windowing enforces the bound). The interim scaling shipped in #1396, but this is the proper redesign.
4. **v1.33.0 audit P4 batch (#1337-#1359)** — 23 small defense-in-depth + cleanup items. Pick by triviality.

**In parallel, qwen3-8b ceiling probe waiting for an overnight window.** Engineering envelope is unblocked (#1394 retries + CPU-warm gate, #1396 routing-threshold scaling); a single bare reindex pass is ~5–7 hours, plus another ~5–7 for the summary reindex. Full restart protocol in `~/training-data/research/models.md` "Qwen3-Embedding-8B ceiling probe — overnight restart protocol" section. User will start it overnight.

**Recent shipped (today, 2026-05-03):**
- v1.34.0, v1.35.0 cut on 2026-05-02 (same day as v1.33.0, then quick follow-up). Default embedder swap to **embeddinggemma-300m**: agg R@1 49.1% / R@5 72.5% / R@20 86.2% on v3.v2 218q dual-judge.
- `#1384` — tokenizer truncation fix (bge-large-ft / v9-200k / coderank tokenizers ship `truncation: max_length=512`; cqs's windowing/counting silently capped at 512 tokens).
- `#1385` — v1.35.0 release; default-model swap.
- `#1386` — post-release tears.
- `#1388` — ci-slow doctest fix (`ignore` → `text` because `--include-ignored` promotes ignored doctests to runnable).
- `#1390` — `test_prune_zero_days` flake fix (insert future-dated rows instead of relying on same-second timing).
- `#1391` — TRT-RTX wiring tracking issue (blocked on ORT 2.0.0-rc.12 Linux platform gate).
- `#1392 → #1394` — `CQS_DISABLE_CPU_WARM` env var to halve host-RAM pressure for large models. Plus hf-hub `max_retries: 0 → 5` and warn-vs-debug for unexpected sidecar download failures.
- `#1393` — qwen3-embedding-8b research preset (opt-in; A6000-class only).
- `#1395` — tracking issue: GPU-vs-CPU routing should use token count, not char count.
- `#1396` — interim fix: scale the char threshold by `max_seq_length / 512`. Empirically validated on the qwen3 probe (66 false routings → 0 + 2 genuine GPU OOMs; RSS 91 GB → 31 GB).

**Eval baseline as of v1.35.0 + tokenizer fix (apples-to-apples, all 5 slots reindexed `--force --llm-summaries`):**

| Slot | Agg R@1 | Agg R@5 | Agg R@20 |
|---|---:|---:|---:|
| **embeddinggemma-300m (default)** | **49.1%** | 72.5% | **86.2%** |
| bge-large-ft | 47.7% | **73.4%** | **86.2%** |
| BGE-large | 47.2% | 72.0% | 84.4% |
| v9-200k | 45.0% | 68.8% | 80.7% |
| nomic-coderank | 45.0% | 67.9% | 78.9% |

Existing slot indexes keep their stored model — only fresh slots / fresh `cqs index` runs pick up the new default. BGE-large remains a first-class preset (`CQS_EMBEDDING_MODEL=bge-large`).

Full per-split numbers + per-category breakdowns + eval-methodology in `~/training-data/research/models.md` "Five-Way A/B" section.

### Recent release history (compressed)

- **v1.35.0** — default embedder swap to embeddinggemma-300m + tokenizer truncation fix (#1384). Truncation fix surfaced via apples-to-apples comparison; bge-large-ft / v9-200k / coderank had been silently dropping ~90% of long-section content.
- **v1.34.0** — bundled the post-v1.33.0 audit close-out (24 fix PRs) + EmbeddingGemma-300m preset (#1301), `cqs eval --reranker` (#1303), `slow-tests` Phase 2 (#1302), ci-slow.yml stabilization. Same day as v1.33.0.
- **v1.33.0** — eval-matcher drift fix (#1284), placeholder-cache 30s startup tax fix (#1288, CI test job 38min→6min), chunk orphan pipeline prune (#1283), `bge-large-ft` preset (#1289), daemon test refactor + nightly CI workflow (#1292, #1286 Phase 1).
- **v1.32.0** — HNSW load-phase flock self-deadlock fix (#1261); structural-trust three-tier (#1221); worktree → main-index discovery (#1254); note kind taxonomy (#1133); persistent TRT engine cache (#1260). Schema v23→v25.

### v1.33.0 audit close-out

16-category audit produced 167 findings; triaged P1=47/P2=41/P3=56/P4=23. **24 fix PRs landed**; 25 medium-effort items filed as tracking issues (#1337-#1377). Coverage: 129 ✅ closed / 15 🎫 issue-tracked / 0 ⬜ open.

**Operational lesson from the audit:** PR #1380 was needed to recover **112 lost ✅ flips** in `docs/audit-triage.md` after force-pushed rebases naively resolved triage-row conflicts using older agents' pre-flip snapshots. Source-code fixes were unaffected; only the bookkeeping rolled back. Mitigation: keep triage flips append-only OR move each PR's triage update into a separate narrow PR per cluster.

### Outstanding follow-ups (small, optional)

- Retroactive vendored / kind tagging for pre-v25 rows — operator can `cqs index --force` if they want immediate flagging.
- `cuvs` crate update — upstream PRs #1840 (serialize/deserialize) + #2019 (search_with_filter) both merged into rapidsai/cuvs; `[patch.crates-io]` entry on `jamie8johnson/cuvs-patched` becomes redundant once a new cuvs crate publishes (RAPIDS ~2-month cadence).

## Open issues

**Sweep targets (small, contained — pick first):**

| # | Title | Why open / scope |
|---|---|---|
| 1107 | `cqs slot create --model` not persisted | Validates the arg but doesn't write to slot.toml. Mechanical fix. |
| 1108 | `content_hash` storm — 5 hot SELECTs missing column | ~2,180 warnings/eval; 5 SELECT statements need the column added. |
| 1395 | GPU-vs-CPU routing should use token count | Proper redesign of #1396's interim threshold scaling. |

**v1.33.0 audit (medium-effort, batchable):**

| Range | Theme |
|-------|-------|
| #1337-#1359 | P4 batch (23 issues) — security defense-in-depth, RM eviction/idle-state, Extensibility refactors, Platform Behavior on Windows, missing e2e smoke tests |
| #1365 | P3-27: clap `--slot` help-text mismatch on slot/cache subcommands |
| #1366 | P3-49: structural CLI registry — top-level command needs three coordinated edits |
| #1370 | P2-9: HNSW M/ef defaults static — auto-scale with corpus |
| #1371 | P2-37: SQLite chunks missing composite index `(source_type, origin)` |
| #1372 | P2-14: `--rerank` (bool) on search vs `--reranker <mode>` on eval |
| #1373 | P2-13: `--depth` flag four defaults across five commands |
| #1374 | P2-4: `IndexBackend` trait uses `anyhow::Result` instead of `thiserror` |
| #1375 | P3-52: `lib.rs` wildcard `pub use diff::* / gather::* / ...` |
| #1376 | P2-8: `serve` async handlers duplicate ~15-20 LOC × 6 |
| #1377 | Umbrella: P2-36, P3-53, P3-54, P3-55 — perf micro-opts |

**Filed during today's qwen3 work (deferred):**

| # | Title | Status |
|---|---|---|
| 1391 | TRT-RTX wiring | Blocked on ORT 2.0.0-rc.12 Linux platform gate |
| 1392 | `CQS_DISABLE_CPU_WARM` env var | ✅ Closed by #1394 |
| 1395 | Token-count routing | Open (sweep target) |

**Pre-existing tier-3 issues (long-running, lower priority):**

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

## Eval baselines

Canonical slate: `evals/queries/v3_test.v2.json` (109q) + `evals/queries/v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109) — gold chunks re-pinned to current line numbers.

**Current baseline (apples-to-apples 2026-05-02, all 5 slots `cqs index --force --llm-summaries` post #1384 truncation fix):**

| Slot | dev R@1 | dev R@5 | dev R@20 | test R@1 | test R@5 | test R@20 |
|---|---:|---:|---:|---:|---:|---:|
| BGE-large | 51.4% | 75.2% | 86.2% | 43.1% | 68.8% | 82.6% |
| embeddinggemma-300m | 49.5% | 76.1% | 89.0% | **48.6%** | 68.8% | 83.5% |
| bge-large-ft | 50.5% | 75.2% | 87.2% | 45.0% | **71.6%** | **85.3%** |
| v9-200k | 45.9% | 69.7% | 81.7% | 44.0% | 67.9% | 79.8% |
| nomic-coderank | 46.8% | 68.8% | 79.8% | 43.1% | 67.0% | 78.0% |

Per-slot summary coverage at measurement (capped by `chunk_type.is_code()` eligibility filter at `src/llm/mod.rs:115` — markdown / json / ini chunks are deliberately skipped, so coverage % varies with each tokenizer's chunk-type distribution):

- default 62.1%, bge-large-ft 62.1%, gemma 99.0%, v9 67.6%, coderank 65.5%

**Apples-to-apples does not mean equal coverage** — it means each slot has all *its* eligible chunks summarized, which is now true. The 62-99% spread is structural, not API-bound (`cached=9222 skipped=10238 api_needed=3` in the 2026-05-02 fill-in run).

Full per-category breakdowns + methodology in `~/training-data/research/models.md` "Five-Way A/B" section. v4 fixtures (1526/split, 14× v3 N) exist for any A/B that needs tighter noise floors.
