# Project Continuity

## Right Now

**v1.36.1 shipped** — 2026-05-04 afternoon. Patch release. Headline: Qwen3-Embedding-4B preset (#1441) + FP16/BF16 ONNX output dispatch (#1442) — extends embedder to decoder-only architectures with `position_ids` input and 16-bit output tensors. Plus daemon ergonomics (server-side `wait_fresh`, idle-shutdown), HNSW perf scaling for large corpora, and 9 audit-driven fixes. 26 commits since v1.36.0; no schema bump. Tag pushed; crates.io published; GitHub Release auto-built.

**v1.36.2 staged** — 2026-05-04 evening. Two fixes merged but not yet released:
- **#1450 — `busy_timeout` 5s→30s** (defense-in-depth)
- **#1451 — `Store::drop` checkpoint TRUNCATE → PASSIVE** (the actual lock-contention root cause — see below)

Plus 5 dependabot bumps (tokio, fast_html2md, tree-sitter-{swift,powershell}, similar 2→3) and the docs(tears) update. Worth cutting v1.36.2 before next session — the lock-contention fix is materially load-bearing for any long-running `cqs index` against any concurrent reader.

**Qwen3-Embedding-4B full probe complete** (2026-05-04 afternoon-evening, 6+ hour session):

Results vs gemma canonical (post-α-retune v1.36.0):

| Slot / Tuning | Split | R@1 | R@5 | R@20 |
|---|---|---:|---:|---:|
| qwen3-4b base (FP16 batch=1 seq=4096) | TEST | 35.8% | 56.0% | 74.3% |
| qwen3-4b +summaries +enrichment | TEST | 48.6% | 67.9% | 81.7% |
| qwen3-4b +summaries +tuned α | TEST | 45.0% | **69.7%** | 81.7% |
| **gemma-300m canonical** | TEST | **49.1%** | **72.5%** | **86.2%** |
| qwen3-4b base | DEV | 45.9% | 70.6% | 86.2% |
| qwen3-4b +summaries +enrichment | DEV | 50.5% | 75.2% | 89.9% |
| qwen3-4b +summaries +tuned α | DEV | 49.5% | **77.1%** | 89.9% |
| **gemma-300m canonical** | DEV | **50.9%** | **79.8%** | 88.6% |

**Bottom line: gemma-300m wins.** Qwen3-4B at 13× the parameters, 3.3× the dim, 2× the context — loses by 2.7-2.8pp R@5 even with full enrichment + per-cat α tuning + Unknown hedge. Architecture isn't the lever; gemma's been pre-tuned and code-specialized.

Per-category α picks for qwen3-4b diverged dramatically from gemma's (5 of 8 categories shifted by ≥0.4 absolute α; identifier_lookup 1.00→0.15, structural_search 0.60→0.15). **This proves per-model α sets are needed** — filed as **#1453**.

Engineering wins from the probe (worth more than the eval result):
- **DB-lock root cause found** (#1451): `Store::drop`'s `wal_checkpoint(TRUNCATE)` blocked the long-running indexer's writes. Repro'd 3× in this session — three crashes with identical "(code: 5) database is locked" terminating runs after 5-25min of work. Switched to `PASSIVE` + 1s timeout; verified under deliberate stress polling. Same shape of bug `EmbeddingCache::drop` had fixed months ago (#1343); slot store had drifted.
- **WSL2 ceiling probed**: 4B FP32 (16 GB mmap) crashes WSL during load; 4B FP16 (8 GB mmap) loads cleanly. Confirms the WSL2 single-file mmap ceiling is somewhere in 8-16 GB on this hardware (96GB RAM, 32GB swap, autoMemoryReclaim=disabled).
- **GPU-OOM cascade understood**: 4B FP16 at batch=8 OOMs at 48.5/49 GB; batch=2 fits with 30 GB headroom; batch=1 fits with 40 GB headroom. CPU fallback fundamentally broken for FP16 models — ORT CPU EP can't execute the FP16 Cast nodes ("GetElementType is not implemented") — so any GPU OOM at FP16 is fatal without batch=1.
- **Cross-slot summary copy works** (saved $3-4 of Haiku spend): 7674/12881 chunk hashes overlapped between gemma and qwen3-4b slots; only 1269 fresh summaries needed. Cost ~$1.27.

Open issues filed:
- **#1452** — `perf(index): skip first-pass embed when --llm-summaries guarantees enrichment overwrite`. The `cqs index --force --llm-summaries` pipeline embeds every chunk twice (raw, then enriched-overwrite). On qwen3-4b that's ~30 min of wasted GPU. Roughly halves the time when picked up.
- **#1453** — `feat(splade): per-slot α tables — gemma-tuned globals don't generalize`. Includes the qwen3-4b sweep matrix. Proposes `slot.toml [splade.alpha]` section with the existing precedence chain (env > slot > preset > global default).

**Qwen3-8B is now feasible** to probe: FP16 dispatch landed (#1442), DB-lock fixed (#1451), batch=1 envelope confirmed safe at FP16 8GB mmap on this hardware. ETA ~2-3 hours bare reindex + ~30 min summary batch + ~30 min enrichment. Not on the critical path but the probe is cheap given the engineering investment we already paid.

**v1.36.0 (two days prior)** — Per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge. Schema v25→v26 (composite `(source_type, origin)` index on chunks). Plus the critical readonly-migration bug fix.

**Eval baseline (post-α-retune, v1.36.0 default):**

| Metric | v1.35 (BGE α) | v1.36 (gemma α + Unknown=0.80) | Δ |
|---|---:|---:|---:|
| Test R@5 | 68.8% | **72.5%** | +3.7pp |
| Dev R@5 | 76.1% | **79.8%** | +3.7pp |
| Agg R@1 | 49.1% | **50.9%** | +1.8pp |
| Agg R@5 | 72.5% | **76.2%** | +3.7pp |
| Agg R@20 | 86.2% | **88.6%** | +2.4pp |

Sweep methodology: 11 alphas × 2 splits × 8 categories = 176 R@K data points on the gemma slot (13,359 chunks). Joint-optimal α picked by argmax of mean(test R@5, dev R@5). Critical insight: the rule-based `classify_query()` misroutes many fixture-labelled queries to `QueryCategory::Unknown`, where pre-v1.36 default α=1.00 (pure dense) was the worst point in the global sweep. Setting `Unknown=0.80` reclaims most of the predicted lift. Sweep artifacts at `/tmp/gemma-alpha-sweep/`.

**Other rows in the README "Retrieval Quality" table are still pre-retune** (BGE-large, bge-large-ft, v9-200k, nomic-coderank were measured under v1.35 alphas). A 5-slot rerun under the new alphas is queued; their numbers will shift up but the gemma row stays the leader.

**In parallel, qwen3-8b ceiling probe waiting for an overnight window.** Engineering envelope is unblocked (#1394 retries + CPU-warm gate, #1396 routing-threshold scaling); a single bare reindex pass is ~5–7 hours, plus another ~5–7 for the summary reindex. Full restart protocol in `~/training-data/research/models.md` "Qwen3-Embedding-8B ceiling probe — overnight restart protocol" section.

**Recent shipped (today, 2026-05-03):**
- v1.36.0 release prep (earlier session). Headline: per-category α retune + Unknown hedge + schema v26.
- 13 audit follow-up PRs landed (#1398 #1399 #1400 #1401 #1402 #1403 #1404 #1405 #1406 #1407 #1408 #1409 #1410 #1411 #1412 #1413 #1414).
- **Post-release autopilot wave 1 (2026-05-03 evening, 7 PRs)**:
  - #1428 — `.claude/scheduled_tasks.lock` removed from repo + gitignored
  - #1429 — perf(impact): `Arc<str>` keys in reverse-BFS + build_test_map (closes #1377 — P3-55, finalizes the umbrella)
  - #1430 — fix(serve): `--open` suppressed under auth to keep token off subprocess argv (closes #1337)
  - #1431 — fix(hnsw): widen `test_build_batched` search windows to top-N (post-#1370 small-tier flake; unblocked main CI)
  - #1432 — test(gc): `cmd_gc` end-to-end test (closes #1358)
  - #1433 — fix(hook): embed POSIX-translated `cqs.exe` path on Windows installs (closes #1354)
  - #1434 — feat(serve): idle-shutdown after `CQS_SERVE_IDLE_MINUTES` (closes #1345)
- **Luxury route (this session, 2026-05-04 early morning, 4 PRs of the 5 picks)**:
  Highest-taste open items, picked by aesthetic merit not effort:
  - #1436 — feat(daemon): server-side `wait_fresh` — single round-trip, zero polling (closes #1228 / RM-2). New `FreshNotifier` (`Mutex<bool> + Condvar`); watch loop publishes, daemon parks parked clients on `Condvar::wait_timeout` until next `false → true` transition. Replaces the 250 ms-poll loop (4-5k connect/parse round-trips per 60 s wait at default budget).
  - #1437 — refactor(batch): macro-table dispatch + uniform `&args` handlers (closes #1216). Collapses 33-arm hand-written dispatch into single `for_each_batch_cmd!` table that emits both `is_pipeable` and `dispatch`. Adding a new variant: declare on `BatchCmd` + write handler + add one row, all compile-enforced. Refactored ~30 handlers to uniform `fn(ctx, &XArgs)` signature; `Reconcile` + `WaitFresh` wrapped in `ReconcileArgs` / `WaitFreshArgs` for shape uniformity.
  - #1438 — test(serve): `cqs serve` end-to-end smoke test (closes #1359). Spawns `cqs serve --port 0`, parses banner for token + ephemeral port, runs three real HTTP/1.1 GETs (Bearer 200, no auth 401, /api/graph 200+JSON). Pins the SEC-1.30-V1 layer-composition order — auth + host-allowlist + body-limit + trace + compression. Uncovered gotchas pinned in code comments: banner-reader threads must keep draining (else server hits EPIPE); Bearer auth dodges the `?token=` 303 redirect; bare `127.0.0.1` Host header beats the port-0 allowlist mismatch.
  - #1439 — perf(reconcile): stream `enumerate_files` + batched mtime lookup (closes #1229 / RM-5). New `enumerate_files_iter() -> impl Iterator<Item = PathBuf>` and `Store::fingerprints_for_origins(origins: &[&str])`. Reconcile no-pre-walk path streams 1k-file batches; peak heap is `O(batch_size)` regardless of tree size. ~12 MB transient → bounded ~per-batch on a 100k-file repo.
- **Skipped from luxury picks: #1366(b)** — `cqs-cli-derive` proc-macro crate. Deferred: the proper version requires extracting ~50 variant bodies into uniform handlers + new proc-macro crate (~6 hours of single-mega-PR work). Better fit for a dedicated session — the four merged PRs don't block it.

### Recent release history (compressed)

- **v1.36.1** — Qwen3-Embedding-4B preset + position_ids ONNX input (#1441); FP16/BF16 output tensor dispatch (#1442); daemon server-side `wait_fresh` (#1436); daemon idle-shutdown (#1434); HNSW defaults scale by corpus size (#1425); reconcile streaming + batched mtime (#1439); plus 9 audit fixes. No schema bump.
- **v1.36.0** — per-category SPLADE α retuned for EmbeddingGemma (test/dev R@5 +3.7pp); schema v25→v26 composite chunks index; `--reranker <none|onnx|llm>` exposed on `cqs search`; readonly migration bug (#1413) caught during eval validation. 13 audit follow-up fixes bundled.
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
| #1337-#1359 | P4 batch — partially landed: #1337 #1345 #1354 #1358 closed this session; remaining P4 items (#1350 #1351 #1359 #1366) are architecture-class or "hard" labelled |
| #1366 | P3-49: structural CLI registry — proc-macro crate; needs dedicated session for body extraction across ~50 variants |
| #1377 | ✅ Closed by #1429 (P3-55 BFS Arc<str> finalized the umbrella) |
| #1345 | ✅ Closed by #1434 (idle eviction) |
| #1354 | ✅ Closed by #1433 (Windows hook PATH) |
| #1358 | ✅ Closed by #1432 (cmd_gc e2e test) |
| #1337 | ✅ Closed by #1430 (token leak via xdg-open argv) |
| #1228 | ✅ Closed by #1436 (server-side wait_fresh) |
| #1216 | ✅ Closed by #1437 (macro-table dispatch) |
| #1359 | ✅ Closed by #1438 (cqs serve smoke test) |
| #1229 | ✅ Closed by #1439 (streaming enumerate_files_iter) |

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
| 1216 | ✅ Closed by #1437 — macro-table dispatch + uniform `&args` handlers |
| 1228 | ✅ Closed by #1436 — server-side `wait_fresh` parks on `FreshNotifier`, single round-trip |
| 1229 | ✅ Closed by #1439 — `enumerate_files_iter` + batched fingerprints, O(batch) heap |
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
