# Audit Triage (post-v1.40.0)

Audit date: 2026-05-08
Main HEAD: e2f64baa (currency fixes after #1624)
Source: `docs/audit-findings.md` (150 raw findings across 16 categories)
Calibration: matches `docs/audit-triage-v1.38.0-final.md` priority matrix.

After consolidation: **150 raw → 78 triage entries.** Heavy overlap: the v1.40.0 surface (polymorphic-routing Phase 1, SNR Phase 4 default flip, Tier 2b macro filter, daemon path duplication) hits multiple auditor lenses for the same root cause.

## Summary by Priority

| Priority | Count | Disposition |
|----------|-------|-------------|
| P1 | 23 | fix this PR — easy + high impact, lying docs, correctness bugs, data corruption, security holes |
| P2 | 20 | fix this batch — medium effort + high impact, regression-test backfill, hot-path perf |
| P3 | 27 | fix if time — easy + low impact (style, doc tweaks, single-test additions, observability nits) |
| P4 | 8 | defer — hard / low-impact / tracked under #1463/#1459/#1512 umbrella issues |
| **Total** | **78** | (from 150 raw) |

---

## Cross-cutting clusters

These clusters drive the recommended PR order. A single PR per cluster collapses 3-7 findings into one fix-pattern review.

### Cluster A — Tier 2b macro filter `filter_invoked_macros` (correctness + perf + tests)
PR #1623 shipped Tier 2b but landed with a SQL-injection-shaped LIKE wildcard bug, no tests, N+1 SQL, language-specific `!` suffix, and case-insensitive match. **5 findings → one PR** (escape + ESCAPE clause + case-sensitive + Rust-only gate + tests).
- EH-V1.40-1 / AC-V1.40-8 (LIKE wildcard escape — `_`/`%`/`\\` not escaped)
- AC-V1.40-1 (Rust `!` suffix misclassifies C/C++/Elixir/Julia macros)
- AC-V1.40-2 (self-match — recursive `macro_rules!` keeps itself alive)
- PB-V1.40-1 (LIKE is case-insensitive — case-distinct names cross-fire)
- TC-ADV-V1.40-5 / TC-HAP-V1.40-2 (zero tests — happy path or adversarial)
- CQ-V1.40-10 / RB-V1.40-4 / EH-V1.40-10 / DS-V1.40-6 / PERF-V1.40-1 (N+1 SQL — separate Cluster A2 fix, may follow same PR)

### Cluster B — Posture/OutputFormat env-var hygiene (security + perf + observability + DS + tests)
Six auditor angles on one root cause: `Posture::current()` and `OutputFormat::current()` re-read env per call, silently swallow malformed values, no log, no caching → data race + perf + opaque ops. **One PR**: `OnceLock<Posture>`, `OnceLock<OutputFormat>`, accept truthy/falsy aliases, log first read, plumb into `write_json_line`/`emit_json` once per request.
- EH-V1.40-2 / API-V1.40-8 / OB-V1.40-3 / PB-V1.40-3 / SEC-V1.40-6 / DS-V1.40-5 (silent swallow + per-emit re-read, TOCTOU, no log)
- CQ-V1.40-7 / RM-V1.40-8 / PERF-V1.40-3 / PERF-V1.40-4 (env-thrash sibling — `resolve_splade_alpha`, `cap_k_to_backend`, `meta_json_fragment_for_posture`)
- CQ-V1.40-5 / CQ-V1.40-6 (Phase 1 `_with_posture` plumbing dead in production — every CLI caller still hits the env-reading shim; `emit_json_with_posture` and `emit_json_error_with_posture` are `#[allow(dead_code)]`)
- TC-ADV-V1.40-7 / TC-HAP-V1.40-9 (env-var edge case + compose contract tests)

### Cluster C — Polymorphic-routing duplication (CQ + API + EXT + RM)
Phase 1 landed shape across 6 commands × 2 surfaces with hand-rolled per-cell duplication: `chunks_to_definitions` 7×, `KindNotes` 5× on daemon side, redirect notes 24+ string pairs, divergent dispatcher signatures (`format: &OutputFormat` vs `json: bool`), `KindHit` allocs wasted on Function happy path. **One refactor PR**: `kind_fallback` module with `chunks_to_definitions` + per-(command × kind) note table + standardized signature.
- CQ-V1.40-1 / CQ-V1.40-2 / API-V1.40-4 / RM-V1.40-1 / TC-HAP-V1.40-7 (`detect_kind_for_store` dead in production — every caller inlines the 3-line incantation; `KindHit` clones wasted)
- CQ-V1.40-3 / EXT-V1.40-2 / EXT-V1.40-10 / TC-HAP-V1.40-10 (per-command fallback + note duplication 7-24×; `chunks_to_definitions` only tested for one ChunkType)
- CQ-V1.40-4 (CLI vs daemon redirect-note drift)
- CQ-V1.40-9 (`cmd_impact_const_fallback` is hand-written duplicate of `cmd_impact_kind_fallback`)
- API-V1.40-1 (divergent `*_kind_fallback` dispatcher signatures)
- API-V1.40-2 (Kind enum mixes 5 routing kinds with 3 resolution outcomes)
- EXT-V1.40-1 (`_ => {}` fallthrough — no compile-time push for new Kind variants)

### Cluster D — Polymorphic-routing data safety (DS + RB + PERF)
Every Phase 1 dispatcher adds a `lookup_by_name` SQL roundtrip BEFORE the existing-flow query. No `chunks.name` index → full table scan; no shared transaction → snapshot drift; `ORDER BY chunk_type` is alphabetical not routing-priority. **One PR**: schema migration (v28) adding `idx_chunks_name`, single read tx in dispatch handlers, CASE-priority ORDER BY, `lookup_by_name` LIMIT.
- RB-V1.40-2 / PERF-V1.40-2 / RM-V1.40-4 (missing `chunks.name` index; full table scan per dispatch; `format!`-built SQL string per call)
- DS-V1.40-1 (daemon `BatchContext` Store cache never invalidates from watch-loop WAL writes — WAL-mode masks main-DB identity; use `PRAGMA data_version`)
- DS-V1.40-8 / DS-V1.40-10 (kind-detect + real-query share no read transaction)
- AC-V1.40-9 (`lookup_by_name` ORDER BY alphabetical chunk_type)
- SHL-V1.40-1 (no result cap — hot names deserialize thousands of chunks for kind classification)

### Cluster E — Lying docs sweep (P1 by team rule)
Per CLAUDE.md memory: docs that promise behavior the code doesn't deliver are correctness bugs.
- DOC-V1.40-1 (ROADMAP polymorphic-routing item shows `[ ]` "ready"; Phase 1 complete)
- DOC-V1.40-2 (`docs/polymorphic-routing.md` Status: ready to execute; Phase 1 shipped)
- DOC-V1.40-3 (`docs/json-snr-restoration.md` Status: ready to execute; Phases 1-4 shipped)
- DOC-V1.40-4 (CHANGELOG `[Unreleased]` empty after 7 substantive PRs since v1.40.0 cut — same recurring pattern as v1.33/v1.38)
- DOC-V1.40-5 (SECURITY.md describes `_meta.handling_advice` + `injection_flags` as always-on; opt-in since v1.40.0)
- DOC-V1.40-6 (CONTRIBUTING.md Architecture Overview missing `kind.rs` + `posture.rs`)
- DOC-V1.40-7 (Cargo.toml + README headline cite stale 46.3/74.8/86.2; current 52.7/77.5/89.4 post-#1607 fixture refresh)
- OB-V1.40-4 (`classify_chunk_type` doc promises tracing::warn that code never emits)

### Cluster F — V2Bare default test gap (TC-ADV + TC-HAP)
SNR Phase 4 (this commit) flipped CLI direct default to bare payload. 38 integration tests pin `CQS_OUTPUT_FORMAT=v1`; nothing exercises the new default path end-to-end.
- TC-ADV-V1.40-1 / TC-HAP-V1.40-1 / TC-HAP-V1.40-8 / TC-HAP-V1.40-9 (V2Bare default emission untested; `emit_json` itself untested at binary boundary; compose contract `CQS_ULTRASECURITY` × `CQS_OUTPUT_FORMAT` unverified)
- SEC-V1.40-1 (V2Bare drops `_meta.worktree_stale` — silent operational degradation)

### Cluster G — Phase 1 dispatch observability (OB + TC)
Without tracing/telemetry on kind-fallback dispatches, the v1.40.0 hypothesis ("agents misroute against types/consts") is unmeasurable on production traffic.
- OB-V1.40-1 (kind-fallback dispatchers emit no `tracing::info!`)
- OB-V1.40-2 (`cqs telemetry` has no kind-fallback category)
- OB-V1.40-5 (`detect_kind_for_store` and `classify_hits` lack entry spans)
- OB-V1.40-6 (Tier 2b filter drops candidates silently — operator can't audit FP rate)
- OB-V1.40-7 (`write_json_line` posture decision unattributable)
- OB-V1.40-8 / OB-V1.40-9 / OB-V1.40-10 (daemon accept successes, max_clients, redact_query_str all silent)
- TC-ADV-V1.40-4 / TC-ADV-V1.40-9 / TC-HAP-V1.40-3 / TC-HAP-V1.40-4 (daemon-path kind-fallback tests + CLI integration tests + happy-path baseline)

### Cluster H — Resource caps + DoS (SEC + RM + SHL + RB)
Daemon hot path has no per-response size cap; `try_kind_fallback` echoes full chunk content for hot names like `Result`/`Error`/`new`; `read_line` allocates 5 MB pre-allocation per connection.
- EH-V1.40-9 / SEC-V1.40-4 (kind-fallback echoes full chunk content with no per-chunk size cap)
- SEC-V1.40-7 (daemon `read_line` pre-allocates `take_cap` bytes per connection)
- SHL-V1.40-1 (`lookup_by_name` no LIMIT)
- RB-V1.40-1 (`build_test_map` `chain_limit = max_depth + 1` panics on `usize::MAX`)
- SHL-V1.40-10 (`chunks_paged` accepts unbounded limit)
- SEC-V1.40-8 (`enumerate_files` no depth/file-count cap on adversarial repos)

---

## P1 — fix this PR (23 entries)

### Correctness (load-bearing bugs)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| EH-V1.40-1 + AC-V1.40-8 | `filter_invoked_macros` LIKE pattern doesn't escape `_`/`%`/`\\` (Tier 2b correctness — macros with underscore cross-fire) | easy | ✅ Cluster A PR |
| AC-V1.40-1 | `filter_invoked_macros` `!` suffix is Rust-only — every C/C++/Elixir/Julia macro misclassified as dead | medium | ✅ Cluster A PR |
| AC-V1.40-2 | `filter_invoked_macros` self-match — recursive `macro_rules!` keeps itself alive (no `id != ?2` filter) | easy | ✅ Cluster A PR |
| PB-V1.40-1 | `filter_invoked_macros` LIKE is case-insensitive (no `PRAGMA case_sensitive_like` or GLOB) | easy | ✅ Cluster A PR |
| AC-V1.40-3 | `bfs_shortest_path` predecessor sentinel `String::new()` collides with anonymous-chunk callers — path reconstruction terminates early | medium | TODO |
| AC-V1.40-4 | Rust Tier 2a `field_initializer` query captures every argument identifier — non-callable variables pollute `function_calls` | medium | TODO |
| AC-V1.40-5 | `bfs_expand` depth update is score-gated — lower-score shorter paths leave depth at longer-path value | medium | TODO |
| RB-V1.40-1 | `build_test_map` `chain_limit = max_depth + 1` panics on `usize::MAX` (no clap range bound) | easy | ✅ misc P1 PR |
| RB-V1.40-3 | `classify_hits` uses `.expect()` — violates "no unwrap outside tests" rule (replace with `unwrap_or(Kind::Other)`) | easy | ✅ misc P1 PR |

### Lying docs (Cluster E)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| DOC-V1.40-1 + DOC-V1.40-2 + DOC-V1.40-3 | ROADMAP + polymorphic-routing.md + json-snr-restoration.md status headers say "ready to execute" — Phase 1 complete, Phases 1-4 of SNR shipped | easy | ✅ Cluster E PR |
| DOC-V1.40-4 | CHANGELOG `[Unreleased]` empty after 7 PRs since v1.40.0 cut (DOC-V1.38-5 sibling, recurrence) | easy | ✅ Cluster E PR |
| DOC-V1.40-5 | SECURITY.md describes `_meta.handling_advice` + `injection_flags` as always-on; both opt-in since v1.40.0 — operators deploy under wrong assumption | easy | ✅ Cluster E PR |
| DOC-V1.40-6 | CONTRIBUTING.md Architecture Overview missing `kind.rs` + `posture.rs` | easy | ✅ Cluster E PR |
| DOC-V1.40-7 | Cargo.toml + README headline cite stale 46.3/74.8/86.2; verified current is 52.7/77.5/89.4 post-#1607 | easy | ✅ Cluster E PR |

### Data safety / security

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| EH-V1.40-2 + API-V1.40-8 + OB-V1.40-3 + PB-V1.40-3 + SEC-V1.40-6 + DS-V1.40-5 | `Posture::current` / `OutputFormat::current` silent swallow + per-emit re-read + no log + TOCTOU + cross-platform case sensitivity (Cluster B) | easy-medium | TODO (Cluster B) |
| DS-V1.40-1 | Daemon `BatchContext` Store caches never invalidate from watch-loop WAL writes (WAL masks main-DB identity); use `PRAGMA data_version` | medium | TODO (Cluster D) |
| DS-V1.40-2 | `cmd_telemetry_reset` non-atomic — kill between `fs::copy` and `fs::write` corrupts archive or current | easy | ✅ misc P1 PR |
| DS-V1.40-3 | `restore_from_backup` `copy_triplet` non-atomic across (main, -wal, -shm) — kill mid-restore replays failed-migration WAL frames against pre-migrate main | medium | TODO |
| DS-V1.40-4 | `evals/regenerate_v3_test.py` writes fixture via `Path.write_text` non-atomically — Ctrl+C corrupts eval ground truth | easy | ✅ misc P1 PR |
| DS-V1.40-7 | Sentiment column accepts arbitrary f32 — schema lets `cqs notes add --sentiment 0.7` corrupt ranking-boost contract; add `CHECK (sentiment IN (-1.0, -0.5, 0.0, 0.5, 1.0))` | easy | TODO |
| SEC-V1.40-1 | V2Bare default drops `_meta.worktree_stale` warning — silent operational degradation under default Friendly posture (#1254 leakage guard regression) | medium | TODO (Cluster F/H) |
| SEC-V1.40-2 | `redact_userinfo` mishandles URLs with `@` in path — produces malformed redacted form (find authority boundary first via `/`) | easy | ✅ misc P1 PR |
| EH-V1.40-9 + SEC-V1.40-4 | Kind-fallback `definitions[]` echoes full chunk content with no size cap — DoS amplifier via `Result`/`Error`/`new` hot names | medium | TODO (Cluster H) |

---

## P2 — fix this batch (20 entries)

### Polymorphic-routing refactor (Cluster C)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| CQ-V1.40-1 + CQ-V1.40-2 + API-V1.40-4 + RM-V1.40-1 + TC-HAP-V1.40-7 | `detect_kind_for_store` dead in production; every caller inlines 3-line incantation; `KindHit` allocations wasted; needs tests | easy | TODO (Cluster C) |
| CQ-V1.40-3 + EXT-V1.40-2 + EXT-V1.40-10 + TC-HAP-V1.40-10 | `chunks_to_definitions` duplicated 7× across CLI + daemon path; per-command notes 5× on daemon side; `KindNotes` per-dispatcher; one ChunkType tested | medium | TODO (Cluster C) |
| CQ-V1.40-4 | CLI vs daemon redirect-note drift (12+ near-duplicate string pairs) | easy | TODO (Cluster C) |
| CQ-V1.40-9 | `cmd_impact_const_fallback` is hand-written duplicate of `cmd_impact_kind_fallback` | easy | TODO (Cluster C) |
| API-V1.40-1 | 6 `*_kind_fallback` dispatchers diverge on `format: &OutputFormat` vs `json: bool` | medium | TODO (Cluster C) |
| API-V1.40-2 | Kind enum mixes 5 routing kinds + 3 resolution outcomes — split into `Kind` + `KindResolution` | medium | TODO (Cluster C) |
| EXT-V1.40-1 | Per-command kind dispatchers use `_ => {}` fallthrough — no compile-time push for new Kind variant | medium | TODO (Cluster C) |
| TC-HAP-V1.40-4 | Graph-command kind-fallback tests reconstruct JSON manually — never call dispatcher functions; refactor + retest | easy | TODO (Cluster C) |

### Daemon-path data safety + perf (Cluster D)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| RB-V1.40-2 + PERF-V1.40-2 + RM-V1.40-4 | Missing `chunks.name` index — every polymorphic-routing call is full table scan; `lookup_by_name` allocates fresh SQL string via `format!` per call | medium | TODO (Cluster D, schema migration v28) |
| DS-V1.40-8 + DS-V1.40-10 | Kind-detect + real-query share no read transaction — snapshot drift between dispatcher and existing-flow | medium | TODO (Cluster D) |
| AC-V1.40-9 | `lookup_by_name` ORDER BY alphabetical (Class before Function for same-name siblings) — should be routing-priority | easy | TODO (Cluster D) |
| SHL-V1.40-1 | `lookup_by_name` no LIMIT — hot names deserialize thousands of chunks for kind classification | medium | TODO (Cluster D) |

### Test backfill for shipped fixes (Cluster F + new)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| TC-ADV-V1.40-1 + TC-HAP-V1.40-1 + TC-HAP-V1.40-8 + TC-HAP-V1.40-9 | V2Bare default emission untested at binary boundary; `emit_json` untested; compose contract `CQS_ULTRASECURITY × CQS_OUTPUT_FORMAT` unverified (Cluster F) | easy-medium | TODO |
| TC-ADV-V1.40-4 + TC-HAP-V1.40-3 | Daemon-path `try_kind_fallback` zero tests for any kind; daemon `dispatch_test_map/trace/deps/impact` zero kind-fallback tests | medium | TODO |
| TC-ADV-V1.40-5 + TC-HAP-V1.40-2 | `filter_invoked_macros` zero tests (happy path + adversarial — pin EH-V1.40-1 fix) | easy | ✅ Cluster A PR (tests added) |
| TC-ADV-V1.40-9 | CLI graph commands' Const/Type/Module/Ambiguous have unit-shape tests but zero CLI integration tests | medium | TODO |

### Hot-path perf (Cluster B sibling)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| CQ-V1.40-7 + RM-V1.40-8 + PERF-V1.40-3 + PERF-V1.40-4 | Posture/OutputFormat/SPLADE-α/CAGRA-cap env reads per emit/encode/search — `OnceLock`/`LazyLock` cache (Cluster B sibling) | easy | TODO (Cluster B) |
| CQ-V1.40-5 + CQ-V1.40-6 | SNR Phase 1 `_with_posture` plumbing dead — every CLI caller still hits env-reading shim; `emit_json_with_posture` `#[allow(dead_code)]` | easy-medium | TODO (Cluster B follow-up) |
| RM-V1.40-9 + PERF-V1.40-5 + RM-V1.40-10 | Daemon socket triple payload allocation (Vec → Value → String); `emit_json` double serialization (`to_value` then `to_string_pretty`) — land `dispatch_value` refactor; refactor `format_envelope_to_string` to `&impl Serialize` | medium | TODO |
| PERF-V1.40-1 + RB-V1.40-4 + EH-V1.40-10 + DS-V1.40-6 + CQ-V1.40-10 | `filter_invoked_macros` N+1 SQL with leading-`%` LIKE (full table scan × N macros) + no transaction — batch into single scan inside read tx (lands with Cluster A correctness fix) | medium | TODO (Cluster A2) |

### Other P2

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| AC-V1.40-6 | `cqs trace <macro> <target>` falls through to BFS — `Kind::Other` Macros silently return empty path | easy | TODO |
| AC-V1.40-7 | `classify_hits` Method+Function with same name → `Multiple` masks API ambiguity — should surface `definitions[]` alongside BFS result | medium | TODO |
| EH-V1.40-3 | Trace `target` kind not validated (asymmetric with `source`) | medium | TODO |
| EH-V1.40-8 + PB-V1.40-7 | `try_kind_fallback` propagates `lookup_by_name` SQL errors as fatal — should fall through; Windows `ERROR_SHARING_VIOLATION` makes this worse | medium | TODO |
| SEC-V1.40-7 | Daemon `read_line` pre-allocates `take_cap` (5 MB) per connection — incremental growth via `read_until` | medium | TODO (Cluster H) |
| SEC-V1.40-8 | `enumerate_files` no depth/file-count cap on adversarial repos | medium | TODO (Cluster H) |
| DS-V1.40-9 | v26→v27 migration sets `needs_embedding=1` but doesn't zero-vec the `embedding` column — half-state contradicts v27 invariant | medium | TODO |
| PB-V1.40-9 | WAL mode unconditional on every store open; SQLite WAL unsupported on `/mnt/c/` 9P bridge — detect at open and switch to DELETE | medium | TODO |
| PERF-V1.40-7 | `build_test_map` iterates all 4270 test chunks per call — invert to iterate ancestors | easy | TODO |
| PERF-V1.40-8 | `meta_json_fragment_for_posture` triple-work per JSONL emit — `LazyLock<HashMap<Posture, String>>` (lands with Cluster B) | easy | TODO |
| RM-V1.40-6 + RM-V1.40-7 | `CrossProjectContext::merged_call_graph` rebuilt per request; reference stores reopened (64 MB mmap × N refs); use `open_readonly_small`; cache in `BatchContext` | medium | TODO |

---

## P3 — fix if time (27 entries)

### Observability

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| OB-V1.40-1 | Kind-fallback dispatchers emit no `tracing::info!` — Phase 2 prioritization signal lost | easy | TODO |
| OB-V1.40-2 | `cqs telemetry` has no kind-fallback category | medium | TODO |
| OB-V1.40-4 | `classify_chunk_type` doc promises `tracing::warn` — code never emits (Cluster E sibling — fix doc OR add warn) | easy | ✅ Cluster E PR |
| OB-V1.40-5 | `detect_kind_for_store` and `classify_hits` no entry span (lands with Cluster C) | easy | TODO |
| OB-V1.40-6 | Tier 2b filter drops candidates silently — operator can't audit FP rate | easy | TODO |
| OB-V1.40-7 | `write_json_line` posture decision unattributable (lands with Cluster B) | easy | TODO |
| OB-V1.40-8 | Daemon `accept()` successes have no log — handle leaks unattributable | easy | TODO |
| OB-V1.40-9 | `max_concurrent_daemon_clients` env override no log | easy | TODO |
| OB-V1.40-10 + SEC-V1.40-3 + TC-ADV-V1.40-6 | `redact_query_str` silent on activation; strict-equality `v != "0"` lacks audit trail; zero tests | easy | TODO |

### Style / code quality nits

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| EH-V1.40-4 | `dispatch_deps` Type-forward semantics misroute Function names without warning | easy | TODO |
| EH-V1.40-5 | `meta_json_fragment_for_posture` `.expect()` panic on hot path — defensive inconsistency | easy | TODO |
| EH-V1.40-6 | `redact_error` chain_id `DefaultHasher` is non-deterministic across processes — doc claims otherwise (fix doc) | easy | ✅ Cluster E PR |
| EH-V1.40-7 | `lookup_by_name` empty-string short-circuit — document or warn | easy | TODO |
| API-V1.40-3 | `Store::lookup_by_name` breaks `get_chunks_by_X` naming convention — rename to `get_chunks_by_name` | easy | TODO (Cluster C) |
| API-V1.40-5 | `OutputFormat` type-name collision (posture vs cli::definitions) — rename `posture::OutputFormat` → `EnvelopeShape` | medium | TODO (Cluster B follow-up) |
| API-V1.40-6 | `emit_json_error_with_data_and_posture` ignores posture when shaping envelope — contradicts `wrap_error_with_posture` | easy | TODO |
| API-V1.40-9 | `CQS_DEFERRED_FLUSH_INTERVAL` is a count not a duration — rename to `_BATCHES` | easy | TODO |
| API-V1.40-10 | `--format` and `--json` flags shadow each other — drop `--json` from `OutputArgs` | easy | TODO |
| CQ-V1.40-8 | `meta_value_for_envelope` + `meta_json_fragment_for_posture` duplicate fallback Map construction | easy | TODO |
| RB-V1.40-5 + SEC-V1.40-5 | `lookup_by_name` debug-span records full user-supplied name — privacy risk; truncate or use `name_len` | easy | TODO |

### Hardcoded limits / scaling

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| SHL-V1.40-2 | `clamp(1, 100)` and `clamp(1, 10)` literals duplicated 13+ times — `cli::limits` exists for this; add named constants | easy | TODO |
| SHL-V1.40-4 | `STALENESS_CHECK_INTERVAL = 100ms` hardcoded — add `CQS_BATCH_STALENESS_CHECK_MS` | easy | TODO |
| SHL-V1.40-5 | `AUDIT_STATE_RELOAD_INTERVAL = 30s` and `CONFIG_RELOAD_INTERVAL = 5min` hardcoded — add env knobs | easy | TODO |
| SHL-V1.40-7 | `KEEP_BACKUPS = 3` hardcoded — add `CQS_MIGRATE_KEEP_BACKUPS` | easy | TODO |
| SHL-V1.40-8 | `LOAD_SPARSE_CHUNK_ID_BATCH = 1000` hardcoded — add env override | easy | TODO |
| SHL-V1.40-10 | `chunks_paged(after_rowid, limit)` accepts unbounded limit — add module ceiling | easy | TODO |

### Tests / Platform

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| TC-ADV-V1.40-2 + EXT-V1.40-5 | `classify_chunk_type` test pin incomplete for 11/24 ChunkType variants — iterate `ChunkType::ALL` | easy | TODO |
| TC-ADV-V1.40-3 | `lookup_by_name` SQL wildcard / SQL injection / very-long-name inputs untested | easy | TODO |
| TC-ADV-V1.40-7 | `Posture::current` / `OutputFormat::current` whitespace + control-char untested (lands with Cluster B) | easy | TODO |
| TC-ADV-V1.40-8 | `emit_json_error` adversarial input (NUL/long/special chars) untested | easy | TODO |
| TC-ADV-V1.40-10 | `current_worktree_name` Unicode / shell-special / very-long inputs untested | easy | TODO |
| TC-HAP-V1.40-5 | `_meta.worktree_name` and `_meta.worktree_stale` envelope fields zero emission tests | easy | TODO |
| TC-HAP-V1.40-6 | `v3_test.v2.json` schema-coverage test missing (only `v3_dev.v2.json` covered) | easy | TODO |
| PB-V1.40-2 | `is_under_wsl_automount` and `is_wsl_drvfs_path` apply different shape validation around shared root | easy | TODO |
| PB-V1.40-4 | `worktree_name` doesn't trim trailing slash — picks up `worktrees` instead of `feature-x` | easy | TODO |
| PB-V1.40-6 | `is_wsl_drvfs_path` UNC arm unguarded for non-WSL hosts (`#[cfg(windows)]` gate) | easy | TODO |
| PB-V1.40-10 | `worktree_name` doesn't `dunce::canonicalize` before `file_name()` — Windows `\\?\` verbatim prefix in JSON | easy | TODO |

### Hot-path nits

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| RM-V1.40-2 | `dispatch_via_view` reconstructs `tokens: Vec<String>` from already-parsed args — second full clone per dispatch | easy | TODO |
| RM-V1.40-3 | `current_worktree_name` clones cached String per envelope emit — `Box<str>` + leak | easy | TODO |
| RM-V1.40-5 | `dispatch_test_map` clones every test chunk per cross-project request — `Vec<&ChunkSummary>` | easy | TODO |
| PERF-V1.40-6 | `upsert_chunks_unembedded_batch` clones every chunk + zero-vec — refactor `batch_insert_chunks` to take slices | easy | TODO |
| PERF-V1.40-9 | Telemetry write does ~5 syscalls per CLI command — daemon-path file-handle caching | medium | TODO |

---

## P4 — defer / track on umbrella issue (8 entries)

| ID(s) | Title | Effort | Status |
|---|---|---|---|
| API-V1.40-7 | BatchProvider trait method names use 6 different verbs — consolidate around `verb_noun` | medium | track on existing #1459 |
| EXT-V1.40-3 | `LanguageDef` no `macro_invocation_suffix` field — Tier 2b extension to non-Rust requires hardcoded language branches | easy | track on #1573 |
| EXT-V1.40-4 + DS-V1.40-7 sibling | Sentiment values `f32` schema accepts arbitrary; CLAUDE.md mandates 5 discrete values — `Sentiment` enum (DS angle filed P1; this is the EXT design refactor) | medium | track on #1463 |
| EXT-V1.40-6 | `ScoringConfig::current` 3-place hand-resolves 9 knobs by string name — macro-derive | medium | track on #1463 |
| EXT-V1.40-7 | Tree-sitter call-query patterns hand-edited per-language — no cross-language pattern library | hard | track on #1573 / new arch issue |
| EXT-V1.40-8 + EXT-V1.40-9 | `cqs notes add --kind` free-string; `is_test_chunk` cross-language detection in lib.rs — promote to `LanguageDef` | medium | track on #1463 |
| PB-V1.40-5 | `daemon_socket_path` no `dunce::canonicalize` — case-skew Windows produces different sockets | medium | track on #1512 |
| PB-V1.40-8 | `daemon_socket_path` `libc::getuid()` Windows-port cliff — doc-only or factor helper | easy | track on #1512 |
| PERF-V1.40-10 | Tier 2a `field_initializer` query inflates `function_calls` table — filter at extraction time | medium | track on #1573 |
| SHL-V1.40-3 | `candidate_count_for` 500 floor corpus-blind — log-scaled formula | medium | track for next perf cycle |
| SHL-V1.40-6 | BM25 column weights hardcoded — promote to `[search.bm25]` slot config | medium | track on #1453 successor |
| SHL-V1.40-9 | `MAX_CHUNKS_SANITY = 1 << 28` hardcoded — env override (low priority defensive) | easy | track on #1463 |

---

## Consolidation map (raw → triaged)

| Triage entry | Raw findings consolidated |
|--------------|---------------------------|
| `filter_invoked_macros` LIKE escape (Cluster A) | EH-V1.40-1, AC-V1.40-8 |
| `filter_invoked_macros` Rust-only `!` (Cluster A) | AC-V1.40-1 |
| `filter_invoked_macros` self-match (Cluster A) | AC-V1.40-2 |
| `filter_invoked_macros` case-insensitive (Cluster A) | PB-V1.40-1 |
| `filter_invoked_macros` N+1 SQL + no transaction (Cluster A2) | CQ-V1.40-10, RB-V1.40-4, EH-V1.40-10, DS-V1.40-6, PERF-V1.40-1 |
| `filter_invoked_macros` zero tests (Cluster A) | TC-ADV-V1.40-5, TC-HAP-V1.40-2 |
| Posture/OutputFormat env-var hygiene (Cluster B) | EH-V1.40-2, API-V1.40-8, OB-V1.40-3, PB-V1.40-3, SEC-V1.40-6, DS-V1.40-5 |
| Posture/SPLADE/CAGRA env-thrash perf (Cluster B sibling) | CQ-V1.40-7, RM-V1.40-8, PERF-V1.40-3, PERF-V1.40-4 |
| Posture-aware plumbing dead (Cluster B follow-up) | CQ-V1.40-5, CQ-V1.40-6 |
| Posture/OutputFormat env-var tests (Cluster B) | TC-ADV-V1.40-7, TC-HAP-V1.40-9 (compose contract) |
| `meta_json_fragment_for_posture` triple-work (Cluster B) | PERF-V1.40-8 |
| `detect_kind_for_store` dead in production (Cluster C) | CQ-V1.40-1, CQ-V1.40-2, API-V1.40-4, RM-V1.40-1, TC-HAP-V1.40-7 |
| `chunks_to_definitions` + per-(command × kind) note duplication (Cluster C) | CQ-V1.40-3, EXT-V1.40-2, EXT-V1.40-10, TC-HAP-V1.40-10 |
| CLI vs daemon redirect-note drift (Cluster C) | CQ-V1.40-4 |
| `cmd_impact_const_fallback` duplicate (Cluster C) | CQ-V1.40-9 |
| `*_kind_fallback` divergent signatures (Cluster C) | API-V1.40-1 |
| Kind enum mixes routing + resolution (Cluster C) | API-V1.40-2 |
| `_ => {}` fallthrough (Cluster C) | EXT-V1.40-1 |
| Graph kind-fallback test refactor (Cluster C) | TC-HAP-V1.40-4 |
| Missing `chunks.name` index + N+1 SQL string alloc (Cluster D) | RB-V1.40-2, PERF-V1.40-2, RM-V1.40-4 |
| Daemon `BatchContext` cache invalidation (Cluster D) | DS-V1.40-1 |
| Kind-detect + real-query no shared tx (Cluster D) | DS-V1.40-8, DS-V1.40-10 |
| `lookup_by_name` ORDER BY alphabetical (Cluster D) | AC-V1.40-9 |
| `lookup_by_name` no LIMIT (Cluster D) | SHL-V1.40-1 |
| Lying docs sweep (Cluster E) | DOC-V1.40-1, DOC-V1.40-2, DOC-V1.40-3, DOC-V1.40-4, DOC-V1.40-5, DOC-V1.40-6, DOC-V1.40-7, OB-V1.40-4 |
| `redact_error` cross-process determinism doc claim (Cluster E) | EH-V1.40-6 |
| V2Bare default + emit_json + compose contract test gap (Cluster F) | TC-ADV-V1.40-1, TC-HAP-V1.40-1, TC-HAP-V1.40-8, TC-HAP-V1.40-9 |
| V2Bare drops worktree_stale (Cluster F/H) | SEC-V1.40-1 |
| Phase 1 dispatch tracing (Cluster G) | OB-V1.40-1 |
| Phase 1 telemetry category (Cluster G) | OB-V1.40-2 |
| Daemon-path test parity (Cluster G) | TC-ADV-V1.40-4, TC-HAP-V1.40-3 |
| CLI integration tests for kind-fallback (Cluster G) | TC-ADV-V1.40-9 |
| `redact_query_str` privacy (Cluster G sibling) | OB-V1.40-10, SEC-V1.40-3, TC-ADV-V1.40-6 |
| Kind-fallback content-cap DoS (Cluster H) | EH-V1.40-9, SEC-V1.40-4 |
| Daemon `read_line` pre-allocation (Cluster H) | SEC-V1.40-7 |
| `enumerate_files` no caps (Cluster H) | SEC-V1.40-8 |
| `build_test_map` panic on usize::MAX (Cluster H) | RB-V1.40-1 |
| `chunks_paged` unbounded (Cluster H) | SHL-V1.40-10 |
| `try_kind_fallback` SQL error fatal + Windows sharing-violation | EH-V1.40-8, PB-V1.40-7 |
| `lookup_by_name` debug-span privacy | RB-V1.40-5, SEC-V1.40-5 |
| `bfs_shortest_path` `String::new()` sentinel | AC-V1.40-3 |
| Tier 2a `field_initializer` over-capture | AC-V1.40-4 |
| `bfs_expand` depth update score-gated | AC-V1.40-5 |
| `Kind::Other` Macro/Impl/Service falls through dispatchers | AC-V1.40-6 |
| `classify_hits` Multiple masks ambiguity | AC-V1.40-7 |
| `cap_k_to_backend` Some(0) boundary | AC-V1.40-10 |
| Trace `target` kind not validated | EH-V1.40-3 |
| `dispatch_deps` Type-forward misroute | EH-V1.40-4 |
| `meta_json_fragment_for_posture` defensive inconsistency | EH-V1.40-5 |
| `lookup_by_name` empty-string shortcut | EH-V1.40-7 |
| `Store::lookup_by_name` naming convention | API-V1.40-3 |
| `OutputFormat` type-name collision | API-V1.40-5 |
| `emit_json_error_with_data_and_posture` ignores posture | API-V1.40-6 |
| `CQS_DEFERRED_FLUSH_INTERVAL` units | API-V1.40-9 |
| `--format` vs `--json` flag shadow | API-V1.40-10 |
| `meta_value_for_envelope` duplicate fallback | CQ-V1.40-8 |
| `classify_hits` `.expect()` | RB-V1.40-3 |
| `cmd_telemetry_reset` non-atomic | DS-V1.40-2 |
| `restore_from_backup` non-atomic triplet | DS-V1.40-3 |
| `regenerate_v3_test.py` non-atomic write | DS-V1.40-4 |
| Sentiment column accepts arbitrary f32 (DS angle) | DS-V1.40-7 |
| v26→v27 migration zero-vec gap | DS-V1.40-9 |
| `redact_userinfo` over-redacts URL with `@` in path | SEC-V1.40-2 |
| WSL DrvFS WAL mode | PB-V1.40-9 |
| `is_under_wsl_automount` shape mismatch | PB-V1.40-2 |
| `worktree_name` trailing slash | PB-V1.40-4 |
| `is_wsl_drvfs_path` UNC arm unguarded | PB-V1.40-6 |
| `worktree_name` Windows verbatim prefix | PB-V1.40-10 |
| `daemon_socket_path` case-skew (Windows) | PB-V1.40-5 (defer to #1512) |
| `daemon_socket_path` `getuid` Windows cliff | PB-V1.40-8 (defer to #1512) |
| `clamp(1, 100)` literals duplicated 13× | SHL-V1.40-2 |
| Daemon staleness/audit/config reload knobs | SHL-V1.40-4, SHL-V1.40-5 |
| Migration backup retention | SHL-V1.40-7 |
| SPLADE load batch | SHL-V1.40-8 |
| CAGRA chunk count sanity | SHL-V1.40-9 |
| Candidate floor corpus-blind | SHL-V1.40-3 |
| BM25 column weights hardcoded | SHL-V1.40-6 |
| `LanguageDef::macro_invocation_suffix` (EXT) | EXT-V1.40-3 |
| `Sentiment` enum (EXT angle) | EXT-V1.40-4 |
| `classify_chunk_type` test iteration | TC-ADV-V1.40-2, EXT-V1.40-5 |
| `ScoringConfig` macro-derive | EXT-V1.40-6 |
| Tree-sitter pattern library | EXT-V1.40-7 |
| `--kind` enum (EXT) + `is_test_chunk` (EXT) | EXT-V1.40-8, EXT-V1.40-9 |
| `lookup_by_name` adversarial input untested | TC-ADV-V1.40-3 |
| `emit_json_error` adversarial input untested | TC-ADV-V1.40-8 |
| `current_worktree_name` adversarial input untested | TC-ADV-V1.40-10 |
| Worktree envelope fields untested | TC-HAP-V1.40-5 |
| `v3_test.v2.json` schema test gap | TC-HAP-V1.40-6 |
| `dispatch_via_view` token clone | RM-V1.40-2 |
| `current_worktree_name` per-emit clone | RM-V1.40-3 |
| `dispatch_test_map` cross-project clones | RM-V1.40-5 |
| `CrossProjectContext` rebuild + reopen | RM-V1.40-6, RM-V1.40-7 |
| Daemon socket triple alloc | RM-V1.40-9 |
| `format_envelope_to_string` double serialization | PERF-V1.40-5, RM-V1.40-10 |
| `upsert_chunks_unembedded_batch` clones | PERF-V1.40-6 |
| `build_test_map` iteration order | PERF-V1.40-7 |
| Telemetry syscall count | PERF-V1.40-9 |
| Tier 2a `field_initializer` table inflation | PERF-V1.40-10 |
| Daemon accept/cap observability | OB-V1.40-1, OB-V1.40-2, OB-V1.40-5, OB-V1.40-6, OB-V1.40-7, OB-V1.40-8, OB-V1.40-9 |
| BatchProvider trait method names | API-V1.40-7 |

---

## Recommended fix order (top-10 PR list)

Cluster-driven, single PR per cluster. Numbers in `[N items]` indicate raw-finding count covered.

1. **PR-A: Tier 2b macro-filter correctness sweep** [11 items: EH-V1.40-1, AC-V1.40-8, AC-V1.40-1, AC-V1.40-2, PB-V1.40-1, TC-ADV-V1.40-5, TC-HAP-V1.40-2, plus N+1 fix CQ-V1.40-10/RB-V1.40-4/EH-V1.40-10/DS-V1.40-6/PERF-V1.40-1] — escape LIKE wildcards, add `ESCAPE '\\'`, branch on language for `!` suffix vs `(`, `id != ?2` for self-match, `PRAGMA case_sensitive_like = 1` (or GLOB), batch into single scan in single read tx. **+ 4 regression tests.** This unblocks the rest of cqs-dead correctness.
2. **PR-B: Posture/OutputFormat env-var hygiene + plumbing** [12 items: EH-V1.40-2, API-V1.40-8, OB-V1.40-3, PB-V1.40-3, SEC-V1.40-6, DS-V1.40-5, CQ-V1.40-7, RM-V1.40-8, PERF-V1.40-3, PERF-V1.40-4, PERF-V1.40-8, TC-ADV-V1.40-7, TC-HAP-V1.40-9] — `OnceLock<Posture>`, accept aliases, log first read, plumb once per request to leaf serializers, retire `_with_posture` `#[allow(dead_code)]`. Tests pin compose contract.
3. **PR-C: Polymorphic-routing dedup + helper module** [11 items: CQ-V1.40-1, CQ-V1.40-2, CQ-V1.40-3, CQ-V1.40-4, CQ-V1.40-9, API-V1.40-1, API-V1.40-2, API-V1.40-3, EXT-V1.40-1, EXT-V1.40-2, EXT-V1.40-10, RM-V1.40-1, TC-HAP-V1.40-4, TC-HAP-V1.40-7, TC-HAP-V1.40-10] — `kind_fallback` module with `chunks_to_definitions`, per-command notes table, single dispatcher signature; rename `lookup_by_name` → `get_chunks_by_name`; split `Kind` + `KindResolution`; exhaustive matches.
4. **PR-D: Polymorphic-routing data safety + perf (schema migration v28)** [7 items: RB-V1.40-2, PERF-V1.40-2, RM-V1.40-4, DS-V1.40-1, DS-V1.40-8, DS-V1.40-10, AC-V1.40-9, SHL-V1.40-1] — `CREATE INDEX idx_chunks_name`, single read tx in dispatchers, `PRAGMA data_version` for daemon cache invalidation, routing-priority ORDER BY, `lookup_by_name` LIMIT.
5. **PR-E: Lying docs sweep** [8 items: DOC-V1.40-1, DOC-V1.40-2, DOC-V1.40-3, DOC-V1.40-4, DOC-V1.40-5, DOC-V1.40-6, DOC-V1.40-7, OB-V1.40-4, EH-V1.40-6] — mostly mechanical edits to ROADMAP/CONTRIBUTING/SECURITY/Cargo.toml/README/CHANGELOG. Adopt the pre-commit-hook proposal from DOC-V1.38-5 to prevent recurrence.
6. **PR-F: Correctness P1 sweep (BFS sentinel + Tier 2a over-capture + bfs_expand depth + RB-V1.40-1 + RB-V1.40-3)** [5 items: AC-V1.40-3, AC-V1.40-4, AC-V1.40-5, RB-V1.40-1 , RB-V1.40-3] — graph correctness, panic surface, `.expect()` removal, depth field accuracy in JSON output.
7. **PR-G: Resource caps + DoS hardening** [6 items: EH-V1.40-9, SEC-V1.40-4, SEC-V1.40-1, SEC-V1.40-7, SEC-V1.40-8, SHL-V1.40-10] — kind-fallback content cap; resurrect `_meta.worktree_stale` under V2Bare default; daemon `read_line` incremental growth; `enumerate_files` depth/file-count cap; `chunks_paged` ceiling.
8. **PR-H: Atomic write + data safety bug sweep** [5 items: DS-V1.40-2, DS-V1.40-3, DS-V1.40-4, DS-V1.40-7, DS-V1.40-9, SEC-V1.40-2] — telemetry reset atomic, backup restore atomic across triplet, regen-fixture atomic, sentiment CHECK constraint, v27 migration zero-vec, `redact_userinfo` path-aware.
9. **PR-I: V2Bare end-to-end test backfill** [5 items: TC-ADV-V1.40-1, TC-HAP-V1.40-1, TC-HAP-V1.40-8, TC-ADV-V1.40-4, TC-ADV-V1.40-9, TC-HAP-V1.40-3] — end-to-end binary tests for V2Bare default, daemon-path kind-fallback parity tests, CLI integration tests for Const/Type/Module/Ambiguous.
10. **PR-J: P3 grab-bag (style, knobs, observability nits)** — single PR per category for the P3 batch above; SHL-V1.40-2/-4/-5/-7/-8/-10 (named constants + env knobs), OB-V1.40-1/-2/-5/-6/-7/-8/-9/-10 (tracing spans + telemetry buckets), small style fixes.

After these 10 PRs: P4 tracking issues filed against #1463 / #1459 / #1512 / #1573 for the deferred umbrella items.

---

## Notes

- **No findings already shipped in #1601-#1625 appeared in this audit cycle.** Auditors correctly skipped already-triaged v1.38.0 carry-overs (per the explicit skip notes in OB and TC-ADV sections).
- **Cross-check vs open issues:** P4 carry-overs map cleanly to existing umbrellas — `EXT-V1.40-3/4/6/7/8/9` → #1463 (P4 umbrella) / #1573 (Tier 3 dead-code follow-up), `API-V1.40-7` → #1459 (API design umbrella), `PB-V1.40-5/8` → #1512 (Windows daemon).
- **No external users (per CLAUDE.md):** rename and breaking-shape changes (e.g., `lookup_by_name` → `get_chunks_by_name`, `posture::OutputFormat` → `EnvelopeShape`, `Kind` split, `Sentiment` enum) are free. No deprecation cycle, no migration shim.
- **Phase 1 polymorphic-routing concentration:** 50+ findings touch the same surface — Cluster C (refactor) + Cluster D (data safety) + Cluster G (observability) + Cluster F (test parity) all attack the same 6-command × 2-surface × 5-kind matrix. Land them in order C → D → G → F to avoid merge churn.
- **Recurrence pattern:** DOC-V1.40-4 (CHANGELOG `[Unreleased]` empty) is the third audit cycle finding this — hook the pre-commit fix proposed in DOC-V1.38-5 alongside Cluster E.
