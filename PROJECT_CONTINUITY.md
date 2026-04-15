# Project Continuity

## Right Now

**v1.26.0 release in flight (2026-04-15 afternoon CDT).** All watch-mode + SPLADE-flag + alpha-routing gaps closed.

Main at `dd97612`. Release branch: `release/v1.26.0` (Cargo.toml + CHANGELOG bumped, PR pending).

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

- **Version:** v1.26.0 (release branch open), Schema v20
- **Binary:** needs rebuild + install after release PR merges (currently at pre-release main)
- **Index:** clean (14,882 chunks, 100% SPLADE coverage)
- **Tests:** 1450+ lib tests pass; 16 router tests updated for v1.26.0 alphas
- **Eval harness:** `evals/run_ablation.py` → `BGE-large+SPLADE` config now actually hits the router (was hardcoding `--splade-alpha 0.7`)

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
