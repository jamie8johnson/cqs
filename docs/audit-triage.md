# Audit Triage (post-v1.38.0)

Audit date: 2026-05-06
Source: `docs/audit-findings.md` (154 findings across 16 categories, batches 1+2)
Calibration: matches `docs/audit-triage-v1.36.2-final.md` priority matrix.

> **Cycle status (2026-05-07):** ~64 findings closed across 33+ cluster PRs (#1514–#1570). The per-row `Status` column in this document is a **snapshot at audit time** and was updated periodically as cluster PRs landed. **Canonical closure list lives in the [#1463 umbrella issue comment](https://github.com/jamie8johnson/cqs/issues/1463#issuecomment-4395768972).** Truly remaining items: API-V1.38-6 (clap conflict — bigger than the audit estimated), DS-V1.38-4 deeper hazard (bundle-rename atomic refactor for HNSW save), PL-V1.38-2 (Windows runner), plus 12 P4 carry-overs on tracking issues.

## Summary by Priority

| Priority | Count | Disposition |
|----------|-------|-------------|
| P1 | 50 | fix this PR — easy + high impact, lying docs, data corruption, security regressions |
| P2 | 37 | fix this batch — medium effort + high impact, regression-test backfill, hot-path perf |
| P3 | 55 | fix if time — easy + low impact (minor logging, doc tweaks, single-test additions) |
| P4 | 12 | defer — hard / low-impact / tracked under #1463/#1459/#1512 umbrella issues |
| **Total** | **154** | |

Excluded from P1/P2 classification (already on a deferral umbrella): findings whose fix lives entirely in #1463 (P4 design umbrella), #1459 (API design umbrella), #1512 (Windows daemon).

---

## Cross-cutting clusters

These clusters shape the recommended PR order at the bottom of the file. A single PR per cluster collapses 3+ findings into one fix-pattern review.

### Cluster A — `needs_embedding` wiring gaps (data safety, 4 findings)
PR #1452/#1497 added the `needs_embedding=1` skip-first-pass-embed mechanism but the read paths were not all updated and `embedding_base` is permanently NULL post-enrichment.
- DS-V1.38-1 (`embedding_batches` doesn't filter `needs_embedding=0` → CAGRA/UMAP/neighbors load zero-vec sentinels)
- DS-V1.38-2 (`enrichment_pass` clears `needs_embedding=0` but never repopulates `embedding_base`)
- DS-V1.38-3 (`BatchContext::check_index_staleness` watches legacy `cqs_dir` not active slot)
- DS-V1.38-8 (v26→v27 migration leaves `embedding_base IS NULL` rows un-flagged)
- TC-HAP-V1.38-3 (regression test for the `enrichment_pass` carve-out)

### Cluster B — Lying docs (10 findings, all P1 by team rule)
Per CLAUDE.md memory: docs that promise behavior the code doesn't deliver are correctness bugs.
- DOC-V1.38-1 (README/lib.rs/SECURITY.md claim `--improve-docs` writes to source; default is patch-mode since v1.30.1)
- DOC-V1.38-2 (CONTRIBUTING.md describes deleted `src/cli/registry.rs` + `for_each_command!`)
- DOC-V1.38-3 (CONTRIBUTING.md says schema v26; current is v27)
- DOC-V1.38-4 (SECURITY.md cites stale `lib.rs:813`; actual `:771`)
- DOC-V1.38-5 (CHANGELOG `[Unreleased]` empty after ~14 PRs)
- DOC-V1.38-6 (README perf table pinned to BGE-large + v1.27.0; default is EmbeddingGemma)
- DOC-V1.38-7 (SECURITY.md schema enumeration missing v27)
- DOC-V1.38-8 (Cargo.toml R@K only retuned for gemma; README rows pre-retune)
- DOC-V1.38-9 (chunk-type count drift)
- DOC-V1.38-10 (MEMORY.md drift — not a repo doc but ships into every session)
- PL-V1.38-3 (`is_suspicious_cache_path` doc promises Windows checks impl skips)

### Cluster C — `stored_model_name` lossy callers (error handling, 4 findings)
PR #1504 migrated `cmd_model_swap` to `try_stored_model_name`; remaining 4 callers still swallow SQL errors → silent dim-drift in destructive paths.
- EH-V1.38-1 (`watch/mod.rs:1037` — daemon main loop)
- EH-V1.38-2 (`watch/rebuild.rs:187` — daemon thread bring-up)
- EH-V1.38-3 (`model.rs:240` `cmd_model_show` reports `<unrecorded>` for both fresh + corrupt cases)
- EH-V1.38-4 (`slot.rs:206` slot listing silently empties model column)

### Cluster D — TC-ADV regression-test backfill (5 findings, all P2)
Fixes shipped in code, but a revert wouldn't fail any test → invisible regression risk.
- TC-ADV-V1.38-1 (`validate_repo_id "..": ` SEC-V1.36-7 fix)
- TC-ADV-V1.38-2 (`NoteBoostIndex::boost` NaN/Inf clamp — P1-36 fix)
- TC-ADV-V1.38-4 (`lookup_main_cqs_dir` `is_dir()` — P1-43 fix)
- TC-ADV-V1.38-5 (`write_active_slot` race — DS-V1.33-2 fix; needs multi-thread harness)
- TC-ADV-V1.38-3 (`upsert_sparse_vectors` write-side NaN/Inf — pin contract)
- TC-ADV-V1.38-10 (`embedding_to_bytes` write-side NaN/Inf — pin contract)

### Cluster E — Hot-path env/alloc thrash (performance, 5 findings)
One-line `OnceLock` wrappers gate allocations on every search query / encode call.
- PF-V1.38-1 (`is_test_chunk` rebuilds 54-language pattern lists per candidate; ~30k allocs/query)
- PF-V1.38-3 (`resolve_splade_alpha` `format!` + 2 env reads per query)
- PF-V1.38-4 (SPLADE `splade_max_chars()` + `default_threshold()` env-thrash per encode)
- PF-V1.38-7 (`extract_call_snippet_from_cache` `lines().collect()` allocates full Vec to take 3)
- PF-V1.38-8 (watch reindex stat() twice per file)

### Cluster F — Algorithm correctness regressions (5 findings)
PR #1502 / earlier total_cmp work missed sibling sites; SPLADE BoundedScoreHeap tiebreak break.
- AC-V1.38-1 (`BoundedScoreHeap::would_accept` violates id-tiebreak invariant — silently drops smaller-id boundary candidates)
- AC-V1.38-2 (`mmr_rerank` raw f32 `==` instead of `total_cmp`)
- AC-V1.38-3 (`bfs_expand` seed sort `partial_cmp.unwrap_or(Equal)`)
- AC-V1.38-5 (`resolve_splade_alpha` global-env arm silently drops parse errors)
- AC-V1.38-9 (HNSW `ef_search` `k * 2` overflow → `saturating_mul`)
- AC-V1.38-6 (`apply_parent_boost` ULP overshoot of `parent_boost_cap`)

### Cluster G — Unbounded subprocess output (resource mgmt, 2 findings)
RM-V1.36-2/-5 fixed the same pattern in `pdf_to_markdown`; new sites missed.
- RM-V1.38-4 (UMAP subprocess `wait_with_output` unbounded)
- RM-V1.38-5 (`export_model.rs` ONNX export unbounded)

### Cluster H — Unbounded `read_to_string` / panic-on-malformed-input (robustness, 4 findings)
Sibling-class of v1.33 RB-V1.33-2 fix; new sites keep landing.
- RB-V1.38-1 (`worktree::resolve_main_project_dir` `commondir` unbounded read)
- RB-V1.38-2 (`cli/watch/reindex.rs:626` daemon-killing panic on chunk-index mismatch)
- RB-V1.38-3 (SPLADE load-path 9× `.try_into().unwrap()` on body slices)
- RB-V1.38-5 (`umap.rs:122` `Vec::with_capacity` overflow on pathological input)

### Cluster I — Security easy wins (5 findings)
All easy, all SEC-V1.36-2 / SEC-V1.30.1-6 sibling regressions.
- SEC-V1.38-1 (`llm_config.api_base` userinfo leaked at 5 log sites)
- SEC-V1.38-2 (`parsed_anthropic_message` debug log echoes prompt content)
- SEC-V1.38-3 (`write_proposed_patch` non-atomic write — SIGINT corrupts patch)
- SEC-V1.38-4 (`cmd_ref_update` skips SEC-V1.30.1-6 symlink-redirect warning)
- SEC-V1.38-8 (`tracing::debug!(?config, ...)` dumps full Config including userinfo)

### Cluster J — Idle-tick eviction wiring (resource mgmt, 1 finding but multi-arm)
Daemon caps depend on file-event tick that never fires on query-only daemons.
- RM-V1.38-1 (EmbeddingCache eviction never fires on query-only daemon)
- RM-V1.38-7 (`last_indexed_mtime` prune size-gated, never trims)

---

## Recommended fix order (top-10 PR list)

Cluster-driven, single PR per cluster where possible. Numbers in `[N items]` indicate finding count covered.

1. **PR-A: needs_embedding wiring + enrichment base repopulation** [5 items: DS-V1.38-1/2/3/8 + TC-HAP-V1.38-3] — corrects #1452 silent search-quality regression on `--llm-summaries` workflows. Single migration + 5 SQL filter changes.
2. **PR-B: lying docs sweep** [11 items: all DOC-V1.38-* + PL-V1.38-3] — mostly mechanical edits to README/CONTRIBUTING/SECURITY. Includes CHANGELOG `[Unreleased]` backfill.
3. **PR-C: stored_model_name lossy migration completion** [4 items: EH-V1.38-1/2/3/4] — finishes the PR #1504 migration. Same pattern, 4 call sites.
4. **PR-F: algorithm correctness sweep** [6 items: AC-V1.38-1/2/3/5/6/9] — total_cmp adoption + saturating arithmetic + tiebreak fix. Each is one-line.
5. **PR-I: security easy wins** [5 items: SEC-V1.38-1/2/3/4/8] — userinfo redactor at type level, atomic patch write, symlink warn lift to shared helper.
6. **PR-E: hot-path perf (OnceLock wrappers + dedup syscalls)** [5 items: PF-V1.38-1/3/4/7/8] — gates allocations on every search. Highest leverage on agent-heavy daemon workloads.
7. **PR-H: unbounded input + panic surface (worktree, watch, splade, umap)** [4 items: RB-V1.38-1/2/3/5] — daemon-killing panics + OOM vectors. RB-V1.38-2 alone is the daemon's hot-path crash arm.
8. **PR-D: TC-ADV regression-test backfill** [6 items: TC-ADV-V1.38-1/2/3/4/5/10] — pure test additions. Revert-resistant for shipped fixes; pins NaN/Inf write-side contracts. TC-ADV-V1.38-5 has medium effort (multi-thread harness).
9. **PR-G: subprocess output caps (UMAP + export_model)** [2 items: RM-V1.38-4/5] — sibling fixes to RM-V1.36-2/-5. Mirror that PR's `spawn` + `take(MAX)` shape.
10. **PR-J: idle-tick eviction + remaining EH/OB/RM cleanup** [grab-bag: RM-V1.38-1/-7, EH-V1.38-5/-6/-9/-10, OB-V1.38-1/-2/-3] — periodic-tick cache evict, `.ok()` → `match warn` migrations, missing entry spans on PR #1505/#1506 handlers.

After these 10 PRs: P3 cleanup batch (single PR per category — doc tweaks, minor logging, single-test additions) and P4 tracking issues for the deferred umbrella items.

---

## P1 — fix this PR (50 items)

### Code Quality
| ID | Title | Difficulty | Status |
|---|---|---|---|
| CQ-V1.38-1 | `parse_env_duration_secs` orphan (`#[allow(dead_code)]` with no callers) | easy | pending |
| CQ-V1.38-2 | `open_project_store()` zero-arg dead variant — invites slot-bypass regression | easy | pending |
| CQ-V1.38-3 | `RISK_THRESHOLD_HIGH/MEDIUM` re-exports dead | easy | pending |
| CQ-V1.38-4 | `parser::MAX_FILE_SIZE` / `MAX_CHUNK_BYTES` re-exports dead, `pub(crate)` contradicts doc | easy | pending |

### Documentation
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DOC-V1.38-1 | `--improve-docs` write-back claim contradicts patch-mode default (README + lib.rs + SECURITY.md) | easy | pending |
| DOC-V1.38-2 | CONTRIBUTING.md describes deleted `src/cli/registry.rs` + `for_each_command!` | easy | pending |
| DOC-V1.38-3 | CONTRIBUTING.md says Schema v26; actual v27 | easy | pending |
| DOC-V1.38-4 | SECURITY.md cites `src/lib.rs:813` for `enumerate_files`; actual `:771` | easy | pending |
| DOC-V1.38-5 | CHANGELOG `[Unreleased]` empty after ~14 PRs since v1.38.0 (incl. schema v27 bump) | easy | pending |
| DOC-V1.38-7 | SECURITY.md schema list `v22..v26`; v27 not enumerated | easy | pending |
| DOC-V1.38-9 | README "How It Works" claims "19 other chunk types"; actual ~30+ | easy | pending |
| DOC-V1.38-10 | MEMORY.md drift — schema v22, version 1.29.1, BGE-large default (ships into every session) | easy | pending |

### API Design
| ID | Title | Difficulty | Status |
|---|---|---|---|
| API-V1.38-4 | `LocalProvider::submit_batch` skips `validate_model` despite trait contract | easy | pending |

### Error Handling
| ID | Title | Difficulty | Status |
|---|---|---|---|
| EH-V1.38-1 | `watch/mod.rs:1037` lossy `stored_model_name()` → corrupted incremental reindex | medium | pending |
| EH-V1.38-2 | `watch/rebuild.rs:187` same lossy pattern in `resolve_index_aware_model_for_watch` | medium | pending |
| EH-V1.38-3 | `cmd_model_show` reports `<unrecorded>` identically for fresh-DB + corrupt-DB | easy | pending |
| EH-V1.38-4 | `cqs slot list` silently empties model column on metadata read failure | easy | pending |
| EH-V1.38-5 | `cli/dispatch.rs:139-142` SPLADE α resolver still uses `.ok()` (sibling of EH-V1.30.1-3 fix) | easy | pending |

### Test Coverage (adversarial)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| TC-ADV-V1.38-1 | `validate_repo_id` `..` rejection has no regression test (security fix without test) | easy | pending |
| TC-ADV-V1.38-10 | `embedding_to_bytes` write-path NaN/Inf untested — corruption surface | easy | pending |

### Robustness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RB-V1.38-1 | `worktree::resolve_main_project_dir` reads `commondir` unbounded — sibling RB-V1.33-2 | easy | pending |
| RB-V1.38-2 | `cli/watch/reindex.rs:626` panics the daemon on chunk-index mismatch | medium | pending |
| RB-V1.38-3 | `splade/index.rs` load-path 9× `.try_into().unwrap()` on body slices | easy | pending |
| RB-V1.38-5 | `umap.rs:122` `Vec::with_capacity` overflow on pathological input | medium | pending |

### Algorithm Correctness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| AC-V1.38-1 | `BoundedScoreHeap::would_accept` violates id-tiebreak invariant — silently drops smaller-id SPLADE boundary candidates | easy | pending |
| AC-V1.38-2 | `mmr_rerank` raw `f32 ==` instead of `total_cmp` — sibling AC-V1.30.1-7 anti-pattern | easy | pending |
| AC-V1.38-3 | `bfs_expand` seed sort `partial_cmp.unwrap_or(Equal)` — outlier vs codebase total_cmp standard | easy | pending |
| AC-V1.38-5 | `resolve_splade_alpha` global-env arm silently drops parse errors per-cat arm warns about | easy | pending |
| AC-V1.38-9 | HNSW `ef_search` `k * 2` overflow → `saturating_mul` | easy | pending |

### Platform
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PL-V1.38-1 | CAGRA `.cagra` blob world-readable on Unix — SEC-1 contract gap vs HNSW | easy | pending |
| PL-V1.38-3 | `is_suspicious_cache_path` doc promises Windows checks impl skips (lying doc) | easy | pending |
| PL-V1.38-5 | `cqs init` writes `.gitignore` missing 11 of 14 actual `.cqs/` files (incl. `audit-mode.json`, `telemetry*.jsonl`) | easy | pending |

### Security
| ID | Title | Difficulty | Status |
|---|---|---|---|
| SEC-V1.38-1 | `llm_config.api_base` userinfo leaked at 5 log sites | easy | pending |
| SEC-V1.38-2 | `parsed_anthropic_message` debug log echoes prompt content (poisoned-chunk → API key in journald) | easy | pending |
| SEC-V1.38-3 | `write_proposed_patch` non-atomic write — SIGINT mid-write → corrupt patch → silent source damage on `git apply` | easy | pending |
| SEC-V1.38-4 | `cmd_ref_update` skips SEC-V1.30.1-6 symlink-redirect warning carried by `cmd_ref_add` | medium | pending |
| SEC-V1.38-8 | `tracing::debug!(?config, ...)` dumps full Config including userinfo (every command, not just LLM) | easy | pending |

### Data Safety
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DS-V1.38-1 | `embedding_batches` doesn't filter `needs_embedding=0` — CAGRA/UMAP/neighbors load zero-vec sentinels | easy | pending |
| DS-V1.38-2 | `enrichment_pass` clears `needs_embedding=0` but never repopulates `embedding_base` — base HNSW degrades on `--llm-summaries` | medium | pending |
| DS-V1.38-3 | `BatchContext::check_index_staleness` watches legacy `cqs_dir` not active slot — daemon caches go stale silently | easy | pending |
| DS-V1.38-7 | `write_active_slot` doesn't hold `slots.lock` itself — relies on caller contract; future caller drops it | easy | pending |

### Performance
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PF-V1.38-1 | `is_test_chunk` rebuilds 54-language pattern lists per candidate (~30k allocs/query, hot path) | medium | pending |
| PF-V1.38-2 | SPLADE `id_map: Vec<String>` — sibling fix to PR #1502 (HNSW + CAGRA already migrated) | easy | pending |
| PF-V1.38-3 | `resolve_splade_alpha` `format!` + 2 env reads per search query | easy | pending |
| PF-V1.38-4 | SPLADE `splade_max_chars()` / `default_threshold()` env-thrash per encode (18k env reads per reindex) | easy | pending |
| PF-V1.38-7 | `extract_call_snippet_from_cache` `lines().collect()` allocates full Vec to pick 3 | easy | pending |
| PF-V1.38-8 | Watch reindex `abs_path` stat'd twice per file (mtime + size) | easy | pending |

### Resource Management
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RM-V1.38-1 | EmbeddingCache eviction never fires on query-only daemon (caps become advisory) | easy | pending |
| RM-V1.38-4 | UMAP subprocess `wait_with_output` unbounded — sibling RM-V1.36-2 | easy | pending |
| RM-V1.38-5 | `export_model.rs` ONNX export unbounded — sibling RM-V1.36-5 | easy | pending |

---

## P2 — fix this batch (37 items)

### Code Quality
| ID | Title | Difficulty | Status |
|---|---|---|---|
| CQ-V1.38-5 | `ScoringConfig::DEFAULT` and `SCORING_KNOBS` carry duplicate hardcoded defaults with no sync check | medium | pending |
| CQ-V1.38-6 | `cqs dead` / HNSW chunk-id mappings stale — costs every audit ~30+ ghost-investigations | medium | pending |

### Documentation
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DOC-V1.38-6 | README "Performance" pinned to BGE-large + v1.27.0; default is EmbeddingGemma since v1.35.0 | medium | pending |
| DOC-V1.38-8 | Cargo.toml gemma R@K vs README rows pre-retune (queued sweep stalled) | easy | pending |

### API Design
| ID | Title | Difficulty | Status |
|---|---|---|---|
| API-V1.38-1 | `ModelCommand` / `HookCommand` use inline `json: bool` instead of shared `TextJsonArgs` | easy | pending |
| API-V1.38-3 | `BatchProvider` trait lacks `Send + Sync` bounds while sibling traits require them | easy | pending |
| API-V1.38-6 | Top-level `Cli` search flags silently ignored when subcommand given (no `global=true`) | medium | pending |

### Error Handling
| ID | Title | Difficulty | Status |
|---|---|---|---|
| EH-V1.38-7 | `embedder/mod.rs:1325-1334` triple-cascade tensor extract swallows non-dtype errors | medium | pending |
| EH-V1.38-9 | `cli/json_envelope.rs:371` + `cli/batch/mod.rs:2235` discard original `to_string_pretty` error in retry path | easy | pending |
| EH-V1.38-10 | `parse_env_usize` / `parse_env_u64` silently accept malformed values (no warn) | easy | pending |

### Observability
| ID | Title | Difficulty | Status |
|---|---|---|---|
| OB-V1.38-1 | `cmd_ref_update` (PR #1506 LLM/HyDE/doc parity) lacks entry span | easy | pending |
| OB-V1.38-2 | `check_index_model_drift` (PR #1505) bails silently — no `tracing::warn!` accompanies fatal error | easy | pending |
| OB-V1.38-3 | Synonym/classifier overlay install silent — operators can't see if TOML config was applied | easy | pending |
| OB-V1.38-4 | `wait_for_batch` polling loop has no entry span + no closing `elapsed_ms` (10-30 min Anthropic latency invisible) | easy | pending |
| OB-V1.38-5 | CAGRA "ineligible — falling through" at debug — operators can't see GPU-fallback decision | easy | pending |
| OB-V1.38-7 | SPLADE `.bak` rollback restore-failure error doesn't surface to operator console | medium | pending |

### Test Coverage (adversarial)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| TC-ADV-V1.38-2 | `NoteBoostIndex::boost` / `OwnedNoteBoostIndex::boost` NaN/+Inf clamp untested (P1-36 fix) | easy | pending |
| TC-ADV-V1.38-3 | `upsert_sparse_vectors` write-path NaN/Inf untested | easy | pending |
| TC-ADV-V1.38-4 | `lookup_main_cqs_dir` "stray .cqs FILE" path lacks test (P1-43 fix) | easy | pending |
| TC-ADV-V1.38-5 | `write_active_slot` concurrent-writer race fix has no regression test (DS-V1.33-2) | medium | pending |
| TC-ADV-V1.38-6 | `check_index_model_drift` case-insensitive / whitespace edges untested | easy | pending |

### Robustness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RB-V1.38-4 | `cli/commands/infra/doctor.rs:582` `api_base.unwrap()` brittle by-flow-only | easy | pending |
| RB-V1.38-8 | `embedder/models.rs` `.expect("guarded by has_X")` on user-config Option (3 sites) | easy | pending |
| RB-V1.38-9 | `nl/fields.rs:122` `unreachable!()` reachable on FieldStyle additions | easy | pending |
| RB-V1.38-10 | `cli/watch/mod.rs` `handle_opt.take().unwrap()` brittle to refactor (daemon-shutdown panic surface) | easy | pending |

### Scaling
| ID | Title | Difficulty | Status |
|---|---|---|---|
| SHL-V1.38-1 | `MAX_PENDING_REBUILD_DELTA = 5_000` doesn't scale with embedding dim — sibling SHL-V1.36-3/4/5 | easy | pending |
| SHL-V1.38-4 | Daemon socket request line capped at 1 MB while CLI accepts 50 MB (review/impact via daemon broken) | medium | pending |

### Algorithm Correctness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| AC-V1.38-6 | `apply_parent_boost` ULP overshoot of `parent_boost_cap` (one extra `.min(cap)` clamp) | easy | pending |
| AC-V1.38-7 | `from_preset(unknown_name)` silently falls back to default — `cqs index --model typo` invisible if defaults align | medium | pending |

### Platform
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PL-V1.38-2 | SPLADE `splade.index.bin.bak` Windows umask + cross-device fallback ACL leak | easy | pending |
| PL-V1.38-4 | Two divergent WSL-DrvFS detectors disagree on custom `automount.root` | medium | pending |

### Data Safety
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DS-V1.38-4 | HNSW `load_with_dim` existence check before shared lock — concurrent saver renames files out from under reader | medium | ✅ #1570 (easy mitigation; bundle-rename for half-state hazard deferred) |
| DS-V1.38-8 | v26→v27 migration leaves `embedding_base IS NULL` rows un-flagged for re-embed | medium | pending |

### Test Coverage (happy path)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| TC-HAP-V1.38-1 | `cqs project search` filter knobs (#1507) parse-only — no behavioral assertions | medium | ✅ #1562 |
| TC-HAP-V1.38-2 | `cqs ref reindex --llm-summaries` (#1506) parse-only — never runs LLM/HyDE pass | medium | ✅ #1563 |
| TC-HAP-V1.38-3 | `enrichment_pass` itself untested despite #1497 carve-out | medium | deferred (needs embedder load) |
| TC-HAP-V1.38-4 | `cqs index --model X` drift detection (#1505) helper unit-tested but no end-to-end CLI test | easy | ✅ #1560 |

---

## P3 — fix if time (55 items)

### API Design
| ID | Title | Difficulty | Status |
|---|---|---|---|
| API-V1.38-2 | `ProjectCommand::Search` duplicates `query/limit/threshold` instead of flattening `SearchArgs` (also missing `parse_finite_f32`) | medium | pending |
| API-V1.38-5 | `Cli::resolved_model` `pub` instead of `pub(super)` | easy | pending |
| API-V1.38-7 | `ExportModel.dim` is `Option<u64>` while every other dim field is `usize` | easy | pending |
| API-V1.38-8 | Two flags for the same semantic — `--wait-secs` (status) vs `--require-fresh-secs` (eval) | easy | pending |
| API-V1.38-9 | `EmbedderError::HfHub(String)` and `RerankerError::ModelDownload(String)` sibling stringified errors | medium | ✅ #1567 (variant-name harmonization; full type collapse via `AuxModelError` deferred — embedder doesn't route through aux_model::resolve) |
| API-V1.38-10 | `LimitArg` flattened in 6 places; inline `limit: usize` still in 7 sister args | easy | ✅ #1544 (concrete `--limit 0` rejection) + ✅ #1569 (structural fan-out across 8 sister args) |

### Error Handling
| ID | Title | Difficulty | Status |
|---|---|---|---|
| EH-V1.38-6 | `cli/commands/infra/hook.rs:393, 536` conflate `NotFound` with permission-denied/oversize | easy | pending |
| EH-V1.38-8 | `parser/{calls,injection,aspx}` log tree-sitter `LanguageError` via `?e` instead of `%e` | easy | pending |

### Observability
| ID | Title | Difficulty | Status |
|---|---|---|---|
| OB-V1.38-6 | Skip-first-pass-embed sentinel batch logs at debug (`cqs index --llm-summaries` mid-run search confusion) | easy | pending |
| OB-V1.38-8 | `--rerank multi-index` warn lacks `multi_index_count` field | easy | pending |
| OB-V1.38-9 | dispatch entry's three overlay loads have no top-level span | easy | pending |
| OB-V1.38-10 | UMAP step warn drops `chunk_count` and `dim` | easy | pending |

### Test Coverage (adversarial)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| TC-ADV-V1.38-7 | `embed_query` oversized-input truncation path untested | easy | pending |
| TC-ADV-V1.38-8 | `score_candidate` `threshold = NaN` / `±Inf` behaviour untested | easy | pending |
| TC-ADV-V1.38-9 | `load_synonym_overlay` / `load_classifier_vocab_overlay` `MAX_BYTES` cap untested | easy | pending |

### Robustness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RB-V1.38-6 | `parser/l5x.rs` regex-capture `.unwrap()` cluster (6 sites) | easy | pending |
| RB-V1.38-7 | `train_data/query.rs:14` static regex `.unwrap()` inconsistent with siblings | easy | pending |

### Scaling
| ID | Title | Difficulty | Status |
|---|---|---|---|
| SHL-V1.38-2 | `--require-fresh-secs` capped at hardcoded `600u64` (not env-overridable) | easy | pending |
| SHL-V1.38-3 | `PIPELINE_FAN_OUT_LIMIT = 50` silently truncates — no env override | easy | pending |
| SHL-V1.38-5 | `STREAM_BATCH_SIZE = 1024` UMAP path dim-blind, no env | easy | pending |
| SHL-V1.38-6 | `parse_channel_depth() = 512` ignores file size + chunk fan-out | easy | ✅ #1566 (default halved 512 → 256; full byte-budget derivation deferred) |
| SHL-V1.38-7 | LLM-pass `PAGE_SIZE = 500` literal duplicated in two files, no env | easy | pending |
| SHL-V1.38-8 | Reconcile streaming `BATCH = 1000` files hardcoded | easy | pending |
| SHL-V1.38-9 | `summary_queue` thresholds (64/200ms/10_000) hardcoded | medium | pending |
| SHL-V1.38-10 | `RETRY_BACKOFFS_MS` hardcoded `&[u64]` (local LLM resilience) | easy | pending |

### Algorithm Correctness
| ID | Title | Difficulty | Status |
|---|---|---|---|
| AC-V1.38-4 | `try_classify_negation` priority 1 fires on bare common nouns (`"no"`, `"exclude"`, `"avoid"`) | medium | pending |
| AC-V1.38-8 | `is_vendored_origin` config entries with slashes silently never match (no validation warn) | easy | pending |
| AC-V1.38-10 | `cmd_index --model X` drift check passes for unknown model when default substitutes silently (companion to AC-V1.38-7) | medium | pending |

### Extensibility
| ID | Title | Difficulty | Status |
|---|---|---|---|
| EX-V1.38-2 | C#/Java/Kotlin/Python `post_process_chunk` re-hardcodes `[Test]` lists already on `LanguageDef::test_markers` | medium | pending |
| EX-V1.38-3 | Reranker tunables env-only — no `[reranker]` config knobs | easy | pending |
| EX-V1.38-5 | `cqs task` waterfall budget weights are `const f64` — no operator knob | easy | pending |
| EX-V1.38-6 | `extract_calls_from_chunk` hardcoded `Language::Markdown` branch — should route through `custom_call_parser` | easy | pending |
| EX-V1.38-8 | `doc_writer/formats.rs` Go-specific "prepend FuncName" hardcoded `if language == Language::Go` | easy | pending |
| EX-V1.38-10 | `test_type_queries_compile` hand-lists 11 languages instead of `Language::ALL.iter()` | easy | pending |

### Platform
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PL-V1.38-6 | `daemon_control_hint` macOS arm should be `cfg(unix)` to cover BSD/illumos | easy | pending |
| PL-V1.38-8 | `train_data::git::Command::new("git")` PATH-lookup gap (sibling PB-V1.33-10 `tasklist` fix) | medium | pending |

### Security
| ID | Title | Difficulty | Status |
|---|---|---|---|
| SEC-V1.38-5 | `serve` `chunk_detail` / `static_asset` log user-controlled paths at info, no size cap | easy | pending |
| SEC-V1.38-6 | `validate_and_read_file` uses pre-canonicalize path for existence + size checks (TOCTOU + redundant check) | medium | pending |
| SEC-V1.38-7 | `run_git_diff` base-ref check missing `\n`/`\r` reject + `--` sentinel | easy | pending |
| SEC-V1.38-9 | `enforce_concurrency_cap` cap defaults from env without upper-bound validation | medium | pending |

### Data Safety
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DS-V1.38-5 | Migration backup `prune_old_backups` swallows `read_dir` errors — unbounded `.bak-*.db` accumulation possible | easy | pending |

### Performance
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PF-V1.38-5 | Tree-sitter `Parser::new()` allocated per `parse_file` call (600× per fresh index) | medium | pending |
| PF-V1.38-6 | `gather::expand` clones `Arc<str>` to `String` for HashMap key per BFS expansion | easy | pending |
| PF-V1.38-9 | `analyze_type_impact` 4+ string clones per (chunk × type) edge | medium | pending |
| PF-V1.38-10 | `find_test_chunks_cross` calls uncached `find_test_chunks()` per project (N×LIKE-scans) | easy | pending |

### Resource Management
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RM-V1.38-2 | `LocalProvider` stash cap is per-batch-count, not per-byte (multi-GB potential) | easy | pending |
| RM-V1.38-7 | `last_indexed_mtime` prune size-gated — long-idle daemon never trims | easy | pending |
| RM-V1.38-9 | Notes cache `Arc<Vec<Note>>` no size accounting | medium | pending |
| RM-V1.38-10 | `LocalProvider` `pool_max_idle_per_host` survives forever via `OnceLock` reuse | easy | pending |

### Test Coverage (happy path)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| TC-HAP-V1.38-5 | `cmd_dead` CLI handler untested end-to-end | easy | ✅ #1557 |
| TC-HAP-V1.38-6 | `cmd_explain` CLI handler untested end-to-end (`tests/graph_test.rs:explain_*` reimplement BFS in-process) | easy | ✅ #1558 |
| TC-HAP-V1.38-7 | `cmd_install` / `cmd_status` for git hooks have no direct test | medium | ✅ #1556 |
| TC-HAP-V1.38-8 | `cmd_trace --cross-project` arm has zero tests | medium | ✅ #1564 (also fixed slot-blindness bug in `from_config`) |
| TC-HAP-V1.38-9 | `Reranker::test_reranker_new` weak — never asserts model loads or scores plausibly | easy | ✅ #1559 |
| TC-HAP-V1.38-10 | `test_save_writes_file_with_0o600_perms` `target_os = "linux"`-gated; macOS coverage absent | easy | ✅ #1561 |

---

## P4 — defer (12 items)

Tracked under existing umbrella issues #1463 (P4 design) / #1459 (API design) / #1512 (Windows daemon), or hard refactors with diffuse impact.

### Extensibility (largely #1459 / #1463 territory)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| EX-V1.38-1 | `doc_format_for` 12-arm match on string tags — refactor to typed `DocFormat` field | easy | tracking (#1463) |
| EX-V1.38-4 | Classifier vocab overlay covers only 2 of 6 vocabularies — finish the overlay design | medium | tracking (#1463) |
| EX-V1.38-7 | `Language::Python` docstring extraction inside `extract_doc_comment` instead of on `LanguageDef` | medium | tracking (#1463) |
| EX-V1.38-9 | `extract_method_receiver_type` Go-specific gate should be on `LanguageDef` | medium | tracking (#1463) |

### Platform (#1512 Windows daemon territory)
| ID | Title | Difficulty | Status |
|---|---|---|---|
| PL-V1.38-7 | SPLADE/CAGRA tmp-file `to_string_lossy()` collision risk on non-UTF-8 paths | medium | tracking (#1512) |
| PL-V1.38-9 | `coarse_fs_resolution` returns Duration::ZERO on Windows (FAT32/exFAT silent drops) | medium | tracking (#1512) |
| PL-V1.38-10 | `aux_model::is_path_like` cache_dir Windows-vs-WSL skew | medium | tracking (#1512) |

### Security
| ID | Title | Difficulty | Status |
|---|---|---|---|
| SEC-V1.38-10 | `find_pdf_script` ownership check + python exec racy under symlink swap (suggested fix: embed script via `include_str!`) | medium | tracking |

### Data Safety
| ID | Title | Difficulty | Status |
|---|---|---|---|
| DS-V1.38-6 | `WRITE_LOCK` is process-global, not per-Store — design-level refactor for multi-store callers | medium | tracking |

### Resource Management
| ID | Title | Difficulty | Status |
|---|---|---|---|
| RM-V1.38-3 | `PLACEHOLDER_CACHE` 32,467 OnceLock slots — design-level cap rework | medium | tracking |
| RM-V1.38-6 | BatchContext SPLADE encoder slot held forever — needs Mutex<Option<...>> wrap | medium | tracking |
| RM-V1.38-8 | SOCKET-thread tokio runtime worker count not adaptive — tokio-design constraint | medium | tracking (won't fix) |

### Carry-overs from prior triage (still applicable)
| ID | Title | Status |
|---|---|---|
| P4-9 | daemon_translate `cfg(unix)`-only — Windows daemon | tracking (#1512) |
| P4-12/13/14 | CAGRA/SPLADE save no `.bak`; HNSW load file-lock no timeout | partially shipped via #1491/#1492 |
| P4-15..18 | open_browser cmd.exe / serve extractor / 64KiB pre-auth / daemon socket cleanup | tracking (#1461 sibling) |
| P4-19 | audit-mode no Windows ACL | tracking (#1463) |

(P4 carry-overs from v1.36.2 are listed for completeness; if the underlying issue moved or shipped, update inline.)

---

## Notes on calibration

- **Lying docs always P1.** All 10 DOC-V1.38-* findings + PL-V1.38-3 land in P1 per the team rule (CLAUDE.md memory: "Docs Lying Is P1").
- **TC-ADV cluster split P1 vs P2.** TC-ADV-V1.38-1 (security fix without test) and TC-ADV-V1.38-10 (data-corruption write surface) are P1; the comment-cited-fix backfills (P1-36/-43, DS-V1.33-2 races) are P2. All revert-resistant for shipped fixes.
- **Hot-path perf one-liners P1.** PF-V1.38-1 (per-candidate alloc storm), -3, -4, -7, -8 all gate the search hot path; OnceLock wrappers are one-line. PF-V1.38-2 is the missed sibling of PR #1502.
- **EH stored_model_name cluster all P1.** PR #1504 only fixed `cmd_model_swap`; the daemon-loop and slot-listing variants are equally destructive on metadata-read failure.
- **Recently-merged code (PR #1456-#1511) leans P2.** `--llm-summaries` (#1452/#1497) wiring gaps in DS-V1.38-1/2/3/8 are P1 because they corrupt search quality silently. PR #1505/#1506 observability gaps in OB-V1.38-1/2 are P2 — fix-pattern is fresh, infrastructure exists.
- **AC-V1.38-1 (BoundedScoreHeap tiebreak) is P1** — silent correctness regression on every SPLADE search at the eviction boundary. The one-line fix mirrors AC-V1.30.1-7's pattern.
- **Difficulty: hard → P4.** No "hard" findings in this audit; the medium-effort PL-V1.38-* items targeting Windows-native are deferred under #1512.
