# v1.30.1 Audit Triage

Generated: 2026-04-28T18:49:20Z
Total findings: 144
Categorization: P1 14, P2 32, P3 78, P4 23 (duplicate entries cross-referenced rather than removed; 3 are explicitly noted as subsumed)

## Cross-cutting themes

- **`delta_saturated` flag is half-wired (3 findings, same root cause).** `WatchSnapshotInput` carries it, the wire shape serializes it, but `WatchSnapshot::compute()` never consults it. Surfaces in code-quality, data-safety, and adversarial test coverage. After a saturated rebuild discards on swap, the snapshot reports `Fresh` and `cqs eval --require-fresh` accepts a doomed rebuild. P1: defeats #1182's day-1 gate.
- **`dropped_this_cycle` reset-before-publish defeats the freshness gate (3 findings, one fix).** Counter is zeroed at line 145 of `events.rs` before reindex completes; `publish_watch_snapshot` runs after the drain, so `compute()` never observes a non-zero value. AC-V1.30.1-4 adds the embedder-init-failure twist. P1 because it lets dropped events pass `--require-fresh` invisibly.
- **`wait_for_fresh` is the new hot path and is full of papercuts (9+ findings across 7 categories).** Poll-cadence cost (RM-2, PF-V1.30.1-2), stringly-typed errors collapsed to `NoDaemon` (EH-V1.30.1-2, AC-V1.30.1-6), no entry/exit telemetry (OB-V1.30.1-4, OB-V1.30.1-6, OB-V1.30.1-8), `Instant` overflow (RB-2), 250 ms hardcoded interval (SHL-V1.30-2), no exponential backoff (RB-9), only the trivial Fresh-on-first-poll path tested (TC-HAP-1.30.1-5, TC-ADV-1.30.1-4). Treat as a single batch refactor pass.
- **"Lying docs" cluster — P1 by team rule (5 findings).** PRIVACY says `(content_hash, model_id)` but schema is `(content_hash, model_fingerprint, purpose)`. SECURITY's symlink matrix promises "follow then validate" but the indexer skips them entirely. SECURITY's trust-level mitigation list claims `read --focus` and `context` carry `trust_level`/`injection_flags` — they don't. SECURITY's auth claim doesn't mention the `cqs_token_<port>` cookie or `NoAuthAcknowledgement`. ROADMAP says #1182 acceptance test is pending — #1196 already merged it.
- **v0.12.1 swallow-error anti-pattern recurrence (7+ findings).** New code (`dispatch.rs:207`, `doctor.rs:923`, `cli/index/build.rs:863`, `reconcile.rs:116`, `reranker.rs:524`) ships with `.ok()` / `.unwrap_or_default()` swallowing real failures with no `tracing::warn!`. The exact pattern MEMORY.md called out post-v0.12.1 audit. EH-V1.30.1-* and TC-ADV-1.30.1-6 come at it from different angles.
- **Wiring-verification regressions (3 findings, P1 by memory rule).** `embed_batch_size_for(model)` shipped as the v1.30.0 fix, still `#[allow(dead_code)]`, prod still uses unscaled `embed_batch_size()=64` — exactly the configurable-models disaster. `BatchCmd` dispatch is hand-routed match (33 arms); `log_query` hand-sprinkled across 6 dispatch arms.
- **Coverage gaps where #1182 commands shipped untested (5 findings).** `cmd_uninstall`/`cmd_fire`/`cmd_hook_status` zero tests; `cmd_status` body never invoked by tests; `require_fresh_gate` itself never called by tests (env logic re-implemented inline); `process_file_changes` zero direct tests; `cqs eval` end-to-end never run *without* `CQS_EVAL_REQUIRE_FRESH=0` bypass.
- **Three-channel auth surface has known bugs pinned in *tests that assert the wrong behavior* (4 findings).** SEC-7 leakage: `strip_token_param` doesn't case-fold or percent-decode; cookie wins over query so stale `?token=` survives in URL; Bearer prefix grammar gaps; reasoning-only collision tests. P1 because security-surface tests asserting current-bad behavior is the wrong shape per audit charter.

## P1 — Easy + high impact (fix immediately)

| ID | Title | Category | Location | Effort | Status |
|----|-------|----------|----------|--------|--------|
| CQ-V1.30.1-2 | `delta_saturated` flag published but ignored by `FreshnessState::compute` | Code Quality | `src/watch_status.rs:199-209` | easy | ✅ PR #1202 |
| CQ-V1.30.1-1 | `dropped_this_cycle` reset before snapshot publish hides drop signal from `--require-fresh` | Code Quality | `src/cli/watch/events.rs:139-146` + `mod.rs:1303` | medium | ✅ PR #1202 |
| CQ-V1.30.1-4 | `strip_token_param` doesn't case-fold or percent-decode — token leaks into redirect URL (SEC-7) | Code Quality | `src/serve/auth.rs:246-260, 305-318` | medium | ✅ PR #1201 |
| AC-V1.30.1-5 | `check_request` cookie-wins-over-query leaks stale `?token=` permanently in URL bar | Algorithm Correctness | `src/serve/auth.rs:269-321` | easy | ✅ PR #1201. Notes: paired with CQ-V1.30.1-4. |
| DOC-V1.30.1-1 | PRIVACY/SECURITY misstate embedding-cache primary key (drops `purpose` column) | Documentation | `PRIVACY.md:16`, `SECURITY.md:47` | easy | ✅ PR #1199 |
| SEC-V1.30.1-2 | SECURITY.md "Symlink Behavior" matrix contradicts `enumerate_files(follow_links=false)` | Security | `SECURITY.md:203-215` | easy | ✅ PR #1199 |
| SEC-V1.30.1-1 | SECURITY.md falsely claims `read --focus` / `context` carry `trust_level`/`injection_flags` | Security | `SECURITY.md:14` vs `read.rs:312`, `context.rs:269-325` | medium | ✅ PR #1200 |
| DOC-V1.30.1-7 | SECURITY.md auth claim missing `cqs_token_<port>` cookie + `NoAuthAcknowledgement` from #1135/#1136 | Documentation | `SECURITY.md:17` | easy | ✅ PR #1201 |
| DOC-V1.30.1-4 | ROADMAP claims #1182 acceptance test pending, but #1196 already merged | Documentation | `ROADMAP.md:16,142` | easy | ✅ PR #1199 |
| SHL-V1.30-1 | `embed_batch_size_for` dead code — production still uses unscaled 64; nomic-coderank OOMs | Scaling | `src/cli/pipeline/types.rs:178-207`, `parsing.rs:42`, `enrichment.rs:74` | easy | ✅ PR #1203. Notes: Wiring-verification regression per MEMORY.md. |
| SEC-V1.30.1-8 | Daemon env snapshot logs every `CQS_*` at info — `CQS_LLM_API_KEY` lands in journal | Security | `src/cli/watch/mod.rs:529-532` | easy | ✅ PR #1201 |
| DS-V1.30.1-D2 | `run_daemon_reconcile` bypasses `max_pending_files()` cap — drowns queue on bulk branch switch | Data Safety | `src/cli/watch/reconcile.rs:103,128` | medium | ✅ PR #1203 |
| AC-V1.30.1-1 | Reconcile `disk > stored` strict predicate misses `git checkout HEAD~5 -- foo.rs` (older mtime) | Algorithm Correctness | `src/cli/watch/reconcile.rs:124` | medium | ✅ PR #1203 |
| AC-V1.30.1-4 | `process_file_changes` resets `dropped_this_cycle` before embedder check — drops silently lost on init failure | Algorithm Correctness | `src/cli/watch/events.rs:139-156` | medium | ✅ PR #1202. Notes: same root cause as CQ-V1.30.1-1. |

## P2 — Medium effort + high impact (fix in batch)

| ID | Title | Category | Location | Effort | Status |
|----|-------|----------|----------|--------|--------|
| EH-V1.30.1-1 | Parse failure leaves stale chunks AND no mtime update — reconciles forever | Error Handling | `src/cli/watch/reindex.rs:255-314` | medium | ✅ PR #1207 |
| EH-V1.30.1-2 | `wait_for_fresh` collapses transport AND parse errors into `NoDaemon` — wrong advice | Error Handling | `src/daemon_translate.rs:660-678` | easy | ✅ PR #1211 |
| EH-V1.30.1-7 | Reconcile mtime-stat error swallowed — file may oscillate as "always stale" | Error Handling | `src/cli/watch/reconcile.rs:116-127` + `reindex.rs:501-509` | medium | ✅ PR #1207 |
| EH-V1.30.1-8 | `try_init_embedder` Err path strands HNSW dirty flag without observability | Error Handling | `src/cli/watch/events.rs:154-185` | medium | ✅ PR #1208 |
| RB-1 | `to_string_lossy()` on path keys silently mangles non-UTF-8 paths into permanent reindex storms | Robustness | `src/cli/watch/reconcile.rs:99` + `staleness.rs:627-637` | medium | ✅ PR #1207 |
| RB-9 | `wait_for_fresh` infinite poll loop on slow daemon — no exp backoff, 2400 socket connects/600s | Robustness | `src/daemon_translate.rs:665-678` | medium | ✅ PR #1211 |
| RB-10 | `now_unix_secs()` swallows clock-before-epoch error as `0` — masks systemic bad-clock | Robustness | `src/watch_status.rs:226-231` | easy | ✅ PR #1208 |
| RB-6 | `enumerate_files` `unwrap_or(&path)` on canonicalize-mismatch leaks abs paths into rel workflow | Robustness | `src/lib.rs:680-685` | medium | ✅ PR #1207 |
| AC-V1.30.1-3 | BFS `bfs_expand` cap check skips score-bump for already-visited neighbors at boundary | Algorithm Correctness | `src/gather.rs:357-381` | medium | ✅ PR #1209 |
| AC-V1.30.1-9 | `daemon_socket_path` uses `DefaultHasher` — Rust-version-dependent, breaks systemd unit naming | Algorithm Correctness | `src/daemon_translate.rs:174-200` | medium | ✅ PR #1211 |
| AC-V1.30.1-10 | `incremental_count = 0` reset on idle-clear loses delta context — late HNSW rebuilds | Algorithm Correctness | `src/cli/watch/mod.rs:1180` | medium | ✅ PR #1208 |
| API-V1.30.1-1 | `cqs status --wait` emits success envelope but exits 1 on timeout — contradicts contract | API Design | `src/cli/commands/infra/status.rs:85-90` | easy | ✅ PR #1211 |
| API-V1.30.1-5 | `daemon_ping`/`status`/`reconcile` return `Result<T, String>` — stringly-typed errors on public API | API Design | `src/daemon_translate.rs:271,422,541` | medium | ✅ PR #1211 |
| API-V1.30.1-10 | `WatchSnapshot.idle_secs` frozen at compute time — wire shape lies once snapshot served later | API Design | `src/watch_status.rs:101,219` | medium | ✅ PR #1208 |
| OB-V1.30.1-3 | `WatchSnapshot` state transitions silent — Fresh↔Stale↔Rebuilding flips have no journal trail | Observability | `src/cli/watch/mod.rs:149-185`, `watch_status.rs:195-224` | medium | ✅ PR #1208 |
| OB-V1.30.1-6 | `require_fresh_gate` lacks entry span + final-decision info — eval gate decisions invisible | Observability | `src/cli/commands/eval/mod.rs:219-275` | easy | ✅ PR #1210 |
| OB-V1.30.1-8 | `daemon_status` connect-failure warns every 250 ms during startup race — 2400 lines/600s wait | Observability | `src/daemon_translate.rs:438-441` | medium | ✅ PR #1211 |
| OB-V1.30.1-9 | `process_file_changes` uses `println!` in non-quiet — bypasses tracing infrastructure | Observability | `src/cli/watch/events.rs:147-152` | easy | ✅ PR #1208 |
| OB-V1.30.1-10 | `serve::search` info logs full query at info — bypasses TraceLayer redaction | Observability | `src/serve/handlers.rs:189-232` | easy | ✅ PR #1206 |
| PB-V1.30.1-1 | `cmd_serve` `--no-auth` warning misses `0.0.0.0` and `::` wildcard binds — most-exposed configs | Platform Behavior | `src/cli/commands/serve.rs:27` | easy | ✅ PR #1206 |
| PB-V1.30.1-3 | `process_exists` (Windows) substring-matches `INFO:` against localized `tasklist` output | Platform Behavior | `src/cli/files.rs:59-72` | medium | ✅ PR #1209 |
| PB-V1.30.1-7 | `cqs hook fire` on Windows-native: `.cqs/.dirty` written but no consumer ever reads it | Platform Behavior | `src/cli/commands/infra/hook.rs:309-335` | medium | ✅ PR #1207 |
| SEC-V1.30.1-3 | `callgraph-3d.js` interpolates `e.message` into `innerHTML` without `escapeHtml` — XSS gap | Security | `src/serve/assets/views/callgraph-3d.js:55` | easy | ✅ PR #1206 |
| SEC-V1.30.1-4 | `tag_user_code_trust_level` shape-coupled — silently no-ops on unknown JSON shapes | Security | `src/cli/commands/mod.rs:216-257` | medium | ✅ PR #1206 |
| DS-V1.30.1-D1 | `cqs index --force` reopen leaves stale `pending_rebuild` against orphaned store handle | Data Safety | `src/cli/watch/mod.rs:1102-1122` | medium | ✅ PR #1209 |
| DS-V1.30.1-D5 | `.cqs/.dirty` fallback marker write not atomic — crash drops the only signal daemon will see | Data Safety | `src/cli/commands/infra/hook.rs:329-332` | easy | ✅ PR #1209 |
| DS-V1.30.1-D7 | HNSW rollback path leaves `.bak` files orphaned when restore-rename fails | Data Safety | `src/hnsw/persist.rs:509-553` | medium | ✅ PR #1209 |
| TC-HAP-1.30.1-2 | `cmd_uninstall`, `cmd_fire`, `cmd_hook_status` — three CLI commands ship with zero tests | Test Coverage (happy) | `src/cli/commands/infra/hook.rs:262-373` | medium | ✅ PR #1209 |
| TC-HAP-1.30.1-3 | `cmd_status` — 6-row behavior matrix promised, zero outcomes pinned | Test Coverage (happy) | `src/cli/commands/infra/status.rs:38-103` | medium | ✅ PR #1209 |
| TC-HAP-1.30.1-4 | `require_fresh_gate` — function never called by any test; bypass logic re-implemented inline | Test Coverage (happy) | `src/cli/commands/eval/mod.rs:219-275` | easy | ✅ PR #1210 |
| TC-HAP-1.30.1-7 | Eval freshness gate untested end-to-end — every test sets `CQS_EVAL_REQUIRE_FRESH=0` bypass | Test Coverage (happy) | `tests/eval_subcommand_test.rs:88-93` | medium | ✅ PR #1210 |

## P3 — Easy + low impact (fix if time)

| ID | Title | Category | Location | Effort | Status |
|----|-------|----------|----------|--------|--------|
| CQ-V1.30.1-3 | `cqs eval --require-fresh` Timeout error message omits drop/saturation signals | Code Quality | `src/cli/commands/eval/mod.rs:248-255` | easy | ✅ PR #1235 |
| CQ-V1.30.1-5 | Three near-identical `ort_err` helpers across embedder/reranker/splade | Code Quality | embedder/reranker/splade modules | easy | ✅ PR #1239 |
| CQ-V1.30.1-6 | Auth `--no-auth` warning at boot silent for localhost binds — duplicate warning logic | Code Quality | `src/cli/commands/serve.rs:27-37` | easy | ✅ PR #1237 |
| DOC-V1.30.1-2 | README canonical command list omits `cqs hook` and `cqs status` (#1182 commands) | Documentation | `README.md:540-569` | easy | ✅ PR #1214 |
| DOC-V1.30.1-3 | CONTRIBUTING Architecture Overview stale — missing eval/, watch_status.rs, daemon_translate.rs, etc | Documentation | `CONTRIBUTING.md:149-340` | easy | ✅ PR #1214 |
| DOC-V1.30.1-5 | PRIVACY/SECURITY document only Linux path for query_log/query_cache (macOS/Windows missing) | Documentation | `PRIVACY.md:21-22`, `SECURITY.md:111-136` | easy | ✅ PR #1214 |
| DOC-V1.30.1-6 | README "Watch Mode" section omits default `--wait-secs` budget (30 s) | Documentation | `README.md:219-220` | easy | ✅ PR #1214 |
| DOC-V1.30.1-9 | v1.30.0 CHANGELOG/ROADMAP lists `cqs cache {stats,prune,compact}` missing `clear` (cosmetic) | Documentation | `CHANGELOG.md:71`, `ROADMAP.md:131` | easy | ✅ PR #1214 |
| API-V1.30.1-2 | `--watch-fresh --wait-secs 30` vs `--require-fresh-secs 600` — same op, 20× different default | API Design | `definitions.rs:792-793`, `eval/mod.rs:85-86` | easy | ✅ PR #1239 |
| API-V1.30.1-3 | `cqs eval --no-require-fresh` is the only `--no-X` flag in the CLI surface | API Design | `src/cli/commands/eval/mod.rs:79-80` | easy | ✅ PR #1235 |
| API-V1.30.1-4 | `PingResponse.last_indexed_at` vs `WatchSnapshot.last_synced_at` — same field, two names | API Design | `daemon_translate.rs:236`, `watch_status.rs:105` | easy | ✅ PR #1235 |
| API-V1.30.1-6 | `DaemonReconcileResponse.queued: bool` documented "always true" — useless field | API Design | `src/daemon_translate.rs:517-519` | easy | ✅ PR #1235 |
| API-V1.30.1-7 | `WatchSnapshotInput::_marker: PhantomData<&'a ()>` is leaked private invariant | API Design | `src/watch_status.rs:181-193` | easy | ✅ PR #1239 |
| API-V1.30.1-8 | `cqs status` (no flag) exits 1 — only mode is gated on a flag user can't avoid | API Design | `src/cli/commands/infra/status.rs:41-54` | easy | ✅ PR #1239 |
| API-V1.30.1-9 | `FreshnessState` has `as_str()` but no `Display` — papercut for `format!`/`tracing::info!(state=%)` | API Design | `src/watch_status.rs:51-60` | easy | ✅ PR #1239 |
| EH-V1.30.1-3 | `dispatch.rs:207` swallows slot-name validation errors via `.ok()` | Error Handling | `src/cli/dispatch.rs:207` | easy | ✅ PR #1238 |
| EH-V1.30.1-4 | `doctor` silently treats `list_slots` failure as empty — diagnostic tool hides its diagnostic | Error Handling | `src/cli/commands/infra/doctor.rs:923` | easy | ✅ PR #1238 |
| EH-V1.30.1-5 | `cqs index --json` envelope reports model="" / chunk_count=0 on resolution failure | Error Handling | `src/cli/commands/index/build.rs:863-867` | easy | ✅ PR #1238 |
| EH-V1.30.1-6 | Reranker checksum-marker write silently dropped — re-verifies on every cold start | Error Handling | `src/reranker.rs:524` | easy | ✅ PR #1238 |
| OB-V1.30.1-1 | "SPLADE routing" `tracing::info!` fires on every search call — journal spam at default | Observability | `src/search/router.rs:469-554` | easy | ✅ PR #1239 |
| OB-V1.30.1-2 | `reclassify_with_centroid` info log per Unknown-gap fill — per-search spam | Observability | `src/search/router.rs:1146-1150` | easy | ✅ PR #1239 |
| OB-V1.30.1-4 | `wait_for_fresh` polling loop returns without closing tracing event recording outcome+elapsed | Observability | `src/daemon_translate.rs:660-679` | easy | ✅ PR #1211 |
| OB-V1.30.1-5 | `enforce_auth` 401 warn lacks rejection reason — three failure modes collapse to one log line | Observability | `src/serve/auth.rs:389-401, 269-321` | easy | ✅ PR #1237 |
| OB-V1.30.1-7 | `daemon_reconcile` walk has no `elapsed_ms` field — pass duration unrecoverable | Observability | `src/cli/watch/reconcile.rs:63-148` | easy | ✅ PR #1238 |
| TC-ADV-1.30.1-1 | `AuthToken::try_from_string` has no upper-bound length check, no length test | Test Coverage (adv) | `src/serve/auth.rs:123-129` | easy | ✅ PR #1237 |
| TC-ADV-1.30.1-2 | `check_request` ambiguous-channel collision tests (cookie+query) missing | Test Coverage (adv) | `src/serve/auth.rs:269-321` | easy | ✅ PR #1237 |
| TC-ADV-1.30.1-3 | `check_request` Bearer-prefix grammar gaps unpinned (`bearer`, double-space, no-space) | Test Coverage (adv) | `src/serve/auth.rs:271-280` | easy | ✅ PR #1237 |
| TC-ADV-1.30.1-7 | `env_disables_freshness_gate` test re-implements function inline — never calls it | Test Coverage (adv) | `src/cli/commands/eval/mod.rs:282-290, 405-432` | easy | ✅ PR #1235 |
| TC-ADV-1.30.1-8 | `WatchSnapshot::compute` `delta_saturated=true, pending=0` reports Fresh — no test | Test Coverage (adv) | `src/watch_status.rs:199-223` | easy | ✅ subsumed by CQ-V1.30.1-2 (P1 #1202). Notes: covered by CQ-V1.30.1-2 fix. |
| TC-ADV-1.30.1-9 | `daemon_status` non-`ok` envelope with null/number `message` returns `daemon error: daemon error` | Test Coverage (adv) | `src/daemon_translate.rs:487-499` | easy | ✅ PR #1237 |
| TC-ADV-1.30.1-10 | `unwrap_dispatch_payload` accepts missing `data` field — passes wrapper through as payload | Test Coverage (adv) | `src/daemon_translate.rs:387-404` | easy | ✅ PR #1237 |
| RB-2 | `wait_for_fresh` panics on `Instant + Duration::from_secs(u64::MAX)` if any caller skips cap | Robustness | `src/daemon_translate.rs:660-662` | easy | ✅ PR #1211 |
| RB-3 | `as_secs() as i64` SystemTime cast pattern landed in 5 new locations post-RB-V1.30-3 | Robustness | watch_status, batch, ping, watch/mod | easy | ✅ PR #1238 |
| RB-4 | `as_millis() as i64` cast in reindex pipeline truncates u128 to i64 silently | Robustness | `src/cli/watch/reindex.rs:507-508` | easy | ✅ PR #1238 |
| RB-5 | `migrate_legacy` sentinel read uses unbounded `fs::read_to_string` (RB-V1.30-2 sibling) | Robustness | `src/slot/mod.rs:656` | easy | ✅ PR #1238 |
| RB-7 | Atomic `as u64`/`as i64` casts in WatchSnapshot trust unbounded usize from caller | Robustness | `src/watch_status.rs:213-218` | easy | ✅ PR #1238 |
| RB-8 | `print_text_report::pct` on empty eval set → division by zero / NaN bleeds into report | Robustness | `src/cli/commands/eval/mod.rs:296-309` | medium | ✅ PR #1235 |
| SHL-V1.30-2 | `wait_for_fresh` poll interval hardcoded at 250 ms — no env knob | Scaling | `src/daemon_translate.rs:663` | easy | ✅ PR #1211 |
| SHL-V1.30-3 | `eval --require-fresh-secs` silently capped to 600 s inside the wait helper | Scaling | `src/cli/commands/eval/mod.rs:237` | easy | ✅ PR #1235 |
| SHL-V1.30-4 | `task::run_task` hardcodes BFS knobs (depth=2, max_nodes=100) overriding `CQS_GATHER_*` | Scaling | `src/task.rs:19-25, 143-149` | easy | ✅ PR #1236 |
| SHL-V1.30-5 | `onboard` callee/caller fetch caps hardcoded at 30/15 — silent truncation | Scaling | `src/onboard.rs:30-33, 174-175` | easy | ✅ PR #1236 |
| SHL-V1.30-6 | `MAX_REFERENCES = 20` hardcoded — no env knob, silent truncation past 20 | Scaling | `src/config.rs:390-405` | easy | ✅ PR #1236 |
| SHL-V1.30-7 | `MAX_NOTES_FILE_SIZE = 10 MB` hardcoded twice + `MAX_NOTES = 10_000` silent truncation | Scaling | `src/note.rs:20, 169, 245` | easy | ✅ PR #1236 |
| SHL-V1.30-8 | `ENRICHMENT_PAGE_SIZE = 500` hardcoded — no env knob | Scaling | `src/cli/enrichment.rs:46, 127` | easy | ✅ PR #1236 |
| SHL-V1.30-9 | `LAST_INDEXED_PRUNE_SIZE_THRESHOLD = 5_000` "intentionally not env" — but `cqs ref` exceeds | Scaling | `src/cli/watch/gc.rs:36-42` | easy | ✅ PR #1236 |
| SHL-V1.30-10 | `daemon_periodic_gc_cap` env override is `OnceLock`-cached — `systemctl set-environment` ineffective | Scaling | `src/cli/watch/gc.rs:78-86` | easy | ✅ PR #1236 |
| AC-V1.30.1-2 | `is_structural_query` keyword match is case-sensitive — `Class Foo` misroutes to Conceptual | Algorithm Correctness | `src/search/router.rs:813-816` | easy | ✅ PR #1239 |
| AC-V1.30.1-6 | `wait_for_fresh` deadline math allows one over-budget poll on slow daemon (~30s slack) | Algorithm Correctness | `src/daemon_translate.rs:660-679` | easy | ✅ PR #1211 |
| AC-V1.30.1-7 | `BoundedScoreHeap::push` score-equality uses `==` not `total_cmp` — bypass-prone | Algorithm Correctness | `src/search/scoring/candidate.rs:231` | easy | ✅ PR #1239 |
| AC-V1.30.1-8 | `idle_secs` truncates sub-second freshness — first 9 ticks after event report 0 | Algorithm Correctness | `src/watch_status.rs:219` | easy | ✅ subsumed by #1208 (idle_secs renamed to last_event_unix_secs) |
| EX-V1.30.1-3 | `log_query` hand-sprinkled across 6 dispatch arms — should be a property of the command | Extensibility | `src/cli/batch/commands.rs` | easy | ✅ PR #1239 |
| EX-V1.30.1-7 | Env-var falsy parsing hand-rolled in `eval/mod.rs`, not reused for 30+ other CQS_* env vars | Extensibility | `src/cli/commands/eval/mod.rs:282-289` | easy | ✅ PR #1239 |
| PB-V1.30.1-2 | `--bind localhost` documented as valid but always fails `SocketAddr::parse` | Platform Behavior | `src/cli/commands/serve.rs:39-41` | easy | ✅ PR #1233 |
| PB-V1.30.1-6 | `atomic_replace` opens parent dir on every Windows write — always fails, debug-spam | Platform Behavior | `src/fs.rs:90-108` | easy | ✅ PR #1233 |
| PB-V1.30.1-8 | `cqs hook` reports use `\`-separated paths on Windows — every other JSON consumer expects `/` | Platform Behavior | `src/cli/commands/infra/hook.rs:99-105, 152, 354` | easy | ✅ PR #1238 |
| PB-V1.30.1-10 | `restart_daemon_if_needed` Linux hardcodes `cqs-watch.service` — fails confusingly without unit | Platform Behavior | `src/cli/commands/infra/model.rs:710-738` | easy | ✅ PR #1233 |
| SEC-V1.30.1-9 | `cqs ref add` ref dir 0o700 but parent `~/.local/share/cqs/refs/` inherits umask | Security | `src/cli/commands/infra/reference.rs:137-145` | easy | ✅ PR #1237 |
| SEC-V1.30.1-10 | `cqs ref add` does not set 0o600 on index DB after writing — falls back to umask defaults | Security | `src/cli/commands/infra/reference.rs:165-178` | easy | ✅ PR #1237 |
| PF-V1.30.1-4 | `run_daemon_reconcile` allocates fresh String per disk file via `replace('\\', "/")` even on Linux | Performance | `src/cli/watch/reconcile.rs:99` | easy | ✅ PR #1238 |
| PF-V1.30.1-5 | `build_stats` issues 4 sequential `fetch_one` round-trips for what is one query | Performance | `src/serve/data.rs:1105-1128` | easy | ✅ PR #1233 |
| PF-V1.30.1-6 | `enforce_auth` allocates two strings per HTTP request for cookie name + lookup needle | Performance | `src/serve/auth.rs:357, 292` | easy | ✅ PR #1237 |
| PF-V1.30.1-7 | Watch reindex clones each `content_hash` String into Vec<String> for incremental HNSW | Performance | `src/cli/watch/reindex.rs:414-417` | easy | ✅ PR #1238 |
| PF-V1.30.1-1 | Daemon publishes watch snapshot every 100ms with `fs::metadata(index_path)` syscall | Performance | `src/cli/watch/mod.rs:149-185, 1303` | easy | ✅ PR #1238 |
| RM-1 | `daemon-client` thread_local `REQ_LINE` is no-op — daemon spawns fresh thread per accept | Resource Management | `src/cli/watch/socket.rs:91-99`, `daemon.rs:189-205` | easy | ✅ PR #1233 |
| RM-3 | `compute_context` reads entire source file into RAM to extract N context lines | Resource Management | `src/cli/display.rs:59-99, 489` | easy | ✅ PR #1236 |
| RM-6 | `serve` auth path allocates fresh `format!("{cookie_name}=")` per request | Resource Management | `src/serve/auth.rs:292` | easy | ✅ PR #1237 |
| RM-7 | `check_request` cookie loop scans every `;`-separated pair — subsumed by RM-6 | Resource Management | `src/serve/auth.rs:287-300` | easy | ✅ subsumed by RM-6 (PR #1237). Notes: subsumed by RM-6. |
| TC-HAP-1.30.1-1 | `cmd_install` upgrade-marker path never executed by any test | Test Coverage (happy) | `src/cli/commands/infra/hook.rs:149-200` | easy | ✅ PR #1235 |
| TC-HAP-1.30.1-8 | `WatchSnapshot::compute` `Rebuilding` state entry untested through `compute()` | Test Coverage (happy) | `src/watch_status.rs` | easy | ✅ PR #1235 |
| TC-HAP-1.30.1-9 | `print_text_report` row-by-row format unpinned — spec-compat guarantee unenforced | Test Coverage (happy) | `src/cli/commands/eval/mod.rs:296+` | easy | ✅ PR #1235 |
| TC-HAP-1.30.1-10 | `daemon_reconcile` happy-path pinned but `args` payload (hook arguments) never observed | Test Coverage (happy) | `src/daemon_translate.rs:1102+`, `hook.rs:296-322` | easy | ✅ PR #1235 |
| TC-HAP-1.30.1-5 | `wait_for_fresh` Stale → Fresh transition (realistic case) untested; only "fresh on first poll" pinned | Test Coverage (happy) | `src/daemon_translate.rs:660-679` | medium | 📌 #1240 |
| TC-ADV-1.30.1-5 | Reconcile clock-skew (stored mtime in future) keeps file out of queue forever — no test | Test Coverage (adv) | `src/cli/watch/reconcile.rs:108-132` | medium | 📌 #1242 |
| TC-ADV-1.30.1-6 | Reconcile `metadata()` Err silently maps to "leave to GC" — masks permission-denied stale state | Test Coverage (adv) | `src/cli/watch/reconcile.rs:116-127` | medium | 📌 #1243. Notes: paired with EH-V1.30.1-7. |
| TC-ADV-1.30.1-4 | `wait_for_fresh` daemon-dies-mid-poll path untested; partial-read / malformed JSON path untested | Test Coverage (adv) | `src/daemon_translate.rs:660-679` | medium | 📌 #1241 |
| RM-4 | `build_hnsw_index_owned` accumulates 17 MB content_hash snapshot used only at swap | Resource Management | `src/cli/commands/index/build.rs:1110-1126`, `rebuild.rs:262-380` | medium | 📌 #1244 |
| PB-V1.30.1-9 | `reconcile.rs:128` inserts non-normalized PathBuf — same-file Windows separator skew possible | Platform Behavior | `src/cli/watch/reconcile.rs:103, 128`, `events.rs:84, 109` | medium | 📌 #1245 |

## P4 — Hard or low impact (file as issues; trivial inline fixes welcome)

| ID | Title | Category | Location | Effort | Status |
|----|-------|----------|----------|--------|--------|
| EX-V1.30.1-1 | `daemon_ping`/`status`/`reconcile` are three near-identical 80-LOC copies — refactor target | Extensibility | `src/daemon_translate.rs:271-621` | medium | 📌 #1215 |
| EX-V1.30.1-2 | `BatchCmd` dispatch is 130-line hand-routed match; macro escape hatch already proven | Extensibility | `src/cli/batch/commands.rs:503-636` | medium | 📌 #1216 |
| EX-V1.30.1-4 | `write_slot_model` clobbers all non-`[embedding]` keys — adding any per-slot field is breaking | Extensibility | `src/slot/mod.rs:307-351` | medium | 📌 #1217 |
| EX-V1.30.1-5 | `check_request` is hardcoded three-channel ladder — fourth auth channel needs trait + registry | Extensibility | `src/serve/auth.rs:269-321, 246-260, 323-332` | medium | 📌 #1218 |
| EX-V1.30.1-6 | Reconcile fingerprint is `(path, mtime)` only — content-hash/size detection needs schema migration | Extensibility | `src/cli/watch/reconcile.rs:84-134` | hard | 📌 #1219 |
| EX-V1.30.1-8 | `Reranker` is concrete struct with hardcoded ONNX assumptions — no trait, blocks ablations | Extensibility | `src/reranker.rs:108-211` | hard | 📌 #1220 |
| SEC-V1.30.1-5 | Search results emit `trust_level: "user-code"` for vendored third-party content in project tree | Security | `src/store/helpers/types.rs:172-196` | hard | 📌 #1221 (proper fix); doc clarification still pending inline |
| SEC-V1.30.1-6 | `cqs ref add` accepts symlinked source path with no audit — references can index outside source root | Security | `src/cli/commands/infra/reference.rs:130-150` | medium | 📌 #1222 |
| SEC-V1.30.1-7 | `LocalProvider` follows up to 2 redirects on `Authorization`-bearing requests — bearer leak risk | Security | `src/llm/local.rs:124-129, 435-437` | medium | 📌 #1223 |
| PB-V1.30.1-4 | `open_browser` on WSL launches Linux browser via `xdg-open` instead of Windows default | Platform Behavior | `src/cli/commands/serve.rs:99-132` | medium | 📌 #1224 |
| PB-V1.30.1-5 | `events.rs` mtime-equality skip wrong on macOS HFS+ — only `is_wsl_drvfs_path` triggers strict `<` | Platform Behavior | `src/cli/watch/events.rs:85-102` | medium | 📌 #1225 |
| PF-V1.30.1-2 | `wait_for_fresh` polls daemon every 250 ms with fresh socket connect + JSON round-trip | Performance | `src/daemon_translate.rs:660-679, 422-510` | medium | ✅ subsumed (RB-9 #1211 + RM-2 #1228) |
| PF-V1.30.1-3 | Periodic GC and reconcile each call `enumerate_files` independently — back-to-back tree walks | Performance | `src/cli/watch/mod.rs:1198-1283`, `gc.rs:209`, `reconcile.rs:74` | medium | 📌 #1226 |
| PF-V1.30.1-8 | `indexed_file_origins` returns `HashMap<String, Option<i64>>` from `SELECT DISTINCT` overwrites silently | Performance | `src/store/chunks/staleness.rs:627-637` | medium | 📌 #1227 |
| RM-2 | `wait_for_fresh` opens fresh socket connect+disconnect every 250ms for up to 600s | Resource Management | `src/daemon_translate.rs:660-679, 438` | medium | 📌 #1228 |
| RM-5 | Reconcile pass holds entire repo's filename set + index origins simultaneously every 30s | Resource Management | `src/cli/watch/reconcile.rs:74-90`, `lib.rs:618` | medium | 📌 #1229 |
| TC-HAP-1.30.1-6 | `process_file_changes` — central watch-loop reindex function has zero direct tests | Test Coverage (happy) | `src/cli/watch/events.rs:131-300+` | hard | 📌 #1230 |
| DS-V1.30.1-D3 | Periodic reconcile reads through stale `store` handle on `cqs index --force` race | Data Safety | `src/cli/watch/mod.rs:1262-1283` | medium | 📌 #1231 |
| DS-V1.30.1-D4 | `slot remove` does not check whether daemon is actively serving the slot it deletes | Data Safety | `src/cli/commands/infra/slot.rs:322-369` | medium | 📌 #1232 |
| DS-V1.30.1-D6 | `WatchSnapshot.delta_saturated` ignored by `compute()` — duplicate of CQ-V1.30.1-2 | Data Safety | `src/watch_status.rs:199-209` | easy | ✅ subsumed by CQ-V1.30.1-2 (P1 #1202) |
| DS-V1.30.1-D8 | `dropped_this_cycle` reset before snapshot publish — duplicate of CQ-V1.30.1-1 | Data Safety | `src/cli/watch/events.rs:139-146` | easy | ✅ subsumed by CQ-V1.30.1-1 (P1 #1202) |
| DOC-V1.30.1-8 | CONTRIBUTING test count + file count out of date — folded into DOC-V1.30.1-3 | Documentation | `CONTRIBUTING.md` | easy | ✅ subsumed by DOC-V1.30.1-3 (PR #1214) |
