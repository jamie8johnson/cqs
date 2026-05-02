# Audit Triage (v1.33.0)

Total findings: 167 across 16 categories. Classified into P1 (fix immediately) / P2 (fix in batch) / P3 (fix if time) / P4 (issue for hard, inline fix for trivial).

## Summary by Priority

| Priority | Count | Description |
|----------|-------|-------------|
| P1 | 47 | Easy + high impact |
| P2 | 41 | Medium effort + high impact (some easy items batched here for category cohesion) |
| P3 | 56 | Easy + low impact |
| P4 | 23 | Hard or low impact (issues filed) |

## Cross-cutting themes

- **Lying-docs cluster (P1 by team rule, ~9 findings).** lib.rs eval claim, README env-var count, README "How It Works" preset list, Retrieval Quality table inconsistencies, SECURITY.md slot-layout drift (#1105), `serve/mod.rs` "no auth" docstring, CONTRIBUTING.md `LlmProvider` ref, Cargo.toml `encrypt` feature contradictory comment, SECURITY.md schema.sql line-range citation drift. Each one promises something the code doesn't deliver — all P1.
- **Convenience-wrapper-hardcodes-default cluster (~5 findings, classic configurable-models disaster shape).** `generate_nl_description` legacy 1-arg wrapper still on prod hot paths; `embed_documents` ignores `embed_batch_size_for(model)`; `model_repo()` discards override in `cmd_doctor`; SHL-V1.33-1 CagraBackend gate uses zero-arg `gpu_available()` instead of the corpus-aware `_for(n,dim)`; CQ-V1.33.0-7/8 dead `pub` shims that mask production breakage. Memory rule says wiring-verification regressions are P1.
- **Unbounded fs::read_to_string family (~7 findings).** `worktree.rs:.git`, `cli/config.rs:Cargo.toml`, `acquire_lock:PID file`, `audit.rs:audit-mode.json`, `add_reference_to_config`, `ProjectRegistry::load`, `parse_wsl_automount_root`, `centroid classifier`. Same RB-V1.30-2 pattern fixed repeatedly; new sites keep landing. All easy single-file caps — P1.
- **Footgun `pub` API surface (~3 findings, overlapping IDs).** `Store::search_embedding_only` is a `pub` wrapper whose own docstring says "use the other one"; `LlmReranker` is `pub use`-exported but every score call returns Err; `search_unified_with_index` is a 6-line wrapper post-SQ-9 that misleads. Visibility/delete moves are one-line P1s.
- **NaN/Inf passthrough cluster (~5 findings).** Knob config-override skips `is_finite`; `mmr_lambda_from_env` accepts NaN via `f32::parse`; BM25 division-by-zero on empty corpus; `update_umap_coords_batch` doesn't filter; `print_telemetry_text` divide-by-zero. All easy single-line filters — P1.
- **SQLite legacy batch-size cluster (5 findings).** type_edges INSERT_BATCH=249, chunks/embeddings.rs+query.rs BATCH_SIZE=500 (5 sites), staleness BATCH_SIZE=100 (3 sites), crud.rs INSERT_BATCH=300, async_helpers HASH_BATCH=500. All "use `max_rows_per_statement(N)` like the sibling already does" — easy P1 perf wins on the hot indexing path.
- **Concurrent migration / temp-file race cluster (~3 findings).** `migrate_legacy_index_to_default_slot` doesn't acquire `slots.lock` (DS-V1.33-1); `write_active_slot` / `write_slot_model` use deterministic `<file>.tmp` paths (DS-V1.33-2); `backup_path_for` uses 1-second timestamp resolution (DS-V1.33-5). DS-V1.33-1 is medium effort; DS-V1.33-2 and DS-V1.33-5 are easy single-line additions of `temp_suffix()` / PID. P1.
- **Windows path-handling regressions (PB-V1.33-1/2/3 + PB-V1.33-6).** `std::fs::canonicalize` returns `\\?\` UNC paths that `starts_with` comparisons reject; `dunce::canonicalize` already used in 16+ other sites. Three sites + a `.cache/huggingface` join — easy mechanical P1 fixes.
- **HNSW persist contract gap (DS-V1.33-3).** `verify_hnsw_checksums` skips files not on disk, so a partial save passes. Single-file fix, schema integrity. P1.
- **Telemetry/observability misses on hot paths (5 OB findings, mostly easy).** `Reranker::run_chunk`, `SpladeEncoder::encode`, `Embedder::embed_query` all missing elapsed_ms; `serve` `http_request` span lacks `request_id`. Most are P3 (easy + low operator-visibility impact); the watch-snapshot one is P2.

## P1 — Fix Immediately

| ID | Category | Title | Difficulty | Status |
|----|----------|-------|------------|--------|
| P1-1 | Documentation | lib.rs top-of-crate eval claim conflicts with README/Cargo.toml | easy | ✅ #1323 |
| P1-2 | Documentation | lib.rs Features list missing three current embedder presets | easy | ✅ #1323 |
| P1-3 | Documentation | README "How It Works" undersells the embedder preset roster | easy | ✅ #1323 |
| P1-4 | Documentation | README "Environment Variables" claims 120 knobs but table has 158 | easy | ✅ #1323 |
| P1-5 | Documentation | Retrieval Quality table inconsistent with TL;DR aggregate numbers | easy | ⬜ |
| P1-6 | Documentation | Retrieval Quality fixture table missing bge-large-ft and embeddinggemma-300m rows | easy | ⬜ |
| P1-7 | Documentation | SECURITY.md filesystem table doesn't reflect slot layout (#1105) | easy | ✅ #1323 |
| P1-8 | Documentation | serve/mod.rs module docstring says "No auth" — contradicts auth implementation | easy | ✅ #1323 |
| P1-9 | Documentation | CONTRIBUTING.md references nonexistent `LlmProvider` type | easy | ✅ #1323 |
| P1-10 | Documentation | Cargo.toml `encrypt` feature has contradictory inline comment | easy | ✅ #1323 |
| P1-11 | Documentation | CHANGELOG [Unreleased] missing 10+ post-v1.33.0 PRs that landed | easy | ✅ #1323 |
| P1-12 | Documentation | SECURITY.md cites schema.sql:180-187 — actual range is 185-192 | easy | ✅ #1323 |
| P1-13 | Documentation | cqs-bootstrap SKILL says "14-category code audit"; should be 16 | easy | ✅ #1323 |
| P1-14 | Error Handling | Three telemetry sites silently coerce pre-epoch clock to ts=0 | easy | ✅ #1329 |
| P1-15 | Error Handling | `set_on_item_complete` keeps bare `.unwrap()` on poisoned mutex | easy | ✅ #1329 |
| P1-16 | Error Handling | `cmd_install` hook silently masks PermissionDenied + clobbers foreign hooks | easy | ✅ #1329 |
| P1-17 | Error Handling | `enumerate_files` silently drops files whose metadata fails | easy | ✅ #1329 |
| P1-18 | Error Handling | `notes_acceptance_status` swallows note-parse error as `(None, None, None)` | easy | ✅ #1329 |
| P1-19 | Error Handling | `slot_promote` slot-missing message uses `unwrap_or_default()` | easy | ✅ #1329 |
| P1-20 | Robustness | BM25 division-by-zero on empty corpus produces NaN scores | easy | ✅ #1332 |
| P1-21 | Robustness | `worktree::resolve_main_project_dir` reads `.git` file unbounded | easy | ✅ #1328 |
| P1-22 | Robustness | `find_cargo_workspace_root` reads every parent `Cargo.toml` unbounded | easy | ✅ #1328 |
| P1-23 | Robustness | `acquire_lock` reads PID file unbounded — DoS lever via lock file | easy | ✅ #1328 |
| P1-24 | Robustness | `EmbeddingCache::scan_or_compute_batch` `Vec::with_capacity` underflow | easy | ✅ #1332 |
| P1-25 | Robustness | `print_telemetry_text` divide-by-zero when sessions==0 | easy | ✅ #1332 |
| P1-26 | Robustness | `cache_cmd::format_timestamp` UNIX_EPOCH addition can overflow on i64::MAX | easy | ✅ #1332 |
| P1-27 | Robustness | `splade.unwrap()` after None-guard — non-idiomatic unwrap in production | easy | ✅ #1332 |
| P1-28 | Code Quality | `generate_nl_description` legacy 1-arg wrapper still on watch + bulk hot paths | medium | ✅ #1330 |
| P1-29 | Code Quality | `embed_documents` inner batch loop hardcodes 64, ignores model dim/seq | easy | ✅ #1330 |
| P1-30 | Code Quality | `model_repo()` discards override and silently lies in `cmd_doctor` | easy | ✅ #1330 |
| P1-31 | Code Quality | `index_pack` uses `break` while `token_pack` uses `continue` (P1.18 mirror miss) | easy | ✅ #1335 |
| P1-32 | Code Quality | `search_embedding_only` `pub` wrapper with zero callers + self-warning footgun | easy | ✅ #1335 |
| P1-33 | Code Quality | `LlmReranker` exported `pub` but stub returns `Err` from every score call | easy | ✅ #1335 |
| P1-34 | Scaling | CagraBackend gate uses zero-arg `gpu_available()`, defeats P2.42 VRAM check | easy | ✅ #1330 |
| P1-35 | Scaling | type_edges INSERT_BATCH=249 still uses pre-2020 SQLite limit | easy | ✅ #1324 |
| P1-36 | Scaling | chunks/embeddings.rs and chunks/query.rs BATCH_SIZE=500 legacy SQLite limit | easy | ✅ #1324 |
| P1-37 | Scaling | chunks/staleness.rs BATCH_SIZE=100 across three sites also legacy | easy | ✅ #1324 |
| P1-38 | Scaling | chunks/crud.rs INSERT_BATCH=300 legacy size for calls table | easy | ✅ #1324 |
| P1-39 | Scaling | chunks/async_helpers.rs HASH_BATCH=500 still hardcoded | easy | ✅ #1324 |
| P1-40 | Algorithm Correctness | `extract_file_from_chunk_id` mishandles markdown table-window IDs (`:tNwM`) | easy | ✅ #1326 |
| P1-41 | Algorithm Correctness | L5X synthetic-routine chunk computes `line_end` with off-by-one | easy | ✅ #1326 |
| P1-42 | Algorithm Correctness | Brute-force scoring path uses unchecked `limit*3`/`limit*2` (no saturation) | easy | ✅ #1326 |
| P1-43 | Algorithm Correctness | Knob config-override path skips `is_finite` check; NaN flows into BM25/RRF | easy | ✅ #1326 |
| P1-44 | Algorithm Correctness | `is_name_like_query` short-circuits NL-word check for ≤2-token queries | easy | ✅ #1326 |
| P1-45 | Algorithm Correctness | HNSW `M`/`ef_construction`/`ef_search` env overrides accept zero | easy | ✅ #1326 |
| P1-46 | Algorithm Correctness | `mmr_lambda_from_env` accepts `NaN`/`Inf` strings, silently disables MMR | easy | ✅ #1326 |
| P1-47 | Algorithm Correctness | `SearchFilter` `include_types` ∩ `exclude_types` produces always-false WHERE | easy | ✅ #1326 |

## P2 — Fix in Batch

| ID | Category | Title | Difficulty | Status |
|----|----------|-------|------------|--------|
| P2-1 | Observability | `serve` axum `http_request` span has no `request_id` field | medium | ✅ #1362 |
| P2-2 | Observability | `WatchSnapshot::compute` and `now_unix_secs` lack tracing on freshness state machine | medium | ✅ #1362 |
| P2-3 | Error Handling | `embedder.fingerprint` silently uses `size = 0` when metadata fails — collides cache keys | medium | ⬜ |
| P2-4 | Error Handling | `IndexBackend` trait — public lib trait uses anyhow::Result instead of thiserror | medium | ⬜ |
| P2-5 | Error Handling | Reconcile mtime-touch chain silently abandons on metadata or `modified()` failure | medium | ✅ #1379 |
| P2-6 | Error Handling | Reference path canonicalize-failure in `Config::validate` skips SEC-4 + SEC-NEW-1 check | medium | ⬜ |
| P2-7 | Robustness | L5X parser line arithmetic uses unchecked u32+u32 — overflow panics in debug | medium | ✅ #1379 |
| P2-8 | Code Quality | `serve` async handlers duplicate 15-20 LOC of permit + spawn_blocking + span ×6 | medium | ⬜ |
| P2-9 | Scaling | HNSW M/ef defaults static, don't auto-scale with corpus | medium | ⬜ |
| P2-10 | TC Adversarial | `enumerate_files` symlink-skip / oversized-skip / non-UTF8-path branches untested | medium | ✅ #1333 |
| P2-11 | TC Adversarial | `CqParser::parse_file` non-UTF8 and oversized-file skip branches untested | medium | ✅ #1333 |
| P2-12 | TC Adversarial | `update_umap_coords_batch` accepts NaN/Inf coords; serializes as bare JSON `NaN` | medium | ✅ #1333 |
| P2-13 | API Design | Same `--depth` flag means four different defaults across five commands | medium | ⬜ |
| P2-14 | API Design | `--rerank` (bool) on search vs `--reranker <mode>` (enum) on eval | medium | ⬜ |
| P2-15 | Algorithm Correctness | `apply_rerank_scores` partial overwrite when `scores.len() != results.len()` | medium | ⬜ |
| P2-16 | Algorithm Correctness | SPLADE hybrid fusion truncates+re-collects into HashMap, scrambles ordering | medium | ⬜ |
| P2-17 | Algorithm Correctness | BM25 IDF formula uses non-standard `+1.0` (Atire) without docs; mismatches FTS5 | medium | ⬜ |
| P2-18 | Data Safety | `migrate_legacy_index_to_default_slot` does not acquire `slots.lock` | medium | ✅ #1327 |
| P2-19 | Data Safety | `write_active_slot`/`write_slot_model` use fixed `<file>.tmp` paths | easy | ✅ #1327 |
| P2-20 | Data Safety | `verify_hnsw_checksums` skips files not on disk — partial index passes verification | easy | ✅ #1325 |
| P2-21 | Data Safety | `EmbeddingCache::evict`/`QueryCache::evict` use deferred transactions | medium | ⬜ |
| P2-22 | Data Safety | `backup_path_for` uses 1-second timestamp with no PID — concurrent migrations collide | easy | ✅ #1327 |
| P2-23 | Data Safety | `evict_lock` reset on every `EmbeddingCache::open` — multiple opens don't share | medium | ⬜ |
| P2-24 | Data Safety | `clear_session` doesn't reset `detected_dim` or `model_fingerprint` | medium | ⬜ |
| P2-25 | Data Safety | Pool `after_connect` has no `wal_autocheckpoint` ceiling | medium | ⬜ |
| P2-26 | Data Safety | `migrate_legacy_index_to_default_slot` checkpoints before sentinel | easy | ✅ #1327 |
| P2-27 | Security | `apply_db_file_perms` runs after pool open — embedding cache born world-readable | easy | ✅ #1331 |
| P2-28 | Security | `ProjectRegistry::save` writes tmp with default umask; chmod after rename | easy | ✅ #1331 |
| P2-29 | Security | `write_model_toml` interpolates `repo` into TOML without escaping | easy | ✅ #1331 |
| P2-30 | Security | `audit-mode.json` parsed without size cap — `.cqs/`-write attacker can OOM cqs | easy | ✅ #1331 |
| P2-31 | Security | `dispatch_read` daemon handler hardcodes `trust_level: "user-code"` | medium | ⬜ |
| P2-32 | Resource Management | `add_reference_to_config`/`remove_reference_from_config` read locked TOML unbounded | easy | ✅ #1328 |
| P2-33 | Resource Management | `ProjectRegistry::load` reads file *then* checks size — full alloc before cap | easy | ✅ #1328 |
| P2-34 | Resource Management | `parse_wsl_automount_root`/`is_slow_mmap_filesystem` read system files unbounded | easy | ✅ #1328 |
| P2-35 | Resource Management | Centroid classifier file loaded with no size guard | easy | ✅ #1328 |
| P2-36 | Performance | `cache.rs::read_batch` decodes f32 blobs via `chunks_exact(4).map` — bytemuck zero-copy | easy | ⬜ |
| P2-37 | Performance | SQLite `chunks` missing composite index on `(source_type, origin)` | medium | ⬜ |
| P2-38 | Platform Behavior | SEC-4 reference-path containment uses `std::fs::canonicalize`, breaking on Windows | easy | ✅ #1328 |
| P2-39 | Platform Behavior | `train_data::git::validate_git_repo` uses raw `canonicalize()` on Windows | easy | ✅ #1328 |
| P2-40 | Platform Behavior | `worktree::resolve_main_project_dir` uses `std::fs::canonicalize` on `.git/` | easy | ✅ #1328 |
| P2-41 | Platform Behavior | `aux_model::hf_cache_dir` joins `".cache/huggingface"` as single component | easy | ✅ #1328 |

(P2 ended up at 41 because there are several easy single-line fixes in Data Safety / Security / RM / PB that are too high-impact for P3 but are batched together by category. P2-19/20/22/26/27/28/29/30/32/33/34/35/36/38/39/40/41 are all easy — but they cluster naturally as batched edits.)

## P3 — Fix If Time

| ID | Category | Title | Difficulty | Status |
|----|----------|-------|------------|--------|
| P3-1 | Observability | `Reranker::run_chunk` per-batch ONNX call has no tracing span | easy | ✅ #1362 |
| P3-2 | Observability | `SpladeEncoder::encode` debug-span lacks completion event with elapsed_ms | easy | ✅ #1362 |
| P3-3 | Observability | `Embedder::embed_query` cache-hit/miss completion event missing elapsed_ms | easy | ✅ #1362 |
| P3-4 | Observability | `notify` watcher errors swallow ErrorKind + paths fields | easy | ✅ #1362 |
| P3-5 | Observability | `cli/watch/events.rs:23` `collect_events` has no entry span | easy | ✅ #1362 |
| P3-6 | Observability | `cli/registry.rs:133` `println!` for Refresh "no daemon running" bypasses tracing | easy | ✅ #1362 |
| P3-7 | Observability | `Embedder::warm` no span, no log — silent ~250 MB+ session init at startup | easy | ✅ #1362 |
| P3-8 | Observability | `LocalProvider` worker threads lack worker-id field on completion | easy | ✅ #1362 |
| P3-9 | Robustness | `set_on_item_complete` lock().unwrap() — duplicate of EH-V1.33-2 | easy | ⬜ |
| P3-10 | Code Quality | `check_model_version()` wrapper dead in production | easy | ⬜ |
| P3-11 | Code Quality | `is_false`/`is_zero_usize` trivial helpers duplicated 3+2 times across modules | easy | ⬜ |
| P3-12 | Code Quality | `search_unified_with_index` is `pub` 6-line wrapper post-SQ-9 | easy | ✅ #1335 |
| P3-13 | Scaling | BM25 K1=1.2, B=0.75 hardcoded in train_data without rationale or env override | easy | ⬜ |
| P3-14 | Scaling | BM25 FTS5 column weights duplicated as inline SQL string at two sites | easy | ⬜ |
| P3-15 | Scaling | cagra build_from_store BATCH_SIZE=10_000 hardcoded, doesn't scale with dim | easy | ⬜ |
| P3-16 | Scaling | search_across_projects rayon thread cap unwrap_or(4) ignores host parallelism | easy | ⬜ |
| P3-17 | Scaling | convert/naming.rs title_to_filename has no length cap | easy | ⬜ |
| P3-18 | TC Adversarial | `search_filtered`/`search_filtered_with_index` with `limit=0` no test | easy | ✅ #1333 |
| P3-19 | TC Adversarial | `HnswIndex::search` with `k=0` untested | easy | ✅ #1333 |
| P3-20 | TC Adversarial | `SpladeIndex::search` with `k=0` and NaN/Inf weights untested | easy | ✅ #1333 |
| P3-21 | TC Adversarial | `rerank_with_passages` length-mismatch error branch untested | easy | ✅ #1333 |
| P3-22 | TC Adversarial | `QueryCache::get` malformed-blob auto-delete path untested | easy | ✅ #1333 |
| P3-23 | TC Adversarial | `parse_env_usize_clamped`/`parse_env_f32` zero tests despite 10+ callers | easy | ✅ #1333 |
| P3-24 | TC Adversarial | `validate_and_read_file` oversized-file branch untested | easy | ✅ #1333 |
| P3-25 | API Design | `cqs project register` lacks `--json` and skips JSON envelope | easy | ⬜ |
| P3-26 | API Design | `cqs notes add\|update\|remove` accept no `--json` at subcommand level | easy | ⬜ |
| P3-27 | API Design | `cqs slot`/`cqs cache` still advertise `--slot` even though it bails | easy | ⬜ |
| P3-28 | API Design | Public `Store::search_embedding_only` is `pub` footgun — visibility flip (overlaps P1-32) | easy | ✅ #1335 |
| P3-29 | API Design | `project register` vs `ref add` — same operation, two verbs | easy | ⬜ |
| P3-30 | API Design | `--json` declared inline on six commands instead of via shared `TextJsonArgs` | easy | ⬜ |
| P3-31 | API Design | `StoreError::SchemaMismatch(String, i32, i32)` uses positional fields | easy | ⬜ |
| P3-32 | TC Happy | `cqs convert` and `convert_path` have zero end-to-end tests | easy | ✅ #1333 |
| P3-33 | TC Happy | `cqs eval --reranker` flag (#1303) has zero CLI integration test | easy | ✅ #1333 |
| P3-34 | TC Happy | `cqs slot {create, remove, promote, list, active}` no CLI integration tests | easy | ✅ #1333 |
| P3-35 | TC Happy | `cmd_explain` CLI handler has no direct test | easy | ✅ #1333 |
| P3-36 | TC Happy | `cqs notes update --new-kind` and `--new-mentions` (#1278) no test coverage | easy | ✅ #1333 |
| P3-37 | TC Happy | `update_umap_coords_batch` (pub Store API) has zero tests | easy | ✅ #1333 |
| P3-38 | TC Happy | `OnnxReranker::with_section` config-path (P1.7) has zero tests | easy | ✅ #1333 |
| P3-39 | Resource Management | `train_data::git_diff_tree` captures unbounded subprocess stdout | easy | ⬜ |
| P3-40 | Resource Management | Atomic-write tmp files leak on intermediate write failure (config / notes) | easy | ⬜ |
| P3-41 | Performance | `output.push_str(&format!(...))` pattern allocates intermediate String 4-6× | easy | ⬜ |
| P3-42 | Performance | `SpladeIndex::search_with_filter` builds score `HashMap` with no capacity hint | easy | ⬜ |
| P3-43 | Performance | `extract_imports_regex` recompiles same `Regex` set on every `cqs where`/`task` call | easy | ⬜ |
| P3-44 | Performance | `Store::search_by_name` lowercases every chunk name even though only ~100 rows scored | easy | ⬜ |
| P3-45 | Performance | `gather::bridge_scores` HashMap clones `pr.chunk.name`/`id` per result | easy | ⬜ |
| P3-46 | Extensibility | `SearchResult::to_json` and `to_json_relative` duplicate 12-field JSON shape | easy | ⬜ |
| P3-47 | Extensibility | `BatchSubmitItem.context` is a stringly-typed bag — every prompt builder reinterprets | easy | ⬜ |
| P3-48 | Extensibility | `run_migration` is a 16-arm hand-coded match — adding migration v26 needs three edits | easy | ⬜ |
| P3-49 | Extensibility | Adding any new top-level CLI command needs three coordinated edits | easy | ⬜ |
| P3-50 | Platform Behavior | Daemon error/operator hints hardcode `systemctl --user` — broken UX for macOS | easy | ⬜ |
| P3-51 | Platform Behavior | `process_exists` (Windows) uses PATH lookup for `tasklist` | easy | ⬜ |
| P3-52 | API Design | Wildcard `pub use diff::* / gather::* / impact::* / scout::* / task::*` in lib.rs | medium | ⬜ |
| P3-53 | Performance | `Store::load_all_sparse_vectors` allocates fresh `String` per row | medium | ⬜ |
| P3-54 | Performance | `Embedder::embed_batch`/`Reranker::run_chunk` allocate three Vec<Vec<i64>> per batch | medium | ⬜ |
| P3-55 | Performance | `reverse_bfs`/`build_test_map` re-allocate String keys despite Arc<str> interning | medium | ⬜ |
| P3-56 | TC Happy | `cmd_trace` (CLI handler) has no direct test — suite reimplements BFS inline | medium | ⬜ |

## P4 — Issues / Inline Trivial

| ID | Category | Title | Difficulty | Action |
|----|----------|-------|------------|--------|
| P4-1 | Security | `cqs serve --open` leaks per-launch token to local processes via argv | medium | 🎫 #1337 |
| P4-2 | Security | `find_7z` accepts `ProgramFiles` env var without `is_safe_executable_path` enforcement | medium | 🎫 #1338 |
| P4-3 | Security | `HF_HOME`/`HUGGINGFACE_HUB_CACHE` accepted without canonicalization or trust check | medium | 🎫 #1339 |
| P4-4 | Security | `LocalProvider`'s `CQS_LLM_API_BASE` accepts `http://` for non-loopback hosts | medium | 🎫 #1340 |
| P4-5 | Security | Chunk content printed verbatim to stdout — embedded ANSI/OSC8 escapes reach terminal | medium | 🎫 #1341 |
| P4-6 | Data Safety | `chunks` `INSERT OR REPLACE` cascade enforced only by call-site convention | medium | 🎫 #1342 |
| P4-7 | Resource Management | `EmbeddingCache`/`QueryCache` `Drop` calls `block_on` — can stall daemon shutdown | medium | 🎫 #1343 |
| P4-8 | Resource Management | HNSW background rebuild loads second `Store` (256 MiB mmap × 2) — daemon peaks at 2× RAM | hard | 🎫 #1344 |
| P4-9 | Resource Management | `cqs serve` has no idle eviction — Store mmap pinned for entire process lifetime | medium | 🎫 #1345 |
| P4-10 | Resource Management | Per-handler `Semaphore` permits decoupled from SQLite pool size (32 vs 4) | medium | 🎫 #1346 |
| P4-11 | Extensibility | `BatchProvider` trait has three near-identical `submit_*_batch` methods | medium | 🎫 #1347 |
| P4-12 | Extensibility | `IndexBackend` registry hand-coded in `backends()` — third backend needs cfg edits | medium | 🎫 #1348 |
| P4-13 | Extensibility | `SearchFilter` has 12 fields and 37 of 54 sites enumerate every field | medium | 🎫 #1349 |
| P4-14 | Extensibility | `apply_scoring_pipeline` is hand-coded — adding third score signal edits two paths | medium | 🎫 #1350 |
| P4-15 | Extensibility | HNSW distance metric type-baked as `DistCosine` — switching needs persist-format migration | hard | 🎫 #1351 |
| P4-16 | Extensibility | Telemetry has one hardcoded sink (`.cqs/telemetry.jsonl`) — no trait, no exporter abstraction | medium | 🎫 #1352 |
| P4-17 | Platform Behavior | `db_file_identity` non-Unix fallback uses mtime — fails to detect rapid `--force` replacements | medium | 🎫 #1353 |
| P4-18 | Platform Behavior | Hook scripts assume `cqs` is on the MSYS-shell PATH on Windows-native | medium | 🎫 #1354 |
| P4-19 | Platform Behavior | `audit.rs` audit-mode file gets no Windows ACL — SEC-1 promise broken on Windows | medium | 🎫 #1355 |
| P4-20 | Platform Behavior | `note.rs::write_notes_file` writes bare `\n` — CRLF round-trip churn on Windows | medium | 🎫 #1356 |
| P4-21 | TC Happy | `run_umap_projection` (`cqs index --umap` orchestrator) has zero tests | medium | 🎫 #1357 |
| P4-22 | TC Happy | `cmd_gc` end-to-end is untested — only `GcOutput` JSON serialization asserted | medium | 🎫 #1358 |
| P4-23 | TC Happy | `cqs serve` has no end-to-end smoke test (run_server + auth + handlers) | hard | 🎫 #1359 |

P1 = 47, P2 = 41, P3 = 56, P4 = 23. Total = 167.
