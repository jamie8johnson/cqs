# Project Continuity

## Right Now

**v1.25.0 + 11th audit merged. Wave A in flight. Waves B/C/D queued for autopilot. (2026-04-14 ~23:45 CDT)**

Main at `ea7c72b` (#976 audit batch). Working branches in flight:
- **#978** `chore/post-audit-tears` — this tears update (will merge once CI green)
- **#979** `wave-a/quick-wins` — Wave A fixes (#961 WSL mmap, #962 CAGRA itopk, #963 reranker batch)

### What landed today (massive day)

**Morning — v1.25.0:**
- #942 determinism fixes (hash iteration + SPLADE-at-α=1.0 + rowid re-sort)
- #943 eval-output-location fix (watch-reindex contamination — root cause of 2 days of drift)
- #944 v1.25.0 release — new per-category alpha defaults + multi_step router fix
- #945 notes mutation daemon bypass

**Afternoon — 11th full audit:**
- 16 categories × 2 batches × 8 parallel opus auditor agents → **236 findings**
- Triaged into P1 (49) / P2 (47) / P3 (97) / P4 trivial (16) / P4 issues (32)
- Executed in 3 waves of implementer agents (wave 1 shared tree, 2+3 worktree-isolated)
- **PR #976 merged 20:40 CDT** — 126 commits closing 166/236 findings
- **PR #977 merged 19:33 CDT** — #856 atexit Mutex UB fix (dependent follow-up)
- 11 stale issues closed, 25 P4 issues filed (#951-#975), 5 refactor issues (#946-#950)
- `cqs-training` pushed (research/sparse.md clean-sweep data + 15 scripts + gitignore cleanup)

**Evening — Wave A kickoff:**
- 3 worktree-isolated opus agents, all produced 1-commit branches:
  - `fix/961-wsl-mmap-autodetect` — detect 9P/DrvFS/NTFS/CIFS, force `mmap_size=0` unless user override
  - `fix/963-reranker-batch` — `CQS_RERANKER_BATCH` chunking (default 32), mirrors `embed_documents`
  - `fix/962-cagra-itopk-env` — `CQS_CAGRA_ITOPK_MIN/MAX`, `GRAPH_DEGREE`, `INTERMEDIATE_GRAPH_DEGREE` with corpus-size log₂ scaling
- Bundled into **PR #979** `wave-a/quick-wins`.

### CI caveat — slow integration tests

A CI `test` job cancelled at 1h48m during the audit-PR merge sequence because I assumed it was hung. **It was not.** `tests/cli_health_test.rs` has pre-existing CLI integration tests (`test_health_cli_text` ≈ 303s each) that shell out to `cqs` and cold-load the whole ONNX/HNSW/SPLADE stack per invocation. Normal test-job time on this repo is ~22 min.

**Filed #980** (`tier-2 / performance / testing`) with the in-process-fixture fix proposal. Today's new tests (`cli_notes_test.rs`, `router_test.rs`) use the correct in-process pattern and are fast (0.16s).

**Do not cancel running CI under 30 min without a specific hang signal.**

### Wave-merge caveat

My first two merge scripts printed "MERGED" unconditionally without checking `gh pr merge` exit status. Three of the claimed merges didn't happen — branch-protection rejected them because:
1. Cancelled runs leave stale "fail" status that blocks required checks
2. Re-triggered CI creates fresh runs but the old statuses can linger
3. `gh pr merge --admin` overrides "fail" / "stale" but **not "in progress"** — if any required check is still running, even admin is blocked

Fix pattern:
1. Always check `$?` after `gh pr merge`
2. Empty commit (`git commit --allow-empty`) retriggers fresh CI that supersedes stale statuses
3. `--admin` is available (I'm authenticated as repo owner); use it only when a stale status is the real blocker, not to bypass actually-failing tests

### Next session — pull list queued, autopilot contract

**Wave B** (1 agent, bigger refactor):
- #947 Commands/BatchCmd unification — half-day refactor, kills daemon/CLI parity drift class. Touches `src/cli/definitions.rs`, `src/cli/dispatch.rs`, `src/cli/batch/`.

**Wave C** (4 parallel worktrees):
- #946 Store typestate (closes write-on-readonly class)
- #948 `atomic_replace` helper (closes fs-persist durability class)
- #949 Model abstraction (unblocks BGE→E5 default switch)
- #950 CAGRA persistence (daemon hot-restart 30s → 5s)

**Wave D** (2 parallel worktrees):
- #972 Daemon `try_daemon_query` test scaffold
- #964 Aho-Corasick `classify_query` (pre-req for classifier accuracy work)

Order: A → B → C → D sequential between waves (later waves benefit from earlier fixes); parallel within each. Rebuild + install binary after each wave merge so daemon uses current code.

### Architecture state

- **Version:** v1.25.0, Schema v20
- **Binary:** last rebuilt + installed from post-merge main (`ea7c72b`), daemon active
- **Index:** clean (post-GC, 13,279 chunks down from 81%-dup 69,444)
- **Per-category SPLADE α defaults:** identifier 0.90, structural 0.60, conceptual 0.85, behavioral 0.05, rest 1.0 (tuned on *dirty* index; re-fit pending — CPU Lane)
- **Determinism:** end-to-end after #942 + #943 + wave-1 sort hardening (15+ sites)
- **Test count:** 1404 lib tests pre-wave-A; +new Wave A tests (WSL mountinfo parser etc.)

### Eval numbers (honest, post-clean-index)

| Eval | Model | R@1 | R@5 | R@20 |
|---|---|---|---|---|
| V2 (265q clean) | BGE-large fully routed | 37.4% | 55.8% | 77.4% |
| V2 (265q clean) | E5 v9-200k fully routed | 37.4% | 56.6% | 78.1% |
| V2 (265q clean) | Oracle per-category α | 49.4% | — | — |

**E5 v9-200k ties BGE on R@1, slight edge on R@5/R@20 — at 1/3 the embedding dim.** Gated on #949 (model abstraction) for low-friction default switch.

Pre-2026-04-14 numbers (44.9% R@1 etc.) were measured against the dirty (81% worktree-dup) index; see GC prune_all suffix-match bug fixed in wave 1.

### Residual puzzles

- **Classifier accuracy** — 4.5pp oracle gap entirely in `classify_query()`, not alpha picks. Today: negation 100%, identifier 84%, structural 19%, behavioral 5%, conceptual 3%, cross_language 0%. Wave D #964 (Aho-Corasick) is a pre-req; actual investigation + centroid-matching-on-BGE-embeddings proposed in ROADMAP.
- **Alpha defaults on clean index** — today's defaults were fit on dirty data. Re-sweep on clean infra to find real optima.

## PR status

- #978 tears (this one) — open, CI running
- #979 Wave A quick-wins — open, CI running
- #980 slow-CLI-tests issue — filed 2026-04-14 evening

## Open Issues (40, tiered)

- **Tier 1 (11)** — fix-worthy near-term: #946-#950 refactors, #953 migration backup, #961 WSL mmap (→ Wave A), #962 CAGRA itopk (→ Wave A), #963 reranker batch (→ Wave A), #972 daemon tests
- **Tier 2 (10)** — real impact, harder: #956 Metal/ROCm, #957 SPLADE/reranker presets, #964 Aho-Corasick, #968 shared runtime, #973 dispatch_search tests, #63 paste RUSTSEC, #916 mmap SPLADE, #917 streaming SPLADE, #921 WSL SPLADE save, #923 INDEX_DB_FILENAME, #980 CLI test perf
- **Tier 3 (19)** — low urgency/blocked upstream: #106 ort RC, #389 CAGRA CPU retain, #717 HNSW RAM, #255 reference packages, plus 15 minor perf/refactor/test items

Filter: `gh issue list --state open --label tier-N`

## Architecture notes
- Deterministic search path + deterministic eval pipeline (end-to-end)
- SPLADE always-on, α controls fusion weight only
- HNSW dirty flag self-heals via checksum verification (per-kind after wave 2 AC-V1.25-8)
- cuVS 26.4 + patched with `search_with_filter` (upstream rapidsai/cuvs#2019)
- Eval results write to `~/.cache/cqs/evals/` (outside watched project dir)
- Daemon (`cqs watch --serve`), thread-per-connection capped at 64 (SEC-V1.25-1)
- WSL mmap auto-detect lands in Wave A (#961) — disables mmap on 9P/NTFS/CIFS
- CAGRA itopk env overrides with corpus-log₂ scaling land in Wave A (#962)
