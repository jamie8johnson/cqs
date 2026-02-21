# Audit Triage — v0.12.12

Generated: 2026-02-21

Source: `docs/audit-findings.md` — 14-category audit, 3 batches, 106 raw findings.

## De-duplication Notes

Cross-category duplicates (fix once):

1. **CQ-13 = PERF-21 = RM-23**: `dispatch_search` bypasses `audit_state()` cache → 1 fix
2. **CQ-12 = AC-24**: `COMMON_TYPES` divergence in onboard.rs → 1 fix
3. **EH-23 = OB-17**: `onboard_to_json` silent null → 1 fix
4. **EH-30 = DS-19**: `get_embeddings_by_hashes` swallows errors → 1 fix
5. **PB-15 = PERF-25**: context abs_path always fails → 1 fix
6. **PERF-27 = RM-17**: `dispatch_drift` uncached store → 1 fix
7. **SEC-13 = (RM)**: drift opens reference store read-write → 1 fix
8. **CQ-14 = TC-19**: batch `--tokens` silently ignored → 1 fix

Non-issues (self-identified by auditors): AC-25, AC-26, AC-29, AC-30, AC-31

After de-duplication: **~88 unique findings**

---

## P1: Fix Immediately (easy + high impact)

### Security

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | Batch stdin no line length limit — unbounded memory allocation | SEC-12 | easy | ✅ fixed |
| 2 | Drift opens reference store read-write instead of read-only | SEC-13 | easy | ✅ fixed |
| 3 | `dispatch_read` TOCTOU — read via canonical path to eliminate race | SEC-15 | easy fix (hard to exploit) | ✅ fixed |
| 4 | Batch `--limit` unclamped on Similar/Gather/Scout/Related — resource amplification | SEC-14 | easy | ✅ fixed |

### Algorithm Correctness (bugs)

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 5 | Windowed chunk IDs misparse — invisible to glob/note filtering | AC-22 | medium | ✅ fixed |
| 6 | `diff` modified sort conflates "unknown similarity" with "maximally changed" | AC-28 | easy | ✅ fixed |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 7 | `upsert_type_edges_for_file` TOCTOU — chunk ID read outside transaction | DS-14 | medium | ✅ fixed |
| 8 | Window priority depends on undefined row order in type edge resolution | DS-18 | easy | ✅ fixed |

### Error Handling

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 9 | `AnalysisError::Embedder` used for non-embedding errors in onboard | EH-26 | easy | ✅ fixed |
| 10 | `borrow_ref` panics with `.expect()` in non-test code | EH-24 | easy | ✅ fixed |

### Code Quality

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 11 | `COMMON_TYPES` defined twice with different contents (onboard vs focused_read) | CQ-12/AC-24 | easy | ✅ fixed |
| 12 | `dispatch_search` bypasses audit_state cache (reads disk per call) | CQ-13/PERF-21/RM-23 | easy | ✅ fixed |

**P1 Total: 12 findings**

---

## P2: Fix Next (medium effort + high impact)

### Performance / Caching

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `get_call_graph()` not cached in BatchContext — reloaded per command | PERF-22 | medium | |
| 2 | `dispatch_drift` opens fresh Store per call — bypasses reference cache | PERF-27/RM-17 | easy | |
| 3 | `list_notes_summaries()` redundantly loaded in search paths | PERF-23 | easy | |
| 4 | N+1 `search_by_name` in focused read type dependency loop | CQ-15 | medium | |
| 5 | N+1 `search_by_name` in `dispatch_trace` path enrichment | PERF-20 | easy | |
| 6 | `onboard` uses full `scout()` when only entry point needed | PERF-28 | easy | |
| 7 | Context abs_path lookup always fails — wasted SQLite query | PB-15/PERF-25 | easy | |

### Resource Management

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 8 | `get_ref` loads ALL reference stores to find one | RM-16 | easy | |
| 9 | Reranker model not cached in BatchContext | RM-18 | easy | |
| 10 | `Config::load` called per batch `get_ref`/`dispatch_drift` | RM-21 | easy | |
| 11 | Pipeline intermediate merge collects unbounded names | RM-20 | easy | |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 12 | Type edges upserted outside chunk transaction — crash inconsistency | DS-13 | medium | |
| 13 | `get_embeddings_by_hashes` swallows errors — partial results | EH-30/DS-19 | easy | |

### Code Quality / Duplication

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 14 | `dispatch_read_focused` duplicates `cmd_read_focused` (~140 lines) | CQ-8 | medium | |
| 15 | `dispatch_read` duplicates `cmd_read` (~90 lines) | CQ-9 | medium | |
| 16 | Duplicate `parse_nonzero_usize` function | CQ-10 | easy | |
| 17 | Duplicate CAGRA/HNSW vector index construction | CQ-11 | easy | |
| 18 | Batch `--tokens` accepted but silently ignored in 4 commands | CQ-14/TC-19 | easy | |

### API Design

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 19 | `get_types_used_by` returns tuple instead of typed struct | AD-21 | easy | |
| 20 | `onboard_to_json` silently returns null on failure | EH-23/OB-17 | easy | |
| 21 | `chunk_type` serialized inconsistently (Display vs Debug) | AD-20 | easy | |

### Robustness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 22 | Float params accept NaN/Infinity without validation (drift, similar, query) | RB-24/RB-28 | easy | |

**P2 Total: 22 findings**

---

## P3: Fix If Time

### Documentation

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | README `--expand 2` example wrong — flag is boolean | DOC-10 | easy | |
| 2 | README missing `cqs health` command | DOC-11 | easy | |
| 3 | README missing `cqs suggest` command | DOC-12 | easy | |
| 4 | CONTRIBUTING.md missing `health.rs`, `suggest.rs`, `deps.rs` from commands | DOC-13 | easy | |
| 5 | CONTRIBUTING.md missing library-level files | DOC-14 | easy | |
| 6 | CHANGELOG missing comparison URLs for v0.12.11/v0.12.12 | DOC-15 | easy | |
| 7 | ROADMAP shows onboard/drift as unchecked | DOC-16 | easy | |
| 8 | SECURITY.md missing reranker model download | DOC-17 | easy | |
| 9 | README missing `--include-types` on impact | DOC-18 | easy | |

### Error Handling

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 10 | `serde_json::to_string().unwrap()` in batch JSONL loop | EH-25 | easy | |
| 11 | `Store::open` without `.context()` in drift commands | EH-27/EH-28 | easy | |
| 12 | `pick_entry_point` returns sentinel instead of error | EH-29 | easy | |
| 13 | Missing `.context()` on embed_query in batch handlers | EH-31 | easy | |
| 14 | `staleness.rs` `warn_stale_results` logs error at debug instead of warn | EH-32 | easy | |

### Observability

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 15 | `run_index_pipeline` missing entry tracing span | OB-15 | easy | |
| 16 | `apply_windowing` missing tracing span | OB-16 | easy | |
| 17 | Pipeline GPU/CPU embedder threads lack thread-level spans | OB-18 | medium | |
| 18 | Batch pipeline errors not counted in summary | OB-19 | easy | |

### API Design

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 19 | TypeGraph missing Debug, Clone derives | AD-22 | easy | |
| 20 | ResolvedTarget missing Debug, Clone derives | AD-23 | easy | |
| 21 | Note missing Serialize derive — hand-rolled JSON | AD-24 | easy | |
| 22 | Drift types not re-exported from lib.rs | AD-25 | easy | |
| 23 | OnboardEntry.edge_kind stringly-typed — TypeEdgeKind exists | AD-26 | easy | |

### Robustness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 24 | `onboard` depth unbounded in library function | RB-26 | easy | |
| 25 | BFS chain reconstruction lacks iteration bound | RB-25 | easy | |
| 26 | `get_type_graph` cast uses `as i64` unnecessarily | RB-27 | easy | |
| 27 | `search_by_name` limit not clamped | RB-29 | easy | |
| 28 | `dispatch_trace` BFS no early-exit on target found | RB-30 | easy | |

### Algorithm Correctness

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 29 | `onboard` total_items excludes key_types | AC-23 | easy | |
| 30 | Pipeline stage numbering off-by-one in logs | AC-27 | easy | |

### Platform Behavior

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 31 | `onboard_to_json` PathBuf without backslash normalization | PB-14 | easy | |

### Data Safety

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 32 | `upsert_type_edges_for_file` deletes ALL file chunks, not just updated | DS-16 | easy | |
| 33 | `BatchContext::refs` uses RefCell — blocks future parallelization | DS-17 | easy | |
| 34 | Batch notes/audit cache never invalidated during session | DS-15 | easy (document) | |

### Performance

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 35 | `strip_markdown_noise` chains 8 intermediate Strings | PERF-24 | medium | |
| 36 | `suggest_notes` dedup uses O(n*m) substring matching | PERF-26 | easy | |

### Test Coverage

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 37 | TypeEdgeKind::from_str() round-trip untested | TC-16 | easy | |
| 38 | `warn_stale_results()` test discards return value | TC-17 | easy | |
| 39 | `onboard.rs` tests test std library, not project code | TC-20 | easy | |

**P3 Total: 39 findings**

---

## P4: Defer / Create Issues

### Test Coverage

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 1 | `onboard()` zero integration test | TC-11 | medium | |
| 2 | `health_check()` only tested with empty store | TC-12 | easy | |
| 3 | `suggest_notes()` only tested with empty store | TC-13 | medium | |
| 4 | `detect_drift()` tested only with empty stores | TC-14 | medium | |
| 5 | `apply_windowing()` zero test coverage | TC-15 | medium | |
| 6 | No CLI integration tests for drift/onboard/health/suggest/deps | TC-18 | medium | |

### Extensibility

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 7 | `suggest_notes` detector registry hardcoded | EXT-20 | easy | |
| 8 | `is_callable_type` hardcodes Function/Method | EXT-21 | easy | |
| 9 | Pipeline tuning constants are local variables | EXT-22 | easy | |
| 10 | `health_check` hardcodes top-5 hotspots | EXT-23 | easy | |
| 11 | `detect_dead_clusters` threshold hardcoded | EXT-24 | easy | |
| 12 | Untested hotspot threshold hardcoded in 2 files | EXT-25 | easy | |
| 13 | `PIPEABLE_COMMANDS` requires manual update | EXT-26 | easy | |
| 14 | `extract_names` field list requires manual update | EXT-27 | easy | |
| 15 | `classify_mention` heuristic tightly coupled | EXT-28 | easy | |

### Resource Management

| # | Finding | Source | Difficulty | Status |
|---|---------|--------|------------|--------|
| 16 | `semantic_diff` no size cap on chunk identity loading | RM-19 | medium | |
| 17 | Batch REPL holds GPU index for entire session | RM-22 | medium (document) | |
| 18 | `onboard` allocates full content for all callees+callers | RM-24 | medium | |

**P4 Total: 18 findings**

---

## Summary

| Priority | Findings | Action |
|----------|----------|--------|
| P1 | 12 | Fix immediately — security + correctness bugs |
| P2 | 22 | Fix next — performance + caching + duplication |
| P3 | 39 | Fix if time — docs + observability + robustness + easy cleanups |
| P4 | 18 | Defer — tests + extensibility + design |
| **Total** | **~91** (unique) | |

## Cross-Category Themes

1. **Batch module is the hotspot**: 40+ findings touch `batch.rs`. The module duplicates CLI logic (CQ-8/9), bypasses caches (CQ-13, PERF-27, RM-16-21), lacks input validation (SEC-12/14), and has no caching for call graph, config, or reranker. A `BatchContext` refactor with proper caching would fix ~15 findings at once.

2. **Onboard module shipped incomplete**: Misuses error variants (EH-26), diverges on COMMON_TYPES (CQ-12), PathBuf serialization (PB-14), sentinel values (EH-29), total_items miscount (AC-23), zero integration tests (TC-11), and wasteful scout() call (PERF-28). All stem from rapid feature development without cross-cutting review.

3. **Type edges subsystem has data safety gaps**: Transaction boundary issues (DS-13, DS-14), undefined row order (DS-18), implicit delete-all contract (DS-16). The type edge upsert was bolted on after the pipeline, and it shows.

4. **Documentation continues to drift**: 9 doc findings, all easy. Same pattern as v0.12.3 audit — new features ship without README/CONTRIBUTING updates.

5. **Windowed chunks are second-class citizens**: AC-22 (chunk ID parsing breaks), OB-16 (no windowing tracing), TC-15 (zero tests). The windowing feature was added to the pipeline but downstream consumers don't handle the ID format correctly.

## Recommended Fix Order

1. **P1 Security (#1-4)** — SEC-12 (line limit), SEC-13 (read-only), SEC-15 (TOCTOU), SEC-14 (limit clamp). All easy, real exposure.
2. **P1 Bugs (#5-8)** — AC-22 (windowed chunk parsing), AC-28 (drift sort), DS-14 (type edge TOCTOU), DS-18 (row order).
3. **P1 Errors/Code (#9-12)** — EH-26 (error variant), EH-24 (panic), CQ-12 (COMMON_TYPES), CQ-13 (cache bypass).
4. **P2 BatchContext caching (#1-3, 8-11)** — Biggest bang-for-buck: add OnceLock fields for call_graph, config, reranker, and fix get_ref to load single reference.
5. **P2 N+1 patterns (#4-5)** — Switch to batch queries where available.
6. **P2 Duplication (#14-18)** — Extract shared read/focused-read/index logic.
7. **P3 Docs (#1-9)** — One pass to update all documentation.
8. **P3 Observability (#15-18)** — Add tracing spans to pipeline + batch.
9. **P3 Easy cleanups (#19-39)** — Derives, depth clamping, iteration bounds.
10. **Re-assess at P3 boundary.**
