# Audit Triage — v1.29.0

Triage date: 2026-04-23. Source: `docs/audit-findings.md` (147 findings across 16 categories, 2 batches of 8 parallel opus auditors).

## Triage rules

- **P1** — easy difficulty AND high impact (real bug or security exposure that could bite a user soon). Fix immediately.
- **P2** — medium effort AND high impact, OR easy + solid MEDIUM impact. Fix in next batch.
- **P3** — easy + low impact, or medium + low impact. Fix if time.
- **P4** — hard effort OR speculative (architectural changes, "would be nice", coverage gaps with no current bug). Hard ones get filed as GitHub issues; trivial ones fixed inline.

Priority bias: lean higher for items in the new `cqs serve` surface (first external-network-reachable code we've shipped), items that break correctness on Windows (silent no-ops, chunk-id drift), and items affecting data safety in GC / TOCTOU paths. Lean lower for "nice to have" coverage, speculative scaling, and bookkeeping cleanups.

## P1 — Easy + High Impact (fix immediately)

| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| SEC-1 | `cqs serve` accepts any `Host` header — DNS-rebinding exfiltration of entire corpus | easy | `src/serve/mod.rs:97-114` | pending |
| SEC-2 | XSS via unescaped error body in hierarchy-3d / cluster-3d views (`body.slice()` → `innerHTML`) | easy | `src/serve/assets/views/hierarchy-3d.js:107`, `cluster-3d.js:114` | pending |
| SEC-3 | `build_graph` (uncapped) + `build_cluster` fetch entire chunks+function_calls tables — DoS | easy | `src/serve/data.rs:232-234,344-350,833-861` | pending |
| PB-V1.29-2 | Watch SPLADE encoder passes Windows `file.display()` to `get_chunks_by_origin` — silent no-op, SPLADE never updates on Windows | easy | `src/cli/watch.rs:1083-1085` | pending |
| DS2-1 | `prune_missing` reads origin list OUTSIDE write tx — TOCTOU against concurrent upsert, wipes just-added chunks | easy | `src/store/chunks/staleness.rs:76-90` | pending |
| DS2-2 | `prune_gitignored` reads origin list OUTSIDE write tx — same TOCTOU class | easy | `src/store/chunks/staleness.rs:333-337` | pending |

## P2 — Medium effort OR solid-MEDIUM impact (fix in next batch)

### Security
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| SEC-4 | `build_graph` / `build_hierarchy` IN-list can exceed SQLite 32k bind limit → 500 | easy | `src/serve/data.rs:326-341, 670-754` | pending |

### Platform / Correctness
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| PB-V1.29-1 | `cqs context` / `cqs brief` fail on Windows backslash paths (never normalized) | easy | `src/cli/commands/io/context.rs:28,115`, `brief.rs:40-42` | pending |
| PB-V1.29-3 | `chunk.id` prefix-strip uses `abs_path.display()` — breaks on Windows verbatim + backslash paths, silent data integrity drift | medium | `src/cli/watch.rs:2432-2434` | pending |
| PB-V1.29-5 | `dispatch_drift` / `dispatch_diff` emit Windows backslashes in JSON `file` field — breaks cross-platform agent chaining | easy | `src/cli/batch/handlers/misc.rs:277,353,365,377` | pending |

### Data Safety
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| DS2-3 | `set_metadata_opt` / `touch_updated_at` bypass `WRITE_LOCK` — SQLITE_BUSY under concurrent reindex + batch-id setters | easy | `src/store/metadata.rs:409-418, 452-475` | pending |
| DS2-4 | Phantom-chunks DELETE in separate tx from upsert — mid-batch crash serves queries against half-pruned index | medium | `src/cli/watch.rs:2568-2579` | pending |
| DS2-8 | `CQS_MIGRATE_REQUIRE_BACKUP` defaults to off — destructive v18→v19 migration with no backup on failure | medium | `src/store/migrations.rs:478-562` | pending |

### Algorithm Correctness
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| AC-V1.29-1 | `semantic_diff` sort has no tie-break — non-determinism in `cqs diff`/`cqs drift` output | easy | `src/diff.rs:202-207, 298-303` | pending |
| AC-V1.29-2 | `is_structural_query` misses keywords at end-of-query — `"find all trait"` misroutes to Conceptual (α=0.70 instead of 0.90) | easy | `src/search/router.rs:787-789` | pending |
| AC-V1.29-3 | `bfs_expand` seeds in HashMap order — non-deterministic `name_scores` when cap hits mid-expansion | easy | `src/gather.rs:317-320` | pending |
| AC-V1.29-5 | `--name-boost` accepts values outside [0,1] — negative embedding weight silently breaks search | easy | `src/cli/args.rs:57-58` | pending |
| AC-V1.29-6 | `reranker::compute_scores_opt` unchecked `batch_size * stride` mul; negative dim wraps to `usize::MAX` | easy | `src/reranker.rs:368-387` | pending |

### API Design
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| API-V1.29-1 | `cqs --json project list/remove` silently emit text | easy | `src/cli/commands/infra/project.rs:90-108, 110-118` | pending |
| API-V1.29-2 | `cqs --json ref add/remove/update` silently emit text | easy | `src/cli/commands/infra/reference.rs:42-69` | pending |
| API-V1.29-4 | `cqs notes list --check` dropped by daemon batch dispatch (NotesListArgs missing field) | easy | `src/cli/args.rs:527-540`, `batch/handlers/misc.rs:85-118` | pending |

### Error Handling
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| EH-V1.29-1 | `cli/commands/io/brief.rs::build_brief_data` swallows 3 consecutive store errors, zero-filled output | medium | `src/cli/commands/io/brief.rs:59-75` | pending |
| EH-V1.29-2 | `ci::run_ci_analysis` silently downgrades dead-code scan failure into "0 dead" with no gate signal | easy | `src/ci.rs:100-128` | pending |
| EH-V1.29-7 | `cache::EmbeddingCache::stats` swallows 5 per-query failures into a single lying `CacheStats` | easy | `src/cache.rs:408-461` | pending |
| EH-V1.29-8 | Daemon gitignore RwLock poison silently treated as "no matcher" — re-indexes ignored files | easy | `src/cli/watch.rs:1737,1945` | pending |

### Code Quality
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| CQ-V1.29-3 | `cmd_similar` local `resolve_target` diverges from `cqs::resolve_target` — CLI picks test chunks, batch picks real ones | easy | `src/cli/commands/search/similar.rs:16-39` | pending |
| CQ-V1.29-6 | `cqs doctor` reports compile-time `MODEL_NAME` constant as index metadata — silently wrong after `cqs model swap` | easy | `src/cli/commands/infra/doctor.rs:144-147,155-156` | pending |

### Documentation
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| DOC-V1.29-1 | CONTRIBUTING.md says "Schema v20" — actual is v22 | easy | `CONTRIBUTING.md:193,207` | pending |
| DOC-V1.29-2 | README doesn't document `cqs serve` (flagship v1.29.0 feature) | medium | `README.md` | pending |
| DOC-V1.29-3 | README/CONTRIBUTING missing `.cqsignore` | easy | `README.md`, `CONTRIBUTING.md` | pending |
| DOC-V1.29-4 | SECURITY.md says integrity check is opt-out — actual is opt-in (backwards) | easy | `SECURITY.md:22` | pending |

### Scaling
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| SHL-V1.29-2 | `MAX_BATCH_LINE_LEN = 1 MB` blocks large diffs via batch/daemon; CLI accepts 50 MB | easy | `src/cli/batch/mod.rs:104` | pending |

### Performance
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| PF-V1.29-1 | Daemon shell-joins and re-splits args on every query (waste on hot path) | medium | `src/cli/watch.rs:315-331` | pending |

### Resource Management
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| RM-V1.29-1 | `load_references` rebuilds rayon pool + reloads every ref Store+HNSW per `--include-refs` query (bypasses LRU) | medium | `src/cli/batch/handlers/search.rs:286`, `src/reference.rs:204-217` | pending |

### Extensibility
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| EX-V1.29-5 | `NotesListArgs` and `NotesCommand::List` are two hand-maintained arg structs — drift already visible | easy | `src/cli/args.rs:527-540`, `commands/io/notes.rs:49-65` | pending |

### Test Coverage
| ID | Title | Difficulty | Location | Status |
|---|---|---|---|---|
| TC-HAP-1.29-1 | `cqs serve` endpoints (`build_graph` / `build_chunk_detail` / `build_hierarchy` / `build_cluster`) never tested with data | medium | `src/serve/data.rs:192, 452, 586, 825` | pending |
| TC-HAP-1.29-2 | 16 batch dispatch handlers (gather/scout/task/where/onboard/callers/…) have zero tests | medium | `src/cli/batch/handlers/*.rs` | pending |
| TC-ADV-1.29-3 | Daemon socket `handle_socket_client` — zero adversarial tests (1 MiB boundary, malformed JSON, NUL-in-args, oversized args) | medium | `src/cli/watch.rs:160-406` | pending |

## P3 — Easy + Low Impact (fix if time)

### Security (low-impact)
- **SEC-5**: `GraphQuery.file` LIKE filter — `%`/`_` metacharacter injection breaks prefix contract (`src/serve/data.rs:241-248`)
- **SEC-8**: LIKE injection in `build_chunk_detail` "tests that cover" heuristic — hostile function name matches many tests (`src/serve/data.rs:533-541`)

### Platform (low-impact)
- **PB-V1.29-4**: `init` writes `.gitignore` LF-only, noisy `git status` on Windows autocrlf (`src/cli/commands/infra/init.rs:36-40`)
- **PB-V1.29-6**: Hardcoded `/mnt/` WSL check ignores custom `wsl.conf automount.root` (`src/hnsw/persist.rs:86-87`, `src/project.rs:85-86`, `src/config.rs:445-451`)
- **PB-V1.29-7**: `EmbeddingCache::open` / `QueryCache::open` propagate `set_permissions` failure on WSL DrvFS (`src/cache.rs:73-80, 1002-1009`)
- **PB-V1.29-9**: `aux_model::expand_tilde` only handles `~/` prefix — misses bare `~` and `~\` (`src/aux_model.rs:101-108`)

### Data Safety (low-impact)
- **DS2-5**: `EmbeddingCache::evict` / `QueryCache::evict` TOCTOU on SELECT size/AVG/DELETE (`src/cache.rs:352-400, 1103-1147`)
- **DS2-6**: HNSW save's `.bak` rename doesn't fsync parent dir before `atomic_replace` pass (`src/hnsw/persist.rs:414-426`)
- **DS2-7**: HNSW dirty-flag drift — `set_hnsw_dirty(false)` failure after save leaves `dirty=1` permanently (`src/cli/watch.rs:2326-2338, 2250-2257`)
- **DS2-9**: `upsert_sparse_vectors` rollback missing generation bump — stale on-disk `splade.index.bin` trusted (`src/store/sparse.rs:113-117, 193-196`)
- **DS2-10**: `as_millis() as i64` mtime cast — pathological mtime → negative value stored in `notes.file_mtime` (`src/cli/watch.rs:2556`, `src/lib.rs:454`, 13 sites total)

### Algorithm Correctness (low-impact)
- **AC-V1.29-4**: `llm::summary::contrastive_neighbors` top-K no tie-break (`src/llm/summary.rs:263,265,267`)
- **AC-V1.29-7**: `llm::doc_comments::select_uncached` sort tie-break (`src/llm/doc_comments.rs:222-229`)

### Code Quality (low-impact)
- **CQ-V1.29-1**: `BatchProvider::submit_batch` (4-arg) + 3 impls dead — delete (`src/llm/provider.rs:27-33`)
- **CQ-V1.29-2**: `build_scout_output` documented "shared" but `dispatch_scout` duplicates it inline (`src/cli/commands/search/scout.rs:26-38`)
- **CQ-V1.29-4**: Risk thresholds duplicated in `cmd_affected` — JSON vs text path (`src/cli/commands/review/affected.rs:92-100, 143-149`)
- **CQ-V1.29-5**: "Empty impact diff" JSON object duplicated across 3 files, 4 sites (`src/cli/batch/handlers/graph.rs:402-407, 412-417`, etc.)

### Documentation (low-impact)
- **DOC-V1.29-5**: ROADMAP.md lists shipped `cqs serve` under "Parked" (line 174)
- **DOC-V1.29-6**: CONTRIBUTING.md Architecture Overview missing 6 files (`aux_model.rs`, `daemon_translate.rs`, `eval/`, `fs.rs`, `limits.rs`, `serve/`)
- **DOC-V1.29-7**: `src/hnsw/build.rs:39` docstring points at nonexistent `cli/commands/index.rs`
- **DOC-V1.29-8**: `.claude/skills/troubleshoot/SKILL.md` references nonexistent `hnsw.bin`, wrong default model
- **DOC-V1.29-9**: `TODO(docs-agent): document this rule in CONTRIBUTING.md` unresolved after landing
- **DOC-V1.29-10**: README Performance section pinned to v1.27.0 eval file, stale chunk count

### API Design (low-impact)
- **API-V1.29-3**: `cqs telemetry --reset --json` silently drops `--json` (`src/cli/commands/infra/telemetry_cmd.rs:520-578`)
- **API-V1.29-5**: `dispatch_drift` / `dispatch_diff` emit `file` via `.display()` not `normalize_path` (subsumed by PB-V1.29-5)
- **API-V1.29-6**: `BatchCmd::Refresh` / `invalidate` has no CLI surface
- **API-V1.29-7**: `cqs eval --limit` missing `-n` short flag (sibling of every other query command)
- **API-V1.29-8**: Pretty/compact JSON drift between CLI and daemon paths
- **API-V1.29-9**: `--expand` on `cqs search` vs `--expand-parent` on top-level — rename to match
- **API-V1.29-10**: `--depth` short flag `-d` present on `cqs onboard`, absent on `impact`/`test-map`

### Error Handling (low-impact)
- **EH-V1.29-3**: `dispatch::try_daemon_query` silently falls back to CLI when re-serialization fails (`src/cli/dispatch.rs:785-790`)
- **EH-V1.29-4**: `cli/commands/io/blame.rs::build_blame_data` silently suppresses callers fetch failure (`:52-59`)
- **EH-V1.29-5**: `suggest::generate_suggestions` silently skips dedup against existing notes on store error (`src/suggest.rs:62-65`)
- **EH-V1.29-6**: `where_to_add::suggest_placement_with_options_core` silently drops pattern data on batch fetch failure (`src/where_to_add.rs:208-214`)
- **EH-V1.29-10**: `cagra::delete_persisted` discards both `remove_file` errors silently (`src/cagra.rs:1097-1102`)

### Observability (low-impact)
- **OB-V1.29-1**: `Reranker::rerank` lacks entry span; `rerank_with_passages` has one (`src/reranker.rs:160`)
- **OB-V1.29-2**: `serve::build_chunk_detail` and `build_stats` lack spans; other three `build_*` have them (`src/serve/data.rs:452, 933`)
- **OB-V1.29-3**: `cmd_project` span doesn't record subcommand (`src/cli/commands/infra/project.rs:75`)
- **OB-V1.29-4**: `verify_hnsw_checksums` uses format-interpolated `tracing::warn!` (`src/hnsw/persist.rs:136`)
- **OB-V1.29-5**: `serve` axum handlers log entry but never completion — no latency trace (medium effort)
- **OB-V1.29-6**: `classify_query` / `reclassify_with_centroid` lack entry span (`src/search/router.rs:561, 1093`)
- **OB-V1.29-7**: `verify_hnsw_checksums` flattens `io::ErrorKind` via `String` — operator loses ErrorKind

### Robustness (low-impact)
- **RB-V1.29-1**: `timeout_minutes * 60` env-var multiplication can overflow (`src/cli/batch/mod.rs:343, 378`)
- **RB-V1.29-2**: UMAP row/dim/id_max_len narrowing cast without ceiling check (`src/cli/commands/index/umap.rs:104-106,116`)
- **RB-V1.29-3**: `serve/data.rs` negative `line_start` from DB silently clamped to 0 then cast to `u32` (`:504, :787, :785-788`)
- **RB-V1.29-6**: `chunk_count as usize` on 32-bit silently truncates — add crate-level 64-bit gate
- **RB-V1.29-9**: SPLADE 6 sites cast `shape[N] as usize` on ORT `i64` dim without negative check (`src/splade/mod.rs:145,154,524,549,770-838`)
- **RB-V1.29-10**: `id_map.len() * dim * 4 * 2` unchecked mul in HNSW persist (`src/hnsw/persist.rs:647`)

### Scaling (low-impact)
- **SHL-V1.29-1**: `pad_2d_i64` hardcodes pad-token-id = 0 — breaks non-BERT tokenizers (RoBERTa pad=1) (medium effort)
- **SHL-V1.29-3**: `MAX_ID_MAP_SIZE = 100 MB` in `count_vectors` silently drops stats for 1.7M+ chunk corpora
- **SHL-V1.29-4**: Onboard `MAX_CALLEE_FETCH=30` / `MAX_CALLER_FETCH=15` no env override
- **SHL-V1.29-5**: `task.rs` gather constants (`DEPTH=2`, `MAX_NODES=100`, `MULTIPLIER=3`) no env override
- **SHL-V1.29-6**: `SCOUT_LIMIT_MAX` / `SIMILAR_LIMIT_MAX` / `RELATED_LIMIT_MAX` hardcoded while siblings have env overrides
- **SHL-V1.29-9**: `DAEMON_PERIODIC_GC_INTERVAL_SECS` / `_IDLE_SECS` hardcoded while CAP has env override
- **SHL-V1.29-10**: `convert/{html,mod}::MAX_FILE_SIZE = 100 MB` duplicated, no env override

### Performance (low-impact)
- **PF-V1.29-2**: `fetch_chunks_by_ids_async` / `fetch_candidates_by_ids_async` hardcode `BATCH_SIZE=500` based on obsolete 999-parameter limit (`src/store/chunks/async_helpers.rs:27,69`)
- **PF-V1.29-3**: `get_type_users_batch` / `get_types_used_by_batch` hardcode 200 — 3× round trips on impact (`src/store/types.rs:392,438`)
- **PF-V1.29-4**: `find_hotspots` allocates String for every callee before truncating (`src/impact/hints.rs:261-271`)
- **PF-V1.29-5**: Parser unconditionally allocates CRLF-replaced copy of every source file (`src/parser/mod.rs:491`)
- **PF-V1.29-6**: `BatchContext::notes()` clones full Vec per call; siblings use `Arc<...>` (medium effort)
- **PF-V1.29-7**: `upsert_notes_batch` fires 3 SQL statements per note (medium effort)
- **PF-V1.29-8**: `prune_missing` fires `dunce::canonicalize` syscall per missing-path candidate (medium effort)
- **PF-V1.29-10**: `finalize_results` unnecessarily clones sanitized FTS string (`src/search/query.rs:363-369`)

### Resource Management (low-impact)
- **RM-V1.29-2**: `evict_global_embedding_cache_with_runtime` opens `QueryCache` with fresh single-thread runtime every eviction tick (`src/cli/batch/mod.rs:1225`)
- **RM-V1.29-3**: `search_across_projects` builds fresh rayon pool per call (medium effort)
- **RM-V1.29-4**: `TelemetryAggregator::query_counts` unbounded — no cardinality cap (medium effort)
- **RM-V1.29-5**: CHM/WebHelp page readers no per-page byte cap (`src/convert/chm.rs:107`, `webhelp.rs:120`)
- **RM-V1.29-6**: `cqs serve` multi-thread runtime no `worker_threads` cap — uses `num_cpus` (`src/serve/mod.rs:63-66`)
- **RM-V1.29-7**: `EmbeddingCache` / `QueryCache` no `Drop` impl → no WAL checkpoint on daemon shutdown (P2 #70 claim was wrong) (medium effort)
- **RM-V1.29-8**: `Box::leak` pattern in watch.rs test helpers (`src/cli/watch.rs:2660-2663, 2686-2689`)

### Extensibility (low-impact)
- **EX-V1.29-6**: `cli/commands/infra/init.rs` hardcodes model sizes to `dim >= 1024` heuristic (`:42-50`)
- **EX-V1.29-9**: `aux_model::config_from_dir` hardcodes on-disk layout per kind (`:136-148`)

### Test Coverage (adversarial, mostly P3)
- **TC-ADV-1.29-1**: `normalize_l2` silently returns NaN/Inf — no test (`src/embedder/mod.rs:1023-1030`)
- **TC-ADV-1.29-2**: `embed_batch` doesn't validate ORT output for NaN/Inf before `Embedding::new` (medium)
- **TC-ADV-1.29-4**: `parse_unified_diff` missing edge-case tests (double `+++`, orphan `@@`, whitespace-only)
- **TC-ADV-1.29-5**: `parse_notes_str` missing tests for huge mentions array, empty text, NUL-in-text
- **TC-ADV-1.29-6**: HNSW `load_with_dim` missing id_map duplicate/empty/NUL tests
- **TC-ADV-1.29-7**: `embedding_slice` silently passes NaN/Inf bytes from DB
- **TC-ADV-1.29-8**: `dispatch_line` shell_words untested on ANSI/BEL/CR
- **TC-ADV-1.29-9**: `SpladeEncoder::encode` raw-logits propagates Inf
- **TC-ADV-1.29-10**: No DoS test for `parse_unified_diff` on 50MB diff (medium)

### Test Coverage (happy path, low-impact)
- **TC-HAP-1.29-3**: `Reranker::rerank`/`rerank_with_passages` no tests (medium)
- **TC-HAP-1.29-4**: `cmd_project { Search }` no integration test (medium)
- **TC-HAP-1.29-5**: `cmd_ref_add/list/remove/update` no end-to-end tests (medium)
- **TC-HAP-1.29-6**: `handle_socket_client` no happy-path round-trip test (medium)
- **TC-HAP-1.29-7**: `cmd_similar` no integration test
- **TC-HAP-1.29-8**: `cmd_ci` happy path untested — library tested, CLI only error paths
- **TC-HAP-1.29-9**: `cmd_gather` (CLI) untested
- **TC-HAP-1.29-10**: `dispatch_line` no happy-path test (only error/adversarial)

## P4 — Hard OR Low Impact (file issues or defer)

### Security (architectural)
- **SEC-6**: `cmd_serve` spawns `xdg-open`/`open`/`explorer.exe` on URL with bind string — command-string injection surface (hard, speculative) → file issue
- **SEC-7**: `cqs serve` has no authentication — default stance relies on "localhost is trusted" (hard, architectural) → file issue

### Platform (low-impact + speculative)
- **PB-V1.29-8**: `HF_HOME` / `HUGGINGFACE_HUB_CACHE` lookup doesn't honor Windows `%LOCALAPPDATA%` default (medium, Windows-only)
- **PB-V1.29-10**: WSL detection via `/proc/version` — false positives on non-WSL Linux with "microsoft" in kernel (medium, speculative)

### Extensibility (architectural)
- **EX-V1.29-1**: Adding a new CLI command requires coordinated edits across 5-7 files (hard) → file as tracking issue
- **EX-V1.29-2**: `where_to_add::extract_patterns` hardcodes Rust/TS-JS/Go custom logic — refactor to `LanguageDef::patterns` (medium)
- **EX-V1.29-3**: `LlmProvider` enum: adding new provider requires 5+ site edits (medium)
- **EX-V1.29-4**: `AuxModelKind` preset registration duplicates matrix (medium)
- **EX-V1.29-7**: Tree-sitter query file naming has no startup self-test (medium)
- **EX-V1.29-8**: Config schema: adding `[foo]` section requires edits in 3-4 files with no shared pattern (medium)

### Robustness (hard + low risk)
- **RB-V1.29-5**: `extract_l5k_regions` regex captures `.unwrap()` on group 0/1/2 (hard, regex-crate-bug only)
- **RB-V1.29-8**: `reranker.rs` ORT `shape[1] as usize` on negative dim (low — already covered by AC-V1.29-6)

### Scaling (low-impact)
- **SHL-V1.29-7**: Hotspot thresholds (`HOTSPOT_MIN_CALLERS=5` etc.) don't scale with corpus size (medium, speculative)
- **SHL-V1.29-8**: Risk score thresholds (`HIGH=5.0, MEDIUM=2.0`) hardcoded (medium, speculative)

### Error Handling (pattern)
- **EH-V1.29-9**: Project-wide `warnings: Vec<String>` field pattern for `.unwrap_or_default()` paths (medium, cross-cutting pattern)

### Performance (hard)
- **PF-V1.29-9**: `suggest_tests` runs `reverse_bfs` inside a loop over callers — O(callers × graph) (hard)

### Resource Management (low-impact)
- **RM-V1.29-9**: Daemon socket thread spawn without pre-bounded stack size (medium)
- **RM-V1.29-10**: `handle_socket_client` BufReader allocates per-connection (easy but low-priority)

## Cross-references to known open issues

Security findings overlap with the threat model in SECURITY.md for `cqs serve`. The platform findings (PB-V1.29-*) are a natural follow-up to v1.27.0 wave-1 triage which found similar `normalize_path` gaps. DS2-1/DS2-2 extend the P2 #32 fix that closed the equivalent class for `prune_all`.

## Stop condition

Triage covers all 147 findings. Next: generate fix prompts for P1 + P2 (bounded, high-signal set). P3 collected for inline sweep after P1/P2 land. P4 items filed as issues or deferred.
