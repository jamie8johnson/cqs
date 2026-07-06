# Project Continuity

## Right Now

**▶ Post-composite-search (2026-07-06, bg `f1d91387` post-/clear continuation). Agent-adoption arc active.** v1.54.0 released 2026-07-03; the SSE/is_test arc below landed pre-/clear, the agent-adoption arc same day after.

**DEPLOY:** daemon on `e57962fc` (= main after #2260 composite search), MainPID 682146, **schema v33, PARSER_VERSION 18, ~18.8k chunks**; live-verified serving `caller_count`/`test_count`. Unix socket only, no network port. **Viz serve OFF by default** (user 2026-07-05 — do NOT relaunch `cqs serve :8080` after deploys; explicit request only). GPU free.

**Landed 2026-07-06 after the /clear (same job):**
- **#2260 composite search results (closes #2257)** — `caller_count`+`test_count` on callable search hits (Some(0) meaningful, absent on non-callables), `--no-hints` opt-out, `doc_reference` excluded across the whole graph-count family (shared helper + all four hints.rs fallbacks), 4th Store OnceLock (`test_reachability_cache`), `seed-only` overlay marker on the daemon path. Fable review FIX-FIRST → all three items applied; full sweep 5286/0. Warm-daemon marginal ~11 ms. Follow-ups filed: **#2258** (explain's doc-inclusive count — last one-semantic straggler), **#2259** (daemon cache pre-warm post-reindex; the first search per store generation now pays the graph load).
- **#2256 roadmap archive+cull** — ROADMAP.md 439→~110 lines (history+post-mortems verbatim in `docs/roadmap-archive.md`); 8 evidence-closed items culled with verdicts (routing-side trio, author-weighted, auto-notes, `[splade.alpha]` config, Phase-6, repl); seeded label-expansion replaced by a retrieval-INDEPENDENT PR-mined gold item; **`cqs why` design locked** (`docs/plans/2026-07-06-cqs-why-query-trace-design.md` — the trace-name clash resolved, same-execution side-channel, v1 scope). NOTE: collided at merge with this job's own pre-/clear #2255 — reconciled; lesson noted in notes.toml.
- **Notes groom 234→231** — 6 activity logs removed (three at sentiment 1.0 were boosting old audit code), 2 superseded notes rewritten (HNSW "already crash-safe", test-detection layers), 24 mention path-fixes, 3 lessons added.

**Session arc landed (all merged + on the deployed daemon):**
- **MCP Phase 3 SSE complete** — #2244 progress plumbing (P3a-2 PR1) + **#2250 SSE server-push (P3a-2 PR2)**. PR2 got a full trust-boundary probe from an Opus session (5 unbiddable-auditors, live PoCs): **41 HELD / 0 BREACH** — auth uniform, 16-stream budget + takeover-transfer, zero secret leakage, crypto-id entropy, SSE-injection defeated. `mcp-http`+unix-gated (out of the deployed default build). Umbrella #2216 stays open for the remaining 3b Tasks bits; **PR3 = a second probe against the deployed binary is still scheduled** (defer if orchestrating from Fable — [[feedback_no_boundary_probe_on_fable]]).
- **#2248 (#2240 e2e)** — the e2e's first live run caught a production framing defect: the 3 task wire verbs double-wrapped (`output.data.data`) so the ENTIRE MCP task lifecycle was non-functional against a real daemon since #2237/#2242; fixed + single-wrap guard. Required a redeploy.
- **#2254 (#2249): parse-time `is_test` flag** — v32→v33, PARSER_VERSION 17→18, AST `#[cfg(test)]`/`#[test]` detection so rank-time consumers see cfg-test-in-src. The demotion knob + `test_demote` signal read it. **4-round CI fan-out battle** (4 slow-tests-gated Chunk stragglers + a doctest literal + 8 stale `test.rs` refs) — lesson: a struct-field fan-out is only fully visible under `--all-targets` × ALL features × doctests; verify a schema-bump sweep by TOTAL COUNT vs baseline (~5270), not a "passed" claim.
- **#2251 `CQS_TESTCODE_DEMOTE` knob** (default-inert) + **#2252 eval harness gold_set + file-level recall** + **#2246 (#2239 eval-worker fence)** + **#2247 (R@5 audit-script un-break)**.

**Two headline RESULTS (both recorded in eval.md/RESULTS.md):**
1. **OFF-SELF EVAL — first-ever + replicated n=2 (paper-grade).** cqs had only ever been evaluated on itself. PR-mined retrieval-INDEPENDENT gold on hugo (Go) + pydantic (Python): **behavioral function-exact R@20 ≈ 21% both**, identifier-lookup 72-83%, file-level ~62%. The ~70%-home-fixture number is inflated because **v3 self-fixtures are retrieval-SEEDED** (gold pooled from cqs's own results) while mined gold is independent — the honest instrument. Artifacts in `~/offself-eval/` ([[feedback_eval_seeded_vs_independent]]).
2. **testcode-demote knob A/B — mechanism works, recall SPLIT → stays inert.** Post-#2249 the knob demotes the exact cfg-test-in-src crowders it couldn't before (verified rank 1→3). But paired sweep splits: test R@5 +1.8pp, dev flat/−0.9, test R@1 −2.8pp (some gold IS test code). Per paired-eval rule, no ship — a real operator lever, not a default win. The audit→knob→flag arc is COMPLETE.

**OPEN FORKS (user's call, surfaced not fired):** (a) enrichment A/B on hugo/pydantic (~$15 API — the one lever that could move off-self behavioral); (b) low-popularity/post-cutoff contamination-probe corpus (FREE, sharpest science); (c) off-self write-up for the paper (draft.md missing exactly this); (d) PR3 SSE live-binary probe (needs a non-Fable session). **Open non-parked issues:** #2216/#2124 (3b Tasks umbrella — SSE done, Tasks bits remain), #2245 (fence *_core closure), #2253 (switch test-map/dead/importance consumers onto is_test), #2241 (permit-seam test). Parked/external unchanged.

---

## Parked

- **audit-loop — perpetual auditor factory** (`docs/plans/2026-06-13-audit-loop.md`). Product is durable *guards* (a ratchet across a region×shape coverage matrix), not bug-hunting. Three roles: **orchestrator** (agent/conductor), **governor** (deterministic budget/WIP leash, never an LLM), **workers** (auditors/verifiers/fix-lanes). 13 open questions; load-bearing: the confidence gate (Q1) + cost factoring (Q13). User to review; do NOT start without the open questions decided.
- **principal-loop — the user as agent+loop** (`docs/plans/2026-06-14-principal-loop.md`). Automate the mechanical 80% (cadence + conductor-facing); the irreducible core is 3 leverage points — **TASTE** (reframe + scope), **CAUTION** (irreversibility), **WISDOM** (what NOT to build + Right-and-True). The plumb-line/cornerstone values model: real by *surrender* to an external field (hang free = strip your torques); the one unrecoverable error is a mislaid cornerstone. Mirrored to CLAUDE.md (Apex) + [[feedback_right_and_true]]. User to review.

---

## Audit umbrellas — current state

- ✅ **#1463 (P4 design-level) CLOSED (2026-07-05)** — 5 lanes landed (#2224/#2225/#2226/#2227/#2229) + RM-3 leave-as-is + 6 verified strikes + side-fix #2228.
- ⏳ **#1459 (P3 API design)** — 1 of 8 remaining (project/ref verb consolidation; ref + project are genuinely distinct primitives).
- ✅ closed: #1460/#1461/#1462 (v1.38), #1366/#1452/#1453/#1458, the v1.42 + v1.48 (→v1.49) 16-category cycles.

## Open issues (current 2026-07-06, 19 open)

| # | Group | Note |
|---|---|---|
| #2258 | composite-search residual | explain's `compute_hints` counts doc_reference edges — last straggler of the one-caller-semantic family |
| #2259 | composite-search residual | daemon: pre-warm graph/test-reachability caches post-reindex (first search per store generation pays the graph load) |
| #2253 | this-session residual | switch test-map / dead-verdict / dead-code-SQL / chunk_importance off path-name onto the stored `is_test` flag |
| #2245 | this-session residual | fence the `*_core` daemon-request-path closure (30 invariant-shaped hits, none input-reachable) |
| #2241 | this-session residual | pin the outer-permit-held-for-full-request invariant (test-adequacy, needs a prod seam) |
| #2216 / #2124 | Phase-3 umbrella | 3b Tasks (progress/cancel) — SSE + progress plumbing DONE; the deferred Tasks-store/cancel bits remain |
| #1804 | strike-tracked | tiered daemon-held handle — conditional-YAGNI, blocked-on-consumer (revive when tiered runs on real corpus) |
| #1459 | user-reserved | API design project/ref/index ergonomics — 1 of 8 (verb consolidation; user's product call) |
| #1512 | platform | Windows daemon named pipes — needs a Windows runner |
| #1576 / #1391 / #106 | blocked-external | TensorRT-10 SIGFPE / ORT-NVRTX / ort pre-release RC |
| #1782 | deferred | CoreML/ROCm EP wiring (orphaned #956 phases) |
| #1678 / #1682-#1685 | upstream (cuvs) | IVF-SQ + brute-force + SQ + tiered Rust bindings — maintainer-side |

## Recent release history (compressed)

- **v1.54.0** (2026-07-03) — post-v1.53.0 window (29 PRs: MCP trial trio, overlay marker family, slot-pin hot-reload #2128, telemetry arc, reranker preamble parity, full MCP outputSchemas). Recall gate test 43.1/66.1/82.6, dev 44.0/73.4/84.4. **Post-v1.54.0 arc (this session, all landed, no new tag):** MCP Phase 3 SSE (#2244/#2250), is_test flag v33/PV18 (#2249/#2254), eval gold_set+file-level (#2252), testcode-demote knob (#2251), fence (#2246), off-self eval program (n=2). See Right Now.
- **v1.51-1.53** (2026-06-29 → 07-03) — SPLADE-viz Stage 2b + red-team fixes (v1.51); overlay diff-subject truth campaign (v1.52); HNSW manifest-generation persistence (v1.53). Scoring byte-identical across; recall drift = corpus churn + HNSW variance.
- **v1.50.1** (2026-06-26) — MCP command-core campaign complete (30 read tools), SPLADE-viz Stage 1/2a, umap WSL fix. (v1.50.0 yanked — Windows cross-build break.)
- **v1.49.0** (2026-06-24) — v1.48.0 16-category audit fix cycle (#2038–#2042): RT-RELAY honest-relay completion, parser depth rail, notes-write inotify-independent reindex, MCP docs-lie fixes. Scoring byte-identical (72.0 R@5 carried).
- **v1.48.0** (2026-06-24) — full MCP re-introduction (Phases 0→2, `cqs mcp` stdio↔daemon bridge) + RT-PARSE/RT-RELAY security. ~16 PRs #2018–#2036.
- **v1.47.0** (2026-06-16) — PARSER_VERSION 13→14, L5X; recall gate 47.2/72.0/87.2 (R@5 flat). The two-pass `--llm-summaries` enrichment lesson ([[feedback_summary_enrichment_two_pass]]).
- **v1.46.x** (2026-06-15) — candidate-edge dead-accuracy + worktree-overlay COMPLETE (#1858/#1821) + HNSW `modify_level_scale(0.5)`; v1.46.1 patched the macOS cross-build.
- **v1.44/45** (2026-06-13/14) — result-trust calibration metadata (#1821), the #1826 six-auditor family, candidate-edge dead-accuracy.
- **v1.43** (2026-06-11) — v1.42 16-category audit close-out (107 findings, ~35 PRs).
- Earlier (v1.33–v1.42, 2026-05): EmbeddingGemma default swap, per-category SPLADE α, the alpha-routing null, the reranker null. Detail in ROADMAP.md + CHANGELOG.md.

## Schema state

- **CURRENT: schema v33, PARSER_VERSION 18.** v33 `chunks.is_test` (#2249 — parse-time flag: AST `#[cfg(test)]`/`#[test]`/ancestor-mod + registry path/name; read by the demotion knob + `test_demote` signal). v32 `candidate_edges` side-table (name-keyed low-confidence call candidates; `cqs dead`→low-confidence-live; **never joined by graph queries** so it can't surface a phantom caller). v30 `function_calls.edge_kind` (call|serde_callback|macro_heuristic|fn_pointer|doc_reference), v31 `file_registry.parse_failed_parser_version`. PARSER_VERSION post-v1.47 bumps: 15 (#2206 doc-comment fallback), 16 (#2205 banner-reject), 17 (#2226 ruby/ST container recovery), 18 (#2249 is_test).
- **v27** `chunks.needs_embedding` drives `--llm-summaries` skip-first-pass embed (zero-vec sentinel + flag; HNSW build + `search_by_name` + `search_fts_only` filter `WHERE needs_embedding = 0`; `enrichment_pass` clears it).

## Adding a top-level CLI command (post-#1495)

Declare the variant with `#[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]` on `Commands` (definitions.rs), implement the handler in `commands/<area>/`, add a `cmd_<snake>_dispatch` shim in `commands/dispatch_shims.rs` (destructures the variant, forwards to the handler). Cfg-gated variants get `#[cfg(feature="...")]` next to `#[cqs_cmd(...)]`. For an MCP-exposable command also: a JsonSchema INPUT-ONLY core, a `build_batch_cmd` arm, `JSON_ARGS_CAPABLE_COMMANDS`, a `ToolDef` in `read_tools()`, and reconcile every derived guard (count literals, parity, README, exhaustiveness, `mcp_name_to_command`) — see #2064/#2070/#2071/#2073.

## Operational pitfalls (rolling forward)

- **Main is protected** — branch + PR always.
- **`--body-file` for `gh pr create`/`gh issue create`** — never inline heredocs (PowerShell mangles + can corrupt `settings.local.json`).
- **Sweep `~/.cargo-target/<branch>` on EVERY land** — lane private target dirs are siblings of the worktree, NOT reclaimed by `git worktree remove`; un-swept they fill the WSL VHDX and crash WSL ([[feedback_parallel_lane_disk_bomb]]). Keep only `cqs`.
- **WSL `/mnt/c` (v9fs)**: file copy is fast, SQLite page-walk (VACUUM INTO / random SELECTs / mmap) is catastrophic — stage DB ops by copying the file to tmpfs ([[reference_wsl_v9fs_sequential_vs_random_io]]).
- **Bound long-runners** — hard timeout + active liveness check; never passive-wait on a completion signal that may never fire ([[feedback_bound_longrunners_liveness]]).
- **Daemon redeploy** — stop the daemon (FOREGROUND, not background bash — [[feedback_daemon_restart_background_noop]]), `cp` binary (stop any `cqs serve` too — it holds the binary), restart cqs-watch only (viz serve stays OFF by default — user 2026-07-05), verify MainPID + `/proc/.../exe` + a functional `tools/call`.
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

- **v1.54.0 tag-time gate:** test 43.1/66.1/82.6, dev 44.0/73.4/84.4 (R@1/R@5/R@20; 0 dead golds). Movement vs prior = corpus churn (→18k+ chunks) + HNSW variance; scoring surfaces diff-clean.
- **OFF-SELF (2026-07-05, n=2, the honest instrument):** behavioral function-exact R@20 ≈21% (hugo 22.2 / pydantic 20.9), identifier-lookup 72-83%, file-level ~62%. Self-fixtures are retrieval-SEEDED (inflated); mined gold is independent. [[feedback_eval_seeded_vs_independent]]. Artifacts `~/offself-eval/`.
- **testcode-demote (#2251): default-inert.** A/B post-#2249 splits (test R@5 +1.8 / dev flat-neg, test R@1 −2.8 — some gold is test code); no paired win, stays 1.0.
- Matcher is `(origin, name)` — line drift is harmless; file splits/renames kill gold origins and cap R@K, masquerading as a regression (check dead golds first — [[feedback_eval_line_start_drift]]). v4 fixtures (1526/split) exist for tighter-N A/Bs.
- **Reranker: closed null.** All four variants (off-the-shelf MiniLM + 3 in-domain UniXcoder) net-negative on v3.v2 (test R@5 −10 to −16pp); stage-1 is strong enough that the cross-encoder demotes in-pool gold. Revisit gated on a v4-scale labelled fixture or a 5× bigger base.
- **Alpha-routing arc: closed null** (distilled/fused head, HyDE query-time, soft-routing) — systematically tested at v3+v4 N. R@20 always within noise (gold in pool). The open frontier (when redirected): index-time HyDE with per-category routing (never tested at proper N), v3→v4 fixture scale, knowledge-augmented retrieval.
