# Project Continuity

## Right Now

**✅ v1.50.1 RELEASED (2026-06-26, autopilot/ultracode).** The post-v1.49.0 arc shipped as v1.50.0 (#2081), then **v1.50.1 (#2083) patched a Windows cross-build break and v1.50.0 was YANKED.** **Recall gate PASSED** (v1.50.0) — a same-corpus binary A/B (v1.49.0 binary vs current on the same index) returned IDENTICAL R@K, so scoring is byte-identical to v1.49.0; agg **47.2/70.7/86.7** (R@1/R@5/R@20) vs the prior 72.0 R@5 is corpus growth (16,925→17,523 chunks), 0 dead golds. Full suite + clippy clean. Daemon serves v1.50.1.

**⚠️ INCIDENT — v1.50.0 shipped Windows-broken, same-day v1.50.1 + yank.** The SPLADE-leg viz added an ungated `cqs::daemon_translate::daemon_socket_path(&cqs_dir)` in `serve.rs` (that fn is `#[cfg(unix)]`) → `x86_64-pc-windows-msvc` release build `E0425` → "Create GitHub Release" skipped (gated on all 3 builds) → AND `cargo install cqs` broke on Windows. **My miss:** the `/release` skill step 6 REQUIRES a pre-tag cross-build dry-run (and [[project_ci_linux_only_release_crossbuild]] named it) — I skipped it under release momentum; v1.46.0 was the same class. Recovery: cfg-gated the path computation (#2083, sibling of #2068), validated via the release.yml workflow_dispatch dry-run (all 3 green) BEFORE tagging v1.50.1, published, yanked v1.50.0. Also hit the `gh run watch --exit-status` masking gotcha (exit 0 hid the failed Windows job — read per-job `conclusion`). Memory sharpened. Headlines of the shipped arc:

- **MCP command-core campaign COMPLETE (#2021).** Every *decidable read command* is now a JsonSchema core + an unconditional MCP read tool — **30 read tools** (34 with `CQS_MCP_ENABLE_MUTATIONS=1`). Landed this session: `cqs_notes_list` (#2064), `cqs_suggest`+`cqs_impact_diff` (#2070), and — the two trust-boundary ones, each cleared by a 3-auditor security workflow (SAFE_EXPOSE: scan==relayed holds, no new risk class vs the already-exposed relay tools, the withhold was a stale pre-#2039 artifact) then an independent 3-adversary verification — `cqs_explain` (#2071) and **`cqs_context` (#2073)**. **#2021 now CLOSED** — only the correctly-withheld infra/mutating tail remains, by design. #2072 (the `///`-doc-marker `leading-directive` gap) **fixed in #2075** (`strip_leading_comment_marker`). A durable `canonical_docs_read_tool_count_matches_registry` guard now pins SECURITY.md/CONTRIBUTING.md/lib.rs counts to the registry (the count straggler recurred twice — caught only by the adversarial verification — now guarded). **Follow-on landed: #2043 type-map single-source (#2077)** — schema/validate/dispatch now declare each command's core type once, with a `precheck_type_agrees_with_dispatch` guard + a `every_tool_table_command_has_a_core_map_row` totality guard (the latter added in response to the code-review finding that the macro's `_` fallback re-opened a silent-drift hole).
- **SPLADE-on-embedding viz (LIVE).** Stage 1 backend (`/api/search_legs`, dense/sparse/fused legs, #2060) + Stage 2a query-anchored mechanism step-through on the three.js cluster plane (#2065). Design locked after 2 adversarial passes (`docs/plans/2026-06-25-splade-viz-design.md`). **Next: Stage 2b** = the eval-gold "where hybrid wins" R@K-delta panel + stratified tour (the payoff); Stage 2c = deck.gl + token-teaching layer.
- **Orthogonal-auditor pass + 3 design-forks (#2056–#2061):** Fork 1 emit-convention skip-when-default (#2056), the **spec-fidelity-auditor** formalized (#2057, the sextet's null-of-the-meta-null), BUG 1 RT-RELAY straggler (#2058), BUG 2 git-diff `--relative` frame (#2063), BUG 3 slot ramp-up fail-closed (#2062), test-hardening consolidation (#2061). Fork 2 → tracked #1992.
- **`cqs index --umap` made to work on WSL `/mnt/c` (#2067 + #2069):** it was (a) silently dropped under a running daemon and (b) hanging **9h20m** on v9fs (random-page SQLite IO). Now runs CLI-side on the delegation path + stages the read through a **fast sequential `fs::copy`** to tmpfs (VACUUM INTO was itself the v9fs pathology — my spec error, corrected in #2069) + a fit-subprocess timeout. **9h20m → ~40s, full coverage.** New memories: [[reference_wsl_v9fs_sequential_vs_random_io]], [[feedback_bound_longrunners_liveness]].
- **serve Windows release-build regression I introduced via #2060, fixed (#2068):** `serve` (default feature) used `std::os::unix::net` ungated → E0432 on `x86_64-pc-windows-msvc`. cfg-gated.

**⚠️ WSL DISK-BOMB CRASH + RECOVERY (2026-06-25).** Mid-session WSL crashed: `~/.cargo-target` hit **375G** (15 un-swept lane private target dirs at `~/.cargo-target/<branch>` — siblings of the worktrees, NOT reclaimed by `git worktree remove` — + 161G `cqs/debug` bloat). Recovered ~367G (375G→7.6G, disk 56%→18%); deleted 49 stale local branches (remote was already clean); nothing merged lost. Lesson sharpened in [[feedback_parallel_lane_disk_bomb]]: `rm -rf ~/.cargo-target/<branch>` on EVERY land, separate from worktree removal — now done.

**DEPLOY STATE:** main `44dd7e6f` (v1.50.0). Release binary rebuilt (v1.50.0) and being installed → daemon restart to serve v1.50.0 (30 MCP tools). Index fully re-enriched (0 summary-unenriched; `--force` applied 2,447 cached summaries, 0 API spend). The viz `cqs serve` is DOWN (killed by the earlier crash) — restartable on request (`CQS_SERVE_IDLE_MINUTES=0 cqs serve --no-auth`, then `localhost:8080/?view=cluster`).

**QUEUE STATE: drained → released.** This session closed **#2025** (embedded-url base-rate 1.13%, premise refuted), **#2021**, **#2043** (#2077), **#2079** (#2080 — parser stack-floor recalibration; the floor-coupling now actually holds); landed **#2076** (daemon over-cap EPIPE de-flake), **#2078** (parser walk completeness guard), then **cut v1.50.0** (#2081). Adversarial verification earned its keep 5× (each landing's incomplete-sweep straggler — docs-count ×2, the EPIPE, the macro totality gap, the README clamp-contract docs-lie — caught by a code-review/sweep pass, never the lane's own green tests). Natural next: **viz Stage 2b** (designed feature, parked). Scoping-required: #2027 residual 1 (per-grammar scanner sandboxing — big security lane), design umbrellas (#1463/#1459/#1969/#1987/#1512), #1992 (Windows). Blocked-external: cuvs/ort/TensorRT. If dry: /archeo or /docs-review. **Parked:** autotune-α (`docs/plans/2026-06-25-autotune-alpha-design.md`), viz Stage 2b/2c.

---

## Parked

- **audit-loop — perpetual auditor factory** (`docs/plans/2026-06-13-audit-loop.md`). Product is durable *guards* (a ratchet across a region×shape coverage matrix), not bug-hunting. Three roles: **orchestrator** (agent/conductor), **governor** (deterministic budget/WIP leash, never an LLM), **workers** (auditors/verifiers/fix-lanes). 13 open questions; load-bearing: the confidence gate (Q1) + cost factoring (Q13). User to review; do NOT start without the open questions decided.
- **principal-loop — the user as agent+loop** (`docs/plans/2026-06-14-principal-loop.md`). Automate the mechanical 80% (cadence + conductor-facing); the irreducible core is 3 leverage points — **TASTE** (reframe + scope), **CAUTION** (irreversibility), **WISDOM** (what NOT to build + Right-and-True). The plumb-line/cornerstone values model: real by *surrender* to an external field (hang free = strip your torques); the one unrecoverable error is a mislaid cornerstone. Mirrored to CLAUDE.md (Apex) + [[feedback_right_and_true]]. User to review.

---

## Audit umbrellas — current state

- ⏳ **#1463 (P4 design-level)** — truly-remaining items are big or platform-blocked: API-V1.38-6 (Cli-flag→subcommand parity, clap conflict), DS-V1.38-4 deeper hazard (HNSW half-renamed-set), PL-V1.38-2 (SPLADE Windows umask), TC-HAP-V1.38-3 (`enrichment_pass` untested). 12 P4 carry-overs tracked separately.
- ⏳ **#1459 (P3 API design)** — 1 of 8 remaining (project/ref verb consolidation; ref + project are genuinely distinct primitives).
- ✅ closed: #1460/#1461/#1462 (v1.38), #1366/#1452/#1453/#1458, the v1.42 + v1.48 (→v1.49) 16-category cycles.

## Open issues (current 2026-06-25)

| # | Status | Why open |
|---|---|---|
| #2027 | deferred (residual 1) | per-grammar external-scanner sandboxing (rlimits+seccomp / cargo-fuzz) — big separate lane; residuals 2+3 done (#2078) |
| #2079 | P4 | parser walk-depth rail / floor-stack margin recalibration — not a shipping DoS (release + 2 MiB default safe) |
| #1992 | platform | Windows dead-code-on-sibling-target sweep + re-add cross-build gate — needs a Windows toolchain / CI-iteration |
| #1969 | architecture | overlay daemon same-uid TOCTOU + fd residuals — airtight = kernel peer-cwd redesign (deferred LOW) |
| #1987 | enhancement | daemon panic resilience — real unwind isolation deferred (panic=unwind tradeoff) |
| #1459 | umbrella | API design — 1 of 8 (project/ref verb consolidation) |
| #1463 | umbrella | P4 design-level — see umbrella state above |
| #1512 | platform | Windows daemon named pipes — needs Windows runner |
| #106 / #1391 / #1576 / #1678-#1685 | blocked-external | ort RC / TensorRT / NVRTX / cuvs upstream chains |
| #255 / #717 / #1043 / #1139 / #1140 / #1350 / #1351 | tier-3 / deferred | infra/platform/architecture; see prior triage |

## Recent release history (compressed)

- **v1.49.0** (2026-06-24) — v1.48.0 16-category audit fix cycle (#2038–#2042): RT-RELAY honest-relay completion, parser depth rail, notes-write inotify-independent reindex, MCP docs-lie fixes. Scoring byte-identical (72.0 R@5 carried).
- **v1.48.0** (2026-06-24) — full MCP re-introduction (Phases 0→2, `cqs mcp` stdio↔daemon bridge) + RT-PARSE/RT-RELAY security. ~16 PRs #2018–#2036.
- **v1.47.0** (2026-06-16) — PARSER_VERSION 13→14, L5X; recall gate 47.2/72.0/87.2 (R@5 flat). The two-pass `--llm-summaries` enrichment lesson ([[feedback_summary_enrichment_two_pass]]).
- **v1.46.x** (2026-06-15) — candidate-edge dead-accuracy + worktree-overlay COMPLETE (#1858/#1821) + HNSW `modify_level_scale(0.5)`; v1.46.1 patched the macOS cross-build.
- **v1.44/45** (2026-06-13/14) — result-trust calibration metadata (#1821), the #1826 six-auditor family, candidate-edge dead-accuracy.
- **v1.43** (2026-06-11) — v1.42 16-category audit close-out (107 findings, ~35 PRs).
- Earlier (v1.33–v1.42, 2026-05): EmbeddingGemma default swap, per-category SPLADE α, the alpha-routing null, the reranker null. Detail in ROADMAP.md + CHANGELOG.md.

## Schema state

- **CURRENT: schema v32, PARSER_VERSION 14.** v32 `candidate_edges` side-table (name-keyed low-confidence call candidates; `cqs dead`→low-confidence-live; **never joined by graph queries** so it can't surface a phantom caller). v30 `function_calls.edge_kind` (call|serde_callback|macro_heuristic|fn_pointer|doc_reference), v31 `file_registry.parse_failed_parser_version`. PARSER_VERSION post-v1.45 bumps: 11 (#1888 L5X byte_start), 12 (#1955 L5X wire-up), 13 (#1967 Dart call_query), 14 (v1.47.0 L5X).
- **v27** `chunks.needs_embedding` drives `--llm-summaries` skip-first-pass embed (zero-vec sentinel + flag; HNSW build + `search_by_name` + `search_fts_only` filter `WHERE needs_embedding = 0`; `enrichment_pass` clears it).

## Adding a top-level CLI command (post-#1495)

Declare the variant with `#[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]` on `Commands` (definitions.rs), implement the handler in `commands/<area>/`, add a `cmd_<snake>_dispatch` shim in `commands/dispatch_shims.rs` (destructures the variant, forwards to the handler). Cfg-gated variants get `#[cfg(feature="...")]` next to `#[cqs_cmd(...)]`. For an MCP-exposable command also: a JsonSchema INPUT-ONLY core, a `build_batch_cmd` arm, `JSON_ARGS_CAPABLE_COMMANDS`, a `ToolDef` in `read_tools()`, and reconcile every derived guard (count literals, parity, README, exhaustiveness, `mcp_name_to_command`) — see #2064/#2070/#2071/#2073.

## Operational pitfalls (rolling forward)

- **Main is protected** — branch + PR always.
- **`--body-file` for `gh pr create`/`gh issue create`** — never inline heredocs (PowerShell mangles + can corrupt `settings.local.json`).
- **Sweep `~/.cargo-target/<branch>` on EVERY land** — lane private target dirs are siblings of the worktree, NOT reclaimed by `git worktree remove`; un-swept they fill the WSL VHDX and crash WSL ([[feedback_parallel_lane_disk_bomb]]). Keep only `cqs`.
- **WSL `/mnt/c` (v9fs)**: file copy is fast, SQLite page-walk (VACUUM INTO / random SELECTs / mmap) is catastrophic — stage DB ops by copying the file to tmpfs ([[reference_wsl_v9fs_sequential_vs_random_io]]).
- **Bound long-runners** — hard timeout + active liveness check; never passive-wait on a completion signal that may never fire ([[feedback_bound_longrunners_liveness]]).
- **Daemon redeploy** — stop the daemon (FOREGROUND, not background bash — [[feedback_daemon_restart_background_noop]]), `cp` binary (stop any `cqs serve` too — it holds the binary), restart, verify MainPID + `/proc/.../exe` + a functional `tools/call`.
- **CI watch** — pin the run ID (`gh pr checks --watch` latches the previous run); use /land. `gh pr merge --auto` is disallowed; merge manually on green.
- **Squash-merge + rebase trap** — cherry-pick onto fresh main. **`cargo publish`**: plain (no features); cqs-macros first; `evals/` excluded.
- **The read-tool count lives in 4 docs** (SECURITY.md/README/CONTRIBUTING/lib.rs) — the `canonical_docs_read_tool_count_matches_registry` guard now pins it; the README list has its own guard.
- **`enumerate_files` returns relative paths** — join with project root before `parse_file()`. **`type_edges`** tracks signature-level uses only (params/returns/fields), not expression-level.

## Collaboration calibration (still load-bearing)

1. **Self-starter / self-orienter is favored** — action over consultation when the next move is clear; on autopilot, execute the most likely option.
2. **"Little give-ups" are the failure pattern** — verify artifacts, investigate silences, redo thin returns, don't trust a green test as truth.
3. **No time estimates in specs** — what/why/gate-criteria, not effort.
4. **Don't suggest ending a session** — 1M context; the user works continuously.
5. **Non-trivial / trust-boundary decisions → adversarial pass first** ([[feedback_auditor_attack_before_decisions]]); under ultracode, orchestrate it as a workflow.

## Eval baselines

Canonical: `evals/queries/v3_{test,dev}.v2.json` (109q each, refreshed 2026-04-25 / PR #1109). Full writeups in `~/training-data/research/eval.md`; cite numbers from there, never memory.

- **v1.49.0 gate (carried from v1.47.0, scoring byte-identical since):** test 46.8/67.9/84.4, dev 45.9/74.3/88.1. R@5/R@20 flat across v1.47→v1.49 (scoring untouched; corpus churn only).
- Matcher is `(origin, name)` — line drift is harmless; file splits/renames kill gold origins and cap R@K, masquerading as a regression (check dead golds first — [[feedback_eval_line_start_drift]]). v4 fixtures (1526/split) exist for tighter-N A/Bs.
- **Reranker: closed null.** All four variants (off-the-shelf MiniLM + 3 in-domain UniXcoder) net-negative on v3.v2 (test R@5 −10 to −16pp); stage-1 is strong enough that the cross-encoder demotes in-pool gold. Revisit gated on a v4-scale labelled fixture or a 5× bigger base.
- **Alpha-routing arc: closed null** (distilled/fused head, HyDE query-time, soft-routing) — systematically tested at v3+v4 N. R@20 always within noise (gold in pool). The open frontier (when redirected): index-time HyDE with per-category routing (never tested at proper N), v3→v4 fixture scale, knowledge-augmented retrieval.
