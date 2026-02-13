# Audit Triage — v0.12.3

Generated: 2026-02-13

Source: `docs/audit-findings.md` — 14-category audit, 3 batches, 95 raw findings.

## De-duplication Notes

Cross-category duplicates (fix once):

1. **EH-17 = OB-12**: `suggest_tests` missing tracing span → 1 fix
2. **EH-21 = RB-13**: Regex per-call in cleaning.rs → 1 fix (LazyLock)
3. **EH-18 = RB-12**: unwrap() on chars().next() → 1 fix
4. **CQ-1 = RM-10**: review_diff double-load graph/tests → 1 fix
5. **CQ-3 = RM-12**: find_transitive_callers N+1 → 1 fix
6. **CQ-4 ≈ AD-14**: review types vs impact types → 1 fix
7. **RB-15/16 = DS-7**: SQLite variable limit → 1 fix (batch inserts)
8. **DS-8 = RM-11**: reference stores read-write → 1 fix
9. **PERF-14 = RM-15**: cross-index bridge sequential → 1 fix
10. **AD-15**: read_stdin/run_git_diff duplication → already on roadmap

After de-duplication: **~75 unique findings**

Non-issues removed: RB-17 (node_letter cast safe), RB-20 (total_limit safe), RB-21 (unicode uppercase edge), RB-23 (random collision), DS-11 (SQL interpolation safe)

---

## P1: Fix Immediately (easy + high impact)

### Security

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | CHM symlink escape — arbitrary file read via crafted archive | SEC-9 | easy | ✅ fixed |
| 2 | Webhelp symlink traversal — file read via symlinks in content dir | SEC-11 | easy | ✅ fixed |
| 3 | CHM zip-slip — 7z extraction path traversal | SEC-10 | medium | ✅ fixed |

### Bugs / Correctness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 4 | `score_name_match` 0.5 floor causes batch result misassignment | AC-13 | easy | ✅ fixed |
| 5 | Reference stores opened read-write instead of read-only (regression) | DS-8/RM-11 | easy | ✅ fixed |
| 6 | `std::canonicalize` in convert overwrite guard (dunce missed) | PB-11 | easy | ✅ fixed |
| 7 | `DiffTestInfo.via` always picks first changed function (regression) | AC-16 | easy | ✅ fixed |

### Observability Gaps

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 8 | `suggest_tests` missing tracing span | EH-17/OB-12 | easy | ✅ fixed |
| 9 | `compute_hints` missing tracing span | OB-13 | easy | ✅ fixed |
| 10 | `cmd_query_name_only` missing tracing span | OB-14 | easy | ✅ fixed |

### Error Handling (silent failures)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 11 | Silent file read failure in `resolve_parent_context` | EH-16 | easy | ✅ fixed |
| 12 | Missing `.context()` on 7z spawn in CHM conversion | EH-19 | easy | ✅ fixed |
| 13 | Missing `.context()` on fs::write/mkdir in convert | EH-20 | easy | ✅ fixed |
| 14 | Regex per-call instead of LazyLock in cleaning.rs (6x) | EH-21/RB-13 | easy | ✅ fixed |

**P1 Total: 14/14 fixed**

---

## P2: Fix Next (medium effort + high impact)

### Performance / N+1 Patterns

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `find_transitive_callers` N+1 search_by_name queries | CQ-3/RM-12 | medium | ✅ fixed |
| 2 | `review_diff` loads call graph + test chunks twice | CQ-1/RM-10 | easy | ✅ fixed |
| 3 | SQLite variable limit overflow on large batch inserts | RB-15/16/DS-7 | medium | ✅ fixed |
| 4 | `analyze_diff_impact` per-function caller fetch N+1 | PERF-13 | easy | ✅ fixed |
| 5 | `context` command N+1 per-chunk caller/callee queries | PERF-12 | medium | ✅ fixed |

### API Design / Types

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 6 | Impact types missing standard derives (Debug, Clone, Serialize) | AD-12 | easy | ✅ fixed |
| 7 | Leaked opaque types — public fields use unexported types | AD-13 | medium | ✅ fixed |
| 8 | CLI args stringly-typed instead of clap value_enum | AD-17 | easy | ✅ fixed |
| 9 | RiskScore contains redundant name field | AD-18 | easy | ✅ fixed |
| 10 | GatherOptions lacks Debug derive | AD-19 | easy | ✅ fixed |

### Code Quality

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 11 | Four reference search functions differ only by weight | CQ-5 | easy | ✅ fixed |
| 12 | `get_caller_counts_batch` / `get_callee_counts_batch` identical | CQ-7 | easy | ✅ fixed |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 13 | `gather_cross_index` no model compatibility check | DS-10 | easy | ✅ fixed |
| 14 | `review_diff` inconsistent error handling (fail-fast vs degrade) | DS-12 | easy | ✅ fixed |

### Documentation

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 15 | README missing `cqs review` command | DOC-1 | easy | ✅ fixed |
| 16 | README missing `--tokens` flag | DOC-2 | easy | ✅ fixed |
| 17 | README missing `--ref` scoped search | DOC-3 | easy | ✅ fixed |
| 18 | CONTRIBUTING.md `impact.rs` → `impact/` directory | DOC-4 | easy | ✅ fixed |
| 19 | CONTRIBUTING.md missing review.rs (commands + library) | DOC-5/6 | easy | ✅ fixed |
| 20 | CHANGELOG missing comparison URLs v0.12.2/v0.12.3 | DOC-7 | easy | ✅ fixed |
| 21 | SECURITY.md missing convert attack surface | DOC-8 | medium | ✅ fixed |
| 22 | ROADMAP completed items under "Next" | DOC-9 | easy | ✅ fixed |

### Security (defense-in-depth)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 23 | CQS_PDF_SCRIPT env var — log warning when active | SEC-8 | easy | ✅ fixed |

**P2 Total: 23/23 fixed**

---

## P3: Fix If Time

### Algorithm / Logic

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | Gather BFS decay double-compounds (expand_depth >= 2) | AC-14 | medium | ✅ fixed |
| 2 | Gather expansion cap overshoot by out-degree | AC-18 | easy | ✅ fixed |
| 3 | `extract_call_snippet` wrong for windowed chunks | AC-19 | medium | ✅ fixed |
| 4 | Context token packing uses file order, not relevance | AC-21 | easy | ✅ fixed |

### Code Quality / Duplication

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 5 | `gather`/`gather_cross_index` ~80 lines BFS duplication | CQ-2 | medium | ✅ fixed |
| 6 | Review types near-duplicate impact types (String vs PathBuf) | CQ-4/AD-14 | easy | ✅ fixed |
| 7 | `convert_file`/`convert_webhelp` duplicate pipeline | CQ-6 | easy | ✅ fixed |
| 8 | Token packing duplicated across 5 commands | EXT-15 | medium | ✅ fixed |

### Test Coverage

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 9 | `review_diff()` zero tests | TC-1 | medium | ✅ fixed |
| 10 | `reverse_bfs_multi()` zero tests | TC-3 | easy | ✅ fixed |
| 11 | Token budgeting no functional tests | TC-5 | medium | ✅ fixed |
| 12 | `analyze_diff_impact` test discovery not verified e2e | TC-9 | medium | ✅ fixed |

### Convention / Minor

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 13 | `unwrap()` in non-test code (Java test name) | EH-18/RB-12 | easy | ✅ fixed |
| 14 | `via` field defaults to empty on BFS anomaly | EH-22 | easy | ✅ fixed |
| 15 | Byte-index slicing → `strip_prefix` | RB-14 | easy | ✅ fixed |
| 16 | `--tokens 0` accepted, produces confusing output | RB-18 | easy | ✅ fixed |

### Extensibility

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 17 | Risk score thresholds magic numbers | EXT-13 | easy | ✅ fixed |
| 18 | `suggest_test_file` hardcoded per-language conventions | EXT-16 | easy | deferred |
| 19 | Review command missing `--tokens` support | EXT-18 | easy | ✅ fixed |
| 20 | Copyright regex hardcoded year range + vendor | EXT-19 | easy | ✅ fixed |

### Resource / Performance

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 21 | CHM/WebHelp unbounded memory accumulation | RM-13 | easy | ✅ fixed |
| 22 | `suggest_tests` N+1 get_chunks_by_origin | RM-14 | medium | ✅ fixed |
| 23 | Token counting per-chunk instead of batch | PERF-15 | easy | ✅ fixed |
| 24 | `python3` not on stock Windows | PB-12 | easy | ✅ fixed |
| 25 | `find_7z` error assumes Debian/Ubuntu | PB-13 | easy | ✅ fixed |

**P3 Total: 24/25 fixed (1 deferred: EXT-16)**

---

## P4: Defer / Create Issues

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `reverse_bfs_multi` depth accuracy (BFS ordering) | AC-15 | hard | |
| 2 | Risk scoring loses blast radius at full coverage | AC-20 | easy (design) | |
| 3 | Token packing doesn't count JSON overhead | AC-17 | easy (design) | |
| 4 | Convert filename TOCTOU race | DS-9 | medium | |
| 5 | Cross-index bridge search sequential | PERF-14/RM-15 | medium | |
| 6 | `DocFormat` N-changes-per-variant (same as #387) | EXT-14 | medium | |
| 7 | `is_webhelp_dir` hardcodes content/ | EXT-17 | easy | |
| 8 | `gather_cross_index` zero tests | TC-4 | hard | |
| 9 | `--ref` CLI integration untested | TC-6 | medium | |
| 10 | Various minor test gaps (TC-2/7/8/10) | TC-2/7/8/10 | easy-medium | |
| 11 | Review missing `--format` option | AD-16 | easy (design) | |
| 12 | PathBuf::from("") fallback cosmetic | RB-19 | easy | |
| 13 | `to_ascii_lowercase` unicode naming | RB-22 | easy | |
| 14 | read_stdin/run_git_diff duplication | AD-15 | easy (roadmap) | |

**P4 Total: 14 findings (deferred)**

---

## Summary

| Priority | Findings | Fixed | Deferred | Action |
|----------|----------|-------|----------|--------|
| P1 | 14 | 14 | 0 | All fixed |
| P2 | 23 | 23 | 0 | All fixed |
| P3 | 25 | 24 | 1 | EXT-16 deferred |
| P4 | 14 | 0 | 14 | Deferred / issues |
| **Total** | **~76** | **61** | **15** |

## Cross-Category Themes

1. **Convert module needs hardening**: 3 security findings (symlink + zip-slip), missing error context, missing LazyLock, memory caps. First new attack surface since MCP removal.
2. **Impact refactoring preserved bugs**: AC-16 (via attribution) and DS-8 (read-write reference stores) are regressions — issues "fixed" in v0.12.1 that survived the impact.rs → impact/ split unchanged.
3. **N+1 patterns still emerging**: CQ-3, PERF-12, PERF-13, RM-14 — four new N+1 query patterns in new code. The batch versions exist but aren't used.
4. **Token budgeting shipped without tests**: 6 commands support `--tokens` but zero behavioral tests. The packing logic is duplicated 5x.
5. **review_diff shipped without tests**: The entire review command has no unit or integration tests. Risk is amplified by the double-load bug (CQ-1).

## Recommended Fix Order

1. **P1 Security (#1-3)** — Symlink/zip-slip in convert module. Easy fixes, real exposure.
2. **P1 Bugs (#4-7)** — AC-13 (batch misassignment) is a correctness bug affecting gather + diff-impact.
3. **P1 Observability + Error (#8-14)** — Mechanical one-liners.
4. **P2 Performance (#1-5)** — N+1 patterns, double-loads, SQLite limit.
5. **P2 API/Types (#6-10)** — Impact derives enables format.rs cleanup.
6. **P2 Docs (#15-22)** — Quick documentation sweep.
7. **P2 Data Safety (#13-14)** — Model compatibility + error consistency.
8. **P3 Tests (#9-12)** — review_diff and bfs_multi need tests before more features build on them.
9. **Re-assess at P3 boundary.**
