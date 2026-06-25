# Roadmap

## Current: v1.49.0 (cut 2026-06-24, crates.io + GitHub Release)

Minor — **v1.48.0 16-category audit fix cycle.** A full audit (16 parallel category auditors → per-category adversarial verification → triage, all opus) produced 36 verified findings → 19 rows; **all P1–P3 + 2 of 3 P4 fixed** across PRs #2038–#2042. Scoring path **byte-identical** to v1.48.0 — retrieval unchanged (recall carried: 72.0% R@5 / 47.2% R@1 / 87.2% R@20).
- **Security:** RT-RELAY honest-relay completion (#2039) — `explain --tokens` / `read --focus` / `trace` / kind-fallback signatures now scanned; **scan == relayed, both directions**; `leading-directive` corrected to line-start anchoring after a review caught an "instead of" false-positive explosion. Parser depth rail made non-defeatable (#2040) — `CQS_PARSER_MAX_WALK_DEPTH` clamped `[1,800]`, only-lowerable.
- **Data-safety (#2041):** the daemon/MCP notes-write reindex was inotify-only → a note could be silently never indexed on WSL `/mnt/c`; fixed with `SharedNotesSignal` (writer flips, watch loop drains every tick, inotify-independent). Plus the relay 1 MiB cap → env-overridable resolver and the `cqs_index` wrong-slot lie → client-facing error.
- **MCP surface (#2042):** two agent-facing docs-lies (fabricated README tool list + phantom `cqs_wait_fresh`/`cqs_status` in the `cqs_index` description) fixed + guard-tested; overlay-schema honesty; bounded bridge read; serve_stdio non-unix fail-fast; perf/obs.
- **Remaining:** #2043 (`*Args`/four-hand-mirrored-enumeration architecture — the carried-forward row was mis-attributed to #1459, which is actually API-ergonomics; corrected), #2044 (cap-boundary nit). Triage: `docs/audit-triage.md` (19 rows resolved).

## v1.48.0 (cut 2026-06-24, crates.io + GitHub Release)

Minor — **MCP server re-introduction** (`cqs mcp`, removed in v0.10.0, now riding the command cores; ~10× smaller than the old in-process server). Scoring path **byte-identical** to v1.47.0 (recall carried: 72.0% R@5 / 47.2% R@1 / 87.2% R@20; the session's index growth to 17.2k chunks is corpus churn, not a scoring change).
- **MCP (Phases 0→2):** Phase 0 schemars `JsonSchema` on the command cores as the inputSchema source (#2019); Phase 1 a stdio↔daemon-socket bridge — daemon JSON-args path (#2023) + the `cqs mcp` bridge, 19 read-only `cqs_*` tools (#2029); Phase 2 gated mutating tools — `cqs_notes_add/update/remove` + `cqs_index` fire-and-forget behind opt-in `CQS_MCP_ENABLE_MUTATIONS`, destructive set withheld **by absence**, daemon `Store` stays read-only (#2033/#2034). Decision ledgers in `docs/plans/2026-06-2{3,4}-mcp-phase{1,2}-*.md`.
- **Security:** RT-PARSE — a stack-overflow DoS via deeply-nested adversarial indexed content (cqs's recursive `calls.rs` walk SIGABRT'd `cqs index`/the daemon — the incomplete-sweep straggler; tree-sitter itself was fine) fixed with a depth rail + parse timeout + stack-sized parser pool (#2028/#2031; outer-layer C-scanner subprocess sandboxing deferred, PoC-gated, #2027). RT-RELAY — `context`/`explain` now scan doc-comments + signatures for injection, not just content (#2024).
- Notes groom 238→220 (#2022); daemon `in_flight` RAII guard against panic unwind (#2018); telemetry/serde-output guards.
- **Process note:** this session's deploys' `systemctl --user restart` ran in background shells and silently no-op'd (no session DBUS/XDG) — the daemon served a stale binary for 12h until a verified foreground restart. Lesson banked.

## v1.47.0 (cut 2026-06-16, crates.io published)

Minor — PARSER_VERSION 13→14 (L5X bare-routine ST wrap-normalization, #2005). Recall **72.0% R@5 / 47.2% R@1 / 87.2% R@20** (R@5 dead-flat vs v1.46.0; the first-gate 71.1 was a two-pass `--llm-summaries` enrichment artifact, not a regression). v1.46.1 was the patch fixing the v1.46.0 macOS cross-build `E0277`.

## v1.46.0 (cut 2026-06-15)

Minor release. Schema v32, PARSER_VERSION 10→13 (L5X byte_start + production wire-up, Dart call_query — reindex required). Recall: **72.0% R@5 / 48.7% R@1 / 87.6% R@20** (v3.v2 dual-judge, 218q; R@5 headline flat vs v1.45.0, R@1 +1.0 / R@20 −0.9 = PV13-reindex corpus churn, default scoring path byte-for-byte unchanged).

**Idle loop + 3 audit rounds — 10 fixes merged, PARSER_VERSION 10→13.**
- **Idle loop (5 issues):** #1942 (candidate_edges watch write gap), #1943 (overlay candidate recompute → low-confidence-live), #1945 (test-fn `ChunkType::Test`) + #1946 (framework-trait `KNOWN_GAP`) closing #1573, #1947 (L5X file-relative byte_start, PV→11). `cqs dead --verdict dead` → 1 genuine entry (was 9).
- **Audit refill (R1–R3, 7 auditors):** found + fixed #1951 (HNSW DotProduct panicked on f32 unit-norm vectors → `DistDotClamped`), #1954 (dead-classifier verdicts steerable by adversarial indexed content → `ChunkType` gates + SECURITY.md note), #1955 (the L5X/L5K custom ST extractor was **dead on the production index path** → wired via its own `LanguageDef`, PV→12), #1966 (cross-project callers/impact dropped trust-ordering at the cap/first-discovery boundary → a remote `doc_reference` evicted/mislabeled a real `call`), #1967 (Dart `call_query` **never wired since #816** → call graph silently empty, PV→13). Plus #1950 (full v10→v32 migration-chain frozen-artifact guard) and #1971 (embedder proptests + cosine clamps). Clean bills: daemon cache/epoch concurrency (loom-verified), migration read/migrate paths.
- **Security #1953 (#1956):** daemon overlay-root validation hardened against forged/symlinked worktrees + a TOCTOU swap — git-registry membership (`git worktree list`) + a dirfd inode-pin (`openat2 RESOLVE_NO_SYMLINKS`, git via `/proc/self/fd/N`). 4 attempts, each bypass caught by review. Residual same-uid check-then-recreate race + non-CLOEXEC fd tracked low-severity in #1969.
- 6 dependabot dependency bumps.

**Landed since:** #1952 (overlay/reference merge sorted incomparable score-frames → merged-set rerank, PR #1974; default no-rerank path byte-for-byte unchanged).
**Deferred:** #1969 (overlay same-uid TOCTOU residual; airtight = kernel peer-cwd redesign). #1973 (nightly model-cache hardening, CI-infra). #1975 (pool-truncation, low-sev). #1573-Tier-3a (dyn-dispatch dead false-positives, deferred-by-design).

## Previous: v1.45.0 (cut 2026-06-14, crates.io published)

Minor release. Schema v32, PARSER_VERSION 10. Recall: **72.0% R@5 / 47.7% R@1 / 88.5% R@20** (v3.v2 dual-judge, 218q; R@5 headline; R@1+R@20 flat vs v1.44.0, R@5 −0.5 agg = corpus-determinacy churn, scoring code unchanged — the reduced-indeterminacy call). Three threads:

**Candidate-edge dead-code accuracy (schema v32, PARSER_VERSION 10; #1933/#1934/#1936).** A name-keyed `candidate_edges` side-table — **never joined by graph queries**, so `cqs dead` classifies candidate-only callees as `low-confidence-live` (not `dead`) without ever inventing a false caller. The parser emits the references it previously dropped, four kinds: bare/macro arg-unresolved, serde container/with-module. A seam audit caught (and fixed) overlay-introduced worktree deaths being mislabeled `low-confidence-live` — only the seam-auditor found it.

**HNSW `modify_level_scale(0.5)` (#1939).** The default `1/ln(M)` over-populates upper layers under parallel insert → a fraction of points self-unreachable (orphaned); 0.5 cut orphaning ~26–100× (M=32: 0/40 builds) with recall flat-to-up. Reduced indeterminacy. Root cause: parallel-build graph-topology nondeterminism (layer heights + insertion order) — NOT an entry-point write race; `entry_point` is RwLock-protected in 0.3.4 (the earlier "write race" framing was a misread, corrected with the maintainer in hnswlib-rs#32). A higher M also cuts it (M=16→62, M=48→7.5 orphans/build on real clustered data); `modify_level_scale` remains the cheap lever.

**Auditor-family expansion (the #1826 trio → a six-auditor family).** Derived the test suite's structural nulls rigorously and staffed each with an orthogonally-shaped auditor whose deliverable is a *durable guard* (a test that bites), not just a find. Two genuinely new nulls beyond the trio: **sweep-auditor** (#1897 — the incomplete sweep: a change applied to N-1 of N peer sites) and **legacy-state-auditor** (#1899 — the version boundary: current code mishandling old-bytes only a past version could have written). Plus two *enumerable* orthogonal auditors that sit beside the correctness family: **adequacy-auditor** (#1902 — the meta-null: does the suite's own assertion bite, via mutation testing) and **red-team-auditor** (#1901 — security axis; oracle is a trust boundary; #1903 added the RT-EXFIL egress category; the `/red-team` skill refactored to a thin 6-category orchestrator). The **Agent-tool grant/withhold principle** (#1896): grant subagent-spawning where findings decompose (general auditor, code-reviewer, red-team, adequacy); withhold from the relational five (seam/property/interleaving/sweep/legacy-state) whose finding *is* a relation. property-auditor gained three shapes (stateful-sequence, fault-injection, execution-profile); seam-auditor gained the error-path taxonomy. **Each auditor was test-fired before shipping** and each earned a real catch, now pinned as durable guards (#1898 gather depth-cap straggler fix; #1904 bundle: rrf_fuse vacuous-proptest killing test, full v10→v31 migration-chain guard, overlay-LRU loom models, daemon-socket overlay-root guards). The taxonomy diagnostic (relation → a null; surface/condition → a manifestation that folds) suggests the space is closed — no clean seventh.

**#1858 worktree-overlay phase-2 — COMPLETE; #1858 + #1821 CLOSED.** Every graph-adjacent command now reflects worktree edits in a `.claude/worktrees/` checkout: search + scout/gather/task seed (#1900), callers/callees (#1908), impact/dead (#1921 — folding in #1910's dead-code `doc_reference` edge-kind fix), and review (#1924). Each result carries `_meta.overlay_graph` (`full` / `callers-only` for impact+review whose tests/transitive/risk sections stay parent-truth / `seed-only` / absent), gated on *actual* overlay participation — never `overlay.is_some()` (the seam-audit discipline, won the hard way in PR1's fix-round). The agent-def "treat results as hints" re-read tax is retired. `cqs ci`'s embedded review + dead were folded into the overlay too (#1928, closing #1926).

**Key-design class closed — the incomplete-sweep-of-the-hardening** (a hardening applied to one key but not its siblings, fixed by centralization): **#1909 freshness** (PR #1917 — one shared `FileIdentity` + `DataVersionProbe` across all three `index.db` readers; two were stragglers on pure `(mtime,size)`) and **#1911 chunk-id** (PR #1923 — markdown table/whole-file id collision + window-suffix re-id blindness; single-source `chunk_id_suffixed`; **PARSER_VERSION 8→9**, reindex required). Auditor findings disposed alongside: #1914 (loom-proven cross-project WAL stale-serve race → EpochCell tag), #1912 (neighbors limit clamp), and adequacy killing tests for `name_match.rs` vacuous assertions (#1918, 24% mutant survival → 4 calibrated tests).

**Apex docs (#1915):** the principal-loop design sketch deepened with the plumb-line / cornerstone model of the values layer (parked).

**Parked:** audit-loop — a perpetual auditor factory (`docs/plans/2026-06-13-audit-loop.md`): ratchet durable-guard coverage across a region×shape matrix, invalidation-driven; 13 open questions. principal-loop — the user-as-agent sketch (`docs/plans/2026-06-14-principal-loop.md`): 3 leverage points (taste/caution/wisdom), deepened with the plumb-line/cornerstone values-layer model. Both user-to-review.

**Open (idle-loop queue):** #1935 (candidate-edges: watch zero-chunk oversize-fn path doesn't write), #1937 (candidate-edges: recompute under the worktree overlay — Direction-B), #1888 (L5X region-relative byte_start — forces a PV bump); older backlog #1573 / #1804 / tier-3 umbrellas (#1459/#1463). Closed in the v1.45.0 cycle: #1916 (#1930), #1925 (#1929), #1926 (#1928), #1893 (#1932), #1905 (#1931).

## Previous: v1.44.0 (cut 2026-06-13, crates.io published)

Minor release. Schema v31, PARSER_VERSION 8. Recall: **72.5% R@5 / 47.7% R@1 / 88.5% R@20** (v3.v2 dual-judge, 218 queries; R@5 is the headline — R@1 is noise-sensitive). Two threads plus a headline bugfix:

- **Result-trust program — calibration metadata so agents act on results without a defensive re-read (#1821, all four axes shipped).** §1 edge provenance (`function_calls.edge_kind`: call/serde_callback/macro_heuristic/fn_pointer/doc_reference) + §2 `cqs dead --verdict` self-classification (#1836); §4 per-result `rank_signals` (why a hit ranked — dense/fts/name_match/note_boost/parent_boost/sparse, #1847); §3 worktree search overlay, **default-on for `.claude/worktrees/` CWDs** so `cqs search` reflects your edits not the parent branch (#1850/1851/1853/1866, search-only). The macro/fn-pointer/serde edge passes (PARSER_VERSION 3→5, #1808/1819/1822) cut confident-dead 44→17.  **(#1821 closed in v1.45.0 — the #1858 overlay completion emptied the hedge clause.)**
- **Auditor trio complete + applied (#1826).** seam/property/interleaving auditor agent defs — the three orthogonally-shaped auditors that staff the test suite's structural null (the space *between* units that happy/sad per-unit coverage can't express). Each found-or-guarded a bug class the example suite cannot. proptests (codec round-trip, HnswMeta contracts, daemon-vs-CLI equivalence) + loom models (watch reconcile-vs-query, index-build NO-LOSS/CALL-GRAPH-FIDELITY).
- **HEADLINE FIX: non-deterministic silent chunk loss in the full-corpus build (closes #1891+#1886, #1892).** GPU/CPU embed stages work-stealing a cloned `parse_rx` with no cross-stage ordering ∘ #1835's per-file prune-flush assuming in-order delivery → a file straddling a parse-batch boundary had its fingerprint overtake its chunks → partial flush + prune → real code silently dropped (struct/impl/fn vanish, tests survive; ~hundreds/index). Fixed by construction (parser file-alignment + store-stage order-independence net), **loom-proven** under every interleaving. The bug was a seam-auditor find. Recovered ~765 chunks (R@5 +0.9 / R@20 +1.3).

**(Done in v1.45.0):** #1858 worktree overlay phase-2 completed — Part A seed-routing + Part B graph shadowing landed, emptied the hedge, closed #1821. See **Current: v1.45.0** above.

## Previous: v1.43.0 (cut 2026-06-11, crates.io published)

Minor release. The **v1.42.0 full 16-category audit campaign** — 107 fresh findings + 50 carried-forward (v1.40), ~35 PRs (#1737–#1795) in ~36h, every P1/P2 and P3/P4 tier closed. Headlines: per-request daemon cache layer (vector index / file_set / notes / cross-project context cached across queries — ~400ms CAGRA load + reference-store re-merges eliminated per request); generation-stamped HNSW sidecars with dirty self-heal; store/search/serve schema-ownership boundary refactor; crash-safe fingerprint stamping on both pipelines; daemon↔CLI output parity; index-time hardening against poisoned inputs. Plus opt-in tiered-index backend (incremental adds replace periodic rebuilds, behind a commented fork pin), `cqs status --watch` daemon stats, slot-parallel reindex, adaptive watch debounce, `DistanceMetric` threading. Triage is the single source of truth in `docs/audit-triage.md`.

## Earlier: v1.42.0 (cut 2026-06-09)

Minor release. Schema v28. Four threads:
- **Comment-canonical embedding reuse** (#1677): `chunks.canonical_hash` (comment-stripped + whitespace-collapsed blake3) keys both embedding-reuse paths on both surfaces (bulk pipeline + watch incremental); comment/formatting-only edits no longer re-embed the corpus.
- **Comment hygiene** (#1673 + #1676): ~1,500+ provenance refs stripped from comments across 231 files, present-tense rewrite; CI provenance lint (fmt job) keeps it clean.
- **cuvs fork retired** (#1679): our CAGRA serialize/deserialize + search_with_filter wrappers shipped in official cuvs 26.6.0 — pin `=26.6`, conda libcuvs 26.06, `[patch.crates-io]` gone. JIT LTO CAGRA search: no cold-start tax (cold CLI 8.4s→7.5s, warm daemon ~0.55s unchanged). Next upstream contribution: **IVF-SQ Rust bindings (#1678, agent in flight)** — 4× memory reduction candidate for multi-slot A/Bs once bound.
- **Eval drift-proofing** (#1675): 16 Python harnesses match gold on `(file, name)` like the Rust matcher; refactor line-shifts can no longer fake regressions.

**Post-v1.40.0 audit verification (2026-06-09, audit-mode):** every still-open finding re-verified against code — 24 confirmed open, 6 partially fixed, 1 fixed, 2 invalidated (incl. the "missing chunks.name index" premise: `idx_chunks_name` was in base schema all along). Verified priority queue in `docs/audit-triage.md` § Verification 2026-06-09; the standing 5h fix loop works from it. Also queued: #1680 (MSRV 1.95→1.96, `assert_matches!` sweep, clippy --all-targets cleanup). New finding from live failure: daemon CLI client has no socket read timeout (request racing a daemon restart blocks forever).

## Previous: v1.41.0 (cut 2026-05-20, crates.io published)

Minor release. 30 commits since v1.40.0; no breaking changes (v1.40.0 carried the SNR wire-format flip). Three threads: polymorphic routing Phase 1 completion (matrix below), post-v1.40.0 audit cycle closure, `cqs dead` false-positive reduction (table below).

**Post-v1.40.0 audit cycle (closed in v1.41.0):** 16-category audit, 150 raw findings → 78 triaged → 9 closeout PRs (#1626–#1634). 21 of 23 P1s closed; 2 deferred with rationale (DS-V1.40-1 daemon Store cache invalidation, DS-V1.40-7 sentiment CHECK constraint — tracked in Open Issues below). Highlights: lying-docs sweep (#1626), Tier 2b `filter_invoked_macros` correctness — LIKE → GLOB, Rust-only guard, recursive-macro self-exclusion (#1627), Posture/OutputFormat env-var caching + truthy aliases (#1629), `worktree_stale` signal restored under V2Bare (#1630), atomic backup-restore sidecar ordering (#1631), kind-fallback DoS caps (#1632), BFS sentinel + Tier 2a anchoring (#1633).

Also: `cqs chat` honors `CQS_OUTPUT_FORMAT=v1` (#1650, first external-contributor fix), `cli_chat_format_test` aligned with the slim envelope — closed 8 consecutive nightly failures (#1655), screw-mcp/screw-tape moved to its own repo (#1656).

**Polymorphic-routing Phase 1 matrix (first cell v1.40.0 #1612; completed in v1.41.0):**

| Command | Function | Type | Const | Module | Ambiguous |
|---|---|---|---|---|---|
| impact | ✓ #1612 | ✓ #1616 | ✓ #1612 | ✓ #1616 | ✓ #1616 |
| callers | ✓ | ✓ | ✓ | ✓ | ✓ (#1617) |
| callees | ✓ | ✓ | ✓ | ✓ | ✓ (#1617) |
| test-map | ✓ | ✓ | ✓ | ✓ | ✓ (#1618) |
| trace | ✓ | ✓ | ✓ | ✓ | ✓ (#1618) |
| deps | ✓ | ✓ | ✓ | ✓ | ✓ (#1618) |

**60/60 Phase 1 dispatch points.** CLI-direct (`cmd_*`, 30 cells) shipped #1612, #1616, #1617, #1618. Daemon-path (`dispatch_*` in `src/cli/batch/handlers/`, 30 cells) shipped #1620, originally with shared `try_kind_fallback` + `build_kind_fallback_value` helpers. Both surfaces consult `cqs::kind::classify_hits` against an exact-name lookup before their happy-path query. (The command-core unification, #1689, later collapsed both surfaces onto a single `*_core` per command; the `try_kind_fallback` / `build_kind_fallback_value` / `KindNotes` helpers named above were deleted in that refactor, replaced by `graph/mod.rs::detect_fallback` + the shared `KindFallbackOutput`.)

**`cqs dead` false-positive reduction (issue #1573):**

| Tier | PR | What it added | Effect |
|---|---|---|---|
| 1 (closed pre-session) | #1572 | `cqs dead` noise filter base | ~50% of v1.39 false positives |
| 2a base | #1621 | `field_initializer` + `(call_expression arguments)` patterns in `rust.calls.scm` | -52 (66 → 14 in `src/language/languages.rs`) |
| 2a follow-up | #1622 | `type_cast_expression` patterns for `Some(fn as T)` casts | -14 (closes `post_process_*` to 0) |
| 2b partial | #1623 | `WHERE content LIKE '%<name>!%'` filter in `dead_code.rs` | -3 of 5 macro false positives |
| 2b correctness | #1627 (v1.41.0) | LIKE → GLOB (case-sensitive, no `_` collision), Rust-only guard, recursive-macro self-exclusion, GLOB metachar escape, +7 tests | closes the Tier 2b false-match edge cases |

Cumulative: ~114 → 35 cqs-dead entries (as of v1.41.0). Remaining 35 includes 2 macros (`for_each_logged_batch_cmd`, `gen_log_query_dispatch`) flagged because their only invocation is at file scope (line 606 of `src/cli/batch/commands.rs`) — outside any chunk's byte range. Closing fully requires a chunker change to include doc comments / file-level statements; deferred as architectural.

**v1.40.0 (2026-05-08):** 14-PR minor release driven by agent-adoption telemetry (search rate dropped 79% → 6% mid-April → early May). **Breaking:** CLI direct `--json` emits the bare JSON payload (no envelope wrap); `CQS_OUTPUT_FORMAT=v1` is the consumer-migration hedge. Contents:
- **SNR restoration Phases 1-4 (#1601, #1602, #1604, #1609, #1613):** `Posture` enum + per-result skip-when-default + posture-gated force-emit + slim batch/daemon envelope + CLI direct → bare payload. ~30% smaller per result + ~70 KB saved per 1000-line fixture batch.
- **Polymorphic routing Phase 1 plumbing + first cell (#1610, #1612):** `cqs::kind` module (Kind enum, classifier, `Store::lookup_by_name`, `detect_kind_for_store`) + `cqs impact <const>` returns kind-labeled definitions instead of empty.
- Plus: env-var-docs hardening (#1606), v3.v2 fixture refresh (#1607, agg R@K +6.4/+2.7/+3.2pp), gitignore housekeeping (#1603), telemetry reset.

Verified eval baseline post-release (v3.v2 218q dual-judge, default slot): test 49.5/72.5/84.4, dev 56.0/82.6/94.5, agg 52.7/77.5/89.4 R@1/R@5/R@20 — above v1.36-snapshot range.

**v1.39.0 (2026-05-07):** 88-commit minor release. Schema v27 (bumped from v26 in #1497). Three threads:
- v1.38.0-cohort audit follow-ups (#1487–#1511): proc-macro `#[derive(CqsCommands)]` (#1495 closes #1366), schema v27 `--llm-summaries` skip-first-pass embed (#1497 closes #1452), atomic SPLADE/CAGRA save with `.bak` rollback (#1491/#1492), `cqs ref reindex` LLM/HyDE flag parity (#1506), `cqs project search` filter knobs (#1507), `cqs index --model` drift detection (#1505), `[index.policy]` config section (#1511).
- Post-v1.38 audit cycle: 154 findings catalogued in PR #1515; ~64 closed across ~33 cluster PRs (#1514–#1570). Sub-clusters covered needs_embedding wiring (DS-V1.38-1/2/3/8), lying-docs sweep (DOC-V1.38-1..10), stored_model_name lossy-caller migration (EH-V1.38-1/2/3/4), algorithm correctness sweep (AC-V1.38-1/2/3/5/6/9), security hardening (SEC-V1.38-1/2/3/4/8/9), env-override sweep (10 new knobs across SHL-V1.38-*), TC-HAP-V1.38 test backfill (9 of 10 sub-items).
- Post-cycle hardening of the watch/reindex path: atomic per-file reindex (#1575 closes #1574), TRT-incompatibility blocklist for Gemma (#1577 closes #1576), `cqs dead` noise filter (#1572 closes ~50% of false positives).

Headline operator-visible changes:
- Daemon stops SIGFPE'ing on EmbeddingGemma reindex (#1577 — observed 4 daemon crashes/day pre-fix).
- Cross-project commands (`trace`, `callers`, `deps`, `impact`, `test_map`) work again on slot-migrated projects since #1105 (#1564 fix).
- Mid-batch crash leaves no asymmetric state between `function_calls` and chunks/FTS (#1575).
- Graph commands now reject `--limit 0` at parse time (#1569 LimitArg fan-out).

Surface changes that justify minor not patch: `EmbedderError::HfHub` → `ModelDownload` rename (#1567), `BatchProvider::set_on_item_complete` trait method dropped per #1470, new `CQS_FORCE_TENSORRT` env override (#1577), 10+ new env knobs documented in README. Schema v27 already lived in code from v1.38.0 development but ships with this release. **First publish of `cqs-macros 0.1.0`** to crates.io (workspace split landed in #1495).

**v1.38.0 (2026-05-06):** 13 audit-driven PRs closing three umbrella tracking issues from the v1.36.2 audit (#1460 P3 Extensibility, #1461 P3 Security, #1462 P3 misc CQ/RM). Headline: per-slot SPLADE α tables (#1472), TOML overlays for FTS synonyms + classifier vocab (#1482, #1483), `cqs serve` outermost concurrent-request cap (#1477), daemon socket parent-dir TOCTOU hardening (#1478), ChunkRow ordinal access on the search-hydration hot path (#1468), daemon accept loop `libc::poll` instead of busy-poll (#1471). Six surface deletions justify the minor bump: `nl::generate_nl_description` + `generate_nl_with_template` (#1473), `rerank: bool` field on `Cli`/`SearchArgs` (#1479), `BatchProvider::set_on_item_complete` (#1470).

**v1.37.0 (2026-05-05):** v1.36.2 16-category audit close-out (#1456) — ~120 of 163 audit findings addressed. All 56 P1s + 13 of 14 P2s shipped. Plus dim-scaled batch sizes (#1464). `cqs::limits` promoted from `pub(crate)` → `pub`. ~28 deferred P3/P4 items filed as tracking issues #1457-#1463.

**v1.36.2 (2026-05-04):** critical fix — long-running `cqs index` runs no longer crash with `(code: 5) database is locked` when a concurrent short-lived `cqs` invocation overlaps the indexer's writes (#1451 `Store::drop` checkpoint TRUNCATE → PASSIVE; the indexer's WAL contention with `cqs stats` / similar polling was surfacing fatal mid-transaction `SQLITE_BUSY`). Plus `busy_timeout` 5s → 30s defense-in-depth (#1450) and 5 dependency bumps from dependabot.

**v1.36.1 (2026-05-04):** `qwen3-embedding-4b` preset (#1441/#1442) — 7.4 GB FP16, 2560-dim, 4096 max-seq. Production-ready first-class preset alongside the v1.36.0 8B ceiling probe.

**v1.36.0 (2026-05-03):** schema v25 → v26 (composite `(source_type, origin)` index on `chunks`; auto-migrated on first read-write open). Headline: per-category SPLADE α retuned for EmbeddingGemma + Unknown=0.80 catch-all hedge — v1.35.0 shipped EmbeddingGemma as the new default but inherited per-category α defaults that were tuned for BGE-large (2026-04-15/16). A fresh sweep on the gemma slot landed different optima: `Structural` 0.90→0.60, `Behavioral` 0.80→1.00, `Conceptual` 0.70→0.80, `TypeFiltered` 1.00→0.00, `CrossLanguage` 0.10→0.70, plus `Unknown` 1.00→0.80. Net agg lift: R@1 +1.8pp, R@5 +3.7pp, R@20 +2.4pp. Plus 13 audit-followup fixes including a critical bug catch (#1413): readonly opens with stale schema were attempting to migrate and failing with SQLite "attempt to write a readonly database" errors. Fixed by surfacing `SchemaMismatch` on stale-schema readonly opens.

**v1.35.0 (released 2026-05-02):** default embedder swap BGE-large → EmbeddingGemma-300m (308M, 768-dim, 2K context). Plus tokenizer-truncation correctness fix (#1384) that affected fine-tuned BERT-family presets (bge-large-ft, v9-200k, coderank).

**v1.34.0 (2026-05-02):** bundled the post-v1.33.0 audit close-out (24 fix PRs, 129 findings closed) plus pre-audit feature work — EmbeddingGemma-300m preset (#1301), `cqs eval --reranker` (#1303), `slow-tests` Phase 2 (#1302), ci-slow.yml stabilization.

**v1.33.0 (2026-05-02):** eval-matcher drift fix (#1284, ~38% of gold chunks were going invisible after audit-driven line shifts), placeholder-cache 30s startup tax fix (#1288, CI 38min→6min), chunk-orphan pipeline prune (#1283), `bge-large-ft` LoRA preset (#1289), daemon test refactor + nightly CI workflow (#1292, #1286 Phase 1).

**Eval baseline (v3.v2 218q dual-judge, historical snapshot at v1.39.0 — current baseline is under "Previous: v1.40.0"):**

| Slot | Test R@5 | Dev R@5 | Test R@20 | Dev R@20 |
|---|---:|---:|---:|---:|
| **embeddinggemma-300m + v1.36 α (default)** | **67.9-69.7%** | **78.0-80.7%** | **80.7-84.4%** | **91.7-92.7%** |
| BGE-large (pre-retune) | 68.8% | 75.2% | 82.6% | 86.2% |
| bge-large-ft (pre-retune) | 71.6% | 75.2% | 85.3% | 87.2% |
| v9-200k (pre-retune) | 67.9% | 69.7% | 79.8% | 81.7% |
| nomic-coderank (pre-retune) | 67.0% | 68.8% | 78.0% | 79.8% |

Default-slot ranges reflect natural variance from the TRT→CUDA EP swap in #1577 (one query rank-shifting at the boundary). All numbers comfortably above pre-v1.36 baselines. Per-split detail + per-category breakdowns + sweep methodology in `~/training-data/research/models.md`.

(Older release detail is in the Done table at the bottom + CHANGELOG.md.)

---

## Active

### GPU Lane

- [x] **Reranker V2 — code-trained cross-encoder, 2026-04-17/18 pass.** Phase 1 calibration: 1k Gemma+Claude triples → 98.3% inter-rater agreement → GEMMA_ONLY decision (PR #1031). Phase 2: 200k Stack v2 hard-negative triples labeled by Gemma 4 31B AWQ on A6000 (12h45m wall, 95.31% overall agreement, 0 parse errors, balanced across 9 langs). Phase 3: trained `microsoft/unixcoder-base` + BCE on 382k pointwise rows. **Result: −24pp R@5 on v3.v2 test.** Even at smallest pool: −4.6pp R@5. Weights stay local at `~/training-data/reranker-v2-unixcoder/`.

  **Post-mortem (full detail `~/training-data/research/reranker.md`):**
  1. TIE labels were dropped from pointwise → trained on binary, weaker signal than BiXSE assumes
  2. Domain shift: trained on raw Stack v2 chunks, deployed on cqs's enriched chunks (NL desc + signature + content + doc)
  3. Pool-size brittleness: `(limit*4).min(100)` over-retrieves; weak rerankers get amplified

  All three are fixable but combined ~1-2 weeks. Not currently top priority. The "ms-marco net negative" result still stands for off-the-shelf rerankers; we now also have the matching result for in-domain-trained rerankers when domain isn't actually matched.

- [x] **ColBERT 2-stage rerank — tested 2026-04-17/18.** `mixedbread-ai/mxbai-edge-colbert-v0-32m` (Apache-2.0, 32M, beats ColBERTv2 on BEIR) via PyLate. Three modes (pure replacement, RRF fusion, alpha sweep). **Test α=0.9: R@5 +2.8pp; dev α=0.9: R@5 +0.9pp.** Test gain didn't fully replicate on dev; only R@20 improves consistently. Eval tool shipped (PR #1037), default OFF in production. Rust integration deferred — gains too marginal/inconsistent to justify the work.

- [x] **Chunker doc fallback for short chunks — landed 2026-04-18 (PR #1040).** `extract_doc_fallback_for_short_chunk` in `src/parser/chunk.rs` plus blank-line tolerance in `extract_doc_comment` close the `truncated_gold` failure mode (chunks <5 lines that ship without leading comment context). 10 happy/sad-path tests; reindex required. **Test R@5 +3.7pp vs canonical (63.3% → 67.0%); dev R@5 −2.7pp** (74.3% → 71.6%) — interlocked with LLM summary regen (5,486 → 7,018 cached, 47.7% coverage). The dev regression and R@20 movement on both splits are partly corpus-pruning artifact (16,095 → 14,734 chunks during reindex); follow-up A/B with a third reindex would isolate.

- [x] **Reranker V2 retrain with post-mortem fixes — tested 2026-04-20, PARKED.** Executed all three fixes: hard-negatives mined from cqs's own v3_pools (9175 cqs-domain graded training rows), TIE labels preserved as 0.5, pool cap lowered default 100→20. Trained UniXcoder three ways: pointwise BCE unweighted, pointwise BCE with auto `pos_weight=3.28`, pairwise `MarginRankingLoss(margin=0.3)`. **All three converged on −5 to −9pp R@5** (unweighted: −6.4 test / −7.3 dev; weighted: −5.5 / −9.2; pairwise: −5.5 / −9.2). R@20 unchanged across all three — gold IS in pool, weak score head consistently demotes it. Pairwise hit 98% train accuracy → fits train pairs perfectly, doesn't generalize. Conclusion: 326 queries × ~30 candidates is too thin to fine-tune a 125M cross-encoder against hard stage-1 negatives. Bottleneck is corpus size + base strength, not loss choice. Shippable wins (windowing + pool cap default + eval tooling) landed in v1.28.2 (PR #1060). Tooling kept: `evals/label_reranker_v3.py`, `evals/rerank_ab_eval.py`, `evals/train_reranker_v2_pairwise.py`. Next attempt would need 10x more queries (Gemma-augmented synthetic) or 5x bigger base (bge-reranker-large at ~3x latency).

- [x] **Reranker re-evaluation post-v1.39.0 — 2026-05-07, CLOSED as definitive null.** User hypothesis: ~100 audit fixes, schema bumps, tokenizer truncation fix (#1384), per-category α retune (v1.36) since 2026-04-20 might have flipped the verdict. Re-ran all four candidates against the v3.v2 fixture on the post-v1.39.0 default slot (EmbeddingGemma + per-cat α + Unknown=0.80). **All four still net-negative, gap actually widened on dev:**

  | Reranker | Test R@5 | Dev R@5 | Δ vs no-reranker baseline |
  |---|---:|---:|---:|
  | **No reranker (baseline)** | **67.9%** | **80.7%** | — |
  | `cross-encoder/ms-marco-MiniLM-L-6-v2` (off-the-shelf default) | 56.0% | 64.2% | -11.9pp test / -16.5pp dev |
  | reranker-v2-cqs-graded-unweighted (BCE) | 55.0% | 60.6% | -12.9pp / -20.1pp |
  | reranker-v2-cqs-graded-weighted (pos_weight=3.28) | 52.3% | 58.7% | -15.6pp / -22.0pp |
  | reranker-v2-cqs-pairwise (MarginRankingLoss) | 57.8% | 55.0% | -10.1pp / -25.7pp |

  R@20 within 1-4pp of baseline across all four — gold IS in the pool, every reranker just demotes it. Stage-1 (EmbeddingGemma + SPLADE + RRF) is now strong enough that the cross-encoder's `(query, NL_desc + signature + content + doc)` scoring adds noise rather than signal at the rank-5 boundary. **The 2026-04-20 verdict was robust**, not artifact of pre-fix bugs. cqs-side reranker work is closed; rerankers stay opt-in via `--rerank` and README documents the regression. Future revisit only on: (a) v4-scale labelled fixture (1526q/split, 14× v3 N), (b) base ≥5× bigger (bge-reranker-large at ~3× latency), or (c) project-specific labelled set proving lift on a different corpus.

### CPU Lane

**Retrieval quality:**
- [x] **Query-time HyDE — tested 2026-04-20, CATASTROPHIC.** Per `evals/hyde_per_category_eval.py`: generate synthetic Rust code via Gemma 4 31B per query, search with synthetic as the query string. **R@5 = 0.0% across all 8 categories on both test and dev splits** (vs baseline 65-95% per category). Inspecting samples: synthetic code is generic Rust/SQL with zero cqs-specific identifiers (e.g. for "table named notes AND columns with NOT NULL constraint" Gemma generated a generic `CREATE TABLE notes (id INTEGER PRIMARY KEY, ...)` — has nothing in common with cqs's actual schema chunks). Search returns generic-looking chunks; gold is never matched. The v2-era HyDE result that motivated the experiment was index-time, not query-time, so we tested the wrong direction.

  **Index-time HyDE re-eval still open.** cqs already has `cqs index --hyde-queries` that adds LLM-generated "queries that would find me" strings to each chunk at index time. The 2026-04-08 measurement on v2_300q showed +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral — net negative on R@1 in a single-config measurement. Per-category routing (only enable hyde-augmented chunks for queries where the v3 sweep says it helps) was never tried. Properly testing this requires: (1) regenerate HyDE for all chunks via the existing Claude Batches pipeline, (2) reindex with `--hyde-queries`, (3) per-category A/B harness that toggles the hyde-augmented embedding column. ~1 day. Lower expected lift than the categorization improvements above; promote only if classifier/regression work plateaus.

- [ ] **Expand the v3 label set with Gemma-generated synthetic queries.** Current v3 train + dev + test = 544 queries. Categorical optimization (alpha sweep, distilled classifier, per-query α regression) is data-bound past 50-100q per category. Generate ~5-10k more via the existing chunk-driven pipeline (`evals/generate_from_chunks.py`), classified self-consistently via Gemma. Bias generation toward thin categories (`conceptual` 0% rule fire, `negation` small-N noise on test). Prerequisite for the distilled classifier and per-query α regression items above. ~1 day of compute (Gemma already up via vLLM); negligible engineering since the pipeline is already working.

- [ ] **Context-aware classification.** Currently the router classifies the query in isolation. Add features available at query time: index language distribution (Rust-heavy vs Python-heavy vs polyglot), project category if known, top-N most-recently-searched terms. The intuition: same query in different project shapes might want different α (e.g., "function with retry" in a Go project routes to behavioral, in a Rust project might route to structural because Rust queries are more often structural in nature). Cheap to add as additional input dims to the distilled classifier or per-query α regression heads (no separate model needed). Effort: ~1 day after the distilled head is in place. Speculative ceiling — could be 0pp if context doesn't predict, or +3-5pp if there's signal we're not using. Also unlocks better behavior when an index spans heterogeneous projects (refs).

- [ ] **Soft routing — distribution over categories instead of argmax.** Today the classifier returns a single `QueryCategory`, the router picks `α(category)`, and a marginal misclassification fully swaps the alpha (e.g., behavioral=0.80 vs structural=0.90 — close enough, but multi_step=0.10 vs structural=0.90 if the classifier puts a multi_step query in `structural` is catastrophic). Soft routing: classifier outputs `P(c)` per category, effective α = `Σ P(c) × α(c)`. A query that's 60% behavioral / 30% structural / 10% multi_step gets α = 0.6×0.80 + 0.3×0.90 + 0.1×0.10 = 0.79.

  **Why now**: this whole arc is fundamentally a classification-and-routing problem. Hard routing throws away the classifier's confidence — even today's centroid classifier internally has soft cosine scores per category, but we softmax → argmax → pick one. Soft routing reuses that signal end-to-end.

  **Compatible with everything**: works on rule+centroid (use centroid cosines as the soft distribution), works on the distilled head (softmax outputs natural), works on the per-query α regression (the regression IS already producing a soft α). Probably a half-day in `src/search/router.rs` to wire centroid-based soft routing today; the rest follows for free when the distilled head lands.

  **Risk**: mixing alphas may attenuate their effect — if behavioral wants 0.80 and structural wants 0.90, mixing gives 0.85, which might be in the "neither helps much" middle ground. Worth measuring with a synthetic test where we know the true category from fixture metadata.

  **Pairs particularly well with the per-query α regression**: train on a soft target (R@5-weighted distribution over categories) instead of a hard one-hot, which gives the model nuanced training signal.
- [x] **BGE → E5 v9-200k — UN-RETIRED 2026-05-01.** Original 2026-04-25 verdict was "30pp behind, retired" but it turned out to be ~95% fixture-side artifact, not a model regression. The eval matcher in `eval/runner.rs` required strict `(file, name, line_start)` to score a gold chunk as matched, and v1.30.x audit waves had shifted line numbers in 42/109 test golds + 40/109 dev golds since the Apr 25 fixture refresh — so search returned the right chunks and the matcher counted them as misses. After loosening the matcher to `(file, name)` (this PR), v9-200k posts test R@1=45.9% R@5=70.6% R@20=80.7% / dev R@1=46.8% R@5=68.8% R@20=81.7% — **ties or marginally beats BGE-large on test R@5** (BGE 69.7% → v9 70.6%, +0.9pp), trails by ~8pp on dev R@5 (BGE 77.1% → v9 68.8%). For a model that's 1/3 the dim (768 vs 1024), 1/3 the params (~110M vs 335M), faster to embed, and already fine-tuned on cqs's call-graph data, it's back in serious contention. **Decision (2026-05-01): keep BGE-large as default.** Dev R@5 is the more reliable signal (advisory, not gating, but the larger gap there suggests v9 generalizes worse out-of-distribution), and BGE has years of upstream pre-training as a hedge against unknown query types. v9-200k stays available as an opt-in preset; it's the right model when memory or embed latency dominates over a few percentage points of R@5.
  - **Lesson (the user pushed back on this):** "if a benchmark number drops by 25pp overnight, that's bug-shaped, not model-shaped." Trust the prior baseline; investigate the harness before retiring the candidate. The fixture-line-drift symptom was already documented in PR #1109's post-mortem (Apr 25); we forgot the lesson and ate the same drift again 5 days later.
- [ ] **Index-time HyDE re-eval** — never tested at proper N. v2-era single-config measurement showed +14pp structural / +12pp type_filtered / −22pp conceptual / −15pp behavioral. Per-category routing (only enable hyde-augmented chunks for categories where sweep says it helps) was never tried. Properly testing requires (1) regenerate HyDE for all chunks via Claude Batches pipeline, (2) reindex with `--hyde-queries`, (3) per-category A/B harness toggling the hyde-augmented embedding column. Lower expected lift than the now-exhausted classifier work; cheap if revisited.

### Empirically closed: alpha-routing arc (2026-04-20 → 2026-04-21)

The classifier-accuracy / per-query-α-regression / soft-routing / fused-head family was systematically tested at proper N (v3 + v4, v4 = 1526 per split). Definitive null result across all variants. Documented in PR #1069 (research artifacts + post-mortems) and PR #1071 (long-chunk doc-aware windowing post-mortem with HNSW-noise meta-finding). Highlights:

| Lever | v3 R@5 (n=109) | v4 R@5 (n=1526) | Verdict |
|---|---|---|---|
| Distilled head (88.1% val acc, retrained on v3+synth) | test ±0 / dev +0.9 | test -0.3 / dev ±0 | parked |
| Fused head (continuous α + corpus fingerprint, contrastive ranking) | test ±0 / dev -0.9 | test -0.4 / dev +0.2 | parked |
| HyDE query-time | test -12.8 / dev -22.0 | test -10.7 / dev -9.8 | killed |
| Long-chunk doc-aware windowing | n/a | test +0.2 / dev +0.1 | neutral (within noise) |

Core finding: R@5 is alpha-insensitive on this corpus state across [0, 1]. The Oracle test's +9.2pp ceiling came from category-driven per-category default flips, not continuous-α refinement. **Routing-side levers (which α, which category, which routing weight) are exhausted.** Future R@5 work should target signal-side levers (chunking, embedder, multi-granularity index) under paired-reindex protocol.

**Index backends (signal-side, recall-leaning):**
- [x] **USearch backend — empirically dead, 2026-05-07.** Closed by sweep, not by implementation. Pitch was "USearch makes `ef_search` bumping cheaper than HNSW." We tested whether there's anything to bump for: at the new candidate-pool floor of 500 (PR #1583), `ef_search` is `max(env, k*2) = max(env, 1000)`. Sweeping `CQS_HNSW_EF_SEARCH` from 1000 → 2000 → 8000 (the last is ~57% of cqs's 14k-chunk corpus, near brute-force) produced **byte-identical R@K** at all three points: R@1 42.20% / R@5 71.56% / R@20 81.65%. HNSW is fully recall-saturated at the candidate-pool layer; USearch would carry the same algorithm class against the same saturated frontier. Adding the dep would bring binary-size cost + maintenance burden + zero R@K lift. **The recall headroom that was on the table lived in the `candidate_count` floor (#1583), not the `ef_search` knob.**
- [ ] **SIMD brute-force fast path under ~5K chunks** — still relevant, but small-project-only. Exact cosine, recall = 1.0 by construction. Wouldn't move cqs's own 14k-chunk eval, but would close the small-project gap and remove an index-build variable from per-slot A/Bs. Plugs into `IndexBackend` as a higher-priority backend that fires when `chunk_count < threshold`. Lower priority than the recall-saturated finding above suggests; promote only if a small-project user reports a recall gap not explained by the `candidate_count` floor.

**Embedder swap workflow (repeatable model A/B):**
- [x] **Index-aware embedder resolution.** Shipped — `src/cli/store.rs:138` `model_config()` reads `Store::stored_model_name()` and overrides env/CLI via `ModelConfig::resolve_for_query`. Closes the `CQS_EMBEDDING_MODEL=foo` foot-gun where queries against a `bar`-model index silently returned zero results.
- [x] **Embedder abstraction.** [#949](https://github.com/jamie8johnson/cqs/issues/949) closed. `ModelConfig` carries `input_names` / `output_name` / `pooling` / `tokenizer`; non-BERT models (Jina v3, Stella, GTE, custom) can be added as config entries rather than encoder forks.
- [x] **Content-keyed embeddings cache + named slots — shipped 2026-04-25 (PR #1105).** `.cqs/embeddings_cache.db` keyed by `(content_hash, model_id)` + project-level `.cqs/slots/<name>/` directories + `cqs slot {list,create,promote,remove,active}` + `cqs cache {stats,clear,prune,compact}` + `--slot`/`CQS_SLOT` on every major command + one-shot migration of `.cqs/index.db` → `.cqs/slots/default/`. Spec at `docs/plans/2026-04-24-embeddings-cache-and-slots.md`. Post-merge wiring fix added `cqs::resolve_index_db()` helper to handle 8 callers that built `.cqs/index.db` paths directly.
  - **Follow-ups:** [#1107](https://github.com/jamie8johnson/cqs/issues/1107) (slot create `--model` persistence) and [#1108](https://github.com/jamie8johnson/cqs/issues/1108) (hot search SELECTs omitting `content_hash`) both closed.
  - **A/B technique that works:** copy `llm_summaries` rows cross-slot by `content_hash` before `cqs index --llm-summaries`. Summary text is model-independent (NL describing the chunk); only the embedding into the slot's HNSW changes. Reduced API spend on coderank A/B prep to ~$0.03 (894 new summaries) and v9-200k to $0 (full overlap on eligible chunks).
- [x] **EmbeddingGemma-300m promoted to default in v1.35.0 (2026-05-02).** PR #1385 swapped the `define_embedder_presets!` `default = true` annotation from `bge_large` to `embeddinggemma_300m`. Apples-to-apples eval (after #1384 truncation fix): agg R@1=49.1% / R@5=72.5% / R@20=86.2%, beats BGE-large on R@1 by +1.9pp at 308M params / 768 dim / 2K context. BGE-large remains a first-class preset (`CQS_EMBEDDING_MODEL=bge-large`); existing slot indexes keep their stored model. TRT-RTX wiring is still a prerequisite for FP16 — TRT 10 fails the engine build on Gemma3's bidirectional-attention head plugin op; `CQS_DISABLE_TENSORRT=1` knob (#1301) falls through to CUDA EP at FP32. Full A/B writeup in `research/models.md`.
- [x] **Qwen3-Embedding-4B ceiling probe — closed 2026-05-04. Result: gemma-300m wins.** Probed Qwen3-Embedding-**4B** as a tractable proxy for the 8B (8B mmap risks on WSL still load-bearing, but 4B carries the same architecture: decoder-only, last-token pooling, instruct-prefix). Full enrichment + per-cat α tuning lifted qwen3-4b to test R@5 69.7% / dev R@5 77.1%, **still 2.7-2.8pp below gemma-300m's 72.5% / 79.8%** despite 13× the params and 3.3× the dim. The probe paid for itself in engineering — DB-lock root cause (#1451), FP16 dispatch (#1442), per-model α-set proof (#1453) — but the architecture finding is decisive: the Qwen3-Embedding family's MTEB-strong general-retrieval profile does not transfer to code search on this fixture. **Embedder-scale retired as a knob for cqs.** Full results in PROJECT_CONTINUITY.md "Right Now". Sweep artifacts: `/tmp/qwen3-sweep/{test,dev}-*.json` (capture before reboot).
  - 8B not run: same architecture, would consume another full day for a finding that's already conclusive at 4B. If it's ever revisited, the engineering envelope is now tested clean (FP16 dispatch + DB-lock fix + batch=1 + 30s busy_timeout + `Store::drop` PASSIVE all merged).
- [x] **NV-Embed-v2 ceiling probe — dropped 2026-05-04 by transitivity.** The Qwen3-4B result above generalizes: large general-purpose retrieval embedders underperform code-specialized small embedders on cqs's fixture. NV-Embed-v2 (8B Mistral base, 4096-dim, MTEB #1 at release) faces the same architecture-vs-domain mismatch plus higher engineering cost (custom pooling head, no community ONNX, our own export). Skip unless evidence emerges that NV-Embed-v2's Latent-Attention pooling specifically helps code retrieval — currently no such signal.
- [x] **llama-3.2-nv-embedqa-1b-v2 — considered, dropped 2026-05-03.** NVIDIA's commercial-OK Llama-based embedder (1B, Matryoshka 384–2048-dim, 8K context). Investigation found: no ONNX in the repo, custom `LlamaBidirectionalModel` architecture (`model_type: llama_bidirec`) needs `trust_remote_code=True`, would require authoring our own ONNX export including pooling + L2-normalize wrapper. 1-2 days of engineering with risk that bidirectional-attention ops don't export cleanly. Value proposition was "commercial-OK alternative to gemma" — but Gemma's restrictions (no weapons / biometric ID / dangerous infrastructure) don't bite cqs's code-search use case. Dropped. Resurrect only if a real Gemma-license blocker appears. Full writeup in `research/models.md`.

**Daemon:**
- [x] **Daemon: full CLI parity** — closed via [#947](https://github.com/jamie8johnson/cqs/issues/947) Commands/BatchCmd unification (shipped in v1.30.x).

**Watch mode:**

The biggest gap between cqs and similar code-intelligence tools: *easy to index, hard to keep indexed between turns*. IDEs solve "between keystrokes" (continuous time, editor consumer); Sourcegraph solves "between pushes" (discrete time, server consumer); cqs needs to solve "between turns" (discrete time, agent consumer). Different consistency models for different consumers — and cqs is the first tool in the space whose primary consumer is the agent, so it's the first that needs the turn-shaped consistency model. Items below are ordered by leverage.

- [x] **#1182 — perfect watch mode (3-layer reconciliation).** Filed 2026-04-28. The closing-the-gap item. Three layers compose: (1) `.git/hooks/post-{checkout,merge,rewrite}` post a `reconcile` message to the daemon socket, (2) periodic full-tree fingerprint reconciliation every `CQS_WATCH_RECONCILE_SECS` (default 30s) catches what hooks + inotify miss, (3) `cqs status --watch-fresh --wait` exposes a freshness contract — eval-runner just calls `--wait` and stops caring. Promise: bounded eventual consistency, agent can either trust `fresh` or block. **Positioning differentiator. Layers 1-4 shipped #1189/#1191/#1193/#1194; 47-file bulk-delta acceptance test landed in #1196.**
- [x] **Adaptive debounce — shipped #1724 (2026-06-10).** Idle-flush: pending set flushes after a quiet gap (`CQS_WATCH_DEBOUNCE_MS`, WSL auto-bump preserved) with a max-latency cap (`CQS_WATCH_MAX_DEBOUNCE_MS`, default 6× gap). A bulk checkout gets one cycle fired after the burst ends. Bonus fix: the old flush decision lived only in the recv-timeout arm, so event streams faster than 100ms starved flushing entirely.
- [x] **`cqs status --watch` — shipped #1720 (2026-06-10).** `WatchOpsStats` over the existing socket: queue depth, dropped events, in-flight clients, reconcile state, last-reindex latency, sticky last error, per-slot vec (extended per-slot by #1727). Composes with `--watch-fresh`/`--wait`. Cache hit rate deliberately omitted (no live counters on the reindex path; `cqs cache stats` answers offline).
- [x] **Whitespace/comment-canonical hash for cache lookup — shipped as schema v28 `chunks.canonical_hash` (v1.42.0).** Comment-stripped/whitespace-collapsed blake3 keys both embedding-reuse paths (store-side and global cache; unified in #1701's `resolve_reuse`), so comment-only edits and `cargo fmt` runs no longer re-embed. Full content_hash retained for store identity.
- [x] **Parallel reindex across slots — shipped #1727 (2026-06-10) as delta propagation.** The active reindex enqueues its file-delta per sibling slot (one event pipeline, no per-slot scans); same-model siblings drain as pure cache hits by construction (active-first ordering populates the global cache), foreign-model slots batch with hysteresis behind `CQS_WATCH_ALL_SLOTS` opt-in; durability via slot-aware #1182 reconcile; per-slot freshness via `cqs status --watch-fresh --slot X`.
- [x] **Kill the periodic full HNSW rebuild — shipped #1794 (2026-06-11) as the opt-in tiered-index backend.** The cuVS *tiered index* (`tiered-index` cargo feature + `CQS_TIERED_INDEX=1`): a brute-force tier absorbs incremental `extend`s and stays searchable immediately while CAGRA compacts inside cuVS, so the watch loop's periodic full rebuild becomes a no-op when active. No persistence (the cuVS C API has no tiered serialize) — cold-start rebuild only. Rust bindings upstream at rapidsai/cuvs#2235; ships via a commented-out `[patch.crates-io]` fork pin until released. The HNSW/CAGRA default path keeps `PendingRebuild` + content-hash-aware drain (#1124) — that machinery now bridges only the non-tiered backends.

**Features (queued, no immediate work):**
- [ ] **Temporal search — `cqs history`** — query by author + time range, ranks by how little a chunk's been touched since. Uses git log + file/line mapping.
- [ ] **Author-weighted search** — `cqs search "..." --author X --boost 0.5`. Complements temporal search.
- [ ] **Auto-notes on commit** — post-commit hook runs `cqs notes add` with message + changed chunk names. Sentiment inferred from heuristics with override flag.
- [ ] **Config file support** — `[splade.alpha]` per-category overrides in `.cqs.toml`.
- [ ] **Phase 6: Explainable search** — depends on SPLADE-Code being production default. Spec: `docs/plans/adaptive-retrieval.md`.
- [ ] **Paper v1.0** — clean rewrite done, needs review/polish + adaptive retrieval results.

**Agent adoption:**
- [ ] **Slim CLAUDE.md** — reduce 30-command reference to top 5 (search, context, read, impact, review) + pointer to `cqs --help`. Measure with telemetry before/after.
- [ ] **Composite search results** — `cqs search` returns mini-impact (caller + test counts) alongside each result. One call instead of search + impact.
- [ ] **`cqs trace "query"`** — show every routing decision: classifier → category → strategy → α → SPLADE top-K → dense top-K → RRF fusion → final ranking. Today understanding why a query ranked X requires `RUST_LOG=debug` + log grepping. Bigger lift; design separately. The agent-facing version of "explain my retrieval".
- [ ] **`cqs repl`** — interactive prompt (sqlite-shell-style) for iterating queries + ad-hoc exploration without `cqs batch` JSONL friction. Persistent connection to daemon, command history, in-line config tweaks. Replaces the heredoc-into-batch dance for exploratory work.

### Cross-Project Architecture

N-project via `[[reference]]` entries → `CrossProjectContext { stores: Vec<NamedStore> }`. Per-store BFS (callers, callees, impact, trace) matches cross-boundary edges by exact name only — wrappers, re-exports, and name mismatches are invisible.

- [ ] Type-signature matching for cross-boundary edges
- [ ] Import-graph resolution (parse `use`/`import` to resolve re-exports)
- [ ] Cross-project search with unified scoring (not just per-store RRF merge)
- [ ] `analyze_impact_cross` resolve file/line from CallGraph (currently empty paths — CQ-3)
- [ ] Cross-project dead code detection

### Agent Adoption — Telemetry

49,242 cqs invocations at `~/.cache/cqs/query_log.jsonl` since 2026-04-06 (snapshot 2026-04-16). 328 unique real queries (99% duplicate rate); 99.9% are `search`.

Historical split (2026-04-09, 16,731 invocations): **main conversation** uses `search` (60%) + `context` (28%) heavily, `impact`/`callers` almost never (0.2% each). **Subagents** drive nearly all `impact`/`callers`/`test-map`/`dead`/`gather` usage. Pre-edit hook bridges the gap by running `impact` automatically.

#### Friction backlog (2026-05-08)

Agent adoption is the strategic frame: cqs runs on dedicated GPU hardware doing real semantic indexing, but agents drift back to `grep` when cqs's surface mismatches their mental model. The load-bearing observation: **`search` rate dropped from 79% of code-intel calls in mid-April to 6% in early May** as the response shape accumulated fields. That's not voluntary refinement; that's agents avoiding a noisier surface. Telemetry archived in `.cqs/telemetry*.jsonl`.

Two designs, both docs landed and reviewable, no implementation yet:

- [x] **Polymorphic command routing — Phase 1 complete** ([`docs/polymorphic-routing.md`](docs/polymorphic-routing.md), PR #1596 design + #1610/#1612/#1616/#1617/#1618/#1620 implementation). All 60 dispatch points (6 commands × 5 kinds × 2 surfaces) ship the kind-fallback shape on both CLI-direct (`cmd_*`) and daemon-path (`dispatch_*`). `kind` and `fallback_from` fields on every kind-mismatch response. Phase 2 (`cqs about <name>` unified entry) is contingent on telemetry — see Phase 2 trigger criteria in the design doc.

- [x] **JSON SNR restoration — Phases 1-4 shipped in v1.40.0** ([`docs/json-snr-restoration.md`](docs/json-snr-restoration.md), PR #1595 design + #1601/#1602/#1604/#1609/#1613 implementation). CLI direct success emits bare JSON payload on stdout (no envelope); failure emits structured JSON to stderr + non-zero exit; batch/daemon JSONL keeps a slimmed envelope `{"data": ...}` or `{"error": ...}` per line; `CQS_ULTRASECURITY=1` restores the full envelope shape + force-emits per-result security fields *(removed in #1703 — the knob and the full-envelope restoration are gone; output is always the slim shape)*. Default-flip (#1613) is the breaking change; `CQS_OUTPUT_FORMAT=v1` restores the v1 envelope as a consumer-migration hedge. Phases 5-6 (per-source rate limit, tracing-noise-suppress) are scoped out — telemetry-contingent on whether agents still struggle with response size after the v1.40.0 corpus ships.

  **Phase status (2026-05-08 autopilot):**
  - ✅ **Phase 1** (#1601): `Posture` enum + `_with_posture` emission helpers. Additive; no behavior change.
  - ✅ **Phase 2** (#1602): per-result skip-when-default + posture-gated force-emit. `build_chunk_json_inner` posture-aware. Per-result lean shape under Friendly is ~30% smaller in the typical case.
  - ✅ **Phase 3** (#1604): slim batch/daemon envelope. `wrap_value` / `wrap_error` / `write_json_line` emit `{"data": <payload>}` or `{"error": {...}}` under Friendly; full envelope under `CQS_ULTRASECURITY=1`. ~70 KB saved on a 1000-line fixture batch. *(The ULTRASECURITY full-envelope path was removed in #1703; the slim envelope is now the only batch/daemon shape.)*
  - ✅ **Phase 4** (#1613): CLI direct → bare payload. Shipped as v1.40.0's breaking change; `CQS_OUTPUT_FORMAT=v1` is the consumer-migration hedge. Tests + eval harness migrated in the same release.
  - ✅ **Phase 5**: `CQS_ULTRASECURITY=1` restores the full envelope on CLI direct. Restoration contract hardened in v1.41.0 — #1630 restored the `worktree_stale` signal under the V2Bare default. *(Removed in #1703 — the knob and the full-envelope restoration are gone; output is always the slim shape. `CQS_OUTPUT_FORMAT=v1` remains the only envelope-restoring knob.)*
  - ✅ **Phase 6** (#1626): docs / README / CHANGELOG finalized in the v1.41.0 lying-docs sweep.

  All six phases shipped. Per-source rate limit and tracing-noise-suppress remain scoped out — telemetry-contingent on whether agents still struggle with response size.

  Superseded design at [`docs/json-noise-audit.md`](docs/json-noise-audit.md), kept for traceability — Appendix A in the SNR doc explains the framing evolution.

**Recommended ordering if shipping serially**: Phase 1 of polymorphic routing first (a failed query is worse than an expensive answer), then SNR restoration, then Phase 2 of polymorphic routing only if triggered. Both designs are independent: shipping one doesn't depend on the other; they address different friction surfaces (routing vs response-size) and overlapping consumer-migration concerns (eval harness, daemon batch protocol). Land in the same release window if possible to amortize migration.

---

## Open Issues

Re-audited 2026-06-10. The v1.39–v1.41 audit cycles closed nearly everything filed from v1.33.0, and the 2026-06-10 implementation campaign closed the last two medium-effort tracking issues: #1350 (ScoreSignal trait, PR #1719) and #1351 (DistanceMetric, PR #1722). Perf tier-3 lost #1244/#1229/#1228 (closed); refactor tier-3 lost #1216 (closed).

**v1.33.0 audit follow-ups: all closed** (#1350 via PR #1719, #1351 via PR #1722, both 2026-06-10).

**v1.40.0 audit cycle — deferred P1s (rationale in CHANGELOG v1.41.0):**

| Finding | Status |
|---------|--------|
| DS-V1.40-1: daemon Store cache invalidation via `PRAGMA data_version` | **FIXED** in PR #1718 (2026-06-10) — dedicated probe connection, WAL-write-no-checkpoint test |
| DS-V1.40-7: sentiment column CHECK constraint (schema v28) | Single-user discrete-value compliance is reliable; migration cost > benefit unless telemetry shows misuse |

**Perf tier-3 (real wins, but each ≥1hr):**

| # | Finding | Status |
|---|---------|--------|
| [#916](https://github.com/jamie8johnson/cqs/issues/916) | perf: mmap SPLADE body (PF-11) | Audit-deprioritized — 59 MB peak transient, dominated by parse-side allocations |
| [#717](https://github.com/jamie8johnson/cqs/issues/717) | perf: HNSW fully loaded into RAM (RM-40) | Needs lib swap to hnswlib-rs (nightly-only) |

**Refactor tier-3 (architectural debt, no user-visible impact):**

| # | Finding | Status |
|---|---------|--------|
| [#1140](https://github.com/jamie8johnson/cqs/issues/1140) | EX: Embedder preset extras map | Explicitly skipped per autopilot directive |
| [#1139](https://github.com/jamie8johnson/cqs/issues/1139) | EX: structural_matchers shared library | Touches 50+ language modules; explicitly skipped |

**Blocked on Windows test env or upstream:**

| # | Finding | Blocker |
|---|---------|---------|
| [#1043](https://github.com/jamie8johnson/cqs/issues/1043) | `is_slow_mmap_fs` ignores Windows network drives + reparse points | Linux/WSL unaffected; needs Windows runner |
| [#106](https://github.com/jamie8johnson/cqs/issues/106) | ort 2.0-rc.12 stable release | Blocked upstream (pykeio); no stable release yet |

**Feature scaffolding deferred:**

| # | Finding | Status |
|---|---------|--------|
| [#255](https://github.com/jamie8johnson/cqs/issues/255) | Pre-built reference packages | Signing/registry design (infra, not code) |

---

## Blocked

- **Clojure** — tree-sitter-clojure requires tree-sitter ^0.25, incompatible with 0.26.
- **Astro, ERB, EEx/HEEx, Move** — no tree-sitter grammar on crates.io.
- **Migrate HNSW to hnswlib-rs** — nightly-only dep, needs fork.
- **ArchestrA QuickScript** — needs custom grammar from scratch.

---

## Parked

- **`nomic-ai/nomic-embed-code` (7B) — Phase 2 of the code-specific embedder A/B, deferred 2026-04-25.** Apache 2.0, base = Qwen2.5-Coder-7B, GGUF quantizations available. Released March 2025; reported to beat Voyage Code 3 + OpenAI Embed 3 Large on CodeSearchNet. ~14 GB VRAM at FP16; embedding is 3584-dim (4.5× current storage). Skipped because at 7B params, inference cost approaches an LLM call — defeats the local-embedder advantage. Would need agentic batching to amortize. Revisit only if Phase 1 (CodeRankEmbed-137M, opt-in via #1110) shows the code-specialist trade-off is worth pushing further at scale.
- **Graph visualization** (`cqs serve`) — interactive web UI for call graphs, chunk types, impact radius. Spec: `docs/plans/graph-visualization.md`.
- **OpenRCT2 → Rust dual-trail experiment** — spec: `docs/plans/2026-04-10-openrct2-rust-port-dual-trail.md`.
- Wiki system (agent-first), MCP server (re-add when CLI solid), pre-built reference packages (#255), Blackwell RTX 6000 (96GB), L5X files from plant, KD-LoRA distillation (CodeSage→E5), ColBERT late interaction, enrichment-mismatch mining (Exp #4), lock/fork-aware training weights (Exp #5), ladder logic (RLL) parser, DXF/Openclaw PLC, SSD fine-tuning experiments.

---

## Done (Summary)

| Version | Highlights |
|---------|-----------|
| v1.35.0 | **Default embedder swap: BGE-large → EmbeddingGemma-300m + tokenizer truncation fix.** No schema bump. PR #1385 moves the `define_embedder_presets!` `default = true` annotation from `bge_large` to `embeddinggemma_300m`; all four downstream constants (`ModelConfig::DEFAULT_REPO`, `ModelConfig::DEFAULT_DIM`, `embedder::DEFAULT_MODEL_REPO`, `EMBEDDING_DIM`) update via the macro. Wins agg R@1 +1.9pp over BGE-large at half the params, 4× context. PR #1384 fixes the tokenizer truncation cap leaking into windowing/count paths — bge-large-ft / v9-200k / coderank tokenizers ship `truncation: max_length=512`, silently capping `encode().get_ids().len()` at 512 even on 5k+ token inputs and dropping ~90% of long-section content from the index. Surgical fix clones the tokenizer and disables truncation for counting paths only. Affected slots gained ~32% chunks on reindex (bge-ft 14,704 → 19,463; v9 14,718 → 19,506). Also: ONNX external-data sidecar download (#1385 fixup), serde_helpers `ignore` → `text` doctest (#1388 closes #1387), `test_prune_zero_days` flake fix (#1390 closes #1389). |
| v1.34.0 | **Audit close-out + EmbeddingGemma preset + reranker eval flag.** Same day as v1.33.0. Bundled 24 fix PRs from the post-v1.33.0 audit (167 findings, 129 closed, 25 medium-effort filed as tracking issues #1337-#1377). Highlights: daemon-aware `cqs index` (#1334, kills 98% of telemetry error rate), 11 unbounded `fs::read_to_string` sites capped (#1328), 5 SQLite legacy batch sizes → `max_rows_per_statement(N)` (#1324, 30-65× round-trip reduction), HNSW partial-save verification (#1325). Plus pre-audit feature work: EmbeddingGemma-300m preset (#1301), `cqs eval --reranker <none\|onnx\|llm>` (#1303 wires #1276's Reranker trait), `slow-tests` Phase 2 (#1302 gates `onboard_test` + `eval_subcommand_test` behind feature; ~12 min off PR-time CI), ci-slow.yml stabilization (#1306-#1308). |
| v1.33.0 | **Eval correctness + indexing performance + new fine-tuned preset.** No schema bump. Release themes: #1284 eval-matcher line-start drift fix (~38% of gold chunks were going invisible after audit-driven line shifts), #1288 placeholder-cache 30s startup-tax fix (CI ~38min → ~6min), #1283 chunk-orphan pipeline prune, #1289 `bge-large-ft` LoRA preset, #1292 daemon thread-local socket-dir override + #1286 Phase 1 nightly CI workflow. Plus 7 internal refactors (Reranker trait #1276, AuthChannel trait #1275, daemon_request<T> #1273, shared enumerate_files walk #1279, write_slot_model flatten #1272, notes kind lifecycle #1278/#1269, ENV_LOCK hoist #1268). |
| v1.32.0 | **HNSW load-phase flock self-deadlock fix + structural-trust + watch-correctness bundle.** Schema v23→v25 (chained, additive). Five themes: #1261 watch-mode flock fix, #1221 three-tier `trust_level: vendored-code`, #1254 worktree → main-index discovery, #1133 note kind taxonomy, #1260 TC-ADV reconcile coverage + persistent TRT engine cache. Post-release sweep landed 13 PRs closing 8 issues — refactor frontier (#1215/#1217/#1218/#1220/#1226) cleared, kind taxonomy fully wired (add/list/update). |
| v1.31.0 | **Schema v22→v23 + watch reconcile cluster + post-v1.30.2 bug drain.** Bundle: #1248 reconcile content-hash fingerprint + path dedup + force-rotation guard, #1249 sparse-upsert chunked sub-transactions, #1250 coarse-mtime FS handling + WSL `cqs serve --open` browser opener, #1251 reqwest same-origin redirect policy, #1252 `cqs slot remove` daemon-aware, #1253 native-Windows `cqs watch` clean shutdown via `ctrlc`, #1255 agent worktree-leakage guard. |
| v1.30.2 | **#1181 mistrust posture + #1182 perfect watch mode + v1.30.1 audit-fix wave.** Default-on `CQS_TRUST_DELIMITERS`, `_meta.handling_advice` on every JSON envelope, per-chunk `injection_flags`. Watch-mode Layers 1-4 closed: `cqs hook install` git hooks, periodic full-tree reconciliation, `cqs status --watch-fresh --wait` API, `cqs eval --require-fresh` gate. v1.30.1 audit-fix omnibus across 19 PRs (P1+P2+P3+P4 trivials, 121 of 144 findings; 18 hard P4s tracked). |
| v1.30.1 | **Indirect-prompt-injection hardening + v1.30.0 audit-fix wave + watch-mode reliability.** Cluster #1166-#1170 + threat model #1171: trust labelling on chunk JSON, `CQS_TRUST_DELIMITERS` opt-in, first-encounter shared-notes prompt on `cqs index`, `CQS_SUMMARY_VALIDATION` for prose summaries before caching, `--improve-docs` review-gated by default, threat model in `SECURITY.md`. Audit-fix omnibus #1141 (152 of 170 P1+P2+P3 findings). Five watch-mode correctness fixes (#1124-#1129): content-hash-aware drain, restore_from_backup pool ordering, summary-write coalescing, daemon mutex hold-time, embedding-cache `purpose` plumbing. Plus four refactors enabling cleaner extension points (#1130 RRF generalize, #1131 IndexBackend trait, #1132 scoring-knob resolver, **#1137 + #1138** registry tables for batch / LLM provider) and 12 dependabot bumps. Schema unchanged from v1.30.0; no reindex required. |
| v1.30.0 | **Cache+slots + three-way embedder A/B + v1.29.0 audit close-out + #956 Phase A scaffolding.** Cache+slots infra (#1105): `.cqs/embeddings_cache.db` keyed on (content_hash, model_id) + project-level `.cqs/slots/<name>/` directories + per-slot `cqs slot {list,create,promote,remove,active}` and `cqs cache {stats,clear,prune,compact}` commands. Three-way embedder A/B (#1109 #1110): fixture refresh absorbed v1.29.x line-start drift; BGE-large stays default; CodeRankEmbed-137M added as opt-in preset; v9-200k retired from production candidacy on the v3.v2 distribution. v1.29.0 audit close-out batch (#1112 #1113 #1114 #1117 #1118 #1119): every umbrella finding from #1095 closed. #956 Phase A scaffolding (#1120): `gpu-index` → `cuda-index` cargo feature rename (legacy alias preserved); `ep-coreml` / `ep-rocm` features added; `ExecutionProvider` enum gains cfg-gated `CoreML` and `ROCm { device_id }` variants. CUDA path byte-identical at runtime. |
| v1.29.1 | **v1.29.0 audit close-out** (147 findings triaged; 142 fixed). No new commands, no schema bump, no reindex. CAGRA SIGSEGV root-caused (missing `Drop` on `GpuState`) + fixed; `cqs serve` security hardened (host allowlist, SQL caps, HTML escape, loopback `--open`); transaction integrity fixes (staleness / metadata / cache / HNSW persist); 13 new `CQS_*` env var knobs for thresholds (additive); `rustls-webpki` GHSA-high patch. Remaining 5 audit items split to issues #1095/#1096/#1097/#1098. |
| v1.29.0 | **`cqs serve` + `.cqsignore` + slow-tests cron killed.** Interactive web UI for the call graph with 4 views — 2D Cytoscape, 3D force-directed, hierarchy (Y axis = BFS depth), embedding cluster (X/Z = UMAP, Y = caller count). Schema bumps v21→v22 for `umap_x`/`umap_y` columns; opt-in via `cqs index --umap` (Python umap-learn). Serve perf pass: ~60s → ~3-4s first paint (SQL-side max_nodes cap, default 300 nodes, `cose` layout, gzip, lazy 3D bundle). New `.cqsignore` mechanism layered on `.gitignore` (drops 18,954 → 15,488 indexed chunks on the cqs corpus, all noise). 5 of 16 slow-test binaries converted to in-process `InProcessFixture`-based tests; nightly `slow-tests.yml` cron deleted. Two Dependabot security bumps (openssl 0.10.78, rand 0.8.6). |
| v1.28.3 | **Per-category SPLADE α re-sweep targeting R@5** (the 2026-04-15 sweep optimized R@1 — different optima in many categories). Two alpha changes ship: `behavioral` 0.00 → 0.80, `multi_step` 1.00 → 0.10. Production net: test R@5 +0.9pp, dev R@5 ±0, no regressions. |
| v1.28.2 | **Reranker V2 retrain follow-ups.** Windowing fix (`chunks.content` was lossy WordPiece-decoded text for 7228/15616 chunks; PR #1060), `cqs index --force` fail-fast vs running daemon (#1061), `cqs notes list` daemon dispatch (#1062), `cli_review_test` `--format` → `--json` migration miss (#1063, fixes 2-day-red slow-tests nightly). Plus reranker pool cap default 100→20, centroid classifier flipped default-on after isolated A/B (test R@5 +3.7pp), `notes_boost_factor` measured (zero impact). Reindex required. |
| v1.28.0 | **Post-audit release.** Closes the post-v1.27.0 16-category audit: 150 findings landed across PRs #1041 (P1, 26) / #1045 (P2, 47) / #1046 (P3, 69); 6 deferred items filed as issues #1042-#1044, #1047-#1049. **BREAKING:** uniform JSON envelope across CLI/batch/daemon-socket (PR #1038). Schema v21 adds `parser_version` column on chunks (PR #1040 + P2 #29). 17 new env-var knobs. Daemon defaults tuned. Eval bumps: PR #1040 chunker doc fallback for short chunks → test R@5 63.3% → 67.0% (vs canonical). PR #1037 ColBERT 2-stage eval tool (default OFF, marginal/inconsistent gain). PR #1039 rustls-webpki CVE bumps. |
| v1.26.0 | **Watch + SPLADE hardening + Wave D–F audit batch.** `cqs watch` respects `.gitignore` (#1002, PR #1006). Incremental SPLADE in watch (#1004, PR #1007) — 100% coverage stays. Per-category α re-fit on clean 14,882-chunk index (PR #1005, +1.8pp R@1 on v2). `--splade` CLI flag respects router (PR #1008). `Store::open_readonly_after_init` replaces unsafe `into_readonly` (#986, PR #998). **Refactor lane** #946–#950 all closed (PRs #981–#985): Store typestate, Commands/BatchCmd unification, `cqs::fs::atomic_replace` helper, embedder model abstraction, CAGRA persistence. **Quick-wins lane**: WSL 9P/NTFS mmap auto-detect + CAGRA itopk envs + reranker batch chunking (#961/#962/#963, PR #979). **Wave D–F batch**: Aho-Corasick language_names (#964, PR #992), dispatch_search content tests (#973, PR #997), shared `Arc<Runtime>` (#968, PR #1000), migration fs-backup (#953, PR #996), NameMatcher ASCII fast path (#965, PR #990), `open_readonly_small` (#970, PR #993), reindex drain-owned chunks (#967, PR #991), `INDEX_DB_FILENAME` constant (#923, PR #994), CAGRA sentinel `INVALID_DISTANCE` (#952, PR #995), daemon `try_daemon_query` test scaffold (#972, PR #999). **Eval expansion**: v3 consensus dataset (544 dual-judge queries, train/dev/test 326/109/109, every category N≥23). |
| v1.25.0 | **11th full audit** (16 categories, 236 findings). Per-category SPLADE α defaults from clean 21-point sweep. Multi_step router fix (`"how does"` → not Behavioral, +0.7pp). Eval output to `~/.cache/cqs/evals/` (#943, root cause of 2 days of eval drift). Notes daemon-bypass routing (#945). Determinism fixes across 15+ sort sites + GC suffix-match bug (81% chunks orphaned, root cause of v1.24.0 → v1.25.0 R@1 inflation). |
| v1.24.0 | GPU-native CAGRA bitset filtering (upstream PR rapidsai/cuvs#2019), daemon stability (CAGRA non-consuming search fixes SIGABRT under load), cagra.rs simplified −357 lines, batch/daemon base index routing, router update (type_filtered + multi_step → base), cuVS 26.4. |
| v1.23.0 | **Daemon mode** (`cqs watch --serve`, 3-19ms queries), per-category SPLADE α routing + 11-point sweep, persistent query cache, shared runtime, AC-1 fusion fix, 90 audit findings. |
| v1.22.0 | Adaptive retrieval Phases 1-5 (classifier + routing + dual base/enriched HNSW), SPLADE-Code 0.6B eval chain (null result), SPLADE index persistence (#895), v19/v20 migrations (#898/#899), read-only batch store (#919), `Store::clear_caches` (#918). |
| v1.21.0 | Cross-project call graph (#850), 4 new chunk types to 29 (#851), chunk type coverage across 15 languages (#852), 14-category audit 40+ fixes (#859), API renames + 8 batch flags (#860), paper v1.0, docs refresh. |
| v1.20.0 | 14-category audit (71 findings, 69 fixed), Elm (54th), batch `--include-type`/`--exclude-type`, SPLADE code training (null), env var docs, README eval rewrite. |
| v1.19.0 | Include/exclude-type, Java/C# test+endpoint, batch `--rrf`, capture list unification, Phase 2 chunks, 265q eval, store dim check. |
| v1.18.0 | Embedding cache, 5 chunk types, v2 eval harness, batch query logging. |
| v1.17.0 | SPLADE sparse-dense hybrid, schema v17, HNSW traversal filtering, ConfigKey, CAGRA itopk fix. |
| v1.16.0 | Language macro v2, Dart (53rd), Impl chunk type. |
| v1.15.2 | 10th audit 103/103, typed JSON output structs, 35 PRs. |
| v1.15.1 | JSON schema migration, batch/CLI unification. |
| v1.15.0 | L5X/L5K PLC, telemetry, CommandContext, custom agents, BGE-large FT. |
| v1.14.0 | `--format text\|json`, ImpactOptions, scoring config. |
| v1.13.0 | 296-query eval, 9th audit, 16 commands. |
| v1.12.0 | Pre-edit hooks, query expansion, diff impact cap. |
| v1.11.0 | Synonym expansion, f32→f64 cosine, 80/88 audit fixes. |
