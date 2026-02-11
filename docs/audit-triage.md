# Audit Triage — v0.12.1

Generated: 2026-02-11

Source: `docs/audit-findings.md` — 14-category audit, 3 batches, 151 raw findings.

## De-duplication Notes

Cross-category duplicates (fix once):

1. **ScoutError/SuggestError** — AD-2/AD-9/EH-2/EH-3/CQ-8. 5 findings → 1 fix (unify to anyhow::Result or shared AnalysisError).
2. **is_test_name false positives** — AC-11/RB-8/EXT-10. 3 findings → 1 fix (tighten pattern, reuse store/calls.rs logic).
3. **Test detection hardcoded/divergent** — EXT-7/EXT-10/AC-11/RB-8. 4 findings → 1 fix (shared is_test_function + config).
4. **related.rs ChunkType string comparison** — AD-6/AC-5. 2 findings → 1 fix.
5. **suggest_placement error swallowing** — EH-4/RB-10/OB-8(partial). 3 findings → 1 fix.
6. **suggest_tests error swallowing** — EH-7/OB-9. 2 findings → 1 fix.
7. **scout error swallowing** — EH-8/EH-9/OB-10. 3 findings → 1 fix (add tracing::warn in scout).
8. **LIKE wildcard escaping** — AC-4/SEC-5. 2 findings → 1 fix.
9. **search_chunks_by_signature substring matching** — AC-12/PERF-4. 2 findings → 1 fix.
10. **git diff argument injection** — SEC-1/RB-7. 2 findings → 1 fix (insert `--` separator).
11. **impact-diff stdin unbounded** — SEC-2/RB-2. 2 findings → 1 fix.
12. **diff_parse CRLF** — PB-1/TC-15. 2 findings → 1 fix + test.
13. **std::canonicalize instead of dunce** — PB-3/PB-4/PB-5. 3 findings → 1 sweep.
14. **Temp file predictable names** — SEC-3/SEC-4/SEC-6. 3 findings → 1 sweep.
15. **Per-function reverse BFS** — PERF-1/PERF-8/PERF-10. 3 findings → 1 fix (multi-source BFS).
16. **N+1 name queries** — PERF-2/PERF-3. 2 findings → batch API usage.
17. **map_hunks overlap + zero-count** — AC-1/RB-1. 2 findings → 1 fix.
18. **diff_parse boundary tracking** — AC-9/RB-11. 2 findings → 1 fix.

After de-duplication: **~120 unique findings**

---

## P1: Fix Immediately (easy + high impact)

### Bugs / Correctness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `is_test_name` false positives — `contains("_test")` matches `contest`, `fastest` | AC-11/RB-8/EXT-10 | easy | ✅ |
| 2 | `diff_parse` doesn't handle CRLF from Windows git | PB-1/TC-15 | easy | ✅ |
| 3 | `check_origins_stale` resolves relative paths against CWD, not project root | PB-6 | medium | ✅ |
| 4 | `DiffTestInfo.via` records first-iterated, not shortest-depth path | AC-3 | easy | ✅ |
| 5 | `map_hunks_to_functions` zero-count hunk edge case + u32 overflow | AC-1/RB-1 | easy | ✅ |
| 6 | `check_origins_stale` doesn't report deleted files as stale | PB-7 | easy | ✅ |
| 7 | `extract_call_snippet` wrong offset for windowed chunks | AC-2 | medium | ✅ |

### Security / Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 8 | `impact-diff --base` git argument injection via `--` prefixed values | SEC-1/RB-7 | easy | ✅ |
| 9 | `impact-diff --stdin` no input size limit (OOM) | SEC-2/RB-2 | easy | ✅ |
| 10 | `diff_parse.rs` Regex::new().unwrap() on every call | EH-1 | easy | ✅ |

### Error Swallowing (silent wrong results)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 11 | `suggest_placement` swallows store error → empty patterns | EH-4/RB-10 | easy | ✅ |
| 12 | `scout` swallows caller count + staleness errors → zeros | EH-8/OB-10 | easy | ✅ |
| 13 | `cmd_stats` swallows multiple store errors → misleading zeros | EH-11 | easy | ✅ |
| 14 | `cmd_context` swallows caller/callee count errors → zeros | EH-12 | easy | ✅ |
| 15 | `map_hunks_to_functions` silently skips files on store error | EH-6 | easy | ✅ |
| 16 | `analyze_diff_impact` silently skips callers on error | EH-10 | easy | ✅ |

### Documentation (factually wrong)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 17 | lib.rs Quick Start calls non-existent `store.search()` | DOC-1 | easy | ✅ |
| 18 | README search example uses deleted `serve_http` function | DOC-5 | easy | ✅ |
| 19 | ROADMAP version says v0.12.0, actual is v0.12.1 | DOC-2 | easy | ✅ |
| 20 | ROADMAP lists 2 completed items as "Next" | DOC-3 | easy | ✅ |

### Code Quality (high ROI)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 21 | `std::canonicalize` instead of `dunce` in 3 locations | PB-3/PB-4/PB-5 | easy | ✅ |
| 22 | `related.rs` ChunkType string comparison instead of enum match | AD-6/AC-5 | easy | ✅ |
| 23 | `ScoutError` missing `std::error::Error` impl | AD-9/EH-2 | easy | ✅ |
| 24 | `suggest_placement` unused `_root` parameter | AD-3 | easy | ✅ |
| 25 | `GatherDirection::FromStr` uses anyhow::Error | AD-10 | easy | ✅ |
| 26 | `gather` decay_factor accepts NaN/negative/>1.0 | RB-9 | easy | ✅ |

**P1 Total: 26 findings**

---

## P2: Fix Next (medium effort + high impact, or easy + moderate impact)

### Error Handling / Observability

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `suggest_tests` swallows graph/test errors → empty | EH-7/OB-9 | easy | |
| 2 | `find_relevant_notes` swallows error → empty | EH-9 | easy | |
| 3 | `compute_hints` silently discards errors via .ok() | OB-8 | easy | |
| 4 | `resolve_to_related` silently drops store errors | EH-5 | easy | |
| 5 | `get_chunks_by_ids` error swallowed in query | EH-13 | easy | |
| 6 | `suggest_tests` file chunk lookup swallows error | EH-15 | easy | |

### Observability (tracing gaps)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 7 | `scout()` no tracing span (140-line orchestrator) | OB-1 | easy | |
| 8 | `suggest_placement()` no tracing span | OB-2 | easy | |
| 9 | `find_related()` no tracing span | OB-3 | easy | |
| 10 | `analyze_impact()` no tracing span | OB-4 | easy | |
| 11 | `analyze_diff_impact()` no tracing span | OB-5 | easy | |
| 12 | `map_hunks_to_functions()` no tracing | OB-6 | easy | |
| 13 | 11 CLI commands missing tracing spans | OB-7 | easy | |
| 14 | `staleness.rs` uses eprintln instead of tracing | OB-11 | easy | |

### Code Quality

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 15 | CLI store-opening boilerplate 17+ times | CQ-1 | medium | |
| 16 | Path relativization duplicated 30+ times | CQ-2 | medium | |
| 17 | Config validation repeats clamp-and-warn 4x | CQ-4 | easy | |
| 18 | Atomic config write duplicated in 2 functions | CQ-5 | easy | |
| 19 | `impact_diff.rs` "no changes" JSON duplicated | CQ-6 | easy | |
| 20 | `related.rs` JSON construction tripled | CQ-3 | easy | |
| 21 | `ScoutError`/`SuggestError` near-identical types | CQ-8/AD-2 | easy | |
| 22 | `analyze_diff_impact` returns empty changed_functions for caller to fill | AD-11 | easy | |

### API Design

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 23 | `CallerInfo` name collision (store vs impact) | AD-1 | medium | |
| 24 | `ScoutChunk.chunk_type` is String instead of enum | AD-5 | easy | |
| 25 | `resolve_target` returns unnamed tuple | AD-7 | easy | |
| 26 | Path relativization handled inconsistently across modules | AD-8 | easy | |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 27 | Watch mode chunks + call graph not atomically consistent | DS-1 | medium | |
| 28 | `function_calls` table missing path normalization (Windows) | DS-2 | easy | |
| 29 | `save_audit_state()` non-atomic write | DS-3 | easy | |
| 30 | Watch mode mtime recorded after indexing (race) | DS-6 | easy | |

### Documentation

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 31 | CHANGELOG missing comparison URLs for 11 versions | DOC-4 | easy | |
| 32 | `--no-stale-check` undocumented in README | DOC-6 | easy | |
| 33 | `--summary` on context undocumented | DOC-7 | easy | |
| 34 | `--format mermaid` undocumented | DOC-8 | easy | |
| 35 | `--expand` search flag undocumented | DOC-9 | easy | |
| 36 | SECURITY.md missing write paths | DOC-10 | easy | |
| 37 | SECURITY.md confusing read/write note for refs | DOC-11 | easy | |

### Platform

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 38 | `map_hunks_to_functions` path mismatch diff vs index | PB-2 | medium | |
| 39 | `note_mention_matches_file` doesn't handle backslashes | PB-8 | easy | |
| 40 | `run_git_diff` missing `--no-pager` and `--no-color` | PB-9 | easy | |
| 41 | `suggest_test_file` hardcodes forward slashes | PB-10 | easy | |

### Resource Management

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 42 | References use `Store::open` (read-write) instead of `open_readonly` | RM-2 | easy | |
| 43 | `search_across_projects` opens read-write Store per project | RM-3 | medium | |

**P2 Total: 43 findings**

---

## P3: Fix If Time (moderate impact, can batch)

### Performance

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | Per-function reverse BFS in impact/scout (multi-source BFS fix) | PERF-1/8/10 | medium | |
| 2 | N+1 `search_by_name` queries in impact (batch exists) | PERF-2 | medium | |
| 3 | N+1 `get_chunks_by_name` in related.rs | PERF-3 | easy | |
| 4 | Per-type LIKE scan in related.rs | PERF-4/AC-12 | medium | |
| 5 | Per-file `get_chunks_by_origin` in where_to_add | PERF-5 | easy | |
| 6 | `extract_patterns` joins all content into one string | PERF-6 | easy | |
| 7 | `imports.contains()` O(n^2) dedup | PERF-9 | easy | |
| 8 | `get_call_graph()` double-clones strings | PERF-11 | easy | |

### Test Coverage

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 9 | `related.rs` zero tests (133 lines) | TC-1 | medium | |
| 10 | 5 new CLI commands (scout/where/related/impact-diff/stale) no integration tests | TC-6 | medium | |
| 11 | `suggest_tests()` zero coverage | TC-7 | medium | |
| 12 | `analyze_impact()` no direct tests | TC-8 | medium | |
| 13 | 4 tautological tests (TC-2/3/4/5) | TC-2/3/4/5 | easy | |
| 14 | `read_context_lines()` zero tests (80 lines) | TC-9 | easy | |
| 15 | `search_chunks_by_signature()` zero tests | TC-10 | easy | |
| 16 | Impact/diff-impact JSON serialization zero tests | TC-11 | easy | |
| 17 | `mermaid_escape` / `node_letter` untested | TC-12 | easy | |
| 18 | `display.rs` (496 lines) only 1 test | TC-13 | medium | |
| 19 | `warn_stale_results()` zero tests | TC-14 | easy | |
| 20 | `compute_hints_with_graph` stale data edge case untested | TC-16 | easy | |

### Extensibility

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 21 | `scout()` hardcodes search params (15/0.2) | EXT-1 | easy | |
| 22 | `suggest_placement()` hardcodes search params (10/0.1) | EXT-2 | easy | |
| 23 | `MODIFY_TARGET_THRESHOLD` hardcoded at 0.5 | EXT-3 | easy | |
| 24 | `MAX_TEST_SEARCH_DEPTH` = 5 not exposed | EXT-4 | easy | |
| 25 | `MAX_EXPANDED_NODES` = 200 in gather not configurable | EXT-5 | easy | |
| 26 | Test detection patterns not user-configurable | EXT-7 | medium | |
| 27 | `apply_config_defaults` desync risk (3 patterns) | EXT-11 | easy | |
| 28 | Import cap hardcoded at 5 in where_to_add | EXT-12 | easy | |

### API / Types

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 29 | Inconsistent JSON serialization patterns | AD-4 | medium | |
| 30 | `SuggestError`/`ScoutError` wrap errors as String, losing chain | EH-3 | easy | |
| 31 | `node_letter` ambiguous labels for indices 26+ | AC-7/EH-14 | easy | |

### Resource Management

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 32 | `last_indexed_mtime` grows without bound in watch | RM-1 | easy | |
| 33 | `find_test_chunks()` loads full content unnecessarily | RM-5 | easy | |
| 34 | `where_to_add` loads all chunks per file | RM-6 | easy | |
| 35 | `Embedder::clear_session` unusable in watch (needs &mut) | RM-8 | easy | |
| 36 | `reindex_files` clones Chunk+Embedding during grouping | RM-9 | easy | |
| 37 | `scout()` loads full call graph + test chunks per invocation | RM-4 | medium | |

### Robustness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 38 | `diff_parse` silently defaults unparseable hunk lines to 1 | RB-3 | easy | |
| 39 | `window_idx` i64→u32 cast truncates without clamping | RB-5 | easy | |
| 40 | `display.rs` end_idx+context+1 overflow | RB-6 | easy | |
| 41 | `Parser::new()` .expect() in library code | RB-4 | medium | |
| 42 | `diff_parse` doesn't track `diff --git` boundaries | AC-9/RB-11 | easy | |
| 43 | LIKE wildcard escaping in `search_chunks_by_signature` | AC-4/SEC-5 | easy | |

### Security (defense-in-depth)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 44 | Temp file predictable names (note.rs, config.rs, project.rs) | SEC-3/4/6 | easy | |

### Other

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 45 | `compute_hints_with_graph` prefetched count can diverge from graph | AC-8 | easy | |
| 46 | `BoundedScoreHeap` last-wins bias at equal scores | AC-6 | easy | |

**P3 Total: 46 findings**

---

## P4: Defer / Create Issues

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | Watch mode full HNSW rebuild on every change | PERF-7 | hard | existing deferred DS6 |
| 2 | HNSW multi-file save not atomically consistent | DS-4 | hard | |
| 3 | HNSW load TOCTOU between checksum and deserialization | DS-5 | hard | |
| 4 | `cqs dead` 86% false positive rate | CQ-7 | medium | |
| 5 | Adding CLI command requires 5 locations / 3 files | EXT-6 | medium | inherent to Rust+clap |
| 6 | `extract_patterns` 145-line closed switch per language | EXT-8 | medium | |
| 7 | Pattern enum still requires 5 changes per variant | EXT-9 | medium | existing X2 |
| 8 | Pipeline parses all files into one Vec (100K batch) | RM-10 | medium | |
| 9 | CAGRA dataset ~146MB retained permanently | RM-7 | medium | existing RM2/P4-5 |
| 10 | SEC-7: No regressions found (positive) | SEC-7 | n/a | |

**P4 Total: 10 findings (1 positive)**

---

## Summary

| Priority | Findings | Easy | Medium | Hard | Action |
|----------|----------|------|--------|------|--------|
| P1 | 26 | 23 | 3 | 0 | Fix immediately |
| P2 | 43 | 34 | 9 | 0 | Fix next |
| P3 | 46 | 33 | 13 | 0 | When convenient |
| P4 | 10 | 0 | 5 | 3 | Defer / issues |
| **Total** | **~125** (deduped from 151 raw) | **90** | **30** | **3** |

## Cross-Category Themes

1. **New modules lack observability + error handling**: scout, where_to_add, related, diff_parse all added post-v0.9.7 with zero tracing and pervasive `.unwrap_or_default()`. ~25 findings from this pattern alone.
2. **Test detection divergence**: 4 separate hardcoded pattern sets across SQL and Rust. Affects scout, impact, test-map, dead.
3. **Per-item BFS**: impact, diff-impact, and scout all run individual reverse BFS per function. Multi-source BFS would fix 3 findings.
4. **Path handling inconsistency**: 3 remaining `std::canonicalize` (should be dunce), backslash normalization gaps, CWD-relative staleness check.

## Recommended Fix Order

1. **P1 Bugs (#1-7)** — Wrong results reachable from normal usage.
2. **P1 Security (#8-10)** — Argument injection, OOM, unwrap.
3. **P1 Error swallowing (#11-16)** — Silent failures masking real errors.
4. **P1 Docs (#17-20)** — Factually wrong, confuses users.
5. **P1 Code quality (#21-26)** — Quick easy fixes.
6. **P2 Error/Observability (#1-14)** — Mechanical tracing additions.
7. **P2 Code quality (#15-22)** — Deduplication, boilerplate extraction.
8. **P2 Data safety + platform (#27-41)** — Correctness on edge platforms.
9. **P2 Resource management (#42-43)** — Easy wins (open_readonly).
10. **P2 Docs (#31-37)** — Fill documentation gaps.
11. **Re-assess at P2/P3 boundary.**
