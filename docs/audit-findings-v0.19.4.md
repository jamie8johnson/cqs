# Audit Findings — v0.19.2

Generated: 2026-02-27

## API Design

#### AD-1: `GatherDirection` passed as raw string through CLI/batch instead of using clap ValueEnum
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:506, src/cli/batch/commands.rs:98, src/cli/commands/gather.rs:36
- **Description:** The `gather` command's `--direction` parameter is typed as `String` in the CLI definition and parsed manually via `FromStr` in the handler (`direction.parse().map_err(...)`). `GatherDirection` already implements `FromStr` and has exactly 3 valid values (`both`, `callers`, `callees`). Using clap's `ValueEnum` derive on `GatherDirection` would give better help text (showing valid values), automatic validation with a proper error message, and shell completion. Every other enum-like parameter in the CLI (`OutputFormat`, `DeadConfidenceLevel`, `GateLevel`) uses `ValueEnum` — `GatherDirection` is the sole outlier.
- **Suggested fix:** Add `#[derive(clap::ValueEnum)]` to `GatherDirection`, change the CLI arg type from `String` to `GatherDirection` in both `Commands::Gather` and `BatchCmd::Gather`.

#### AD-2: `audit-mode` state argument is `Option<String>` instead of an enum
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:541, src/cli/commands/audit_mode.rs:51-96
- **Description:** `cqs audit-mode [state]` accepts an `Option<String>` for `state`, then manually matches on `"on"` / `"off"` with a catch-all error: `"Invalid state '{}'. Use 'on' or 'off'."`. This should be a proper clap `ValueEnum` with two variants, which gives better `--help` output (showing valid values), shell completions, and moves validation to the parser. The `None` case (query current state) can be handled by keeping it `Option<AuditState>`.
- **Suggested fix:** Create `enum AuditState { On, Off }` with `#[derive(clap::ValueEnum)]`, change the arg to `Option<AuditState>`.

#### AD-3: Inconsistent positional argument naming for "query-like" inputs across commands
- **Difficulty:** easy
- **Location:** src/cli/mod.rs (multiple commands), src/cli/batch/commands.rs (multiple commands)
- **Description:** Commands that take a natural language input use inconsistent names for the same semantic concept — a free-text search/description string:
  - `Scout` CLI: `task: String` but Batch: `query: String`
  - `Onboard` CLI: `concept: String` but Batch: `query: String`
  - `Gather`: `query: String` (consistent)
  - `Where`: `description: String` (consistent)
  - `Task`: `description: String` (consistent)

  The batch/CLI split is the most confusing: `cqs scout "error handling"` sees the positional as `task`, but `batch> scout "error handling"` sees it as `query`. The help text diverges too. This creates friction when reading code — you have to remember which name each command chose.
- **Suggested fix:** Standardize on `query` for search-like inputs (scout, onboard, gather) and `description` for creation-like inputs (where, task). Update batch to match CLI naming or vice versa.

#### AD-4: Inconsistent `--json` flag pattern — some commands have both `--format` and `--json`
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:377-381 (Impact), 411-415 (Review), 429-433 (Ci), 451-455 (Trace)
- **Description:** Four commands (Impact, Review, Ci, Trace) carry both `--format` (a `ValueEnum` with text/json/mermaid) and `--json` (a bool alias). The dispatch code then does `let fmt = if json { &OutputFormat::Json } else { format }`. All other commands (30+) use only `--json`. The dual-flag pattern means `--format json --json false` has no clear precedence (json flag wins), and the `--json` flag description says "(alias for --format json)" which is only present on 4 of ~35 commands. Users of the batch interface don't even have either option — all batch output is JSON. This inconsistency confuses users expecting `--format` to work on other commands.
- **Suggested fix:** Remove `--json` from Impact/Review/Ci/Trace and rely solely on `--format`. Or add `--format` globally and deprecate per-command `--json`. The mermaid format is only relevant to Impact and Trace, so a middle ground is: keep `--format` only on commands that support mermaid, and `--json` everywhere else.

#### AD-5: Several cmd_ handlers accept `_cli: &Cli` but never use it
- **Difficulty:** easy
- **Location:** src/cli/commands/deps.rs:14, graph.rs:11, graph.rs:59, doctor.rs:15, project.rs:46, impact.rs:11, review.rs:9
- **Description:** Seven command handlers take `_cli: &Cli` as their first parameter (prefixed with `_` to silence the unused warning) but never access any field from it. Meanwhile, some commands that *should* use `cli` for staleness checks or quiet mode (e.g., `cmd_deps`, `cmd_callers`) don't. The `_cli` parameter is passed through from the dispatcher just for uniformity, but it's misleading — it suggests the function might need CLI state when it doesn't. This is a code smell, not a bug, but it creates confusion when reading unfamiliar handlers: "what does this use `cli` for?"
- **Suggested fix:** Remove `_cli` from handlers that don't use it. If future uniformity is desired, a trait-based dispatch would be cleaner. This is low priority given no external users.

#### AD-6: `ScoutResult`, `TaskResult`, `PlacementResult`, `GatherResult` missing `Serialize` derive
- **Difficulty:** medium
- **Location:** src/scout.rs:81, src/task.rs:29, src/where_to_add.rs:67, src/gather.rs:147
- **Description:** Result types returned from library functions have inconsistent `Serialize` derives. `DriftResult`, `ImpactResult`, `OnboardResult` derive `Serialize`, but `ScoutResult`, `TaskResult`, `PlacementResult`, `GatherResult`, `DiffResult`, and `RelatedResult` do not. The non-serializable types force each CLI/batch handler to hand-build JSON via `serde_json::json!()` blocks, which is exactly what the prior audit's P2 item AD-8 ("HealthReport missing Serialize") fixed for `HealthReport`. The same pattern applies to these types. Having some result types serializable and others not is an API consistency issue — callers can't generically handle result → JSON conversion.
- **Suggested fix:** Add `#[derive(serde::Serialize)]` to `ScoutResult`, `ScoutChunk`, `ScoutSummary`, `FileGroup`, `ChunkRole`, `TaskResult`, `TaskSummary`, `PlacementResult`, `FileSuggestion`, `LocalPatterns`, `GatherResult`, `GatheredChunk`, `DiffResult`, `DiffEntry`, `RelatedResult`, `RelatedFunction`. Then simplify the CLI/batch handlers to use `serde_json::to_value()` instead of manual JSON assembly.

#### AD-7: `suggest_placement` has 4 API variants that could be 1 with an options struct
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:101-152
- **Description:** The placement API exposes 4 public functions: `suggest_placement()`, `suggest_placement_with_embedding()`, `suggest_placement_with_options()`, and the private `suggest_placement_with_embedding_and_options()`. This combinatorial explosion (embedding: yes/no × options: yes/no) is the classic builder-avoidance anti-pattern. Compare with `scout`/`gather` which use an options struct with defaults — they have 2 variants max (`scout()` + `scout_with_options()`). The `where_to_add` module went further by also factoring on "pre-computed embedding" as a separate axis.
- **Suggested fix:** Fold the embedding parameter into `PlacementOptions` as `pub query_embedding: Option<Embedding>`. Then there are 2 functions: `suggest_placement(store, embedder, desc, limit)` (convenience) and `suggest_placement_with_options(store, embedder, desc, limit, opts)` (full control). The `_with_embedding` variant becomes `PlacementOptions { query_embedding: Some(emb), ..Default::default() }`.

#### AD-8: `cmd_diff` and `cmd_drift` have near-identical reference resolution boilerplate
- **Difficulty:** medium
- **Location:** src/cli/commands/diff.rs:11-76, src/cli/commands/drift.rs:11-54
- **Description:** Both `cmd_diff` and `cmd_drift` contain ~30 lines of identical reference resolution logic: load config, find reference by name with the same error message format, check if `index.db` exists with the same bail message, open the store. This pattern appears in 4 places: `cmd_diff` (source + target), `cmd_drift`, and `cmd_gather` (ref_name). Each copies the same error message templates. A shared `resolve_reference_store(name: &str, config: &Config) -> Result<Store>` helper would deduplicate this and ensure consistent error messages.
- **Suggested fix:** Add a `resolve_reference_store()` helper in `cli/commands/resolve.rs` (which already exists for `resolve_target`). Use it in `cmd_diff`, `cmd_drift`, and `cmd_gather`.

#### AD-9: `SearchFilter` uses `pub` fields with a `new()` that duplicates `Default`
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:406-479
- **Description:** `SearchFilter` has all public fields, implements `Default`, and also has a `fn new() -> Self` that just calls `Self::default()`. The `with_query()` builder method is the only builder, and it's rarely used — most callers construct via struct literal with `..SearchFilter::default()`. The `new()` method adds no value over `Default::default()` and having both creates the question "which should I use?" This is a minor inconsistency but the `new()` method is dead weight.
- **Suggested fix:** Remove `SearchFilter::new()` and update callers to use `SearchFilter::default()` or `SearchFilter { ..Default::default() }`. Keep `with_query()` as it provides a fluent chain.

#### AD-10: `StoreError::Runtime` is a catch-all variant masking unrelated errors
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:34, src/search.rs:62-63
- **Description:** `StoreError::Runtime(String)` is documented as "catch-all for errors that don't fit other variants" and is used for at least 3 unrelated purposes: (1) tokio runtime initialization failures, (2) "No function found matching" in `resolve_target`, (3) JSON serialization failures. Using it for "not found" errors in `resolve_target` is particularly problematic — callers can't distinguish "the function doesn't exist" from "the runtime crashed" without string-matching the error message. This makes error handling brittle.
- **Suggested fix:** Add a `StoreError::NotFound(String)` variant for `resolve_target` and similar lookup failures. This lets callers match on the variant for retry/suggest logic without parsing error messages.

## Documentation

#### DOC-1: lib.rs quick start example uses invalid `SearchFilter` configuration
- **Difficulty:** easy
- **Location:** src/lib.rs:35-36
- **Description:** The doc example creates `SearchFilter { enable_rrf: true, ..Default::default() }` but doesn't set `query_text`. Since v0.19.2, `SearchFilter::validate()` (helpers.rs:500-502) returns an error when `enable_rrf` is true and `query_text` is empty: `"query_text required when name_boost > 0 or enable_rrf is true"`. The example code would fail at runtime if anyone tried to use it. The comment says "// Search for similar code (hybrid RRF search)" but the filter as constructed would error, not perform RRF.
- **Suggested fix:** Either set `query_text` in the example: `SearchFilter { enable_rrf: true, query_text: "parse configuration file".to_string(), ..Default::default() }`, or drop `enable_rrf` and use the default (embedding-only) search, which matches what the example actually demonstrates.

#### DOC-2: CONTRIBUTING.md lists C++, Kotlin, Swift as "Feature Ideas" — all are already implemented
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:70
- **Description:** The "Feature Ideas" section says "Additional language support (tree-sitter grammars: C++, Kotlin, Swift, and more)". All three were added in v0.18.0 (C++) and v0.19.0 (Kotlin, Swift). This suggests to contributors they should work on adding these languages, when they're already done.
- **Suggested fix:** Remove C++, Kotlin, and Swift from the list. Replace with actually-unimplemented languages like Elixir, Lua, PHP, or Dart. Or simplify to "Additional language support (see `src/language/` for current list)".

#### DOC-3: CONTRIBUTING.md architecture lists phantom `deps.rs` at top level
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:172
- **Description:** Line 172 lists `deps.rs - Type-level dependency impact analysis` as a top-level `src/` file. No `src/deps.rs` exists. There's only `src/cli/commands/deps.rs` (the CLI command handler), which is correctly listed at line 91. The `deps` CLI command calls into `store::types` and `impact` modules directly — there's no standalone `deps.rs` analysis module.
- **Suggested fix:** Remove line 172 (`deps.rs - Type-level dependency impact analysis`) from the top-level source listing.

#### DOC-4: README Claude Code Integration section says `cqs project add` — actual command is `cqs project register`
- **Difficulty:** easy
- **Location:** README.md:402
- **Description:** The Claude Code Integration section's command reference says `cqs project add/remove/list`. The actual subcommand is `register`, not `add` (`src/cli/commands/project.rs:17`). The README's own "Cross-project search" section at line 257 correctly says `cqs project register`. The CLAUDE.md is not affected (it doesn't mention project commands in the quick reference).
- **Suggested fix:** Change `cqs project add/remove/list` to `cqs project register/remove/list/search` on line 402.

#### DOC-5: README HNSW table says `ef_search = 100` (fixed) — actually adaptive since v0.19.2
- **Difficulty:** easy
- **Location:** README.md:463
- **Description:** The HNSW Index Tuning table says `ef_search | 100 | Search width at query time`. Since v0.19.2 (PR #499), ef_search is adaptive: `EF_SEARCH.max(k * 2).min(index_size.max(EF_SEARCH))` (hnsw/search.rs:41). The constant 100 is the *baseline*, but actual search width scales with k and index size. The trade-offs section below the table also implies ef_search is static ("Higher ef_search improves recall but slows queries").
- **Suggested fix:** Update the table to: `ef_search | 100 (adaptive) | Baseline search width; actual value scales with k and index size`. Update trade-offs to note the adaptive behavior.

#### DOC-6: SECURITY.md claims `PRAGMA quick_check` runs "on every database open" — `open_readonly` skips it
- **Difficulty:** easy
- **Location:** SECURITY.md:20, src/store/mod.rs:279-339
- **Description:** SECURITY.md line 20 states "Database corruption: `PRAGMA quick_check` on every database open". However, `Store::open_readonly()` (used for reference stores and background builds) does not run `PRAGMA quick_check` — it only runs `check_schema_version`, `check_model_version`, and `check_cq_version`. Only `Store::open()` runs the integrity check. This means reference indexes loaded via `open_readonly` skip corruption detection.
- **Suggested fix:** Either (a) add `PRAGMA quick_check` to `open_readonly`, or (b) update SECURITY.md to say "on read-write database open" to accurately reflect the behavior. Option (a) is safer but adds latency to reference loading.

#### DOC-7: CONTRIBUTING source/ architecture section doesn't note the module is unused/reserved
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:108-110
- **Description:** The architecture overview lists `source/` with "Source trait" and "File-based source implementation" as if it's an active part of the codebase. The module's own doc comment says "Not yet wired into the indexing pipeline — reserved for future use" and has `#![allow(dead_code)]`. A contributor following the architecture guide would assume `Source` is used in the indexing flow, when it isn't.
- **Suggested fix:** Add "(reserved — not yet wired into indexing pipeline)" after the source/ description in the architecture section.

## Observability

#### OB-1: Store module has zero tracing spans on performance-critical operations
- **Difficulty:** medium
- **Location:** src/store/calls.rs:469, src/store/calls.rs:707, src/store/calls.rs:1026, src/store/chunks.rs:46, src/store/chunks.rs:454, src/store/chunks.rs:638, src/store/chunks.rs:1145, src/store/notes.rs:134
- **Description:** The entire `store/` module — containing the heaviest database operations — has zero `tracing::info_span!` calls. This includes `get_call_graph()` (loads up to 500K edges), `find_dead_code()` (multi-phase SQL with filter/score logic), `find_test_chunks()`, `prune_missing()` (batch delete), `upsert_chunks_batch()` (multi-row INSERT), `search_by_names_batch()`, `check_origins_stale()`, and `search_notes()`. These operations involve SQL queries that take 10-500ms depending on index size, but there's no way to see where time is spent with `RUST_LOG=info`. By contrast, every analysis module (scout, gather, task, impact, etc.) properly instruments entry points.
- **Suggested fix:** Add `tracing::info_span!` with relevant parameters at the entry of each public method listed above. At minimum: `get_call_graph`, `find_dead_code`, `find_test_chunks`, `prune_missing`, `upsert_chunks_batch`, `search_by_names_batch`, `check_origins_stale`, `search_notes`.

#### OB-2: `parse_notes()` errors silently swallowed in `read` command
- **Difficulty:** easy
- **Location:** src/cli/commands/read.rs:280, src/cli/commands/read.rs:312
- **Description:** Both `cmd_read_file()` and `cmd_read_focused()` call `parse_notes(&notes_path).unwrap_or_default()` without logging the error. If `notes.toml` is malformed or locked, the user gets no indication — notes silently disappear from `cqs read` output. The batch handler (src/cli/batch/mod.rs:150) correctly logs `tracing::warn!` when `parse_notes` fails, so the correct pattern exists — just not applied consistently.
- **Suggested fix:** Replace `parse_notes(&notes_path).unwrap_or_default()` with a match that logs `tracing::warn!` on `Err`, matching the batch handler pattern.

#### OB-3: `search_by_names_batch()` error swallowed in `cmd_read_focused`
- **Difficulty:** easy
- **Location:** src/cli/commands/read.rs:223-224
- **Description:** `store.search_by_names_batch(&type_names, 5).unwrap_or_default()` silently swallows a store error. If the SQL query for type resolution fails, focused read silently omits type definitions with no warning. This makes debugging partial `cqs read --focus` output difficult.
- **Suggested fix:** Replace with a match that logs `tracing::warn!` on `Err`.

#### OB-4: `get_call_graph()` doesn't log edge count or warn on truncation
- **Difficulty:** easy
- **Location:** src/store/calls.rs:469-498
- **Description:** `get_call_graph()` loads up to 500K edges with `LIMIT ?1` but never logs how many edges were loaded. More critically, if the limit is hit (rows.len() == 500,000), the call graph is silently truncated — callers (scout, task, onboard, review, gather) get incomplete data with no warning. Typical projects have ~2K edges so this rarely fires, but adversarial or very large codebases could hit it.
- **Suggested fix:** Add `tracing::info!(edges = rows.len(), "Call graph loaded")` before returning. If `rows.len() as i64 == MAX_CALL_GRAPH_EDGES`, also log `tracing::warn!("Call graph truncated at {} edges — analysis may be incomplete", MAX_CALL_GRAPH_EDGES)`.

#### OB-5: `Store::open()` and `Store::open_readonly()` lack timing span
- **Difficulty:** easy
- **Location:** src/store/mod.rs:175, src/store/mod.rs:279
- **Description:** `Store::open()` performs pool creation, 7 PRAGMAs per connection, integrity check, schema version check, and model version check. This takes 50-200ms but has no enclosing span. The `tracing::info!("Database connected")` at line 252 fires after pool creation but before integrity/schema checks, so it doesn't capture total open time. `open_readonly()` similarly has no span. When debugging slow startup, the store open time is invisible.
- **Suggested fix:** Add `tracing::info_span!("store_open", path = %path.display()).entered()` at the start of both methods.

#### OB-6: `search_across_projects()` missing entry span
- **Difficulty:** easy
- **Location:** src/project.rs:155
- **Description:** `search_across_projects()` iterates over all registered projects, opens stores, and runs filtered search. Individual failures are logged with `tracing::warn!`, but there's no entry span or summary logging. When cross-project search is slow, total elapsed time and the count of projects searched vs skipped are invisible.
- **Suggested fix:** Add `tracing::info_span!("search_across_projects", project_count = registry.project.len()).entered()` after loading the registry, and log result count before returning.

#### OB-7: `gather()` doesn't log BFS expansion results
- **Difficulty:** easy
- **Location:** src/gather.rs (main `gather` and `gather_cross_index` functions)
- **Description:** The `gather()` function has an `info_span!` at entry and logs seed search results, but doesn't log how many nodes BFS expanded to, whether expansion was capped, or the final chunk count after deduplication. The `bfs_expand()` helper returns an `expansion_capped` boolean that is silently consumed by the caller. When gather returns fewer results than expected, there's no trace indicating whether the cause was too few seeds, BFS capping, or deduplication.
- **Suggested fix:** Add `tracing::info!(expanded = name_scores.len(), capped = expansion_capped, "BFS expansion complete")` after `bfs_expand()`, and log final `chunks.len()` before returning.

#### OB-8: HNSW `build_batched()` lacks per-batch progress logging
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs
- **Description:** `build_batched()` streams embeddings in batches for large indexes but only logs the start ("Building HNSW index batched...") and end ("HNSW index built"). For indexes with >50k vectors, the build can take 10+ seconds with no intermediate progress indication. This makes it impossible to tell whether a long index build is progressing or stuck.
- **Suggested fix:** Add `tracing::debug!(batch = batch_num, vectors_so_far = total, "HNSW batch inserted")` in the batch loop.

#### OB-9: `find_dead_code()` has no span or result count logging
- **Difficulty:** easy
- **Location:** src/store/calls.rs:707
- **Description:** `find_dead_code()` runs a multi-phase SQL query (uncalled functions, test/entry-point filter, signature inspection, confidence scoring) that can take seconds on large codebases. It has no tracing span and doesn't log how many functions were analyzed or how many were classified as dead. When `cqs ci` or `cqs dead` reports results, the DB time is invisible in traces.
- **Suggested fix:** Add `tracing::info_span!("find_dead_code", include_pub).entered()` at entry. Log `tracing::info!(confident = confident.len(), possible = possibly_pub.len(), "Dead code analysis complete")` before returning.

## Error Handling

#### EH-1: `serde_json::to_string().unwrap()` in batch REPL can panic on non-serializable values
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:288, 313, 328, 336, 343, 353
- **Description:** The batch REPL loop serializes JSON output with `serde_json::to_string(&value).unwrap()` in 6 places. While `serde_json::json!()` values are always serializable, the `dispatch()` call at line 334 returns an arbitrary `serde_json::Value` constructed by each batch handler. Most handlers use `serde_json::json!()` (safe), but some use `serde_json::to_value()` which can fail if a type has a custom `Serialize` impl that errors (e.g., `HealthReport` with `Serialize` derive could contain `f32::NAN` which is rejected by serde_json by default). A panic in the batch REPL kills the entire batch session — agents lose all accumulated context.
- **Suggested fix:** Replace `.unwrap()` with `.unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {}"}}"#, e))` or use `serde_json::to_string(&value).map_err(|e| anyhow::anyhow!("JSON serialization failed: {e}"))?` to propagate the error cleanly. Since the REPL already handles broken-pipe by breaking, serialization errors should be reported as error JSON objects, not panics.

#### EH-2: `BatchContext` OnceLock accessors use `.unwrap()` relying on implicit init invariant
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:75, 86, 136, 169, 183, 194, 211
- **Description:** Seven `BatchContext` accessor methods follow the pattern: check `OnceLock::get()`, if empty initialize and `set()`, then `get().unwrap()`. The `unwrap()` is safe because `set()` was just called, but this relies on an invariant not enforced by the type system — if `set()` silently fails (it returns `Err` if another thread set first), the `get().unwrap()` still succeeds because *someone* set it. The pattern works but is fragile to refactoring. The `borrow_ref()` method at line 169 is more concerning: it does `Ref::map(map, |m| m.get(name).unwrap())` inside an `if map.contains_key(name)` guard. If the map is mutated between the `contains_key` check and the `get` call (not possible with `RefCell` borrow rules, but the pattern is non-obvious), it would panic.
- **Suggested fix:** Use `get_or_init` / `get_or_try_init` instead of the set-then-get pattern, which is both cleaner and eliminates the unwrap entirely. For `borrow_ref`, document the invariant in a comment or use `get(name).expect("checked contains_key above")` to make the assumption explicit.

#### EH-3: `gc.rs` silently ignores HNSW file deletion failures before rebuild
- **Difficulty:** easy
- **Location:** src/cli/commands/gc.rs:64-67
- **Description:** Before rebuilding the HNSW index, `cmd_gc` deletes the old index files with `let _ = std::fs::remove_file(...)` on 4 files (graph, data, checksum, id_map). If deletion fails (file locked by another process, permissions issue), the stale HNSW files remain and the subsequent `build_hnsw_index` will overwrite what it can. However, if only some files are deleted (e.g., graph succeeds but data fails), the index is left in an inconsistent state — the new graph file may reference data offsets in the old data file. The comment says "Delete the stale HNSW first so concurrent searches fall back to brute-force" but if deletion silently fails, this safety mechanism doesn't work.
- **Suggested fix:** Log warnings on deletion failures: `if let Err(e) = std::fs::remove_file(&path) { tracing::warn!(path = %path.display(), error = %e, "Failed to remove stale HNSW file"); }`. Consider making the rebuild conditional on successful deletion of at least the graph file.

#### EH-4: `cmd_ref_add` bare `?` on `Store::open` and `store.init` loses path context
- **Difficulty:** easy
- **Location:** src/cli/commands/reference.rs:104-105
- **Description:** Lines 104-105 do `Store::open(&db_path)?` and `store.init(&ModelInfo::default())?` with no `.context()`. If the store open fails (permissions, disk full), the user sees a raw SQLite error like "unable to open database file" with no indication which path or reference is involved. The same file correctly uses `.with_context()` on line 94 for directory creation but misses these two store operations. The second `Store::open(&db_path)?` at line 133 (after pipeline) has the same issue.
- **Suggested fix:** Add `.with_context(|| format!("Failed to open reference store at {}", db_path.display()))` to both `Store::open` calls.

#### EH-5: `cmd_diff` bare `?` on `Store::open` for source and target stores
- **Difficulty:** easy
- **Location:** src/cli/commands/diff.rs:46, 55, 75
- **Description:** `Store::open(&source_db)?` and `Store::open(&index_path)?` (lines 46, 55, 75) propagate store errors without context indicating which store (source reference, project, or target reference) failed to open. A SQLite error like "database disk image is malformed" would reach the user with no indication which of up to 3 stores is corrupted. The function already has good error messages for missing files (`bail!("Reference '{}' has no index...")`), but the actual `Store::open` calls lack equivalent context.
- **Suggested fix:** Add `.with_context(|| format!("Failed to open {} store at {}", label, path.display()))` where `label` is "source reference", "project", or "target reference".

#### EH-6: `AnalysisError::Embedder(String)` discards typed error chain
- **Difficulty:** medium
- **Location:** src/lib.rs:148, src/onboard.rs:113, src/scout.rs:145, src/task.rs:105, src/where_to_add.rs:150
- **Description:** `AnalysisError::Embedder(String)` converts `EmbedderError` to `String` via `.to_string()` at 4 call sites. This discards the error chain — `EmbedderError` has structured variants like `ModelNotFound`, `ChecksumMismatch`, `InferenceFailed`, `TokenizerError` that callers could match on. For example, a `ChecksumMismatch` error during scout would show the user "embedding failed: Checksum mismatch for /path/to/model.onnx: expected abc, got xyz" — the same information — but it can't be programmatically distinguished from "embedding failed: ONNX session creation failed". This matters for CLI commands that want to suggest different remediation (re-download model vs. check GPU drivers).
- **Suggested fix:** Change `AnalysisError::Embedder(String)` to `AnalysisError::Embedder(#[from] EmbedderError)`. The 4 call sites become `embedder.embed_query(text)?` with no explicit `map_err`. This preserves the error chain and enables `EmbedderError` variant matching in callers.

#### EH-7: `impact/analysis.rs` and `impact/diff.rs` swallow `search_by_names_batch` errors
- **Difficulty:** easy
- **Location:** src/impact/analysis.rs:72-77, src/impact/diff.rs:131-136
- **Description:** Both `build_caller_info()` and the impact-diff caller resolution use `store.search_by_names_batch(...).unwrap_or_else(|e| { tracing::warn!(...); HashMap::new() })`. While the warning is logged, returning an empty `HashMap` means the impact analysis silently omits all caller snippets — the `CallerDetail` entries are still created but with `snippet: None` for every caller. The user sees callers listed without code context and has no way to know snippets were supposed to be there. The `gather.rs` module handles this better — it sets a `search_degraded` flag that propagates to the CLI and displays a visible warning.
- **Suggested fix:** Propagate a `degraded` flag (like `GatherResult::search_degraded`) through `ImpactResult` so the CLI can display "Warning: caller snippets unavailable" when batch name search fails.

#### EH-8: `onboard.rs` uses `.unwrap()` on a guaranteed-non-empty iterator result
- **Difficulty:** easy
- **Location:** src/onboard.rs:128
- **Description:** Line 128 does `.or(results.first()).unwrap()` after checking `results.is_empty()` returns early at line 117. The `unwrap()` is logically safe (if `find()` returns `None`, `results.first()` is `Some` because the list is non-empty), but it's the only `unwrap()` in this module's production code. The safety depends on the early return 11 lines above, which is a non-local invariant that could break during refactoring.
- **Suggested fix:** Use `.expect("results guaranteed non-empty by early return above")` to document the invariant, or restructure to avoid the `unwrap` entirely: `let entry = results.iter().find(|r| is_callable_type(r.chunk.chunk_type)).or(results.first()).ok_or_else(|| AnalysisError::NotFound(...))?;`

## Code Quality

#### CQ-1: `cmd_query` repeats empty-result / exit / token-pack / display boilerplate across 5 code paths
- **Difficulty:** medium
- **Location:** src/cli/commands/query.rs:16-625 (301-line main function + 4 helper functions, 723 lines total)
- **Description:** `cmd_query` dispatches to 4 private sub-functions (`cmd_query_name_only`, `cmd_query_ref_only`, `cmd_query_ref_name_only`, plus its own inline body). All 5 code paths repeat the same patterns:
  1. Empty-result JSON: `r#"{{"results":[],"query":"{}","total":0}}"#` — 5 identical copies (lines 240, 302, 437, 547, 611)
  2. `std::process::exit(signal::ExitCode::NoResults as i32)` — 5 identical copies (lines 244, 306, 441, 551, 615)
  3. `json_overhead = if cli.json { JSON_OVERHEAD_PER_RESULT } else { 0 }` — 4 copies (lines 201, 448, 534, 597)
  4. Token-pack + display dispatch — each sub-function ends with nearly identical 15-line block
  5. Config load + reference lookup + error message — 2 copies (lines 489-500, 572-583)

  A bug fix to the empty-result format requires editing 5 locations. The file is the 4th largest CLI command module.
- **Suggested fix:** Extract `emit_empty_results(query, json) -> !` for the empty+exit pattern. Extract `json_overhead_for(cli) -> usize` one-liner. The token-pack + display tail could be a `emit_query_results()` generic over result type.

#### CQ-2: `run_index_pipeline` is 458 lines — the largest function in the codebase
- **Difficulty:** hard
- **Location:** src/cli/pipeline.rs:238-695
- **Description:** Handles 6 concerns in a single function scope: channel creation, parser thread (with file batching, ID rewriting, mtime caching), GPU embedder thread (with failure routing to CPU), CPU fallback embedder thread, storage thread (upsert batching + NL generation), and thread join + stats collection. Uses 12 `Arc` clones, 5 `thread::spawn` calls, and 3 crossbeam channels. The parser thread closure alone is 120+ lines. Individual pipeline stages cannot be tested in isolation because they're closures capturing shared state. Adding a new pipeline stage (e.g., call-graph extraction) requires modifying this single massive function.
- **Suggested fix:** Extract each stage into a named function: `parser_stage()`, `gpu_embed_stage()`, `cpu_embed_stage()`, `store_stage()`. The main function becomes channel setup + thread spawns + joins. This is a hard refactor due to shared `Arc<AtomicUsize>` state and channel types.

#### CQ-3: HNSW file extension list duplicated between `persist.rs` and `watch.rs` (with a mismatch)
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:15, src/cli/watch.rs:217-221
- **Description:** `persist.rs` defines `HNSW_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"]` (3 files). `watch.rs` has its own inline list `["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"]` (4 files). The mismatch is intentional — `persist.rs` handles checksum separately — but it means a new HNSW file type requires updating two locations independently. The `persist.rs` save/load functions also hardcode `"hnsw.checksum"` in multiple places (lines 29, 170, 195, 209, 221) rather than using a constant.
- **Suggested fix:** Add `pub const HNSW_ALL_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"]` to `hnsw/persist.rs`. Export it from the `hnsw` module. Use it in `watch.rs` cleanup code.

#### CQ-4: `cmd_watch` reaches 9 indent levels — deeply nested reindex/HNSW/notes handling
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:39-290
- **Description:** The main loop body reaches 9 indent levels: `loop → match → Ok(Ok) → for → if → match → Ok → match → Ok(Some)`. The function mixes three orthogonal concerns (event collection, file reindexing, note reindexing) in one match arm. The embedder lazy-init pattern is duplicated verbatim at lines 168-177 and 243-252 within the same function.
- **Suggested fix:** Extract `try_init_embedder(cell: &OnceCell<Embedder>) -> Option<&Embedder>` to deduplicate the 7-line init pattern. Extract `process_file_changes()` and `process_note_changes()` as separate functions. This brings nesting to 5 levels.

#### CQ-5: Reference lookup boilerplate duplicated 6 times with identical error messages
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:489-500, query.rs:572-583, diff.rs:24-35, diff.rs:55-66, drift.rs:26-37, gather.rs:54-63
- **Description:** Six sites perform identical 10-line reference resolution: `Config::load(&root)` → `load_references(&config.references)` → `.find(|r| r.name == ref_name).ok_or_else(|| anyhow!("Reference '{}' not found. Run 'cqs ref list' to see available references."))`. The error message string is character-for-character identical across all 6. A `resolve.rs` module already exists in `cli/commands/` for `resolve_target` — reference resolution is the same pattern.
- **Suggested fix:** Add `pub(crate) fn find_reference(root: &Path, name: &str) -> Result<ReferenceIndex>` to `cli/commands/resolve.rs`. All 6 sites become one-liners.

#### CQ-6: `find_dead_code` is 233 lines with an inline struct definition and 3 distinct phases
- **Difficulty:** medium
- **Location:** src/store/calls.rs:707-939
- **Description:** `find_dead_code` defines `struct LightChunk` (8 fields) at line 728 inside the function body. The function performs 3 phases: (1) SQL fetch of all uncalled functions (707-763), (2) lightweight name/test/path filtering without content (806-835), (3) batch content fetch + confidence scoring (837-928). Each phase has distinct concerns. The inline struct definition makes it invisible to tests and prevents extracting phases into separate methods that accept/return `LightChunk` values.
- **Suggested fix:** Move `LightChunk` to a `pub(crate)` type in `store/calls.rs` (below the impl block). Extract phases into helper methods: `fetch_uncalled_functions()` → `Vec<LightChunk>`, `filter_candidates(uncalled, tests, ...) -> Vec<LightChunk>`, `score_confidence(candidates, ...) -> (Vec<DeadFunction>, Vec<DeadFunction>)`.

#### CQ-7: `GatheredChunk` constructed field-by-field from `SearchResult` at 4 non-test call sites
- **Difficulty:** easy
- **Location:** src/gather.rs:244, gather.rs:435, onboard.rs:483, onboard.rs:496
- **Description:** `GatheredChunk` has 11 fields. Converting from `SearchResult`/`ChunkSummary` requires copying 9 fields identically (name, file, line_start, line_end, language, chunk_type, signature, content, score) with only `depth` and `source` varying per call site. This 10-line block is repeated at 4 production sites. There is no `From` impl or factory constructor.
- **Suggested fix:** Add `impl GatheredChunk { pub fn from_search(sr: &SearchResult, depth: usize, source: Option<String>) -> Self }` in `gather.rs`. Update the 4 call sites to one-liners.

#### CQ-8: `search_filtered` is 219 lines mixing SQL assembly, scoring, RRF fusion, and content fetch
- **Difficulty:** hard
- **Location:** src/search.rs:414-632
- **Description:** `search_filtered` handles 5 distinct concerns in one method: (1) SQL WHERE clause assembly from filter fields (lines 434-476) with manually computed bind parameter indices, (2) cursor-based embedding batch loading (489-569), (3) per-chunk scoring with name matching, glob filtering, note boosting, and demotion (525-568), (4) RRF fusion of semantic + FTS results (574-602), (5) full content fetch + parent dedup (608-630). All of this runs inside `self.rt.block_on(async { ... })`, making the scoring logic untestable without a live database. The bind parameter indexing (`bind_values.len() + i + 1`) is fragile — adding a new filter condition requires recalculating all subsequent indices.
- **Suggested fix:** Extract `build_filter_sql(filter) -> (conditions, bind_values)` for testable SQL assembly. Extract `score_candidate(embedding, query, filter, matcher, notes) -> f32` for testable scoring. These two extractions alone would remove ~100 lines from the async block.

## Extensibility

#### EX-1: `ChunkType` Display/FromStr/error still manually maintained — `define_chunk_types!` macro never created
- **Difficulty:** medium
- **Location:** src/language/mod.rs:268-335
- **Description:** The v0.19.0 audit triage claims EX-1/EX-8 was fixed by a `define_chunk_types!` macro, but no such macro exists. `ChunkType` still has 3 manual match blocks: `Display` (lines 268-289), `FromStr` (lines 310-335), and `ParseChunkTypeError::fmt` (lines 298-306 with a hardcoded valid-options string). Additionally, `nl.rs:338-355` has a 4th match block (`type_word`) that maps `ChunkType` variants to human-readable strings — nearly identical to `Display` except `TypeAlias` maps to `"type alias"` instead of `"typealias"`. Adding a new `ChunkType` variant requires updating 4 match arms plus the hardcoded error string. Compare with `Language`, which uses `define_languages!` to generate all these from a single table.
- **Suggested fix:** Create a `define_chunk_types!` macro analogous to `define_languages!`, or at minimum use `strum` derives (`Display`, `EnumString`, `EnumIter`) to generate these. The `nl.rs` type_word should call a `ChunkType::human_name()` method generated alongside Display.

#### EX-2: Structural `Pattern` has no language-specific dispatch hooks — new languages fall through to generic heuristics
- **Difficulty:** medium
- **Location:** src/structural.rs:64-178
- **Description:** Each structural pattern function (`matches_builder`, `matches_error_swallow`, `matches_async`, `matches_mutex`, `matches_unsafe`, `matches_recursion`) contains a `match language` with explicit arms for 4-5 languages (Rust, Python, Go, TypeScript/JavaScript) plus a catch-all. The 15 other supported languages (C++, Java, C#, Kotlin, Swift, etc.) all fall through to generic text-matching heuristics. For example: Java's `synchronized` keyword is not detected by `matches_mutex`; Kotlin's `suspend fun` is invisible to `matches_async`; C#'s `async` methods are caught only by the generic `content.contains("async")` fallback. Adding language-specific patterns requires modifying 6 separate functions. There's no `LanguageDef` hook for pattern matching — patterns are hardcoded per-language in `structural.rs` rather than being part of the language definition.
- **Suggested fix:** Add an optional `structural_patterns: Option<&'static [(&'static str, fn(&str, &str) -> bool)]>` field to `LanguageDef`, keyed by pattern name. Each language module can provide its own matchers for relevant patterns (e.g., Java provides `("mutex", |content, _name| content.contains("synchronized"))`). `Pattern::matches` falls back to the current generic heuristics when the language doesn't provide a specific matcher.

#### EX-3: `ENTRY_POINT_NAMES` and `TRAIT_METHOD_NAMES` are hardcoded in `store/calls.rs` with no language-specific extension point
- **Difficulty:** easy
- **Location:** src/store/calls.rs:44-56 (entry points), src/store/calls.rs:108-171 (trait methods)
- **Description:** Dead code detection uses two hardcoded lists: `ENTRY_POINT_NAMES` (12 names, mixed across Rust/Python/JS/Java) and `TRAIT_METHOD_NAMES` (38 names, almost entirely Rust `std` traits plus `new`/`build`/`builder`). These lists have no connection to the language system — they're global constants applied to all languages regardless of relevance. `beforeEach`/`afterEach` (JS test hooks) are checked for Rust files; Rust's `fmt`/`deref`/`poll` are checked for Python files. The `TRAIT_IMPL_RE` regex (`impl\s+\w+\s+for\s+`) is Rust-only but applied universally. For languages added since these lists were written (Kotlin, Swift, Objective-C, etc.), framework entry points like Kotlin's `onCreate`, Swift's `viewDidLoad`, or ObjC's `applicationDidFinishLaunching` are not recognized, causing false-positive dead code reports.
- **Suggested fix:** Add `entry_point_names: &'static [&'static str]` and `trait_method_names: &'static [&'static str]` to `LanguageDef`. Each language module provides its own lists. `find_dead_code` unions the lists from the chunk's language instead of using global constants. The Rust module gets the current `TRAIT_METHOD_NAMES`; JS gets `beforeEach`/`afterEach`; Kotlin gets `onCreate`/`onDestroy`; etc.

#### EX-4: `callable_sql_list()` duplicates `is_callable()` logic — adding a callable ChunkType requires updating both
- **Difficulty:** easy
- **Location:** src/language/mod.rs:244-266
- **Description:** `ChunkType::is_callable()` uses `matches!(self, Function | Method | Property | Macro)` and `callable_sql_list()` hardcodes the same 4 variants via `let callable = [Function, Method, Property, Macro]`. The doc comment on `callable_sql_list` says "keep in sync when adding new callable variants" — a manual-sync comment is a code smell. If a future variant (e.g., `Extension` for Swift extensions) is added to `is_callable()` but not `callable_sql_list()`, dead code detection and test-map SQL queries silently exclude it. There's no compiler-enforced link between the two.
- **Suggested fix:** Derive `callable_sql_list()` from `is_callable()`: iterate `ChunkType::all_variants()` (which doesn't exist yet for ChunkType but does for Language), filter by `is_callable()`, and format. Or define the callable list once as a const array and derive both methods from it.

#### EX-5: `PIPEABLE_COMMANDS` list in `pipeline.rs` must be manually updated when adding new batch commands
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs:15-17
- **Description:** The pipeline system uses a hardcoded `PIPEABLE_COMMANDS` array to determine which batch commands can receive piped function names. Adding a new command that accepts a name argument (like a hypothetical `ancestors` command) requires adding it to both `BatchCmd` enum and this separate list. There's no compile-time check that the list is complete — the only guard is a test (`test_pipeable_commands_parse_with_name_arg`) that verifies listed commands accept a name, but nothing catches commands that *should* be listed but aren't. The list currently has 9 entries out of 26 batch commands.
- **Suggested fix:** Add a `#[pipeable]` attribute or a `fn is_pipeable() -> bool` method on `BatchCmd`. Or derive the list from the command definitions: any command whose first positional argument is named "name", "target", or "query" could be auto-detected as pipeable.

#### EX-6: `NAME_ARRAY_FIELDS` in `pipeline.rs` must be manually updated for each new batch command's JSON output shape
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs:84-98
- **Description:** Pipeline name extraction uses a hardcoded list of 12 JSON field names (`results`, `chunks`, `callers`, `calls`, `tests`, `dead`, `possibly_dead_pub`, `path`, `shared_callers`, `shared_callees`, `shared_types`, `similar`, `callees`) to find function names in dispatch results. Adding a new command that returns names under a different JSON key (e.g., `ancestors` returning `{"ancestors": [{"name": ...}]}`) requires adding the key here. There's no connection between a handler's output schema and the pipeline's name extraction — a new handler author could easily miss this. The `extract_from_scout_groups` function is a separate hardcoded extractor for scout's unique nesting pattern.
- **Suggested fix:** Standardize on a common output convention: all batch handlers that return function lists use `"results"` as the key. Alternatively, make extraction key-agnostic: recursively walk the JSON tree looking for objects with a `"name"` field, rather than probing known field names.

#### EX-7: `NlTemplate` match arms in `generate_nl_with_template` require updating for new chunk types
- **Difficulty:** easy
- **Location:** src/nl.rs:338-355
- **Description:** The NL description generator has a `match chunk.chunk_type` block that maps each `ChunkType` to a human-readable string (e.g., `Function => "function"`, `TypeAlias => "type alias"`). This is nearly identical to `ChunkType::Display` (which maps `TypeAlias => "typealias"`) but with different formatting for `TypeAlias`. Adding a new `ChunkType` variant requires updating this match, the Display impl, the FromStr impl, and the error message string — 4 locations. The NL module has no way to discover new variants automatically.
- **Suggested fix:** Add `ChunkType::human_name(&self) -> &'static str` that provides space-separated names for NL context (e.g., `"type alias"` instead of `"typealias"`). The nl.rs match becomes `let type_word = chunk.chunk_type.human_name();`. This centralizes the NL-specific naming next to the Display impl.

#### EX-8: Test heuristics in `find_test_chunks` and `find_dead_code` use hardcoded patterns not connected to the language system
- **Difficulty:** medium
- **Location:** src/store/calls.rs:83-100 (constants), src/store/calls.rs:815-821 (path patterns)
- **Description:** Test detection uses three hardcoded constant lists: `TEST_NAME_PATTERNS` (`test_%`, `Test%`), `TEST_CONTENT_MARKERS` (`#[test]`, `@Test`), and `TEST_PATH_PATTERNS` (`%/tests/%`, `%_test.%`, etc.). These cover Rust and Java/Go but miss many language conventions: Python's `pytest` markers, C#'s `[TestMethod]`/`[Fact]`, Kotlin's `@Test` (from JUnit, already covered) but not `@ParameterizedTest`, Swift's `XCTest` subclassing, JavaScript's `describe`/`it` patterns. The path patterns are also Rust/Go-centric — Python's `test_*.py` naming is covered but Kotlin's `src/test/kotlin/` is not. Dead code detection's inline path checks (lines 815-821: `contains("/tests/")`, `contains("_test.")`, etc.) duplicate and diverge from `TEST_PATH_PATTERNS`.
- **Suggested fix:** Add `test_markers: &'static [&'static str]` and `test_path_patterns: &'static [&'static str]` to `LanguageDef`. Each language provides its own test detection heuristics. `find_test_chunks` unions markers from all enabled languages. The inline path checks in `find_dead_code` should use the same `TEST_PATH_PATTERNS` constant to avoid divergence.

## Robustness

#### RB-1: `serde_json::to_string().unwrap()` in batch REPL — 6 sites can panic on NaN/Inf scores
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:288, 313, 328, 336, 343, 353
- **Description:** The batch REPL serializes JSON output with `serde_json::to_string(&value).unwrap()` at 6 sites. While most values are safe `serde_json::json!()` constructs, the `dispatch()` result at line 336 can contain `f32::NAN` or `f32::INFINITY` scores from cosine similarity (e.g., zero-norm embeddings produce NaN). `serde_json::to_string` rejects NaN/Infinity by default, causing a panic that kills the entire batch session. Agents using `cqs batch` would lose all accumulated context.
- **Suggested fix:** Replace `.unwrap()` with `.unwrap_or_else(|e| format!(r#"{{"error":"serialization failed: {e}"}}"#))` at all 6 sites. The error becomes a JSON error object instead of a process crash.

#### RB-2: `BatchContext` OnceLock accessors use `.unwrap()` after `set()` — 7 fragile sites
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:75, 86, 136, 183, 194, 211, src/store/mod.rs:711
- **Description:** Seven `BatchContext` accessor methods plus one Store method follow the set-then-get-unwrap pattern: check `OnceLock::get()`, if empty initialize and `set()`, then `get().unwrap()`. The `unwrap()` is safe because `set()` was just called, but this relies on an implicit single-threaded invariant not enforced by the type system. The `borrow_ref()` method at line 169 uses `Ref::map(map, |m| m.get(name).unwrap())` inside a `contains_key` guard — safe with `RefCell` borrow rules but non-obvious.
- **Suggested fix:** Use `get_or_init`/`get_or_try_init` for `OnceLock`-backed fields, which is idiomatic and eliminates the unwrap entirely. For `borrow_ref`, use `.expect("checked contains_key above")` to document the invariant.

#### RB-3: `search_filtered` uses `.unwrap()` on `normalized_query` with non-local safety invariant
- **Difficulty:** easy
- **Location:** src/search.rs:583, src/search.rs:523
- **Description:** Line 583 does `normalized_query.as_ref().unwrap()` inside an `if use_rrf` block. The variable is `Some` when `use_rrf` is true (set at line 574), but the safety depends on code 9 lines earlier — a refactoring that changes the `if`/`else` structure or moves the normalization could introduce a panic. The `batch.last().unwrap()` at line 523 has the same pattern: safe because of the `is_empty()` check 3 lines above, but fragile to reordering.
- **Suggested fix:** For line 583, restructure to avoid the unwrap: `let fts_ids = if let Some(nq) = normalized_query.as_ref() { ... } else { vec![] };`. For line 523, use `.expect("batch non-empty checked above")`.

#### RB-4: Embedder and Reranker session guard `.unwrap()` after `session()` — relies on undocumented postcondition
- **Difficulty:** easy
- **Location:** src/embedder.rs:523, src/reranker.rs:128
- **Description:** Both sites do `guard.as_mut().unwrap()` after `self.session()` returns `Ok`. The `session()` method guarantees the `Option<Session>` is `Some` after initialization, but the unwrap relies on an implementation detail not enforced by the type system. The comment says "Safe: session() guarantees Some after init" — but if `session()` is refactored to return the guard without always initializing, both sites panic.
- **Suggested fix:** Have `session()` return `MutexGuard<Session>` instead of `MutexGuard<Option<Session>>` by restructuring the lazy init to unwrap internally. Alternatively, use `.expect("session() guarantees initialized after Ok return")` to make the invariant machine-visible.

#### RB-5: `ChunkOutput` serialization uses `.expect()` — can panic on NaN scores
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers.rs:34, 152
- **Description:** Two sites use `serde_json::to_value(ChunkOutput::from_search_result(r, ...)).expect("ChunkOutput serialization cannot fail")`. `ChunkOutput` includes a `score: f32` field from search results. If a score is `f32::NAN` (from corrupt embeddings or zero-norm vectors), `serde_json::to_value` returns `Err` and the `expect` panics. This is the same NaN propagation path as RB-1.
- **Suggested fix:** Replace `.expect(...)` with `.map_err(|e| anyhow::anyhow!("ChunkOutput serialization failed: {e}"))?` to propagate the error instead of panicking.

#### RB-6: `Parser::new()` panics with `expect("registry/enum mismatch")` if language registry is inconsistent
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:62
- **Description:** `Parser::new()` iterates over `REGISTRY.all()` and calls `def.name.parse::<Language>().expect("registry/enum mismatch")`. If a new language is added to the registry macro but the `Language` enum's `FromStr` impl doesn't include it, this panics on the first `Parser::new()` call — which happens during every `cqs` invocation that needs parsing. The panic message doesn't indicate which language name failed.
- **Suggested fix:** Return an error instead: `let lang: Language = def.name.parse().map_err(|_| ParserError::UnsupportedLanguage(def.name.to_string()))?;`. Or at minimum use `.expect(&format!("Language enum missing variant for registry entry '{}'", def.name))` to identify the offending language.

#### RB-7: `diff_parse.rs` `.unwrap()` after `starts_with` guard on external input (git diff output)
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:50
- **Description:** Line 50 does `line.strip_prefix("+++ ").unwrap()` inside a block guarded by `line.starts_with("+++ ")`. The unwrap is logically safe but the diff parser processes external input (git diff output) that could be malformed. Using the `if let` pattern would be more defensive and idiomatic.
- **Suggested fix:** Replace the `starts_with` + `strip_prefix` + `unwrap` with `if let Some(path) = line.strip_prefix("+++ ")`.

#### RB-8: `onboard.rs` score-to-u64 cast produces incorrect ordering for NaN/negative scores
- **Difficulty:** easy
- **Location:** src/onboard.rs:182
- **Description:** `u64::MAX - ((*score * 1e6) as u64)` converts a f32 score to u64 for sort ordering. If `score` is NaN (from corrupt embeddings), `NaN * 1e6` is NaN, and `NaN as u64` saturates to 0 in Rust, producing `u64::MAX` — making NaN scores sort last (highest key = lowest priority for `cap_scores`), which silently includes garbage entries. If `score` is negative, the same saturation to 0 happens. The function doesn't validate scores before the conversion.
- **Suggested fix:** Filter out non-finite/negative scores before `cap_scores`: `caller_scores.retain(|_, (s, _)| s.is_finite() && *s >= 0.0);`. Or use `f32::total_cmp` for ordering instead of the u64 conversion.

#### RB-9: `convert/mod.rs` `expect("FORMAT_TABLE must cover all DocFormat variants")` — runtime panic for compile-time invariant
- **Difficulty:** easy
- **Location:** src/convert/mod.rs:191
- **Description:** The `FORMAT_TABLE.iter().find(|e| e.variant == format).expect(...)` panics if a `DocFormat` variant exists without a table entry. Adding a new `DocFormat` variant without updating `FORMAT_TABLE` causes a panic on first conversion, with a message that doesn't say which variant is missing.
- **Suggested fix:** Add a `#[test]` that iterates all `DocFormat` variants and asserts each has a `FORMAT_TABLE` entry (compile-time enforcement). Improve the runtime message: `.unwrap_or_else(|| panic!("FORMAT_TABLE missing entry for {:?}", format))`.

#### RB-10: `onboard.rs` line 128 `.unwrap()` after early return — non-local safety invariant
- **Difficulty:** easy
- **Location:** src/onboard.rs:128
- **Description:** `.or(results.first()).unwrap()` after `results.is_empty()` returns early at line 117. The `unwrap()` is logically safe (if `find()` returns `None`, `results.first()` is `Some` because the list is non-empty), but safety depends on an early return 11 lines above. It's the only `unwrap()` in this module's production code.
- **Suggested fix:** Use `.expect("results guaranteed non-empty by early return above")` or restructure with `ok_or_else` to propagate as an error.

## Platform Behavior

#### PB-1: `path_matches_mention` does not normalize backslashes before matching
- **Difficulty:** easy
- **Location:** src/note.rs:311-321
- **Description:** `path_matches_mention()` does suffix/prefix matching using `/` as the component separator (lines 315, 318), but never normalizes backslashes in either the `path` or `mention` arguments. Compare with `note_mention_matches_file()` in scout.rs:414-416 which explicitly does `mention.replace('\\', "/")` and `file.replace('\\', "/")` before matching. If a note mention contains a backslash path like `src\store\chunks.rs` (e.g., copy-pasted from Windows), `path_matches_mention` will fail to match it against the normalized `src/store/chunks.rs` origin stored in the DB. The same function is used by `note_boost()` in search.rs to boost results based on note mentions — so notes with backslash paths silently lose their boosting effect.
- **Suggested fix:** Add `let path = path.replace('\\', "/"); let mention = mention.replace('\\', "/");` at the top of the function, matching the pattern in `note_mention_matches_file`.

#### PB-2: `find_dead_code` Phase 1 inline path filter checks forward slashes only — inconsistent with `is_test_chunk`
- **Difficulty:** easy
- **Location:** src/store/calls.rs:814-819
- **Description:** The lightweight Phase 1 filter in `find_dead_code` does `path_str.contains("/tests/")` (line 815) but does not check `"\\tests\\"`. The centralized `is_test_chunk()` in lib.rs:209-212 explicitly checks BOTH separators: `file.contains("/tests/") || file.contains("\\tests\\")` and `file.starts_with("tests/") || file.starts_with("tests\\")`. The Phase 1 filter also misses the `starts_with("tests/")` check entirely. While origins in the DB are normalized to forward slashes (via `normalize_origin`), the `LightChunk.file` field is constructed via `PathBuf::from(row.get::<String, _>(1))` (line 743) — `PathBuf::to_string_lossy()` on Windows would re-introduce backslashes. On WSL/Linux this is harmless since `PathBuf` preserves forward slashes, but the code is not portable and diverges from the centralized test detection.
- **Suggested fix:** Replace the 4-line inline check at lines 815-819 with `if crate::is_test_chunk(&chunk.name, &path_str)`. This uses the centralized function (which handles both separators and includes `starts_with("tests/")`) and eliminates the code duplication. The centralized function also checks name patterns which are already handled by `test_names.contains` above, so the only net change is adding backslash support and the `starts_with` prefix check.

#### PB-3: 30+ sites manually call `.replace('\\', "/")` for path normalization — no centralized function
- **Difficulty:** medium
- **Location:** 15+ files (store/chunks.rs, store/mod.rs, store/types.rs, store/calls.rs, store/helpers.rs, cli/display.rs, cli/pipeline.rs, cli/batch/types.rs, cli/commands/stale.rs, cli/commands/explain.rs, cli/commands/gather.rs, cli/commands/graph.rs, cli/commands/project.rs, cli/staleness.rs, impact/diff.rs, impact/analysis.rs, onboard.rs, scout.rs, source/filesystem.rs)
- **Description:** Backslash-to-forward-slash normalization is performed ad-hoc at 30+ call sites across the codebase. Only `store/chunks.rs:22` has a named `normalize_origin()` function, but it takes `&Path` and is private to the store module. Every other site uses inline `.to_string_lossy().replace('\\', "/")`. A missed normalization in any new code path creates a silent bug where paths don't match DB origins. The pattern is well-established but undiscoverable — a new contributor wouldn't know to add the `.replace('\\', "/")` call.
- **Suggested fix:** Add a public `pub fn normalize_path(path: &Path) -> String` to `lib.rs`. Replace the 30+ inline `.to_string_lossy().replace('\\', "/")` calls with it. The `normalize_origin` in `store/chunks.rs` becomes a thin wrapper. This makes the normalization discoverable and greppable.

#### PB-4: HNSW advisory file locking silently ineffective on WSL `/mnt/c/` — no runtime warning
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:119-126 (save), src/hnsw/persist.rs:272-284 (load)
- **Description:** The HNSW save/load functions use `file.lock()` and `file.lock_shared()` to prevent concurrent corruption. Comments correctly note "File locking is advisory only on WSL over 9P." However, there is no runtime detection or warning when locking is advisory-only. On WSL with files on `/mnt/c/` (NTFS over 9P), lock calls succeed but provide no mutual exclusion — concurrent `cqs index` and `cqs watch` could both write the HNSW index simultaneously, producing corruption. The `is_wsl()` function exists in `config.rs` for platform detection but is private.
- **Suggested fix:** When `is_wsl()` is true and the index directory starts with `/mnt/`, log `tracing::warn!("HNSW file locking is advisory-only on WSL/NTFS — avoid concurrent index operations")` on first lock acquisition. Move `is_wsl()` to `lib.rs` as a public function (see PB-5).

#### PB-5: `is_wsl()` is private to `config.rs` — platform detection not reusable
- **Difficulty:** easy
- **Location:** src/config.rs:14-24
- **Description:** The `is_wsl()` function (cached WSL detection via `/proc/version`) is `fn is_wsl()` (private) in `config.rs`. It's only used at one site: the config file permission check (line 205). But WSL detection is needed in at least 3 other contexts: (1) HNSW lock advisory-only warning (PB-4), (2) watch mode unreliability warning (PB-6), (3) any future platform-aware code. Each would need to re-implement the detection.
- **Suggested fix:** Move `is_wsl()` to `lib.rs` as `pub fn is_wsl() -> bool`. The `OnceLock` caching makes it efficient for repeated calls. Update `config.rs` to use the public version.

#### PB-6: Watch mode doesn't warn when monitoring `/mnt/c/` paths where inotify is unreliable
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:69-70
- **Description:** `cmd_watch` calls `watcher.watch(&root, RecursiveMode::Recursive)` regardless of whether the root is on a WSL `/mnt/c/` mount where inotify over 9P is unreliable. Events can be delayed, duplicated (handled by mtime dedup at lines 96-97), or missed entirely (e.g., `git checkout` or editor save-to-temp-then-rename). CLAUDE.md documents this: "cqs watch uses inotify, which is unreliable on WSL /mnt/c/". However, the watch command gives no indication to the user. A user running `cqs watch` from `/mnt/c/Projects/foo` would expect reliable monitoring.
- **Suggested fix:** After `watcher.watch()`, check if the root path starts with `/mnt/` on WSL and print: `"Warning: Monitoring /mnt/c/ via WSL inotify — some file changes may be missed. Run 'cqs index' periodically."`. Use `is_wsl()` (from PB-5) combined with `root.starts_with("/mnt/")`.

#### PB-7: `suggest.rs` `find_stale_mentions` doesn't normalize backslashes in mentions before disk check
- **Difficulty:** easy
- **Location:** src/suggest.rs:192 (find_stale_mentions function)
- **Description:** `classify_mention()` at line 172 correctly identifies file-like mentions by checking for `.`, `/`, or `\\`. But `find_stale_mentions()` resolves file mentions against disk using `project_root.join(mention)` followed by `path.exists()`. If a mention contains backslashes (e.g., `src\store\chunks.rs`), `PathBuf::join()` on Linux treats the backslash literally — creating the path `{root}/src\store\chunks.rs` (a single directory entry with literal backslashes) rather than `{root}/src/store/chunks.rs`. This causes false-positive staleness reports ("mention file not found") even though the file exists with forward slashes.
- **Suggested fix:** Normalize backslashes before the file existence check: `let normalized = mention.replace('\\', "/"); let path = project_root.join(&normalized);`

#### PB-8: `ChunkSummary.file` typed as `PathBuf` but semantically holds normalized forward-slash strings
- **Difficulty:** easy (documentation) / medium (refactor)
- **Location:** src/store/helpers.rs:99, src/store/helpers.rs:140
- **Description:** `ChunkSummary.file` is typed `PathBuf` (line 99) but constructed from the `origin` column via `PathBuf::from(row.origin)` (line 140), where origins are always forward-slash normalized. This means `file` never contains OS-native separators on Windows — it's a forward-slash relative path wrapped in `PathBuf`. Code that calls `.to_string_lossy().replace('\\', "/")` on this field is performing a no-op on WSL/Linux. Code that does `root.join(&chunk.file)` works on both platforms only because forward slashes are accepted by all major OSes. The type implies OS-native semantics that the data doesn't follow.
- **Suggested fix:** This is the same issue as the prior audit's AD-1 ("Inconsistent String vs PathBuf for file paths"), deferred as a pure type refactor. At minimum, add a doc comment on the `file` field: `/// Source file path (always forward-slash normalized, not OS-native)`. This prevents future contributors from assuming `PathBuf` semantics.

## Algorithm Correctness

#### AC-1: `search_by_candidate_ids` silently skips RRF fusion when HNSW index is available
- **Difficulty:** medium
- **Location:** src/search.rs:664-793
- **Description:** `search_filtered` (line 459) checks `filter.enable_rrf` and performs RRF fusion when true — combining semantic similarity scores with FTS BM25 keyword rankings via `rrf_fuse()`. However, `search_by_candidate_ids` (line 664), used when an HNSW index is available, completely ignores `filter.enable_rrf`. It scores candidates purely by cosine similarity (with name boost and note boost), but never runs the FTS query or calls `rrf_fuse()`. This means search behavior silently changes depending on whether an HNSW index exists: with HNSW, keyword matches are invisible; without HNSW, they boost results via RRF. The code path is: `search_filtered_with_index` -> `search_by_candidate_ids` (when index present) -> no RRF. This affects all callers of `search_filtered_with_index`: `query`, `explain`, `similar`, batch handlers, `reference.rs`, and `project.rs`. Practical impact: a search for "parse configuration" ranks a function named `parse_config` differently depending on index availability, because FTS keyword matches on "parse" and "configuration" are lost in the HNSW path.
- **Suggested fix:** Add RRF fusion to `search_by_candidate_ids` when `filter.enable_rrf && !filter.query_text.is_empty()`. After cosine scoring candidates, run the same FTS query as `search_filtered` (lines 582-596), then call `rrf_fuse()` to combine ranked lists. The FTS query is cheap (BM25 on FTS5 table) — the expensive embedding comparison is already done.

#### AC-2: `bfs_expand` overwrites shallower depth with deeper depth when updating score
- **Difficulty:** easy
- **Location:** src/gather.rs:202-205
- **Description:** When `bfs_expand` encounters an already-visited neighbor with a higher score from the current node, it updates both score and depth: `*existing = (new_score, depth + 1)`. The depth `depth + 1` may be deeper than the node's existing depth. Example with `expand_depth > 1`: node Y at depth 1 from seed A (score 0.72) has neighbor Z. Z was previously found at depth 1 from seed B with score 0.45. Now Y discovers Z at depth 2 with score 0.576 > 0.45, so Z's depth changes from 1 to 2 despite being reachable at depth 1. The depth field is used by `cap_scores` in `onboard` (line 179) to sort by depth ascending, prioritizing shallower nodes. An incorrect deeper depth causes the node to appear farther from the entry point in the reading list and potentially be evicted by `cap_scores` when it should be kept.
- **Suggested fix:** When updating score, preserve the minimum depth: `if new_score > existing.0 { existing.0 = new_score; existing.1 = existing.1.min(depth + 1); }`.

#### AC-3: `onboard` search uses embedding-only search instead of hybrid RRF
- **Difficulty:** easy
- **Location:** src/onboard.rs:114-115
- **Description:** The `onboard` function creates `SearchFilter::default()` which has `enable_rrf: false` and `query_text: ""`. The initial search uses pure embedding similarity with no keyword matching or RRF fusion. Compare with `gather()` (src/gather.rs:300-303) which correctly sets `enable_rrf: true` and `query_text: query_text.to_string()`. When a user runs `cqs onboard "error handling"`, the search misses functions containing the exact words "error" or "handling" in their names if the embeddings aren't close enough. The `gather`, `scout`, and `task` commands all use RRF — `onboard` is the sole outlier among analysis commands that take free-text input.
- **Suggested fix:** Change the filter to: `let filter = SearchFilter { query_text: concept.to_string(), enable_rrf: true, ..SearchFilter::default() };`.

#### AC-4: `search_filtered` parent dedup reduces results below requested limit after RRF
- **Difficulty:** easy
- **Location:** src/search.rs:598, 612-628
- **Description:** After RRF fusion returns exactly `limit` entries (line 598), parent dedup (lines 612-628) filters out windowed child chunks sharing a `parent_id`. This can reduce the result count below `limit`. With `limit=10`, if RRF returns 10 results and 3 are windowed children of parents already in the set, dedup removes them, leaving 7 results. The brute-force path pre-RRF over-fetches with `semantic_limit = limit * 3`, but RRF caps output to `limit` before dedup. The `search_by_candidate_ids` path has the same issue — parent dedup after `.take(limit)`. Users passing `--limit 10` may see fewer than 10 results even when sufficient distinct chunks exist.
- **Suggested fix:** Request `limit * 2` from `rrf_fuse` (or `limit + expected_dedup_overhead`) instead of `limit`, then apply parent dedup and truncate to `limit`. The extra candidates are cheap — content fetch happens after dedup in phase 2.

#### AC-5: `drift.rs` test uses `partial_cmp().unwrap_or(Equal)` — the pattern fixed in v0.19.1
- **Difficulty:** easy
- **Location:** src/drift.rs:167-171
- **Description:** The `test_drift_sort_order` test sorts using `b.drift.partial_cmp(&a.drift).unwrap_or(std::cmp::Ordering::Equal)`. The v0.19.1 audit P1 (AC-1/AC-2) fixed 11 production sites to use `f32::total_cmp()` for NaN-safe sorting, but this test was missed. The production code at line 88 correctly uses `b.drift.total_cmp(&a.drift)`. While NaN drift values are unlikely in tests, the test demonstrates the exact anti-pattern flagged as P1 and could propagate via copy-paste.
- **Suggested fix:** Change lines 167-171 to: `entries.sort_by(|a, b| b.drift.total_cmp(&a.drift));`

#### AC-6: `token_pack` always includes first item regardless of budget, can overshoot by 50x
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:132
- **Description:** The condition `if used + tokens > budget && keep.iter().any(|&k| k)` ensures at least one item is always packed regardless of token cost. A single chunk with 50,000 tokens will be included even with `--tokens 1000`. In the `task` command's waterfall budgeting (scout 15%, code 50%, impact 15%, placement 10%, notes 10%), one oversized chunk in the first section can consume the entire token budget, leaving nothing for later sections. The `token_count` and `token_budget` fields in JSON output expose the overshoot, but downstream consumers (agents) may not check these fields.
- **Suggested fix:** Add a hard cap: if the first item exceeds `budget * 2`, truncate its content to fit `budget` and set a `"truncated": true` flag in output. Alternatively, document this in `--tokens` help text.

## Test Coverage

#### TC-1: `search_across_projects` has zero tests — critical cross-project search path untested
- **Difficulty:** medium
- **Location:** src/project.rs:155-231
- **Description:** `search_across_projects()` is a 76-line public function that loads the project registry, iterates registered projects, opens stores with `open_readonly`, loads HNSW indexes, runs filtered search with RRF, merges results, and sorts by score. It has zero unit tests and zero integration tests. The function interacts with the global registry at `~/.config/cqs/projects.toml`, opens multiple stores, and performs HNSW-guided search — all untested paths. The `project.rs` module has 6 inline tests but they only cover `ProjectRegistry` CRUD operations and `make_project_relative()`. The actual search function is completely uncovered. This matters because `search_across_projects` uses `search_filtered_with_index` which silently drops RRF fusion when HNSW is available (the AC-1 finding) — a bug that a test would catch.
- **Suggested fix:** Add integration tests using `TestStore` with multiple temp directories simulating registered projects. Test: (1) empty registry errors, (2) single project returns results, (3) multi-project merges and sorts by score, (4) missing project index is skipped with warning.

#### TC-2: Schema migration `migrate_v10_to_v11` never executed in any test
- **Difficulty:** medium
- **Location:** src/store/migrations.rs:29-57 (migrate), 84-107 (v10 to v11)
- **Description:** The only migration tests are `test_migration_not_supported_error` (checks error message formatting for unknown version pairs) and `test_current_schema_version_documented` (asserts version == 11). The actual `migrate()` function and `migrate_v10_to_v11` are never executed in any test. This means: (1) the transactional migration path (begin/commit) is untested, (2) the "from > to" rejection path (`SchemaNewerThanCq`) is untested at runtime, (3) the v10-to-v11 migration (CREATE TABLE type_edges with indexes) is only verified by its existence, not by execution. If `type_edges` already exists in the v10 schema (it uses `IF NOT EXISTS` so it's safe), the migration is a no-op — but the test should verify the migration runs without error and leaves the schema in a valid state.
- **Suggested fix:** Add a test that: (1) creates a store, (2) manually downgrades schema_version to 10 and drops type_edges, (3) calls `Store::open()` which triggers migration, (4) verifies type_edges table exists and schema_version is 11. Also test the `from > to` rejection path.

#### TC-3: `check_origins_stale` SQLite 999-parameter batch boundary untested
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:638-698
- **Description:** The P1 fix for DS-8 (SQLite 999-param limit) added batching in groups of 900 to `check_origins_stale`. Four inline tests exist (empty, all-fresh, mixed, unknown-origin) but all use 0-2 origins. No test verifies the batch boundary — passing 901+ origins to confirm the batching loop actually processes multiple batches correctly. The batching code is straightforward but the SQL construction (`format!("SELECT ... WHERE origin IN ({})"))` builds fresh bind placeholders for each batch, and an off-by-one in the placeholder range (e.g., starting at `?0` instead of `?1`) would only manifest with batch sizes > 900.
- **Suggested fix:** Add a test that inserts 950+ chunks with distinct origins and calls `check_origins_stale` with all 950+ origin strings. Verify the result is correct across the batch boundary. This doesn't need real files — use synthetic mtimes where half are stale.

#### TC-4: `resolve_index_dir` migration logic (.cq -> .cqs rename) has zero tests
- **Difficulty:** easy
- **Location:** src/lib.rs:168-183
- **Description:** `resolve_index_dir()` has migration logic: if `.cq/` exists and `.cqs/` doesn't, it renames `.cq` to `.cqs`. This logic has zero tests. The function also has 3 return paths: (1) `.cqs/` exists after migration, (2) legacy `.cq/` exists but rename failed, (3) neither exists (returns `.cqs/` as default). None of these paths are tested. A regression in the rename logic (e.g., accidentally reversing the condition) would silently break index discovery for users migrating from older versions.
- **Suggested fix:** Add tests using `tempfile::TempDir`: (1) only `.cq/` exists -> verify rename to `.cqs/` and correct return, (2) both exist -> verify `.cqs/` returned without rename, (3) neither exists -> verify `.cqs/` returned, (4) `.cq/` exists but rename fails (e.g., `.cqs/` is a file) -> verify `.cq/` returned.

#### TC-5: `rel_display` path utility has zero tests
- **Difficulty:** easy
- **Location:** src/lib.rs:223-228
- **Description:** `rel_display()` strips a root prefix from a path and normalizes backslashes to forward slashes. It's used in 20+ locations across the codebase (CLI display, review output, CI reports, impact display). Despite being a pure function with clear contract, it has zero tests. Edge cases that could silently break output formatting: (1) path not under root (should return full path), (2) root is `/` (should strip only the leading `/`), (3) path with mixed separators on Windows.
- **Suggested fix:** Add 4 inline tests: basic prefix strip, path not under root, backslash normalization, empty root.

#### TC-6: `suggest_placement` / `suggest_placement_with_options` — only trivial empty-result test
- **Difficulty:** medium
- **Location:** src/where_to_add.rs:101-250
- **Description:** The placement suggestion API (`suggest_placement`, `suggest_placement_with_options`, `suggest_placement_with_embedding`) has one inline test: `test_placement_empty_result` which verifies that an empty store returns an empty result. The actual logic — search for similar code, extract file-level patterns (naming convention, visibility, imports, doc style), rank suggestions by relevance — is tested only via CLI integration tests (`test_where_json_output`, `test_where_text_output`) which just verify the command runs without error and produces JSON. No test verifies: (1) pattern extraction correctness (e.g., that a Rust file with snake_case naming is detected correctly), (2) that suggestions rank the correct file highest, (3) that the `max_context_files` option limits results.
- **Suggested fix:** Note: `extract_patterns` and `detect_naming_convention` DO have inline tests (15 tests). The gap is in the integration between search -> pattern extraction -> ranking. Add a test that seeds a store with files having known patterns and verifies the returned `PlacementResult` ranks the correct file first.

#### TC-7: `search_by_candidate_ids` (HNSW-guided path) has no test verifying RRF behavior
- **Difficulty:** medium
- **Location:** src/search.rs:664-793
- **Description:** `search_by_candidate_ids` (used when an HNSW index is available) is tested via `test_search_filtered_with_index_uses_index` which verifies it returns results, but no test checks RRF/keyword behavior. The brute-force path (`search_filtered`) has dedicated RRF tests (`test_search_filtered_rrf_hybrid`), but the HNSW-guided path skips RRF entirely (this is the AC-1 finding). There's no test that verifies search results are equivalent between HNSW and brute-force paths when both should use RRF. A test that seeds a store with a function named "parse_config", runs both code paths with `enable_rrf: true`, and compares results would catch the discrepancy.
- **Suggested fix:** Add a test that runs the same query with `enable_rrf: true` through both `search_filtered` (brute-force) and `search_filtered_with_index` (HNSW-guided) and verifies both include keyword-matching results.

#### TC-8: `review_diff` note matching never tested with actual notes
- **Difficulty:** easy
- **Location:** tests/review_test.rs
- **Description:** The 4 review tests seed chunks and call graph data but never insert notes. The `match_notes` function (review.rs:183-212) is always exercised against an empty notes list, which means the note-matching logic — path_matches_mention matching, sentiment filtering, NoteEntry construction — is never tested. The `relevant_notes` field in every test's `ReviewResult` is always empty. A test that inserts a note mentioning "src/math.rs" (one of the test fixtures) and verifies it appears in `relevant_notes` would cover this path.
- **Suggested fix:** Add a test that: (1) inserts chunks + calls as existing tests do, (2) adds a note with `mentions = ["math.rs"]` via `store.upsert_note()`, (3) runs `review_diff`, (4) asserts `relevant_notes` is non-empty and contains the note text.

#### TC-9: `enumerate_files` has zero tests
- **Difficulty:** easy
- **Location:** src/lib.rs:312+
- **Description:** `enumerate_files()` walks a directory tree, applies gitignore rules, filters by language extension, and returns a `HashSet<PathBuf>`. It's used by `list_stale_files`, `cmd_stale`, and the index pipeline. Despite handling multiple concerns (directory walking, gitignore filtering, extension filtering, error handling for permission-denied), it has zero tests. Edge cases that matter: (1) nested `.gitignore` files, (2) symlinks, (3) directories with no supported files, (4) the `.cqs/` directory itself should be excluded.
- **Suggested fix:** Add integration tests using `tempfile::TempDir` with a known file structure. Test: (1) finds .rs files, (2) skips files matching .gitignore, (3) skips .cqs/ directory, (4) returns empty for directory with no supported files.

#### TC-10: `index_notes` has zero tests
- **Difficulty:** medium
- **Location:** src/lib.rs:247-308
- **Description:** `index_notes()` is a 61-line public function that embeds notes using the embedder, appends sentiment as the 769th dimension, stores them in the DB, and updates mtime. It has zero tests — no inline unit tests and no integration tests. This is the only function that handles the sentiment dimension injection (other code reads it but doesn't write it). A bug in the sentiment injection (e.g., wrong dimension index, clamping logic) would corrupt all note embeddings, causing note search to return wrong results. The function requires an embedder, which makes testing harder, but `tests/embedding_test.rs` already demonstrates embedding tests with real/mock embedders.
- **Suggested fix:** Add a test that creates notes, calls `index_notes` with the real embedder (or mock), then queries the stored note embeddings to verify the 769th dimension matches the sentiment value.

## Data Safety

#### DS-1: Watch mode never acquired index lock (DS-1/DS-6 fix never applied)
- **Difficulty:** medium
- **Location:** src/cli/watch.rs (entire file)
- **Description:** The v0.19.0 audit triage claims "Watch locking: `acquire_index_lock()` with `try_lock()` before reindex cycles (watch.rs)" was fixed. However, watch.rs contains zero references to `acquire_index_lock`, `index_lock`, or even the word "lock". The fix was never actually applied. This means `cqs watch` and `cqs index` can run simultaneously, both writing to the same SQLite database and HNSW files without coordination. While SQLite's WAL mode provides some protection against corruption, the HNSW index files (graph, data, ids, checksum) have no such protection — concurrent `build_hnsw_index` from watch and index commands can produce a corrupted HNSW index.
- **Suggested fix:** Before each reindex cycle in `cmd_watch`, call `acquire_index_lock(&cqs_dir)` with `try_lock()`. If the lock is held (another `cqs index` or `cqs gc` is running), skip this reindex cycle and log a message. Release the lock after HNSW rebuild completes. Use the existing `acquire_index_lock` from `cli/files.rs`.

#### DS-2: Watch mode chunks and call graph not atomic (DS-2 fix never applied)
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:394-427
- **Description:** The v0.19.0 audit triage claims "Atomic transactions: `upsert_chunks_and_calls()` for chunk+call graph atomicity (watch.rs)" was fixed. However, watch.rs calls `store.replace_file_chunks()` (line 404), then separately calls `store.upsert_function_calls()` (line 415) and `store.upsert_type_edges_for_file()` (line 419) in a separate loop. If the process crashes between `replace_file_chunks` and the relationship extraction, the index will have chunks but stale/missing call graph and type edges for those files. The pipeline (`pipeline.rs`) correctly uses `upsert_chunks_and_calls()` for atomicity, but watch mode does not.
- **Suggested fix:** Restructure `reindex_files` to use `upsert_chunks_and_calls()` like the pipeline does. Extract relationships during the same loop that processes each file, and pass them to the atomic upsert. Type edges can be added to a combined atomic operation, or at minimum should be documented as best-effort with a recovery path (re-running `cqs index` fixes it).

#### DS-3: Notes summaries cache never invalidated in long-lived Store
- **Difficulty:** easy
- **Location:** src/store/mod.rs:170, src/store/mod.rs:704-712
- **Description:** `Store` uses `OnceLock<Vec<NoteSummary>>` for `notes_summaries_cache`. Once `cached_notes_summaries()` is called, the cache is set and never cleared. In watch mode, the Store lives for the entire session. When notes are reindexed via `reindex_notes()` (which calls `replace_notes_for_file()`), the cached summaries become stale. Subsequent searches will use stale note data for mention-based filtering and note boost scoring, potentially returning wrong results or missing newly added notes. The `OnceLock` type has no `clear()` method.
- **Suggested fix:** Replace `OnceLock<Vec<NoteSummary>>` with `std::sync::RwLock<Option<Vec<NoteSummary>>>` or `parking_lot::RwLock`. Add a `pub(crate) fn invalidate_notes_cache(&self)` method that clears the cache. Call it from `upsert_notes_batch()` and `replace_notes_for_file()`.

#### DS-4: GC prune operations not atomic — partial prune on crash
- **Difficulty:** easy
- **Location:** src/cli/commands/gc.rs:41-56
- **Description:** `cmd_gc` calls `prune_missing()`, `prune_stale_calls()`, and `prune_stale_type_edges()` as three separate operations. Each internally uses its own transaction (or no transaction for the stale calls/type edges, which are single DELETE statements). If the process crashes after `prune_missing()` but before `prune_stale_calls()`, the index will have orphaned call graph entries pointing to deleted chunks. While `prune_stale_calls()` would clean them up on the next GC run, queries using the call graph in the interim could return stale results or fail lookups. The `prune_missing()` function itself uses per-batch transactions (groups of 100), so a crash mid-prune also leaves partial state.
- **Suggested fix:** Wrap all three prune operations in a single transaction. Add a `pub fn gc_prune(&self, existing_files: &HashSet<PathBuf>) -> Result<(u32, u64, u64), StoreError>` method to Store that performs prune_missing + prune_stale_calls + prune_stale_type_edges atomically.

#### DS-5: HNSW copy fallback in save() is not atomic
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:225-236
- **Description:** When `std::fs::rename()` fails (cross-device, e.g., Docker overlayfs), the save path falls back to `std::fs::copy()`. Unlike rename, copy is not atomic — it creates a new file and writes bytes. If the process crashes during the copy of `hnsw.graph` or `hnsw.data`, the final file will be partially written. On next load, the checksum verification will catch this (the checksum file is renamed last), but the old valid index will already have been overwritten by the partial copy. This means the HNSW index is lost entirely — requiring a full rebuild with `cqs index`.
- **Suggested fix:** In the copy fallback path, copy to a second temp file in the target directory first, then rename that temp file to the final path. Since temp-to-final is on the same device, the rename will succeed atomically. Pattern: `copy(temp_dir/file, dir/.file.new) -> rename(dir/.file.new, dir/file)`.

#### DS-6: `prune_missing` uses per-batch transactions — incomplete prune on crash
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:474-507
- **Description:** `prune_missing()` deletes chunks for missing files in batches of 100, with each batch in its own transaction. If the process crashes after some batches but not all, the index is in a partially-pruned state: some missing files are removed, others remain. This is self-healing on the next GC run, but in the interim the HNSW index may have been rebuilt based on the partially-pruned state (GC rebuilds HNSW after prune). The HNSW index will then contain orphan IDs for chunks that were supposed to be pruned in later batches.
- **Suggested fix:** Use a single transaction for all prune batches. The batching is for SQL parameter limits, not transaction boundaries — a single transaction with multiple DELETE statements is fine and consistent. Change the loop to acquire `tx` before the loop and commit after.

#### DS-7: `rewrite_notes_file` opens file read-only then writes via temp — lock not held during write
- **Difficulty:** medium
- **Location:** src/note.rs:185-274
- **Description:** `rewrite_notes_file` acquires an exclusive lock on the notes file via `File::open()` with read-only mode (line 185-193). It reads the content, applies the mutation, then writes to a temp file and renames. The lock is on the original file handle opened read-only. On some systems (particularly WSL/NTFS), advisory locks acquired on a read-only file descriptor may not prevent writes by another process opening the same file. Additionally, the lock file handle is never explicitly closed before the rename — the rename replaces the inode that the lock is held on, which on some POSIX implementations releases the lock before the operation completes. In practice this is unlikely to cause issues for a single-user dev tool, but it's architecturally fragile.
- **Suggested fix:** Open the lock file with read+write mode (`.read(true).write(true)`) instead of read-only. This ensures the lock is valid for both read and write operations across all platforms.

## Security

#### SEC-1: SQLite connection URL constructed from unescaped file path — `?` or `#` in path injects connection parameters
- **Difficulty:** easy
- **Location:** src/store/mod.rs:180, src/store/mod.rs:286
- **Description:** `Store::open` and `Store::open_readonly` construct the SQLite connection URL via `format!("sqlite://{}?mode=rwc", path_str)` where `path_str` is the raw file path with only backslash-to-forward-slash conversion. If the database path contains a `?` character (valid on Unix filesystems), it splits the URL prematurely: e.g., path `/data/my?project/index.db` becomes `sqlite:///data/my?project/index.db?mode=rwc`, where `project/index.db?mode=rwc` is parsed as the query string, not the path. This can cause: (1) `mode=rwc` to be ignored (sqlx default mode may differ), (2) sqlx to attempt opening a different file than intended, (3) unexpected query parameters to be injected. While `?` in paths is uncommon, `#` (also URL-significant) is more plausible in project paths. The reference config `path` field is user-supplied via `cqs ref add` — a reference path containing `?` would silently open the wrong database. The risk is primarily data integrity (wrong database opened) rather than remote exploitation, since this is a local CLI tool.
- **Suggested fix:** Use sqlx's `SqliteConnectOptions::new().filename(path).create_if_missing(true)` which takes a `Path` directly, bypassing URL parsing entirely. This eliminates the entire class of URL injection issues. If the URL approach is kept, percent-encode the path before construction: `let encoded = path_str.replace('%', "%25").replace('?', "%3F").replace('#', "%23");` (encode `%` first to avoid double-encoding).

#### SEC-2: Temp file naming uses only PID + `subsec_nanos()` — low entropy on systems with coarse clocks
- **Difficulty:** easy
- **Location:** src/note.rs:233-237, src/config.rs:321-326, src/config.rs:393-398, src/audit.rs:115-120, src/project.rs:70-75
- **Description:** Five temp file creation sites use the pattern `format!("toml.{}.{}.tmp", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0))`. The PID component adds ~15 bits of entropy (typical PID range 1-32768), and `subsec_nanos()` adds up to 30 bits (0-999999999). However: (1) PIDs are predictable on single-user systems (sequential allocation), (2) `subsec_nanos()` resolution depends on the system clock — on some VMs and containers, the clock resolution is 1ms (only ~20 bits), (3) both values are deterministic for an attacker who can observe the process start time and system clock. For a local CLI tool this is low severity, but the v0.19.1 triage (SEC-1/SEC-8) that fixed "predictable temp file names" specifically called out PID+timestamp as the solution. A stronger approach would use random bytes. The temp files are written then immediately renamed, limiting the race window, but on slow filesystems (NFS, `/mnt/c/`) the window is wider.
- **Suggested fix:** Replace the PID+timestamp suffix with `std::collections::hash_map::RandomState` for process-unique randomness with no new dependencies: `use std::hash::{BuildHasher, Hasher}; let suffix = std::collections::hash_map::RandomState::new().build_hasher().finish();` producing `format!("toml.{:016x}.tmp", suffix)`. This gives 64 bits of unpredictable entropy per temp file.

#### SEC-3: `open_readonly` skips `PRAGMA quick_check` — reference indexes bypass corruption detection
- **Difficulty:** easy
- **Location:** src/store/mod.rs:279-340
- **Description:** `Store::open()` runs `PRAGMA quick_check` after connection to catch B-tree corruption early. `Store::open_readonly()` skips this check entirely — it only runs `check_schema_version`, `check_model_version`, and `check_cq_version`. Reference indexes loaded via `open_readonly` (used in `load_references`, `search_across_projects`, and `cmd_drift`) could have corrupted B-trees that silently produce wrong results rather than failing fast. SECURITY.md line 20 claims "Database corruption: PRAGMA quick_check on every database open" but this is inaccurate for read-only opens. While reference indexes are typically created by the user's own `cqs ref add`, they could become corrupted by interrupted writes, filesystem errors, or manual tampering. The DOC-6 finding in this audit's Documentation section also flags this discrepancy.
- **Suggested fix:** Add `PRAGMA quick_check` to `open_readonly`. The ~10-50ms cost per reference load is acceptable since references are loaded lazily and cached. Update SECURITY.md to accurately reflect the behavior.

#### SEC-4: `convert_directory` file walk does not filter symlinks — inconsistent with other convert walks
- **Difficulty:** easy
- **Location:** src/convert/mod.rs:345-360
- **Description:** The `convert_directory` function's `walkdir::WalkDir::new(dir)` at line 345 uses default settings which do not follow symlinks into directories. However, it also does not explicitly filter symlink entries with `filter_entry(|e| !e.path_is_symlink())`, unlike the CHM extraction (chm.rs:91) and webhelp conversion (webhelp.rs:60) which both explicitly skip symlinks. While `walkdir` defaults to not following directory symlinks, the inconsistency creates a defense-in-depth gap: a symlink to a file with a supported extension (e.g., `.pdf`, `.html`) would be detected by `detect_format()` at line 349 and passed to `convert_file()`, which reads and converts the target file content. The risk is information disclosure — a malicious symlink in the convert source could cause an external file's content to be written to the output directory. This is mitigated by the fact that users explicitly choose what directory to convert.
- **Suggested fix:** Add `.filter(|e| !e.path_is_symlink())` to the `convert_directory` walk chain, matching the pattern used in chm.rs:91 and webhelp.rs:60. This makes symlink handling consistent across all convert module walks.

#### SEC-5: `search_by_name` FTS query uses string interpolation — safety depends on undocumented sanitization ordering
- **Difficulty:** easy
- **Location:** src/store/mod.rs:598, src/store/chunks.rs:1170
- **Description:** Both `search_by_name` and `search_by_names_batch` construct FTS5 queries via `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)` where `normalized` has been through `sanitize_fts_query(normalize_for_fts(name))`. The defense is sound: `normalize_for_fts` strips to `[a-zA-Z0-9_ ]`, and `sanitize_fts_query` removes `"*()+-^:` and FTS5 boolean operators. After both transformations, the string cannot contain `"` to break out of the phrase. However: (1) the safety depends on the double-pass ordering — neither function documents that it MUST be called in this order, (2) the FTS query uses `format!` string interpolation rather than bind parameters (bind parameters work with `MATCH ?1` but cannot express column-scoped phrases like `name:"..."`), (3) a future caller adding a search variant that skips `normalize_for_fts` would need to know that `sanitize_fts_query` alone is still sufficient (it independently strips `"`). The risk is low since both layers are independently sufficient, but the lack of documentation creates maintenance risk.
- **Suggested fix:** Add a doc comment on `sanitize_fts_query`: `/// SAFETY: This function independently strips all FTS5-significant characters including double quotes. Safe for use in format!-constructed FTS5 queries even without normalize_for_fts().` Also add `debug_assert!(!normalized.contains('"'), "sanitized query must not contain double quotes");` at the FTS query construction sites.

#### SEC-6: `CQS_PDF_SCRIPT` override only logs at tracing level — user may not see the warning
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:57-63
- **Description:** When `CQS_PDF_SCRIPT` env var is set, `find_pdf_script` logs a `tracing::warn!` about using a custom script. This warning is only visible when `RUST_LOG` includes `warn` level, which is not the default for normal CLI usage. The custom script is passed to `python3` for execution. While this is intentional user customization and the env var is within the user's trust boundary, the silent override means a user could unknowingly run a script placed by another process or inherited from a parent shell. The non-.py extension check at line 59 also uses `tracing::warn!` only.
- **Suggested fix:** Print the warning to stderr unconditionally when `CQS_PDF_SCRIPT` is used: `eprintln!("cqs: Using custom PDF script: {}", script);`. This ensures visibility regardless of logging configuration.

## Performance

#### PF-1: `upsert_chunks_batch` and `upsert_chunks_and_calls` issue N individual SELECT queries to snapshot old content hashes
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:64-74, src/store/chunks.rs:332-343
- **Description:** Both `upsert_chunks_batch` (line 64) and `upsert_chunks_and_calls` (line 332) snapshot old content hashes before the batch INSERT with a per-chunk loop: `for (chunk, _) in chunks { sqlx::query_as("SELECT content_hash FROM chunks WHERE id = ?1").bind(&chunk.id)... }`. For a batch of 55 chunks, this issues 55 individual `SELECT` queries inside the transaction. During a full reindex, the pipeline writer calls `upsert_chunks_and_calls` per file group — a project with 1000 files averaging 5 chunks/file results in ~5000 individual SELECT queries just for content hash snapshotting. The content hash is only used to decide whether to re-normalize FTS text (PF-9 optimization from v0.19.2). This N+1 query pattern is the primary bottleneck in the storage stage of the indexing pipeline.
- **Suggested fix:** Replace the per-chunk loop with a single batch query: `SELECT id, content_hash FROM chunks WHERE id IN (...)`, batched in groups of 500 (respecting the SQLite 999-param limit). This reduces 55 queries to 1 per batch. The batch pattern already exists for `get_embeddings_by_ids` (chunks.rs:1104), `get_callers_with_context_batch` (calls.rs:537), and similar methods.

#### PF-2: Pipeline `needs_reindex` called per-chunk instead of per-file — redundant queries for multi-chunk files
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:362-380
- **Description:** The pipeline's mtime filter at line 364 calls `store.needs_reindex(&abs_path)` for every chunk in the batch. But `needs_reindex` queries by `origin` (file path), and a single file produces multiple chunks (one per function/struct/etc.). For a file with 10 chunks, 10 identical `SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1` queries are issued. The `file_mtimes` HashMap accumulates results for files that NEED reindexing (line 369), but files that are FRESH are not cached — their chunks all individually re-query. For a project with 1000 files where 90% are fresh, this means ~4500 redundant queries (5 chunks/file x 900 fresh files x 4 redundant chunks/file).
- **Suggested fix:** Check `file_mtimes` before calling `needs_reindex`, and also cache negative results (fresh files) in a separate `HashSet<PathBuf>`:
  ```rust
  let mut fresh_files: HashSet<PathBuf> = HashSet::new();
  // In filter closure:
  if file_mtimes.contains_key(&c.file) { return true; }
  if fresh_files.contains(&c.file) { return false; }
  match store.needs_reindex(&abs_path) { ... }
  ```

#### PF-3: `search_by_names_batch` post-filter calls `score_name_match` with redundant `to_lowercase()` allocations per row x batch
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1205-1218, src/store/helpers.rs:594-609
- **Description:** After fetching FTS results for a batch of 20 names, each result row iterates over all 20 names in the batch calling `score_name_match` (line 1207). `score_name_match` (helpers.rs:598-599) calls `name.to_lowercase()` and `query.to_lowercase()` on every invocation — each involving a heap allocation. For a batch of 20 names returning 200 rows, this is 4000 `score_name_match` calls = 8000 `to_lowercase()` allocations. The chunk name (`chunk.name`) is the same for a given row but is re-lowercased for each batch name comparison. The batch names are the same across all rows but are re-lowercased for each row. This function is called during `gather` BFS expansion (potentially with 200 names batched at 20), `impact` caller resolution, and `read --focus` type resolution — all hot paths.
- **Suggested fix:** Pre-lowercase the batch names outside the row loop. Also lowercase the chunk name once per row. Either add a `score_name_match_pre_lower(name_lower: &str, query_lower: &str) -> f32` variant, or restructure the loop to lowercase outside.

#### PF-4: `note_boost` iterates all notes x all mentions per chunk in brute-force search inner loop
- **Difficulty:** easy
- **Location:** src/search.rs:266-288, called at src/search.rs:558
- **Description:** `note_boost(file_path, chunk_name, notes)` is called for every chunk in the brute-force search inner loop (line 558). It iterates over ALL notes and ALL their mentions, calling `path_matches_mention` (string prefix/suffix operations) for each. With 50 notes averaging 2 mentions each, that's 100 string comparisons per chunk. For a 10,000-chunk brute-force scan, that's 1,000,000 string comparisons just for note boosting. The same file path appears for multiple chunks from the same file, but the note scan is repeated for each.
- **Suggested fix:** Pre-compute a lookup structure before entering the scoring loop. Build two maps: `HashMap<&str, f32>` from file-path to strongest sentiment (iterating notes once), and similarly for names. Then `note_boost` becomes two HashMap lookups (O(1)) instead of a linear scan. The pre-computation cost is O(notes x mentions) once, amortized across all chunks.

#### PF-5: HNSW-guided `search_by_candidate_ids` loads full content + doc for all 500 candidates before scoring
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:1235-1270, src/search.rs:694-697
- **Description:** `search_by_candidate_ids` (the HNSW-guided search path) calls `fetch_chunks_with_embeddings_by_ids_async` which SELECTs all 12 columns including `content` (often 1-10KB per chunk) and `doc`. The scoring phase (search.rs:709-770) only uses `name`, `origin`, `language`, `chunk_type`, and `embedding`. Content is never accessed during scoring — only needed after top-N selection for building the final `SearchResult` via `ChunkSummary::from(row)`. For 500 HNSW candidates (default: `limit * 5 = 50`), this loads content unnecessarily for candidates that won't make the final cut. The brute-force path correctly uses a two-phase approach: lightweight columns during scoring (search.rs:466-468), then full content fetch only for top-N (search.rs:608-610).
- **Suggested fix:** Split the HNSW path into two phases like brute-force: (1) fetch `id, origin, language, chunk_type, name, embedding` for scoring, (2) fetch full content via `fetch_chunks_by_ids_async` only for the top-N after scoring and parent dedup. Create a `fetch_lightweight_with_embeddings_by_ids_async` that omits `content`, `doc`, `signature`, and `line_start/end`.

#### PF-6: `analyze_impact` loads test chunks redundantly — calls `find_affected_tests` which reloads internally
- **Difficulty:** easy
- **Location:** src/impact/analysis.rs:27-28, src/impact/analysis.rs:136-141
- **Description:** `analyze_impact` at line 27 loads the call graph, then calls `find_affected_tests(store, &graph, target_name)` at line 28. `find_affected_tests` internally calls `store.find_test_chunks()` at line 141. A `find_affected_tests_with_chunks` variant exists at line 170 that accepts pre-loaded test chunks. The redundancy means every `cqs impact <function>` invocation loads test chunks (a full table scan with LIKE patterns on content) that could be pre-loaded once. Other callers (review, diff-impact, task) already use the `_with_chunks` variant.
- **Suggested fix:** Change `analyze_impact` to pre-load test_chunks and call `find_affected_tests_with_chunks`:
  ```rust
  let test_chunks = store.find_test_chunks()?;
  let tests = find_affected_tests_with_chunks(&graph, &test_chunks, target_name, DEFAULT_MAX_TEST_SEARCH_DEPTH);
  ```

#### PF-7: `get_call_graph` called 15 times across codebase with no caching — each call scans entire `function_calls` table
- **Difficulty:** medium
- **Location:** src/store/calls.rs:469 (definition), 15 call sites across 11 files
- **Description:** `get_call_graph()` runs `SELECT DISTINCT caller_name, callee_name FROM function_calls LIMIT 500000`, builds two `HashMap<String, Vec<String>>` adjacency lists, and clones each string once (for forward/reverse entries). For a typical project (~2000 edges), this takes 5-20ms. While several callers (task, review, batch) properly pre-load and share the graph, many do not: `analyze_impact` (analysis.rs:27), `suggest_tests` (analysis.rs:253), `compute_hints` (hints.rs:84), `gather` (gather.rs:325), `onboard` (onboard.rs:139), `health_check` (health.rs:82), and `suggest` (suggest.rs:113) all load independently. When `cqs ci` runs review + dead code + gate, the call graph is loaded at least 3 times. BatchContext caches it (batch/mod.rs:181), but CLI single-command paths don't.
- **Suggested fix:** Add a `OnceLock<CallGraph>` cache to `Store`, similar to the existing `notes_summaries_cache`. The first `get_call_graph()` populates the cache; subsequent calls return a reference. Invalidate when `function_calls` table is modified (in `upsert_chunks_and_calls`, `replace_file_chunks`, `prune_stale_calls`). This is transparent — callers don't change.

#### PF-8: `find_dead_code` Phase 1 uses `NOT IN (subquery)` anti-pattern instead of `LEFT JOIN ... IS NULL`
- **Difficulty:** easy
- **Location:** src/store/calls.rs:714-722
- **Description:** The Phase 1 query finds uncalled functions via: `WHERE c.name NOT IN (SELECT DISTINCT callee_name FROM function_calls)`. With `NOT IN`, SQLite materializes the full subquery result set into a temporary table, then checks each chunk name against it. For a project with 5000 chunks and 2000 call edges, this forces a full materialization of the DISTINCT callee set. The `NOT EXISTS` or `LEFT JOIN ... WHERE IS NULL` patterns allow SQLite to use the `idx_function_calls_callee` index with an anti-join optimization, avoiding materialization.
- **Suggested fix:** Replace with: `LEFT JOIN function_calls fc ON c.name = fc.callee_name WHERE fc.callee_name IS NULL`. Or use `NOT EXISTS (SELECT 1 FROM function_calls WHERE callee_name = c.name LIMIT 1)`. Both patterns enable anti-join optimization with the existing callee index.

#### PF-9: `search_filtered` rebuilds identical SQL string and bind parameters on every cursor batch iteration
- **Difficulty:** easy
- **Location:** src/search.rs:489-518
- **Description:** The cursor-based batching loop at line 494 rebuilds the SQL string, WHERE clause, and bind parameter list on every iteration. The `conditions`, `bind_values`, `columns`, and `batch_where` format strings are identical across iterations — only `last_rowid` (line 515) changes. For large indexes (50,000+ chunks), the 10+ iterations each rebuild the same `IN (?, ?, ...)` placeholder string via `format!()`. While string formatting is cheap relative to I/O, the rebuild also requires re-binding all filter values (line 512-514) per batch, which involves cloning and type conversions.
- **Suggested fix:** Hoist the SQL template construction out of the loop. Build the SQL string once before the loop with fixed placeholder positions for rowid and limit. Only rebind `last_rowid` per iteration. This simplifies the code and avoids redundant work.

#### PF-10: `find_test_chunks` scans content with `LIKE '%marker%'` — full table scan on large BLOB column
- **Difficulty:** medium
- **Location:** src/store/calls.rs:946-978
- **Description:** `find_test_chunks_async` builds a SQL query with `content LIKE '%#[test]%' OR content LIKE '%@Test%'` patterns. The `LIKE '%...%'` operator on the `content` column forces SQLite to read and scan the full content of every callable chunk row — typically 1-10KB per row. For a 10,000-chunk index, this scans ~50MB of data. The `chunks_fts` table already indexes content via FTS5 and could accelerate this. Additionally, `find_test_chunks` is called from 13 sites across the codebase (often not cached in CLI single-command paths). Each call rescans the same data.
- **Suggested fix:** Two improvements: (1) Cache the result in `Store` with `OnceLock<Vec<ChunkSummary>>` (invalidated on chunk upsert), since the test chunk set rarely changes during a session. (2) For the SQL itself, replace the `content LIKE '%marker%'` patterns with a JOIN on `chunks_fts`: `c.id IN (SELECT id FROM chunks_fts WHERE chunks_fts MATCH 'content:"test"')`. FTS5's inverted index makes content keyword searches O(matches) instead of O(total_rows).


## Resource Management

#### RM-1: `count_vectors()` reads entire ID map file into memory as a string
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:389-413
- **Description:** `count_vectors()` calls `std::fs::read_to_string(&id_map_path)` to load the entire `.hnsw.ids` JSON file into a String, then checks its length against a 100MB cap. The function only needs the vector count (number of JSON array elements), not the actual data. For a 100K-chunk project, the id map is ~3-4MB — this reads the whole string just to count entries. The `load()` function correctly uses `BufReader` with streaming JSON parse for the same file, but `count_vectors` does not. Peak memory is 2x file size momentarily (String + serde parse). The 100MB guard prevents catastrophic cases but the allocation is still wasteful for a "stats" operation that should be lightweight.
- **Suggested fix:** Use `BufReader` and `serde_json::from_reader::<_, Vec<String>>` to count entries without holding the whole file as a raw string, or stream-parse to count array elements without deserializing.

#### RM-2: `gather()` and `scout()` load the full call graph on every standalone CLI invocation
- **Difficulty:** medium
- **Location:** src/gather.rs:325, src/scout.rs:146-147
- **Description:** `gather()` calls `store.get_call_graph()` on line 325, loading up to 500K edges into two `HashMap<String, Vec<String>>` adjacency lists. `scout()` independently loads both the call graph (line 146) and test chunks (line 147). When called from `BatchContext` or `task()`, these are cached. But standalone CLI calls (e.g., `cqs gather "query"`, `cqs scout "task"`) build and discard the graph each time. For typical projects (~2000 edges): ~200KB and ~50ms — fine. For larger projects approaching the 500K edge cap: ~50MB of String data allocated and immediately discarded. The code comments on gather.rs:321-324 acknowledge this: "If gather() is ever called in a loop, accept a pre-loaded &CallGraph parameter." The `task()` function already demonstrates the shared-resource pattern (loading once, passing by reference). This is an API design gap rather than a bug.
- **Suggested fix:** Add `scout_with_resources()` and `gather_with_graph()` variants (like `task_with_resources()`) that accept a pre-loaded `&CallGraph`. Wire CLI commands that chain scout+gather (e.g., `cqs task`) to use these. Low priority since standalone CLI calls are single-shot.

#### RM-3: `find_dead_code()` loads full `ChunkSummary` structs just to extract test names
- **Difficulty:** easy
- **Location:** src/store/calls.rs:768-773
- **Description:** `find_dead_code()` calls `self.find_test_chunks_async().await?` which returns `Vec<ChunkSummary>` — full structs with content, doc, embedding data, etc. The result is immediately reduced to just names via `.map(|c| c.name).collect()` into a `HashSet<String>`. For 500 test functions averaging 1KB content each, this loads ~500KB of content that's immediately discarded. The content is the largest field in `ChunkSummary` and is never used here.
- **Suggested fix:** Add a `find_test_chunk_names()` method that runs `SELECT name FROM chunks WHERE chunk_type IN (...)` returning just names. Use it here instead of the full `find_test_chunks_async()`.

#### RM-4: Store `mmap_size` documented as 256MB per connection but virtual address cost is benign
- **Difficulty:** easy (documentation only)
- **Location:** src/store/mod.rs:214
- **Description:** `Store::open()` sets `PRAGMA mmap_size = 268435456` (256MB) per connection with a 4-connection pool. This reserves up to 1GB of virtual address space per store. `open_readonly()` uses 64MB × 1 connection. When searching across references, each reference adds 64MB. With 3 references: 1GB (project) + 192MB (refs) = ~1.2GB virtual. On 64-bit Linux, virtual address space is 128TB — this is not a real memory issue. Mmap pages are demand-paged from the file, and the OS evicts them under memory pressure. The 256MB cap is generous for typical databases of 10-50MB but prevents mmap from growing unboundedly on very large indexes. However, the idle memory cost (RSS) is worth noting: SQLite's mmap keeps recently-accessed pages resident, so after a large search the RSS may stay elevated at the actual DB size (not the 256MB cap) until the OS reclaims pages.
- **Suggested fix:** Add a module-level doc comment to `store/mod.rs` documenting the mmap virtual address footprint and that it's intentional/benign on 64-bit systems.

#### RM-5: `merge_results()` hashes all results before truncating to limit
- **Difficulty:** easy
- **Location:** src/reference.rs:186-198
- **Description:** `merge_results()` sorts all results by score, then runs blake3 content hashing on every `UnifiedResult::Code` for deduplication, then truncates to `limit`. The hashing runs on ALL results, not just the top `limit`. For typical usage (10-50 results), this is trivial. But with multiple references returning high limits (e.g., 5 refs × 100 results each = 500 results), all 500 are hashed before only the top 10-20 are kept. Blake3 is extremely fast (~1GB/s) so even 500 results × 500 bytes completes in microseconds. More significantly, the content strings themselves are held in memory for all 500 results until `tagged.truncate(limit)` drops the excess.
- **Suggested fix:** Truncate to `limit * 3` before the dedup loop to bound hashing work, then `truncate(limit)` after. Low priority given blake3's speed and typical result counts.

#### RM-6: `BatchContext` holds embedder + reranker + HNSW simultaneously (~1GB peak) with no idle timeout
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:46-63
- **Description:** `BatchContext` lazily initializes and caches 11 resources for session lifetime. The heavy ones: embedder (~500MB ONNX session), reranker (~200MB ONNX session), HNSW index (~3KB/vector, so 100K vectors = ~300MB). If a batch session uses all three, peak memory is ~1GB for model/index data alone, plus SQLite mmap and call graph cache. This is the expected cost of the "amortize init across N commands" design, well-documented in the module header. Watch mode has `clear_session()` after 5 minutes idle (lines 265-272); batch mode does not. A long-running batch session (hours, during a coding session) holds all resources even between commands.
- **Suggested fix:** Add an idle-timeout pattern to `BatchContext` that clears the embedder and reranker ONNX sessions after N minutes of no commands (matching watch mode's `cycles_since_clear` pattern). Sessions re-initialize on next use. Low priority since batch sessions are typically task-scoped.

#### RM-7: `embed_batch()` per-batch tensor allocation is well-sized and inherent
- **Difficulty:** n/a (informational)
- **Location:** src/embedder.rs:511-514
- **Description:** `embed_batch()` allocates three `Array2<i64>` tensors per batch: `input_ids_arr`, `attention_mask_arr`, `token_type_ids_arr`. Each is `batch_size × max_len` elements of i64. For pipeline's `EMBED_BATCH_SIZE=32` and `max_length=512`: 3 × 32 × 512 × 8 = ~393KB per batch. The ONNX runtime takes ownership (`Tensor::from_array` consumes the array), so buffer reuse is impossible without API changes. Total per-batch: ~400KB for tensors + ~100KB for mean-pooling buffers. Modest and proportional.
- **Suggested fix:** None needed. Allocation is inherent to the ONNX inference pattern.

#### RM-8: HNSW `build()` loads all embeddings at once — mitigated by `build_batched()` but routing unverified
- **Difficulty:** easy (verify routing)
- **Location:** src/hnsw/build.rs:52-100
- **Description:** `HnswIndex::build()` calls `store.all_embeddings()` loading every embedding (~3KB each) into a Vec. For 100K chunks: ~300MB peak. The batched alternative `build_batched()` (line 108+) streams in batches. The comment says "For >50k chunks, prefer build_batched()." Watch mode's `build_hnsw_index` uses `build()` for rebuilds after small file changes (appropriate since only current corpus). The concern is whether initial large-index builds via `cmd_index` correctly route to `build_batched()` for large corpora.
- **Suggested fix:** Verify that `cmd_index` routes to `build_batched()` for indexes exceeding 50K chunks. If not, add a threshold check in `build_hnsw_index`.

#### RM-9: Pipeline bounded channels correctly limit in-flight memory
- **Difficulty:** n/a (informational)
- **Location:** src/cli/pipeline.rs:40-42
- **Description:** The 3-stage pipeline uses `crossbeam_channel::bounded(256)` for both parser→embedder and embedder→writer channels. Worst case: parser→embedder = 256 messages × 20 chunks × 1KB = ~5MB; embedder→writer = 256 messages × 20 chunks × 4KB = ~20MB. Total buffering: ~25MB peak. Bounded channels provide backpressure — if the embedder is slow, the parser blocks. Well-designed.
- **Suggested fix:** None needed. Bounded channels correctly limit in-flight memory.

#### RM-10: Watch mode correctly manages idle resources
- **Difficulty:** n/a (informational)
- **Location:** src/cli/watch.rs:88-89, 265-272, 159, 201
- **Description:** Watch mode uses `OnceCell<Embedder>` for lazy init (~500MB only when files change) and clears the session after ~5 minutes idle (3000 cycles × 100ms, line 267-271). The `last_indexed_mtime` HashMap is pruned after each reindex (line 201: `retain(|f, _| root.join(f).exists())`). The `pending_files` HashSet is shrunk after drain (line 159: `shrink_to(64)`). Store connection pool has 300s idle timeout. Thorough resource management for a long-running daemon.
- **Suggested fix:** None needed. Idle resource management is exemplary.
