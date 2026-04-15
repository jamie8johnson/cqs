# Project Continuity

## Right Now

**Waves A–F all merged or draining. 11 PRs / 13 issues closed this session; autopilot successful. (2026-04-15 ~13:35 CDT)**

Main at `10b1f14`. Landed this session (in order):

| PR | Closes | What |
|---|---|---|
| #979 | #961/#962/#963 | Wave A quick wins (WSL mmap, CAGRA itopk, reranker batch) |
| #981 | #947 (manual) | Wave B — Commands/BatchCmd unification |
| #983 | #948 | Wave C2 — `cqs::fs::atomic_replace` helper |
| #984 | #949 | Wave C3 — ModelConfig abstraction (InputNames/PoolingStrategy) |
| #985 | #950 | Wave C4 — CAGRA persistence (.cagra + .cagra.meta) |
| #982 | #946 | Wave C1 — Store typestate (`Store<ReadOnly>`/`<ReadWrite>`) |
| #987 | — | Hotfix: `save_with_store<Mode>` (parallel-merge-order artifact) |
| #988 | #980 (partial) | CI slow-tests feature gate (PR-CI ~2h 15m → ~18m) |
| #989 | — | Tears update + Reranker V2 roadmap with Gemma 4 pipeline |
| #990 | #965 | F1 — NameMatcher ASCII fast path (1.2-1.5× speedup) |
| #991 | #967 | F3 — Reindex drain-owned chunks (~180MB/burst saved) |
| #992 | #964 | D2 — Aho-Corasick + LazyLock language_names |
| #994 | #923 | F4 — INDEX_DB_FILENAME constant (56 literal sites) |
| #995 | #952 | F5 — CAGRA sentinel INVALID_DISTANCE const |

**In flight (final drain by Monitor `bhki2izu4`):**

- #993 (F2 #970 open_readonly_small, 16MB mmap for refs) — rebased post-#994
- #996 (E1 #953 migration fs backup)
- #997 (E2 #973 dispatch_search content assertions) — rebased
- #998 (F6 #986 open_readonly_after_init + drop unsafe into_readonly) — rebased
- #999 (D1 #972 daemon try_daemon_query tests)
- #1000 (E3 #968 shared Arc<Runtime> across Store/EmbeddingCache/QueryCache) — rebased

### Aggregate impact of the session

**Architecture/safety:**
- Store typestate closes write-on-readonly bug class at compile time (#946)
- Commands/BatchCmd parity via shared arg structs (#947)
- atomic_replace + migration fs-backup + restore close data-integrity class (#948, #953)
- CAGRA persistence cuts daemon hot-restart 30s → 5s (#950)
- ModelConfig abstraction unblocks BGE→E5 v9-200k default switch (#949)
- Shared `Arc<Runtime>` across caches reduces runtime proliferation (#968)
- `open_readonly_after_init` closure pattern replaces unsafe `into_readonly` ManuallyDrop/ptr::read (#986)

**Performance:**
- Classifier Aho-Corasick: 1.31× on type_filtered path, zero-alloc `language_names` (#964)
- NameMatcher hot path: 1.2-1.5× via ASCII fast path (#965)
- Reindex clone elimination: ~180MB + ~1.4M allocs saved per 20-file watch burst (#967)
- `open_readonly_small` for reference stores: 256MB → 16MB mmap × N refs (#970)
- CI slow-tests gate: PR CI 2h 15m → ~18m (#980 partial)

**Testing:**
- Daemon `try_daemon_query` coverage + socket mock (#972)
- Content-asserting `dispatch_search` tests (#973)

### Pending follow-ups (scoped, not gated on anything)

- **#951** re-bench README Performance table on v1.25.0 — do at next eval window (~20 min, not agent-task)
- **#980 full rewrite** — convert 5 `cli_*_test` binaries to in-process fixture pattern. ~5h agent-task, separate project
- **Wave G candidates** (all Tier-3 perf/refactor): #955, #958, #959, #960, #966, #969, #971, #974, #975

### Reranker V2 pipeline (captured in ROADMAP)

Data-labeling: **Gemma 4 31B Dense** at Q4_K_M via vLLM on A6000 — 200k pairwise judgments in ~5h at $0 cost, vs. ~$600 for Claude Haiku. Calibration: 1k gold subset agreement ≥85% → local-only; otherwise hybrid Gemma+Haiku. Training: 100-300M param cross-encoder with pairwise ranking loss (DPO-family), ~1-2 days on A6000. Export to ONNX, ship alongside current ms-marco reranker in `~/.local/share/cqs/`. Separate project — no immediate work.

## Eval numbers — with a baseline correction

**Session eval finding:** the prior "fully routed 37.4%" baseline in PROJECT_CONTINUITY.md was **actually dense-only** (`--config bge-large`, no SPLADE). Verified by inspecting the stored `run_20260414_135451/results.json` — `"configs": {"BGE-large": ...}`, no `+SPLADE` variant. The tears doc mislabeled it as "fully routed" which made today's SPLADE-enabled eval look like a regression that wasn't one.

### Corrected baselines (2026-04-15, post-cleanup index 14,882 chunks, 100% SPLADE coverage)

| Config | R@1 | R@5 | R@20 | ident R@1 |
|---|---|---|---|---|
| **BGE-large dense only** | **35.8%** | 54.7% | 74.7% | **96.0%** |
| BGE-large + SPLADE (per-category routed default α's) | 26.8% | 45.7% | 75.5% | 54.0% |
| Yesterday (dense only, pre-Wave-A) | 37.4% | — | — | 92.0% |

**Dense-only is within 1.6pp of yesterday (noise).** SPLADE is actively *hurting* R@1 on the clean index with the v1.25.0 per-category alphas (identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05, rest 1.0). This is the "alphas tuned on dirty index" risk that ROADMAP called out — now confirmed empirically.

**No code regression was introduced by waves A-F.** The reverts of #964 and #965 during diagnosis each gave identical SPLADE-enabled R@1 (20.4%), proving neither was the cause.

### SPLADE coverage gap — separate finding

Before reindex: **SPLADE coverage was only 70%** (10,358/14,882 chunks) because `cqs watch --serve` skips SPLADE encoding for new/changed chunks (known ROADMAP item). Backfilling via `cqs index` brought it to 100% but didn't help R@1 — the alpha problem dominates.

Default-model switch (BGE → E5 v9-200k) now unblocked by #949. Pending:
- **21-point alpha re-sweep on clean index** — running via `evals/run_alpha_sweep.sh` (kicked off at 2026-04-15 ~15:10 CDT)
- Extract per-category optima from the sweep, update `router.rs` defaults
- Confirmation re-run with new defaults + E5 v9-200k

## Architecture state

- **Version:** v1.25.0, Schema v20
- **Binary:** rebuilt + installed from `10b1f14` (post-#989), daemon active
- **Index:** clean (13,279 chunks)
- **Test count:** 1415+ lib tests, expanded by wave D/E/F tests once merged
- **Open issues:** 26 (blocked-upstream: 3, fresh tier-2/3 backlog: rest)

## Operational pitfalls captured this session

1. **Multi-agent worktree leaks to main tree** — `isolation: "worktree"` doesn't chroot the Edit/Write tool. Agents can still write to the parent repo root via absolute paths. Filed with Anthropic; memory updated with mitigation text to paste into every agent prompt.
2. **Shared cargo target-dir contention** — agents clobber each other's compile units during parallel work. Agents that encountered this set `CARGO_TARGET_DIR=...agent-XXXX` to dodge. Configurable via env in `.cargo/config.toml` future work.
3. **CI clippy is `--features gpu-index -- -D warnings` (no `--all-targets`)** — local `--all-targets` treats test warnings as warnings; CI promotes them. Match CI invocation exactly before pushing.
4. **Parallel PRs on same file → merge conflicts** — 4 PRs touching `src/store/mod.rs` and `src/cli/batch/mod.rs` this session required manual rebase. Monitor serializes merges, handler handles rebase conflicts.
5. **`gh pr merge --delete-branch` fails when local worktree holds the branch** — just a warning, remote delete still succeeds. Clean up worktrees first or accept the noise.
6. **PR bodies need `Closes #NNN`** — #947 got orphaned because #981 only mentioned it in prose. Auto-close requires the magic keyword.
7. **`git stash drop` is destructive** — mid-rebase stash can lose uncommitted work (lost 4 session notes + tears diff once; recoverable via `cqs notes add` re-run).
8. **`cqs watch` does not respect `.gitignore`** — filed as #1002 (full fix) + #1003 (short-term `.claude/` hardcoded skip). `cqs index` uses the `ignore` crate correctly; only the watch loop is the gap. Blew up the index to 96k chunks mid-session from auto-indexed agent worktrees.
9. **Watch-mode skips SPLADE for incremental updates** — new/changed chunks get embeddings but no sparse vectors. Discovered this session: coverage dropped to 70% after agent-worktree-triggered reindexes. Full `cqs index` run brings it to 100%. Known ROADMAP item: "Daemon: incremental SPLADE in watch mode."
10. **The "BGE-large fully routed" label in tears was wrong** — it was dense-only. Triggered a 4-hour wild-goose chase reverting perfectly-good perf PRs. Always check the `results.json` config name field, don't trust the tears label.
11. **SPLADE alphas tuned on dirty index hurt on clean index** — confirmed this session. At default per-category alphas, SPLADE-enabled R@1 is 9pp below dense-only R@1. Next step: 21-point sweep + per-category re-fit.

## Open Issues (26)

Tier-1 remaining: **none** (all Wave D/E/F Tier-1s closing as PRs merge). Tier-2 remaining focus: #63, #916/#917/#921 (SPLADE perf trio), #956 (Metal/ROCm), #957 (SPLADE preset registry), #980 follow-through. Tier-3 remaining: #255 reference packages + 15 minor perf/refactor/test items + blocked-upstream (#106 ort RC, #717 HNSW mmap).

Filter: `gh issue list --state open --label tier-N`

## Architecture notes

- Deterministic search + deterministic eval (end-to-end)
- SPLADE always-on (model at `~/.cache/huggingface/splade-onnx/`, hash-identical to `~/training-data/splade-code-v1/onnx/model.onnx`)
- HNSW dirty flag self-heals via per-kind checksum verification
- cuVS 26.4 patched with `search_with_filter` + `add-serialize-deserialize` (for #950)
- CAGRA persistence: `.cagra` + `.cagra.meta` sidecar (blake3) + `CQS_CAGRA_PERSIST` toggle
- `atomic_replace(tmp, final)`: fsync tmp → rename → fsync parent → EXDEV copy fallback
- Migration fs-backup: `index.db.bak-v{from}-v{to}-{ts}.db` via atomic_replace, last-2 pruning (#953)
- ModelConfig: InputNames + PoolingStrategy + output_name drive ONNX inputs/output (#949)
- Store typestate: `Store<ReadOnly>`/`Store<ReadWrite>`; write methods on `impl Store<ReadWrite>` (#946)
- `open_readonly_after_init`: closure-based fixture setup, SQLite-level RO guarantee (#986)
- `open_readonly_small`: 16MB mmap for reference indexes (#970)
- Shared `Arc<Runtime>` across Store+EmbeddingCache+QueryCache (daemon threads via `cqs-shared-rt` pool, #968)
- NameMatcher ASCII fast path (#965), reindex owning iterator (#967), classifier Aho-Corasick (#964)
- `cqs::fs::atomic_replace` helper (#948) + `INDEX_DB_FILENAME` constant (#923)
- CI slow-tests gate + nightly workflow (#980 partial, #988)
- Daemon (`cqs watch --serve`), thread-per-connection capped at 64 (SEC-V1.25-1)
