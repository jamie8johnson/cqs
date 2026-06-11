# Audit Triage (v1.42.0)

Audit date: 2026-06-10
Main HEAD: 7284e31e
Source: `docs/audit-findings.md` (107 findings — batch 1: 59 across 8 categories; batch 2: 48 across the other 8, run after batch-1 triage)
Calibration: matches `docs/audit-triage-v1.40.0.md` priority matrix.
Status: **P1 + P2 tiers COMPLETE (2026-06-11)** — P1: 28 findings + 4 riders across #1738-#1746/#1748; P2: all 30 v1.42 P2s across #1752-#1754, #1756-#1761, #1763 (+ P3 riders RB-V1.42-3, TC-V1.42-9/10, EH-V1.42-4, DS-V1.42-12, API-V1.42-4, TC-HAP-V1.42-7). Also closed en route: GitHub issues #1734 (#1762 text-mode bypass) and #1749 (#1751 nightly doctest split); follow-up filed #1755. **89 items remain open**: P3 (~24), P4 (7), CF-P2 (10), CF-P3 (40) + the GPU-half of TC-V1.42-8. Next: P3/P4 impact tiers 1-2, then re-rank.

Fix-run review notes for the next cycle: combined-short bools (`-qv`) leak on the bare-query daemon path (pre-existing, found in #1746's review); `review`/`ci` are daemon-marked but silently ignore `--stdin` daemon-up (found by the new exhaustiveness test); remaining `clamp(1, 100)` literals on non-search commands fold into CF SHL-V1.40-2.

Nested-lead experiment: 5 of 16 categories ran as lead + 3 read-only sub-scope agents with lead-side verification before append. Funnels — batch 1: Code Quality 20→16, TC-adversarial 18→15; batch 2: Performance 13→11, Data Safety 15→12, TC-happy 21→11. Combined: 87 raw candidates → 65 appended (cross-category dups caught, same-root-cause merges, stale-triage rejects). The other 11 categories ran as single auditors; Security ran on opus per the skill rule.

GitHub cross-check (open issues): API-V1.42-1 is the same surface-dependence class as open #1734 (daemon-forward text-mode output) but a distinct bug — note both in whichever PR touches `daemon_translate.rs`. No other overlap with open issues; auditors deduped against `audit-triage-v1.40.0.md` (which feeds #1459/#1463 umbrellas) before filing.

## Summary by Priority

| Priority | Count | Definition |
|----------|-------|------------|
| P1 | 28 | Easy + high impact (or lying docs) — fix first |
| P2 | 30 | Medium effort + high impact — fix in batch |
| P3 | 40 | Easy + low impact — fix if time |
| P4 | 9 | Hard or low impact — issues / inline trivials |
| CF-P2 | 10 | Carried forward from v1.40 triage, P2-grade, still open |
| CF-P3 | 40 | Carried forward from v1.40 triage, P3-grade, still open |

New v1.42 findings: 107 (batch 1: 59 = 15/11/26/7; batch 2: 48 = 13/19/14/2). Carry-overs from prior triages still open: 50. Grand total open: 157.

## P1 — easy + high impact

| ID | Finding | Location | Status |
|----|---------|----------|--------|
| SHL-V1.42-1 | DAEMON_LIMIT_CAP rationale comment false — CLI clamp exists at dispatch.rs:207; parity accidental | src/cli/batch/handlers/search.rs:9-11 | ✅ PR #1746 |
| OB-V1.42-1 | Corrupt global-cache embedding silently dropped in resolve_reuse (no warn; store-cache sibling warns) | src/cli/pipeline/reuse.rs:127 | ✅ PR #1743 |
| OB-V1.42-2 | CLI→daemon transport fallback debug-only — wedged daemon = silent ~32s hang | src/cli/dispatch.rs:300-410 | ✅ PR #1746 |
| DOC-V1.42-1 | README says Rust 1.95+; MSRV is 1.96 | README.md:27 | ✅ PR #1738 |
| DOC-V1.42-2 | README cuvs-fork [patch.crates-io] install note — fork retired in #1679; false supply-chain claim | README.md:33 | ✅ PR #1738 |
| DOC-V1.42-3 | README env table: CQS_CAGRA_THRESHOLD default 50000; actual 5000 (10x error, behavior-selecting knob) | README.md:784 | ✅ PR #1738 |
| DOC-V1.42-7 | SECURITY.md advisory table missing RUSTSEC-2025-0119; bincode rationale contradicts .cargo/audit.toml | SECURITY.md:300-307 | ✅ PR #1738 |
| EH-V1.42-1 | load_audit_state conflates EACCES/EIO with NotFound — audit mode silently off, zero log | src/audit.rs:92-95 | ✅ PR #1743 |
| EH-V1.42-2 | CQS_SLOT env doesn't bypass daemon — silent wrong-slot results (--model analog warns) | src/cli/dispatch.rs:213-221 | ✅ PR #1746 |
| RB-V1.42-2 | extract_return_sql slices original string at to_uppercase() offset — panic on non-ASCII SQL; siblings use to_ascii_* | src/language/languages.rs:6743-6745 | ✅ PR #1741 |
| API-V1.42-1 | Daemon arg translation remaps -n→--limit globally — `cqs blame foo -n 3` hard-errors daemon-up, works daemon-down (verified live; related: open #1734 class) | src/daemon_translate.rs:51 | ✅ PR #1746 |
| API-V1.42-9 | Daemon graph-dispatch doc comments enumerate wire fields that don't exist (line/function vs line_start/name) | src/cli/batch/handlers/graph.rs:37-137 | ✅ PR #1738 |
| TC-V1.42-6 | NaN/Inf embeddings pass HNSW build zero-vector filter (NaN != 0.0) into parallel_insert_data — can panic whole index build | src/hnsw/build.rs:204-226 | ✅ PR #1741 |
| TC-V1.42-7 | EmbeddingCache::read_batch bytemuck cast panics on truncated blob BEFORE length check; query_cache fixed this exact bug | src/cache/embedding_cache.rs:261 | ✅ PR #1741 |
| CQ-V1.42-12 | v27 needs_embedding visibility gate lives only in production-dead search_fts; live RRF FTS leg unfiltered — unembedded chunks can surface mid-reindex | src/store/search.rs:120-150 vs src/search/query.rs:358-364 | ✅ PR #1740 |
| SEC-V1.42-1 | Kind-fallback definitions[] relays full chunk content with no injection_flags/trust_level — SECURITY.md promises both on every chunk-returning output (docs-lying rule) | src/cli/commands/graph/mod.rs:206-253 | ✅ PR #1742 |
| EXT-V1.42-2 | No exhaustiveness link between batch="daemon" Commands variants and BatchCmd — forgotten variant fails only at runtime, only daemon-up (~15-line test) | src/cli/definitions.rs:337+ vs src/cli/batch/commands.rs:46 | ✅ PR #1746 |
| EXT-V1.42-3 | CONTRIBUTING "Adding a New CLI Command" documents nonexistent attribute surface (#[cqs(handler=...)]) and omits dispatch_shims + daemon wiring steps (lying recipe) | CONTRIBUTING.md:448 | ✅ PR #1738 |
| EXT-V1.42-4 | Migration how-to describes removed run_migration() match ladder; no MIGRATIONS-table contiguity test — forgotten row fails only at user upgrade | src/store/migrations.rs:10, :380-398 | ✅ PR #1738 |
| AC-V1.42-1 | map_hunks_to_functions skips count==0 hunks — pure-deletion changes invisible to impact/review under -U0 diffs | src/impact/diff.rs:77-79 | ✅ PR #1744 |
| AC-V1.42-2 | Staleness checks use `current > stored` where reconcile pins `!=` — backward-mtime restores never look stale (feeds OB-V1.42-3 plumbing) | src/store/chunks/staleness.rs:742, :948-951 | ✅ PR #1744 |
| PERF-V1.42-1 | Daemon rebuilds vector index from disk on EVERY search-class query (~360-480ms vs 3-19ms budget, journal-confirmed) — BatchView fallback never writes back; SPLADE cell got the fix, vector/base/file_set/notes didn't | src/cli/batch/view.rs:325-347, context.rs:208 | ✅ PR #1739 |
| PERF-V1.42-2 | Hybrid-fused search fetches ~1.4 MB of embedding BLOBs per query that are provably never read | src/store/chunks/async_helpers.rs:85-108, src/search/query.rs:932-939 | ✅ PR #1743 |
| PERF-V1.42-5 | split_into_windows deep-clones the entire HF tokenizer (262k vocab) once per chunk — ~14k clones per reindex | src/embedder/core.rs:601-607 | ✅ PR #1743 |
| DS-V1.42-1 | Embedding-reuse by-hash lookups lack needs_embedding gate — zero-vec sentinels laundered into permanent "real" embeddings (silent retrieval corruption); 3 one-line WHERE clauses | src/store/chunks/embeddings.rs:34, :111-113, :188 | ✅ PR #1740 |
| DS-V1.42-3 | HNSW loader deletes a live save's staging dir BEFORE taking the shared lock — concurrent save reports success while destroying the on-disk index | src/hnsw/persist.rs:837-850 vs :414, :684 | ✅ PR #1745 |
| DS-V1.42-7 | Deferred cache invalidation permanently lost — both staleness discriminators consumed before deferral, promised retry observes no change | src/cli/batch/context.rs:619-624, :529, :468 | ✅ PR #1739 |
| DS-V1.42-8 | doc_writer::atomic_write falls back to non-atomic truncate-write of USER SOURCE FILES on any rename failure, never fsyncs — only non-rebuildable data cqs writes | src/doc_writer/rewriter.rs:646-702 | ✅ PR #1745 |

## P2 — medium effort + high impact

| ID | Finding | Location | Status |
|----|---------|----------|--------|
| OB-V1.42-3 | Daemon-served searches drop staleness warnings — recommended default path never warns; WSL inotify makes it the path that needs it most | src/cli/batch/handlers/search.rs:67-129 | ✅ PR #1752 |
| DOC-V1.42-5 | CONTRIBUTING JSON envelope section teaches pre-V2Bare shape as universal; not_found/io_error "reserved" claim false (also json_envelope.rs:155) | CONTRIBUTING.md:63-93 | ✅ PR #1756 |
| DOC-V1.42-6 | CONTRIBUTING Architecture Overview missing ~14 files; commands/eval lists moved schema.rs | CONTRIBUTING.md:221-432 | ✅ PR #1756 |
| RB-V1.42-1 | collect_comment_ranges / find_type_identifier_recursive recurse per tree-depth — stack overflow (SIGSEGV) on rayon workers aborts index/watch | src/parser/chunk.rs:126, :756 | ✅ PR #1757 |
| API-V1.42-3 | callers vs callees output topology asymmetry; module doc's type-discrimination claim false for callees | src/cli/commands/graph/callers.rs:74-90 | ✅ PR #1760 |
| API-V1.42-8 | Args-layer contract drift in campaign code: borrowed non-Deserialize AuditModeArgs, two no-Args conventions, adapter-only flag in core Args | infra/audit_mode.rs:38 + slot.rs/reference.rs/cache_cmd.rs + index/stale.rs:76-82 | ✅ PR #1760 |
| TC-V1.42-1 | notes add --mentions can write notes.toml past 10 MiB cap → bricks all notes ops; note.rs:30-33 doc overpromises write-side enforcement | src/cli/commands/io/notes.rs:306-311, src/note.rs:307-320 | ✅ PR #1748 |
| TC-V1.42-8 | CAGRA accepts non-finite queries + zero/NaN build vectors HNSW guards — backend asymmetry (CPU-side prepare_index_data filter is the easy half; GPU-gated verification is P4-grade) | src/cagra.rs:424, src/hnsw/mod.rs:583-602 | ✅ PR #1761 |
| CQ-V1.42-1 | Config ef_search silently ignored on every direct-CLI search path — wrapper hardcodes None; knob only works via batch/daemon | src/cli/store.rs:397-402 | ✅ PR #1758 |
| CQ-V1.42-8 | prune_gitignored never cleans function_calls — orphan call-graph rows (ghost callers) until next prune_all; 3 prune fns already diverged | src/store/chunks/staleness.rs:266, :389, :532 | ✅ PR #1759 |
| CQ-V1.42-11 | Cross-project branches escaped command-core extraction on both surfaces — duplicated, no parity tests, no deferred-ledger entry | src/cli/batch/handlers/graph.rs vs src/cli/commands/graph/* | ✅ PR #1760 |
| SEC-V1.42-2 | cqs serve relays content_preview/signature/doc with no trust_level/injection_flags — wire them in OR scope SECURITY.md's universal claim to exempt serve (decision needed) | src/serve/data.rs:92-114, :508-645 | ✅ PR #1761 |
| PB-V1.42-1 | v23 reconcile fingerprint columns stamped only by watch path — CLI `cqs index` leaves them NULL/stale → spurious reindex churn wave after every branch-switch index under a live daemon | src/cli/pipeline/upsert.rs:174-178, async_helpers.rs:329 | ✅ PR #1753 |
| EXT-V1.42-1 | daemon_translate hand-mirrors 5 of ~25 clap flags — `-v`/`--rrf` break daemon-up (verified live); derive strip/remap sets from Cli::command() like telemetry.rs does; structural cause of API-V1.42-1 | src/daemon_translate.rs:47-52 | ✅ PR #1746 |
| AC-V1.42-3 | find_test_matches keeps the empty-string predecessor sentinel AC-V1.40-3 fixed in bfs_shortest_path, and is the only BFS without a node cap | src/impact/test_map.rs:40-78 | ✅ PR #1757 |
| AC-V1.42-4 | Python UntilColon signature truncates at first annotation colon — annotated defs lose params + return type (retrieval quality; needs PARSER_VERSION bump) | src/parser/chunk.rs:293 | ✅ PR #1757 |
| PERF-V1.42-3 | Dense-index path discards HNSW/CAGRA scores then re-fetches embeddings to recompute identical cosine (verify CAGRA score-scale parity first) | src/search/query.rs:785-794, :941-947 | ✅ PR #1758 |
| PERF-V1.42-4 | Incremental `cqs index` fully parses every file before the staleness filter + per-file N+1 mtime SELECT — O(corpus) instead of O(changed) | src/cli/pipeline/parsing.rs:62-209 | ✅ PR #1753 |
| PERF-V1.42-6 | store_stage defeats row batching with per-file transactions ×2 passes (~30k tx at ceiling); function-calls write in same fn already fixed this class | src/cli/pipeline/upsert.rs:172-179, :247-261 | ✅ PR #1753 |
| TC-HAP-V1.42-1 | scout pipeline untested at every layer (lib core, CLI core, compute_hints_batch, binary) — CLAUDE.md-mandated per-session command | src/scout.rs:124-181, src/impact/hints.rs:96 | ✅ PR #1763 |
| TC-HAP-V1.42-2 | search_hybrid — the production search path — referenced by exactly one test, inside an #[ignore]d eval; #1584's bug lived exactly here | src/search/query.rs:493 | ✅ PR #1763 |
| TC-HAP-V1.42-3 | Flagship `cqs "<query>"` has zero binary-spawn coverage in any format; only default-format search payload test is shape-only | tests/cli_chat_format_test.rs:129-141 | ✅ PR #1763 |
| TC-HAP-V1.42-6 | No test drives review_core/ci_core/affected_core through a populated diff — existing tests accept the empty branch (lands with AC-V1.42-1's -U0 test) | tests/cli_review_test.rs:245-275, review/affected.rs:54 | ✅ PR #1763 |
| TC-HAP-V1.42-10 | Every-session agent command set (impact, test-map, deps, callers/callees happy, trace, context, read, where, related, health, stats) has no happy-path binary spawn | tests/cli_surface_test.rs (pattern exists at :130) | ✅ PR #1763 |
| TC-HAP-V1.42-11 | v1-envelope pin monoculture — 34 test files route through cqs_v1(); shipped default (V2Bare) tested for only ~5 command payloads | tests/common/mod.rs:53 | ✅ PR #1763 |
| DS-V1.42-2 | HNSW dirty-flag self-heal trusts self-referential checksum manifest — crash between chunk commit and HNSW save silently serves stale index (generation stamp; shares mechanism with DS-V1.42-5) | src/hnsw/mod.rs:712-719, persist.rs:229 | ✅ PR #1754 |
| DS-V1.42-4 | HNSW stale-.bak guard: post-success debris locks out all future saves; guard runs outside the exclusive lock (TOCTOU); shutdown detaches rebuild thread mid-save | src/hnsw/persist.rs:366-394, watch/mod.rs:1891-1937 | ✅ PR #1754 |
| DS-V1.42-5 | Background HNSW rebuild saves sidecars outside index.lock — lost update vs concurrent `cqs index`, dirty flag cleared over the gap (generation stamp closes it) | src/cli/watch/rebuild.rs:260-357 | ✅ PR #1754 |
| DS-V1.42-6 | ensure_splade_index publishes a stale-snapshot index into the shared cell after invalidation already ran — served indefinitely on a quiet repo | src/cli/batch/view.rs:397-463 | ✅ PR #1739 |
| DS-V1.42-10 | Migration backup has no cross-process write exclusion — torn snapshot under live daemon; restore discards daemon commits (use VACUUM INTO) | src/store/backup.rs:110-178 | ✅ PR #1759 |

## P3 — easy + low impact

| ID | Finding | Location | Status |
|----|---------|----------|--------|
| DOC-V1.42-4 | README HNSW tuning table presents mid-tier values as fixed; defaults are corpus-tiered | README.md:696-710 | ✅ PR #1738 |
| EH-V1.42-3 | Batch dispatch swallows response-write failures via `let _ =` (7 sites) — daemon side built tracked writes for exactly this | src/cli/batch/context.rs:684-797 | open |
| EH-V1.42-4 | chunk_count().ok() blanks slot-list chunks column silently; adjacent model-name read got the warn ladder | src/cli/commands/infra/slot.rs:253 | ✅ PR #1759 |
| RB-V1.42-3 | extract_signature UntilAs loop bound makes end-of-string guard unreachable — trailing "AS" never matched | src/parser/chunk.rs:302-306 | ✅ PR #1757 |
| RB-V1.42-4 | injection.rs u32-cast safety comment cites nonexistent "MAX_FILE_SIZE (50MB)"; real cap 1 MiB + unbounded env override | src/parser/injection.rs:221 | ✅ PR #1738 |
| API-V1.42-2 | Core Args reuse clap struct names — two contradictory alias conventions (CallersCoreArgs vs CoreCalleesArgs) | src/cli/commands/graph/callers.rs:34 et al. | open |
| API-V1.42-4 | --cross-project flips callees topology object→flat array, different entry schema (fold into API-V1.42-3) | src/cli/batch/handlers/graph.rs:154-161 | ✅ PR #1760 |
| API-V1.42-5 | serde(default) coverage inconsistent — graph cores reject minimal wire payloads io/search cores accept | src/cli/commands/graph/callers.rs:33-39 | open |
| API-V1.42-6 | --expand alias: bool on search, value-taking depth on gather | src/cli/args.rs:189 vs :234 | open |
| API-V1.42-7 | drift's hand-rolled --limit accepts 0 and defaults unlimited — contradicts LimitArg contract | src/cli/args.rs:581-583 | open |
| TC-V1.42-2 | Whitespace-only note = cross-note wildcard for update/remove; duplicate-text semantics unpinned | src/cli/commands/io/notes.rs:298-572 | ✅ PR #1748 |
| TC-V1.42-3 | cmd_batch stdin line-cap rejection path (CQS_BATCH_MAX_LINE_LEN) zero tests (daemon analog is pinned) | src/cli/batch/session.rs:155-173 | open |
| TC-V1.42-5 | parse_nonzero_usize untested while f32 siblings exhaustively pinned in same file | src/cli/definitions.rs:94-100 | open |
| TC-V1.42-9 | HNSW zero-vector skip / id_map desync — documented past bug class, no regression test | src/hnsw/build.rs:200-222 | ✅ PR #1758 |
| TC-V1.42-10 | HNSW search k > index size unpinned (CAGRA analog test exists) | src/hnsw/search.rs:105-117 | ✅ PR #1758 |
| TC-V1.42-11 | Store::open on garbage/truncated index.db untested | src/store/mod.rs:906 | open |
| TC-V1.42-12 | Non-integer schema_version → Corruption arm untested | src/store/migrations.rs:230-236 | open |
| TC-V1.42-14 | parser_stage TOCTOU (file deleted mid-pipeline): parse_errors arm + mtime=0 sentinel untested | src/cli/pipeline/parsing.rs:127-176 | open |
| CQ-V1.42-2 | gather_cross_index production-dead wrapper skews test coverage to brute-force path | src/gather.rs:561 | open |
| CQ-V1.42-3 | CAGRA in-memory constructors test-only API masquerading as production surface | src/cagra.rs:801, :370-376 | open |
| CQ-V1.42-4 | CagraIndex::save ~75-line test-only twin of save_with_store hardcoding splade_generation: 0 | src/cagra.rs:1013-1086 | open |
| CQ-V1.42-5 | CommandContext.project_cqs_dir/slot_name written never read, masked by #[allow(dead_code)] | src/cli/store.rs:162-168 | open |
| CQ-V1.42-6 | ScoutOptions knobs unreachable from any surface | src/scout.rs:101, :135 | open |
| CQ-V1.42-10 | Env-knob parsing re-implemented across six sites; cli/limits.rs private copy; divergent silent-swallow semantics | hnsw/persist.rs, cli/limits.rs, onboard.rs, note.rs, diff.rs, task.rs | open |
| CQ-V1.42-15 | Library llm modules eprintln! user-facing hints directly | src/llm/summary.rs:101, doc_comments.rs:281, batch.rs:638 | open |
| CQ-V1.42-16 | Crate root accretes utilities that have dedicated homes (serde/path/fs helpers in lib.rs) | src/lib.rs:410-645 | open |
| SEC-V1.42-3 | Cleartext-http API-key guard split across two layers with contradictory localhost policy — local.rs re-check unreachable; under ALLOW_INSECURE=1 silently drops opted-into header | src/llm/mod.rs:357-372, local.rs:146-171 | open |
| PB-V1.42-2 | linux_fs_resolution magic table omits V9FS_MAGIC (0x01021997) — manually mounted DrvFS gets fine-grained mtime treatment, re-opening dropped-second-save bug | src/config.rs:270-289 | open |
| RM-V1.42-1 | CAGRA interrupted-save tmp orphans never swept on load — HNSW/SPLADE both sweep theirs; multi-hundred-MB accumulation under crash-looping daemon | src/cagra.rs:1186, :1426, :1556 | open |
| PERF-V1.42-7 | `cqs task` computes the full test-reachability BFS twice with identical inputs | src/task.rs:144, :194 | open |
| PERF-V1.42-8 | test_reachability allocates 2-3 Strings per BFS visit — siblings in same file use Arc<str> interning (runs on every scout/task/health/review) | src/impact/bfs.rs:354-386 | open |
| PERF-V1.42-10 | build_chunk_detail tests-that-cover query is an unindexed full-table content LIKE scan per dashboard click — use chunks_fts | src/serve/data.rs:619-629 | open |
| PERF-V1.42-11 | Daemon success response written unbuffered — one write() syscall per JSON fragment (fold into CF dispatch_value refactor, RM-V1.40-9 cluster) | src/cli/watch/socket.rs:298 | open |
| TC-HAP-V1.42-5 | Gather direction filtering (Callers/Callees) tested only by is_ok() — deliberate fixture wasted | tests/gather_test.rs:185, :231 | open |
| TC-HAP-V1.42-7 | Command-core parity tests are parity-by-construction — no parity test pins a single output value; add one fixture-grounded assert each | src/cli/batch/handlers/graph.rs:862-1080 | ✅ PR #1760 |
| TC-HAP-V1.42-8 | cache_compact_core zero coverage; cache_stats_core per-model branch never executed | src/cli/commands/infra/cache_cmd.rs:222, :117-130 | open |
| TC-HAP-V1.42-9 | telemetry_core populated path and all:true flag have zero assertions — telemetry feeds real decisions | src/cli/commands/infra/telemetry_cmd.rs:395 | open |
| DS-V1.42-9 | checkpoint_legacy_index opens via URL parsing — special-char paths silently skip the WAL drain the slot migration depends on | src/slot/mod.rs:1053 | open |
| DS-V1.42-11 | slot create --model killed between dir creation and slot.toml write loses the model pin; retry guidance steers toward wrong-model indexing | src/cli/commands/infra/slot.rs:313-325 | open |
| DS-V1.42-12 | `cqs convert --overwrite` writes via bare fs::write — truncated doc ingested as valid content on next index | src/convert/mod.rs:523-526 | ✅ PR #1759 |

## P4 — hard or low impact

| ID | Finding | Location | Status |
|----|---------|----------|--------|
| TC-V1.42-4 | read_stdin oversize-cap + invalid-UTF-8 untested (review --stdin, impact --diff, ci --stdin) | src/cli/commands/mod.rs:566-580 | open |
| TC-V1.42-13 | No SQLITE_BUSY / concurrent-writer test anywhere; busy_timeout env fallback unpinned | src/store/helpers/sql.rs:14 | open |
| TC-V1.42-15 | Read-only .cqs/ — no permission-denied test for index creation/open | src/cli/commands/index/build.rs:135-190 | open |
| CQ-V1.42-7 | Fused-tx phantom-chunk pruning verbatim inline copy of delete_phantom_chunks | src/store/chunks/crud.rs:1059-1144 | open |
| CQ-V1.42-9 | Ref-path query preparation duplicated, wider than deferred ledger entry; cmd_query_project double-search_hybrid is the easy first slice | src/cli/commands/search/query.rs:918-1174 vs :239-527 | open |
| CQ-V1.42-13 | serve/data.rs writes raw SQL via pub(crate) pool access — schema knowledge in two modules | src/serve/data.rs:224-1124 | open |
| CQ-V1.42-14 | store ↔ search bidirectional coupling — Store's search API implemented in search module on private fields (root cause of CQ-V1.42-12's gate-in-dead-code) | src/search/query.rs:67, src/store/search.rs:8 | open |
| PERF-V1.42-9 | build_cluster streams the entire function_calls table per /api/embed/2d request, aggregating degree counts Rust-side — push GROUP BY into SQL like build_graph | src/serve/data.rs:1011-1026 | open |
| TC-HAP-V1.42-4 | task e2e tests #[ignore]d AND vacuous (assertions satisfiable by any outcome); --tokens waterfall branch never executed | tests/task_test.rs:285-327, train/task.rs:609-641 | open |

## Suggested fix clustering (for the next implementation session)

- **Daemon-surface parity cluster (P1/P2):** API-V1.42-1, EXT-V1.42-1 (structural cause — derive flag sets from clap), EXT-V1.42-2 (exhaustiveness test), EH-V1.42-2, OB-V1.42-2, OB-V1.42-3 + AC-V1.42-2 (staleness predicate feeds the envelope plumbing), SHL-V1.42-1 — all "same query, different behavior depending on daemon" class; pairs with open #1734.
- **Daemon cache-lifecycle cluster (P1/P2):** PERF-V1.42-1 (vector-index write-back — the headline), DS-V1.42-6 (stale SPLADE publish), DS-V1.42-7 (lost deferred invalidation) — all BatchContext/BatchView shared-cell lifecycle; one PR restores the daemon's latency contract AND closes the stale-serve races.
- **HNSW persistence-lifecycle cluster (P2):** DS-V1.42-2/3/4/5 — one generation-stamp mechanism + lock-ordering fixes closes all four; RM-V1.42-1 (CAGRA tmp sweep) rides along.
- **Indexing-crash cluster (P1/P2):** TC-V1.42-6, TC-V1.42-7, RB-V1.42-1, RB-V1.42-2, TC-V1.42-8 — panic/abort paths reachable during index/watch.
- **Search-correctness:** CQ-V1.42-12 (+ CQ-V1.42-14 as the structural root cause, separate PR), CQ-V1.42-8, DS-V1.42-1 (sentinel laundering — three WHERE clauses).
- **Search hot-path perf:** PERF-V1.42-2/3 (shared hydration-helper fix surface).
- **Indexing pipeline perf:** PERF-V1.42-4/5/6 + PB-V1.42-1 (fingerprint stamp belongs in the same shared-upsert surgery).
- **Atomic-write sweep (easy P1/P3):** DS-V1.42-8 (user source files — highest stakes), DS-V1.42-9/11/12.
- **Trust-signal relay (P1/P2):** SEC-V1.42-1 (one shared transform fixes 6 commands × 2 surfaces), SEC-V1.42-2 (decision: wire serve or scope the SECURITY.md claim).
- **Docs sweep (P1+P2 docs in one PR):** DOC-V1.42-1/2/3/7, then DOC-V1.42-5/6, API-V1.42-9, RB-V1.42-4, SHL-V1.42-1's comment half, EXT-V1.42-3/4 (recipes), SEC-V1.42-3 (one trust model).
- **Notes hardening:** TC-V1.42-1, TC-V1.42-2.
- **Command-core hygiene:** API-V1.42-2/3/4/5/8, CQ-V1.42-11 — one campaign-style cleanup pass; TC-HAP-V1.42-7's value asserts land here.
- **Test backfill (P2 batch):** TC-HAP-V1.42-1/2/3/6/10/11 — production-search and mandated-command coverage first; AC-V1.42-1's -U0 test lands with TC-HAP-V1.42-6.
- **Impact/diff correctness:** AC-V1.42-1/3/4.

---

# Carried forward from prior triages (still open)

Source of truth: `docs/audit-triage-v1.40.0.md` — its "Verification 2026-06-09" section supersedes that file's per-row Status cells — reconciled against what the 2026-06-10 campaigns actually closed (PROJECT_CONTINUITY records), with spot-greps against main today (2026-06-10) for the ambiguous items.

**Closed since the 2026-06-09 verification (NOT carried):** queue item 1 cap-parity (command-core campaign), queue items 2/3/7 (#1725 — EH-V1.40-8+PB-V1.40-7, SHL-V1.40-1+AC-V1.40-9, kind-fallback test backfill), queue item 5 / DS-V1.40-1 (#1718 data_version probe), Cluster C core (CQ-V1.40-1/2/3/4/5/6/9, API-V1.40-1/4, EXT-V1.40-1/2, RM-V1.40-1, TC-HAP-V1.40-4/7/10 — campaign), Cluster F V2Bare binary-boundary tests (#1703). Verification-invalidated: SEC-V1.40-7, DS-V1.40-9, RB-V1.40-2's index sub-claim, TC-HAP-V1.40-7's no-tests sub-claim.

**Still deferred by standing decision (not in tables below):** DS-V1.40-7 / EXT-V1.40-4 (sentiment CHECK constraint / Sentiment enum — schema-migration cost > benefit, single user). P4 umbrella items remain tracked on #1463 (4 truly-remaining design items), #1459 (1 of 8: project/ref verb consolidation), #1512 (Windows daemon), #1573 (dead-code tiers 3a/3b/4: EXT-V1.40-3/7, PERF-V1.40-10), SHL-V1.40-3 (next perf cycle), SHL-V1.40-6 (#1453 successor) — no need to re-list here.

## CF-P2 — carried forward, P2-grade

| ID(s) | Finding | Effort | Status |
|---|---|---|---|
| DS-V1.40-8 + DS-V1.40-10 | Kind-detect + real query share no read snapshot — dispatcher/existing-flow drift (v1.40 queue item 6) | medium | open |
| CQ-V1.40-10 + RB-V1.40-4 + EH-V1.40-10 + DS-V1.40-6 + PERF-V1.40-1 | Cluster A2: `filter_invoked_macros` N+1 GLOB full scans, no transaction (correctness fixed in #1627; perf half remains) | medium | open |
| RM-V1.40-9 + PERF-V1.40-5 + RM-V1.40-10 | Daemon socket triple payload allocation + `emit_json` double serialization — `dispatch_value` refactor (socket.rs TODO P2 #62) | medium | open |
| PERF-V1.40-7 | `build_test_map` iterates all test chunks per call — invert to iterate ancestors (modest; loop body O(1)) | easy | open |
| RM-V1.40-6 + RM-V1.40-7 | `CrossProjectContext::merged_call_graph` rebuilt per request; reference stores reopened (64 MB mmap × N refs) — cache in BatchContext (note: new CQ-V1.42-11 touches the same surface) | medium | open |
| AC-V1.40-6 + AC-V1.40-7 + EH-V1.40-3 | trace: macro falls through to empty BFS; `Multiple` masks ambiguity; `target` kind unvalidated | medium | open |
| SEC-V1.40-8 | `enumerate_files` no depth/file-count cap on adversarial repos (Cluster H leftover) | medium | open |
| PB-V1.40-9 | WAL mode unconditional on store open — unsupported on /mnt/c 9P bridge; detect and switch to DELETE | medium | open |
| PERF-V1.40-8 | `meta_json_fragment` triple-work per JSONL emit — CAUTION: proposed per-posture LazyLock is unsafe (fragment carries dynamic worktree_stale); cache only the static part | easy | open |
| RM-V1.40-4 | `lookup_by_name` builds SQL via `format!` per call (surviving sub-claim of RB-V1.40-2) | easy | open |

## CF-P3 — carried forward, P3-grade

| ID(s) | Finding | Effort | Status |
|---|---|---|---|
| OB-V1.40-1 | Kind-fallback dispatchers emit no fallback-fired tracing (verified today: graph/mod.rs warns only on store error) | easy | open |
| OB-V1.40-2 | `cqs telemetry` has no kind-fallback category — Phase 2 routing decision blocked on this signal | medium | open |
| OB-V1.40-5 | `detect_fallback` / `classify_hits` no entry spans | easy | open |
| OB-V1.40-6 | Tier 2b macro filter drops candidates silently — FP rate unauditable | easy | open |
| OB-V1.40-7 | `write_json_line` format decision unattributable | easy | open |
| OB-V1.40-8 | Daemon `accept()` successes unlogged — handle leaks unattributable | easy | open |
| OB-V1.40-9 | `max_concurrent_daemon_clients` env override unlogged | easy | open |
| OB-V1.40-10 + SEC-V1.40-3 + TC-ADV-V1.40-6 | `redact_query_str` silent on activation, strict `v != "0"`, zero tests | easy | open |
| EH-V1.40-4 | `dispatch_deps` Type-forward misroutes Function names without warning | easy | open |
| EH-V1.40-5 | `meta_json_fragment` `.expect()` on hot path | easy | open |
| EH-V1.40-7 | `lookup_by_name` empty-string short-circuit undocumented | easy | open |
| API-V1.40-2 | Kind enum mixes 5 routing kinds + 3 resolution outcomes (verified today: no KindResolution split exists) | medium | open |
| API-V1.40-3 | `Store::lookup_by_name` breaks `get_chunks_by_X` convention (verified today: unrenamed) | easy | open |
| API-V1.40-5 | Two `pub enum OutputFormat` (output_format.rs vs cli/definitions.rs — verified today both exist; posture.rs rename in #1711 didn't resolve the type collision) | medium | open |
| API-V1.40-6 | `emit_json_error_with_data` shape inconsistency vs wrap_error path | easy | open |
| API-V1.40-9 | `CQS_DEFERRED_FLUSH_INTERVAL` is a count, not a duration — rename `_BATCHES` | easy | open |
| API-V1.40-10 | `--format` and `--json` flags shadow each other | easy | open |
| CQ-V1.40-8 | `meta_value_for_envelope` duplicate fallback Map construction (partially reduced by #1711's delegation) | easy | open |
| RB-V1.40-5 + SEC-V1.40-5 | `lookup_by_name` debug-span records full user-supplied name | easy | open |
| SHL-V1.40-2 | `clamp(1, 100)` literals duplicated 13+× — named constants in cli::limits (new SHL-V1.42-1 is the false-rationale sibling; fix together) | easy | open |
| SHL-V1.40-4 | `STALENESS_CHECK_INTERVAL = 100ms` hardcoded (verified today) | easy | open |
| SHL-V1.40-5 | `AUDIT_STATE_RELOAD_INTERVAL = 30s` / `CONFIG_RELOAD_INTERVAL = 5min` hardcoded (verified today) | easy | open |
| SHL-V1.40-7 | `KEEP_BACKUPS = 3` hardcoded | easy | open |
| SHL-V1.40-8 | `LOAD_SPARSE_CHUNK_ID_BATCH = 1000` hardcoded | easy | open |
| SHL-V1.40-10 | `chunks_paged` accepts unbounded limit | easy | open |
| TC-ADV-V1.40-2 + EXT-V1.40-5 | `classify_chunk_type` pin incomplete for 11/24 ChunkType variants | easy | open |
| TC-ADV-V1.40-3 | `lookup_by_name` wildcard/injection/long-name inputs untested | easy | open |
| TC-ADV-V1.40-8 | `emit_json_error` adversarial input untested | easy | open |
| TC-ADV-V1.40-10 | `current_worktree_name` Unicode/shell-special/long inputs untested | easy | open |
| TC-HAP-V1.40-5 | `_meta.worktree_name`/`_meta.worktree_stale` zero emission tests | easy | open |
| TC-HAP-V1.40-6 | `v3_test.v2.json` schema-coverage test missing (dev-only covered) | easy | open |
| PB-V1.40-2 | `is_under_wsl_automount` vs `is_wsl_drvfs_path` divergent shape validation | easy | open |
| PB-V1.40-4 | `worktree_name` doesn't trim trailing slash | easy | open |
| PB-V1.40-6 | `is_wsl_drvfs_path` UNC arm unguarded for non-WSL hosts | easy | open |
| PB-V1.40-10 | `worktree_name` no dunce::canonicalize — Windows verbatim prefix leaks into JSON | easy | open |
| RM-V1.40-2 | `dispatch_via_view` reconstructs token Vec — second full clone per dispatch | easy | open |
| RM-V1.40-3 | `current_worktree_name` clones cached String per envelope emit | easy | open |
| RM-V1.40-5 | `dispatch_test_map` clones every test chunk per cross-project request | easy | open |
| PERF-V1.40-6 | `upsert_chunks_unembedded_batch` clones every chunk + zero-vec | easy | open |
| PERF-V1.40-9 | Telemetry write ~5 syscalls per CLI command — daemon-path handle caching | medium | open |
