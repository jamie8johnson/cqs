# Audit Triage — v1.4.0

**Date:** 2026-03-24
**Scope:** Full 14-category audit, 3 batches, 78 findings (76 unique after dedup)
**Cross-refs:** DS-15=EH-17, DS-16=PB-16, RM-26 overlaps existing #389

Informational "already fixed" confirmations excluded: CQ-21, EX-22, PERF-24

---

## P1: Easy + high impact — fix immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| EH-17/DS-15 | Corrupt dimension metadata bypasses check → garbage search | Error Handling / Data Safety | `store/mod.rs:574` | ✅ fixed |
| PB-16/DS-16 | `prune_missing` deletes entire index on native Windows (path separator mismatch) | Platform / Data Safety | `staleness.rs:30` | ✅ fixed |
| RB-12 | `&query[..200]` panics on multi-byte UTF-8 (CJK, emoji) | Robustness | `cli/commands/query.rs:41` | ✅ fixed |
| DS-13 | Watch mode `hnsw_dirty=true` stuck permanently after reindex failure → brute-force forever | Data Safety | `cli/watch.rs:430` | ✅ fixed |
| SEC-10 | `CQS_API_BASE` redirects API key to arbitrary URL without validation | Security | `llm/mod.rs:78` | ✅ fixed |
| AC-12 | `extract_call_snippet_from_cache` picks wrong-file chunk for common names (`new()`) | Algorithm | `impact/analysis.rs:114` | ✅ fixed |
| EX-21 | `related.rs` filters to Function/Method only — misses Constructor, Property, Macro, Extension | Extensibility | `related.rs:134` | ✅ fixed |
| RB-13 | Reranker `outputs[0]` panics on corrupt/wrong ONNX model | Robustness | `reranker.rs:190` | ✅ fixed |
| PERF-19 | N+1 `get_chunks_by_origin` per file in `map_hunks_to_functions` — batch variant exists | Performance | `impact/diff.rs:37` | ✅ fixed |
| PERF-22 | Per-file `upsert_function_calls` transactions — 3000 separate txns during indexing | Performance | `cli/pipeline.rs:717` | ✅ fixed |

**10/10 fixed.**

---

## P2: Medium effort + high impact — fix in batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| EH-18 | `DiffImpactResult` missing `degraded` flag — review risk scoring uses empty callers silently | Error Handling | `impact/diff.rs:114` | ✅ fixed |
| EH-20 | Pipeline swallows call/type-edge write errors — incomplete call graph invisible | Error Handling | `cli/pipeline.rs:721` | ✅ fixed |
| AC-9 | Forward BFS vs reverse BFS diverge on test counting | Algorithm | `impact/hints.rs:86` | ✅ fixed |
| AD-28 | Types derive Serialize + have parallel hand-built `_to_json()` — inconsistent path format | API Design | `scout.rs:432` + 4 others | ✅ fixed |
| DS-14 | HNSW save rollback deletes old index files without restoring them | Data Safety | `hnsw/persist.rs:312` | ✅ fixed |
| PERF-20 | Scout runs 15 independent `reverse_bfs` — batch exists but unused | Performance | `scout.rs:268` | ✅ fixed |
| PERF-21 | Diff impact runs N independent `reverse_bfs` for `via` attribution | Performance | `impact/diff.rs:164` | ✅ fixed |
| CQ-15/16/17/18 | LLM batch subsystem ~400 lines duplication | Code Quality | `llm/batch.rs` + 4 files | ✅ fixed (-215 lines) |
| EX-19 | NL generator has 5 hardcoded ChunkType branches | Extensibility | `nl.rs:456` | ✅ fixed |
| TC-17 | `check_model_version` zero tests | Test Coverage | `store/mod.rs:543` | ✅ fixed (4 tests) |
| TC-18 | `check_schema_version` zero tests | Test Coverage | `store/mod.rs:478` | ✅ fixed (4 tests) |
| RM-21 | `embed_batch` copies 50MB ONNX output tensor | Resource Mgmt | `embedder/mod.rs:610` | ✅ fixed |
| RM-22 | Reranker has no batch size cap | Resource Mgmt | `reranker.rs:94` | ✅ fixed (64 cap) |

**15/15 fixed.**

---

## P3: Easy + low impact — fix if time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| DOC-15 | `cqs doctor` shows stale model name | Documentation | `cli/commands/doctor.rs` | ✅ fixed |
| DOC-16 | Duplicated doc comment (LLM artifact) | Documentation | `embedder/mod.rs:23` | ✅ fixed |
| DOC-17 | Wrong file path in plan.rs checklist | Documentation | `plan.rs:151` | ✅ fixed |
| DOC-18 | ROADMAP says v1.3.0 | Documentation | `ROADMAP.md:3` | ✅ fixed |
| DOC-19 | MOONSHOT 769-dim → 768 | Documentation | `docs/MOONSHOT.md:21` | ✅ fixed |
| DOC-20 | MOONSHOT notes behavior stale | Documentation | `docs/MOONSHOT.md:17` | ✅ fixed |
| DOC-21 | notes.toml 769-dim | Documentation | `docs/notes.toml:300` | ✅ fixed |
| PB-20 | Doc references `normalize_origin()` | Documentation | `store/helpers.rs:197` | ✅ fixed |
| AD-23 | `GateLevel`/`GateThreshold` duplicate enums | API Design | `cli/mod.rs`, `ci.rs` | ✅ fixed |
| AD-24/CQ-20 | `cmd_index` 10 params → IndexArgs struct | API Design | `cli/commands/index.rs` | ✅ fixed |
| AD-25 | `PlanResult` missing `Clone` | API Design | `plan.rs:23` | ✅ fixed |
| AD-26 | `GatherOptions` missing `Clone` | API Design | `gather.rs:26` | ✅ fixed |
| AD-27 | `GatherResult` missing `Clone` | API Design | `gather.rs:244` | ✅ fixed |
| AD-29 | `gather`/`related` return `StoreError` not `AnalysisError` | API Design | `gather.rs`, `related.rs` | ✅ fixed |
| OB-15 | `llm/batch.rs` zero tracing spans | Observability | `llm/batch.rs` | ✅ fixed |
| OB-16 | `upsert_chunks_and_calls` missing span | Observability | `store/chunks/crud.rs` | ✅ fixed |
| OB-17 | 5 batch store methods missing spans | Observability | `store/chunks/crud.rs` | ✅ fixed |
| OB-18 | `compute_hints_with_graph` no span | Observability | `impact/hints.rs:19` | ✅ fixed |
| RB-14 | Bare `.unwrap()` convention | Robustness | `train_data/query.rs` | ✅ fixed |
| PB-19 | Predictable temp filenames | Platform | `doc_writer/rewriter.rs` | ✅ fixed |
| SEC-11 | Batch ID not validated before storage | Security | `llm/batch.rs` | ✅ fixed |
| SEC-12 | Absolute path bypass in `read_context_lines` | Security | `cli/display.rs` | ✅ fixed |
| EH-19 | `find_type_impacted` no degraded signal | Error Handling | `impact/analysis.rs` | ✅ fixed |
| EH-21 | BM25 corpus parse errors swallowed | Error Handling | `train_data/mod.rs` | ✅ fixed |
| AC-10 | Language filter case sensitivity | Algorithm | `search/scoring/filter.rs` | ✅ fixed |
| AC-15 | Token budget overshoot | Algorithm | `cli/commands/task.rs` | ✅ fixed |
| EX-18 | Test name generation hardcoded | Extensibility | `impact/analysis.rs` | ✅ fixed |
| EX-20 | Positional index fallback | Extensibility | `plan.rs:326` | ✅ fixed |
| PB-18 | Missing Windows python/7z names | Platform | `convert/pdf.rs`, `convert/chm.rs` | ✅ fixed |
| TC-19 | `resolve_target` zero tests | Test Coverage | `search/mod.rs` | ✅ fixed (4 tests) |
| TC-20 | `compute_risk_batch` zero tests | Test Coverage | `impact/hints.rs` | ✅ fixed (6 tests) |
| TC-21 | JSONL parsing untested | Test Coverage | `llm/mod.rs` | ✅ fixed (9 tests) |
| TC-22 | LLM chunk filtering untested | Test Coverage | `llm/summary.rs` | ✅ fixed (6 tests) |
| TC-23 | Diff impact `via` attribution untested | Test Coverage | `impact/diff.rs` | ✅ existing test |
| RM-24 | Watch HNSW no idle eviction | Resource Mgmt | `cli/watch.rs` | ✅ fixed |
| RM-27 | BatchContext LRU 4 → 2 | Resource Mgmt | `cli/batch/mod.rs` | ✅ fixed |
| DS-19 | `INSERT OR REPLACE` FK cascade | Data Safety | `store/chunks/crud.rs` | ✅ documented |

**37/37 fixed.**

---

## P4: Hard or low impact — defer / create issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| CQ-19 | `extract_patterns` 383-line match | Code Quality | `where_to_add.rs` | ✅ fixed (383→109 lines) |
| PERF-23 | `test_reachability` per-test BFS | Performance | `impact/bfs.rs` | ✅ fixed (equiv classes) |
| RM-23 | `enrichment_pass` pre-loads ~105MB (documented) | Resource Mgmt | `cli/enrichment.rs` | deferred |
| RM-25 | `search_across_projects` no concurrency cap | Resource Mgmt | `project.rs` | ✅ fixed (4-thread cap) |
| RM-26 | CAGRA dataset ~147MB (cuVS upstream) | Resource Mgmt | `cagra.rs` | existing #389 |
| AC-14 | `saturating_add` overflow | Algorithm | `impact/diff.rs` | ✅ fixed (checked_add) |
| PB-17 | Forward-slash invariant undocumented | Platform | `staleness.rs` | ✅ fixed (debug_assert) |
| PB-21 | WSL poll-mode misses UNC paths | Platform | `cli/watch.rs` | ✅ fixed |
| DS-17 | GC prune individual transactions | Data Safety | `cli/commands/gc.rs` | informational |
| DS-18 | `function_calls` no FK cascade | Data Safety | `schema.sql` | informational |
| EH-22 | Missing metadata keys pass checks | Error Handling | `store/mod.rs` | ✅ documented |
| SEC-13 | `callable_sql_list()` interpolation | Security | `language/mod.rs` | ✅ fixed (debug_assert) |

**8/12 fixed, 2 informational, 2 deferred (upstream/documented).**

---

## Summary

| Priority | Count | Fixed | Remaining |
|----------|-------|-------|-----------|
| P1 | 10 | **10** | 0 |
| P2 | 15 | **15** | 0 |
| P3 | 37 | **37** | 0 |
| P4 | 12 | **8** | 4 (2 informational, 2 deferred) |
| **Total** | **74** | **70** | **4** |

**Tests:** 1916 pass, 0 fail, 0 warnings (up from 1095 pre-audit)
