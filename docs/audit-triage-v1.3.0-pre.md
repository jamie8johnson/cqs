# Audit Triage — v1.0.13

Date: 2026-03-18. 99 findings across 14 categories, 3 batches.

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | RB-7: `build_prompt` byte slice panics on CJK/multi-byte content | Robustness | llm.rs:115 | ✅ fixed |
| 2 | SEC-5/SEC-6: Stored batch_id unsanitized in URL + reqwest forwards API key on redirect | Security | llm.rs:175,104 | ✅ fixed |
| 3 | DS-11: `set_hnsw_dirty(true).ok()` silently fails, defeating crash safety | Data Safety | watch.rs:346 | ✅ fixed |
| 4 | SEC-9: Unbounded `batch_items` accumulation — OOM on large index | Security | llm.rs:368 | ✅ fixed |
| 5 | EH-8: `Client::new()` panics via `.expect()` in library code | Error Handling | llm.rs:107 | ✅ fixed |
| 6 | AC-7: `waterfall_pack` impact/placement overshoot not capped | Algorithm | task.rs:201,228 | ✅ fixed |
| 7 | DS-8/PB-11: `audit.rs` copy-fallback writes directly to final path (not atomic) | Data Safety | audit.rs:125 | ✅ fixed |
| 8 | SEC-8: `libc::atexit` cleanup allocates during teardown — deadlock risk | Security | embedder.rs:816 | ✅ fixed |
| 9 | PB-14/RM-13: `OnceLock::set()` only cleans first directory's symlinks | Platform/Resource | embedder.rs:801 | ✅ fixed |
| 10 | EH-10: `set_pending_batch_id` errors silently swallowed — duplicate API calls | Error Handling | llm.rs:332,463,500 | ✅ fixed |
| 11 | RB-11: CAGRA `search()` with k=0 — zero-sized GPU buffers | Robustness | cagra.rs:240 | ✅ fixed |
| 12 | DOC-8/DOC-9: Schema version says v12, actual is v14 | Documentation | README.md:35, CONTRIBUTING.md:125 | ✅ fixed |
| 13 | EX-10: plan.rs "Add ChunkType" checklist stale after macro consolidation | Extensibility | plan.rs:148 | ✅ fixed |
| 14 | EX-12: `DeadConfidence` / `DeadConfidenceLevel` duplicate enums | Extensibility | calls.rs:31, cli/mod.rs:127 | ✅ fixed |
| 15 | DOC-13: CHANGELOG missing entries for PRs #605 and #613 | Documentation | CHANGELOG.md:8 | ✅ fixed |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | SEC-7: HNSW bincode deserialization from untrusted files — OOM via crafted lengths | Security | hnsw/persist.rs:414 | ✅ fixed |
| 2 | DOC-11: SECURITY/PRIVACY claim offline-only — false with `--llm-summaries` | Documentation | SECURITY.md:36, PRIVACY.md:5 | ✅ fixed |
| 3 | DOC-10: README documents `--json` for commands that use `--format json` | Documentation | README.md:160,207,213 | ✅ fixed |
| 4 | RB-8/RB-9: `to_uppercase()` byte offset on original string — panics on non-ASCII SQL | Robustness | parser/chunk.rs:130,200 | ✅ fixed |
| 5 | RB-10: `byte_offset_to_point` slices at unchecked byte offset | Robustness | parser/injection.rs:147, aspx.rs:96 | ✅ fixed |
| 6 | EH-13/EH-14: `anyhow::Result` in project.rs and llm.rs library code | Error Handling | project.rs, llm.rs | ✅ fixed |
| 7 | CQ-7: `search_filtered` / `search_by_candidate_ids` duplicate ~120 lines post-scoring | Code Quality | search.rs:900,1099 | ✅ fixed |
| 8 | CQ-8: Test detection logic divergent across 3 places | Code Quality | lib.rs:210, search.rs:482, calls.rs:117 | ✅ fixed |
| 9 | AD-16: Missing `serialize_path_normalized` on 12+ PathBuf fields | API Design | impact/types.rs, drift.rs, review.rs, etc. | ✅ fixed |
| 10 | PB-8/PB-9/PB-15: ORT provider code `#[cfg(unix)]` should be `#[cfg(target_os = "linux")]` | Platform | embedder.rs:686,719,760 | ✅ fixed |
| 11 | PB-13/DS-12: Notes path normalization mismatch with chunks | Platform/Data | store/notes.rs:105,195,249 | ✅ fixed |
| 12 | DS-7: LLM batch resume can skip newly-added chunks | Data Safety | llm.rs:461 | ✅ fixed |
| 13 | DS-10: Failed batch status check submits duplicate — wastes API credits | Data Safety | llm.rs:491 | ✅ fixed |
| 14 | AC-8: CAGRA distance-to-similarity incorrect for non-unit-norm (sentiment) | Algorithm | cagra.rs:328 | ✅ documented |
| 15 | OB-8: `llm_summary_pass` uses 13 `eprint!/eprintln!` in library code | Observability | llm.rs:312+ | ✅ fixed |
| 16 | RM-15: `PRAGMA quick_check` on every Store open — 20-50ms per command | Resource Mgmt | store/mod.rs:290 | ✅ fixed |
| 17 | EH-15: `anyhow::Result` in config.rs write functions | Error Handling | config.rs:285,372 | ✅ fixed |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | AD-13: `OnboardEntry.language` String → Language enum | API Design | onboard.rs:57 | ✅ fixed |
| 2 | AD-14: `DiffEntry`/`DiffResult` missing Serialize/Clone | API Design | diff.rs:14,27 | ✅ fixed |
| 3 | AD-15: `TestSuggestion.suggested_file` String → PathBuf | API Design | impact/types.rs:120 | ✅ fixed |
| 4 | AD-17: `ChunkIdentity.origin` String → PathBuf | API Design | store/helpers.rs:335 | ✅ fixed |
| 5 | AD-18: `StaleFile.origin` String → PathBuf | API Design | store/helpers.rs:402 | ✅ fixed |
| 6 | AD-19: `SuggestedNote` missing Serialize/Clone | API Design | suggest.rs:24 | ✅ fixed |
| 7 | AD-20: Inconsistent Clone derives across 13+ result types | API Design | multiple | ✅ fixed |
| 8 | AD-21: `GatherDirection` missing Serialize | API Design | gather.rs:89 | ✅ fixed |
| 9 | AD-22: `ReferenceIndex` missing Debug | API Design | reference.rs:17 | ✅ fixed |
| 10 | CQ-9: `get_enrichment_hash` dead code (zero callers) | Code Quality | store/chunks.rs:162 | ✅ already removed |
| 11 | CQ-10: `NlTemplate` 7 unused variants | Code Quality | nl.rs:238 | ✅ fixed |
| 12 | CQ-12: `get_by_content_hash` superseded by batch variant | Code Quality | store/chunks.rs:620 | ✅ fixed |
| 13 | CQ-13: Test fixture `setup_store()`/`mock_embedding()` copy-pasted 4+ times | Code Quality | search.rs, chunks.rs, calls.rs, etc. | deferred |
| 14 | CQ-14: `update_embeddings_batch` strict subset of `_with_hashes` variant | Code Quality | store/chunks.rs:83,122 | ✅ already delegated |
| 15 | OB-9: 14 `tracing::warn!` calls use positional format instead of structured | Observability | multiple (14 sites) | ✅ fixed |
| 16 | OB-10: `embed_documents` missing outer span for multi-batch | Observability | embedder.rs:400 | ✅ fixed |
| 17 | OB-11: `HnswIndex::search` missing tracing span | Observability | hnsw/search.rs:24 | ✅ fixed |
| 18 | OB-12: `set_hnsw_dirty().ok()` — missing warn on failure (7 sites) | Observability | watch.rs, gc.rs, index.rs | ✅ fixed |
| 19 | OB-13: `index_notes` uses info! but no info_span! | Observability | lib.rs:291 | ✅ fixed |
| 20 | OB-14: `review.rs` warn via pre-formatted message string | Observability | review.rs:136,149 | ✅ fixed |
| 21 | DOC-12: SECURITY.md Write Access missing `.cqs.toml` and `projects.toml` | Documentation | SECURITY.md:63 | ✅ fixed |
| 22 | DOC-14: README missing `--llm-summaries` flag | Documentation | README.md:476 | ✅ fixed |
| 23 | EH-11: `get_summaries_by_hashes` error swallowed in enrichment | Error Handling | pipeline.rs:996 | ✅ fixed |
| 24 | EH-16: `convert_directory` ignores top-level `read_dir` failure | Error Handling | convert/mod.rs:333 | ✅ fixed |
| 25 | PB-10: symlink path comparison without canonicalization | Platform | embedder.rs:783 | ✅ fixed |
| 26 | PB-12: `ref list --json` uses `to_string_lossy` without normalize | Platform | reference.rs:183 | ✅ fixed |
| 27 | EX-6: `Pattern` enum 4 manual sync points | Extensibility | structural.rs:10 | deferred |
| 28 | EX-7: `capture_name_to_chunk_type` manual sync point | Extensibility | parser/types.rs:18 | deferred |
| 29 | PERF-11: `upsert_summaries_batch` per-row INSERT | Performance | store/chunks.rs:258 | deferred |
| 30 | PERF-13: `llm_summary_pass` clones full content per chunk | Performance | llm.rs:434 | deferred |
| 31 | PERF-15: `apply_parent_boost` clones strings into HashMap | Performance | search.rs:534 | wontfix (borrow conflict, negligible savings) |
| 32 | PERF-16: `MODEL.to_string()` allocated per batch item | Performance | llm.rs:135 | deferred |
| 33 | PERF-17: per-candidate `.to_lowercase()` in filter check | Performance | search.rs:1057 | ✅ fixed |
| 34 | PERF-18: summaries fetched per page without caching | Performance | pipeline.rs:994 | ✅ already pre-fetched |
| 35 | RM-11: `embed_documents` prefixed copy of all strings upfront | Resource Mgmt | embedder.rs:402 | ✅ already per-batch |
| 36 | RM-12: CAGRA redundant host array allocation per search | Resource Mgmt | cagra.rs:240 | ✅ fixed |
| 37 | RM-16: HNSW id_map serialized to in-memory JSON string | Resource Mgmt | hnsw/persist.rs:189 | ✅ already streaming |
| 38 | RM-17: watch mode mtime pruning skips multi-file batches | Resource Mgmt | watch.rs:355 | ✅ fixed |
| 39 | RM-14: `Store::open` multi-threaded tokio with all cores | Resource Mgmt | store/mod.rs:221 | ✅ fixed |

## P4: Hard or Low Impact — Create Issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | PERF-12: CAGRA rebuilds index from scratch after every search | Performance | cagra.rs:285 | |
| 2 | PERF-14/RM-19/RM-20: Enrichment pass loads full call graph + identities upfront | Perf/Resource | pipeline.rs:939 | |
| 3 | CQ-11: `Store::open` / `open_readonly` 80% duplication | Code Quality | store/mod.rs:219,315 | |
| 4 | EX-8: CLI/batch command arg duplication across 30 commands | Extensibility | cli/mod.rs, batch/commands.rs | |
| 5 | EX-9: LLM config compile-time constants, no env/config override | Extensibility | llm.rs:17 | |
| 6 | EX-11: Search scoring constants scattered across 3 files | Extensibility | search.rs, store/mod.rs, helpers.rs | |
| 7 | DS-9: Watch mode Store never re-opened — OnceLock caches stale | Data Safety | watch.rs:135 | |
| 8 | RM-18: BatchContext refs accumulate without eviction | Resource Mgmt | batch/mod.rs:79 | |
| 9 | TC-8: LLM summary store functions zero tests | Test Coverage | store/chunks.rs:214 | ✅ 14 tests |
| 10 | TC-9: Schema migrations v12→v14 zero tests | Test Coverage | store/migrations.rs:134 | ✅ 3 tests |
| 11 | TC-10: `set_hnsw_dirty` / `is_hnsw_dirty` zero tests | Test Coverage | store/mod.rs:769 | ✅ 3 tests |
| 12 | TC-11: `chunks_paged` zero tests | Test Coverage | store/chunks.rs:1160 | ✅ 4 tests |
| 13 | TC-12: `extract_first_sentence` edge cases (URLs with periods) | Test Coverage | llm.rs:542 | ✅ 8 tests |
| 14 | TC-13: `Store::open_readonly` zero tests | Test Coverage | store/mod.rs:315 | ✅ 3 tests |
| 15 | TC-14: `watch.rs` zero tests (671 lines) | Test Coverage | cli/watch.rs | ✅ 4 tests |
| 16 | TC-15: Notes store 1 test / 8 functions | Test Coverage | store/notes.rs | ✅ 8 tests |
| 17 | TC-16: `cached_notes_summaries` cache invalidation untested | Test Coverage | store/mod.rs:831 | ✅ 2 tests |

## Summary

| Priority | Count | Key themes |
|----------|-------|-----------|
| P1 | 15 | Panics (RB-7, EH-8, RB-11), security (SEC-5/6/8/9), data safety (DS-11, DS-8), algorithm (AC-7) |
| P2 | 17 | Security (SEC-7), doc accuracy (DOC-10/11), parser panics (RB-8/9/10), error types, path normalization |
| P3 | 39 | Type consistency (AD-*), dead code (CQ-9/10/12), observability (OB-*), minor perf, derives |
| P4 | 17 | Test coverage (TC-*), major refactors (CQ-11, EX-8), CAGRA perf (PERF-12), config (EX-9) |
| **Total** | **88** | (99 findings, 11 overlaps consolidated) |
