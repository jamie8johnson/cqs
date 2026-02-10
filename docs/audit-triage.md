# Audit Triage — v0.9.7

Generated: 2026-02-10

Source: `docs/audit-findings.md` — 14-category audit, 3 batches, ~161 raw findings.

## De-duplication Notes

Cross-category duplicates (fix once):

1. **eprintln → tracing** — O3/EH1 (config), O4/EH8 (lib.rs migration), O5/EH2 (notes), O15/EH17 (signal), O16/EH3 (reference). 5 pairs = 10 findings → 5 fixes.
2. **cosine_similarity duplication** — A5/CQ-6/AC9/EH14. 4 findings → 1 fix (remove diff.rs copy, use math.rs).
3. **name scoring duplication** — A4/CQ-7. 2 findings → 1 fix.
4. **normalize_for_fts byte truncation panic** — R7/S10/EH13(partial). 3 findings → 1 fix.
5. **notes text byte truncation** — R8/EH13. 2 findings → 1 fix.
6. **resolve.rs duplication** — CQ-1/EH11. 2 findings → 1 fix.
7. **config read-modify-write race** — DS4/S9. 2 findings → 1 fix.
8. **HNSW temp cleanup** — PB9 (partial overlap with prior triage PB3).
9. **watch mode path normalization** — PB1/PB2/DS11. 3 findings → 1 fix.
10. **notes temp file leak** — EH16/DS8. 2 findings → 1 fix.

After de-duplication: **~140 unique findings**

---

## P1: Fix Immediately (easy + high impact)

### Bugs / Correctness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `normalize_for_fts` byte-slices CJK text → panic (MCP crashable) | R7/S10 | medium | |
| 2 | `notes list` byte-truncation `&note.text[..117]` → panic on CJK | R8/EH13 | easy | |
| 3 | Windowed chunk ID path extraction via `rfind(':')` breaks glob filter | AC10 | easy | |
| 4 | Dead code detection trait impl check matches method body, not impl block | AC5 | medium | |
| 5 | `resolve_target` silently returns wrong-file result on filter miss | EH11 | medium | |
| 6 | CLI `--limit` not clamped — `usize::MAX as i64` wraps to -1, returns all rows | R4 | easy | |
| 7 | `gather()` BFS non-deterministic output (HashMap iteration order) | AC2 | easy | |
| 8 | Tautological assertion in gather test (`!empty || empty`) | TC1 | easy | |
| 9 | Diff test never asserts on `modified` list | TC12 | easy | |

### Duplication / Code Quality

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 10 | Duplicate `cosine_similarity` in diff.rs (different behavior from math.rs) | A5/CQ-6 | easy | PR #333 |
| 11 | Duplicate name scoring logic in store | A4/CQ-7 | easy | PR #333 |
| 12 | resolve.rs copied identically between CLI and MCP (52 lines x2) | CQ-1 | easy | PR #333 |
| 13 | Per-call `Regex::new().unwrap()` in markdown parser (should be LazyLock) | CQ-8/R1 | easy | PR #333 |
| 14 | Duplicate `make_embedding` test helper across HNSW modules | CQ-10 | easy | PR #333 |
| 15 | Duplicate tokenization impl in nl.rs (function + iterator) | CQ-9 | easy | PR #333 |

### eprintln → tracing (5 locations)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 16 | Config::load() eprintln | O3/EH1 | easy | |
| 17 | resolve_index_dir migration eprintln | O4/EH8 | easy | |
| 18 | CLI notes commands eprintln | O5/EH2 | easy | |
| 19 | signal handler setup eprintln | O15/EH17 | easy | |
| 20 | reference commands eprintln | O16/EH3 | easy | |

### Documentation

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 21 | ROADMAP: Markdown listed as "Parked" but shipped in v0.9.6 | D1 | easy | |
| 22 | CONTRIBUTING.md: missing audit_mode.rs, read.rs, audit.rs | D2/D3 | easy | |
| 23 | README: `cqs notes list` in "Call Graph" section | D4 | easy | |
| 24 | README: `--sources` documented as CLI flag, is MCP-only | D5 | easy | |
| 25 | lib.rs Quick Start: unused `ModelInfo` import | D6 | easy | |
| 26 | ROADMAP: "8 languages total" should be 9 | D7 | easy | |

### Observability (easy tracing additions)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 27 | `gather()` no logging/tracing spans | O1 | easy | |
| 28 | `semantic_diff()` no logging/timing | O2 | easy | |
| 29 | `cmd_gather` no tracing span | O6 | easy | |
| 30 | `cmd_gc` no tracing span | O7 | easy | |
| 31 | `extract_call_graph` no timing | O8 | easy | |
| 32 | `build_hnsw_index` no timing | O9 | easy | |
| 33 | `find_dead_code` no logging | O12 | easy | |
| 34 | `prune_stale_calls` no logging | O13 | easy | |
| 35 | `replace_notes_for_file` no completion log | O14 | easy | |

### Error Handling (easy fixes)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 36 | `check_model_version` ignores dimension parse failure | EH7 | easy | |
| 37 | MCP batch tool errors serialized but not logged | EH9 | easy | |
| 38 | `set_permissions` errors silently discarded (7 locations) | EH10 | easy | |
| 39 | `cosine_similarity` returns 0.0 on dim mismatch with no warning | EH14 | easy | |
| 40 | Watch canonicalize silently falls back | EH15 | easy | |
| 41 | Temp file not cleaned on notes serialization failure | EH16/DS8 | easy | |

### Platform / Permissions (easy fixes)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 42 | `save_audit_state` no 0o600 permissions | PB5 | easy | |
| 43 | `ProjectRegistry.save()` no 0o600 permissions | PB6 | easy | |
| 44 | Inconsistent canonicalization (3 patterns, dunce already a dep) | PB4 | easy | PR #333 |
| 45 | HNSW temp cleanup uses `remove_dir` not `remove_dir_all` | PB9 | easy | |

### Extensibility (easy)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 46 | `cqs_batch` only supports 6/20 tools (gap grew from 8/14) | X1 | easy | |
| 47 | `gather()` hardcodes seed params (5 results, 0.3 threshold) | X5 | easy | |
| 48 | BFS decay 0.8 hardcoded in gather | X6 | easy | |
| 49 | Config missing `note_weight` and `note_only` | X3 | easy | |

**P1 Total: 49 findings**

---

## P2: Fix Next (medium effort + high impact, or easy + moderate impact)

### Bugs / Correctness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `gather()` BFS assigns first-discovered score, not best | AC3 | medium | |
| 2 | Diff ChunkKey includes line_start → false add+remove on reorder | AC4 | medium | |
| 3 | `search_across_projects` never uses HNSW (always O(n)) | AC7 | medium | |
| 4 | Unified search note slots over-allocated when code sparse | AC1 | medium | |
| 5 | impact/test_map MCP tools swallow DB errors with `.ok()` | EH5 | medium | |
| 6 | impact/test_map CLI tools swallow DB errors with `.ok()` | EH6 | medium | |
| 7 | `gather()` silently falls back to empty on batch search failure | EH4 | medium | |

### Duplication (medium)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 8 | Focused-read logic (TYPE_NAME_RE, COMMON_TYPES) duplicated CLI/MCP | CQ-2 | medium | PR #333 |
| 9 | Note injection logic duplicated in 4 places | CQ-3 | medium | PR #333 |
| 10 | Impact command duplicated CLI/MCP (377+256 lines) | CQ-4 | medium | PR #333 |
| 11 | JSON result formatting duplicated in 5+ locations | CQ-5 | medium | PR #333 |

### Performance (high-impact easy/medium)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 12 | Watch mode per-chunk upsert (50 txns vs 1) | P2 | easy | |
| 13 | Watch mode re-embeds all chunks (no content-hash cache) | P3 | easy | |
| 14 | `embedding_to_bytes` per-float iterator instead of memcpy | P7 | easy | |
| 15 | `search_by_names_batch` N+1 FTS queries | P4 | medium | |
| 16 | `find_dead_code` loads full content for all candidates | P10 | easy | |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 17 | Config read-modify-write race (no file lock) | DS4/S9 | medium | |
| 18 | Watch mode delete + reinsert not atomic | DS3 | medium | |
| 19 | Pipeline chunks + call graph in separate transactions | DS2 | medium | |
| 20 | `ProjectRegistry.save()` no locking or atomic write | DS5 | easy | |
| 21 | `parse_notes` reads via separate handle after lock | DS7 | easy | |

### Security

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 22 | `tool_context` no path traversal validation | S2 | medium | |
| 23 | Reference config `path` allows arbitrary filesystem access | S5 | medium | |
| 24 | Project .cqs.toml can override user config references | S6 | medium | |
| 25 | `sanitize_error_message` misses path prefixes | S3 | easy | |
| 26 | MCP protocol version header reflected unsanitized | S4 | easy | |

### Robustness (medium)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 27 | HNSW save `assert_eq!` panics in MCP context | R9 | medium | |
| 28 | `embedding_to_bytes` `assert_eq!` panics | R10 | medium | |
| 29 | `embed_batch` discards tensor shape, trusts hardcoded dim | R6 | medium | |

### Platform (medium)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 30 | Watch mode path separator mismatch (Windows chunk duplication) | PB1/PB2/DS11 | medium | |
| 31 | HNSW save no cross-device rename fallback | PB3 | medium | |

### Resource Management (easy)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 32 | Store page cache 16MB x 4 x N stores (up to 384MB) | RM6 | easy | |
| 33 | HNSW ID map load doubles memory during parse | RM7 | easy | |
| 34 | Pipeline creates 2 Embedders simultaneously (~1GB) | RM12 | easy | |
| 35 | Background CAGRA opens second Store with full pool | RM5 | easy | |
| 36 | Watch `pending_files` retains capacity after burst | RM9 | easy | |

### API Design / Types

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 37 | `ChunkIdentity`/`DiffEntry` use String where enums exist | A1 | easy | |
| 38 | `call_stats`/`function_call_stats` return unnamed tuples | A3 | easy | |
| 39 | `Embedding::new()` skips dim validation (unsafe-by-default) | A7 | easy | |
| 40 | `Language::def()` panics on registry desync | R2 | easy | |
| 41 | `as_object_mut().unwrap()` in impact JSON | R3 | easy | |
| 42 | `cap.get(0).unwrap()` in markdown extraction | R5 | easy | |

### Extensibility (medium)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 43 | `apply_config_defaults` magic numbers detect unset flags | X9 | medium | |
| 44 | Test-file patterns hardcoded in SQL, duplicated in Rust | X4 | medium | |
| 45 | Adding structural Pattern requires 5 changes + MCP schema | X2 | medium | |

**P2 Total: 45 findings**

---

## P3: Fix If Time (moderate impact, can batch)

### Observability

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | Reference search no per-reference timing | O10 | medium | |
| 2 | Embedding cache hit/miss not observable | O11 | medium | |

### Test Coverage

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 3 | 11/20 MCP tools untested | TC2 | medium | |
| 4 | 11 CLI commands untested | TC3 | medium | |
| 5 | `search_filtered` no unit tests | TC4 | medium | |
| 6 | `search_across_projects` zero tests | TC5 | medium | |
| 7 | `store/chunks.rs` 817 lines no inline tests | TC8 | medium | |
| 8 | `reference.rs` load/search no direct tests | TC9 | medium | |
| 9 | `cmd_gc` zero tests | TC10 | easy | |
| 10 | `cmd_dead` no CLI integration test | TC11 | easy | |
| 11 | MCP error assertions only check `is_some()` | TC15 | easy | |
| 12 | Note round-trip test missing | TC7 | easy | |
| 13 | `find_project_root` no tests | TC13 | easy | |
| 14 | 127 dead functions, most untested | TC14 | medium | |

### API Design

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 15 | Asymmetric callers/callees return types | A2 | medium | |
| 16 | `SearchFilter` mixed encapsulation (pub fields + builder) | A6 | easy | |
| 17 | `serve_stdio`/`serve_http` inconsistent path param types | A8 | easy | |
| 18 | Note/NoteEntry/NoteSummary naming overload | A9 | medium | |
| 19 | `GatherOptions` lacks builder methods | A10 | easy | |

### Performance

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 20 | `normalize_for_fts` called 4x per chunk | P6 | easy | |
| 21 | Diff loads all identities even with language filter | P5 | medium | |
| 22 | `search_across_projects` new Store per project per search | P8 | medium | |
| 23 | Gather loads entire call graph every invocation | P11 | medium | |
| 24 | Pipeline writer clones chunk+embedding pairs | P9 | easy | |
| 25 | `get_call_graph` clones all strings into both maps | P12 | easy | |

### Resource Management

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 26 | `semantic_diff` loads all matched-pair embeddings at once | RM1 | medium | |
| 27 | Reference hot-reload blocks search during WAL checkpoint | RM3 | medium | |
| 28 | Each Store creates own tokio Runtime (7+ with refs) | RM4 | medium | |
| 29 | Embedder ~500MB persists forever via OnceLock | RM8 | easy | |
| 30 | HNSW+CAGRA held simultaneously during upgrade | RM10 | easy | |
| 31 | `all_chunk_identities` loads full table no SQL filter | RM11 | easy | |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 32 | `embedding_batches` LIMIT/OFFSET unstable under writes | DS9 | medium | |
| 33 | Store::init() DDL without transaction | DS1 | medium | |
| 34 | WAL checkpoint failure silently returns Ok | DS13 | easy | |
| 35 | No SQLite integrity check on open | DS12 | medium | |
| 36 | Schema migration no downgrade guard | DS10 | medium | |

### Security

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 37 | FTS5 injection via double-quote escape | S1 | medium | |
| 38 | HNSW ID map no size limit on deser | S8 | medium | |
| 39 | Windows PID substring matching false positive | S7 | easy | |

### Platform

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 40 | WSL detection heuristic only checks `/mnt/` | PB7 | easy | |
| 41 | project.rs tests use Unix-only paths | PB8 | easy | |

### Extensibility

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 42 | `cmd_doctor` no extension points | X7 | medium | |

### Other

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 43 | `StoreError::Runtime` catch-all string variant | EH12 | medium | |
| 44 | `note_stats` thresholds assume discrete sentiment, no DB constraint | AC6 | easy | |
| 45 | `BoundedScoreHeap` drops equal-score newcomers (iteration-order bias) | AC8 | easy | |

**P3 Total: 45 findings**

---

## P4: Defer / Create Issues

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `search_filtered` brute-force loads all embeddings | P1 | hard | existing #269 |
| 2 | HNSW not rebuilt after watch updates | DS6 | hard | existing #236 |
| 3 | `embed_documents` no tests (requires model) | TC6 | hard | |
| 4 | MCP tool schemas handwritten JSON, not generated | X8 | hard | |
| 5 | CAGRA `build_from_store` no OOM guard on pre-alloc | RM2 | medium | existing #302 |

**P4 Total: 5 findings**

---

## Summary

| Priority | Findings | Easy | Medium | Hard | Action |
|----------|----------|------|--------|------|--------|
| P1 | 49 | 43 | 6 | 0 | Fix immediately |
| P2 | 45 | 19 | 26 | 0 | Fix next |
| P3 | 45 | 17 | 28 | 0 | When convenient |
| P4 | 5 | 0 | 1 | 4 | Defer / issues |
| **Total** | **~144** (deduped from ~161 raw) | **79** | **61** | **4** |

## Open GitHub Issues Cross-Reference

| Issue | Overlaps With |
|-------|---------------|
| #236 | DS6 (HNSW stale after watch) |
| #269 | P1 (brute-force all embeddings) |
| #302 | RM2 (CAGRA OOM) |
| #300 | R10 (embedding_to_bytes assert) |

## Recommended Fix Order

1. **P1 Bugs (#1-9)** — Highest priority. #1 and #2 are panics reachable via user input.
2. **P1 Duplication (#10-15)** — High ROI, reduces maintenance burden.
3. **P1 eprintln (#16-20)** — Mechanical, 5 locations.
4. **P1 Docs (#21-26)** — Lowest risk, highest confidence.
5. **P1 Observability (#27-35)** — Mechanical tracing additions.
6. **P1 Error/Platform/Ext (#36-49)** — Easy fixes, batch together.
7. **P2 by sub-category** — Start with bugs, then perf, then data safety.
8. **Re-assess at P2/P3 boundary.**
