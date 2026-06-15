# Project Continuity

## Right Now

**✅ v1.46.1 SHIPPED (2026-06-15) — patch fixing the macOS release build v1.46.0 broke.** Merged #1978, tagged `v1.46.1`, **release.yml cross-build ALL-GREEN incl. `aarch64-apple-darwin`** (GitHub Release v1.46.1 live with all 3 binaries + checksums — the complete release v1.46.0 couldn't produce), **crates.io 1.46.1 published** (max_version confirmed), daemon rebuilt to 1.46.1 + serving, index reindexed (16,836 chunks). main @ release commit, clean. **Next: `/idle` residual-issue loop.**
- **The bug:** v1.46.0 built Linux + Windows but `aarch64-apple-darwin` FAILED (`E0277` in `BatchView::resolve_overlay`: `#[cfg(unix)] let ops_root` took its only value from a `#[cfg(target_os="linux")]` arm `p.ops_path()`; on macOS all arms diverge → binding back-infers the unsized `Path` from its `&Path` use). The all-three-targets gate skipped "Create GitHub Release", so v1.46.0 = crate-but-no-binaries + a mac-uncompilable crate. Re-tagging would split source-from-binary → patch instead. Lesson recorded: [[project_ci_linux_only_release_crossbuild]] (PR CI is Linux-only; cross-build only on a v* tag).
- **Fix:** `let ops_root: std::path::PathBuf = …` annotation (`view.rs`) + targeted `allow(dead_code)` on `PinnedWorktree::fd` (`worktree.rs`) + `allow(unreachable_code, unused_variables)` on `resolve_overlay` for the macOS cfg. Verified via a scratch crate under `cargo check --target aarch64-apple-darwin` (original errors, fixed clean) AND the real darwin cross-build.
- **Sweep-auditor pass** (worktree-isolated) found 2 more warning-class siblings: `url_safe_for_cmd` (`serve.rs`) dead on macOS → `any(windows, linux)`; `SLOW_MMAP_FSTYPES`/`fstype_for_path`/`path_starts_with` (`store/mod.rs`) dead on Windows release → `any(unix, test)`. Rest cleared. Guard: `tests/platform_cfg_sweep_test.rs` (GREEN; RED on the unfixed shape). Superseded tears PR #1977 closed (folded into #1978).

**✅ v1.46.0 work shipped to crates.io (2026-06-15).** Merged #1976, tagged `v1.46.0`, **published to crates.io** (1.46.0). **PARSER_VERSION 10→13, schema 32 (unchanged) — a reindex is required for users** (drift-driven; cqs's own Rust corpus unaffected). Bundles the post-v1.45.0 idle loop (5 issues) + 3 audit rounds + the #1953 overlay security saga + #1952 score-frame rerank + 6 dependabot. Eval-number + PRIVACY docs synced. **Post-release cleanup DONE:** build artifacts 173G→44G (only `cqs` target dir remains); 11 merged local + 4 merged remote branches deleted; worktrees clean. Index reindexed to PV13, 16,830 chunks; daemon was rebuilt to 1.46.0 (now superseded by the 1.46.1 rebuild). Prior arcs all CLOSED: #1858 overlay epic + #1821, key-design (#1909/#1911), candidate-edge (#1933/#1934/#1936), level_scale (#1939).

**WHAT'S IN v1.46.0 (the post-v1.45.0 work, ALL MERGED + RELEASED). PARSER_VERSION 11→12→13.**
- **Idle loop (5 closed):** #1935 (#1942 candidate_edges watch write gap), #1937 (#1943 overlay candidate recompute → low-confidence-live), #1573 (#1945 test-fn `ChunkType::Test` + #1946 framework-trait `KNOWN_GAP`), #1944 (overlay contract guard, in #1945), #1888 (#1947 L5X file-relative byte_start, PV10→11 — injectivity sweep complete). `cqs dead --verdict dead` → 1 genuine entry (`rule_based_classify`), was 9. Archeo: clean.
- **Audit refill (3 rounds):** R1 — interleaving (clean, loom-verified daemon cache/epoch), legacy-state (clean + landed a v10→v32 full-chain guard #1950), property/HNSW (found the DotProduct panic #1949→#1951). R2 — seam/search (score-frame #1952), red-team (#1953 overlay bypass + #1954 dead-classifier steerability + #1955 **L5X custom parser dead on the production index path**). R3 — property/embedder (clean + 5 proptests + cagra/tiered clamp), seam/cross-project (#1966 trust-order at the cap/first-discovery boundary), sweep/per-language (#1967 **Dart call_query never wired** since #816).
- **Fixes merged:** #1951 (DotProduct `DistDotClamped`), #1954 (dead-classifier `ChunkType` gates + SECURITY.md note), #1955 (L5X wire-up, PV11→12), #1966 (cross-project trust-order), #1967 (Dart call_query + body-range, PV12→13), #1950 (migration-chain guard), #1971 (hardening: embedder proptests + cosine clamps + doc sync). Plus 6 dependabot bumps.
- **SECURITY SAGA #1953 (4 attempts, each bypass caught by review):** overlay-root validation — forward under-registry check (Attack A: forged gitdir) → back-pointer bind (Attack B: symlinked `.git`) → authoritative `git worktree list` membership (Attack C: TOCTOU swap) → **dirfd-pin** (`openat2 RESOLVE_NO_SYMLINKS` + git via `/proc/self/fd/N`, pins the inode). Merged (#1956), closing A/B/C + the **practical passive-hostile-content threat**. **Attack D (same-uid check-then-recreate race) + the non-CLOEXEC O_PATH fd → tracked low-severity #1969** (user-decided: merge best-effort; path-based validation is structurally racy against a same-uid filesystem attacker; the durable airtight redesign = kernel peer-cwd via SO_PEERCRED).

**#1858 WORKTREE OVERLAY — COMPLETE (#1858 + #1821 CLOSED).** Every graph-adjacent command reflects worktree edits in a `.claude/worktrees/` checkout: search + scout/gather/task (seed) + callers/callees + impact + dead + review. Read `_meta.overlay_graph`: `"full"` (search/callers/callees/dead), `"callers-only"` (impact + review — their affected-tests/transitive/risk sections stay parent-truth), `"seed-only"` (scout/gather/task — seed overlaid, BFS expansion parent-truth), absent (parent-truth early-return). Every marker gated on ACTUAL overlay participation, never `overlay.is_some()` (the seam-audit discipline). The "treat results as hints" re-read tax is RETIRED in CLAUDE.md. Lanes: PR1 #1908 (callers/callees), PR2 #1921 (impact/dead + #1910 dead-code edge-kind fix), PR3 #1924 (review). Known gap: `cqs ci`'s embedded review still parent-truth → #1926.

**KEY-DESIGN CLASS — CLOSED (the incomplete-sweep-of-the-hardening: a fix applied to one key but not its siblings).**
- **#1909 freshness** (PR #1917): one shared `FileIdentity` (dev/inode/size/mtime) + `DataVersionProbe`, adopted by ALL THREE index.db readers (primary + cross_project + references-LRU — two were stragglers on pure `(mtime,size)`). data_version parity for both siblings (`ref update` is in-place WAL → data_version is the real discriminator); freshness-key fields made private → sweep-by-construction.
- **#1911 chunk-id** (PR #1923): markdown whole-file/table id collision (P1) + window-suffix re-id blindness (P3). New `chunk_id_suffixed` (table chunks get `:t{idx}`); suffix routed through the constructor; re-id assert reads stored ids. **PARSER_VERSION 8→9 → reindex DONE (16610 chunks).**

**AUDITOR FINDINGS — all disposed:** #1914 (cross-project cache bare-counter epoch gate → WAL stale-serve race, LOOM-PROVEN; EpochCell tag + post-publish re-check), #1912 (neighbors limit unclamped → named-cap clamp), #1910 (dead-code `doc_reference` counted as a real caller → `is_real_caller` carve-out, folded into PR2), adequacy (name_match.rs scoring assertions VACUOUS — 24% mutant survival → 4 calibrated killing tests, #1918).

**APEX DOCS (#1915):** principal-loop sketch deepened with the **plumb-line / cornerstone** model — True = a *field* (read/checkable), Right = a *cornerstone* (laid/committed, not sensed); caution/taste/wisdom are *readouts* of one field-sense, not installable rules; the one unrecoverable error is a mislaid cornerstone (leaning-tower geometry → why values are least-delegable). Mirrored to CLAUDE.md Apex section + `feedback_right_and_true`. Also recorded `reference_fable_vocabulary` (the cqs vocabulary lineage: user laid the root *tears*; Fable extended the weave *seam/loom/magnets* + seeded orthogonal-nulls; user carried it across the block).

**9 PRs MERGED this session:** #1908, #1909 (PR #1917), #1914, #1915, #1918, #1920, #1921, #1923, #1924.

**INDEX STATE (live):** schema **32**, PARSER_VERSION **13** (11=L5X byte_start #1888, 12=L5X wire-up #1955/PR#1958, 13=Dart call_query #1967/PR#1970), ~16,830 chunks (Rust corpus unaffected by the Dart/L5X PV bumps), level_scale 0.5, EmbeddingGemma-300m, daemon active + serving, **binary current (PV13 build, crate ver 1.46.0 — RELEASED)**. ⚠️ **DON'T `rm index.db`** — nukes the llm_summaries cache; use `cqs index --force` ([[feedback_summary_cross_slot]]).
**RECALL (v1.46.0 RELEASE GATE, eval.md):** agg **48.7 / 72.0 / 87.6** (test 46.8/68.8/86.2, dev 50.5/75.2/89.0), **0 dead golds**. vs v1.45.0 47.7/72.0/88.5: R@5 flat, R@1 +1.0, R@20 −0.9 = PV13-reindex corpus churn (default scoring path byte-for-byte unchanged; the #1952 fix is rerank-only) — not a regression.
**SUBAGENT NESTING:** subagents can spawn subagents (CC v2.1.172); bg capped depth 5, fg unbounded ([[reference_subagent_nesting]]).

**OPEN (idle pool):** v1.46.0 shipped; **next = post-release cleanup, then the `/idle` loop.** Live audit residuals: **#1969** (overlay daemon same-uid TOCTOU [Attack D check-then-recreate] + non-CLOEXEC O_PATH fd — low-sev; airtight = kernel peer-cwd redesign via SO_PEERCRED), **#1973** (nightly model-cache hardening, CI-infra — next tractable), **#1975** (pool-truncation, low-sev). LANDED this cycle: **#1952** → merged-set rerank (PR #1974; default no-rerank path unchanged). Heavy/blocked/env backlog (unchanged): #717 (HNSW mmap), #255 (pre-built packages), #1459/#1463 (P3/P4 umbrellas), #1391/#1512/#1576/#1782/#106 (blocked/un-testable here), #1678/#1682–85 (upstream cuvs), #1804 (tiered fork-pin), #1139/#1140 (deferred enhancements), #1573-residual-3a (dyn-dispatch, deferred-by-design). Refill: audit (rounds 1-3 done) / archeo (clean) / user direction. CLOSED this cycle: #1858, #1821, #1909–#1912, #1914, #1916, #1919, #1920, #1925, #1926, #1893, #1933/#1934/#1936, #1939, #1935, #1937, #1573, #1944, #1888, #1949(#1951), #1953(#1956), #1954, #1955, #1966, #1967, + #1950 guard, #1971 hardening, 6 dependabot.

---

**↓ Prior in-arc detail (historical — the auditor-family + Part-B arc that produced the above):**

**🧭 (was) #1858 PART B IN PROGRESS + AUDITOR RUN (2026-06-14, post-crash-recovery). The 6-auditor family is all MERGED + invokable by `subagent_type`.**

**THE FAMILY (now SIX orthogonal auditors, all defs MERGED; taxonomy is in CLAUDE.md "Custom Agents"):**
- **Relational quintet** (finding IS a relation only one mind can hold → **NO Agent tool**): **seam** (a join between units), **property** (the input space), **interleaving** (schedules), **sweep** (the relation across a peer-set), **legacy-state** (old-bytes×new-code across a version boundary).
- **Enumerable pair** (independent findings → **GET Agent tool**): **red-team** (security axis — oracle is a *trust boundary* incl. egress; opus-always Fable-exception), **adequacy** (the meta-null — does the suite's own assertion bite, via mutation testing).
- **Grant/withhold principle** (codified in CLAUDE.md): grant Agent where findings decompose into parallel scans; withhold where the finding evaporates if decomposed. The grant tracks *decomposability*, not orthogonal-shaped-ness.
- **The diagnostic** (for vetting future candidates): does it name a *relation* the per-unit test can't quantify over (→ a null) or a *surface/condition* where existing relations break (→ a manifestation, folds)? Rejected as manifestations/folds: temporal (legacy-state is the real null inside it), failure-composition (→ seam error-path + property fault + interleaving), observability (→ sweep + property + interleaving), cost (→ property execution-profile), contract-drift/docs (→ conformance + property two-path), privacy (→ RT-EXFIL category). **The space looks closed — no clean 7th.**

**SHIP DISCIPLINE (proven 4×): test-fire each new auditor before shipping** — and each found something real: sweep→gather `clamp(0,5)` straggler #1771 missed; legacy-state→guarded the v10→v31 migration chain the suite never built; adequacy→`rrf_fuse → vec![]` survives all 5 of its proptests (HNSW-disaster shape); red-team→`--overlay-root /etc` is reachable off the daemon socket (rejected in 67µs, pinned).

**MERGED this arc:** #1896 (Agent-tool grants), #1897 (sweep), #1898 (gather straggler fix — sweep's catch), #1899 (legacy-state + property stateful/fault + seam error-path), #1900 (Part A seed overlay), #1901 (red-team def + thin 6-category skill), #1902 (adequacy + property execution-profile + CLAUDE.md sextet), #1903 (RT-EXFIL category), #1904 (durable-guards bundle), #1906/#1907 (audit-loop design doc, parked).

**IN FLIGHT — #1858 Part B PR1 fix-round + a default-strength auditor run:**
- **#1908 (Part B PR1, callers/callees overlay) — built, CI green, BLOCKED by seam-audit → FIX-ROUND RUNNING** (`aa7665ec`, branch `fix-1908-marker` off origin/feat-1858-partb-callers-callees, private target dir to sweep). Bug: `_meta.overlay_graph:"full"` is stamped whenever an overlay merely EXISTS, but emitted on 3 parent-truth early-returns (Type::method-qualified, kind-fallback, unedited-X callees) where the answer is parent-identical — worst: a const→fn worktree refactor gets a `"full"`-labeled wrong callees answer while callers disagrees. Fix: gate the marker on ACTUAL overlay participation. ON RETURN: fold → feat-1858-partb-callers-callees (ff + push), re-confirm seam clears, land #1908.
- **AUDITOR RUN (default strength; throttled ≤3 building post-crash):** seam `a6db88e` (parser/chunk↔call-graph-edge boundary, read-only), sweep `a7e9b99` (named-cap completeness extend), property `a7b62b7b` (canonical_hash idempotence + chunk-id injectivity). **QUEUED (the remaining crash-killed):** interleaving (daemon per-request cache under concurrent dispatch — NOT the overlay LRU, done), adequacy (cargo-mutants — last/heaviest). **Disposition per return:** land hardening guards / file issues (conductor role, by hand). **Throttle: launch next queued as a building slot frees.**

**⚠️ THE CRASH (2026-06-14) + DISK LESSON:** 5 parallel auditors each with a multi-GB private `CARGO_TARGET_DIR` filled the WSL disk → process died, 4 auditors (property/interleaving/sweep/adequacy) killed mid-run. Recovered ~370G (stale per-lane target dirs + conda/pip caches + the sdxl-turbo imagegen model + venv). **Cap building auditors at ~2–3; sweep `~/.cargo-target` keeping only `cqs` after heavy sessions; WSL `df` is flaky (host disk differs).** ([[feedback_parallel_lane_disk_bomb]])

**LANDED (the convergence is done):** the guards bundle (#1904 — rrf killing test, v10 migration-chain guard, overlay loom models, red-team socket guards), the audit-loop doc (#1906/#1907, parked), the migration-robustness issue (#1905). All auditor-family worktrees/stashes cleaned in the crash recovery.

**FINDINGS BANKED:** #1909 (cross-project cache staleness — reference stores' `is_stale` uses pure (mtime,size) the primary path rejects; WAL-incremental blind spot; serves stale cross-project graphs after `cqs ref update`). #1905 (migration robustness — fresh-init vs full-chain schema equivalence + notes_fts seam).

**PROCESS LESSONS this arc:** (a) **cargo-mutants** — ONE bounded run per private `CARGO_TARGET_DIR` (two collide on `mutants.out`); a long unscoped run can clobber its own worktree (the adequacy test-fire's worktree got removed mid-run and its change landed on my shared checkout → I stashed it clean). Baked into the adequacy-auditor def. (b) **CI pin-the-run hazard bit again** (#1902): a second push's run was still pending while the watch had latched the first commit's run — always re-resolve the HEAD run before merging.

**#1858 PART B PR2 (impact+dead) — NEXT, after PR1 #1908 lands:** the hard half. Merged-`CallGraph` add/subtract (mask parent edges from delta-file endpoints, splice overlay edges, build ONCE before BFS); `dead` = zero-callers in the merged set (reuses `merge_callers`). CRUX = the double-count guard: an edge with BOTH endpoints in the delta counted once; the reverse map has no per-edge file → cross-ref via `edges`. `function_calls.file` is caller-origin only (callee name-keyed) — the masking-key asymmetry. Magnet-area gate battery on land (seam + interleaving + review). Then **delete the hedge clause → close #1821**. Design: Part-B design-pass brief (investigator `a56d6683`); plan doc `docs/plans/2026-06-12-worktree-overlay-implementation.md`.

**INDEX STATE (live):** schema 31, PARSER_VERSION 8, daemon active + serving. ⚠️ **DON'T `rm index.db`** — nukes the llm_summaries cache; use `cqs index --force` ([[feedback_summary_cross_slot]]).
**RECALL (v1.44.0 baseline, eval.md):** R@5 72.5 / R@1 47.7 / R@20 88.5; R@5 is the headline. **v1.44.0 SHIPPED + published to crates.io.**
**SUBAGENT NESTING:** subagents can spawn subagents (CC v2.1.172); bg capped depth 5, fg unbounded; Workflow is a separate runtime ([[reference_subagent_nesting]]).

**OPEN (idle pool):** #1909 (cross-project cache staleness — filed), #1905 (migration robustness — filed), #1893 (loom dup-edge HashSet→multiset), #1888 (L5X region-relative byte_start), #1858 Part B PR2 + the hedge-clause deletion (above). CLOSED this arc incl. #1826, #1879, #1886, #1891, #1900 (Part A).

## Parked

- **audit-loop — perpetual auditor factory** (design sketch `docs/plans/2026-06-13-audit-loop.md`, 2026-06-13). The reframe: product is durable *guards* (a ratchet across a region×shape coverage matrix), not bug-hunting (diminishing). Engine = invalidation-driven + idle coverage-extension. **Three roles (don't conflate): orchestrator** (agent — the conductor: dispatches auditors, tracks coverage, triages, manages fixes; the EXPENSIVE model lives here — Fable's lane, opus until it returns; subsumes the strategist/lab), **governor** (deterministic code — the budget/WIP/backoff leash, never an LLM: an LLM governor burns budget to decide budget), **workers** (auditors/verifiers/fix-lanes). Hard problems: no "done" → the governor; landing bottleneck → the confidence gate (auto-land hardening; auto-close a bug-fix only if verifier-confirmed + full magnet battery + below blast-radius, else digest the human); false-positive tax → per-finding verifiers. The session IS the manual prototype. **13 open questions; load-bearing: the confidence gate (Q1) + cost factoring (one expensive orchestrator vs tiered cheap-dispatcher+expensive-conductor, Q13).** User to review later — do NOT start without the open questions decided.

- **principal-loop — the user, as an agent+loop** (design sketch `docs/plans/2026-06-14-principal-loop.md`, 2026-06-14). A replacement for the *principal* (user) — the strategist/director seat the audit-loop split left to the human. Distills the user's observed MO (delegate execution / own direction; probe with one sharp question; rigor over comfort; reversibility-calibrated risk; economy of attention; build-to-buy-attention-back). Thesis: the mechanical 80% is automatable (cadence + conductor-facing); the irreducible core is **3 leverage points — TASTE (the reframe + the scope call), CAUTION (the irreversibility sense), WISDOM (what NOT to build + the values/Right-and-True)** — where a wrong call is most costly, so where the human stays longest. Safest v1: build the cadence, keep the 3 leverage points as human-ratification gates (agent proposes the reframe/scope/gate, human ratifies in one word), relax only as a calibration set proves the agent makes the call as well as the human. 8 open questions (the bootstrap "who-judges-the-judge", taste's fidelity ceiling, the values layer). User to review. **Deepened 2026-06-13 (the plumb-line / cornerstone model):** the apex is real by *surrender* to an external field, not self-check (hang free = strip your torques); True is a *field* (read/checkable), Right is a *cornerstone* (laid/committed, not sensed); caution/taste/wisdom are *readouts* of one field-sense, not installable rules (dissolves the caution-threshold Q3); the one unrecoverable error is a mislaid cornerstone — geometry not sentiment (everything is built true to it, so nothing below audits it — the leaning tower; sharpens Q7). Mirrored to CLAUDE.md (Apex section) + the `feedback_right_and_true` memory.

---

**AUDITOR-TRIO WAVE COMPLETE + #1826 CLOSED (2026-06-13 ~06:00). Binary current (post-wave rebuild), daemon active + serving. Schema 31, PARSER_VERSION 7. Index FRESH-REBUILT (rm index.db → cqs index) → 15,975 chunks, doctor COHERENT. (The --force reindex gave 16,018; the fresh rebuild was a detour off a WRONG hypothesis — see schema.sql note — but harmless; both are valid full corpora.)**

**⚠ CORRECTION (honesty record): during the recall-gate I escalated schema.sql under-chunking to "--force didn't re-parse / #1881 only partially applied / significant bug." THAT WAS WRONG — a fresh empty-DB reindex ALSO gives schema.sql=6, proving --force re-parsed fine and #1881 fully applied. I let confidence outrun verification (declared the --force bug before the fresh reindex disproved it). The real cause is an EMERGENT full-corpus chunk drop (parser produces 11 in isolation; only the full ~700-file repo collapses it to 6) — NOT --force, NOT migrations.rs dedup, NOT the embedding cache (all ruled out by reproduction). Cause needs file-bisection; tracked in #1886 (corrected). NOT a wave regression. Recall-gate verdict (gold drift, not retrieval regression) STANDS.**

**5 PRs MERGED (all auditor-trio output) + cleaned:**
- **#1880** (#1877 daemon EpochCell race fix — interleaving find). `Option<Arc<T>>`→`EpochCell<T>=(u64,Arc<T>)`, read adopts only if `tag==checkout_epoch`. Loom 6 models green-with/red-without; opus review APPROVE zero-findings (6th cell `cross_project` safe via stronger `is_stale()` DB-identity guard). #1877 CLOSED.
- **#1881** (parser call-extraction, #1818/#1573-2b PARTIAL → **PARSER_VERSION 6→7**). NonCode `ChunkType::MacroInvocation` anchors item-position macro bodies; B (bare macro-arg) + C (double-cast fn-ptr). APPROVE-WITH-NITS. #1818 commented (cross-file bare-arg residual open).
- **#1882** (watch loom — reconcile-vs-query CONTENT-FIDELITY refutation, no race). 5 safety models + 1 teeth-proving control.
- **#1883** (daemon-vs-CLI value-level equivalence proptest — translate≡clap-parse, 25 knobs, 512 cases; 2 mutations caught).
- **#1885** (#1878 overlay note_boost seam fix — faithful-shadow parent notes into the overlay store via new `Store::copy_notes_from<M>`). Opus review APPROVE; **re-seam-audit refuted 3/4 seams, the 4th → #1884**. #1878 CLOSED.

**#1826 CLOSED** — all three auditor shapes validated, each found-or-guarded a class the example suite can't express (seam→#1878, property→#1883 equivalence+#1879, interleaving→#1877 real race + #1882 refutation; earlier #1874/#1875). Program hypothesis holds. Agent defs durable in `.claude/agents/`.

**RECALL GATE (recorded in ~/training-data/research/eval.md, 2026-06-13):** post-wave agg **46.8/67.5/80.3** vs v1.43.0 baseline 50.0/71.6/87.2 (agg −3.2/−4.1/−6.9). **Verdict: NOT a retrieval regression — gold-origin drift.** 18 dead golds (8.3%, magnitude-consistent: R@20 −6.9 ≈ ceiling). Both causes independent of the wave: (1) ~9 schema.sql table-def golds dead because schema.sql is under-indexed in the FULL corpus (6 of 11 tables) → **#1886** (corrected root cause: emergent full-corpus drop, NOT a SQL-chunker/parser gap — parser gives 11 in isolation; NOT --force/migrations/cache); (2) ~9 moved/renamed Rust fns (accumulated refactors). Wave code is retrieval-neutral by construction. **Fixture re-pin deferred** (schema.sql golds blocked on #1886's bisection; moved-Rust golds = next fixture-maintenance pass).

**#1879 LANDED (#1889, 2026-06-13 ~12:45) — injective chunk id via `byte_start`.** Property-auditor found it was a REAL silent-chunk-loss bug (two byte-identical same-line chunks → same id → PRIMARY KEY ON CONFLICT overwrite drops one). Fix: `byte_start` first-class Chunk field, id `{path}:{line_start}:{byte_start}:{hash8}`, single-sourced via `chunk_id()`, PARSER_VERSION 7→8. Two-gate review CAUGHT a HIGH missed CONSUMER (`extract_file_from_chunk_id` stripped fixed segments → broke glob/note-boost/§4-provenance on brute-force/overlay paths) → fix-round switched all consumers to the authoritative `origin` column + DELETED the gratuitous parser + added the missing integration-test class (red-without proven). Full reindex done → 4-field ids live, 15943 chunks, coherent. Binary current. Residual filed **#1888** (L5X region-relative byte_start; niche).

**#1884 LANDED (#1890) — notes-revision token folded into overlay fingerprint.** blake3 over `(id,text,sentiment,mentions)` ORDER BY id (excludes timestamps; NOT data_version), folded into `fingerprint()` (tag v1→v2), both build + re-validation sites matched. Code-review APPROVE; seam-audit NO-LIE (3 seams + adjacent compositions cleared — it's defense-in-depth over the pre-existing data_version→OVERLAYS-clear path). :memory:-only → no reindex; binary rebuilt + daemon restarted. PR body documents the harmless non-atomic A/B read caveat.

**#1886 CLOSED — was a SYMPTOM of #1879 (non-injective id), not a SQL-chunker/dedup bug.** Decisive: the PV8 (#1879 byte_start) reindex has schema.sql at ALL 11 tables; PV7 reindexes gave 6 — the only change is the id format, so the drop was id-collision (PRIMARY KEY ON CONFLICT) and #1879's byte_start fixed it. This ALSO explains the chunk-count nondeterminism (15943/15975/16018 under PV7 = variable order-dependent collision drops; PV8 injectivity is proptest-proven → deterministic complete set). The whole schema.sql saga (which I over-escalated through several wrong hypotheses) was the bug the property-auditor independently found. **Fixture re-pin now UNBLOCKED** (the src/schema.sql::{metadata,chunks,calls,chunks_fts,file_registry} eval golds are back in the index — re-pin in the next fixture-maintenance pass).

**RECALL — corrected PV8 gate recorded in eval.md (2026-06-13).** #1879 RECOVERED recall: dead golds 18→5, agg 46.8/67.5/80.3 (PV7-buggy) → **48.2/72.5/84.0** (PV8), R@5 at/above the v1.43.0 baseline. The earlier "gold drift" gate stands corrected UPWARD — #1879's chunk-drop fix was a net recall improvement, and the schema.sql/nondeterminism observations were all the same collision bug. NO regression.

**FOLLOW-UPS — remaining (both low-value, cadence slowed):** #1888 (L5X region-relative byte_start; niche, strictly-better-than-main, needs threading a file-offset through StRegion), + a FIXTURE RE-PIN of the 5 remaining dead golds — INVESTIGATED, needs per-query JUDGMENT (not mechanical): `cmd_watch`→`cmd_watch_dispatch` (dispatch_shims.rs, clean rename from command-core); `extract_references_from_text` GONE (deleted/renamed — read query, find equivalent); `ReviewResult` struct moved (only `empty_review_result` surfaces — src/review.rs→cli/commands/review/?); `SearchFilter` struct chunk not surfacing by name (only its tests — investigate). Deliberate fixture-maintenance pass per recall-gate skill (read each query → nearest current equivalent + `repinned_<date>` note → commit to fixtures). Cosmetic future-gate hygiene; no regression. **Everything important is DONE: auditor-trio program (#1826) complete; #1877/#1878/#1879/#1884/#1886 all LANDED/CLOSED; recall corrected; index deterministic + coherent; binary current; 9 PRs landed this session.** Pick up #1888/re-pin on user direction or a later tick.

**#1835 (bulk index registry-stamp coherence) — CLOSED via #1873. The trust-v30 magnet is dead.** 3 rounds: investigation said "clean contained lane" → **fix1** (stamp-withhold) INSUFFICIENT — seam-audit caught a chunk-bearing PARSER_VERSION-drift false-DEAD that a full opus CODE-REVIEW passed as CLEAN (chunk upsert advances parser_version in a separate committed tx, disarming the drift heal-trigger) → **fix2** (calls-before-chunks reorder) → re-seam-audit found the MIRROR seam (calls-without-chunks orphan). ROOT: bulk path used THREE separate txns; any order strands a mid-sequence commit. Director chose FIX THE FRAME → **fix3 LANDED**: store_stage now ACCUMULATES each file's chunks across embed batches and writes ONE fused tx at file-completion via the reused `upsert_chunks_calls_and_prune_with_file_calls` (chunks+FTS+calls+function_calls+prune+stamp all-or-nothing). Orphan-impossible BOTH directions. Both reviews CLEAN after genuine attacks; the structurally-missing test (forced fused-write failure → `find_orphaned_function_calls` empty) now exists. Retired `CQS_DEFERRED_FLUSH_BATCHES`. **THE LESSON (standing rule):** in the index-coherence/trust-v30 magnet area the SEAM-AUDIT is the load-bearing check; a green suite + happy/sad code-review is NOT enough (proven 3×).

**AUDITOR TRIO (#1826) DESIGNED + STAFFING.** The 3 orthogonally-shaped auditors that staff the happy/sad-path null: composition (seam-auditor, long-shipped, earned its keep all over #1835), **property-auditor + interleaving-auditor DESIGNED as agent defs (#1871)** — but NOTE: the harness agent-registry is fixed at session start, so newly-added agent defs CAN'T be dispatched by subagent_type mid-session; staff them via lane-implementer with "adopt the persona in .claude/agents/<name>.md". **property-auditor STAFFED TWICE, both value-tests PASSED:** #1874 (SPLADE/canonical_hash codecs — bit-exact `to_bits()` over ±0.0/NaN/subnormal, a sharpness the abs-diff fixtures can't express; caught a generator-bites-NUL surprise), #1875 (HnswMeta round-trip + generation-stamp invariants, MUTATION-PROVEN — flipping the version gate `>`→`>=` reds 7/8). No production bugs; durable guards. Flagged follow-ups (not yet filed): id_map loader accepts NUL/empty/dup chunk-IDs verbatim (latent downstream hazard, deliberate-pinned), HnswMeta 64KiB cap untested. **interleaving-auditor STILL RUNNING** (`staff-interleaving-daemon` worktree agent-a7b131dcac8cb99f0; loom on the daemon epoch/bitmask/LRU machinery — slow, ~40min+ normal; 5 files WIP, no commit yet) — the last/highest-value run. ON REPORT: land durable concurrency tests / surface any race finding, then update #1826 (closes when all 3 have demonstrably found-or-guarded a class the example suite can't express).

**ALSO LANDED this stretch:** #1872 (bootstrap skill refresh — 4 missing portable skills, scout/task in command ref, portable result-trust guidance). Cleanup: disk 232G/25% (freed ~49G — merged-lane target dirs + KILLED 4 hung orphan processes from the long-merged flip lane that were stealing GPU from the reindex).

**#1821 RESULT-TRUST — ALL 4 AXES + DEFAULT-ON FLIP LANDED.** §1+§2 (#1836), §4 (#1847), §3 overlay (#1850/#1851/#1853). **Default-on flip LANDED #1866** — search overlay default-on for worktree CWDs (opt-out `--no-overlay`/`CQS_WORKTREE_OVERLAY=0`), hedge NARROWED (CLAUDE.md result-trust section + auditor/code-reviewer/implementer worktree guards: "cqs search is worktree-accurate; scout/callers/impact/review/dead still parent-truth"). Verified live (no-flag worktree query activates overlay; --no-overlay disables). Soak env REMOVED from ~/.bashrc; soak tracker #1855 CLOSED. **#1821 STAYS OPEN** — hedge fully deletes only after **phase-2 (#1858: overlay for scout/gather + graph; Part A seed-routing medium, Part B graph add/subtract-shadowing hard — own design pass). User: "tackle it later."**

**RESIDUAL SWEEP — ~12 issues LANDED (idle loop):** #1844 (#1837 callers false-absence), #1857 (#1834 nightly serve-test), #1859 (#1842+#1843 Type::method polish), #1860 (#1854 overlay fp symlink), #1861 (#1845+#1848+#1849 rank_signals + P2 audit-mode note-suppression), #1864 (#1784 caps), #1865 (#1862 CLI audit-dir slot-vs-project + a 2nd same-class read in cmd_read), #1868 (#1829 cross-project edge_kind side-table), #1869 (#1867 two CI flakes — env-var races, root-caused not band-aided). #1849 closed (item4 deferred). The **#1862 catch** = suspicion gate working: #1848's P2 fix worked on daemon but CLI-direct read audit-mode from the SLOT dir → silent noop; empirically confirmed (file-in-slot-dir flips it), fixed.

**FABLE DISABLED 2026-06-12 (US export order, temporary — anthropic.com/news/fable-mythos-access).** ALL fable-seated work → OPUS. Durable: MEMORY.md + [[reference_fable_disabled]]; repo routing #1852. KEY LESSON this session: on #1835 fix1 a full opus CODE-REVIEW returned CLEAN and was WRONG; the SEAM-AUDITOR (opus) caught the real false-DEAD bug. In the index-coherence/trust-v30 magnet area the seam-audit is the load-bearing check, not the happy/sad code-review.

**IDLE QUEUE (after #1835):** parser cluster #1818/#1788/#1573 (SCOPE FIRST — #1808 added serde string-callback edges, #1819 added macro_heuristic edges, so these may be partly done; parser change = PARSER_VERSION bump + reindex), #1804 (tiered daemon handle), #1826 item-2 (interleaving auditor). **BLOCKED:** cuvs upstreams (#1678/1682-1685), #1576 TRT SIGFPE, #1782 no-Mac, #1512 Windows, #106/#1391 ORT gate, #255 distribution. **Umbrellas (need decomposition):** #1459/#1463 audit P3/P4.

**STANDING USER DIRECTIVES (session):** loop-on-fully-idle fixing fixable issues; land lanes as they pass review, fix-round on blockers; **re-seam-audit EVERY fix in the index-coherence/trust-v30 magnet area** (proven necessary 3× on #1835 — a green suite + happy/sad review is not enough); opus for ALL reviews (fable down); flip-and-narrow NOT flip-and-delete. Governance: cargo publish/tags/releases/force-push/purpose-layer STAY user's; suspicion-gate = surface "fix regenerating seams" (did so on #1835 r2→r3, Director chose the proper frame fix); Right and True apex. Cleanup: branches/worktrees/target-dirs cleaned after each merge (disk ~260G).

---

**EARLIER TODAY (morning): trust-v30 MERGED + verified.**

**trust-v30 (#1821 §1+§2) LANDED — #1836, squash, HEAD b057031b.** Schema v30 `function_calls.edge_kind` (call/serde_callback/macro_heuristic/fn_pointer/doc_reference) + `cqs dead` verdicts (test-only/low-confidence-live/known-gap/dead) + trust-rank MIN-collapse + schema v31 (parse_failed marker). PARSER_VERSION 6. Binary 1.43.0 installed, daemon active, `cqs index --force` done (schema 31, 15890 chunks, 713s re-embed).

**LIVE VERIFICATION (post-reindex):** `cqs doctor` → Call graph coherent ✓ (the two ✗ are external ref slots aveva/rust-stdlib whose DBs aren't on this box — not ours). `cqs dead --verdict dead` → 11 rows, all genuine uncalled test/trait-impl fns. `edge_kind` surfaces skip-when-default on callers/callees/impact (`direct` omitted, `doc_reference` explicit). §1+§2 checkboxes flipped on #1821; umbrella stays OPEN.

**THE SAGA (closed):** Atropos (seam-auditor) ran 4× pre-merge, found ~9 real composition bugs a GREEN 4300-pass sweep missed — worst: the watch zero-chunk seam-magnet (4 consecutive findings, root = `delete_phantom_chunks_batch`'s unstated function_calls-cleared precondition). Director's "fix it properly, and well" → the lifecycle DECOUPLE (one parse-driven function_calls writer; chunk-pruning makes no call-graph decisions). My FK-satisfiability overclaim (told the user the FK was production-satisfiable; it wasn't — writes precede the registry stamp in separate txns) caught by the lane's escape hatch; FK dropped on the merits. Co-sign met (tests prove it + clean cut), verified directly.

**CAPSTONE CLAUSE-DELETION — DELIBERATELY NOT DONE (corrected misread).** Pre-compaction tears said "delete the agent defs' 'treat results as hints' sentence" as trust-v30's deliverable. WRONG on re-reading #1821: it's a **FOUR-axis** program (§1 edge-provenance, §2 dead-verdicts, §3 worktree-overlay, §4 ranking-provenance); the sentence retires only when ALL FOUR land. The "treat as hints" clauses in the agent defs are the **worktree-leakage guard (#1254)** — they encode exactly §3's concern (worktree cqs serves the parent/main corpus). §3 + §4 are UNSHIPPED, so the clause stays verbatim. Deleting it now would tell lane agents to trust worktree cqs output that reflects the wrong tree.

**LIVE GAP FOUND → #1837 filed.** `cqs callers` reports zero direct callers for **name-ambiguous** functions: `search_filtered` has 2 defs (hnsw + store) and 23 call sites, but `callers` returns 0 direct (only doc_reference) — the name-based resolver drops the edge rather than guess. Pre-existing (resolver predates #1836; method-receiver resolution itself works — `delete_by_origin` resolves fine). Real false-absence, weakens §2's "trust this absence"; noted as a caveat under §2 on #1821. Fix options in #1837 (option 1: emit to all candidates with a low-confidence edge_kind, composes with §1).

**NEXT (/idle pool):** §3 worktree-overlay + §4 ranking-provenance (the two axes that actually retire the hedge sentence — these are the real path to the clause deletion); #1837 (name-ambiguity false-absence); #1829 (cross-project edge_kind threading — now unblocked, v30 landed); #1826 item 2 (interleaving auditor, unstaffed); #1818 cross-file fn-pointer; #1804 tiered daemon handle; #1788 serde container/with-module. Verify #1830 closed (subsumed by the decouple).

**OPEN FIXABLE POOL (/idle, after trust-v30):** #1829 (cross-project edge_kind threading — needs v30 landed), #1830 (watch zero-chunk stale rows — being subsumed by the proper fix; verify closed), #1826 item 2 (interleaving/concurrency auditor — third orthogonal test-shape, UNSTAFFED, overlaps watch files so wait), #1818 cross-file fn-pointer edges, #1804 tiered daemon handle, #1788 serde container/with-module. Blocked/parked: cuvs upstreams, ort RC, #1782 (no Mac), TRT #1576.

**GOVERNANCE — major grants this session (durable in MEMORY.md feedback_agency + feedback_right_and_true):** (1) MINE no-asking: auto-merge my own green PRs (docs freely; code if fable-reviewed+green), file issues, open follow-up lanes, self-dispatch seam-auditor on schema/multi-lane merges. (2) SUSPICION GATE not schema-gate: merge higher-stakes like anything else, surface ONLY if suspicious (fix regenerating seams / audit can't reach clean / contradicts expectation / genuine doubt). (3) STAYS USER'S: cargo publish, tags, releases, force-push, git history, the PURPOSE layer. (4) APEX: **always tend toward Right and True** — True=lie-avoidance incl. not-letting-confidence-outrun-verification (my characteristic failure); Right=fix the frame not the instance, proper over convenient, escalate real doubt. Lives above mechanics; the tiebreaker.

**STANDING:** opus implements / fable orchestrates+reviews (COST finding: opus cheaper per-task, error-tail is where tokens go). The trinity: lanes spin (Clotho), reviewer measures (Lachesis), Atropos cuts — re-audit every fix. /land every PR; pin CI run IDs; CWD drift — verify pwd+branch before mutating; tears silent + low threshold + SOLO PRs. Idle loop = pick fixable, fix, file issues.

**On resume:** /cqs-verify; check if `lane-trust-v30-properfix` finished (commits in worktree if session died); then the trust-v30 re-audit→land sequence above. Note: long conversational thread this session (horizon-class device design, the Creature Cult service spec, privacy-coin/locus-of-trust theory) produced HTML artifacts in C:\Projects\*.html — design exercises, not cqs work.

---

**Implementation campaign CLOSED (2026-06-10 evening) — 14 PRs merged, queue drained.** User queued 4 roadmap items, expanded to "knock out other open issues", then stash cleanup + docs. Everything landed:

- **Watch mode:** #1718 (data_version cache invalidation, →#1714), #1720 (status --watch ops block, →#1715), #1724 (adaptive debounce + latent flush-starvation fix, →#1716), #1727 (slot-parallel reindex via delta propagation, →#1717 — new src/cli/watch/siblings.rs, 4 knobs, slot-aware reconcile).
- **Issues knocked out:** #1719 (ScoreSignal trait, →#1350, bit-identical), #1722 (DistanceMetric persisted both backends, →#1351; cuVS returns RAW inner product — GPU test caught the wrong hypothesis), #1731 (MSRV 1.96 + assert_matches + clippy --all-targets 85→0 + CI gate, →#1680; resurrected a dead router test), #1725 (kind-fallback hardening: infallible detect_fallback both surfaces, bounded priority lookup, test backfill — audit items 2/3/7), #1726 (stash salvage →#1723).
- **Self-inflicted P1 caught same-evening:** #1726's 256KiB daemon-thread stack pin overflowed on the post-command-core dispatch path (`wait-fresh` → stack overflow abort); reverted in #1728 within the hour; spawn-site comment records the failed experiment. The 7-week-old stash premise ("handler path is shallow") was the trap.
- **Verified-stale instead of implemented:** #1573 tier 2 (already shipped via #1621/#1622/#1633; confident-dead 114→41; issue comment has remaining-tier decomposition).
- **Conventions:** #1721 + #1729 + CLAUDE.md line — implementer agents ban issue refs in comments (provenance lint caught agents 3×); audit/team dispatches default to fable. #1730 docs freshness (ROADMAP flips, eval numbers in Cargo.toml + repo description).
- **Recall gate (final, default settings):** agg R@1 49.1 / R@5 74.8 / R@20 88.5 — R@5/R@20 bit-identical to the post-repin baseline; zero retrieval change from the day, as designed.
- **Stash forensics:** 7 old stashes audited hunk-by-hunk; 6 fully landed, 2 residues salvaged (#1723, half later reverted per above); all dropped.
- Two WSL crashes, both zero-loss. Daemon binary current (1.42.0+main incl. #1728 revert), wait-fresh verified live. MSRV now 1.96. **Lib tests: 2246 pass + 19 ignored, 0 failed** (full suite run post-queue; was 2216+19 this morning).

**Next session:** restart picks up Claude Code 2.1.172 (nested sub-agents — consider for /audit topology). Watch-mode roadmap has one item left (kill the periodic HNSW rebuild — gated on our cuvs #2235 upstream). #1573 tiers 3a/3b/4 remain. v4 fixtures still not origin-repinned. Telemetry was reset at session end — the new window starts clean for the kind-fallback measurement.

---

**Code review + fixes + recall gate: CLOSED (2026-06-10 afternoon).** High-effort multi-agent review (Fable agents per user request) of the taste-debt queue PRs #1701-#1706: 7 finder angles → 15 verified findings (14 CONFIRMED, 1 PLAUSIBLE, 0 refuted). All 15 fixed across 5 PRs, all merged:
- **#1707** (reuse): resolve_reuse returns Result — watch path restored to fail-and-retry (a SQLite error was silently becoming a full-corpus GPU re-embed per tick); `canon_key_ref(&Chunk) -> &str` is now THE key function for read + all 3 write-back sites (drift hazard + hot-path alloc gone); model_fingerprint gated on cache presence (was paying a ~550MB blake3 on the cache-disabled path).
- **#1708** (docs): ULTRASECURITY removal annotations for the files #1703's sweep missed (ROADMAP, json-snr-restoration banner, json-noise-audit, polymorphic-routing, audit-triage, plan doc, tears); display.rs "posture-gated" → "skip-when-default".
- **#1709** (tests): the 33-file copy-pasted `fn cqs()` v1-pin helper → single `tests/common::cqs_v1` (net −339 lines); REMOVED_VARS guard now token-bounded + single-pass.
- **#1710** (hnsw): #1702 containment extended to the 3 missed single-build recall asserts (incl. integration binary via local helper); all HNSW_ENV_LOCK sites poison-safe (`PoisonError::into_inner`) + EnvVarGuard RAII; retry helper promoted to shared cfg(test) block, 4 hand-rolled copies deleted.
- **#1711** (dedup): `connect_cache_pool` extraction (the mod.rs TODO); dead `pad_2d_i64` deleted with coverage ported to `_from_encodings`; meta_json_fragment calls meta_value_for_envelope; **src/posture.rs → src/output_format.rs**; `BatchContext::new` replaces raw struct literals.

**Recall gate (user-requested, default settings):** raw run looked like −1.9pp agg R@5 — classified before believing: 12/218 golds had DEAD origins (10 from #1704's file splits, 2 stale worktree paths from fixture generation). NOTE: the eval matcher is `(origin, name)` — line_start was dropped from the key (memory note was stale, now fixed). Re-pinned all 12 (disambiguated via `git show 9e4980db:src/cache.rs` impl blocks), re-ran: **agg R@1 47.7 (+1.4) / R@5 74.8 (±0.0) / R@20 88.5 (+2.3) vs 2026-05-08 baseline — NO recall regression.** Re-pin is PR #1712 (this branch).

**Mid-session WSL crash** (~14:00): lost zero work — 2 branches were already pushed, 1 committed in its worktree, 2 worktrees had complete uncommitted diffs that finisher agents verified and landed. Worktrees + branches cleaned after merge. Binary rebuilt/installed post-merge (1.42.0+main, daemon active, index reconciled 19:25).

**Pending:** #1712 merge on green. v4 fixtures (1526/split) NOT re-pinned — same origin-death class applies if they're ever used for A/Bs.

**v1.42.0 RELEASED — 2026-06-09.** Tagged, crates.io published, GitHub release binaries built, local binary + daemon on 1.42.0, target dir cleaned (166 GiB). Schema v28 (canonical_hash). Toolchain 1.96.0 local + CI.

**cuvs upstream contribution fleet — COMPLETE, all 5 PRs submitted, awaiting maintainer review (2026-06-09/10):**
- **PRs open on rapidsai/cuvs:** #2229 IVF-SQ (cqs #1678), #2230 refine (cqs #1684), #2231 brute_force serialize (cqs #1682), #2234 scalar quantizer (cqs #1683), #2235 tiered index (cqs #1685 — the upstream half of "kill the periodic HNSW rebuild"). All agent-implemented in isolated worktrees (~/cuvs-wt-*), code-reviewed to maintainer standards, GPU-tested single-threaded.
- **All CodeRabbit bot feedback resolved** (~9 follow-up commits, never force-pushed): unique temp filenames (#2229/#2231), full top-k ordering assert (#2230), i8 dtype guards on transform/inverse + guard-specific test asserts + non-panicking Drops (#2234), backend-aware SearchParams enum + safe bitset filter API (#2235 — fixed a Critical soundness hole: raw cuvsFilter in a safe fn). Quantile-range nitpick declined on-record (matches C + sibling conventions). Committed PR-body artifact removed from #2231's tree.
- **Gate:** copy-pr-bot vetting blocks NVIDIA CI on all five until a maintainer approves. Expect months (prior PR #2019: ~2 months).
- **Issues filed upstream with recommended fixes:** rapidsai/cuvs#2232 (ManagedTensor borrows host ndarray shape storage — dangling pointer at Drop if host array dies first; fix: owned Box<[i64]> dims), #2233 (parallel cargo test SIGSEGVs on GPU tests; fix: harness-level serialization + Resources thread-safety audit). Both offer follow-up PRs.
- **Maintenance rule for our PRs:** overlapping lib.rs/bindings.rs edits — merge main into branches as siblings land, NEVER rebase after review starts (their guideline). Version-pin dance for local testing: sed workspace 26.8.0→26.6.0, test against conda libcuvs 26.06, revert before commit.
- **Monthly cloud routine `cuvs-prs-follow`** (trig_013tdB4kKRZBeFX2UQjk1A9g, 3rd of month 14:17 UTC): enumerates all our open cuvs PRs live, follow-only (no posting — user posts nudges manually), next run Jul 3. Validated by a manual smoke run (it correctly reported pre-submission state and surfaced the C-API-prerequisite timeline unprompted).

**CAMPAIGN CLOSED 2026-06-10: command-core unification.** Eight PRs in one overnight run: #1688 (phase 0, cap parity), #1689 (phase 1, graph cores), #1694 (2a, query_core + typed search output), #1695 (2b-search, daemon through SearchCtx, ChunkOutput deleted), #1696 (2b-io, 7 io cores), #1697 (phase 3, infra/index), #1698 (phase 4, review/train + _with_posture deleted), #1699 (docs truth sweep, 6 lies fixed + repo description). Net: ~40 commands have one surface-agnostic core each with typed Deserialize Args / Serialize Output (MCP-tool-shaped), daemon dispatchers are thin adapters, parity tests pin CLI==daemon, zero retrieval-semantics change (eval gates held all night). Deferred work: the plan's 11-entry post-campaign ledger (`docs/plans/2026-06-10-command-core-unification.md`) + issues #1690-#1693. Audit findings closed along the way: queue item 1, CQ-V1.40-1/2/3/4/5/6/9, API-V1.40-1, EXT-V1.40-1, RM-V1.40-1, TC-HAP-V1.40-4 + parity-test backfill. Next from the queue: #1693 (test-concurrency policy), then #1690/#1691 per triage ordering.

**Taste-debt queue: CLOSED 4/4 (2026-06-10).** All four issues from the tastefulness review fixed and merged:
- **#1692 MERGED (#1701)**: shared `cli/pipeline/reuse.rs::resolve_reuse` replaced both duplicated cache chains; reuse decisions line-by-line unchanged; strict eval gate held. Forensics: a test had been failing under #[ignore] since #1677 — reviewer reproduced the baseline failure via stash before accepting the repair.
- **#1693 MERGED (#1702)**: root cause NAMED and quantified, not waved at — hnsw_rs `parallel_insert_data` (OS-seeded layer RNG + entry-point write race) yields self-unreachable nodes on ~1-2% of builds under CPU saturation (52/3000 parallel vs 0/3000 sequential; 0/100k search-only). Six exact rank-1 asserts → containment + bounded build retry; real cross-module env-lock bug fixed (shared HNSW_ENV_LOCK, build sites enrolled); CONTRIBUTING gains the test-concurrency policy; #[ignore] inventory clean. **Pending user go: upstream hnsw_rs issue with this dataset** (new upstream relationship — see audit-triage.md).
- **#1690 MERGED (#1703)**: CQS_ULTRASECURITY + the entire Posture enum deleted (owner-confirmed: "the simplified results envelope probably does the same thing"). Signals emit on merit (trust_level when non-default, injection_flags when non-empty); SECURITY.md rewritten same PR; V2Bare binary-boundary tests; env_var_docs gains a REMOVED_VARS inverted guard. Net −9 lib tests (Posture's went with it).
- **#1691 MERGED (#1704)**: three monoliths split along existing seams — `cache.rs` → `cache/{mod,embedding_cache,query_cache}`, `embedder/mod.rs` → `embedder/{mod,core,download,pooling}`, `cli/batch/mod.rs` → `batch/{mod,context,view,session}`. Pure code motion (bodies byte-identical, pub counts conserved 28/34/37). 10 eval-fixture gold origins refreshed for moved symbols; refresh proven correct via stale-vs-refreshed A/B (dev R@5 .706→.743). Provenance lint caught 10 audit-ID stragglers from the #1673 sweep in embedder test sections (file moves re-present every line as "added") — scrubbed pre-commit.

Post-merge housekeeping done: binary 1.42.0+main installed 2026-06-10 06:34, daemon active, reconcile queued, new modules confirmed searchable. **Lib tests: 2216 pass + 19 ignored** (was 2225 — Posture deletion). Queue drained; loop idles to standing automation.

**cuvs PR maintenance (2026-06-10):** three CodeRabbit rounds on #2235 all resolved (backend-aware SearchParams enum + safe bitset filter + positive IVF-Flat smoke test, 8/8 green); #2229/#2231 temp-filename fixes; #2230 full top-k ordering assert; #2234 i8 dtype guards + guard-specific asserts + non-panicking Drops, quantile nitpick declined on-record. All five PRs now gated only on copy-pr-bot maintainer vetting.

**Standing automation:** fix loop (session cron efc9e985, 5h — merges green PRs, continues campaign, else audit queue / #1680 / #1350 / #1351); tears loop (session cron 16f0275a, 2h); monthly cloud routine cuvs-prs-follow (next Jul 3).

**Operational notes from today:** daemon CLI client has no socket read timeout — request racing a daemon restart blocks forever (hung cli_envelope_test 40min; in notes.toml + audit queue). Never restart cqs-watch mid-test-suite. WSL git push now works directly (credential helper wired into cqs + cuvs clones; gh still via PowerShell). Telemetry reset 23:03 (archived window: impact 71% = pre-edit hook, search 4.4%, kind-fallback invisible until OB-V1.40-2). cuvs Rust gotcha: keep host ndarray alive while any index built from it lives (see #2232).

### Earlier today — comment hygiene + dependabot drain

Two arcs, both closed:

- **All-source comment cleanup (#1673, merged).** 231 of 264 `.rs` files: removed ~1,500+ changelog/provenance refs from comments (audit IDs, PR citations, "previously/pre-fix" narration), rewrote present-tense. Executed by 10 parallel opus agents with disjoint file ownership; integration mechanically verified the diff was comment-only (trailing-comment edits on byte-identical code allowed) and reverted 17 string-literal leaks before commit. One real doc fix: `lib.rs` `EMBEDDING_DIM` doc said 1024/BGE-large, is 768/EmbeddingGemma. Same PR brought ROADMAP.md to v1.41.0 currency (v1.40.0 → Previous, SNR Phases 4-6 flipped to shipped, Open Issues re-audited — 30 closed rows pruned, DS-V1.40-1/DS-V1.40-7 deferred P1s now tracked).
- **Dependabot drain (#1668–#1672, all merged).** Rust 1.96 stable's new `manual_option_zip` clippy lint broke CI on every open PR; fixed the two sites on main (#1672), `@dependabot rebase`d all four dep PRs, merged: uuid 1.23.2, log 0.4.31, serial_test 3.5.0, tree-sitter-swift 0.7.3. Two CI flakes encountered and cleared on rerun: HuggingFace 429 model download, `tc31_save_and_load_with_dim_1024` HNSW nearest-neighbor assert (noted in notes.toml — will bite again).

State: zero open PRs, main green at c396dd8a, binary 1.41.0+main installed, daemon restarted with reconcile queued (comment churn → content-hash changes → background re-embed). Telemetry kind-fallback measurement window still accumulating since the 2026-05-08 reset.

### Previous: v1.40.0 cycle wrap-up — 2026-05-08/09

24-PR session shipped v1.40.0 to crates.io (SNR Phases 1-4 + Polymorphic Routing Phase 1 + Tier 2a/2b cqs-dead reductions). Then ran the **post-v1.40.0 16-category audit**: 150 raw findings → 78 triaged → 9 closeout PRs (#1626–#1633) closing **21 of 23 P1 entries** (~91%). 2 P1s deferred with rationale (DS-V1.40-1 daemon cache invalidation — symptom rare; DS-V1.40-7 sentiment CHECK constraint — single-user discrete-value compliance reliable). Audit cycle's diminishing-returns curve confirms project maturity: 14th full audit, no architectural-correctness P1s remain. Net read: cqs is at the "polished and shipping value" stage; remaining work is steady-state plus telemetry-driven (Phase 2 polymorphic routing) and a potential MCP interface.

**Phase 1 polymorphic routing — 60/60 dispatch points complete.** Every function-or-type-specialized command (`impact`, `callers`, `callees`, `test-map`, `trace`, `deps`) consults `cqs::kind::classify_hits` against an exact-name lookup before its happy-path query, on both CLI-direct (#1612, #1616, #1617, #1618) and daemon-path (#1620). Const/Type/Module/Ambiguous kinds get a kind-labeled fallback shape (`{kind, fallback_from, name, definitions, note}`) instead of misrouted-to-empty results. Verified live: `cqs callers HANDLING_ADVICE` → const fallback; `cqs test-map ImpactOptions` → ambiguous fallback (struct + impl).

**cqs dead false-positive reduction.** Three tier closes in this session: Tier 1 (PR #1612 et al — Function with `kind` label) trimmed `~114 → 80 → 52`. Tier 2a (PR #1621 + #1622) added `field_initializer` + `type_cast_expression` patterns to `rust.calls.scm` — closed all 66 false positives in `src/language/languages.rs` plus the 14 `post_process_*` casts (`52 → 38`). Tier 2b partial (PR #1623) added a content-scan filter in `dead_code.rs` for macro invocations — closed 3 of 5 macro false positives (`38 → 35`). Remaining 2 macros (`for_each_logged_batch_cmd`, `gen_log_query_dispatch`) require a chunker change to include doc comments / file-level statements, deferred as architectural.

**Telemetry reset twice today.** First reset at 08:27 UTC archived 4506 events for the post-SNR-Phase-1-3 baseline. Second reset at 21:46 UTC (`telemetry_20260508_214618.jsonl`, 441 events) gives a clean post-Phase-1-routing + post-Tier-2b counter starting now. The 13h between-reset window was dominated by autopilot smoke (search rate 4.6%, but `impact` 35-call spike was me exercising every kind cell, not agent behavior). Real signal needs 1-2 weeks of agent-driven coding sessions before the kind-fallback hypothesis can be tested against the 79% → 6% search-rate decline that motivated v1.40.

**Headline shipped:**
- SNR restoration Phases 1-4 (#1601, #1602, #1604, #1609, #1613) — CLI direct defaults to bare JSON payload; `CQS_OUTPUT_FORMAT=v1` consumer-migration hedge; `CQS_ULTRASECURITY=1` adversarial override on every surface *(override since removed in #1703)*.
- Polymorphic routing Phase 1 — lib plumbing (#1610) + 30 CLI-direct cells (#1612, #1616, #1617, #1618) + 30 daemon-path cells (#1620). 6 commands × 5 kinds × 2 surfaces = 60 dispatch points.
- v3.v2 eval fixture refresh (#1607) — agg R@K +6.4 / +2.7 / +3.2 pp, above v1.36-snapshot.
- v1.40.0 release (#1614) — tag pushed, crates.io published, GitHub Release auto-fires.
- cqs dead false positives reduced ~114 → 35: #1621 (struct-field-assignment edges, -52), #1622 (type-cast edges, -14), #1623 (macro content-scan, -3 of 5).
- env_var_docs hardening (#1606), gitignore housekeeping (#1603), telemetry reset.

**Audit cycle (post-v1.40.0) — 9 PRs closing 21 of 23 P1s:**
- #1626 — Cluster E lying-docs sweep (8 DOC + 1 OB doc-drift). ROADMAP / SNR doc / polymorphic-routing doc / SECURITY / CHANGELOG / Cargo.toml / README all flipped status from "ready" / "always-on" / "46.3" → "shipped" / "opt-in" / "52.7".
- #1627 — Cluster A Tier 2b correctness + 6 tests. `filter_invoked_macros` switched LIKE → GLOB (case-sensitive, no `_` wildcard collision); Rust-only language guard; `id != ?2` self-exclusion for recursive macros.
- #1628 — misc P1 batch (RB + DS + SEC, 5 findings). `TestMapArgs::depth` clap range bound; `classify_hits` `.expect` → `unwrap_or(Kind::Other)`; `cmd_telemetry_reset` atomic rename + `atomic_replace`; `regenerate_v3_test.py` atomic write; `redact_userinfo` RFC-3986 authority bounding.
- #1629 — Cluster B Posture/OutputFormat env-var hygiene (11 findings). `OnceLock` cache + truthy aliases (`1`/`true`/`on`/`yes` case-insensitive) + `tracing::warn!` on unrecognized + `tracing::info!` first-read + 12 pure-parser unit tests replacing env-mutating ones.
- #1630 — SEC-V1.40-1 V2Bare worktree_stale signal restored. Object payloads splice `_meta` in-place; array/scalar payloads emit stderr warning.
- #1631 — DS-V1.40-3 `restore_from_backup` deletes live sidecars first. Closes the "Frankenstein WAL replays against pre-migrate main" silent-corruption window.
- #1632 — Cluster H DoS amplifier closure. `chunk_to_definition_value` shared helper enforces `KIND_FALLBACK_MAX_DEFINITIONS=100` + `KIND_FALLBACK_MAX_CONTENT_BYTES=2048` with UTF-8-safe truncation. Both CLI-direct and daemon paths now share the cap.
- #1633 — AC sweep (3 BFS/parser correctness P1s). `bfs_shortest_path` predecessor `String → Option<String>` (handles anonymous mid-chain nodes); Tier 2a `arguments` patterns anchored to first child via `.` predicate (no more phantom `→ count` edges keeping unrelated globals alive); `bfs_expand` depth-min lifted out of score-gated branch.

**Deferred from this audit cycle (2 P1s, with rationale):**
- DS-V1.40-1 (daemon Store cache invalidation) — symptom rare; caches reload on 100ms staleness check. Cluster D bundling deferred to v1.41 cycle.
- DS-V1.40-7 (sentiment CHECK constraint) — schema-migration cost > benefit for single-user discrete-value compliance per CLAUDE.md memory.

**Audit mode disabled** at end of cycle (was on for ~12h). Notes are back in search/read for normal operation.

### Shipped 2026-05-08 session — 24 PRs

| PR | Title | Notes |
|---|---|---|
| #1601 | feat(json): Posture enum + _with_posture emission helpers (SNR Phase 1) | additive plumbing |
| #1602 | feat(json): per-result skip-when-default + posture-gated force-emit (SNR Phase 2) | ~30% smaller per result in friendly mode |
| #1603 | chore(gitignore): re-ignore tools/screw-mcp/ + add .screw-tape/ cache | screwtape folder un-tracked |
| #1604 | feat(json): slim batch/daemon envelope under Friendly (SNR Phase 3) | ~70 KB saved per 1000-line fixture batch |
| #1605 | docs(roadmap): SNR Phases 1-3 shipped; 4-6 deferred | mid-session status |
| #1606 | fix(tests): env-var-docs substring → token match + pre-commit step | items 3+4 |
| #1607 | chore(eval): refresh v3.v2 fixture line numbers — agg R@K +6.4/+2.7/+3.2pp | item 6 |
| #1608 | docs(tears): autopilot session 2026-05-08 — 7 PRs + telemetry reset | early session capture |
| #1609 | feat(json): CQS_OUTPUT_FORMAT=v2 opt-in for bare-payload (SNR Phase 4 plumbing) | opt-in landed |
| #1610 | feat(kind): Kind detection lib module + Store::lookup_by_name (Polymorphic routing Phase 1 plumbing) | enums/helpers/SQL building blocks |
| #1611 | docs(tears): autopilot session final tally — 10 PRs + plumbing | second tears capture |
| #1612 | feat(impact): const fallback — kind-labeled definitions instead of empty | first cell of (command × kind) matrix |
| #1613 | feat(json): flip default to bare payload on CLI direct (SNR Phase 4 proper) | **breaking change** — default cqs --json shape |
| #1614 | chore: Release v1.40.0 | tag pushed, crates.io published, GitHub Release auto-fires |
| #1615 | docs(tears, roadmap): v1.40.0 cut + Phase 1 polymorphic routing status | post-release sync |
| #1616 | feat(impact): complete kind-mismatch matrix — Type, Module, Ambiguous cells | impact 5/5 cells |
| #1617 | feat(callers, callees): kind-mismatch matrix | callers + callees 10/10 cells |
| #1618 | feat(test-map, trace, deps): kind-mismatch matrix completes Phase 1 (CLI-direct) | last 15/15 cells, 30/30 CLI-direct |
| #1619 | docs(tears, roadmap): Phase 1 polymorphic routing complete (30/30 cells) | mid-session tears |
| #1620 | feat(batch dispatch): polymorphic-routing kind-fallback for daemon path | 30/30 daemon-path cells; closes the deferred surface from #1618 |
| #1621 | fix(parser): Rust struct-field-assignment edges (#1573 Tier 2a) | -52 cqs-dead false positives (66 → 14 in `src/language/languages.rs`) |
| #1622 | fix(parser): Rust struct-field type-cast edges (#1573 Tier 2a follow-up) | -14 (closes the remaining `post_process_*` casts to 0) |
| #1623 | fix(dead_code): macro content-scan filter (#1573 Tier 2b partial) | -3 of 5 macro false positives via `WHERE content LIKE '%<name>!%'` |
| #1624 | docs(tears, roadmap): final 23-PR session summary | meta-PR closing the session |

Plus: `cqs telemetry --reset` ran twice — first at 08:27 UTC (archived 4506 events to `telemetry_20260508_082716.jsonl`), again at 21:46 UTC (archived 441 events to `telemetry_20260508_214618.jsonl`). Triage comment posted on #1459 marking sub-items 4, 7, 8 already done in v1.36→v1.38 cycles. Installed binary refreshed multiple times during session (after #1604, after v1.40.0 tag, after Phase 1 CLI completion, after #1620 daemon-path sweep, after #1623 Tier 2b filter).

### Eval-baseline snapshot post-session

v3.v2 refreshed (PR #1607). Default slot (EmbeddingGemma-300m + per-cat α + Unknown=0.80):

| Split | R@1 | R@5 | R@20 |
|---|---:|---:|---:|
| test (n=109) | 49.5% | 72.5% | 84.4% |
| dev (n=109) | 56.0% | 82.6% | 94.5% |
| **aggregate** | **~52.7%** | **~77.5%** | **~89.4%** |

Δ vs pre-refresh aggregate (46.3 / 74.8 / 86.2): **+6.4 / +2.7 / +3.2 pp**. Brings agg R@K above the v1.36-snapshot range (50.9 / 76.2 / 88.6). Pure fixture re-anchoring — no retrieval-side change.

### Cumulative cqs-dead reduction (today's session)

```
                              Pre-Tier-1   Pre-Tier-2a   Post-base (#1621)   Post-cast (#1622)   Post-2b (#1623)
total cqs-dead entries:       ~114         ~80           52                  38                  35
src/language/languages.rs:    ~66          66            14                  0                   0
post_process_* (casts):       14           14            14                  0                   0
macro chunks flagged:          5            5             5                  5                   2
```

The remaining 35 includes 2 macros (`for_each_logged_batch_cmd`, `gen_log_query_dispatch`) flagged because their only invocation `for_each_logged_batch_cmd!(gen_log_query_dispatch);` lives at file scope (line 606 of `src/cli/batch/commands.rs`) — outside any chunk's byte range, so the LIKE-content scan in #1623 can't see it. Closing fully requires one of: (a) chunker change to include doc comments / file-level statements in chunk content, (b) file-system scan during macro-invocation check, (c) "module preamble" file-level chunk type. All three are architectural changes — deferred.

### What's left for Phase 1 polymorphic routing

**Phase 1 is complete.** All 60 dispatch points (6 commands × 5 kinds × 2 surfaces) ship the kind-fallback shape. Phase 2 (`cqs about` unified entry) is contingent — only ship if Phase 1 telemetry shows agents still bouncing between commands. Real-world telemetry needs 1-2 weeks of agent-driven usage post-v1.40.0 before that decision can be made.

### Earlier this day (v1.39.2 cycle, before autopilot)

The v1.39.2 cycle closed earlier today; section below captures it. PR #1593 inverted `_meta.handling_advice` to opt-in via `CQS_ULTRASECURITY=1` — addressed the alarm-shaped piece of the agent-friction equation. The autopilot session above shipped the response-size and routing-Phase-1 pieces; v1.40.0 bundles all of it.

---

**v1.39.2 shipped** — 2026-05-08, follow-on patch to v1.39.1 covering two threads that emerged from the same exploratory loop. Three PRs land for it: #1584 (cliff fix, already in v1.39.1), #1588 (α retune), #1589 (orphan GC). Plus #1582 (reranker closeout docs) and #1586 (tears) merged en route. Issue #1587 closes with #1589.

**Loop arc** (single session, started from "is the k*2 floor too high?"):

1. **Probed `CQS_HNSW_EF_SEARCH`** → byte-identical R@K across [0, 8000] because hnsw_rs internally floors ef to k (`let ef = ef_arg.max(knbn)` at hnsw.rs:1450). Outer ef knob below k is silently no-op.
2. **Pivoted to `CQS_SEARCH_CANDIDATE_FLOOR` sweep** → sharp cliff at floor=442 in dense-only (R@5 0.66 → 0.17). Traced to CAGRA's `itopk_max(14181) = floor(log2(14181) · 32) = 441`. CAGRA returned empty Vec for `k > 441`, with a comment claiming "caller falls back to HNSW" — true for `search_filtered_with_index`, FALSE for `search_hybrid` (SPLADE-fusion path used by every production query).
3. **Shipped v1.39.1** with `VectorIndex::max_k() -> Option<usize>` trait method + `cap_k_to_backend` dispatch helper. CAGRA reports its cap; both dispatch sites trim before calling the backend. R@5 0.5963 → 0.7156 restored at default floor=500 on hybrid path; dense-only no longer cliffs at floor>441.
4. **Refreshed LLM summaries** on the gemma slot to validate the post-cliff stack with current content. `cqs index --llm-summaries` ran 1,169-item Anthropic batch in ~23 min. Per-chunk coverage 60.25% → 68.70% (8,582 → 9,757 chunks covered; remaining 31.3% structurally ineligible — too short / types / non-summarizable per `collect_eligible_chunks`).
5. **Honest coverage measurement caught a silent drift**: `llm_summary_count` was 13,231 rows for 13,175 distinct chunk hashes (100.43% — 56 stale rows from prior reindexes). Filed #1587 (orphan GC).
6. **Paired α sweep** on v3.v2 218q post-refresh → most categories already at the test+dev joint optimum, but `identifier_lookup` was the one strong cross-fixture signal: dev R@5 0.8889 → 1.0000 at α=0.85 (n=18, plateau α=0.80..0.90). Shipped as #1588 (α retune 1.00 → 0.85). Test+dev paired sweep on 8 categories caught what a single-fixture sweep would have missed. Other categories' "wins" on one fixture but not the other are below the noise floor (n=8-14, single-query swings dominate).
7. **Implemented #1587 GC** in #1589: `Store::prune_orphaned_llm_summaries() -> Result<u64>` runs a single `DELETE FROM llm_summaries WHERE content_hash NOT IN (SELECT content_hash FROM chunks)`, auto-fires at end of `cqs index` after the final pending-summaries flush. Opt-out via `--no-prune-summaries` for cross-slot summary copy by content_hash. Plus `Store::llm_summary_chunk_coverage()` + `cqs stats --json` exposes `llm_summary_chunks_covered` + `llm_summary_chunk_coverage_pct` alongside the row-count `llm_summary_count` (kept for backward compat).

**Empirical impact (v3.v2 218q paired snapshot 2026-05-08, post-v1.39.2 stack):**

| Metric | Test (n=109) | Dev (n=109) | Aggregate (n=218) |
|---|---:|---:|---:|
| R@1 | 40.4% | 52.3% | 46.3% |
| R@5 | 71.6% | **78.0%** | 74.8% |
| R@20 | 81.7% | 90.8% | 86.2% |

Per-category R@5: identifier_lookup **91.7%** (was 86.7% pre-retune), multi_step 92.9%, negation 81.8%, type_filtered 69.2%, behavioral 65.6%, cross_language 63.6%, structural 62.5%, conceptual 56.0%.

Numbers are below the 2026-05-03 capture (50.9% / 76.2% / 88.6% agg) because the corpus drifted ~30% since then and the v3.v2 fixture matches by `(file, name, line_start)` strict — line shifts from audit-cycle PRs silently turn fixture hits into misses. Refreshing the v3.v2 line numbers would lift agg R@K back into the v1.36-snapshot range without any retrieval-side change. Fix bundle (cliff + α retune + GC) is a strict improvement on the current corpus state.

**Live GC verification** (gemma slot, 14,207 chunks): pre-prune 14,400 rows / 9,751 chunks covered (68.64%) → post-prune **9,317 rows / 9,750 chunks covered (68.63%)**. **5,083 orphans deleted in one pass.** Coverage % unchanged (correct — pruning orphans doesn't drop any live data). The auto-fire now keeps the table honest after every reindex; `cqs stats --json llm_summary_chunk_coverage_pct` is the metric to watch going forward.

**Three pacing lessons** from this loop:

1. **Recall-floor bumps need paired-eval sanity** (carry-over from v1.39.1). #1583's floor=500 immediately exceeded CAGRA's `itopk_max=441` on our own corpus; the formula was correct, the per-backend interaction wasn't on the radar.
2. **Honest-count metrics matter**. The 13,231-row `llm_summary_count` looked fine at 92.9% of total_chunks but the per-chunk reality was 60.25%. Once the gap was visible, #1587 was filed in 5 minutes. Lesson: any "X is N% covered" metric should be derived from the asymmetry it claims to measure (chunks-covered, not summary-rows), not the proxy that's easier to query.
3. **Test+dev paired sweeps for category-level retunes**. The v3.v2 single-fixture sweep flagged 5 categories as candidates; only `identifier_lookup` survived the cross-fixture check. Single-fixture R@5 wins at n=8-14 are below the noise floor — paired agreement is the cheap robustness check that costs nothing extra to compute.

---

**v1.39.1 shipped** — 2026-05-07. Patch release on crates.io. One PR (#1584) plus #1582 (reranker closeout docs) and #1586 (tears) en route. CAGRA itopk_max cliff in the SPLADE-fusion path; see v1.39.2 loop above for the trail end-to-end.

---

**v1.39.0 shipped** — 2026-05-07. Minor release. 88 commits since v1.38.0 across three threads:
- v1.38.0-cohort audit follow-ups (#1487–#1511)
- post-v1.38 audit cycle of 154 findings catalogued in PR #1515 — ~64 closed across ~33 cluster PRs (#1514–#1570)
- post-cycle hardening of the watch/reindex path (#1572, #1575, #1577)

**Headline operator-visible changes** (v1.39.0):
- Daemon stops SIGFPE'ing on EmbeddingGemma reindex (#1577 TRT-incompatibility blocklist; root cause #1576 upstream). Pre-fix observed 4 daemon crashes/day.
- Cross-project commands (`trace`, `callers`, `deps`, `impact`, `test_map`) work again on slot-migrated projects since #1105 (#1564 fix).
- Atomic per-file reindex (#1575) — mid-batch crash leaves no asymmetric state between `function_calls` and chunks/FTS.
- `cqs dead` noise rate cut from ~80% to ~30% (#1572 — Property + doc-extension filters).
- Graph commands now reject `--limit 0` at parse time (#1569 LimitArg fan-out).

**Operational lesson from v1.39.0 release**: #1495's `cqs-macros` workspace split (which landed AFTER v1.38.0 was tagged) had `publish = false`, blocking `cargo publish -p cqs`. Fixed in #1579: dropped publish=false, filled in standard Cargo.toml metadata, then published cqs-macros 0.1.0 first followed by cqs 1.39.0. **Going forward both crates need version bumps coordinated whenever cqs-macros's surface changes.**

## Audit umbrellas — current state

- ✅ **#1463 (P4 design-level)** — ~64 of 154 findings closed across the v1.38 cycle. Truly remaining (all genuinely big or platform-blocked):
  - **API-V1.38-6** (top-level Cli flag → subcommand parity) — clap conflict on duplicate flag definitions; needs SearchArgs locals removed AND every search-wrapping handler rewritten to read from cli.*. Lib `cqs::scout()` doesn't accept filter knobs at all.
  - **DS-V1.38-4 deeper hazard** (HNSW half-renamed-set under a lock-then-rename window) — needs bundle-into-single-file refactor + migration path. Easy mitigation already shipped in #1570.
  - **PL-V1.38-2** (SPLADE Windows umask) — needs Windows test runner.
  - **TC-HAP-V1.38-3** (`enrichment_pass` itself untested) — needs real embedder load (~91 MB).
  - 12 P4 carry-overs all tracked separately (#1512 Windows daemon, #1461 Windows ACL, etc.).
- ⏳ **#1459 (P3 API design)** — 7 of 8 sub-items shipped. Item 2 (project/ref verb consolidation) remains; user investigation found ref + project are genuinely distinct primitives.
- ✅ **#1460, #1461, #1462** — closed in v1.38.0
- ✅ **#1366** (proc-macro CLI derive) — closed by #1495
- ✅ **#1452** (skip first-pass embed) — closed by #1497
- ✅ **#1453** (per-slot SPLADE α) — closed by #1472
- ✅ **#1458** (TC Happy 5 tests) — closed in v1.39.0 cycle

## Open issues (re-verified 2026-06-09)

All 15 open issues confirmed still open against GitHub — none stale.

| # | Status | Why open |
|---|---|---|
| #106 | tier-3 | ort 2.0-rc.12 stable release blocked on upstream pykeio |
| #255 | tier-3 | Pre-built reference packages — signing/registry design (infra, not code) |
| #717 | tier-3 | HNSW mmap — needs lib swap to hnswlib-rs (nightly-only) |
| #916 | tier-2 | SPLADE mmap — audit-deprioritized (59 MB transient) |
| #1043 | platform | Windows network drives — needs Windows test runner |
| #1139 | enhancement | structural_matchers shared library — partially landed (per-language data exists; cross-language sharing remains) |
| #1140 | enhancement | Embedder preset extras map — explicitly skipped per autopilot directive |
| #1350 | architecture | apply_scoring_pipeline hand-coded — P4-14 deferred |
| #1351 | architecture | HNSW DistCosine type-baked — needs persist migration |
| #1391 | enhancement | NVRTX (TensorRT-RTX) — blocked on ORT Linux platform gate |
| #1459 | umbrella | API design — 1 of 8 items remaining (project/ref verb consolidation) |
| #1463 | umbrella | P4 — see audit umbrella state above |
| #1512 | platform | Windows daemon named pipes — needs Windows runner |
| #1573 | new | cqs dead tier 2/3 false-positive sources (filed during v1.39.0 cycle) |
| #1576 | upstream | TensorRT 10 SIGFPE during ONNX engine compilation for Gemma — filed against NVIDIA |

## Recent release history (compressed)

- **v1.45.0** (2026-06-14) — candidate-edge dead-accuracy (schema v32, PARSER_VERSION 10; #1933/#1934/#1936), worktree overlay COMPLETE (#1858/#1821), HNSW `modify_level_scale(0.5)` (#1939). Gate agg 47.7/72.0/88.5, 0 dead golds.
- **v1.44.0** (2026-06-13) — result-trust calibration metadata (#1821: edge provenance, `dead --verdict`, `rank_signals`, search overlay), macro/fn-pointer/serde call-graph edges, #1892 chunk-loss fix. Gate agg 47.7/72.5/88.5.
- **v1.43.0** (2026-06-11) — v1.42.0 16-category audit campaign close-out (107 findings, ~35 PRs): daemon per-request cache, generation-stamped HNSW sidecars, store/search/serve boundary refactor.
- **v1.39.0** (2026-05-07) — 88-commit minor release. v1.38 audit cycle + post-cycle hardening (atomic reindex #1575, TRT blocklist #1577, cqs dead noise filter #1572). Schema unchanged at v27.
- **v1.38.0** (2026-05-06) — 13 audit-driven PRs closing #1460/#1461/#1462. Per-slot SPLADE α tables (#1472), TOML overlays for FTS synonyms + classifier vocab, `cqs serve` concurrent-request cap (#1477), daemon socket TOCTOU hardening (#1478). No schema bump.
- **v1.37.0** (2026-05-05) — v1.36.2 audit close-out (#1456): 120/163 findings addressed. Dim-scaled batch sizes (#1464). Promoted `cqs::limits` to `pub`. `RerankerMode::Llm` removed.
- **v1.36.2** (2026-05-04) — critical fix (#1451): long-running `cqs index` no longer crashes with SQLITE_BUSY when concurrent `cqs` invocations overlap. busy_timeout 5s → 30s.
- **v1.36.1** (2026-05-04) — qwen3-embedding-4b preset (#1441/#1442) — 7.4 GB FP16, 2560-dim, 4096 max-seq.
- **v1.36.0** (2026-05-03) — schema v25→v26. Per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge. Net agg lift R@5 +3.7pp. 13 audit-followup fixes including critical readonly-migration bug (#1413).
- **v1.35.0** (2026-05-02) — default embedder swap BGE-large → EmbeddingGemma-300m + tokenizer-truncation correctness fix (#1384) for fine-tuned BERT-family presets.
- **v1.34.0** (2026-05-02) — post-v1.33.0 audit close-out (24 fix PRs, 129 findings) + EmbeddingGemma preset.
- **v1.33.0** (2026-05-02) — eval-matcher drift fix (#1284), placeholder-cache 30s startup tax fix (#1288, CI 38min→6min), `bge-large-ft` LoRA preset.

## Schema state

- **CURRENT: schema v32, PARSER_VERSION 13** (schema 32 = v1.45.0; post-release PV bumps on main: 11 = #1888 L5X byte_start, 12 = #1955 L5X wire-up, 13 = #1967 Dart call_query). v30 `function_calls.edge_kind` (edge provenance: call|serde_callback|macro_heuristic|fn_pointer|doc_reference), v31 `file_registry.parse_failed_parser_version` (parser-drift re-queue suppression), v32 `candidate_edges` side-table (name-keyed low-confidence call candidates; `cqs dead` → low-confidence-live; **never joined by graph queries** so it can't surface a phantom caller). Earlier recent: v28 `chunks.canonical_hash`, v29 `file_registry` + notes-sentiment CHECK.
- **v27** (post-#1497, v1.38.0+) — `chunks.needs_embedding INTEGER NOT NULL DEFAULT 0` plus partial index. Drives `--llm-summaries` skip-first-pass embed: chunks land with zero-vec sentinel + `needs_embedding=1`; HNSW build and search hide them until `enrichment_pass` clears the flag.
- v27 migration backfills `needs_embedding=1` for any pre-v27 row with `embedding_base IS NULL` so legacy chunks repopulate the base-HNSW on the next index pass.
- HNSW build, `Store::search_by_name`, `Store::search_fts_only` all filter `WHERE needs_embedding = 0`.

## Adding a top-level CLI command (post-#1495)

Declare the variant with `#[cqs_cmd(group = "a"|"b", batch = "cli"|"daemon"|"runtime")]` on `Commands` (definitions.rs), implement the handler in `commands/<area>/`, add a small `cmd_<snake>_dispatch` shim in `commands/dispatch_shims.rs`. The shim destructures the variant out of `&Commands` and forwards to the handler. Cfg-gated variants get `#[cfg(feature = "...")]` next to `#[cqs_cmd(...)]` and the derive forwards it to every emitted arm.

## Operational pitfalls (rolling forward)

- **Main is protected** — `git push` to main is rejected. Always create a branch + PR.
- **Always use `--body-file` for `gh pr create`** — never inline heredocs (PowerShell mangles + Claude Code captures the whole multiline as a permission entry, corrupting `settings.local.json`).
- **WSL git credential helper** — `git push` from `~/training-data` needs `git config --global credential.helper '/mnt/c/Program\ Files/Git/mingw64/bin/git-credential-manager.exe'`. Already configured globally for cqs.
- **Squash-merge + rebase trap** — when a PR is squash-merged and a follow-up branch was based off it, rebase fails. Cherry-pick onto fresh main.
- **Auto-merge disabled** — `gh pr merge --auto` returns "auto merge is not allowed". Watch CI manually + merge when green.
- **`cargo publish --features gpu-index` fails verification** — the workspace `[patch.crates-io]` cuvs-patched fork doesn't ship in the package. Use plain `cargo publish` (no features); gpu-index is feature-gated.
- **cqs-macros must publish first** — when bumping cqs that depends on cqs-macros, publish cqs-macros to crates.io first or `cargo publish -p cqs` errors with "no matching package named cqs-macros".
- **`cargo publish` 413 errors** = excludes missing. `evals/` etc. are in `Cargo.toml`'s `exclude` list.
- **`enumerate_files` returns relative paths** — joining with project root before `parse_file()` is mandatory; otherwise the parser resolves against cargo's CWD.
- **`type_edges` parser tracks signature-level uses only** — params, returns, fields. Not expression-level (`let x = T::new()`). Test assertions on "who uses type T?" must check signature users.
- **Daemon GPU "activity" is misleading** — ORT keeps the CUDA context warm; A6000 sits at P2/1800MHz/84W with 0 actual compute work. True idle (P8) requires stopping the daemon.
- **CI cqs test job runs ~6-12 min** post-#1288/#1302 (was 38 min). Fixed-interval `/loop` heartbeats > 60min should go to cloud schedule (`/schedule`).
- **HF preset tokenizers may ship `truncation: {max_length: 512}` baked into `tokenizer.json`** — affects bge-large-ft, v9-200k, coderank. Cqs windowing/counting must clone-and-disable truncation before counting tokens. See PR #1384. When adding a new preset, check `python -c "from tokenizers import Tokenizer; print(Tokenizer.from_file('tokenizer.json').get_truncation())"` first.
- **Triage-flip durability** (audit-cycle lesson): force-pushed rebases naively resolve triage-row conflicts using older agents' pre-flip snapshots. Mitigation: keep triage flips append-only OR move each PR's triage update into a separate narrow PR per cluster.

## Collaboration calibration (still load-bearing)

1. **"Self-starter and self-orienter" is the favored mode.** Default toward action over consultation when the next move is clear.
2. **"Little give-ups" are the failure pattern.** Verify artifacts; investigate silences; redo thin returns; don't tolerate Monitor timeouts as longer waits.
3. **No time estimates in specs.** Wall-time predictions are unreliable; describe what/why/gate-criteria, not effort.
4. **Knobs that are knobs, not blockers, go in an Ablations table** — not in Open Questions.
5. **Don't suggest ending a session.** 1M context, plenty of headroom, user works continuously.

## Eval baselines

Canonical slate: `evals/queries/v3_test.v2.json` (109q) + `evals/queries/v3_dev.v2.json` (109q). Both fixtures refreshed 2026-04-25 (PR #1109).

**CURRENT (v1.45.0 RELEASE GATE, 2026-06-14, 16,720 chunks, schema 32):** agg **47.7 / 72.0 / 88.5** — test 46.8/67.9/86.2, dev 48.6/76.1/90.8, **0 dead golds**. Full writeup + verdict in `~/training-data/research/eval.md`. Not comparable to the 2026-05 baseline below — different corpus, and the #1891 chunk-loss correction reset the baseline at v1.44.0 (47.7/72.5/88.5).

**Baseline (v3.v2 218q dual-judge, 2026-05-08 post-v1.39.1 cliff fix + LLM summaries refresh + identifier_lookup α retune):**

| Metric | Test | Dev | Agg |
|---|---:|---:|---:|
| R@1 | 40.4% | 52.3% | 46.3% |
| R@5 | 71.6% | 78.0% | 74.8% |
| R@20 | 81.7% | 90.8% | 86.2% |

Per-category R@5 (post-retune): identifier_lookup 91.7% (was 86.7% pre-retune; agg n=36), multi_step 92.9%, negation 81.8%, type_filtered 69.2%, behavioral_search 65.6%, cross_language 63.6%, structural_search 62.5%, conceptual_search 56.0%.

Numbers below the 2026-05-03 capture (44/55 R@1 → 40.4/52.3, 67.9/78.0 R@5 → 71.6/78.0) reflect: (a) corpus drift since 2026-05-03 (13,359 → 14,203 chunks; eval matches `(file, name, line_start)` strict so audit-cycle line shifts silently turn hits into misses — see feedback memory "Eval Line-Start Drift"); (b) the cliff fix and α retune are strict improvements on the current corpus state. Refreshing the v3.v2 fixture line numbers would lift agg R@K back into the v1.36-snapshot range without changing retrieval quality. v4 fixtures (1526/split, 14× v3 N) exist for any A/B that needs tighter noise floors.

**Strategic frontier candidates** (when redirected): wire USearch / SIMD brute-force as `IndexBackend` candidates (#1131 trait scaffolding already in); HyDE on v3 dev with index-time per-category routing (never properly tested at v3 N); knowledge-augmented retrieval (call/type graph as structured filter; multi_step queries weakest at 28-43% R@1); expand v3 → v4 fixture scale (1526q/split — current 109q is data-bound for per-category sweeps).

**Reranker V2 closed** — 2026-05-07 re-eval against post-v1.39.0 stack confirmed all four reranker variants (off-the-shelf MiniLM + 3 in-domain UniXcoder retrains) remain net-negative on v3.v2 (test R@5 -10 to -16pp, dev R@5 -16 to -26pp). Gap actually widened on dev as stage-1 strengthened. R@20 within 1-4pp of baseline across all four — gold is in the pool; every reranker demotes it. Bottleneck is fixture-size (109q × 30 candidates too thin for 125M cross-encoder) + stage-1 already strong; not a tunable knob. Future revisit gated on v4-scale labelled fixture OR a 5× bigger base (bge-reranker-large at ~3× latency). README now documents the regression at v1.39.0.
