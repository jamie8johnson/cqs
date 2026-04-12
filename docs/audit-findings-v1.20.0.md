# Audit Findings — v1.20.0

Audit date: 2026-04-08

## Security

#### SEC-1: Temp file created world-readable before `set_permissions` in config write paths
- **Difficulty:** easy
- **Location:** src/config.rs:449, src/config.rs:533, src/audit.rs:120, src/hnsw/persist.rs:242
- **Description:** `add_reference_to_config` and `remove_reference_from_config` both use `std::fs::write(&tmp_path, &serialized)` to create a temp file, then call `set_permissions(0o600)` afterward. Between creation and the permission call there is a window where the file is world-readable (subject to the process umask, typically 0o644). The config file can contain `llm_api_base` URLs, reference paths, and LLM configuration. The same pattern appears in `audit.rs` (audit-mode.json) and in the HNSW fallback copy path (`hnsw/persist.rs`: `fs::copy` to a temp with no permissions set). The comment at config.rs:451 says "BEFORE rename so the file is never world-readable" — but it IS briefly world-readable between creation and the set_permissions call.
- **Suggested fix:** Use `OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp_path)` (Unix) instead of `fs::write` to atomically create the file with the correct permissions. For the `fs::copy` fallback, apply `set_permissions` on the copy destination before the subsequent rename.

#### SEC-2: `fs::copy` fallback in cross-device rename creates world-readable temp file
- **Difficulty:** easy
- **Location:** src/config.rs:467, src/config.rs:552, src/audit.rs:136, src/hnsw/persist.rs:349
- **Description:** When an atomic `fs::rename` fails due to a cross-device move (e.g., Docker overlayfs, NFS), all four write paths fall back to `fs::copy(src, dest_tmp)` then `fs::rename(dest_tmp, final)`. The copy creates the destination file with default umask permissions (typically 0o644). No `set_permissions` call is made on `dest_tmp` before it is renamed into place, so the final file may be world-readable if the umask allows it. This is distinct from SEC-1 (which affects the primary write) — this affects only the fallback branch.
- **Suggested fix:** Add `set_permissions(dest_tmp, 0o600)` immediately after each `fs::copy` call succeeds, before the subsequent rename.

#### SEC-3: `llm_api_base` URL logged verbatim at debug level, exposing embedded credentials
- **Difficulty:** easy
- **Location:** src/config.rs:211, src/config.rs:339
- **Description:** `Config` derives `Debug` and is logged at `tracing::debug!(?merged, "Effective config")` and `tracing::debug!(?config, "Loaded config")`. The `llm_api_base: Option<String>` field is included in this output. Users who configure cqs to talk to LiteLLM, LM Studio, or other proxy APIs sometimes embed credentials in the URL (`https://user:key@host/v1` or `https://host/v1?api_key=sk-...`). With `RUST_LOG=debug`, these credentials appear in terminal output and structured log sinks. The same risk applies to `llm_model` if it encodes provider information.
- **Suggested fix:** Implement a custom `Debug` for `Config` (or use a wrapper type) that redacts `llm_api_base` to show only the host portion (e.g., `https://***@host/v1`). Alternatively, log the field separately through a redacting formatter: `tracing::debug!(api_base = redact_url(merged.llm_api_base.as_deref()), ...)`.

#### SEC-4: `ReferenceConfig.path` accepts any filesystem path without containment validation
- **Difficulty:** medium
- **Location:** src/reference.rs:59-96, src/config.rs:61
- **Description:** The `[[reference]]` stanza in `.cqs.toml` accepts a `path` field that is a raw `PathBuf` from TOML deserialization. `load_single_reference` checks only for symlinks, then opens `cfg.path.join("index.db")` as a SQLite database. There is no check that the path stays within any project boundary, a managed refs directory, or even a user-owned directory. A `.cqs.toml` checked into a shared repository could point `path` at `/etc/`, `/proc/`, or another user's home directory. While the store opens read-only and SQLite will reject non-database files gracefully, the act of attempting to open arbitrary paths could cause unexpected I/O in sensitive locations (e.g., audit trails on `/etc/shadow` access attempts), and HNSW load at `cfg.path` performs additional directory reads.
- **Suggested fix:** After parsing, validate that `ReferenceConfig.path` is either absolute and under a user-controlled base (e.g., `refs_dir()`, home dir, or a configurable allow-list), or resolve it to a canonical path and reject traversal attempts. At minimum, add a `tracing::warn!` when a path escapes the project directory.

#### SEC-5: `search_by_name` FTS injection guard is defense-in-depth only — injected format string still assembled
- **Difficulty:** easy
- **Location:** src/store/search.rs:88-97
- **Description:** `search_by_name` calls `sanitize_fts_query` which strips `"` from the name, then checks `if normalized.contains('"')` as a defense-in-depth guard. However, the FTS query string is assembled by `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)` on line 97 — this string is only ever bound as a parameterized value to SQLite via `.bind(&fts_query)`, so SQL injection is not possible. However, FTS5 treats the bound string as a query expression, not a literal. A name containing FTS5 boolean operators (`OR`, `AND`, `NOT`) would pass through `sanitize_fts_query` (which only filters these as standalone words) if they are part of a multi-word name, and could alter the FTS5 query semantics in unexpected ways. The current sanitizer strips standalone `OR`/`AND`/`NOT` words but not multi-word combinations like `foo OR bar` embedded in a single "name" token.
- **Suggested fix:** This is low severity since `sanitize_fts_query` already handles the main cases. Consider wrapping the FTS5 string in double-quotes from the start rather than assembling `name:"foo" OR name:"foo"*` via format. Alternatively, document the current sanitization approach as intentional and add a test for multi-word name inputs.

## Data Safety

#### DS-1: `prune_missing` (called by `cqs index`) leaves orphan sparse_vectors for deleted files
- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:29-121, src/schema.sql:120-127
- **Description:** `prune_missing` deletes chunks (and cascades to `calls` and `type_edges` via FK) but does NOT clean up `sparse_vectors`. The `sparse_vectors` table has no FK on `chunk_id`, so when `cqs index` runs and files have been deleted, the sparse vectors for those deleted chunks remain in the DB. This causes the in-memory `SpladeIndex` (built from `load_all_sparse_vectors`) to contain IDs of chunks that no longer exist. Sparse search returns these stale IDs as candidates; subsequent fetch-by-ID silently returns nothing, reducing recall for valid results. The orphans persist until `cqs gc` is run. `prune_all` (used by gc) also omits sparse_vectors pruning from its transaction — only calls `prune_orphan_sparse_vectors` separately afterward.
- **Suggested fix:** Add `ON DELETE CASCADE` FK to `sparse_vectors.chunk_id` (requires migration), or call `prune_orphan_sparse_vectors` inside `prune_missing`'s transaction, or include it inside `prune_all`'s transaction before commit.

#### DS-2: `batch_insert_chunks` (INSERT OR REPLACE) silently resets `enrichment_hash` to NULL on every re-index
- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:196-233
- **Description:** `batch_insert_chunks` uses `INSERT OR REPLACE INTO chunks (...)` which, in SQLite, is equivalent to DELETE + INSERT. The INSERT column list omits `enrichment_hash` and `enrichment_version`, so these columns always get their DEFAULT values (NULL and 0) after any upsert — even for chunks whose content hasn't changed. This means every `cqs index` run resets enrichment_hash to NULL for ALL re-indexed chunks, making the RT-DATA-2 idempotency check (`if stored_hash == new_hash { skip }`) always fail. Every incremental `cqs index --enrich` after a normal `cqs index` re-enriches all chunks instead of only changed ones, doubling API cost and latency.
- **Suggested fix:** Switch to `INSERT INTO ... ON CONFLICT(id) DO UPDATE SET ... WHERE content_hash != excluded.content_hash` (SQLite upsert syntax) that preserves `enrichment_hash` and `enrichment_version` when the row already exists, or use `COALESCE` in the UPDATE clause to preserve existing values when re-inserting the same content.

#### DS-3: `gc.rs` ignores `set_hnsw_dirty(true)` failure before HNSW rebuild — crash leaves stale HNSW trusted
- **Difficulty:** easy
- **Location:** src/cli/commands/index/gc.rs:101-103
- **Description:** After `prune_all` deletes chunks, `gc.rs` calls `store.set_hnsw_dirty(true)` but only logs a warning on failure — it continues with the HNSW rebuild. If the dirty flag fails to be written (e.g., DB locked, disk error) and the subsequent HNSW rebuild is interrupted (SIGTERM, crash), the next `cqs` invocation loads a stale HNSW containing IDs of deleted chunks, returning ghost results. `build.rs` (the `cqs index` path) correctly aborts on `set_hnsw_dirty` failure with `.context(...)?`, but GC does not apply the same treatment.
- **Suggested fix:** Return an early error when `set_hnsw_dirty(true)` fails in `gc.rs`, mirroring the pattern in `build.rs:190-191`: `store.set_hnsw_dirty(true).context("Failed to mark HNSW dirty before GC rebuild")?;`.

#### DS-4: `migrate_v14_to_v15` hardcodes '768' dimensions, corrupting non-E5-base installs
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:180
- **Description:** The v14→v15 migration unconditionally sets `dimensions = '768'`. A user who had configured BGE-large (1024-dim) on a v14 schema will have their dimensions metadata overwritten to 768 after migration. `Store::open` reads `dim` from metadata at open time, so `store.dim()` returns 768 after migration — mismatching the actual 1024-dim embeddings still in the `chunks` table. The migration also sets `hnsw_dirty = '1'` which forces a rebuild on next `cqs index`, restoring correctness. But any search run between migration and the forced rebuild uses `store.dim = 768` to interpret 1024-dim blobs, producing wrong embeddings and degraded search results.
- **Suggested fix:** Read the existing dimensions value before overwriting: only write '768' if the stored value is '769' (the sentinel indicating the old sentiment-augmented dim). Use `UPDATE metadata SET value = '768' WHERE key = 'dimensions' AND value = '769'` to preserve non-769 dimension values.

#### DS-5: DEFERRED transactions on all write paths yield SQLITE_BUSY under concurrent indexers
- **Difficulty:** medium
- **Location:** src/store/chunks/crud.rs:52, src/store/calls/crud.rs:20, src/store/types.rs:151 (and ~15 other `pool.begin()` sites)
- **Description:** All store write operations use `pool.begin()` which issues `BEGIN DEFERRED`. A DEFERRED transaction upgrades from a shared to an exclusive lock only on first write. Two concurrent processes (e.g., `cqs watch` triggering a reindex while `cqs index` holds a write transaction) can both acquire DEFERRED transactions and then race to upgrade — one gets SQLITE_BUSY. The 5-second `busy_timeout` helps but is insufficient for long-running transactions (HNSW build takes seconds). `cqs watch` uses `try_acquire_index_lock` which skips the cycle if the lock is held, but the process-level file lock and the SQLite transaction are acquired independently — a race window exists between them. Previously identified as DS-38 (v1.13.0 triage) and remains unfixed.
- **Suggested fix:** Use `BEGIN IMMEDIATE` for all write transactions (acquires reserved lock at BEGIN, eliminating the upgrade race). In sqlx, this requires executing `BEGIN IMMEDIATE` manually before the first write, or contributing `SqliteTransactionKind::Immediate` support to sqlx.

#### DS-6: `prune_all` transaction omits sparse_vectors, leaving split atomicity window for concurrent readers
- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:163-220, src/cli/commands/index/gc.rs:68-87
- **Description:** `prune_all` commits a transaction deleting chunks, function_calls, type_edges, and llm_summaries atomically. Immediately after, `gc.rs` calls `prune_orphan_sparse_vectors()` in a separate transaction. Between these two commits, concurrent readers see chunks deleted but their sparse vectors still present. A `SpladeIndex` loaded in this window includes ghost IDs. If the process is killed (SIGTERM) between the two commits, orphan sparse vectors persist until the next GC run — there is no equivalent of `set_hnsw_dirty` to flag this condition.
- **Suggested fix:** Move `DELETE FROM sparse_vectors WHERE chunk_id NOT IN (SELECT id FROM chunks)` inside `prune_all`'s transaction as a new step before commit, and remove or downgrade the separate `prune_orphan_sparse_vectors` call in `gc.rs`.

## Code Quality

#### CQ-1: `--cross-project` accepted but silently falls back to local on 4 commands
- **Difficulty:** medium
- **Location:** src/cli/commands/graph/impact.rs:23, src/cli/commands/graph/trace.rs:108, src/cli/commands/graph/test_map.rs:188, src/cli/commands/graph/deps.rs:82 (and matching batch handlers in src/cli/batch/handlers/graph.rs:31,142,198,243)
- **Description:** Four commands — `impact`, `trace`, `test-map`, `deps` — accept `--cross-project` via the argument parser, emit a `tracing::warn!` saying "not yet implemented", then silently proceed with local-only results. Users who pass the flag get no indication in normal output that the flag was ignored; the warn goes to the trace subscriber which most users never see. The PR (#850) wired the flag through the entire call chain but only `callers` and `callees` have real implementations. The other four commands have stub bodies.
- **Suggested fix:** Either implement the cross-project path for these four commands using the already-written `analyze_impact_cross` / `trace_cross` functions, or return an `Err` with a clear user-visible message ("--cross-project not yet supported for impact; use callers/callees instead") so the flag isn't silently swallowed.

#### CQ-2: `analyze_impact_cross` and `trace_cross` are library-exported but never called from CLI
- **Difficulty:** easy
- **Location:** src/impact/cross_project.rs:42,171 / src/lib.rs:133
- **Description:** `analyze_impact_cross` and `trace_cross` are public functions re-exported in `lib.rs` but no CLI command calls them. The commands that could use them (`cmd_impact`, `cmd_trace`) have the cross-project stub that warns and falls back. These functions are only exercised by their own unit tests. They are effectively dead in production paths.
- **Suggested fix:** Wire them into the stub commands (resolving CQ-1), or add a `// Note: called by cmd_impact cross-project path` comment and a CLI-level integration test. As-is they give false confidence that cross-project impact/trace are implemented.

#### CQ-3: `analyze_impact_cross` returns empty `file` and `line: 0` for all callers
- **Difficulty:** medium
- **Location:** src/impact/cross_project.rs:97-116
- **Description:** `analyze_impact_cross` builds its result from a `HashMap<String, (usize, String)>` (name → (depth, project)) and populates `CallerDetail` and `TransitiveCaller` with `file: PathBuf::new()` and `line: 0` for every entry. The JSON output would show empty file paths and zero line numbers for all callers, making cross-project impact results unusable for navigation. The underlying data is available in `CallGraph` but is not threaded through.
- **Suggested fix:** Resolve caller file and line from the per-project CallGraph's `forward`/`reverse` maps after BFS, similar to how `analyze_impact` does it via `get_callers_full`.

#### CQ-4: `CrossProjectContext::from_config` panics on local store open failure
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:83-84
- **Description:** `from_config` opens the local store with a fallback: `Store::open_readonly(...).unwrap_or_else(|_| Store::open(...).expect("open local"))`. If `open` also fails (corrupt DB, missing file), this panics in production code with an unhelpful message. The function signature returns `Result<Self, StoreError>` but uses `.expect()` instead of `?`.
- **Suggested fix:** Replace with `Store::open_readonly(...).or_else(|_| Store::open(...)).map_err(|e| StoreError::Open(e.to_string()))?`.

#### CQ-5: `_local: &Store` parameter in `CrossProjectContext::from_config` is completely ignored
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:74-84
- **Description:** `from_config` takes `_local: &Store` (underscore prefix signals intentional non-use) but then opens a fresh separate `Store` from disk at the same path. This means every cross-project call opens two connections to the local DB — the already-open one in `CommandContext` and a new one from `from_config`. The parameter was likely intended to allow using the existing open store directly.
- **Suggested fix:** Either accept the already-open local store directly (`NamedStore { name: "local", store: local.clone() }` — requires `Store: Clone` or wrapping in `Arc`), or remove the unused parameter so callers don't pass it.

#### CQ-6: `include_types` silently ignored in `analyze_impact_cross` with no user-visible warning
- **Difficulty:** easy
- **Location:** src/impact/cross_project.rs:126-127
- **Description:** `analyze_impact_cross` accepts `include_types: bool` but suppresses it with `let _ = include_types` and returns `type_impacted: Vec::new()`. There is no `tracing::warn!` or user-visible indication that the type impact path is not supported cross-project. This differs from the cross-project stub behavior in CLI commands which at least log a `tracing::warn!`.
- **Suggested fix:** Add `if include_types { tracing::warn!("--include-types not supported in cross-project mode"); }` before discarding the parameter.

#### CQ-7: `ScoringConfig::with_overrides` marked dead with explicit follow-up note
- **Difficulty:** medium
- **Location:** src/search/scoring/config.rs:39-40
- **Description:** `ScoringConfig::with_overrides` has `#[allow(dead_code)]` and a doc comment reading "Callers: currently test-only; wiring into scoring pipeline is a follow-up." It is exercised only by two unit tests in the same file. The scoring pipeline never reads per-project scoring overrides from `.cqs.toml`. This means the `[scoring]` config section in `.cqs.toml` is parsed but silently has no effect on search results.
- **Suggested fix:** Wire `with_overrides` into `search_filtered` / `score_candidate` by loading `ScoringOverrides` from `Config` at startup, or remove the function and config section if the feature is deferred.

#### CQ-8: Duplicate `make_named_store` test helpers across cross-project modules
- **Difficulty:** easy
- **Location:** src/impact/cross_project.rs:277, src/store/calls/cross_project.rs:239
- **Description:** Both test modules define a private `make_named_store` helper that builds a `NamedStore` backed by a temp SQLite DB with synthetic call edges. The two implementations differ in API (one takes `Vec<(&str,&str)>` edge list, the other takes forward/reverse `HashMap`s) but do the same thing. Neither can reuse the other because both are in `#[cfg(test)]` blocks in different crates/modules.
- **Suggested fix:** Extract to a shared test utility module (e.g., `tests/cross_project_test.rs` already has `create_project` / `insert_chunk_and_call` that could be extended) or a `#[cfg(test)] pub(crate) mod test_utils` in `store/calls/cross_project.rs` that `impact/cross_project.rs` imports.

#### CQ-9: `std::mem::forget(dir)` in cross-project test helpers leaks temp directories
- **Difficulty:** easy
- **Location:** src/impact/cross_project.rs:300, src/store/calls/cross_project.rs:290
- **Description:** Both test helpers call `std::mem::forget(dir)` to prevent the `TempDir` from cleaning up its directory. The comment in `cross_project.rs` says "Tests are short-lived so this is fine." But `mem::forget` permanently leaks the directory for the process lifetime, and each test run accumulates orphaned directories under `/tmp`. The integration test file (`tests/cross_project_test.rs`) correctly uses `let _dir = TempDir::new()` (RAII) instead.
- **Suggested fix:** Use `let _dir = dir;` (let RAII drop at end of scope), or call `dir.into_path()` to convert to a `PathBuf` and accept cleanup. Both properly clean up after the test.

## Error Handling

#### EH-1: `evict()` avg-entry query failure silently swallowed without tracing
- **Difficulty:** easy
- **Location:** src/cache.rs:319-324
- **Description:** The `avg_entry` query in `evict()` uses `.unwrap_or(4200)` when the SQLite query fails, with no `tracing::warn!`. Every other SQLite fallback in `cache.rs` uses `unwrap_or_else(|e| { tracing::warn!(error = %e, "..."); default })` — `stats()` has five such warn sites at lines 350-390. The inconsistency means a DB error during eviction (e.g., corrupt table, busy writer) is silently swallowed, making the eviction use a hardcoded estimate rather than actual data without any observability.
- **Suggested fix:** Replace `.unwrap_or(4200)` with `.unwrap_or_else(|e| { tracing::warn!(error = %e, "Cache evict avg-entry query failed, using default"); 4200 })`.

#### EH-2: `from_config` silently eats `open_readonly` failure for local store without logging
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:83-84
- **Description:** `from_config` opens the local store with `Store::open_readonly(...).unwrap_or_else(|_| Store::open(...).expect("open local"))`. The `unwrap_or_else(|_|)` discards the readonly-open error entirely — no tracing, no context. If the DB cannot be opened read-only (permissions, missing WAL file, corruption), the fallback to writable open occurs with no log entry. Operators cannot tell whether the readonly bypass happened. The subsequent `.expect("open local")` panics if the writable open also fails (noted as CQ-4). Even in the success path, there is zero observability for the fallback path.
- **Suggested fix:** Log the error before falling through: `.unwrap_or_else(|e| { tracing::warn!(error = %e, "Local store open_readonly failed, falling back to writable"); Store::open(&root.join(".cqs/index.db")).map_err(StoreError::from)? })`. This also naturally converts the downstream panic into a propagated error.

#### EH-3: `get_all_summaries_full()` failure silently discards all LLM summaries before reindex
- **Difficulty:** easy
- **Location:** src/cli/commands/index/build.rs:129
- **Description:** Before destroying and rebuilding the index, `cmd_index` reads LLM summaries from the existing DB to preserve them across the reindex. The call `old_store.get_all_summaries_full().unwrap_or_default()` swallows any DB error silently. If the query fails (schema mismatch on old DB, corrupt `llm_summaries` table, SQLite busy), `summaries` is silently empty and all cached LLM summaries are discarded — the next index run regenerates them at full API cost. The immediately surrounding `Err(e)` arm for `Store::open` failure (line 139-141) has a `tracing::warn!`, but no equivalent warn covers the query failure case.
- **Suggested fix:** Replace with `match old_store.get_all_summaries_full() { Ok(s) => s, Err(e) => { tracing::warn!(error = %e, "Failed to read LLM summaries, reindex will regenerate them"); Vec::new() } }`.

#### EH-4: `chunk_type_language_map` silently drops chunks with unrecognized type or language
- **Difficulty:** easy
- **Location:** src/store/chunks/query.rs:458-460
- **Description:** `chunk_type_language_map` builds the HNSW filter metadata from every row in the `chunks` table. For each row it calls `(ct.parse(), lang.parse())` and silently skips rows where either parse fails — no logging, no counter, no fallback. Any chunk with an unrecognized `chunk_type` or `language` is absent from the map. The search predicate at `src/search/query.rs:356` returns `false` for map-absent chunks, silently excluding them from all `--include-type`, `--exclude-type`, and `--lang` filtered searches. Every other parse site in the codebase (`query.rs:53-83`, `query.rs:539-550`, `helpers/types.rs:74-88`, `calls/dead_code.rs:92-103`) emits a `tracing::warn!` before using a default value. This is the only parse site that silently discards without logging.
- **Suggested fix:** Replace the silent `if let (Ok(...), Ok(...))` with explicit match arms that log and skip: `let Ok(chunk_type) = ct.parse::<ChunkType>() else { tracing::warn!(id = %id, raw = %ct, "Unknown chunk_type in map cache, chunk excluded from type filters"); continue; };` (and same for language).

## Performance

#### PF-1: `get_neighbors` allocates `Vec<String>` on every BFS node expansion in `gather`
- **Difficulty:** easy
- **Location:** src/gather.rs:736-755
- **Description:** `get_neighbors` creates a fresh `Vec<String>` per BFS node, converting `Arc<str>` neighbor lists to owned `String` via `.to_string()`. With up to 200 expanded nodes (the default cap) and potentially large adjacency lists, this creates 200+ allocation/copy sequences per `gather` call. The `CallGraph.forward` and `CallGraph.reverse` maps already store `Arc<str>` values — the conversion to `String` is unnecessary. The queue and `visited` set also store `String` for each node name.
- **Suggested fix:** Change `get_neighbors` to return `Vec<Arc<str>>` (clone the `Arc`, O(1) refcount bump). Update `bfs_expand` to use `Arc<str>` in `visited: HashSet<Arc<str>>` and `queue: VecDeque<(Arc<str>, usize)>`, and change `name_scores` key type to `Arc<str>`. This eliminates all per-node String heap allocations in the BFS loop.

#### PF-2: `bfs_expand` double-initializes seed nodes across `visited` and `queue`
- **Difficulty:** easy
- **Location:** src/gather.rs:308-313
- **Description:** `bfs_expand` initializes `visited` from `name_scores.keys().cloned()` (one String clone per seed), then immediately iterates `name_scores` again to push all seeds into `queue` (a second clone per seed). Each seed name is cloned twice during initialization. Paired with PF-1, fixing to `Arc<str>` would make each of these O(1).
- **Suggested fix:** Combine initialization into a single pass: `for (name, _) in name_scores.iter() { let owned = name.clone(); visited.insert(owned.clone()); queue.push_back((owned, 0)); }`.

#### PF-3: `compute_risk_and_tests` calls `reverse_bfs` once per target despite a multi-source alternative
- **Difficulty:** medium
- **Location:** src/impact/hints.rs:185-238
- **Description:** `compute_risk_and_tests` precomputes forward-BFS test reachability via `test_reachability` (one traversal, O(T*E)), then calls `reverse_bfs(graph, name, ...)` in a loop — once per target — for test attribution (line 187). With N targets (e.g., `scout` processing 10+ search results), this is N additional BFS traversals. The `reverse_bfs_multi_attributed` function already exists in the same module and performs multi-source reverse BFS in a single pass, producing per-source attribution.
- **Suggested fix:** Replace the per-target `reverse_bfs` loop with a single `reverse_bfs_multi_attributed(graph, targets, max_depth)` call before the loop. Distribute attributed test entries from the combined result to each target using the source index. This reduces N reverse BFS traversals to one.

#### PF-4: `find_contrastive_neighbors` clones the candidates `Vec` on every iteration
- **Difficulty:** easy
- **Location:** src/llm/summary.rs:263,268
- **Description:** The per-row loop over N chunks builds `per_row_neighbors: Vec<Vec<(usize, f32)>>` by calling `candidates.clone()` on every iteration (lines 263 and 268, both branches). With N=12,000 chunks and limit=3, this is 12,000 `Vec` clones. The comment on line 253 notes "Reuse a single candidates buffer" but this refers only to the buffer's allocation; the contents are still cloned into `per_row_neighbors` on each iteration.
- **Suggested fix:** Eliminate `per_row_neighbors` entirely. After each row's `select_nth_unstable_by` + `truncate` + sort, immediately construct the neighbor name strings from `candidates` and insert into `result` inline — no intermediate clone needed.

#### PF-5: `CrossProjectContext::from_config` opens a duplicate local store connection
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:81-85
- **Description:** `from_config` takes `_local: &Store` (unused, underscore-prefixed) but opens a fresh `Store::open_readonly` on the same local DB path. The caller's existing open store has warm caches (call graph `OnceLock`, test chunks `OnceLock`, chunk_type_language_map `OnceLock`). The duplicate connection discards all warm state, runs the SQLite integrity check again, and allocates a new connection pool. Related to CQ-5 (design issue) — the same fix resolves both.
- **Suggested fix:** Reuse the existing open store by wrapping `Store` in `Arc<Store>` and having `CrossProjectContext` hold `Arc<Store>` references. The caller passes `Arc::clone(&ctx.store)` as the local entry, avoiding the duplicate connection.

#### PF-6: `search_by_names_batch` materializes full `ChunkSummary` before name-match check
- **Difficulty:** medium
- **Location:** src/store/chunks/query.rs:407-424
- **Description:** In the post-filter loop, `ChunkSummary::from(ChunkRow::from_row(&row))` is called — materializing all string fields including content and doc — before checking whether the chunk matches any query name. If it matches, it is then cloned again (line 416) into the result vec. With a batch of 20 names and `total_limit = limit_per_name * 20` rows, every row undergoes full materialization even if it matches nothing. A `ChunkSummary` includes content (potentially kilobytes), signature, doc, and other String fields.
- **Suggested fix:** Add an early filter: check `name` column first (or fetch only id + name in the initial query), then load full content only for matched chunk IDs via `fetch_chunks_by_ids`. This separates the cheap name-filter step from the expensive content-load step.

#### PF-7: `cached_notes_summaries` acquires a `Mutex` on every warm search call
- **Difficulty:** medium
- **Location:** src/store/metadata.rs:282-294
- **Description:** `cached_notes_summaries` uses `Mutex<Option<Arc<Vec<NoteSummary>>>>`. On the warm path (cache populated), the function locks the Mutex, reads the Arc, and returns a clone. Other caches in `Store` use `OnceLock<Arc<T>>` for zero-overhead reads. Notes require `Mutex` (not `OnceLock`) because they support invalidation after `upsert`/`delete`. In batch mode, every search request acquires this Mutex, creating a serialization point for concurrent searches sharing one `Store`.
- **Suggested fix:** Replace `Mutex<Option<Arc<Vec<NoteSummary>>>>` with `arc_swap::ArcSwap<Vec<NoteSummary>>` from the `arc-swap` crate. Warm reads are a single atomic pointer load with no locking. Invalidation swaps in a new `Arc`. Alternatively use `RwLock` to allow concurrent readers.

#### PF-8: Inline placeholder construction in `upsert_fts_conditional` duplicates `make_placeholders`
- **Difficulty:** easy
- **Location:** src/store/chunks/async_helpers.rs:264-270
- **Description:** The batch DELETE loop constructs placeholder strings via `batch.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect::<Vec<_>>().join(",")`, creating N small String allocations. The `make_placeholders(n)` helper in `src/store/helpers/sql.rs` does exactly this and is already used at line 178 in the same file (`snapshot_content_hashes`), and in `get_callers_with_context_batch`, `get_callers_full_batch`, and other sites. The inline re-implementation is inconsistent and bypasses any future optimization to `make_placeholders`.
- **Suggested fix:** Replace the inline placeholder construction with `super::super::helpers::make_placeholders(batch.len())`.

#### PF-9: `suggest_tests` calls `reverse_bfs` per direct caller for test-status check
- **Difficulty:** medium
- **Location:** src/impact/analysis.rs:316-328
- **Description:** `suggest_tests` iterates `impact.callers` and calls `reverse_bfs(&graph, &caller.name, DEFAULT_MAX_TEST_SEARCH_DEPTH)` per caller to check if any test reaches that caller. For widely-called functions with 20-50 direct callers, this runs 20-50 independent BFS traversals over the same graph. The comment says "Caller count is typically small (direct callers only), so this is fine" — but the impact command is most useful for exactly these high-caller-count functions. `reverse_bfs_multi` handles multi-source BFS in one pass.
- **Suggested fix:** Collect all caller names, call `reverse_bfs_multi(graph, &caller_names, max_depth)` once, then check each caller's test status against the merged ancestor map (any test in ancestors → tested).

#### PF-10: `GatherOptions::default` reads and parses env var on every construction in batch mode
- **Difficulty:** easy
- **Location:** src/gather.rs:161-193
- **Description:** `GatherOptions::default()` calls `std::env::var("CQS_GATHER_MAX_NODES")` and parses it inline on every construction. In batch mode, `gather` is called repeatedly and each invocation constructs `GatherOptions::default()`, paying the env var read cost each time. Compare with `bfs_max_nodes()` in `src/impact/bfs.rs` which caches the env var result in `OnceLock<usize>` and reads it only once per process.
- **Suggested fix:** Extract to a `fn default_max_expanded_nodes() -> usize` using `OnceLock<usize>`: `static CAP: OnceLock<usize> = OnceLock::new(); *CAP.get_or_init(|| parse_env_or_default(...))`. Reference this from `GatherOptions::default()`.

## Test Coverage

#### TC-27: `exclude_types` filter has zero test coverage
- **Difficulty:** easy
- **Location:** src/search/query.rs:345,492 / src/search/scoring/filter.rs:104 / src/store/helpers/search_filter.rs:22
- **Description:** `SearchFilter::exclude_types` is applied in three separate locations in the search pipeline (`search_filtered`, `search_filtered_with_index`, and `score_candidate`), and is wired through both the CLI (`--exclude-type`) and the batch handler. Despite this broad use, not a single test passes `exclude_types: Some(vec![...])` and verifies the excluded type is absent from results. The `search_filter.rs` unit tests cover `chunk_types` (include filter) but not `exclude_types`. The `search_test.rs` and `query.rs` inline tests only use `chunk_types`.
- **Suggested fix:** Add a unit test in `src/search/query.rs` (alongside `test_search_filtered_chunk_type_filter`) that inserts chunks of two types, sets `exclude_types = Some(vec![ChunkType::Function])`, and asserts the returned results contain no `Function` chunks. Also add a test for `exclude_types` overlapping with `chunk_types` (both set simultaneously).

#### TC-28: Solidity `post_process_solidity_solidity` reclassifications untested
- **Difficulty:** easy
- **Location:** src/language/languages.rs:6463 / tests/language_test.rs:5707
- **Description:** `post_process_solidity_solidity` has two reclassification branches with no test: (1) `name == "constructor"` → `ChunkType::Constructor`, and (2) a `Property` whose source text contains `"constant "` or `"immutable "` → `ChunkType::Constant`. The five Solidity tests cover contract extraction, interfaces, calls, struct/enum, and events — none test `constructor()` or constant/immutable state variables.
- **Suggested fix:** Add two tests: one with `constructor() public { }` in a contract asserting `ChunkType::Constructor`, and one with `uint256 public constant MAX_SUPPLY = 1000000;` asserting `ChunkType::Constant`.

#### TC-29: PowerShell Pester block reclassification (`Describe`/`It`/`Context` → Test) untested
- **Difficulty:** easy
- **Location:** src/language/languages.rs:5041 / tests/language_test.rs:4375
- **Description:** `post_process_powershell_powershell` reclassifies functions named `Describe`, `It`, `Context`, or starting with `Test` to `ChunkType::Test`. The six PowerShell tests cover function, class, method, property, enum, and calls — none test Pester-style test blocks. There is no test asserting that a `Describe { }` block receives `ChunkType::Test`.
- **Suggested fix:** Add a test with a Pester-style script (`Describe "My suite" { It "should work" { } }`) and assert that the `Describe` chunk has `chunk_type == ChunkType::Test`.

#### TC-30: Scala `var` → `Variable` reclassification untested
- **Difficulty:** easy
- **Location:** src/language/languages.rs:6264 / tests/language_test.rs:5642
- **Description:** `post_process_scala_scala` reclassifies `Constant` chunks whose source text starts with `var ` to `ChunkType::Variable`. The only existing val/var test (`parse_scala_val_const`) tests `val maxRetries` → `Constant` and nothing more. No test covers a mutable `var` declaration to confirm it is reclassified. This branch is the primary functional difference between `val` and `var` in Scala chunk classification.
- **Suggested fix:** Add a test: `object Config { var counter: Int = 0 }` asserting `counter` has `chunk_type == ChunkType::Variable`.

#### TC-31: Dart `post_process_dart_dart` edge cases untested (Extension, factory Constructor, test() → Test)
- **Difficulty:** easy
- **Location:** src/language/languages.rs:1124 / tests/language_test.rs:7462
- **Description:** `post_process_dart_dart` has three reclassification branches with no dedicated tests: (1) `name.starts_with("test") || name == "group"` → `ChunkType::Test`; (2) `factory` prefix or `constructor_signature` node → `ChunkType::Constructor`; (3) a `Class` node whose source starts with `"extension "` → `ChunkType::Extension`. The five Dart tests cover function, class, enum, method, and doc comment — all happy-path extraction. The `post_process` logic that makes Dart chunks semantically meaningful is entirely untested.
- **Suggested fix:** Add three tests: one with a Dart `test('name', () {})` top-level call asserting `ChunkType::Test`, one with `factory Widget.fromJson(Map json)` asserting `ChunkType::Constructor`, and one with `extension StringX on String { ... }` asserting `ChunkType::Extension`.

#### TC-32: Bash `post_process_bash_bash` variable-inside-function skip is untested
- **Difficulty:** easy
- **Location:** src/language/languages.rs:79 / tests/language_test.rs:125
- **Description:** `post_process_bash_bash` walks up the parent chain from a `Variable` node and returns `false` (discard) if any ancestor is a `function_definition`. This prevents variables declared inside Bash function bodies from appearing in the chunk index. No test exercises this path. `parse_bash_readonly_constant` only tests top-level `readonly`, and `parse_bash_no_chunks_outside_function` tests bare commands but not variables nested in functions.
- **Suggested fix:** Add a test: `function deploy() { local CONFIG="prod"; }` asserting no chunk is produced for `CONFIG`.

#### TC-33: CUDA `extern "C"` → `Extern` and no-return-type → `Constructor` reclassifications untested
- **Difficulty:** easy
- **Location:** src/language/languages.rs:1014 / tests/language_test.rs:1570
- **Description:** `post_process_cuda_cuda` has two reclassification branches with no test coverage: (1) functions inside a `linkage_specification` (`extern "C" { ... }`) → `ChunkType::Extern`; (2) `function_definition` with no `type` child (i.e., no return type) → `ChunkType::Constructor`. The three CUDA tests cover kernel extraction, struct extraction, and call extraction from the sample fixture; neither `extern "C"` blocks nor constructors are in `sample.cu`.
- **Suggested fix:** Add two inline tests (not fixture-based): one with `extern "C" { void cuda_bridge(float* d); }` asserting `ChunkType::Extern`, and one with a CUDA class constructor (function body, no return type) asserting `ChunkType::Constructor`.

#### TC-34: `SearchFilter::validate()` with `enable_splade = true, splade_alpha = NaN` is untested
- **Difficulty:** easy
- **Location:** src/store/helpers/search_filter.rs:132
- **Description:** The `splade_alpha` NaN guard (RB-12 fix) at line 133 is only reachable when `enable_splade = true`. All existing `validate()` tests leave `enable_splade = false` (the default), so the guard `if self.enable_splade && !(0.0..=1.0).contains(&self.splade_alpha)` is never exercised. In Rust, `(0.0_f32..=1.0).contains(&f32::NAN)` returns `false` due to NaN comparison semantics, so the guard fires correctly — but this is not verified by any test.
- **Suggested fix:** Add two tests in `search_filter.rs`: `enable_splade = true, splade_alpha = f32::NAN` → `Err`, and `enable_splade = true, splade_alpha = 1.5` → `Err`. The existing `test_search_filter_invalid_name_boost_nan` is the right template.

#### TC-35: `parse_file` oversized file path (MAX_FILE_SIZE guard) has no test
- **Difficulty:** medium
- **Location:** src/parser/mod.rs:177
- **Description:** `parse_file` returns `ParserError::FileTooLarge` when a file exceeds `MAX_FILE_SIZE` (50MB). `parse_file_all` has the same guard at line 363. Neither path is exercised by any test. The adversarial tests in `mod.rs` all call `parse_source` directly, bypassing the file-size check. Creating a genuine 50MB file in tests is impractical.
- **Suggested fix:** Make `MAX_FILE_SIZE` `pub(crate)` (it already is, at line 30), then add a test that creates a temp file, appends enough data to exceed a test-configurable limit, and asserts `parse_file` returns `Err`. Alternatively, test the logic via a size-checking helper that accepts the limit as a parameter under `#[cfg(test)]`.

#### TC-36: `search_across_projects` end-to-end (real registry path) untested
- **Difficulty:** hard
- **Location:** src/project.rs:204
- **Description:** The four inline unit tests for `search_across_projects` test only sub-components: path detection logic, empty registry detection, a direct `search_filtered_with_index` call, and sort/truncate logic. None of them call `search_across_projects` itself, which loads a registry from `~/.cqs/projects.toml`, builds a Rayon thread pool, fans out to `search_single_project` per registered project, and merges results. The Rayon sequential fallback path and the multi-project score merge are both untested end-to-end.
- **Suggested fix:** Add a test that sets `HOME` to a temp directory, writes a `projects.toml` pointing at two pre-indexed stores, calls `search_across_projects`, and asserts results arrive from both projects. The `cli_batch_test.rs` pattern of pointing `CQS_STORE_PATH` at a known fixture is the right approach.

## Extensibility

#### EXT-3: `human_name()` requires manual update for compound-name chunk types — no compile-time guard
- **Difficulty:** easy
- **Location:** src/language/mod.rs:574
- **Description:** `define_chunk_types!` is the single source of truth for adding a new `ChunkType`, and `test_all_chunk_types_classified` enforces that `is_callable()` and `is_code()` are updated. However, `human_name()` has a wildcard catch-all (`other => other.to_string()`) that silently falls through for compound-name types. If someone adds `ChunkType::HttpRoute => "httproute"`, the display name in NL descriptions becomes `"httproute"` rather than `"HTTP route"`. The plan.rs placement template mentions `human_name()` as a soft reminder (`src/language/mod.rs — Update is_callable() and human_name() if needed`), but no compile-time mechanism forces the update.
- **Suggested fix:** Add a test that calls `human_name()` on all `ChunkType::ALL` variants and asserts no result contains an uppercase letter followed by a lowercase letter mid-word (i.e., the display name doesn't look like a CamelCase identifier that failed to convert). This would catch newly added variants like `ChunkType::TypeAlias` returning `"TypeAlias"` instead of `"type alias"`.

#### EXT-4: Adding a language requires updating 5+ doc locations — no compile-time counter
- **Difficulty:** easy
- **Location:** src/lib.rs:17, README.md:5, README.md:592, Cargo.toml:6, CONTRIBUTING.md:103,155,157
- **Description:** The `define_languages!` macro correctly generates all enum machinery, and `test_all_variants_count` catches registry/variant mismatches. However, the hardcoded language count `"54"` appears in 7 documentation locations (lib.rs TL;DR comment, README.md TL;DR, README.md How it Works paragraph, Cargo.toml description, CONTRIBUTING.md architecture section ×3). CONTRIBUTING.md documents this at line 459 as a manual checklist step. The Elm PR (#840) required a separate fix commit because CI failed due to a missed count assertion — evidence that the manual checklist is insufficient even for attentive contributors.
- **Suggested fix:** Add a `#[test]` in `src/language/mod.rs` that reads the language count from `Language::all_variants().len()` and asserts it equals a hardcoded constant. Any mismatch tells the contributor to update docs. Alternatively, generate the count via a build script and inject it into a `generated_constants.rs` file referenced by the doc comments.

#### EXT-5: `ScoringOverrides` in config does not expose `rrf_k` — only env var
- **Difficulty:** easy
- **Location:** src/config.rs:81, src/store/search.rs:12
- **Description:** All 10 `ScoringConfig` fields are exposed through `ScoringOverrides` in `.cqs.toml` (name_exact, note_boost_factor, splade_alpha, etc.). However, `rrf_k` — the RRF fusion constant that significantly affects hybrid search result ordering — is env-var-only (`CQS_RRF_K`, defaulting to 60). Users who want a per-project `rrf_k` value must set an environment variable rather than placing `[scoring]\nrrf_k = 40` in `.cqs.toml`. The asymmetry is inconsistent: splade_alpha is a comparable search-tuning scalar and it is in `ScoringOverrides`.
- **Suggested fix:** Add `rrf_k: Option<f32>` to `ScoringOverrides`, apply it in `ScoringConfig::from_overrides()`, and read it in `rrf_k()` before checking the env var. Add clamping in `validate()` (reasonable range: 1.0–1000.0).

## Scaling & Hardcoded Limits

#### SHL-25: 25 env vars not documented in README — users cannot discover them
- **Difficulty:** easy
- **Location:** README.md:647–660
- **Description:** The README documents 13 env vars in its "Environment Variables" table. The source code defines 38 `CQS_*` env vars total. The 25 absent from the README include significant tuning knobs: `CQS_LLM_MAX_TOKENS`, `CQS_LLM_MODEL`, `CQS_LLM_PROVIDER`, `CQS_HYDE_MAX_TOKENS`, `CQS_HNSW_M`, `CQS_HNSW_EF_CONSTRUCTION`, `CQS_HNSW_EF_SEARCH`, `CQS_HNSW_MAX_DATA_BYTES`, `CQS_HNSW_MAX_GRAPH_BYTES`, `CQS_GATHER_MAX_NODES`, `CQS_IMPACT_MAX_NODES`, `CQS_MAX_CONTRASTIVE_CHUNKS`, `CQS_RAYON_THREADS`, `CQS_SKIP_ENRICHMENT`, `CQS_DEFERRED_FLUSH_INTERVAL`, `CQS_QUERY_CACHE_SIZE`, `CQS_RERANKER_MAX_LENGTH`, `CQS_MD_MAX_SECTION_LINES`, `CQS_MD_MIN_SECTION_LINES`, `CQS_LLM_MAX_CONTENT_CHARS`, `CQS_MAX_SEQ_LENGTH`, `CQS_EMBEDDING_DIM`, `CQS_PDF_SCRIPT`, `CQS_API_BASE`, `CQS_LLM_API_BASE`. This was previously identified (SHL-24, fixed in #842) but the subsequent addition of new env vars reopened the gap.
- **Suggested fix:** Add all 25 to the README env var table. Add a CI test (or extend `ci.rs`) that greps the source for `CQS_*` env vars and asserts each appears in the README table, to prevent future drift.

#### SHL-26: `llm_max_tokens` config capped at 4096 — lower than current model limits
- **Difficulty:** easy
- **Location:** src/config.rs:248
- **Description:** `validate()` clamps `llm_max_tokens` to `[1, 4096]` with a hard `clamp(1, 4096)`. Current models in use (claude-haiku-4-5) support up to 8192 output tokens. Users who set `llm_max_tokens = 8000` to get longer summaries have the value silently clamped to 4096 with only a `tracing::warn!` (not user-visible at normal log levels). The env var path (`CQS_LLM_MAX_TOKENS`) bypasses this cap, so power users can work around it, but the config file path cannot.
- **Suggested fix:** Raise the cap to 32768 (covers current and near-future Claude generations). The upstream API enforces its own per-model limits — a misconfigured value will produce an API error, which is more informative than silent clamping.

#### SHL-27: `ENRICH_EMBED_BATCH` hardcoded at 64, ignores `CQS_EMBED_BATCH_SIZE`
- **Difficulty:** easy
- **Location:** src/cli/enrichment.rs:73
- **Description:** The main indexing pipeline uses `embed_batch_size()` which reads `CQS_EMBED_BATCH_SIZE` for GPU/CPU tuning. The enrichment pass (LLM summary embedding) uses a separate `const ENRICH_EMBED_BATCH: usize = 64` that is independent of the env var. Similarly, the SPLADE index build path in `build.rs:398` uses `const SPLADE_BATCH: usize = 64`. Users who reduce `CQS_EMBED_BATCH_SIZE` to avoid GPU OOM during indexing still get full-size batches during enrichment, which may cause the same OOM.
- **Suggested fix:** Replace `const ENRICH_EMBED_BATCH: usize = 64` with a call to `embed_batch_size()` from `cli::pipeline::types`. Do the same for `SPLADE_BATCH`. Both paths embed text through the same ONNX session, so sharing the batch size limit is correct.

#### SHL-28: `MAX_REFERENCES = 20` hardcoded with no env override and no rationale
- **Difficulty:** easy
- **Location:** src/config.rs:221
- **Description:** `validate()` silently truncates the `[[reference]]` array to 20 entries. There is no comment explaining why 20 (performance? memory? RRF score validity?). The truncation emits a `tracing::warn!` which is invisible to users at default log levels — a user who configures 25 references gets 20 without notification. No env var allows override.
- **Suggested fix:** Add a comment explaining the limit (e.g., "RRF cross-project score merge becomes meaningless with >N projects"). Change the truncation to a user-visible `eprintln!` warning. Optionally add `CQS_MAX_REFERENCES` env override.

#### SHL-29: Pipeline channel depths (`PARSE_CHANNEL_DEPTH=512`, `EMBED_CHANNEL_DEPTH=64`) not env-configurable
- **Difficulty:** easy
- **Location:** src/cli/pipeline/types.rs:95–97
- **Description:** The `define_channel_depth!`-equivalent constants are compile-time fixed. On machines with large RAM and many cores, `PARSE_CHANNEL_DEPTH=512` creates up to 512 parsed batches in memory simultaneously. On memory-constrained systems, `EMBED_CHANNEL_DEPTH=64` may still queue too many embedding batches (each containing `embed_batch_size()` × dim-1024 × 4 bytes of f32). `CQS_EMBED_BATCH_SIZE` tunes the batch count but not the channel depth, so the number of queued batches is separately unconfigurable. The triage note SHL-23 deferred this as "low priority", but these values have no documented rationale for their specific sizes.
- **Suggested fix:** Convert both constants to env-readable functions following the `embed_batch_size()` pattern (`CQS_PARSE_CHANNEL_DEPTH`, `CQS_EMBED_CHANNEL_DEPTH`). Add defaults matching current values with a comment explaining memory implications.

#### SHL-30: HNSW ID map size limit (500MB) uses inline constant, not env-readable function
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:461
- **Description:** `hnsw_max_graph_bytes()` and `hnsw_max_data_bytes()` are already env-overridable via `CQS_HNSW_MAX_GRAPH_BYTES` and `CQS_HNSW_MAX_DATA_BYTES`. The ID map size limit at line 461 (`const MAX_ID_MAP_SIZE: u64 = 500 * 1024 * 1024`) is a `const` inside the `load()` function body with no env override. A large codebase with long chunk IDs (e.g., paths with deep directory trees) can exhaust this limit before reaching the entry-count guard at line 518 (`MAX_ID_MAP_ENTRIES = 10_000_000`). Both limits are security guards, but the inconsistency makes it harder for operators to tune limits for their environment.
- **Suggested fix:** Convert `MAX_ID_MAP_SIZE` to a `hnsw_max_id_map_bytes()` function following the same `OnceLock` + `CQS_HNSW_MAX_ID_MAP_BYTES` pattern as the graph/data limits. Keep the entry-count limit (`MAX_ID_MAP_ENTRIES = 10_000_000`) as a pure security guard without env override.

## Robustness

#### RB-15: `CrossProjectContext::from_config` panics instead of returning `Err` when local DB is inaccessible
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:84
- **Description:** `from_config` returns `Result<Self, StoreError>` but uses `.expect("open local")` when both `Store::open_readonly` and `Store::open` fail on the local index path. If `.cqs/index.db` does not exist yet (first run before `cqs index`) or is corrupted and both open modes fail, the process panics rather than returning a `StoreError`. The reference-loading path on lines 88–100 correctly handles failures with `tracing::warn!` + `continue`, but the local-store path uses `.expect`.
- **Suggested fix:** Replace with `Store::open_readonly(...).or_else(|_| Store::open(...))?` to propagate both errors as `StoreError` to the caller.

#### RB-16: `reranker::score_passages` panics on ONNX model with zero outputs
- **Difficulty:** easy
- **Location:** src/reranker.rs:209
- **Description:** After running ONNX inference, the reranker accesses the first output with `outputs[0]` without first checking that the outputs map is non-empty. A malformed or incompatible cross-encoder model that produces no outputs causes an index-out-of-bounds panic rather than a recoverable `RerankerError`. The embedder avoids this by using `outputs.get("last_hidden_state").ok_or_else(...)` with a named key lookup.
- **Suggested fix:** Replace `outputs[0]` with `outputs.values().next().ok_or_else(|| RerankerError::Inference("ONNX model produced no outputs".into()))?` or use `outputs.get(output_name)` with the known output name (e.g., `"logits"`).

#### RB-17: `post_process` functions slice `node_text` at byte offset 200/300 without checking UTF-8 char boundaries
- **Difficulty:** easy
- **Location:** src/language/languages.rs:538, 1726, 2961, 3529, 5164, 5530, 6259, 7414
- **Description:** Eight `post_process_*` functions truncate `node_text` (a `&str` extracted from tree-sitter source) using `&node_text[..node_text.len().min(200)]` or `.min(300)`. The `.min(N)` gives a byte count, not a char count. If the source code contains multi-byte UTF-8 characters (e.g., Unicode identifiers, comments with non-ASCII text, Chinese class names, emoji) and the Nth byte falls in the middle of a multi-byte sequence, the slice panics at runtime with `byte index N is not a char boundary`. Affected languages: C# (line 538), F# (1726), Java (2961), Kotlin (3529), Python (5164), Razor injection detection (5530), Scala (6259), VB.NET (7414). The embedder's own truncation at `src/embedder/mod.rs:550` uses the correct `is_char_boundary` walk-back pattern. The `suggest.rs` and `scout.rs` truncations use `floor_char_boundary` (stable since Rust 1.86).
- **Suggested fix:** Replace each `&node_text[..node_text.len().min(N)]` with `&node_text[..node_text.floor_char_boundary(N)]`. This is a one-line fix per site and is consistent with the pattern used elsewhere in the codebase.

#### RB-18: `find_insertion_point` accesses `file_lines[idx]` without bounds check when `line_start` exceeds file length
- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:71
- **Description:** In the `BeforeFunction` branch of `find_insertion_point`, `idx` is computed as `line_start - 2` (converting from 1-based to 0-based, indexing the line above the function). There is no check that `idx < file_lines.len()` before accessing `file_lines[idx]`. If the index DB contains a chunk with a `line_start` larger than the current file length (which can happen when a file is edited and shortened between index operations, or when the `--improve-docs` command is run on stale index data), the access panics. The `detect_existing_doc_range` function in the same file has this check (`if idx >= file_lines.len() { return None; }` at line 119) but `find_insertion_point` does not. The existing guard comment at line 46 (`RB-12: empty file_lines would panic`) only protects against the empty-file case, not the out-of-range index case.
- **Suggested fix:** Add `if idx >= file_lines.len() { return line_start; }` immediately after the `idx = line_start - 2` assignment on line 64, before entering the loop.

## API Design

#### AD-1: `Store::open_light` is the read-only primary path but is named to suggest "lightweight", not "readonly"
- **Difficulty:** easy
- **Location:** src/store/mod.rs:273, src/cli/store.rs:41
- **Description:** There are three store open modes: `Store::open` (read-write, 4-connection pool, 256MB mmap), `Store::open_light` (readonly, 1-connection, 256MB mmap — **full performance**), and `Store::open_readonly` (readonly, 1-connection, 64MB mmap — **reduced resources**). `CommandContext::open_readonly` calls `open_project_store_readonly()` which calls `Store::open_light` — not `Store::open_readonly`. The name `open_light` suggests reduced resource usage but it actually uses the same 256MB mmap as the full `open`. The CLI's "readonly" path for search, callers, explain, etc. goes through `open_light`; `open_readonly` is used only for reference stores in `CrossProjectContext`. A developer reading `CommandContext::open_readonly` would expect it to call `Store::open_readonly`, but it calls `Store::open_light`. The doc comment at store.rs:270 says "read-only mode with single-threaded runtime but full memory" — the distinction between "light" and "readonly" is not surfaced in names.
- **Suggested fix:** Rename `Store::open_light` to `Store::open_readonly_full` (readonly with full mmap, for primary index reads) and rename `Store::open_readonly` to `Store::open_readonly_light` (readonly with reduced mmap, for reference stores). Then `CommandContext::open_readonly` → `Store::open_readonly_full` is self-documenting. Alternatively, collapse to two modes: `open` (read-write) and `open_readonly(full: bool)`.

#### AD-2: `--include-type`/`--exclude-type` (search ChunkType filter) vs `--include-types` (impact boolean) share a confusing prefix with different semantics
- **Difficulty:** easy
- **Location:** src/cli/definitions.rs:162–167, src/cli/args.rs:43–44
- **Description:** The `--include-type` and `--exclude-type` flags on the search command accept a list of `ChunkType` values to filter search results (e.g., `--include-type function`). The `--include-types` flag on `impact` is a boolean that controls whether type-impacted functions are included in impact analysis — an entirely different concept. The similar names create a cognitive hazard: a user familiar with `--include-type` for search would expect `--include-types` on impact to also accept a list of type names. README.md line 286 shows both on adjacent lines: `cqs --include-type function "retry logic"` and `cqs impact search_filtered --include-types`. The singular/plural distinction (`--include-type` vs `--include-types`) is the only signal distinguishing them.
- **Suggested fix:** Rename `impact --include-types` to `impact --type-deps` or `impact --with-types` to make it unambiguous. The Rust field `include_types: bool` in `ImpactArgs` would become `type_deps: bool`. Update README and batch commands accordingly.

#### AD-3: `CrossProjectCallee.line` serializes as `"line"` but single-project callee uses `"line_start"`
- **Difficulty:** easy
- **Location:** src/store/calls/cross_project.rs:44, src/cli/commands/graph/callers.rs:22
- **Description:** When `cqs callees foo --json` runs without `--cross-project`, output uses `CalleeEntry.line_start: u32` serialized as `"line_start"`. When `--cross-project` is used, output comes from `Vec<CrossProjectCallee>` which has `pub line: u32` serialized as `"line"` (no rename). Callers in the same command have consistent naming: single-project uses `CallerInfo.line` renamed to `"line_start"`, and cross-project uses `#[serde(flatten)]` on `CallerInfo` which inherits the same rename. Only callees are inconsistent. A caller parsing `cqs callees foo --json` output must switch field names depending on whether `--cross-project` is passed.
- **Suggested fix:** Add `#[serde(rename = "line_start")]` to `CrossProjectCallee.line`, matching the `CallerInfo` pattern. This is a one-line fix.

#### AD-4: Batch `Search` command is missing `--no-demote`, `--name-boost`, `--no-content`, `--expand`, `--ref`, and `--include-refs` flags present in CLI search
- **Difficulty:** medium
- **Location:** src/cli/batch/commands.rs:32–68, src/cli/definitions.rs:153–227
- **Description:** The CLI top-level search command has 18 flags; the batch `Search` command has 10. Flags available in CLI but absent from batch: `--no-demote` (disable test-function score demotion), `--name-boost` (name match weight), `--no-content` (return file:line only), `--context N` (show N lines of context), `--expand` (parent context retrieval), `--ref name` (search in a specific reference index), `--include-refs` (include reference indexes in results), `--no-stale-check`. Of these, `--no-demote` is particularly significant: batch users are AI agents that often want to find test functions (which are demoted by default), and there is no way to disable demotion in batch mode. `--name-boost` is also impactful because batch agents use `--json` output and frequently want name-biased results. The absence of `--ref` from batch means agents cannot do reference-specific search in batch mode even though the `gather` batch command supports `--ref`.
- **Suggested fix:** Add the missing flags to batch `Search`. Minimum viable additions: `--no-demote`, `--name-boost`, `--ref`, and `--include-refs`. The `--no-content` and `--context` flags are display-only and less critical for batch (which always uses JSON), but `--expand` affects content returned and should be included.

#### AD-5: `SearchFilter.chunk_types` field is named differently from both CLI (`--include-type`) and batch API (`include_type`)
- **Difficulty:** easy
- **Location:** src/store/helpers/search_filter.rs:20, src/cli/definitions.rs:163, src/cli/batch/handlers/search.rs:19
- **Description:** The same concept — "filter to include only these chunk types" — is named three different ways in three layers: the `SearchFilter` struct field is `chunk_types` (no "include" prefix, plural), the CLI flag is `--include-type` (with "include" prefix, singular), and the batch `SearchParams` struct field is `include_type` (singular, matches CLI). The batch handler maps `include_type → chunk_types` at the boundary. This was noted as AD-19 in the v1.19.0 triage and tracked as issue #844 but not yet fixed. The inconsistency means a caller building a `SearchFilter` programmatically uses `chunk_types`, while a batch API caller uses `include_type`, and the CLI user uses `--include-type` (with an alias `--chunk-type`).
- **Suggested fix:** Rename `SearchFilter.chunk_types` to `SearchFilter.include_types` (plural, consistent with exclude_types). Update all direct struct-literal construction sites. Alternatively rename to `include_type` (singular) to match the CLI/batch surface, but plural is more idiomatic for `Vec<ChunkType>`.

#### AD-6: `StoredProc`, `ConfigKey`, and `TypeAlias` chunk type display names are squashed single-words in the CLI type filter
- **Difficulty:** easy
- **Location:** src/language/mod.rs:538,546,556
- **Description:** The `define_chunk_types!` macro defines the `Display` string for each chunk type. Three types use squashed single-word identifiers: `TypeAlias => "typealias"`, `ConfigKey => "configkey"`, `StoredProc => "storedproc"`. These are the values users must pass to `--include-type` and `--exclude-type`. A user running `cqs --include-type storedproc "trigger"` has to know the magic squashed form. The `human_name()` method translates these to "type alias", "config key", "stored procedure" for NL text, but `Display` (used in filter parsing and error messages) still shows the squashed form. The CLI docs say `--include-type configkey` (not `config-key` or `config_key`). None of the 29 other chunk types squash multiple words — they are all single concepts: `function`, `method`, `class`, `endpoint`, `service`, `middleware`, etc. Only these three are multi-concept names that get squashed.
- **Suggested fix:** Allow hyphenated forms as aliases in `ChunkType::from_str`: accept `"stored-proc"` → `StoredProc`, `"config-key"` → `ConfigKey`, `"type-alias"` → `TypeAlias` in addition to the current squashed forms. Update help text examples to use the hyphenated forms. Keep the squashed forms as valid inputs for backward compatibility. This is purely additive — no breaking change.

#### AD-7: `CommandContext` has no `open_readwrite` counterpart that uses the project store helper — write commands call `open_project_store()` directly
- **Difficulty:** easy
- **Location:** src/cli/store.rs:74–91, src/cli/commands/io/notes.rs:168, src/cli/commands/infra/reference.rs:145,200,234,330,370, src/cli/commands/io/diff.rs:103, src/cli/commands/io/drift.rs:98
- **Description:** `CommandContext::open_readwrite` exists (wraps `open_project_store()` and builds the full context), but write commands in the CLI (`notes`, `reference`, `diff`, `drift`) call `Store::open` or `open_project_store()` directly instead of `CommandContext::open_readwrite`. This means these commands get a raw `Store` without the lazy `reranker`, `embedder`, `splade_encoder`, and `splade_index` helpers. If any of these commands later need reranking or embedding (e.g., notes add with auto-embedding), they must rebuild `CommandContext` or access the store differently. The inconsistency also means the command dispatch path is not uniform: most commands go through `CommandContext` but a subset bypass it and open stores manually.
- **Suggested fix:** Route all write commands through `CommandContext::open_readwrite`. This is a refactor to make dispatch uniform, not a behavior change. The `CommandContext.store` field is public, so existing `store.method()` calls in these commands remain valid.

#### AD-8: `CallerWithContext.line` serializes as `"line"` while all other caller/callee location fields serialize as `"line_start"`
- **Difficulty:** easy
- **Location:** src/store/helpers/types.rs:191, src/impact/types.rs:14,27,38,49
- **Description:** `CallerInfo.line` has `#[serde(rename = "line_start")]` and serializes as `"line_start"`. `CallerDetail.line`, `TestInfo.line`, `TransitiveCaller.line`, `TypeImpacted.line`, `DiffTestInfo.line` all have `#[serde(rename = "line_start")]`. `ChangedFunction` uses `pub line_start: u32` (native name). But `CallerWithContext.line` at `types.rs:191` has no rename and serializes as `"line"`. Although `CallerWithContext` is not directly emitted in any current CLI JSON output path (it is converted to `CallerDetail` before serialization), it is `pub` in `lib.rs` and is `#[derive(Serialize)]`, so library consumers who call `get_callers_with_context` and serialize the result get `"line"` while every other field in the ecosystem uses `"line_start"`. The inconsistency is a latent bug for any consumer that treats `CallerWithContext` as a JSON type.
- **Suggested fix:** Add `#[serde(rename = "line_start")]` to `CallerWithContext.line` at `src/store/helpers/types.rs:191`, consistent with every other caller location field in the codebase.

## Observability

#### OB-7: `search_hybrid` silently falls back to dense-only when SPLADE is requested but unavailable
- **Difficulty:** easy
- **Location:** src/search/query.rs:323-326, src/cli/store.rs:142-143
- **Description:** When a user passes `--splade`, the CLI sets `filter.enable_splade = true` and calls `search_hybrid`. If the SPLADE model is not installed, `splade_encoder()` returns `None` with only a `tracing::debug!("SPLADE model not found, hybrid search unavailable")` — invisible at default log levels. The `splade_query` becomes `None` and `search_hybrid` receives `splade = None`. At line 324, the condition `!filter.enable_splade || splade.is_none()` fires, silently delegating to `search_filtered_with_index` with no log entry. The user gets pure-dense results while believing hybrid search ran. The only observable difference is search quality — there is no "SPLADE was requested but not available" warning at any visible log level. The analogous situation (HNSW index missing) emits an `info!` at line 510: "Index returned no candidates, falling back to brute-force search".
- **Suggested fix:** Promote the debug log in `splade_encoder()` to `tracing::warn!` when `cli.splade` is true. Alternatively, add a `tracing::warn!("SPLADE requested but not available (no encoder or no index), falling back to dense-only search")` at line 325 in `search_hybrid` when `filter.enable_splade && splade.is_none()`. The second option is more robust because it covers both the "no encoder" case and the "no indexed sparse vectors" case.

#### OB-8: `collect_events` in watch mode has zero tracing — dropped events are invisible
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:380-431
- **Description:** `collect_events` is called on every file system event. It silently skips paths for four distinct reasons: (1) path is under `.cqs/` (internal directory), (2) path is not a supported extension, (3) mtime is unchanged since last index (WSL/NTFS dedup), (4) `pending_files` is full (overflow cap). None of these paths emit a log message at any level. When debugging watch mode problems — files not being reindexed, events being dropped — there is no way to determine which skip condition fired. Every other event-filtering system in cqs logs the reason for skipping at `debug!` level (e.g., `tracing::debug!("Skipping nested block inside parent block")` in `post_process_hcl`).
- **Suggested fix:** Add `tracing::debug!` calls at each skip point: `tracing::debug!(path = %norm_path, "watch: skipping .cqs dir")`, `tracing::debug!(path = %path.display(), ext = %ext, "watch: unsupported extension")`, `tracing::debug!(path = %rel.display(), "watch: mtime unchanged, skipping dedup")`, and `tracing::warn!(path = %rel.display(), "watch: pending_files at capacity, event dropped")` (warn for overflow since events are lost).

#### OB-9: `pending_files` overflow in watch mode silently drops file events
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:425-427
- **Description:** When `pending_files.len() >= max_pending_files()` (default 10,000), `collect_events` silently skips inserting the new path. The file change is permanently lost — it will not be reindexed in the current or any future cycle (unless the file is modified again). There is no warning log and no counter. A mass git checkout, branch switch, or `sed -i` across thousands of files can silently overflow the queue and leave the index partially stale. The overflow is documented in the code comment near `WatchState` but has no runtime signal. The `process_file_changes` function logs "N file(s) changed, reindexing..." at the point of processing, but since the dropped files never entered `pending_files`, they never appear in this count.
- **Suggested fix:** Add `tracing::warn!(path = %rel.display(), cap = max_pending_files(), "Watch pending_files at capacity, event dropped — run 'cqs index' to catch up")` at lines 425-427 when the cap is reached. Optionally, emit this warn once per cycle rather than per dropped file (track a `dropped_count` in `WatchState` and log it at cycle boundary).

#### OB-10: `search_single_project` has no tracing span — cross-project search timing is invisible
- **Difficulty:** easy
- **Location:** src/project.rs:295-346
- **Description:** `search_single_project` is called from `search_across_projects` via Rayon's thread pool — potentially 4 invocations in parallel. Each opens a `Store::open_readonly`, loads an `HnswIndex`, runs `search_filtered_with_index`, and maps results. This I/O path (per-project: DB open + HNSW load + search) can take 10-100ms per project. There is no `info_span!` or timing instrumentation inside `search_single_project`. When cross-project search is slow, there is no way to identify which project is the bottleneck without adding external profiling. The outer `search_across_projects` span covers total time but not per-project breakdown. By contrast, `search_reference` (same pattern for reference indexes) has an `info_span!` at `reference.rs:152`.
- **Suggested fix:** Add `let _span = tracing::info_span!("search_single_project", project = %entry.name, path = %index_path.display()).entered();` at the start of `search_single_project`. This gives per-project timing in flamegraphs and log output.

#### OB-11: `reindex_files` in watch mode does not log embedding cache hit/miss ratio
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:682-713
- **Description:** `reindex_files` separates chunks into `cached` (have stored embeddings, skip re-embedding) and `to_embed` (need embedding). The split is computed at lines 685-692 but never logged. If a file changes superficially (e.g., comment edit) and chunks are hash-equivalent to what is stored, all chunks are in `cached` and embedding is skipped. If a file changes substantially, all chunks need re-embedding. An operator cannot tell from logs whether watch cycles are fast (all cached) or slow (all to_embed). The full indexing pipeline (`prepare_for_embedding` in `src/cli/pipeline/embedding.rs:98-104`) logs this breakdown at `info!` level with `global_hits`, `store_hits`, and `to_embed` counts. Watch mode's equivalent path has no analogous logging.
- **Suggested fix:** After the `for (i, chunk) in chunks.iter().enumerate()` loop at line 686, add `tracing::info!(total = chunks.len(), cached = cached.len(), to_embed = to_embed.len(), "Watch embedding cache stats");`. This mirrors the pattern in `prepare_for_embedding` and gives the same observability in watch mode that exists in batch mode.

#### OB-12: `load_single_reference` has no tracing span — parallel reference load timing is invisible
- **Difficulty:** easy
- **Location:** src/reference.rs:59-96
- **Description:** `load_single_reference` is called in parallel from a Rayon pool in `load_references`. Each invocation opens `Store::open_readonly` + `HnswIndex::try_load_with_ef` for one reference. These I/O operations can take 10-50ms per reference. The outer `load_references` has a `debug_span!` at line 104 covering total time and emits `tracing::info!("Loaded N reference indexes")` at completion. But there is no per-reference span, so when one reference is slow (large HNSW, cold disk cache, symlink check), it cannot be identified. Contrast with `search_reference` which has an `info_span!` for each per-reference search — the same visibility is missing for loading. With 10-20 references (the max before truncation at `MAX_REFERENCES = 20`), the bottleneck is invisible.
- **Suggested fix:** Add `let _span = tracing::debug_span!("load_single_reference", name = cfg.name, path = %cfg.path.display()).entered();` at the start of `load_single_reference`. Use `debug_span` to match the outer `load_references` span level, and to avoid spamming `info` on every search command that loads references.

## Documentation

#### DOC-32: CHANGELOG `[Unreleased]` empty — 3 post-v1.20.0 feature commits have no entries
- **Difficulty:** easy
- **Location:** CHANGELOG.md:8
- **Description:** Three feature commits merged after the v1.20.0 release tag have no entries in the `[Unreleased]` section: `#850` (cross-project call graph — `CrossProjectContext`, `analyze_impact_cross`, `trace_cross`, `--cross-project` on callers/callees); `#851` (4 new chunk types: Extern, Namespace, Middleware, Modifier — 29 total); `#852` (chunk type coverage gaps across 15 languages — Test/Constructor reclassification added to F#, PowerShell, Ruby, Scala, Dart, ObjC, Swift, Perl, Julia, Elixir, Kotlin, VB.NET, and more). Users reading the changelog cannot discover these changes.
- **Suggested fix:** Add entries for all three commits to `[Unreleased]` following the existing format. The commit messages contain all necessary detail.

#### DOC-33: ROADMAP shows 5 shipped items as "ready to pick up" (unchecked)
- **Difficulty:** easy
- **Location:** ROADMAP.md:30-34
- **Description:** Five items in the "CPU Lane — ready to pick up" section are still unchecked `[ ]` despite having been shipped in post-v1.20.0 commits: (1) "Cross-project call graph" (line 30) — shipped in `#850`; (2) "Extern chunk type" (line 31) — shipped in `#851`; (3) "Namespace chunk type" (line 32) — shipped in `#851`; (4) "Middleware chunk type" (line 33) — shipped in `#851`; (5) "Solidity modifier chunk type" (line 34) — shipped in `#851` as `ChunkType::Modifier`. All five are implemented and tested. Keeping them unchecked creates confusion about what is actually done.
- **Suggested fix:** Mark all five items as `[x]` and move them to the Done summary table at the bottom of ROADMAP.md.

#### DOC-34: CONTRIBUTING.md `store/calls/` listing missing `cross_project.rs`
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:169
- **Description:** The architecture overview lists `store/calls/` submodule files as `mod.rs, crud.rs, dead_code.rs, query.rs, related.rs, test_map.rs`. The file `cross_project.rs` — added in `#850` — is absent from this list. It is the largest new module in this subdirectory and implements `CrossProjectContext`, `NamedStore`, `CrossProjectCaller`, `CrossProjectCallee`, and `CrossProjectTestChunk`. The corresponding `impact/cross_project.rs` is correctly listed at line 231.
- **Suggested fix:** Update line 169 to: `mod.rs, crud.rs, dead_code.rs, query.rs, related.rs, test_map.rs, cross_project.rs`.

#### DOC-35: README shows `cqs trace --cross-project` as working, but trace falls back to local with a warning
- **Difficulty:** easy
- **Location:** README.md:191
- **Description:** The Call Graph section shows `cqs trace <a> <b> --cross-project    # Call chain that may cross project boundaries` as an example. However, `cmd_trace` accepts `--cross-project` and immediately emits `tracing::warn!("--cross-project for trace is not yet implemented, using local only")` then proceeds locally. Only `callers` and `callees` have real cross-project implementations. Line 190 (`callers --cross-project`) is accurate; line 191 (`trace --cross-project`) is not.
- **Suggested fix:** Either remove line 191 until `trace_cross` is wired into `cmd_trace`, or annotate it: `# Note: stub — falls back to local (not yet implemented)`.

#### DOC-36: 8 `post_process_*` functions added in `#852` have no `///` doc comments
- **Difficulty:** easy
- **Location:** src/language/languages.rs:79, 1014, 1124, 1699, 5041, 5970, 6250, 6463
- **Description:** Eight `post_process_*` functions added in commit `#852` — `post_process_bash_bash`, `post_process_cuda_cuda`, `post_process_dart_dart`, `post_process_fsharp_fsharp`, `post_process_powershell_powershell`, `post_process_ruby_ruby`, `post_process_scala_scala`, `post_process_solidity_solidity` — have no `///` doc comments. They contain only inline `//` comments. Many pre-existing functions (`post_process_cpp_cpp`, `post_process_csharp_csharp`, `post_process_java_java`) have proper `///` doc comments describing their reclassification logic. Of 42 total `post_process_*` functions, at least 8 of the newly added ones lack doc comments, making cross-language comparison harder for contributors.
- **Suggested fix:** Add a one-line `///` doc comment to each of the 8 functions describing its reclassifications, following the style of `post_process_java_java` ("promote `static final` fields from Property to Constant...").

#### DOC-37: CONTRIBUTING.md has no "Adding a New Chunk Type" section
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:293
- **Description:** CONTRIBUTING.md has guides for "Adding a New CLI Command" (line 293), "Adding Injection Rules" (line 311), and "Adding a New Language" (line 341), but no section for adding a chunk type. The procedure requires: (1) adding a variant to `define_chunk_types!`; (2) classifying it in `is_callable()` and `is_code()`; (3) updating `human_name()` for multi-word names; (4) wiring into `post_process_*` functions or `.scm` queries; (5) updating `test_all_chunk_types_classified`; (6) updating CHANGELOG. Without a written checklist, contributors may miss steps — EXT-3 (current audit) notes that `human_name()` has no compile-time guard for omissions.
- **Suggested fix:** Add an "Adding a New Chunk Type" section between "Adding Injection Rules" and "Adding a New Language", listing required steps and referencing Extern and Modifier from `#851` as examples.

#### DOC-38: `callees --cross-project` is functional but absent from README Call Graph section
- **Difficulty:** easy
- **Location:** README.md:184-199
- **Description:** The Call Graph section documents `--cross-project` for `callers` (line 190) and, inaccurately, `trace` (line 191). The `callees` command has a full cross-project implementation — `cmd_callees` calls `CrossProjectContext::from_config` and `get_callees_cross` — but `cqs callees <name> --cross-project` does not appear in the README. Users reading the README would not know the flag is functional for callees.
- **Suggested fix:** Add `cqs callees <name> --cross-project   # Callees across all reference projects` to the Call Graph examples alongside the `callers` line.

#### DOC-39: README "How It Works" parse description lists 7 types but 29 exist
- **Difficulty:** easy
- **Location:** README.md:592
- **Description:** The "How It Works" section says "Tree-sitter extracts functions, classes, structs, enums, traits, constants, and documentation across 54 languages." This list was accurate for early versions. The project now has 29 chunk types including Test, Variable, Endpoint, Service, StoredProc, Extern, Namespace, Middleware, Modifier, ConfigKey, Impl, Extension, Constructor, TypeAlias, Delegate, Event, Module, Macro, Object, and Property — none of which appear. Users cannot discover from this description that `--include-type endpoint` or `--include-type test` exist.
- **Suggested fix:** Update line 592 to say something like "extracts 29 chunk types (functions, classes, structs, enums, traits, tests, endpoints, services, and more — see `--include-type` filters)".

## Resource Management

#### RM-3: `SpladeEncoder` ONNX session cannot be freed during batch mode idle — `check_idle_timeout` does not cover it
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:107-127, src/splade/mod.rs:47-51
- **Description:** `check_idle_timeout` clears the embedder and reranker ONNX sessions after 5 minutes of inactivity by calling `emb.clear_session()` and `rr.clear_session()`. Both of these hold `Mutex<Option<Session>>` so clearing sets the inner value to `None`, freeing the ~500MB ONNX model. `SpladeEncoder`, stored in `BatchContext.splade_encoder: OnceLock<Option<SpladeEncoder>>`, holds `session: Mutex<Session>` — not `Mutex<Option<Session>>`. There is no `clear_session` method on `SpladeEncoder`, and `OnceLock` cannot be cleared. Once the SPLADE encoder is loaded (~300MB for the BERT model), its ONNX session occupies memory for the entire batch session lifetime regardless of idle time. A user who runs one SPLADE search and then lets the session idle for hours retains the full SPLADE model in VRAM/RAM indefinitely. The embedder and reranker correctly implement the idle-release pattern; SPLADE is the only ONNX model that does not.
- **Suggested fix:** Change `SpladeEncoder.session` from `Mutex<Session>` to `Mutex<Option<Session>>`, add a `clear_session(&self)` method (same pattern as `Embedder::clear_session`), and call it from `check_idle_timeout` after clearing embedder and reranker.

#### RM-4: `EmbeddingCache::open` uses `new_multi_thread().worker_threads(1)` — spawns extra background thread unnecessarily
- **Difficulty:** easy
- **Location:** src/cache.rs:65-69
- **Description:** `EmbeddingCache::open` creates its internal Tokio runtime with `tokio::runtime::Builder::new_multi_thread().worker_threads(1)`. A multi-thread runtime with 1 worker still spawns an I/O driver background thread (the "epoll/kqueue thread"), giving 2 OS threads total. `new_current_thread()` would use 1 thread — the calling thread — which is correct since the pool is `max_connections(1)` and all async work is invoked via `rt.block_on(...)` from a single synchronous call site. The test at line 600 in the same file correctly uses `new_current_thread()`. `Store::open_light` and `Store::open_readonly` were already fixed (PB-1) to use `new_current_thread()` for the same reason. The fix was not applied to `EmbeddingCache`.
- **Suggested fix:** Replace `tokio::runtime::Builder::new_multi_thread().worker_threads(1)` with `tokio::runtime::Builder::new_current_thread()` at `src/cache.rs:65-66`. This matches the test runtime at line 600 and is consistent with the rationale documented in the PB-1 fix for `Store::open_light`.

#### RM-5: `EmbeddingCache` pool has no `idle_timeout` — WAL lock held indefinitely when cache is idle
- **Difficulty:** easy
- **Location:** src/cache.rs:79-83
- **Description:** The `SqlitePoolOptions` for `EmbeddingCache` sets only `max_connections(1)` with no `idle_timeout`. A SQLite WAL-mode connection holds a shared lock as long as it is open. Since the cache pool keeps its single connection open indefinitely, the WAL cannot be checkpointed by any external process while the pool lives. `Store::open_with_config` sets `.idle_timeout(Duration::from_secs(30))` (PB-2 fix) specifically to release WAL locks during idle periods. The cache pool, opened at the start of `run_index_pipeline` and dropped at function return, is short-lived for CLI use. However, if the cache were ever held across multiple pipeline runs (e.g., in a long-running daemon), the missing `idle_timeout` would prevent WAL truncation. Additionally, `EmbeddingCache` has no `Drop` implementation — unlike `Store`, which issues `PRAGMA wal_checkpoint(TRUNCATE)` on drop, the cache pool closes connections but never checkpoints the WAL, leaving the WAL file to grow until the next open.
- **Suggested fix:** Add `.idle_timeout(Duration::from_secs(30))` to the `SqlitePoolOptions` call at line 80. Add a `Drop` implementation that calls `PRAGMA wal_checkpoint(TRUNCATE)` before closing the pool, mirroring `Store::drop`.

#### RM-6: `load_all_sparse_vectors` materializes all rows with `fetch_all` before building the index — peak memory is 3× final index size
- **Difficulty:** medium
- **Location:** src/store/sparse.rs:69-108
- **Description:** `load_all_sparse_vectors` fetches all sparse vector rows using `.fetch_all(&self.pool).await?`, which materializes the entire result set into a `Vec<_>` before any processing. It then iterates this `rows` Vec to build a second `Vec<(String, SparseVector)>`. Both Vecs exist simultaneously in memory. When `SpladeIndex::build` is subsequently called, the input Vec (containing all chunk sparse vectors) and the built `postings: HashMap<u32, Vec<(usize, f32)>>` + `id_map: Vec<String>` also coexist briefly. For a codebase with 100K chunks and ~10 tokens per chunk average, the `rows` Vec holds ~1M `sqlx::sqlite::SqliteRow` objects (each with 3 fields). Peak RSS during SPLADE index construction is ~3× the final index footprint. For very large codebases with SPLADE enabled this can be 500MB–1GB of transient memory.
- **Suggested fix:** Replace `fetch_all` with `fetch` (streaming) and accumulate directly into the `result` Vec without materializing all rows first. Use `use futures::StreamExt; let mut stream = query.fetch(&self.pool);` and process each row as it arrives. This reduces peak memory to approximately the final index size.

## Algorithm Correctness

#### AC-4: `paired_bootstrap` p-value can exceed 1.0 — one-sided proportion doubled without clamping
- **Difficulty:** easy
- **Location:** tests/eval_common.rs:234-244
- **Description:** `paired_bootstrap` computes the two-sided p-value by counting the proportion of bootstrap samples on the "opposite side of zero" from the observed delta, then multiplying by 2.0. The one-sided proportion can exceed 0.5 when the observed delta is near zero and the bootstrap distribution is symmetric, giving `p_value * 2.0 > 1.0`. Example: `observed_delta = 0.001` (barely positive), 60% of bootstrap samples <= 0 → returned p-value = 1.2. The existing test (`test_paired_bootstrap_identical`) uses `assert!(p > 0.5)` which passes for values up to 2.0, so it does not catch invalid p-values. The correct two-sided p-value is `2 * min(proportion_left, proportion_right)` clamped to [0, 1].
- **Suggested fix:** Clamp the result: `(p_value * 2.0).min(1.0)`. Alternatively use `boot_deltas.iter().filter(|&&d| d.abs() >= observed_delta.abs()).count() as f64 / n_resamples as f64` which is naturally bounded [0, 1].

#### AC-5: `bootstrap_ci` lower CI index is off-by-one — gives 2.51th percentile, not 2.5th
- **Difficulty:** easy
- **Location:** tests/eval_common.rs:187
- **Description:** `lo_idx = (n_resamples as f64 * 0.025) as usize` truncates via `as usize`. With `n_resamples = 10000`, `0.025 * 10000 = 250.0` → `lo_idx = 250`. Index 250 in the sorted estimates vector is the 251st smallest sample (the 2.51th percentile), not the 2.5th. The 2.5th percentile is at index 249. The upper bound correctly uses `ceil(0.975 * 10000) - 1 = 9749`. The asymmetry (floor for lower, ceil-1 for upper) makes the CI slightly narrower on the lower side than the stated 95% coverage — anti-conservative.
- **Suggested fix:** Change `lo_idx = (n_resamples as f64 * 0.025) as usize` to `lo_idx = ((n_resamples as f64 * 0.025).ceil() as usize).saturating_sub(1)`. This gives index 249 for n=10000, symmetric with the `ceil - 1` convention used for the upper bound.

#### AC-6: `bfs_expand` sets `expansion_capped = true` when seeds alone fill the node cap — no BFS expansion occurs but user sees a misleading warning
- **Difficulty:** easy
- **Location:** src/gather.rs:319-322
- **Description:** `bfs_expand` receives `name_scores` pre-populated with seed nodes. The cap check at line 319 (`if name_scores.len() >= opts.max_expanded_nodes`) fires before any neighbor expansion. If the number of seeds already equals or exceeds `max_expanded_nodes`, the function sets `expansion_capped = true` and breaks immediately without traversing a single neighbor. The CLI then emits a "BFS expansion capped" warning to the user. No BFS was cut short — seeds simply consumed the full node budget before any traversal began. A user with `CQS_GATHER_MAX_NODES=5` and 5 seed results will always see this warning even though the configuration is intentional.
- **Suggested fix:** Record the seed count before BFS (`let initial_size = name_scores.len()`) and only return `expansion_capped = true` when nodes were actually added via BFS: `name_scores.len() > initial_size && name_scores.len() >= opts.max_expanded_nodes`. When seeds alone fill the cap, return `false`.

#### AC-7: `VectorIndex::search_with_filter` default over-fetches only 3x — highly selective filters silently under-return
- **Difficulty:** medium
- **Location:** src/index.rs:55
- **Description:** The default `search_with_filter` implementation fetches `k * 3` unfiltered candidates then post-filters. With a highly selective filter (e.g., `--include-type endpoint --lang go` on a codebase where 2% of chunks match), the expected passing count from a 3x over-fetch is `0.06 * k` — far fewer than k. The caller receives an under-sized result set with no warning. The HNSW override handles this correctly via traversal-time filtering; only non-HNSW indexes (CAGRA, mock, future custom implementations) use this default. There is no warning or retry mechanism when post-filter yields fewer than k results.
- **Suggested fix:** Add a `tracing::warn!` when `results.len() < k` after filtering to surface the under-return. For correctness, implement iterative over-fetching: if the first pass returns fewer than k results, double the fetch count and retry until k results pass or the index is exhausted.

#### AC-8: SPLADE hybrid search sorts fused scores with `partial_cmp` + `unwrap_or(Equal)` — NaN treated as equal to all scores
- **Difficulty:** easy
- **Location:** src/search/query.rs:443-447
- **Description:** The `fused` vector in `search_hybrid` is sorted with `b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)`. `partial_cmp` on `f32` returns `None` only for NaN; the fallback treats NaN as tied with every score, producing non-deterministic ordering. Every other sort in the search pipeline uses `total_cmp` (line 652 in the same file, and throughout `scoring/candidate.rs`), which gives NaN a consistent last position. The fused score is computed from finite source scores so NaN is unlikely in practice, but the inconsistency is a latent correctness hazard.
- **Suggested fix:** Replace `b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)` with `b.score.total_cmp(&a.score)`. One-line change, identical behavior for finite inputs.

#### AC-9: `test_reachability` equivalence-class optimization over-counts tests whose transitive reach diverges after depth 1
- **Difficulty:** medium
- **Location:** src/impact/bfs.rs:241-300
- **Description:** The optimization groups tests by their set of direct callees and performs BFS once per class, multiplying reachability counts by `class_size`. This is correct only when all tests in a class have identical transitive reach. Two tests can share all direct callees (same class, `class_size=2`) while having different transitive reach: if test_a and test_b both call helper(), and helper()'s call graph reaches both path_a() and path_b() (regardless of which test actually exercises which path), both path_a and path_b get count=2. The correct counts are 1 each. The over-count causes `compute_risk_and_tests` to report false test coverage, potentially masking under-tested functions. The doc comment's "Limitation" note says "counts are accurate" — this is incorrect for the transitive-diverge case.
- **Suggested fix:** Correct the doc comment: "counts may over-count when tests in the same class have different transitive reach." For high-accuracy attribution, run BFS once per test node. The optimization is acceptable as an approximation for performance, but the correctness claim in the doc comment should be removed.

## Platform Behavior

#### PB-1: `prune_all` missing absolute/relative path suffix fallback — GC can falsely delete chunks

- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:155-158
- **Description:** `prune_all` (used by `cqs gc` and batch GC) determines which DB origins are missing by doing `!existing_files.contains(&origin_path)` on non-macOS. `prune_missing` (used by `cqs index`) adds a suffix-match fallback after the exact-match check to handle absolute/relative path mismatches: if the stored origin is an absolute path and `existing_files` contains relative paths (or vice versa), the suffix check saves the chunk from false deletion. `prune_all` has no such fallback. In a scenario where `existing_files` contains paths with a different prefix representation than the DB origins — e.g., if a reference store's `enumerate_files` returns absolute paths — `prune_all` would falsely classify every file as missing and delete all chunks. The macOS case-fold fix is present in both functions, but the suffix fallback is absent from `prune_all`.
- **Suggested fix:** Copy the suffix-match fallback from `prune_missing` (lines 62-70) into the `#[cfg(not(target_os = "macos"))]` branch of `prune_all` (line 155-158). Extract a shared helper `fn is_origin_present(origin_path: &Path, existing: &HashSet<PathBuf>) -> bool` used by both functions to prevent future divergence.

#### PB-2: `list_stale_files` missing macOS case-fold normalization — stale count wrong on APFS

- **Difficulty:** easy
- **Location:** src/store/chunks/staleness.rs:277-278
- **Description:** `list_stale_files` (displayed by `cqs stale`) checks `existing_files.contains(&PathBuf::from(&origin))` at line 278 to determine whether a DB origin file is still on disk. On macOS (case-insensitive APFS/HFS+), a file indexed as `src/MyFile.rs` and later renamed to `src/myfile.rs` (case-only rename) would have the old case in the DB. `PathBuf::from("src/MyFile.rs")` != `PathBuf::from("src/myfile.rs")` in a case-sensitive HashSet comparison, so the file is reported as missing even though it exists. Both `prune_missing` and `prune_all` apply `#[cfg(target_os = "macos")]` to normalize both sides to lowercase before comparison. `list_stale_files` does not have this guard. The result is that `cqs stale` over-counts missing files on macOS when case-only renames have occurred.
- **Suggested fix:** Apply the same `#[cfg(target_os = "macos")]` case-fold normalization used in `prune_missing` (lines 52-57) to the `existing_files.contains` check in `list_stale_files` (line 278).

#### PB-3: WSL watch auto-poll detection hardcodes `/mnt/` prefix — misses custom `automount.root`

- **Difficulty:** easy
- **Location:** src/cli/watch.rs:193-197
- **Description:** `cmd_watch` auto-selects poll mode when `is_wsl()` is true and the project root starts with `"/mnt/"` or `"//wsl"`. This correctly handles the default WSL automount at `/mnt/c/`. However, WSL2 allows customizing the mount root via `automount.root` in `/etc/wsl.conf`. A user who sets `automount.root = /win/` has Windows drives at `/win/c/`, `/win/d/` etc. For such a user, a project at `/win/c/Projects/...` would be on a DrvFS mount (NTFS over 9P, where inotify is unreliable), but the `/win/` prefix check fails and `use_poll` remains false. `RecommendedWatcher` (inotify) is used, silently missing file change events. The existing `tracing::warn!` at line 199-200 does not fire because the code thinks the project is NOT on a Windows mount.
- **Suggested fix:** Read `/proc/mounts` (or `/proc/self/mountinfo`) to check whether the project root's filesystem type is `9p` or `drvfs`, rather than relying on a path prefix. Alternatively, read `automount.root` from `/etc/wsl.conf` when `is_wsl()` is true and substitute that prefix into the check. A pragmatic middle ground: emit the advisory warning whenever `is_wsl()` is true regardless of path, not only for `/mnt/` paths.

#### PB-4: `atomic_write` in `rewriter.rs` falls back to direct `fs::write` on any rename failure

- **Difficulty:** easy
- **Location:** src/doc_writer/rewriter.rs:472-479
- **Description:** `atomic_write` writes to a temp file and renames it over the target. On rename failure it unconditionally removes the temp file and falls back to `std::fs::write(path, data)` — a non-atomic direct write. On Windows, `fs::rename` fails with `ERROR_SHARING_VIOLATION` (error 32) when the target file is open in another process (mandatory locking), not just on cross-device errors. The fallback then tries `fs::write`, which opens and truncates the target file; if the same lock prevents the write, the error from `fs::write` is returned but the original rename error (which explains _why_ the atomic write failed) is lost. More critically, if `fs::write` succeeds during this window, the file is written non-atomically: a crash mid-write leaves corrupted source. The compare-and-exchange contract of `rewrite_file` is violated. Compare with `note.rs:302-323` and `config.rs:461-478`, which use a copy-to-same-directory-then-rename pattern to preserve atomicity on cross-device failures.
- **Suggested fix:** Match the pattern used in `note.rs`: on rename failure, copy `temp_path` to a second temp in the _same directory as `path`_ (guaranteed same device), then rename the second temp to `path`. Only fall back to direct write if the second rename also fails. This preserves atomicity in the cross-device case without silently degrading to non-atomic on locking failures.

#### PB-5: `libc::atexit` cleanup handler allocates memory via `Mutex::lock()` — UB on process exit

- **Difficulty:** hard
- **Location:** src/embedder/provider.rs:177-188
- **Description:** `register_provider_cleanup` (Linux-only, `#[cfg(target_os = "linux")]`) registers a `cleanup` function via `unsafe { libc::atexit(cleanup) }`. The cleanup function calls `CLEANUP_PATHS.lock()`, which acquires a `std::sync::Mutex`. On Rust, atexit handlers run _after_ `main` returns but _before_ the process image exits. Rust's global allocator (jemalloc or the system allocator) may have already been deregistered by Rust's own runtime cleanup, making calls that allocate — including mutex lock in some configurations — technically undefined behavior. In practice, with the default system allocator on Linux, this works because glibc's `malloc` outlives `atexit` handlers. But if the project ever switches to a custom allocator with explicit teardown, or if compiled with `-Cmiracle-allocator`, the handler could allocate after the allocator is torn down. Additionally, panics inside `atexit` handlers are UB (Rust's panic handler may not be installed at atexit time), and `CLEANUP_PATHS.lock()` panics on a poisoned mutex.
- **Suggested fix:** Use `AtomicBool` + a statically-allocated array, or replace the `atexit` approach with a `Drop`-implementing RAII guard that cleans up symlinks when the guard is dropped at the end of `main`. The provider session object could hold a `CleanupGuard` field. This eliminates `unsafe` and the allocator dependency entirely.
