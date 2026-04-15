# Project Continuity

## Right Now

**v1.26.0 SHIPPED (2026-04-15 evening CDT). Now: eval expansion via local Gemma 4 31B.**

Main at `28f76b9` (Release v1.26.0). crates.io published `cqs 1.26.0`. Local binary installed, `cqs-watch` daemon active on v1.26.0. GitHub release workflow built binaries for Linux/macOS/Windows.

### Next goal: bigger, honest v3 eval dataset

Current eval (265q) has sampling-floor problems: cross_language N=21 (±10pp SE), type_filtered N=24. Per-category routing decisions live in the noise at that scale. Expansion target: N≥100 per category, ~1000 queries total, with proper train/dev/test split (600/200/200).

Wired up today to make this tractable:

- **vLLM Gemma 4 31B AWQ** serving at `http://127.0.0.1:8000/v1` (model id `gemma-4-31b`)
  - Env: `~/miniforge3/envs/vllm-serve/` (separate from `cqs-train` because cqs-train runs CUDA 13 and vLLM wants 12.8)
  - Launch command + notes in `~/.claude/projects/-mnt-c-Projects-cqs/memory/reference_vllm_gemma.md`
  - A6000 is maxed at ~47.6 GB (19.6 weights + 20 KV + 7 overhead) — if training needs the GPU, `pkill -f 'vllm serve'` first
  - Log: `~/logs/vllm-serve.log`. Restart after reboot: re-run the launch command from memory file (no systemd unit yet).
  - Known issue: requires `transformers` from git (5.6.0.dev0) because 4.57.6 doesn't recognize `gemma4` arch. vLLM 0.19 warns about the version but runs.
- **Labeling harness** at `evals/llm_client.py` — async OpenAI-compatible client with three prompt modes (`classify`, `generate`, `validate`) and blake3→SQLite cache at `~/.cache/cqs/llm-cache.db`. Smoke-tested end-to-end: classify/generate/validate all functional.

### Plan (what's not done yet)

1. **Telemetry mining** — 16,731 logged cqs invocations in `~/.config/cqs/` (or wherever telemetry lives). Cluster real queries, stratify-sample ~1500. No API cost.
2. **LLM-generated scale-up** — walk indexed chunks, have Gemma generate 2-3 queries each across categories, filter to ~1000 diverse.
3. **Multi-judge labeling** — Gemma + Claude Sonnet + one other model label each query's category and gold-answer chunk. Majority vote; hand-adjudicate ties.
4. **v3 split** — 600 train (centroids, alpha sweep) / 200 dev (threshold tuning) / 200 test (frozen).
5. **Cross-project test set** — run against openclaw, a Python project, a TypeScript project. Catches cqs-only overfitting.
6. **Calibration gate** — before step 3 commits 200k labels, 100-query calibration run with both Gemma and Claude. If <70% agreement, bigger model or API-only.
7. **Then** centroid classifier (the original target), alpha re-sweep on expanded eval, measure lift.

### Session landings (on top of Wave A–F from earlier in the day)

### Session landings (on top of Wave A–F from earlier in the day)

| PR | Closes | What |
|---|---|---|
| #1003 | #1002 (short-term) | Hardcode `.claude/` skip in watch loop while full fix was in flight |
| #1006 | #1002 | `cqs watch` respects `.gitignore` (RwLock-wrapped `Gitignore`, `matched_path_or_any_parents`, kill-switch env) |
| #998  | #986  | `Store::open_readonly_after_init` replaces unsafe `into_readonly` |
| #1005 | —     | Per-category SPLADE alphas re-fit on genuinely-clean 14,882-chunk index (+1.8pp R@1) |
| #1007 | #1004 | Incremental SPLADE encoding in `cqs watch` (dense+sparse inline, kill-switch, per-batch error isolation) |
| #1008 | —     | `--splade` flag no longer bypasses router (semantic bug from pre-routing era) |

### Eval numbers (2026-04-15, clean 14,882-chunk index, 100% SPLADE coverage)

| Config | R@1 | R@5 | R@20 | Notes |
|---|---|---|---|---|
| **BGE-large + SPLADE (router, v1.26.0 alphas)** | **39.2%** | 58.8% | 78.6% | Per-category: ident 1.00, struct 0.90, concept 0.70, behav 0.00, neg 0.80, rest 1.00 |
| BGE-large dense only | 35.8% | 54.7% | 74.7% | Router path with dense-only (no SPLADE) |
| v1.25.0 alphas on clean index | 26.8% | 45.7% | 75.5% | Old alphas tuned on the dirty 96k-chunk index — 9pp below dense-only |
| v1.25.0 baseline (stored JSON) | 37.4% | 55.8% | 77.4% | Actually dense-only, was mislabeled as "fully routed" in tears |

**+1.8pp over the corrected v1.25.0 baseline; +3.4pp over dense-only.** Net session: router gives meaningful lift once alphas are tuned on the real index and the `--splade` bypass is gone. `~/training-data/research/models.md` has the 21-point sweep details.

### Open issues

**18 total** (down from 26). **Tier-1 remaining: 0.** Tier-2 focus: #63 paste advisory, #916/#917/#921 SPLADE perf trio, #956 Metal/ROCm, #957 SPLADE preset registry. Tier-3: 12 audit-v1.25.0 items (refactor + test + perf) forming the Wave G backlog.

Closed this session: #1002 (PR #1006), #1004 (PR #1007), #986 (PR #998). #951 README re-bench deferred until next eval window.

## Architecture state

- **Version:** v1.26.0 RELEASED, Schema v20
- **Binary:** rebuilt + installed from `28f76b9`; `cqs-watch` daemon running v1.26.0
- **crates.io:** `cqs 1.26.0` published
- **GitHub release:** artifacts built by `release.yml` workflow for Linux/macOS/Windows
- **Index:** clean (14,917 chunks, 100% SPLADE coverage)
- **Tests:** 1450+ lib tests pass; 16 router tests updated for v1.26.0 alphas
- **Eval harness:** `evals/run_ablation.py` → `BGE-large+SPLADE` config now actually hits the router (was hardcoding `--splade-alpha 0.7`)
- **Labeling infra:** vLLM Gemma 4 31B AWQ at :8000, `evals/llm_client.py` client with SQLite cache at `~/.cache/cqs/llm-cache.db`

## Operational pitfalls captured this session

1. **`--splade` flag bypassed the router** — pre-routing-era CLI code took clap's `default_value = "0.7"` for `splade_alpha` whenever `--splade` was set, silently skipping `classify_query` + `resolve_splade_alpha`. The fix: `splade_alpha: Option<f32>` with no default, then `match (splade_alpha, classification)` at the call site. Caught only because the "regression" investigation forced a read of the actual CLI code.
2. **Alphas tuned on a dirty index are wrong for the clean one** — the 2026-04-14 sweep ran on a 96,029-chunk index polluted by `.claude/worktrees/*`. Once those were evicted (14,882 chunks), the same 21-point sweep produced materially different optima for 4 of 5 categories. Lesson: never sweep against an index you haven't just run `cqs index` against and confirmed chunk count.
3. **`cqs watch` didn't respect `.gitignore`** — watch used raw notify events, skipping the `ignore` crate entirely. `cqs index` used it correctly. This is what blew the index up to 96k chunks from agent worktrees. Closed by PR #1006 with `Gitignore::matched_path_or_any_parents(path, false)` (the `.matched(p, false)` variant only checks the leaf, not parent directories — initial test run caught this).
4. **Watch skipped SPLADE for incremental updates** — dense-only during watch meant SPLADE coverage drifted (observed 70% on a day of active dev). Closed by PR #1007; encoder held in a `Mutex`, batches of `CQS_SPLADE_BATCH` (default 32) with per-batch try-catch so one pathological file doesn't block the loop.
5. **Rebase-conflict markers got committed** — during #998's rebase I resolved the `into_readonly()` → `open_readonly_after_init` conflict but left a stray `<<<<<<< HEAD` without the `=======` / `>>>>>>>` lines. CI "encountered diff marker" caught it; wrong-worktree confusion during my shell session was the real cause.
6. **Router unit tests are a spec, not a reference** — PR #1005 changed `resolve_splade_alpha` defaults but didn't update `tests/router_test.rs`, which asserted exact old values. CI caught 10 failures. The comment in that file ("This table is a spec, not a reference — do not update it without a corresponding alpha-sweep update in docs") is correct — I had to update both together.
7. **`gh issue list --jq` is fragile under PowerShell** — wrap the whole arg in single quotes, no embedded `\"`. When in doubt, dump raw JSON and parse locally.

## Architecture notes (delta since v1.25.0)

- `Store::open_readonly_after_init(path, init_fn)` — closure takes `&Store<ReadWrite>`, returns `Store<ReadOnly>` after drop; no more `ptr::read` / `ManuallyDrop` unsafe (#986, PR #998)
- `cqs watch` pipeline: `build_gitignore_matcher(root)` + `RwLock<Option<Gitignore>>` in `WatchConfig` — hot-swappable if the user edits `.gitignore`; `CQS_WATCH_RESPECT_GITIGNORE=0` turns it off (#1002, PR #1006)
- `cqs watch` SPLADE: `build_splade_encoder_for_watch()` + `Mutex<SpladeEncoder>` in `WatchConfig`, `encode_splade_for_changed_files()` batches by `CQS_SPLADE_BATCH`; per-batch try-catch; kill-switch `CQS_WATCH_INCREMENTAL_SPLADE=0` (#1004, PR #1007)
- `SearchArgs::splade_alpha` / `Cli::splade_alpha` now `Option<f32>`; explicit `--splade-alpha X` overrides router, bare `--splade` force-on with router active, no flag = dense-only (PR #1008)
- Per-category SPLADE alphas (PR #1005): `IdentifierLookup=1.00`, `Structural=0.90`, `Conceptual=0.70`, `Behavioral=0.00`, `Negation=0.80`, rest=`1.00`. Negation is now an explicit match arm (was catch-all at 1.00).
