# Audit Triage — v1.0.0

Date: 2026-03-12. 100 findings across 14 categories, 3 batches.

## P1: Easy + High Impact — Fix Immediately

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | PB-1: `.cqs/.gitignore` omits HNSW files — users commit 50-500MB binaries | Platform | init.rs:37-41 | ✅ fixed |
| 2 | RB-1/RB-2: `cached_notes_summaries()` panics on poisoned RwLock; invalidate silently no-ops | Robustness | store/mod.rs:748-770 | ✅ fixed |
| 3 | SEC-1/RB-5: `assert!` in FTS query path crashes process instead of returning error | Security/Robustness | store/mod.rs:623, chunks.rs:1189 | ✅ fixed |
| 4 | EH-3: `cmd_doctor` prints "All checks passed." even on failure | Error Handling | doctor.rs:129 | ✅ fixed |
| 5 | SEC-3: CHM/PDF paths passed to external processes without `--` end-of-options | Security | chm.rs:31, pdf.rs:22 | ✅ fixed |
| 6 | DS-2: `schema_version` parse failure defaults to 0, bypasses migration guard | Data Safety | store/mod.rs:442-459 | ✅ fixed |
| 7 | AC-3: `bfs_expand` depth check silently skips expansion for cross-index seeds at depth 1 | Algorithm | gather.rs:197 | ✅ fixed |
| 8 | RB-3: Reranker stride=0 bypasses bounds check, panics on empty data | Robustness | reranker.rs:147-163 | ✅ fixed |
| 9 | RB-4: `embedder.rs` panics via `outputs["last_hidden_state"]` if custom ONNX model | Robustness | embedder.rs:538 | ✅ fixed |
| 10 | RB-6: Zero-vector embeddings produce NaN cosine distances, corrupt HNSW graph | Robustness | hnsw/build.rs | ✅ fixed |
| 11 | SEC-2: `validate_ref_name` permits dot-prefixed names (`.git`, `.cqs`) | Security | reference.rs:214 | ✅ fixed |
| 12 | RM-8: `hnsw.lock` not in `HNSW_ALL_EXTENSIONS` — survives `cqs gc --delete` | Resource Mgmt | hnsw/persist.rs | ✅ fixed |
| 13 | DS-1: `add/remove_reference_to_config` cross-device copy fallback is non-atomic | Data Safety | config.rs:347-357,420-430 | ✅ fixed |

## P2: Medium Effort + High Impact — Fix in Batch

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | DOC-1: Model download size "~440MB" → actual "~547MB" in SECURITY.md + PRIVACY.md | Documentation | SECURITY.md:39, PRIVACY.md:27 | ✅ fixed |
| 2 | DS-5: `cmd_index` + `run_index_pipeline` open two Stores on same DB simultaneously | Data Safety | pipeline.rs:781, index.rs:68-80 | ✅ fixed |
| 3 | DS-6: Migration commits before model-version check — schema upgraded but Store rejected | Data Safety | store/mod.rs:459-476 | ✅ fixed |
| 4 | EH-1/EH-2: `impact/`, `review.rs`, `gather.rs`, `health.rs`, `suggest.rs`, `ci.rs` use `anyhow::Result` in library code | Error Handling | multiple (14 sites) | ✅ fixed |
| 5 | PERF-3: `upsert_chunks_and_calls` duplicates ~120 lines of chunk-upsert logic | Performance | store/chunks.rs | ✅ fixed |
| 6 | PERF-4: `gather_cross_index` fires N brute-force scans instead of HNSW search | Performance | gather.rs | ✅ fixed |
| 7 | AC-2: `waterfall_pack` surplus propagation charges overshoot to downstream sections | Algorithm | task.rs:144-146 | ✅ fixed |
| 8 | CQ-4: SQLite placeholder builder duplicated ~20 times across store/ | Code Quality | store/ (22 sites) | ✅ fixed |
| 9 | RM-6: `verify_checksum` reads full 547MB ONNX model on every startup | Resource Mgmt | embedder.rs | ✅ fixed |
| 10 | RM-9: `embed_documents` has no batch-size cap — unbounded GPU memory | Resource Mgmt | embedder.rs | ✅ fixed |
| 11 | RM-5: `Store::open` creates multi-threaded tokio runtime unnecessarily | Resource Mgmt | store/mod.rs | deferred (pipeline needs multi-thread) |
| 12 | AD-1: `file` field type String vs PathBuf inconsistent across 6+ public types | API Design | impact/types.rs, review.rs, ci.rs, drift.rs, diff.rs | ✅ fixed |
| 13 | PERF-7: `embed_batch` mean-pooling uses scalar loops instead of ndarray SIMD | Performance | embedder.rs | ✅ fixed |
| 14 | SEC-4: FTS5 MATCH sanitizer is sole injection barrier — no fuzz tests | Security | store/mod.rs:627, chunks.rs:1193 | ✅ fixed |
| 15 | CQ-6: `cmd_query` is 270 lines handling 4 dispatch paths | Code Quality | query.rs:40-303 | ✅ fixed |

## P3: Easy + Low Impact — Fix If Time

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | DOC-2: `store/mod.rs` module comment missing `types` and `migrations` submodules | Documentation | store/mod.rs:6-11 | ✅ fixed |
| 2 | DOC-3: README config example missing `ef_search`, `stale_check`, `note_only` | Documentation | README.md:112-128 | ✅ fixed |
| 3 | DOC-4: `--no-demote` search flag undocumented in README | Documentation | README.md | ✅ fixed |
| 4 | DOC-5: SECURITY.md Write Access table missing `audit-mode.json` | Documentation | SECURITY.md:63-72 | ✅ fixed |
| 5 | DOC-6: `cqs completions` command not documented in README | Documentation | README.md | ✅ fixed |
| 6 | DOC-7: `store/mod.rs` misleading "sync wrappers" comment | Documentation | store/mod.rs:1-4 | ✅ fixed |
| 7 | CQ-1: `resolve_reference_store` / `_readonly` near-identical | Code Quality | resolve.rs:44-99 | ✅ fixed |
| 8 | CQ-2: Random temp-file suffix pattern duplicated 5 times | Code Quality | audit.rs, config.rs, note.rs, project.rs | ✅ fixed |
| 9 | CQ-3: `DeadConfidence` → `&str` mapping repeated in 3 locations | Code Quality | dead.rs, ci.rs, handlers.rs | ✅ fixed |
| 10 | CQ-5: `diff.rs` duplicates `full_cosine_similarity` tests from `math.rs` | Code Quality | diff.rs:206-237 | ✅ fixed |
| 11 | EH-4: `reference list` swallows `Store::open` errors, shows `0` chunks | Error Handling | reference.rs:172-196 | ✅ fixed |
| 12 | EH-5: `convert` module silently skips `walkdir` errors in 6 locations | Error Handling | convert/mod.rs, chm.rs, webhelp.rs | ✅ fixed |
| 13 | EH-6: `audit.rs` swallows JSON parse error with bare `Err(_)` | Error Handling | audit.rs:75 | ✅ fixed (prior) |
| 14 | EH-7: `parse_duration` drops `ParseIntError` from `map_err(|_| ...)` | Error Handling | audit.rs:161,177,198 | ✅ fixed |
| 15 | OB-1: `process_file_changes` has no tracing span | Observability | watch.rs:295 | ✅ fixed |
| 16 | OB-2: `find_pdf_script` double-emits to `eprintln!` and `tracing::warn!` | Observability | pdf.rs:57-58 | ✅ fixed |
| 17 | OB-3: `cmd_query` span captures only `query_len`, not query text | Observability | query.rs:41 | ✅ fixed |
| 18 | OB-4: `semantic_diff` span missing source/target/threshold | Observability | diff.rs:79 | ✅ fixed |
| 19 | OB-5: `--rerank` warning uses `eprintln!` instead of `tracing::warn!` | Observability | query.rs:248-251 | ✅ fixed |
| 20 | OB-6: `convert` module missing bytes/duration metrics | Observability | convert/mod.rs, pdf.rs, html.rs | deferred |
| 21 | OB-7: `reindex_files` warn uses positional format, not structured fields | Observability | watch.rs:500 | ✅ fixed |
| 22 | AD-2: `chunk_type` String in `OnboardEntry`/`DriftEntry` when enum exists | API Design | onboard.rs:58, drift.rs:17 | ✅ fixed |
| 23 | AD-3: `ChunkRole` Serialize PascalCase vs `as_str()` snake_case | API Design | scout.rs:15-34 | ✅ fixed |
| 24 | AD-4: `DeadInDiff.confidence` is String when `DeadConfidence` has Serialize | API Design | ci.rs:46 | ✅ fixed |
| 25 | AD-5: `DiffEntry` not re-exported despite being in public `DiffResult` | API Design | lib.rs:104, diff.rs:14 | ✅ fixed |
| 26 | AD-6: `review::NoteEntry` not exported, name-collides with `note::NoteEntry` | API Design | lib.rs, review.rs:50 | ✅ fixed |
| 27 | AD-7: `FileSuggestion::to_json()` silently omits `patterns` field | API Design | where_to_add.rs:54-62 | ✅ fixed |
| 28 | AD-8: `suggest_placement_with_embedding` redundant | API Design | where_to_add.rs:125-136 | ✅ fixed |
| 29 | AD-9: `TaskResult.risk` anonymous tuple — inconsistent serialization | API Design | task.rs:37 | ✅ fixed |
| 30 | AD-10: `ScoutResult.relevant_notes` `#[serde(skip)]` but in `scout_to_json()` | API Design | scout.rs:83-85 | ✅ fixed |
| 31 | AD-11: `ModelInfo` missing `Debug`, `Clone`, `Serialize` | API Design | store/helpers.rs:591 | ✅ fixed |
| 32 | AD-12: `score_name_match_pre_lower` not exported despite doc recommending it | API Design | store/helpers.rs:668 | ✅ fixed |
| 33 | AC-1: `ef_search` cap formula doesn't enforce index-size bound (harmless) | Algorithm | hnsw/search.rs:41-44 | ✅ fixed |
| 34 | AC-4: Snippet window asymmetry when `call_line == line_start` | Algorithm | impact/analysis.rs:143-145 | ✅ fixed |
| 35 | AC-5: `reverse_bfs` depth-0 invariant undocumented | Algorithm | impact/bfs.rs:15 | ✅ fixed |
| 36 | AC-6: `token_pack` first-item guarantee can exceed budget — no warning | Algorithm | commands/mod.rs:135, task.rs:62 | ✅ fixed |
| 37 | PB-3: `is_wsl()` should check `WSL_DISTRO_NAME` env var first | Platform | config.rs:17-27 | ✅ fixed |
| 38 | PB-5: WSL poll detection prefix-based, not filesystem-based (doc only) | Platform | watch.rs:67-72 | ✅ fixed |
| 39 | PB-7: `ensure_ort_provider_libs` silently skips GPU when `LD_LIBRARY_PATH` unset | Platform | embedder.rs:685-700 | ✅ fixed |
| 40 | PERF-1: SQL placeholder rebuilt on every batch iteration (22 sites) | Performance | chunks.rs, calls.rs, types.rs | |
| 41 | PERF-2: `search_by_names_batch` post-filter O(results × batch_names) | Performance | store/mod.rs | |
| 42 | PERF-5: `prune_missing` builds identical placeholder string twice | Performance | store/chunks.rs | ✅ fixed (prior) |
| 43 | PERF-6: Test SQL rebuilt dynamically on every cold cache call | Performance | store/calls.rs | ✅ fixed |
| 44 | PERF-8: `sanitize_fts_query` allocates two intermediate strings always | Performance | store/mod.rs | ✅ fixed |
| 45 | PERF-9: `strip_markdown_noise` applies 6 regex replacements unconditionally | Performance | markdown parser | ✅ fixed |
| 46 | PERF-10: `find_dead_code` runs two full-table scans — should UNION | Performance | store/calls.rs | ✅ fixed |
| 47 | DS-3: `ProjectRegistry::load()` TOCTOU — size check and read are separate | Data Safety | project.rs:32-51 | ✅ fixed |
| 48 | DS-4: `call_graph_cache`/`test_chunks_cache` OnceLock — no invalidation | Data Safety | store/mod.rs, calls.rs | ✅ fixed |
| 49 | RM-1: `HnswIndex::build` doubles peak memory (flat buffer + Vec coexist) | Resource Mgmt | hnsw/build.rs:57-79 | |
| 50 | RM-2: `count_vectors` deserializes full id map to count entries | Resource Mgmt | hnsw/persist.rs | ✅ fixed |
| 51 | RM-4: Watch mode holds old + new HNSW index simultaneously | Resource Mgmt | cli/watch.rs | |
| 52 | RM-7: `BatchContext` OnceLock caches not cleared during idle | Resource Mgmt | cli/batch/mod.rs | |
| 53 | RM-10: `reindex_files` O(files × total_calls) in watch mode | Resource Mgmt | cli/watch.rs | |
| 54 | EX-1: `CHUNK_CAPTURE_NAMES` is third sync point for ChunkType | Extensibility | parser | |
| 55 | EX-2: `Pattern::FromStr` error hardcodes valid names | Extensibility | parser | ✅ fixed |
| 56 | EX-3: `--chunk-type` CLI help lists 11/16 variants | Extensibility | cli/mod.rs | ✅ fixed |
| 57 | EX-4: `nl.rs` hardcodes `"typealias"` multi-word workaround | Extensibility | nl.rs | ✅ fixed |
| 58 | PB-2: `7z -o` uses `Path::display()` — lossy on non-UTF-8 | Platform | chm.rs:30 | ✅ fixed (prior) |
| 59 | PB-4: `find_pdf_script` relative to CWD, not project root | Platform | pdf.rs:72-83 | ✅ fixed |
| 60 | PB-6: `chm_to_markdown` uses `to_string_lossy()` for paths | Platform | chm.rs:29 | ✅ fixed (prior) |

## P4: Hard or Test Coverage — Create Issues

| # | Finding | Category | Location | Status |
|---|---------|----------|----------|--------|
| 1 | RM-3: CAGRA GPU index retains full CPU-side dataset copy (existing #389) | Resource Mgmt | cagra.rs:64 | existing #389 |
| 2 | TC-1: `convert/html.rs`, `chm.rs`, `webhelp.rs` — zero tests | Test Coverage | convert/ | |
| 3 | TC-2: `suggest.rs` `high_risk` branch never exercised | Test Coverage | suggest.rs:141-151 | |
| 4 | TC-3: `health.rs` `untested_hotspots` never asserted | Test Coverage | health.rs:284-346 | |
| 5 | TC-4: `review.rs` `match_notes` partial-match edge cases | Test Coverage | review.rs:183-211 | |
| 6 | TC-5: `impact/diff.rs` depth-0 exclusion and BFS anomaly untested | Test Coverage | impact/diff.rs:168,181 | |
| 7 | TC-6: `related.rs` unit tests only test struct construction | Test Coverage | related.rs:170-234 | |
| 8 | TC-7: `convert/pdf.rs` `find_pdf_script` logic untested | Test Coverage | convert/pdf.rs | |
| 9 | EX-5: `find_project_root` hardcodes markers for 5/50 languages | Extensibility | lib.rs | |
