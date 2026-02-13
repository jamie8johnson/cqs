# Audit Findings — v0.12.1

Generated: 2026-02-11

## API Design

#### AD-1: `CallerInfo` name collision between `store::helpers` and `impact`
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:209, src/impact.rs:13
- **Description:** Two distinct `CallerInfo` structs exist with different fields. `store::helpers::CallerInfo` has `{name, file, line}` and is re-exported from `store::mod`. `impact::CallerInfo` has `{name, file, line, call_line, snippet}`. This forces callers to disambiguate with full paths and confuses code readers about which `CallerInfo` they're dealing with. The `impact` version is re-exported from `lib.rs` too, so both are in the public namespace.
- **Suggested fix:** Rename `impact::CallerInfo` to `ImpactCaller` or `CallerDetail`, since it extends the base type with call-site context (snippet, call_line). Alternatively, unify by adding optional fields to one type.

#### AD-2: Inconsistent error types across analysis modules
- **Difficulty:** medium
- **Location:** src/scout.rs:340, src/where_to_add.rs:325, src/impact.rs:53, src/gather.rs:78, src/related.rs:37
- **Description:** Each analysis module uses a different error strategy:
  - `scout` → `ScoutError` (custom enum: Store, Embedder)
  - `where_to_add` → `SuggestError` (custom enum: Embedding, Store)
  - `impact` → `anyhow::Result`
  - `gather` → `anyhow::Result`
  - `related` → `Result<_, StoreError>`
  - `diff` → `Result<_, StoreError>`

  `ScoutError` and `SuggestError` are structurally identical (both wrap `StoreError` + embedder string). `impact` and `gather` use `anyhow` while similar modules use typed errors. This inconsistency means callers can't uniformly handle errors from analysis functions.
- **Suggested fix:** Either (a) standardize on `anyhow::Result` for all analysis functions (they're all called from CLI, which uses anyhow), or (b) create a shared `AnalysisError` enum with `Store(StoreError)` and `Embedder(String)` variants, used by all analysis modules. Option (a) is simpler and matches the actual usage.

#### AD-3: `suggest_placement` has unused `_root` parameter
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:59
- **Description:** The `_root: &Path` parameter is accepted but never used (prefixed with underscore to suppress warnings). This is a dead parameter that confuses the API — callers must pass a value that has no effect.
- **Suggested fix:** Remove the parameter. If it was intended for path relativization (like `scout` and `gather` do), wire it in. Otherwise, drop it.

#### AD-4: Inconsistent JSON serialization patterns
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:167, src/impact.rs:278, src/scout.rs:272
- **Description:** Three different patterns for JSON serialization:
  1. **Method on type**: `SearchResult::to_json()`, `SearchResult::to_json_relative()`, `NoteSearchResult::to_json()`, `UnifiedResult::to_json()` — instance methods on the result types.
  2. **Free function with result + root**: `impact_to_json(result, root)`, `diff_impact_to_json(result, root)`, `scout_to_json(result, root)` — standalone functions.
  3. **No serialization**: `PlacementResult`, `RelatedResult`, `GatherResult` — CLI constructs JSON inline.

  The free functions exist because they need `root` for path relativization, but `SearchResult::to_json_relative(root)` shows that methods can accept root too. Pattern 3 means CLI commands duplicate JSON construction logic.
- **Suggested fix:** Add `to_json(root)` methods to `ImpactResult`, `DiffImpactResult`, `ScoutResult`, `PlacementResult`, `RelatedResult`, and `GatherResult`. Keep the free functions as deprecated aliases if needed. This makes serialization discoverable via the type.

#### AD-5: `ScoutChunk.chunk_type` is `String` instead of `ChunkType` enum
- **Difficulty:** easy
- **Location:** src/scout.rs:30
- **Description:** `ScoutChunk` stores `chunk_type: String` while the source data (`ChunkSummary`) has `chunk_type: ChunkType`. The conversion happens at line 163: `chunk.chunk_type.to_string()`. This discards type safety — downstream code can't pattern-match on the type. Same issue noted in v0.9.7 for `ChunkIdentity` (A1), but `ScoutChunk` is a new module not covered by that finding.
- **Suggested fix:** Change `ScoutChunk.chunk_type` to `ChunkType`. Update `scout_to_json` to call `.to_string()` at serialization time instead.

#### AD-6: `related.rs` compares `ChunkType` via string instead of enum
- **Difficulty:** easy
- **Location:** src/related.rs:104-105
- **Description:** `find_type_overlap` filters chunks with `chunk.chunk_type.to_string() != "function" && chunk.chunk_type.to_string() != "method"`. This calls `to_string()` twice per chunk and relies on Display output strings matching. The `ChunkType` enum supports direct comparison.
- **Suggested fix:** Replace with `!matches!(chunk.chunk_type, ChunkType::Function | ChunkType::Method)`.

#### AD-7: `resolve_target` returns unnamed tuple `(ChunkSummary, Vec<SearchResult>)`
- **Difficulty:** easy
- **Location:** src/search.rs:45
- **Description:** The return type is `Result<(ChunkSummary, Vec<SearchResult>), StoreError>` — an unnamed tuple. Callers access via positional indexing (`resolved.0`, `resolved.1`), which is opaque. The `Vec<SearchResult>` is the full name-search results (used by `explain` to show alternates), while the `ChunkSummary` is the best match. This is noted in the prior audit as A2 (asymmetric callers/callees return types), but `resolve_target` is a separate case.
- **Suggested fix:** Introduce a `ResolvedTarget { chunk: ChunkSummary, alternates: Vec<SearchResult> }` struct.

#### AD-8: `gather()` takes `project_root` for path stripping but `scout()` and `suggest_placement` handle it differently
- **Difficulty:** easy
- **Location:** src/gather.rs:118, src/scout.rs:89, src/where_to_add.rs:59
- **Description:** Path relativization is handled inconsistently:
  - `gather(store, query_emb, query_text, opts, project_root)` — strips prefix inside the function.
  - `scout(store, embedder, task, root, limit)` — does NOT strip paths internally; `scout_to_json()` does it.
  - `suggest_placement(store, embedder, desc, _root, limit)` — accepts `root` but never uses it; paths are absolute in results.

  This means callers get different path formats depending on which function they call: `gather` returns relative paths, `scout` returns absolute paths (serialization handles it), and `suggest_placement` returns absolute paths with no way to relativize.
- **Suggested fix:** Pick one convention: either all analysis functions return absolute paths (and `to_json` relativizes), or all accept root and relativize internally. The `scout` pattern (absolute paths in result, relativize in serializer) is cleanest.

#### AD-9: `ScoutError` and `SuggestError` don't implement `std::error::Error` consistently
- **Difficulty:** easy
- **Location:** src/scout.rs:340-352, src/where_to_add.rs:325-345
- **Description:** `SuggestError` implements `std::error::Error` (line 339). `ScoutError` implements `Display` but does NOT implement `std::error::Error`. This means `ScoutError` can't be used with `?` in functions returning `anyhow::Result` without `.map_err()`, and doesn't participate in error source chains.
- **Suggested fix:** Add `impl std::error::Error for ScoutError {}` (or derive with `thiserror`).

#### AD-10: `GatherDirection::FromStr` uses `anyhow::Error` as error type
- **Difficulty:** easy
- **Location:** src/gather.rs:77
- **Description:** `FromStr` for `GatherDirection` returns `anyhow::Error` as the error type. This is unusual — `FromStr` implementations typically use a lightweight error type. It couples the gather module to anyhow at the type level, preventing use in no-anyhow contexts. All other enums in the codebase that implement `FromStr` (like `ChunkType`, `Language`) use string-based errors or custom types.
- **Suggested fix:** Use a simple error type: `type Err = String` with `Err(format!(...))`, matching the pattern used by `Language::FromStr` and `ChunkType::FromStr`.

#### AD-11: `analyze_diff_impact` returns empty `changed_functions` field
- **Difficulty:** easy
- **Location:** src/impact.rs:528
- **Description:** `analyze_diff_impact()` returns `DiffImpactResult` with `changed_functions: Vec::new()` and a comment "filled by caller". The caller (`cmd_impact_diff`) must manually set this field after the call. This is a leaky API — the function knows about the changed functions (it receives `&[ChangedFunction]`) but delegates populating the output to the caller.
- **Suggested fix:** Clone the input `changed` slice into `changed_functions` inside `analyze_diff_impact`, or accept ownership via `Vec<ChangedFunction>`.

## Observability

#### OB-1: `scout()` does search + call graph + staleness check with zero tracing
- **Difficulty:** easy
- **Location:** src/scout.rs:84-224
- **Description:** `scout()` is a 140-line function that orchestrates embedding, search, call graph loading, batch caller counts, staleness checks, and file grouping. It has no tracing span, no timing, no logging of result counts. When scout returns unexpected results (empty, slow, wrong grouping), there's no way to diagnose which phase failed or was slow. This is new code added in PR #370, after the v0.9.7 audit.
- **Suggested fix:** Add `let _span = tracing::info_span!("scout", task_len = task.len(), limit).entered();` at the top. Log result count: `tracing::debug!(file_groups = groups.len(), total_functions, "scout complete");`

#### OB-2: `suggest_placement()` has no tracing span
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:55-144
- **Description:** `suggest_placement()` does embedding, search, file grouping, chunk loading per file, and pattern extraction. No tracing at all. New code from PR #369. When it returns no suggestions or unexpected files, there's no diagnostic output to determine whether search returned nothing or pattern extraction failed.
- **Suggested fix:** Add `let _span = tracing::info_span!("suggest_placement", desc_len = description.len(), limit).entered();` at the top. Log `tracing::debug!(suggestions = suggestions.len(), "placement complete");` before return.

#### OB-3: `find_related()` has no tracing span
- **Difficulty:** easy
- **Location:** src/related.rs:33-61
- **Description:** `find_related()` resolves a target, queries shared callers, shared callees, and shared types via three separate store queries plus signature search. No tracing. New code from PR #367. The function does 4+ database round trips with no visibility into which is slow or returns empty.
- **Suggested fix:** Add `let _span = tracing::info_span!("find_related", target = target_name).entered();` and log counts: `tracing::debug!(shared_callers = shared_callers.len(), shared_callees = shared_callees.len(), shared_types = shared_types.len(), "related analysis complete");`

#### OB-4: `analyze_impact()` does BFS + test discovery with no tracing span
- **Difficulty:** easy
- **Location:** src/impact.rs:49-69
- **Description:** `analyze_impact()` loads callers, builds full call graph, runs BFS test discovery, and optionally finds transitive callers. No tracing span or timing. This function existed before v0.9.7 but was in the MCP tools then; now it's the shared core extracted in PR #333. For large codebases, the BFS can be slow, and there's no visibility into caller count vs test count vs graph loading time.
- **Suggested fix:** Add `let _span = tracing::info_span!("analyze_impact", target = target_name, depth).entered();` and `tracing::debug!(callers = callers.len(), tests = tests.len(), transitive = transitive_callers.len(), "impact complete");`

#### OB-5: `analyze_diff_impact()` iterates all changed functions with no tracing
- **Difficulty:** easy
- **Location:** src/impact.rs:460-533
- **Description:** `analyze_diff_impact()` loads call graph and test chunks, then iterates all changed functions running BFS for each. No tracing span, no logging of how many functions processed or aggregate caller/test counts. New code from PR #362. For large diffs touching many functions, this can be slow with no diagnostic visibility.
- **Suggested fix:** Add `let _span = tracing::info_span!("analyze_diff_impact", changed_count = changed.len()).entered();` and `tracing::debug!(callers = all_callers.len(), tests = all_tests.len(), "diff impact complete");`

#### OB-6: `map_hunks_to_functions()` has no tracing
- **Difficulty:** easy
- **Location:** src/impact.rs:417-454
- **Description:** `map_hunks_to_functions()` groups hunks by file, queries the store for each file's chunks, and computes line-range overlaps. No tracing. New code from PR #362. When it returns empty (meaning no indexed functions overlap the diff), the user gets "No indexed functions affected" but can't tell if it was a file mismatch, a line-range issue, or an empty index.
- **Suggested fix:** Add `tracing::debug!(hunks = hunks.len(), files = by_file.len(), matched = functions.len(), "mapped hunks to functions");`

#### OB-7: 11 CLI commands missing tracing spans (new since v0.9.7)
- **Difficulty:** easy
- **Location:** Multiple files in src/cli/commands/
- **Description:** The following CLI command functions have no `info_span!` at entry, making them invisible in tracing output. Prior audit (v0.9.7) found this for `cmd_gather` and `cmd_gc`, which are now fixed. These are all new or extracted since then:
  - `cmd_scout` (src/cli/commands/scout.rs:10) — new PR #370
  - `cmd_where` (src/cli/commands/where_cmd.rs:9) — new PR #369
  - `cmd_related` (src/cli/commands/related.rs:9) — new PR #367
  - `cmd_impact` (src/cli/commands/impact.rs:11) — extracted PR #333
  - `cmd_impact_diff` (src/cli/commands/impact_diff.rs:12) — new PR #362
  - `cmd_dead` (src/cli/commands/dead.rs:12) — existed but still missing
  - `cmd_explain` (src/cli/commands/explain.rs:12) — existed but still missing
  - `cmd_context` (src/cli/commands/context.rs:10) — existed but still missing
  - `cmd_similar` (src/cli/commands/similar.rs:37) — existed but still missing
  - `cmd_trace` (src/cli/commands/trace.rs:14) — existed but still missing
  - `cmd_test_map` (src/cli/commands/test_map.rs:12) — existed but still missing
  Only `cmd_query`, `cmd_gather`, `cmd_gc`, `cmd_index`, and `cmd_stale` have spans. This means 11 of 16 substantive CLI commands are invisible to tracing.
- **Suggested fix:** Add `let _span = tracing::info_span!("cmd_NAME", ...).entered();` to each. Mechanical change, 1-2 lines per file.

#### OB-8: `compute_hints()` silently discards errors via `.ok()`
- **Difficulty:** easy
- **Location:** src/cli/commands/explain.rs:103, src/cli/commands/read.rs:151
- **Description:** Both `cmd_explain` and `cmd_read` call `compute_hints(&store, &chunk.name, ...).ok()` which silently discards errors. If call graph loading or test chunk queries fail, the hints section is just omitted with no warning. The user sees no "caller_count" or "test_count" and has no idea why. This is different from the prior audit's EH5/EH6 (impact/test_map swallowing errors) — those were fixed. These are still present.
- **Suggested fix:** Replace `.ok()` with:
  ```rust
  match compute_hints(&store, &chunk.name, Some(callers.len())) {
      Ok(h) => Some(h),
      Err(e) => { tracing::warn!(error = %e, "Failed to compute hints"); None }
  }
  ```

#### OB-9: `suggest_tests()` silently returns empty on call graph or test chunk load failure
- **Difficulty:** easy
- **Location:** src/impact.rs:611-618
- **Description:** `suggest_tests()` calls `store.get_call_graph()` and `store.find_test_chunks()` and returns `Vec::new()` on error for either. No logging. The caller (`cmd_impact` with `--suggest-tests`) shows "no suggestions" but can't distinguish "no untested callers" from "database error loading test data."
- **Suggested fix:** Add `tracing::warn!(error = %e, "Failed to load call graph for test suggestions");` and similar for test chunk failure.

#### OB-10: `find_relevant_notes()` in scout silently swallows note listing errors
- **Difficulty:** easy
- **Location:** src/scout.rs:243-246
- **Description:** `find_relevant_notes()` calls `store.list_notes_summaries()` and returns empty `Vec` on `Err(_)`. No logging. If notes fail to load, the scout result silently omits them.
- **Suggested fix:** Add `tracing::debug!(error = %e, "Failed to load notes for scout");` in the `Err` branch.

#### OB-11: `staleness.rs` uses `eprintln!` for stale file warnings instead of tracing
- **Difficulty:** easy
- **Location:** src/cli/staleness.rs:22-29
- **Description:** `warn_stale_results()` uses `eprintln!` with colored output for the "N result files changed since last index" warning. While stderr is intentional (to not pollute JSON), this means the warning is invisible to tracing subscribers and log aggregation. The error path at line 35 correctly uses `tracing::debug!`, creating an inconsistency within the same function.
- **Suggested fix:** Use `tracing::warn!` for the staleness warning. The tracing subscriber can be configured to write to stderr. Alternatively, keep `eprintln!` for the user-facing message but ALSO emit `tracing::info!(stale_count = count, "Stale files detected in results");` for structured logging.

## Error Handling

#### EH-1: `diff_parse.rs` uses `Regex::new().unwrap()` on every call
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:31
- **Description:** `parse_unified_diff` calls `Regex::new(...).unwrap()` inside the function body. This: (a) violates project convention (no `.unwrap()` outside tests), and (b) re-compiles the regex on every invocation. Prior audit P1 #13 fixed the identical pattern in the markdown parser via `LazyLock`.
- **Suggested fix:** Move `hunk_re` to a `static` `LazyLock<Regex>` at module level with `.expect("hardcoded regex")`, matching the pattern in `nl.rs`, `store/calls.rs`, and `focused_read.rs`.

#### EH-2: `ScoutError` does not implement `std::error::Error`
- **Difficulty:** easy
- **Location:** src/scout.rs:339-352
- **Description:** `ScoutError` implements `Display` but not `std::error::Error`. The CLI works around this via `map_err(|e| anyhow::anyhow!("{e}"))` at `cli/commands/scout.rs:29`, which loses the error's source chain. `SuggestError` (where_to_add.rs:339) correctly implements `std::error::Error` — `ScoutError` should match. (Also noted in AD-9.)
- **Suggested fix:** Add `impl std::error::Error for ScoutError {}` and replace the CLI `map_err` with direct `?`.

#### EH-3: `SuggestError` and `ScoutError` wrap embedding errors as `String`, losing source chain
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:63-65, src/scout.rs:94
- **Description:** Both `SuggestError::Embedding(String)` and `ScoutError::Embedder(String)` convert the original embedding error to a string via `.to_string()`. This loses the `source()` chain and prevents callers from downcasting or chaining error context.
- **Suggested fix:** Store `Box<dyn std::error::Error + Send + Sync>` instead of `String`, or use `thiserror` derive with `#[from]` to preserve the chain.

#### EH-4: `suggest_placement` silently returns empty patterns on `get_chunks_by_origin` failure
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:107-109
- **Description:** `store.get_chunks_by_origin(...).unwrap_or_default()` silently discards store errors. If the DB query fails (corruption, locked), the function produces a suggestion with empty patterns instead of surfacing the error. The function already returns `Result<_, SuggestError>` and has `From<StoreError>` — errors can propagate.
- **Suggested fix:** Use `?` to propagate the `StoreError` via the existing `From` impl.

#### EH-5: `resolve_to_related` silently drops store errors via `.ok()?`
- **Difficulty:** easy
- **Location:** src/related.rs:69
- **Description:** `store.get_chunks_by_name(name).ok()?` treats store errors the same as "name not found" — both map to `None` and the function silently omits the entry. A transient DB error (lock timeout, I/O failure) silently reduces the result set with no warning or trace.
- **Suggested fix:** Log a `tracing::warn!` when the error is not "not found", or restructure `resolve_to_related` to return `Result` and propagate DB-level errors.

#### EH-6: `map_hunks_to_functions` silently continues on store error
- **Difficulty:** easy
- **Location:** src/impact.rs:431-434
- **Description:** `store.get_chunks_by_origin(file)` error is caught with `Err(_) => continue`, silently skipping the entire file. If the index is corrupted or locked, all functions in that file are silently omitted from the impact analysis.
- **Suggested fix:** At minimum add `tracing::warn!(file = %file, error = %e, "Failed to get chunks for diff impact")`. Better: change return type to `Result` and propagate.

#### EH-7: `suggest_tests` returns empty on call-graph/test-chunk load failure with no logging
- **Difficulty:** easy
- **Location:** src/impact.rs:611-618
- **Description:** Two consecutive `match ... Err(_) => return Vec::new()` silently swallow errors from `store.get_call_graph()` and `store.find_test_chunks()`. The caller (`cmd_impact --suggest-tests`) receives an empty suggestion list with no indication that the feature is broken vs. genuinely no suggestions. (Overlaps with OB-9.)
- **Suggested fix:** Change return type to `Result<Vec<TestSuggestion>>` or add `tracing::warn!` before returning empty.

#### EH-8: `scout` swallows batch caller count and staleness errors via `unwrap_or_default`
- **Difficulty:** easy
- **Location:** src/scout.rs:134-140
- **Description:** `store.get_caller_counts_batch(&all_names).unwrap_or_default()` and `store.check_origins_stale(&origins).unwrap_or_default()` silently discard errors. If the DB is locked or queries fail, caller counts show as 0 and all files appear non-stale — indistinguishable from genuinely zero-caller code. (Overlaps with OB-10.)
- **Suggested fix:** Log `tracing::warn!` on error, or propagate since `scout()` already returns `Result`.

#### EH-9: `find_relevant_notes` silently returns empty on error
- **Difficulty:** easy
- **Location:** src/scout.rs:243-246
- **Description:** `store.list_notes_summaries()` error is caught with `Err(_) => return Vec::new()`. Notes silently disappear from scout results if the notes table query fails.
- **Suggested fix:** Log the error with `tracing::warn!` before returning empty.

#### EH-10: `analyze_diff_impact` silently skips callers on per-function error
- **Difficulty:** easy
- **Location:** src/impact.rs:487
- **Description:** `if let Ok(callers_ctx) = store.get_callers_with_context(&func.name)` silently drops the error for individual functions. If the DB query fails for one function, its callers are omitted from results. The summary `caller_count` undercounts with no warning.
- **Suggested fix:** Log the error. Consider collecting partial errors and exposing them in the result struct.

#### EH-11: `cmd_stats` swallows multiple store errors with default values
- **Difficulty:** easy
- **Location:** src/cli/commands/stats.rs:30-35
- **Description:** `store.count_stale_files().unwrap_or((0, 0))`, `store.note_count().unwrap_or(0)`, and `store.function_call_stats().unwrap_or_default()` all silently produce zeros on error. Stats showing "0 notes, 0 calls" when the real issue is a broken query is misleading.
- **Suggested fix:** Propagate with `?` (function returns `Result`) or log a warning when defaulting.

#### EH-12: `cmd_context` compact mode swallows caller/callee count errors
- **Difficulty:** easy
- **Location:** src/cli/commands/context.rs:49-50
- **Description:** `store.get_caller_counts_batch(&names).unwrap_or_default()` and `get_callee_counts_batch` silently return empty maps on error. Every chunk then shows "0 callers, 0 callees" — indistinguishable from genuinely unconnected code.
- **Suggested fix:** Propagate with `?` or log a warning. Function returns `Result`.

#### EH-13: `get_chunks_by_ids` error swallowed in query parent resolution
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:329
- **Description:** `store.get_chunks_by_ids(&id_refs).unwrap_or_default()` silently discards the store error. Parent context for windowed chunks is silently lost if the DB query fails.
- **Suggested fix:** Log `tracing::warn!` on error, or propagate.

#### EH-14: `node_letter` can overflow on large impact results
- **Difficulty:** easy
- **Location:** src/impact.rs:730-735
- **Description:** `node_letter(i)` computes `(b'A' + (i % 26) as u8) as char`. The expression `i / 26` is formatted as a suffix, but if `i` is very large the division result could be confusing. More importantly, the `i as u8` cast is implicit in the `u8` arithmetic. While unlikely to reach 6656+ callers+tests, library code should be safe.
- **Suggested fix:** Use `format!("N{i}")` for `i >= 26` (simpler), or cap diagram output at a reasonable limit (e.g., 100 nodes).

#### EH-15: `suggest_tests` file chunk lookup silently chains `.ok().unwrap_or_default()`
- **Difficulty:** easy
- **Location:** src/impact.rs:634-637
- **Description:** `store.get_chunks_by_origin(&caller.file.to_string_lossy()).ok().unwrap_or_default()` silently discards store errors when fetching file chunks for inline-test detection. If the query fails, the function assumes no inline tests exist and suggests a new test file. This isn't a correctness bug (wrong file is annoying but not dangerous), but the silent error swallowing matches the broader pattern flagged here.
- **Suggested fix:** Add `tracing::debug!` on the error path.

## Code Quality

#### CQ-1: CLI command boilerplate — store-opening ceremony repeated 17+ times
- **Difficulty:** medium
- **Location:** src/cli/commands/*.rs (17 files)
- **Description:** 17 CLI command functions repeat the exact same 4-line pattern:
  ```rust
  let root = find_project_root();
  let cqs_dir = cqs::resolve_index_dir(&root);
  let index_path = cqs_dir.join("index.db");
  if !index_path.exists() { bail!("Index not found..."); }
  ```
  Followed by `Store::open(&index_path)?` (25 occurrences across 23 files). Some variants use `anyhow::bail!`, others `bail!` (pre-imported). The root + index path discovery is identical in all cases. This grew from ~10 in v0.9.7 to 17+ as new commands were added (scout, where, related, impact-diff, stale).
- **Suggested fix:** Add a helper `fn require_index() -> Result<(PathBuf, Store)>` to `cli/mod.rs` that does root discovery, path construction, existence check, and `Store::open`. Returns `(root, store)`. Cuts ~6 lines per command file. For commands also needing an `Embedder`, add `fn require_index_with_embedder() -> Result<(PathBuf, Store, Embedder)>`.

#### CQ-2: Path relativization duplicated 30+ times across CLI commands
- **Difficulty:** medium
- **Location:** src/cli/commands/*.rs (30+ occurrences in 15+ files)
- **Description:** The pattern `.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/")` is inlined 30+ times across CLI command files. `impact.rs` has a private `rel_path()` helper that does exactly this, but it's scoped to that module. Each CLI command re-implements the same 4-expression chain with minor variations (some use `&root`, some `root`, some add `.to_string()`).
- **Suggested fix:** Extract `impact::rel_path` to a `pub(crate) fn rel_path(path: &Path, root: &Path) -> String` in `cli/display.rs` or a shared module. Replace all 30+ inline occurrences. The function is 5 lines; each replacement saves 3-4 lines and eliminates variation.

#### CQ-3: `related.rs` JSON construction triplicates identical map closure
- **Difficulty:** easy
- **Location:** src/cli/commands/related.rs:28-81
- **Description:** Three identical JSON construction blocks for `shared_callers`, `shared_callees`, and `shared_types` — each does the exact same `strip_prefix + json!({name, file, line, overlap_count})` map on a `RelatedFunction`. The only difference is the source collection. Similarly, the text display section (lines 94-149) has three identical formatting blocks.
- **Suggested fix:** Extract a closure:
  ```rust
  let to_json = |items: &[RelatedFunction]| -> Vec<serde_json::Value> {
      items.iter().map(|r| {
          let rel = rel_path(&r.file, &root);
          json!({"name": r.name, "file": rel, "line": r.line, "overlap_count": r.overlap_count})
      }).collect()
  };
  ```

#### CQ-4: Config validation repeats clamp-and-warn pattern 4 times
- **Difficulty:** easy
- **Location:** src/config.rs:135-174
- **Description:** `Config::load()` has 4 nearly identical blocks for clamping `threshold`, `name_boost`, `note_weight` (all `Option<f32>`, range 0.0-1.0) and `limit` (`Option<usize>`, range 1-100). Each: check `Some`, check bounds, `tracing::warn!` with field name, `.clamp()`. Will need updating each time a new config field is added.
- **Suggested fix:** Extract a `fn clamp_f32_field(field: &mut Option<f32>, name: &str, min: f32, max: f32)` helper. Four blocks become four one-liners.

#### CQ-5: Atomic config write pattern duplicated between `add_reference_to_config` and `remove_reference_from_config`
- **Difficulty:** easy
- **Location:** src/config.rs:336-350, src/config.rs:401-422
- **Description:** Both functions implement an identical ~15-line pattern: write temp file, rename (with copy fallback on failure), remove temp on error, set Unix permissions to 0o600. The lock acquisition, temp-file creation, rename-with-fallback, and permission setting code is copy-pasted. Introduced in the v0.9.7 audit fix for DS4 (config read-modify-write race).
- **Suggested fix:** Extract `fn atomic_write_config(config_path: &Path, content: &str) -> anyhow::Result<()>` that handles the temp + rename + fallback + permissions. Both callers reduce from ~15 lines to 1.

#### CQ-6: `impact_diff.rs` duplicates "no changes" JSON response block
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:36-49, src/cli/commands/impact_diff.rs:56-71
- **Description:** Two identical JSON response blocks — both emit `{"changed_functions": [], "callers": [], "tests": [], "summary": {"changed_count": 0, ...}}`. One fires when `hunks.is_empty()`, the other when `changed.is_empty()`. Same "nothing affected" state, duplicated construction.
- **Suggested fix:** Merge the two early-return conditions (`hunks.is_empty() || changed.is_empty()`) into one, or extract an `empty_impact_response(json: bool)` helper.

#### CQ-7: `cqs dead` has ~86% false-positive rate on real source
- **Difficulty:** medium
- **Location:** (tool output, not a source file)
- **Description:** `cqs dead` reports 130 dead functions. Only 18 are in `src/` proper, and ALL of those are false positives: 9 `extract_return` functions called via function pointer (`LanguageDef.extract_return_nl`), `panic_message` called inside `.map_err()` closure, `default_ref_weight` used via `#[serde(default)]`, `detect_provider` called via `get_or_init()`, `assert_send`/`assert_sync` (compile-time trait assertions), `source_type` (trait impl), and test helpers in `#[cfg(test)]` modules. The remaining 112 are test fixture files (eval_go.go, eval_python.py, etc.) which naturally have no callers.
  This means `cqs dead` has zero true positives in the current codebase, limiting its value as an audit tool.
- **Suggested fix:** (1) Exclude files matching common test fixture patterns from `dead` output by default. (2) Consider detecting function-pointer usage (`fn name` in struct field initializers), serde attribute references, and trait impl methods. (3) Add a `--src-only` flag or respect `.gitignore`-style exclusions.

#### CQ-8: `ScoutError` and `SuggestError` are near-identical error types (code duplication)
- **Difficulty:** easy
- **Location:** src/scout.rs:340-352, src/where_to_add.rs:325-345
- **Description:** Both error types have the same structure: `Store(StoreError)` + embedder-string variant. Different variant names (`Embedder` vs `Embedding`), inconsistent `std::error::Error` impl (SuggestError has it, ScoutError doesn't). API design implications covered in AD-2 and AD-9; this finding is specifically about the code duplication — two copies of the same enum + Display impl + From impl.
- **Suggested fix:** Either unify into a shared `AnalysisError` or (simpler) have both modules return `anyhow::Result`, since they're only called from CLI code that already uses anyhow.

## Documentation

#### DOC-1: lib.rs Quick Start calls non-existent `store.search()` method
- **Difficulty:** easy
- **Location:** src/lib.rs:33
- **Description:** The Quick Start doc example calls `store.search(&query_embedding, 5, 0.3)?` but `Store` has no `search()` method. The actual search API is `Store::search_filtered()` (in `src/search.rs:320`) which takes a `SearchFilter` struct, or `Store::search_fts()` / `Store::search_by_name()`. This means the crate-level doc example won't compile (it's `no_run`, so it doesn't fail CI). Users copying this example will get a compile error immediately. The v0.9.7 audit found D6 (unused `ModelInfo` import) in the same Quick Start — that was fixed, but `store.search()` was broken before that audit and was missed.
- **Suggested fix:** Replace with a working example using `search_filtered`:
  ```rust
  use cqs::{SearchFilter, Store};
  let filter = SearchFilter::new(5, 0.3);
  let results = store.search_filtered(&query_embedding, &filter)?;
  ```
  Or mark the entire Quick Start as conceptual pseudo-code with a note. Better yet, make it a real `#[doc(test)]` that compiles.

#### DOC-2: ROADMAP version says "Current: v0.12.0" but actual version is 0.12.1
- **Difficulty:** easy
- **Location:** ROADMAP.md:3
- **Description:** The heading says `## Current: v0.12.0` but `Cargo.toml` and `cqs --version` report 0.12.1. The v0.12.1 release was cut but the ROADMAP version was not updated.
- **Suggested fix:** Change to `## Current: v0.12.1`.

#### DOC-3: ROADMAP lists two completed items in "Next" section
- **Difficulty:** easy
- **Location:** ROADMAP.md:23-24
- **Description:** Two items in the "Next" checklist were completed in v0.12.1:
  - `- [ ] Delete type_map dead code from LanguageDef` — done per CHANGELOG v0.12.1 "Removed" section
  - `- [ ] Scout note matching precision (suffix matching too loose)` — done per CHANGELOG v0.12.1 "Fixed" section
  These should be checked off or moved to "Recently Completed".
- **Suggested fix:** Move both to "Recently Completed" with `[x]` or remove from "Next".

#### DOC-4: CHANGELOG missing comparison URL links for 11 versions
- **Difficulty:** easy
- **Location:** CHANGELOG.md:822 (bottom of file)
- **Description:** Versions 0.9.4 through 0.12.1 all have `## [X.Y.Z]` section headers but no corresponding `[X.Y.Z]: https://github.com/...` link reference at the bottom of the file. The `[Unreleased]` link is also missing. Per Keep a Changelog format, these should all have comparison URLs. Versions 0.9.3 and below all have correct links. Missing: `[Unreleased]`, `[0.12.1]`, `[0.12.0]`, `[0.11.0]`, `[0.10.2]`, `[0.10.1]`, `[0.10.0]`, `[0.9.9]`, `[0.9.8]`, `[0.9.7]`, `[0.9.6]`, `[0.9.5]`, `[0.9.4]`.
- **Suggested fix:** Add the missing link references at the bottom of CHANGELOG.md:
  ```
  [Unreleased]: https://github.com/jamie8johnson/cqs/compare/v0.12.1...HEAD
  [0.12.1]: https://github.com/jamie8johnson/cqs/compare/v0.12.0...v0.12.1
  [0.12.0]: https://github.com/jamie8johnson/cqs/compare/v0.11.0...v0.12.0
  ...etc
  ```

#### DOC-5: README search example uses deleted function name `serve_http`
- **Difficulty:** easy
- **Location:** README.md:83
- **Description:** The Filters section shows `cqs --name-boost 0.8 "serve_http"` as an example of name-heavy search. `serve_http` was part of the MCP server removed in v0.10.0 and no longer exists in the codebase. Searching for it returns zero results, making this a misleading example. The comment says "Name-heavy for known identifiers" but demonstrates a query that fails.
- **Suggested fix:** Use a function that actually exists, e.g. `cqs --name-boost 0.8 "search_filtered"`.

#### DOC-6: README omits `--no-stale-check` flag (new in v0.12.1)
- **Difficulty:** easy
- **Location:** README.md (Filters section, around line 93)
- **Description:** v0.12.1 added `--no-stale-check` for suppressing per-file staleness checks on slow filesystems (NFS, network mounts). It's also configurable via `stale_check = false` in `.cqs.toml`. The CHANGELOG documents it but the README does not mention it anywhere — not in the Filters section, not in the Configuration example, and not in the Watch Mode section where slow filesystems are relevant.
- **Suggested fix:** Add to the Configuration `.cqs.toml` example:
  ```toml
  # Disable staleness checks (for slow filesystems like NFS)
  stale_check = false
  ```
  And optionally mention `--no-stale-check` in the search flags section.

#### DOC-7: README omits `--summary` flag on `cqs context`
- **Difficulty:** easy
- **Location:** README.md:193-194
- **Description:** The README shows `cqs context src/search.rs --compact` but does not mention the `--summary` flag, which returns summary counts instead of full details. `cqs context --help` shows both `--summary` and `--compact` as separate options. The CHANGELOG v0.9.3 documents `--summary` for the MCP tool but the CLI flag was added alongside `--compact` in v0.12.0 and is undocumented.
- **Suggested fix:** Add `cqs context src/search.rs --summary  # counts only, no details` to the Code Intelligence section.

#### DOC-8: README omits `--format mermaid` output option for trace and impact
- **Difficulty:** easy
- **Location:** README.md:179-190
- **Description:** Both `cqs trace` and `cqs impact` support `--format mermaid` for generating Mermaid diagrams. `cqs trace --format mermaid` was added in v0.9.0 and `cqs impact --format mermaid` was added alongside. Neither is mentioned in the README. Mermaid output is useful for documentation and PRs.
- **Suggested fix:** Add examples:
  ```bash
  cqs trace cmd_query search_filtered --format mermaid  # Mermaid diagram
  cqs impact search_filtered --format mermaid           # Mermaid dependency graph
  ```

#### DOC-9: README omits `--expand` flag on main search command
- **Difficulty:** easy
- **Location:** README.md (Filters section)
- **Description:** The main `cqs` search command has `--expand` for small-to-big retrieval (expanding results with parent context), added for Markdown table-aware chunking in v0.11.0. It's visible in `cqs --help` but not mentioned in the README at all. Only `gather --expand` (different flag, different meaning) is shown.
- **Suggested fix:** Add to the Filters section:
  ```bash
  # Expand results with parent context (small-to-big retrieval)
  cqs --expand "database schema"
  ```

#### DOC-10: SECURITY.md omits `~/.config/cqs/config.toml` write path
- **Difficulty:** easy
- **Location:** SECURITY.md:55-66
- **Description:** The "Write Access" table lists `.cqs/`, `.cqs/index.db`, `.cqs/index.hnsw.*`, `.cqs/cqs.pid`, `docs/notes.toml`, and `~/.local/share/cqs/refs/*/`. Missing: `~/.config/cqs/config.toml` which is written by `cqs ref add` (adds `[[reference]]` sections) and `cqs ref remove` (removes them). Also `~/.config/cqs/projects.toml` is written by `cqs project register` and `cqs project remove`. Both are in the Read table but not the Write table.
- **Suggested fix:** Add to Write Access:
  | `~/.config/cqs/config.toml` | User config (reference entries) | `cqs ref add`, `cqs ref remove` |
  | `~/.config/cqs/projects.toml` | Project registry | `cqs project register`, `cqs project remove` |

#### DOC-11: SECURITY.md Read table lists `~/.local/share/cqs/refs/*/` as "read-only during search" but it's also used for writes
- **Difficulty:** easy
- **Location:** SECURITY.md:55
- **Description:** The Read Access table has `~/.local/share/cqs/refs/*/` with notes "Reference indexes (read-only during search)". But this path also appears in the Write Access table (line 66) for `cqs ref add` / `cqs ref update`. The "(read-only during search)" qualifier in the Read table is correct but the note is confusing since the same path is writable in other contexts. Not a bug, just unclear.
- **Suggested fix:** Change the Read table note from "Reference indexes (read-only during search)" to "Reference indexes" (the write context is already documented below).

## Platform Behavior

#### PB-1: `diff_parse.rs` does not handle CRLF line endings in git diff output
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:35
- **Description:** `parse_unified_diff` uses `input.lines()` to iterate the diff. While `str::lines()` splits on both LF and CRLF, the `+++ b/path` extraction at line 38 strips only the prefix — if the line has a trailing `\r` (from git on Windows or piped from a Windows process), the extracted path will contain a trailing `\r`. For example, `+++ b/src/main.rs\r` produces `file = "src/main.rs\r"`, which will never match any indexed origin. The hunk regex at line 31 would also fail to match `@@ ... @@\r` since the pattern doesn't account for trailing `\r`. On WSL, `git diff` output is LF by default, but `cqs impact-diff --stdin` can receive CRLF-encoded input from Windows processes or piped files. All test cases use LF-only strings.
- **Suggested fix:** Add `let input = input.replace("\r\n", "\n");` at the top of `parse_unified_diff` before processing, consistent with the CRLF normalization in `src/parser/mod.rs:134` and `src/source/filesystem.rs:117`.

#### PB-2: `map_hunks_to_functions` path mismatch between diff paths and index origins
- **Difficulty:** medium
- **Location:** src/impact.rs:426-431
- **Description:** `map_hunks_to_functions` uses `hunk.file` (from `parse_unified_diff`) as the key to look up `store.get_chunks_by_origin(file)`. The diff parser extracts paths like `src/main.rs` from `+++ b/src/main.rs`. The index stores origins via `normalize_origin()` which does `path.to_string_lossy().replace('\\', "/")`. On WSL/Linux, both are forward-slash and match. But `get_chunks_by_origin` does a raw `WHERE origin = ?1` query — it does NOT call `normalize_origin` on the input string. If the index was built on Windows (backslash origins), the diff's forward-slash paths won't match. Additionally, if the git diff was produced from a different working directory depth, paths may be relative to a different root (e.g., running `git diff` from a subdirectory). No normalization or root-relative conversion is applied to the diff paths before lookup.
- **Suggested fix:** Apply `normalize_origin`-equivalent normalization (forward-slash conversion) in `get_chunks_by_origin` to the input string, or normalize at the call site. Consider also validating that diff paths are relative to project root.

#### PB-3: `reference.rs` uses `std::canonicalize` instead of `dunce::canonicalize`
- **Difficulty:** easy
- **Location:** src/cli/commands/reference.rs:79
- **Description:** `source.canonicalize()` uses `std::fs::canonicalize` which on Windows returns UNC paths (`\\?\C:\...`). The rest of the codebase standardized on `dunce::canonicalize` (per prior audit PB4 fix), but this location was missed. The canonicalized path is stored in the config file via `add_reference_to_config`, so on Windows the stored source path would be a UNC path. This doesn't cause failures on WSL (where `canonicalize()` returns normal Unix paths), but would break on native Windows.
- **Suggested fix:** Replace `.canonicalize()` with `dunce::canonicalize(&source)` (already a dependency).

#### PB-4: `project.rs` register uses `std::canonicalize` instead of `dunce::canonicalize`
- **Difficulty:** easy
- **Location:** src/cli/commands/project.rs:54
- **Description:** `abs_path.canonicalize().unwrap_or_else(|_| abs_path.clone())` in the project register command uses `std::canonicalize`. Same issue as PB-3: on Windows this produces UNC paths stored in `projects.toml`. Other project root detection code (`find_project_root` in `cli/config.rs`) correctly uses `dunce::canonicalize`.
- **Suggested fix:** Replace with `dunce::canonicalize(&abs_path).unwrap_or_else(|_| abs_path.clone())`.

#### PB-5: `lib.rs` `enumerate_files` uses `std::canonicalize` with manual UNC stripping instead of `dunce`
- **Difficulty:** easy
- **Location:** src/lib.rs:248, src/lib.rs:282
- **Description:** `enumerate_files` calls `root.canonicalize()` (line 248) then wraps it with `strip_unc_prefix()`, and also calls `e.path().canonicalize()` (line 282) in the file walker. The `strip_unc_prefix` function is a manual UNC stripping that handles the `\\?\` prefix. This predates the `dunce` dependency. While functionally equivalent for the basic case, it's inconsistent with the rest of the codebase which uses `dunce::canonicalize`. The `strip_unc_prefix` path also doesn't handle other UNC variants that `dunce` handles (like `\\?\UNC\` network paths or verbatim paths).
- **Suggested fix:** Replace `strip_unc_prefix(root.canonicalize()?)` with `dunce::canonicalize(&root)?` and `e.path().canonicalize()` with `dunce::canonicalize(e.path())`. Then `strip_unc_prefix` can potentially be removed if no other callers need it.

#### PB-6: `check_origins_stale` treats relative origin paths as relative to CWD, not project root
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:558
- **Description:** `check_origins_stale` constructs `PathBuf::from(&origin)` and calls `.metadata()` on it. Origins are stored as relative paths (e.g., `src/main.rs`). `metadata()` on a relative path resolves relative to the current working directory. If the user runs `cqs search` from a subdirectory (e.g., `cd src && cqs "query"`), the staleness check constructs `PathBuf::from("src/main.rs")` and looks for `./src/main.rs` relative to `src/`, i.e., `src/src/main.rs` — which doesn't exist. The `metadata()` call returns `Err`, `current_mtime` is `None`, and the file is NOT marked stale (it falls through without inserting into `stale`). So stale files from subdirectories are silently missed, while fresh files are not falsely flagged. The `Store` doesn't know about the project root — callers would need to pass it in or resolve paths before checking.
- **Suggested fix:** Either: (a) pass project root to `check_origins_stale` and resolve `root.join(&origin)` before calling `.metadata()`, or (b) document that callers must ensure CWD is project root (which `find_project_root()` does not guarantee — it returns the root but doesn't chdir to it).

#### PB-7: `check_origins_stale` does not report deleted files as stale
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:558-570
- **Description:** When a file is deleted after indexing, `PathBuf::from(&origin).metadata()` returns `Err`. The code maps this to `current_mtime = None` and then does nothing (no insert into `stale`). So a deleted file is never reported as stale. This also manifests on NFS or network filesystems where `metadata()` may fail transiently for existing files (timeouts, stale handles), causing those files to also be silently skipped. Deleted files are the most clearly stale — they were indexed but no longer exist.
- **Suggested fix:** Distinguish between "file not found" (definitely stale) and other metadata errors (skip as now). Use explicit error matching: `Err(e) if e.kind() == ErrorKind::NotFound => { stale.insert(origin); continue; }`.

#### PB-8: `note_mention_matches_file` in scout.rs only checks `/` boundary, not `\`
- **Difficulty:** easy
- **Location:** src/scout.rs:264-268
- **Description:** `note_mention_matches_file` checks if the byte before the mention suffix is `b'/'` for path-component boundary detection. The result files passed to this function are already normalized (line 199: `.replace('\\', "/")`), so the `file` parameter always uses forward slashes. However, `mention` values come from user-authored `notes.toml` and could contain backslashes on Windows (e.g., `src\search.rs`). In that case, `"src/search.rs".ends_with("src\\search.rs")` returns false, silently missing the note match.
- **Suggested fix:** Normalize `mention` by replacing `\` with `/` before matching: `let mention = mention.replace('\\', "/");`

#### PB-9: `run_git_diff` doesn't pass `--no-pager` or `--no-color`
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:95-112
- **Description:** `run_git_diff` spawns `git diff` as a subprocess. Two issues: (1) No `--no-pager` flag is passed — if the user has `core.pager` or `GIT_PAGER` configured, `git diff` may invoke a pager and block waiting for terminal interaction. Since the output is captured via `cmd.output()`, this typically works (git detects non-TTY stdout), but some pager configurations override this. (2) No `--no-color` flag — if the user has `color.diff = always`, the output contains ANSI escape codes that will corrupt path extraction and hunk matching. Both `--no-pager` and `--no-color` are defensive flags that git commands invoked programmatically should always include.
- **Suggested fix:** Change to `cmd.args(["--no-pager", "diff", "--no-color"])` instead of just `cmd.arg("diff")`.

#### PB-10: `suggest_test_file` constructs paths with hardcoded forward slashes
- **Difficulty:** easy
- **Location:** src/impact.rs:698-718
- **Description:** `suggest_test_file` uses `Path::new(source)` to decompose the source path, then constructs the suggested test file with `format!("{parent}/tests/{stem}_test.rs")`. This always uses `/` separators regardless of OS. Currently safe because the input `source` is pre-normalized to forward slashes (line 677), but `Path::parent()` on Windows returns backslash-separated paths. If anyone calls `suggest_test_file` with a non-normalized Windows path, `parent` would contain `\` and the output would have mixed separators. The function is not `pub` so the risk is low, but it's a latent issue.
- **Suggested fix:** Either document that the input must be forward-slash normalized, or use `to_string_lossy().replace('\\', "/")` on the `parent` result for consistency.

## Algorithm Correctness

#### AC-1: `map_hunks_to_functions` uses inconsistent interval semantics (half-open hunk vs inclusive chunk)
- **Difficulty:** medium
- **Location:** src/impact.rs:436-449
- **Description:** The overlap test on line 439 is `hunk.start <= chunk.line_end && hunk_end > chunk.line_start` where `hunk_end = hunk.start + hunk.count` (exclusive). The chunk range is `[line_start, line_end]` (inclusive on both ends — see `ChunkSummary.line_end` doc comment "Ending line number (1-indexed)"). This overlap test is correct for a half-open `[start, start+count)` hunk vs inclusive `[line_start, line_end]` chunk. However, the git unified diff hunk header `@@ +start,count @@` has a special case: when `count=0`, the hunk represents a pure deletion at that position (no new-side lines). In this case `hunk_end = start + 0 = start`, so the condition becomes `start <= chunk.line_end && start > chunk.line_start`. This means a zero-count hunk (pure deletion) at a line inside a function body will still match that function, but a deletion at line_start itself will NOT match (because `start > line_start` is false when equal). A deletion between two functions would correctly match nothing. The real issue: `parse_unified_diff` defaults `count` to 1 when absent, but `@@ -N,0 +M,0 @@` (count explicitly 0) is valid git output for context-only hunks. In this case, `hunk_end == hunk.start` and the overlap test becomes a single-point check rather than a range check, which is subtly different from the documented "half-open" semantics.
- **Suggested fix:** Add a guard: `if hunk.count == 0 { continue; }` — a zero-count hunk on the new side means no new lines were added (pure deletion), so no new-side functions are affected. Alternatively, document the semantics clearly.

#### AC-2: `extract_call_snippet` offset calculation can produce wrong snippet when chunk has windowed children
- **Difficulty:** medium
- **Location:** src/impact.rs:151-169
- **Description:** `extract_call_snippet` computes `offset = caller.call_line.saturating_sub(r.chunk.line_start)` and then indexes into the chunk's content lines. This assumes `r.chunk.line_start` corresponds to line 0 of the content. However, `search_by_name` returns the first match which could be a windowed child chunk (with a parent_id). Windowed chunks have `line_start` set to the parent's start line but their `content` is a subset (window) of the parent content. So `call_line - line_start` doesn't correctly index into the windowed content. Additionally, `call_line` is the line number as stored in the call graph (absolute), while the content is relative. If the call site is in lines 50-55 of a function starting at line 10, and the chunk is windowed to show lines 30-60 of the parent, the offset computation will be wrong because `line_start` is the parent's start (line 10), not the window's start.
- **Suggested fix:** Filter the `search_by_name` result to prefer chunks with `parent_id.is_none()` (full chunks, not windows). Or adjust the offset computation to use the actual start line of the window. Given this is for display only (snippet extraction), the worst case is showing the wrong lines, not a crash.

#### AC-3: `DiffTestInfo.via` only records the first changed function that reaches a test, not the closest
- **Difficulty:** easy
- **Location:** src/impact.rs:502-516
- **Description:** In `analyze_diff_impact`, the `seen_tests` HashSet prevents duplicate test entries, which is correct. However, the `via` field (which changed function leads to this test) is set to whichever changed function is processed first. The iteration order over `changed` is the order from `map_hunks_to_functions`, which is file-order + dedup-order, not relevance order. If function A reaches test T at depth 4 and function B reaches test T at depth 1, whichever is iterated first wins. The user sees "via: A, depth: 4" instead of "via: B, depth: 1". The depth shown corresponds to whichever function won the insertion race, not the shortest path.
- **Suggested fix:** When a test is already in `seen_tests`, check if the new path has a shorter depth. If so, update the existing entry. Or change the dedup strategy to prefer the shortest depth path:
  ```rust
  // Collect all test paths first, then dedup by shortest depth
  if depth > 0 {
      let entry = test_map.entry(test.name.clone()).or_insert_with(|| (..., depth));
      if depth < entry.depth { *entry = (..., depth); }
  }
  ```

#### AC-4: `search_chunks_by_signature` LIKE pattern is unescaped — SQL wildcards in type names cause false matches
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:805, src/related.rs:99
- **Description:** `search_chunks_by_signature` builds a `LIKE` pattern with `format!("%{}%", type_name)`. If `type_name` contains SQL LIKE wildcards (`%` or `_`), they are interpreted as wildcards, not literals. The type name comes from `extract_type_names` which extracts PascalCase identifiers from signatures. While most type names won't contain `%` or `_`, Rust types like `HashMap` fed through a signature like `fn foo(map: HashMap<String, Vec_Custom>)` would extract `Vec_Custom`, and the underscore would match any single character. More practically, if any parser produces type names with underscores (e.g., Go's `error_handler`), the LIKE query matches more broadly than intended.
- **Suggested fix:** Escape LIKE wildcards in the pattern: `type_name.replace('%', "\\%").replace('_', "\\_")` and add `ESCAPE '\\'` to the SQL query. Or use an exact match approach since the goal is to find exact type names in signatures.

#### AC-5: `related.rs::find_type_overlap` filters on string-ified `ChunkType` instead of enum comparison
- **Difficulty:** easy
- **Location:** src/related.rs:104-105
- **Description:** The filter `chunk.chunk_type.to_string() != "function" && chunk.chunk_type.to_string() != "method"` calls `to_string()` twice per chunk and relies on the Display impl matching the string literals. If `ChunkType::to_string()` ever changes format (e.g., capitalized), this filter silently breaks. The `ChunkType` enum supports direct comparison. This was already noted in AD-6 but from an API design perspective; this is the algorithm correctness angle — the comparison could produce wrong results if Display output changes.
- **Suggested fix:** Replace with `!matches!(chunk.chunk_type, ChunkType::Function | ChunkType::Method)`.

#### AC-6: `BoundedScoreHeap::push` uses `>=` for replacement, biasing toward later entries at equal scores
- **Difficulty:** easy
- **Location:** src/search.rs:289
- **Description:** When at capacity, the heap replaces the minimum entry if `score >= *min_score`. The `>=` means that for equal scores, later entries always replace earlier ones. This is intentional to avoid the prior audit's AC8 (iteration-order bias toward HashMap first-seen). However, it introduces the opposite bias: last-seen wins for equal scores. The iteration order is database rowid order (ascending), so higher-rowid chunks are favored at equal scores. For brute-force search across thousands of equally-scoring chunks, the result set contains the last N chunks by rowid rather than a representative sample. This was previously noted in the v0.9.7 audit as AC8/P3-45 with status "PR #340", so it may already be a conscious design choice.
- **Suggested fix:** This is a documented trade-off. If deterministic-but-arbitrary selection is preferred over last-wins, use `>` instead of `>=`. Both have bias; the question is which is more useful. Document the choice in the heap's doc comment.

#### AC-7: `node_letter` generates ambiguous labels for indices 26-51
- **Difficulty:** easy
- **Location:** src/impact.rs:730-735
- **Description:** `node_letter(26)` produces `"A1"`, `node_letter(27)` produces `"B1"`, ..., `node_letter(51)` produces `"Z1"`, `node_letter(52)` produces `"A2"`. But `node_letter(0)` produces `"A"`. The problem is that single-letter labels `A-Z` and suffixed labels `A1-Z1` coexist in the same Mermaid diagram. If there are >26 callers+tests, the diagram has both node `A` (the target function on line 343-346) and potentially node `A1` (26th caller/test). Mermaid treats `A` and `A1` as different nodes, so there's no collision at the node ID level. But the labeling is confusing: `A` is the target, `A1` is the 27th result. The real bug is that `idx` starts at 1 (line 348), so `node_letter(1)` = `"B"`, meaning letter A is reserved for the target. But `node_letter(26)` = `"A1"` which could be confused with "node A, version 1".
- **Suggested fix:** Use a different naming scheme for overflow: `node_letter(26+)` could use `"AA"`, `"AB"`, etc., or just `"N27"`, `"N28"` for clarity.

#### AC-8: `compute_hints_with_graph` double-counts when prefetched caller count doesn't match graph data
- **Difficulty:** easy
- **Location:** src/impact.rs:86-104
- **Description:** `compute_hints_with_graph` accepts `prefetched_caller_count: Option<usize>` from the caller. When `Some(n)`, it uses `n` directly. When `None`, it looks up callers from `graph.reverse`. The problem: `prefetched_caller_count` comes from `store.get_caller_counts_batch()` (SQL query on `function_calls` table), while `graph.reverse` is built from the same table but loaded separately. These should always agree, BUT `get_caller_counts_batch` counts distinct caller names from SQL while `graph.reverse` has a `Vec<String>` per callee that could have duplicates if the call graph has duplicate edges (same caller calling same callee from multiple sites). In practice this means `prefetched_caller_count` could be lower than `graph.reverse.get(name).len()` due to deduplication differences, or they could diverge if the call graph was loaded at a different time than the batch query (no transaction coordination). The impact is minor (caller count off by a few) but visible in the output.
- **Suggested fix:** Either always use the graph data (remove the prefetched path), or ensure `get_caller_counts_batch` and `get_call_graph` use consistent deduplication. The prefetch exists for performance (avoid loading the full graph), so the best fix is to document that the count is approximate when prefetched.

#### AC-9: `parse_unified_diff` doesn't handle the `\ No newline at end of file` marker
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:35
- **Description:** Git diff output can include the line `\ No newline at end of file` after a hunk's content lines. This line starts with `\`, not `+` or `-` or `@`. The current parser correctly ignores it (no branch matches), so it doesn't cause errors. However, if a file path starts with `\ ` (theoretically possible on some systems), the parser would skip it. More importantly, the parser doesn't handle the `diff --git` line itself as a file boundary reset — it only looks for `+++ `. If a diff contains a `+++ ` line inside a file's content (unlikely but possible in generated code), it would be misinterpreted as a file header. This is minor and not a bug in practice, just a robustness consideration.
- **Suggested fix:** Reset `current_file` when encountering `diff --git` lines, not just `+++ ` lines. This provides an additional boundary signal. The `\ No newline at end of file` handling is fine as-is.

#### AC-10: `gather` BFS decay computation uses `depth + 1` but depth is already 1-indexed from the loop
- **Difficulty:** easy
- **Location:** src/gather.rs:181
- **Description:** In the BFS loop, `depth` starts at 0 for seed nodes. The decay is computed as `opts.decay_factor.powi((depth + 1) as i32)`. At depth 0 (seed's immediate neighbors), decay = 0.8^1 = 0.8. At depth 1, decay = 0.8^2 = 0.64. The `new_score = base_score * decay` means immediate neighbors get 80% of the seed score, second-hop neighbors get 64%. This is mathematically correct — depth 0 means "I'm expanding seeds", and neighbors discovered from seeds are at distance 1 from the seed, so decay^1 is right. No bug here, but the variable naming is confusing because `depth` in the queue represents the source node's depth, and the neighbor is at `depth + 1`. The `new_score` is correctly stored at `depth + 1` in `name_scores`. This is fine.
- **Suggested fix:** No fix needed — the math is correct. Optionally rename the variable for clarity: `let neighbor_depth = depth + 1;`

#### AC-11: `is_test_name` in scout.rs has false positives for names containing `_test` or `.test` as substrings
- **Difficulty:** easy
- **Location:** src/scout.rs:76-81
- **Description:** `is_test_name` checks `name.contains("_test")` and `name.contains(".test")`. This matches non-test functions like `validate_test_input`, `get_latest_version`, `contest_handler`, `fastest_path`. The function is used to classify chunks as `TestToUpdate` in scout results. A false positive means a production function gets labeled as a test, affecting the `untested_count` metric (tests are excluded from untested count) and the role classification shown to users. The `store/calls.rs` has a more sophisticated test detection pattern (JIRA: AC5 in v0.9.7 fixed trait impl check), but this scout-local function is simpler and more prone to false positives.
- **Suggested fix:** Tighten the patterns: require `_test` at the end of the name or as a prefix (`_test` → `name.ends_with("_test")` or `name.contains("_test_")`). Better: reuse the test detection from `store/calls.rs` which has the `find_test_chunks_async` SQL query using path patterns (`/tests/`, `_test.`, `.test.`, `.spec.`) AND name patterns (`test_`, `Test`).

#### AC-12: `search_chunks_by_signature` SQL LIKE pattern matches substring, causing false positives for short type names
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:805, src/related.rs:97-113
- **Description:** `search_chunks_by_signature` uses `LIKE %type_name%` to find chunks whose signature contains a type name. For short type names like `Path` (from `use std::path::Path`), this matches ANY signature containing "Path" as a substring — including `PathBuf`, `FilePath`, `SearchPath`, etc. The `extract_type_names` function filters out common types like `String`, `Vec`, `Result`, etc., but many legitimate short custom type names will produce false matches. For example, a type called `Node` would match `NodeId`, `TreeNode`, `NodeVisitor`, etc. This inflates the `shared_types` overlap count in `find_related`, making type-based co-occurrence analysis unreliable for short type names.
- **Suggested fix:** Use word-boundary matching. In SQLite, this could be done with a more specific LIKE pattern: `LIKE '%(type_name,%' OR LIKE '%(type_name)%' OR LIKE '%: type_name%' OR LIKE '% type_name,%'`. Alternatively, fetch candidates with LIKE and post-filter with a regex word-boundary check in Rust. Or use the FTS5 index for signature search.

## Extensibility

#### EXT-1: `scout()` hardcodes search parameters (15 results, 0.2 threshold)
- **Difficulty:** easy
- **Location:** src/scout.rs:103
- **Description:** `store.search_filtered(&query_embedding, &filter, 15, 0.2)` hardcodes both the seed result count (15) and the similarity threshold (0.2). These are not configurable via CLI flags or config file. The scout CLI exposes `--limit` for file groups but has no way to control seed search breadth. A user with a large codebase may want more seeds; a user wanting precision may want a higher threshold. Compare to `gather()` which exposes both via `GatherOptions.seed_limit` and `GatherOptions.seed_threshold`.
- **Suggested fix:** Add `seed_limit` and `seed_threshold` parameters to `scout()` (or wrap in a `ScoutOptions` struct like `GatherOptions`). Wire to CLI flags `--seed-limit` and `--seed-threshold`, defaulting to 15 and 0.2.

#### EXT-2: `suggest_placement()` hardcodes search parameters (10 results, 0.1 threshold)
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:74
- **Description:** `store.search_filtered(&query_embedding, &filter, 10, 0.1)` hardcodes both values. The `where` CLI exposes `--limit` for file suggestions but not the underlying search breadth or threshold. Same pattern as EXT-1.
- **Suggested fix:** Add `seed_limit` and `seed_threshold` parameters or accept them via a struct. Wire to CLI flags or use `GatherOptions`-style defaults.

#### EXT-3: `MODIFY_TARGET_THRESHOLD` hardcoded at 0.5 in scout
- **Difficulty:** easy
- **Location:** src/scout.rs:73
- **Description:** `const MODIFY_TARGET_THRESHOLD: f32 = 0.5` determines whether a chunk is classified as `ModifyTarget` vs `Dependency`. Not configurable. Users searching niche topics may find all results below 0.5 (all classified as "Dependency"), making role classification useless. Users with targeted queries may want a higher threshold to reduce noise.
- **Suggested fix:** Make this a field on a `ScoutOptions` struct with default 0.5, or add a `--modify-threshold` CLI flag.

#### EXT-4: `MAX_TEST_SEARCH_DEPTH` hardcoded at 5 in impact analysis
- **Difficulty:** easy
- **Location:** src/impact.rs:46
- **Description:** `const MAX_TEST_SEARCH_DEPTH: usize = 5` limits reverse BFS depth for test discovery. Used by `analyze_impact()`, `analyze_diff_impact()`, `compute_hints_with_graph()`, and `suggest_tests()`. A user with deep call chains (5+ layers between test and target) silently misses affected tests. The `impact` CLI exposes `--depth` for transitive callers but this is a *separate* depth limit — `MAX_TEST_SEARCH_DEPTH` for tests is unconditionally 5 regardless of `--depth`. Compare: `trace` CLI exposes `--max-depth` up to 50.
- **Suggested fix:** Accept `test_depth` as a parameter in `analyze_impact()` and `analyze_diff_impact()` instead of a module-level constant. Wire to CLI flag `--test-depth`, defaulting to 5.

#### EXT-5: `MAX_EXPANDED_NODES` (200) in gather not configurable
- **Difficulty:** easy
- **Location:** src/gather.rs:110
- **Description:** `const MAX_EXPANDED_NODES: usize = 200` caps BFS expansion in `gather()`. The `GatherOptions` struct has `expand_depth`, `limit`, `seed_limit`, `seed_threshold`, and `decay_factor` — all configurable. `MAX_EXPANDED_NODES` is the only gather parameter that isn't. A user analyzing a hub function intentionally may want to raise this limit.
- **Suggested fix:** Add `max_expanded_nodes: usize` to `GatherOptions` with default 200. Wire to CLI `--max-expand`.

#### EXT-6: Adding a new CLI command requires changes in 5 locations across 3 files
- **Difficulty:** medium
- **Location:** src/cli/commands/mod.rs, src/cli/mod.rs (Commands enum + dispatch + imports)
- **Description:** Adding a new CLI subcommand requires: (1) create handler file, (2) add `mod` + `pub use` in `commands/mod.rs`, (3) add `Commands` enum variant with clap attributes, (4) add import in `use commands::{...}`, (5) add dispatch match arm in `run_with()`. This is 5 locations across 3 files. Since the prior audit (which noted X2 for Pattern), 4 new commands were added (scout, where, related, impact-diff), each touching all 5 locations.
- **Suggested fix:** The 5-file cost is inherent to Rust's module system + clap derive. Document the checklist in CONTRIBUTING.md. Optionally create a `scripts/new-command.sh` template. The cost is manageable but should be explicit.

#### EXT-7: Test detection patterns hardcoded in SQL and Rust with no user override
- **Difficulty:** medium
- **Location:** src/store/calls.rs:41-56, src/scout.rs:76-81
- **Description:** Test file/function detection uses four separate hardcoded pattern lists:
  1. `TEST_NAME_PATTERNS` (SQL LIKE): `["test_%", "Test%"]`
  2. `TEST_CONTENT_MARKERS`: `["#[test]", "@Test"]` — misses `@pytest.mark`, `#[tokio::test]`, `#[rstest]`, `describe(`.
  3. `TEST_PATH_PATTERNS`: `["%/tests/%", "%_test.%", "%.test.%", "%.spec.%"]` — misses `__tests__/` (Jest), `e2e/`, `integration/`.
  4. Scout's `is_test_name()` in Rust: a *different* heuristic set.

  None are user-configurable. A user with non-standard test conventions gets incorrect test-map and impact results with no recourse. Prior audit X4 (P2) noted SQL/Rust duplication but not the lack of user configurability.
- **Suggested fix:** Add `[test_detection]` section to `.cqs.toml` config with `name_patterns`, `path_patterns`, `content_markers`. Load in `Config`, pass to store queries and scout. Unify the Rust `is_test_name()` to use the same pattern set. Highest-impact extensibility fix — test detection accuracy affects `impact`, `test-map`, `scout`, and `dead`.

#### EXT-8: `extract_patterns` language logic is a closed 145-line switch statement
- **Difficulty:** medium
- **Location:** src/where_to_add.rs:147-291
- **Description:** `extract_patterns()` has a 145-line `match language` with per-language pattern extraction for Rust, Python, TS/JS, Go, Java. Adding a new language requires a new match arm. SQL and Markdown fall through to `_ => "default"`. The `define_languages!` macro makes adding a *parsed* language easy (one line), but `where` command support requires a separate code change. The `LanguageDef` struct could carry pattern extraction hints to make this data-driven.
- **Suggested fix:** Add optional hints to `LanguageDef`:
  ```rust
  pub import_prefixes: &'static [&'static str],
  pub error_indicators: &'static [(&'static str, &'static str)],
  ```
  Then `extract_patterns` becomes a generic loop instead of a match-per-language. New languages get pattern extraction for free by filling in their `LanguageDef`.

#### EXT-9: Structural `Pattern` enum still requires 5 changes to add a variant (prior X2 unfixed)
- **Difficulty:** medium
- **Location:** src/structural.rs:10-73
- **Description:** Prior audit X2 (P2) noted adding a `Pattern` requires 5 changes: (1) enum variant, (2) `FromStr`, (3) `Display`, (4) `all_names()`, (5) `matches()` + matcher function. Still true in v0.12.1 — no refactoring occurred. The `test_all_names_covers_all_variants` test (line 245) hardcodes `assert_eq!(Pattern::all_names().len(), 6)` which must be manually bumped. Compare to `Language` which uses `define_languages!` macro.
- **Suggested fix:** A `define_patterns!` macro similar to `define_languages!` could reduce cost to 1 declaration line + matcher function. Alternatively, move to data-driven approach with `PatternDef` structs.

#### EXT-10: `is_test_name()` in scout duplicates and diverges from test detection in `store/calls.rs`
- **Difficulty:** easy
- **Location:** src/scout.rs:76-81 vs src/store/calls.rs:41-56
- **Description:** Scout defines its own `is_test_name()` with 4 patterns that diverge from the store's SQL-based test detection. Scout matches `foo_test` (contains `"_test"`) but SQL `test_%` wouldn't match by name alone. Scout doesn't check content markers or path patterns. This means scout and impact/test-map can disagree on what's a test.
- **Suggested fix:** Extract a shared `is_test_function(name, file, content)` in a common module. Both scout and store/calls use the same function. Creates single point for user-configurable patterns (EXT-7).

#### EXT-11: `apply_config_defaults` uses two different detection patterns with desync risk
- **Difficulty:** easy
- **Location:** src/cli/config.rs:91-133
- **Description:** `apply_config_defaults()` detects whether CLI flags were explicitly set by comparing against known defaults: epsilon comparison for `f32` fields, identity for `usize`, and `!flag` for booleans. Each new config-backed CLI flag requires a manually-maintained `DEFAULT_*` constant in `cli/config.rs` that must stay in sync with the `default_value` in the clap derive attribute in `cli/mod.rs`. A desync silently breaks config override logic. Prior audit X9 noted this for f32 fields; since then `stale_check` and `note_only` were added with boolean comparison — a third pattern.
- **Suggested fix:** Use `Option<T>` in `Cli` instead of pre-filled defaults. With `Option<T>`, the check becomes `if cli.limit.is_none() { cli.limit = config.limit; }` — no epsilon comparison, no sync risk. Same fix suggested in prior X9 but with more context on pattern divergence.

#### EXT-12: `where_to_add` import cap hardcoded at 5
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:282
- **Description:** `imports.truncate(5)` caps imports in placement suggestions with no override. For files with 20+ imports (common in Go, Java, large Rust modules), users see only 5.
- **Suggested fix:** Accept `max_imports` as a parameter or field on a `PlacementOptions` struct. Default to 5. Low priority.

## Robustness

#### RB-1: `hunk.start + hunk.count` can overflow `u32` in `map_hunks_to_functions`
- **Difficulty:** easy
- **Location:** src/impact.rs:436
- **Description:** `let hunk_end = hunk.start + hunk.count` where both are `u32`. A malformed diff with extremely large line numbers could cause this to wrap. `diff_parse.rs` uses `caps[1].parse().unwrap_or(1)` which defaults to 1 when parse fails (including overflow past `u32::MAX`), so the immediate risk is low. However, on a legitimate very large file near the `u32` boundary, `start + count` could overflow in debug mode (panic) or wrap in release mode (incorrect overlap detection).
- **Suggested fix:** Use `hunk.start.saturating_add(hunk.count)` to cap at `u32::MAX` instead of wrapping.

#### RB-2: `impact-diff --stdin` has no input size limit
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:89-92
- **Description:** `read_stdin()` calls `std::io::stdin().read_to_string(&mut buf)?` with no size guard. If piped multi-GB input, the process attempts unbounded allocation until OOM. Every other file-reading path in the codebase has size guards: `MAX_CONFIG_SIZE` (1MB), `MAX_NOTES_FILE_SIZE`, `MAX_DISPLAY_FILE_SIZE` (10MB), `MAX_ID_MAP_SIZE` (500MB). The `impact-diff --stdin` path has none.
- **Suggested fix:** Add `const MAX_DIFF_SIZE: usize = 50 * 1024 * 1024;` and use `stdin().take(MAX_DIFF_SIZE as u64 + 1).read_to_string(&mut buf)`, then error if too large.

#### RB-3: `diff_parse.rs` silently defaults unparseable hunk line numbers to 1
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:60-64
- **Description:** `caps[1].parse().unwrap_or(1)` and `m.as_str().parse().unwrap_or(1)` silently map any parse failure (including numeric overflow past `u32::MAX`) to `start=1, count=1`. This means a malformed diff could report impact against line 1 of the file instead of surfacing an error. Combined with `map_hunks_to_functions`, a corrupted hunk header silently blames whatever function starts near line 1.
- **Suggested fix:** Log `tracing::warn!` when parse fails, or skip the hunk entirely. At minimum don't silently map to line 1 — return a sentinel or skip.

#### RB-4: `parser/mod.rs` `.expect("registry/enum mismatch")` in library code
- **Difficulty:** medium
- **Location:** src/parser/mod.rs:56
- **Description:** `def.name.parse::<Language>().expect("registry/enum mismatch")` panics if the language registry contains a name that doesn't have a matching `Language` enum variant's `FromStr` impl. This is a programmer invariant, but it's in `Parser::new()` which is public library API. If a new language is added to the registry without updating `Language::FromStr`, the panic occurs at runtime during initialization, not at compile time. The similar issue with `Language::def()` was noted in the prior audit as R2 and fixed — this instance was missed.
- **Suggested fix:** Return `Err(ParserError::...)` instead of panicking, or add a compile-time test asserting all registry names parse.

#### RB-5: `window_idx` cast `i64 as u32` truncates without clamping
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1048
- **Description:** `.map(|i| i as u32)` on an `i64` from SQLite. Negative values or values exceeding `u32::MAX` (from DB corruption or manual edits) silently truncate. Other integer fields from SQLite use `clamp_line_number()` (helpers.rs:611) which clamps to `1..u32::MAX`, but `window_idx` uses bare `as u32`. Since `window_idx` represents a 0-based window offset, negative values should map to 0 and huge values should be capped.
- **Suggested fix:** Use `.map(|i| i.clamp(0, u32::MAX as i64) as u32)`.

#### RB-6: `display.rs` `end_idx + context + 1` can overflow on extreme values
- **Difficulty:** easy
- **Location:** src/cli/display.rs:61
- **Description:** `(end_idx + context + 1).min(lines.len())` — if `end_idx` is near `usize::MAX`, the addition overflows in debug mode (panic) or wraps in release. In practice `end_idx` comes from clamped `line_end as usize` and `context` is a small CLI parameter. The inconsistency is that `context_start` (line 50) uses `saturating_sub` for the same kind of arithmetic, but `context_end` doesn't use `saturating_add`.
- **Suggested fix:** Use `end_idx.saturating_add(context).saturating_add(1).min(lines.len())`.

#### RB-7: `run_git_diff` doesn't reject flag-like `base` argument
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:96-100
- **Description:** `run_git_diff(base)` passes the user-provided `base` string directly to `std::process::Command::arg()`. This is NOT shell injection (Command bypasses the shell), but git's diff accepts flags that could modify behavior. A user passing `--output=/tmp/exfil` or `--no-index /etc/passwd /etc/shadow` as the base ref could cause git to write files or compare arbitrary paths. The `base` argument is intended to be a git ref.
- **Suggested fix:** Reject `base` values starting with `-`, or insert `--` before the base argument: `cmd.args(["diff", "--"]).arg(b)`.

#### RB-8: `is_test_name` has false positives for names containing `_test` as substring
- **Difficulty:** easy
- **Location:** src/scout.rs:76-81
- **Description:** `name.contains("_test")` matches any function with `_test` anywhere: `validate_test_config`, `get_contest_results`, `fastest_path`. These production functions get misclassified as `TestToUpdate` in scout results, excluding them from the `untested_count` metric. The `store/calls.rs` test detection uses SQL patterns (`test_%`, `Test%`) which only match prefixes, not substrings. Scout's check is looser and inconsistent. Also noted in AC-11 and EXT-10 from other perspectives.
- **Suggested fix:** Tighten to `name.ends_with("_test")` or `name.contains("_test_")`, or reuse the test detection logic from `store/calls.rs`.

#### RB-9: `gather` `decay_factor` has no validation — negative, NaN, or >1.0 values accepted
- **Difficulty:** easy
- **Location:** src/gather.rs:49-52
- **Description:** `GatherOptions::with_decay_factor()` accepts any `f32` with no bounds check. Negative values cause alternating-sign scores. Values > 1.0 cause exponential amplification per BFS level. NaN propagates through all scores, producing unpredictable sort order. The CLI parses this from a user-provided float with no validation.
- **Suggested fix:** Clamp in `with_decay_factor()`: `self.decay_factor = factor.clamp(0.0, 1.0)` with `tracing::warn!` if clamped.

#### RB-10: `suggest_placement` silently degrades all suggestions on store error
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:107-109
- **Description:** `store.get_chunks_by_origin().unwrap_or_default()` silently returns empty on DB error. Downstream `extract_patterns()` produces default patterns for every field: `"default"` visibility, empty imports, `"snake_case"` naming. The user gets placement suggestions with all-default patterns with no indication that pattern data was unavailable. This is the robustness angle of EH-4 — not just error handling but silent degradation producing misleading output.
- **Suggested fix:** Propagate with `?` since the function returns `Result<_, SuggestError>` and has `From<StoreError>`.

#### RB-11: `diff_parse` doesn't reset `current_file` on `diff --git` boundary lines
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:35
- **Description:** The parser only resets file context on `+++ ` lines. If content contains a line starting with `+++ ` (possible in nested diffs, generated code, or documentation), it would be misinterpreted as a new file header, misassigning subsequent hunks. The `diff --git a/... b/...` line is the canonical file boundary but isn't tracked. A spurious `+++ ` inside a `+` (addition) line would only appear with that prefix, making false matches unlikely but possible.
- **Suggested fix:** Track `diff --git` lines as the authoritative boundary. Reset `current_file = None` on `diff --git `. Only process `+++ ` lines between a `diff --git` and the first hunk header.

## Test Coverage

#### TC-1: `related.rs` (133 lines) has zero tests — no unit or integration tests
- **Difficulty:** medium
- **Location:** src/related.rs
- **Description:** The entire `related.rs` module — `find_related()`, `resolve_to_related()`, and `find_type_overlap()` — has no `#[cfg(test)]` module and no integration test file. This is a 133-line module with three functions performing store queries (shared callers, shared callees, signature-based type overlap) that are completely untested. The `find_related()` function is the public entry point called from `cli/commands/related.rs`. The only coverage comes from the CLI binary integration test suite, which also has no `related` test. Compare: `scout.rs` (similar complexity) has 8 unit tests, `where_to_add.rs` has 8 unit tests, `diff_parse.rs` has 8 unit tests.
- **Suggested fix:** Add integration tests in `tests/` using the existing `TestStore` helper (see `tests/impact_diff_test.rs` for pattern). Test: (1) `find_related` with known shared callers returns expected overlap, (2) `find_related` with no callers returns empty, (3) `find_type_overlap` with matching signatures, (4) `resolve_to_related` with missing chunks returns empty (the `.ok()?` path).

#### TC-2: `test_scout_summary_zero` is tautological — asserts struct field equals what was just assigned
- **Difficulty:** easy
- **Location:** src/scout.rs:422-432
- **Description:** The test creates `ScoutSummary { total_files: 0, total_functions: 0, untested_count: 0, stale_count: 0 }` then asserts `summary.total_files == 0` and `summary.stale_count == 0`. This tests that Rust struct initialization works, not any business logic. It exercises zero code paths in the scout module. Similar to the v0.9.7 finding TC1 (tautological gather assertion), which was fixed in PR #334.
- **Suggested fix:** Either remove the test or replace with a meaningful test: call `scout_to_json` with a non-empty `ScoutResult` and verify the JSON includes expected fields with correct values (file count, function names, role classification). Better yet, add a `test_classify_role_boundary` test that checks the threshold edge case (`classify_role(0.5, "foo")` vs `classify_role(0.499, "foo")`).

#### TC-3: `test_suggest_tests_no_callers` is tautological — asserts empty callers are empty
- **Difficulty:** easy
- **Location:** src/impact.rs:827-839
- **Description:** Creates an `ImpactResult` with `callers: Vec::new()` and asserts `result.callers.is_empty()`. The comment says "We can't call suggest_tests without a store" — but the existing `TestStore` helper (used in `tests/impact_diff_test.rs`) provides exactly this capability. This test doesn't call `suggest_tests()` or any other function. It tests that an empty Vec is empty.
- **Suggested fix:** Either call `suggest_tests()` with a `TestStore` containing an untested caller (chunk with caller that's not in test paths), or remove the test. The `suggest_tests` function has zero direct test coverage — it's only exercised through the CLI `cqs impact --suggest-tests` if there's an integration test for it (there isn't).

#### TC-4: `test_placement_empty_result` is tautological — asserts empty suggestions are empty
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:465-472
- **Description:** Creates `PlacementResult { suggestions: Vec::new() }` and asserts `result.suggestions.is_empty()`. Same pattern as TC-2 and TC-3 — tests struct initialization, not any function. The `suggest_placement()` function has zero integration test coverage.
- **Suggested fix:** Replace with a test using `TestStore`. Insert chunks with known patterns (Rust with `pub(crate)` functions), call `suggest_placement()`, verify the returned `PlacementSuggestion` contains expected patterns (detected naming convention, visibility, imports). Or write an integration test in `tests/where_test.rs`.

#### TC-5: `test_chunk_role_equality` tests Rust's `#[derive(PartialEq)]`
- **Difficulty:** easy
- **Location:** src/scout.rs:452-457
- **Description:** Tests that `ChunkRole::ModifyTarget == ChunkRole::ModifyTarget` and `ChunkRole::ModifyTarget != ChunkRole::Dependency`. This tests the `PartialEq` derive, which is compiler-generated and guaranteed correct. Zero lines of project code exercised.
- **Suggested fix:** Remove or replace with a test that exercises `classify_role()` with boundary values. For instance, test that score 0.5 is `ModifyTarget` but 0.499 is `Dependency`, or that test-named functions get `TestToUpdate` regardless of score.

#### TC-6: No integration tests for 5 new CLI commands (scout, where, related, impact-diff, stale)
- **Difficulty:** medium
- **Location:** tests/cli_test.rs, tests/cli_graph_test.rs
- **Description:** The CLI integration test suite (`cli_test.rs` with 25 tests, `cli_graph_test.rs` with 27 tests) covers `search`, `explain`, `similar`, `context`, `gather`, `impact`, `trace`, `test-map`, `callers`, `callees`, `dead`, `gc`, `doctor`, `stats`, `init`, `index`, `notes`, `project`, `read`, and `audit-mode`. Missing: `scout`, `where`, `related`, `impact-diff`, and `stale`. These are 5 commands added since v0.9.7 with zero CLI integration testing. The prior audit TC3 noted "11 CLI commands untested" — these 5 plus 6 others (`diff`, `resolve`, `doctor`, `graph`, `reference`, `stale`) were missing. Since then `doctor` and more were added, but these 5 are still uncovered.
- **Suggested fix:** Add integration tests to `cli_graph_test.rs` (which already has the test store with call graph data needed for impact-diff and related). Minimal tests: (1) `cqs scout "task" --json` returns valid JSON with `file_groups` key, (2) `cqs where "description" --json` returns valid JSON, (3) `cqs related <function> --json` returns valid JSON, (4) `cqs impact-diff --base HEAD --json` returns valid JSON, (5) `cqs stale --json` returns valid JSON. These smoke tests catch serialization regressions and argument parsing bugs.

#### TC-7: `suggest_tests()` has zero test coverage (only dead-code test for empty callers)
- **Difficulty:** medium
- **Location:** src/impact.rs:598-695
- **Description:** `suggest_tests()` is a 100-line function that discovers untested callers, detects inline test presence, generates language-specific test names, and suggests test file locations. The `suggest_test_file` helper has 6 tests (good), but `suggest_tests()` itself — the orchestrator that calls `get_call_graph()`, `find_test_chunks()`, and combines everything — has zero test coverage. The `test_suggest_tests_no_callers` test doesn't call it (TC-3). The CLI `cqs impact --suggest-tests` flag has no integration test.
- **Suggested fix:** Add an integration test in `tests/impact_diff_test.rs` or a new `tests/impact_test.rs`: create a `TestStore` with chunks and call graph where one function has a caller that is NOT a test, then call `suggest_tests()` and verify it returns a `TestSuggestion` with the correct test name, file, and inline flag.

#### TC-8: `analyze_impact()` integration-level function has no direct tests
- **Difficulty:** medium
- **Location:** src/impact.rs:49-69
- **Description:** `analyze_impact()` is the main entry point for impact analysis: it calls `build_caller_info()`, `get_call_graph()`, `find_affected_tests()`, and `find_transitive_callers()`. The unit tests cover `reverse_bfs()` (3 tests) and `suggest_test_file()` (6 tests), but `analyze_impact()` itself is never directly called in tests. The closest is `analyze_diff_impact()` which shares `reverse_bfs` but takes a different code path. CLI integration tests (`test_impact_json`, `test_impact_text_output`) exist but they test the full CLI binary, not the library function — if the CLI wiring changes, the library function could break without any test catching it.
- **Suggested fix:** Add an integration test using `TestStore` that calls `analyze_impact()` directly. Insert chunks with a known call chain (A calls B calls test_C), call `analyze_impact("B", 2)`, verify callers contains A and tests contains test_C.

#### TC-9: `read_context_lines()` (80+ lines) in `display.rs` has zero tests
- **Difficulty:** easy
- **Location:** src/cli/display.rs:17-76
- **Description:** `read_context_lines()` handles edge cases: files larger than 10MB, `line_start=0` normalization, off-by-one clamping to valid ranges, context window before/after, empty files. This function is used by `cmd_explain`, `cmd_context`, and `cmd_similar` for code snippet display. Despite 80+ lines with 6+ edge-case branches, it has zero test coverage. An off-by-one here would produce garbled code output for every code snippet shown to users.
- **Suggested fix:** Add unit tests: (1) normal range extraction, (2) `line_start=0` normalization, (3) `line_end` past file end clamped, (4) `context=0` returns empty before/after, (5) file larger than 10MB rejected. Use `tempfile` to create test fixtures.

#### TC-10: `search_chunks_by_signature()` has zero tests
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:800-824
- **Description:** `search_chunks_by_signature()` is a public store method used by `related.rs::find_type_overlap()`. It performs a `LIKE %type_name%` SQL query on chunk signatures. Zero direct tests. The prior audit (v0.9.7) flagged general store/chunks test gaps (TC8, now fixed), but this specific method was added later and was missed. Issues: (1) no test for LIKE wildcard characters in type names (AC-4), (2) no test for the 100-result LIMIT, (3) no test verifying it only returns function/method chunks.
- **Suggested fix:** Add tests in `store::chunks::tests`: insert chunks with known signatures, query with a type name, verify matches. Test with `_` in type name to verify SQL LIKE wildcard behavior.

#### TC-11: Impact and diff-impact JSON serialization functions have zero tests
- **Difficulty:** easy
- **Location:** src/impact.rs:228-379, src/impact.rs:536-597
- **Description:** `impact_to_json()` (150 lines), `diff_impact_to_json()` (62 lines), and the mermaid diagram generation functions (`build_mermaid_graph`, `build_impact_mermaid`) have zero test coverage. These serialization functions construct complex nested JSON with path relativization and special formatting. `scout_to_json` has one test (empty case only). A typo in a JSON field name silently breaks downstream consumers (Claude Code skills, JSON piping). Compare: `SearchResult::to_json()` has tests via integration tests that assert on JSON structure.
- **Suggested fix:** Add tests for `impact_to_json` with a non-empty `ImpactResult`: verify key field names (`"function_name"`, `"callers"`, `"tests"`, `"transitive_callers"`, `"summary"`), verify path relativization works, verify mermaid output contains expected node labels.

#### TC-12: `mermaid_escape()` and `node_letter()` helper functions untested
- **Difficulty:** easy
- **Location:** src/impact.rs:730-742
- **Description:** `mermaid_escape()` escapes `"`, `<`, `>` for Mermaid diagram output. `node_letter()` generates letter-based labels for diagram nodes. Neither has tests. `mermaid_escape` would silently miss other Mermaid-special characters (e.g., `|`, `[`, `]`, `{`, `}`). `node_letter` has the ambiguity issue noted in AC-7 — but even without fixing the ambiguity, basic tests would document the current behavior and catch regressions.
- **Suggested fix:** Add tests: `assert_eq!(mermaid_escape("a<b>c"), "a&lt;b&gt;c")`, `assert_eq!(node_letter(0), "A")`, `assert_eq!(node_letter(25), "Z")`, `assert_eq!(node_letter(26), "A1")`.

#### TC-13: `display.rs` format functions (496 lines) have only 1 test
- **Difficulty:** medium
- **Location:** src/cli/display.rs
- **Description:** `display.rs` is 496 lines with 10+ public functions: `read_context_lines`, `display_result_text`, `display_result_text_with_context`, `display_unified_results_text`, `display_unified_results_json`, `display_tagged_text`, `display_tagged_json`, `display_notes_section`, `display_result_with_notes`, and `rel_path`. Only `display_unified_results_json` has one test (`test_display_unified_results_json_empty`), and that tests only the empty case. These functions format all user-visible output — text display, JSON output, note annotations, reference tagging. A regression in any of them produces garbled output that users see immediately.
- **Suggested fix:** Add tests for the JSON output functions (testable without terminal): `display_tagged_json` with mock `TaggedResult`, `display_result_text` with known inputs. Text formatting tests can assert on presence of expected strings rather than exact output.

#### TC-14: `warn_stale_results()` (39 lines) in `staleness.rs` has zero tests
- **Difficulty:** easy
- **Location:** src/cli/staleness.rs
- **Description:** `warn_stale_results()` checks search results against stale files and prints a warning. The function queries the store for stale origins, intersects with result files, and formats a user-visible warning. Zero tests. A bug here would silently suppress or spam staleness warnings, which are critical for users to know their results may be outdated.
- **Suggested fix:** Testable by extracting the core logic (count stale files in result set) into a pure function that takes result file paths and stale origins, returns count. Test with: some results in stale set, no results in stale set, all results in stale set.

#### TC-15: No tests for CRLF handling in `diff_parse.rs`
- **Difficulty:** easy
- **Location:** src/diff_parse.rs (all tests use LF-only strings)
- **Description:** All 8 `diff_parse` tests use `"\n"` line endings. Per PB-1, CRLF input from Windows processes can corrupt path extraction and hunk matching. No test verifies that `parse_unified_diff` handles CRLF input correctly. The fix for PB-1 (adding `input.replace("\r\n", "\n")`) should come with a test proving it works.
- **Suggested fix:** Add `test_parse_unified_diff_crlf()` with CRLF-encoded diff input (replace `\n` with `\r\n` in an existing test case). Assert identical results to the LF version.

#### TC-16: `compute_hints_with_graph()` edge case: stale call graph data not tested
- **Difficulty:** easy
- **Location:** src/impact.rs:77-104
- **Description:** `compute_hints_with_graph()` counts callers and tests from a pre-loaded call graph and test chunk list. The function uses `prefetched_caller_count` to avoid reloading, but no test verifies what happens when `prefetched_caller_count` disagrees with `graph.reverse` data (AC-8). The existing `hints_test.rs` tests use a `TestStore` where both sources are consistent. Adding a test with inconsistent data would document the behavior.
- **Suggested fix:** Add a test in `tests/hints_test.rs` where `prefetched_caller_count = Some(5)` but the graph has 3 callers. Verify `hints.caller_count == 5` (prefetched wins), documenting this as intentional.

## Performance

#### PERF-1: `suggest_tests()` runs per-caller reverse BFS — O(callers * graph_size)
- **Difficulty:** medium
- **Location:** src/impact.rs:610-695
- **Description:** `suggest_tests()` iterates over every caller in the impact result and calls `reverse_bfs(&graph, &caller.name, MAX_TEST_SEARCH_DEPTH)` for each one. With `MAX_TEST_SEARCH_DEPTH=5`, each BFS can touch a significant portion of the call graph. For a function with 20 callers, this means 20 independent BFS traversals. Additionally, each untested caller triggers `store.get_chunks_by_origin()` (line 634) to check for inline tests — another DB query per caller. The entire call graph + test chunk list are also loaded separately from the `analyze_impact()` caller (which already loads them at line 55-56), meaning duplicate loads.
- **Suggested fix:** (1) Accept `&CallGraph` and `&[ChunkSummary]` as parameters instead of loading them internally — the caller (`analyze_impact` or CLI) already has them. (2) For the BFS: precompute a single reverse BFS from all callers simultaneously (multi-source BFS) to avoid redundant traversal of shared ancestors. (3) For inline test detection: batch the `get_chunks_by_origin` calls by collecting unique files first, then querying once per file.

#### PERF-2: `extract_call_snippet()` N+1 `search_by_name` queries in impact analysis
- **Difficulty:** medium
- **Location:** src/impact.rs:131-170
- **Description:** `build_caller_info()` calls `extract_call_snippet()` for each caller (line 137), and `extract_call_snippet()` calls `store.search_by_name(&caller.name, 1)` — an FTS query per caller. Similarly, `find_transitive_callers()` at line 223 calls `store.search_by_name(caller_name, 1)` per transitive caller. For a function with 10 direct callers and 50 transitive callers, this is 60 individual FTS queries. The batch name search API (`search_by_names_batch`) already exists and could replace both loops.
- **Suggested fix:** Collect all caller names, call `store.search_by_names_batch(&all_names, 1)` once, then look up results from the returned HashMap. This reduces ~60 FTS queries to ~3 batched queries (at 20 names per batch).

#### PERF-3: `related.rs` N+1 `get_chunks_by_name` in `resolve_to_related()`
- **Difficulty:** easy
- **Location:** src/related.rs:64-79
- **Description:** `resolve_to_related()` calls `store.get_chunks_by_name(name)` for each `(name, overlap_count)` pair. With the default limit of 10, this is 10 individual `SELECT ... WHERE name = ?` queries for shared callers, plus 10 for shared callees = 20 queries. These could be batched into 1-2 queries.
- **Suggested fix:** Collect all names from both shared_callers and shared_callees, build a single `WHERE name IN (...)` batch query (similar to `get_caller_counts_batch`), then distribute results back. Add a `get_chunks_by_names_batch(&[&str])` method to Store.

#### PERF-4: `related.rs` `search_chunks_by_signature()` per type name — unbounded LIKE queries
- **Difficulty:** medium
- **Location:** src/related.rs:97-113
- **Description:** `find_type_overlap()` calls `store.search_chunks_by_signature(type_name)` in a loop for each extracted type name. Each call executes `SELECT ... WHERE signature LIKE '%TypeName%' LIMIT 100` — a full table scan with LIKE pattern matching. Functions with complex signatures (e.g., generic Rust functions) can have 5+ type names, resulting in 5+ full scans. LIKE with leading `%` cannot use indexes.
- **Suggested fix:** Combine all type names into a single query using `OR` conditions: `WHERE signature LIKE '%Type1%' OR signature LIKE '%Type2%' OR ... LIMIT 100`. This reduces N full scans to 1 full scan. Alternatively, add a tag column to chunks for type names mentioned in signatures (indexed), but that's a schema change.

#### PERF-5: `where_to_add.rs` per-file `get_chunks_by_origin` for pattern extraction
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:107-109
- **Description:** `suggest_placement()` groups search results by file, then for each file calls `store.get_chunks_by_origin(&file.to_string_lossy())` (line 107) to load ALL chunks from that file for pattern extraction. With the default limit of 3 files, this is 3 extra DB queries. The search results already contain chunk summaries with name, signature, and content — enough for most pattern extraction. The full-file load is only needed for import detection and inline test detection (which require non-matching chunks).
- **Suggested fix:** Use the already-fetched search result chunks for most pattern extraction (visibility counting, naming convention, error style). Only fall back to `get_chunks_by_origin` when the file's non-matching chunks are actually needed (import detection for non-matching chunks, and test module detection which could use a simpler `WHERE origin = ? AND content LIKE '%#[cfg(test)]%' LIMIT 1` query).

#### PERF-6: `where_to_add.rs` `extract_patterns` O(n) line scan concatenates all chunk content
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:152-158
- **Description:** `extract_patterns()` joins ALL chunk content into a single `String` (line 153: `chunks.iter().map(|c| c.content.as_str()).collect::<Vec<_>>().join("\n")`) and then scans it line-by-line for imports, error handling style, and test markers. For a large file with 50 chunks and ~100 lines each, this creates a ~5000-line string, then iterates all lines multiple times (once per language branch for imports, plus additional `contains()` calls for error style and test module detection). The content data already exists in `ChunkSummary.content` — iterating chunks directly avoids the join allocation and extra copy.
- **Suggested fix:** Iterate chunks directly instead of joining: for import detection, check each chunk's first few lines (imports are at file top); for `has_inline_tests`, check `chunks.iter().any(|c| c.content.contains("#[cfg(test)]"))`; for error style, check `chunks.iter().any(|c| c.content.contains("anyhow::"))`. This avoids the large string allocation and reduces to a single pass per chunk.

#### PERF-7: Watch mode rebuilds full HNSW index on every file change
- **Difficulty:** hard
- **Location:** src/cli/watch.rs:188
- **Prior:** Overlaps with DS6/P4 deferred from v0.9.7 audit. Re-reported because the impact is now more visible: per-chunk upsert was fixed (PR #336), so DB update is fast, but HNSW rebuild still dominates watch-mode latency.
- **Description:** After reindexing changed files, watch mode calls `build_hnsw_index(&store, &cqs_dir)` which rebuilds the entire HNSW index from scratch. `build_hnsw_index` loads ALL embeddings from the database via `store.embedding_batches()`, builds a new index, and writes it to disk. For a 10K-chunk index, this is ~30MB of embeddings loaded and reprocessed every time a single file changes.
- **Suggested fix:** Implement incremental HNSW updates: `hnsw-rs` supports `add_point()` and `remove_point()` on an existing index. After file reindex, compute the diff (removed chunk IDs, added chunk IDs), load the existing HNSW index, remove old points, insert new ones, save. This would reduce watch-mode HNSW update from O(N) to O(delta). Alternatively, debounce HNSW rebuilds separately from DB updates (e.g., rebuild HNSW only every 30 seconds or on idle).

#### PERF-8: `analyze_diff_impact` runs per-function reverse BFS — same issue as PERF-1
- **Difficulty:** medium
- **Location:** src/impact.rs:485-517
- **Description:** `analyze_diff_impact()` iterates over each changed function and calls `reverse_bfs(&graph, &func.name, MAX_TEST_SEARCH_DEPTH)` per function (line 503). For a diff touching 15 functions, this is 15 BFS traversals that may overlap significantly (shared call graph ancestors). Additionally, `extract_call_snippet()` is called per-caller (line 490), repeating the N+1 pattern from PERF-2.
- **Suggested fix:** (1) Multi-source BFS: Start BFS from all changed function names simultaneously, tracking which source led to each ancestor. This gives the same result with 1 BFS instead of N. (2) Batch `extract_call_snippet`: collect all unique caller names across all changed functions, do one `search_by_names_batch` call, then distribute snippets.

#### PERF-9: `imports.contains()` O(n^2) dedup in `extract_patterns()`
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:164
- **Description:** When collecting import statements, `extract_patterns()` checks `!imports.contains(&trimmed.to_string())` before pushing (lines 164, 201, 218, 238, 259). `Vec::contains()` is O(n) per check, making the total cost O(n^2) over all import lines. This pattern repeats for all 5 language branches. While import counts are typically <50, the `.to_string()` allocation for each comparison is wasteful — it allocates even when the import already exists.
- **Suggested fix:** Use a `HashSet<String>` for dedup instead of `Vec::contains()`. Insert and check in O(1). Convert to `Vec` only at the end (imports.truncate(5) is already applied). This also eliminates the unnecessary `.to_string()` allocation for each `contains` check.

#### PERF-10: `scout.rs` `compute_hints_with_graph` runs reverse BFS per chunk
- **Difficulty:** medium
- **Location:** src/scout.rs:152-157
- **Description:** Scout calls `compute_hints_with_graph()` for each search result chunk (line 152). Inside, `compute_hints_with_graph` calls `reverse_bfs(&graph, function_name, 5)` per chunk. With 15 search results, this is 15 reverse BFS traversals of the call graph. The caller counts are already batch-fetched (line 134: `get_caller_counts_batch`), but test counts still require individual BFS. Same class of issue as PERF-1 and PERF-8.
- **Suggested fix:** Multi-source BFS: Start a single BFS from all 15 function names simultaneously. The result maps each ancestor to which sources can reach it. Then intersect with test_chunks once. This replaces 15 BFS with 1.

#### PERF-11: `get_call_graph()` clones every caller and callee string twice
- **Difficulty:** easy
- **Location:** src/store/calls.rs:423-429
- **Prior:** Same as P12/P3 from v0.9.7 audit (PR #340 noted but not yet fixed). Re-reported for completeness.
- **Description:** `get_call_graph()` iterates over `(caller, callee)` rows and clones strings into both forward and reverse maps: `callee.clone()` for reverse entry key, `caller.clone()` for reverse entry value, `caller` moved into forward key, `callee` moved into forward value. But the `callee` is cloned on line 425 before the `caller` is cloned on line 427 — meaning every edge allocates 2 extra String clones. For a project with 5000 call edges and average 20-char function names, this is ~200KB of unnecessary allocations.
- **Suggested fix:** Use `Rc<str>` or intern strings so both maps share the same allocation. Or, since caller/callee names repeat heavily, build a `HashMap<String, usize>` name-to-id table first, then use `Vec<Vec<usize>>` adjacency lists.

## Security

#### SEC-1: `impact-diff --base` allows git argument injection via `--` prefixed values
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:95-112
- **Description:** `run_git_diff(base)` passes the `--base` CLI argument directly as `cmd.arg(b)` to `git diff`. While `std::process::Command` does not invoke a shell (so no shell injection), git itself interprets `--` prefixed strings as flags. A user passing `--base "--output=/tmp/overwrite"` causes `git diff --output=/tmp/overwrite`, writing diff output to an arbitrary file path. Other exploitable git-diff flags include `--no-index` (diff arbitrary files on disk, information disclosure). Since cqs is CLI-only and the user controls their own terminal, the practical severity is low (local user attacking themselves), but defense-in-depth warrants a fix.
- **Suggested fix:** If `base` starts with `-`, reject it with an error ("base ref must not start with '-'"). Alternatively, insert `--` before the base argument: `cmd.args(["diff", "--"]).arg(b)` so git treats it as a positional ref, not a flag. The `--` separator is the standard git convention for this.

#### SEC-2: `impact-diff --stdin` reads unbounded input into memory
- **Difficulty:** easy
- **Location:** src/cli/commands/impact_diff.rs:89-93
- **Description:** `read_stdin()` calls `std::io::stdin().read_to_string(&mut buf)` with no size limit. A piped input of arbitrary size (e.g., `cat /dev/urandom | cqs impact-diff --stdin`) causes unbounded memory allocation until OOM. While this is a local-only DoS (user piping to their own CLI), the `cqs read` command demonstrates the pattern of capping input size (10MB at line 41-48 of `read.rs`).
- **Suggested fix:** Use `BufReader` with a size cap, similar to how `cqs read` caps at 10MB. Read up to `MAX_DIFF_SIZE` bytes, then bail with an error if exceeded: `if buf.len() > MAX_DIFF_SIZE { bail!("Diff too large (max 10MB)"); }`. A 10MB diff is already extreme for impact analysis.

#### SEC-3: `note.rs` temp file uses predictable PID-based name, not random suffix
- **Difficulty:** easy
- **Location:** src/note.rs:228
- **Description:** The comment says "random suffix to prevent predictable name attacks" but uses `std::process::id()` which is the current PID — entirely predictable. The temp file path is `notes_path.with_extension(format!("toml.{}.tmp", std::process::id()))`. An attacker with write access to the project directory could create a symlink at the predictable path before the write occurs, causing the atomic write to follow the symlink and overwrite an arbitrary file. Practical severity is low (requires write access to the project directory, which already implies compromise), but the comment is misleading about the security property.
- **Suggested fix:** Either (a) use a truly random suffix: `format!("toml.{:016x}.tmp", rand::random::<u64>())`, or (b) use `tempfile::NamedTempFile` in the same directory for safe atomic creation, or (c) fix the misleading comment to say "PID-based suffix prevents concurrent process collision" (which is the actual property). Option (c) is honest and matches the actual threat model — the file lock already serializes access within a single process.

#### SEC-4: `config.rs` temp file uses fully static name `config.toml.tmp`
- **Difficulty:** easy
- **Location:** src/config.rs:337, src/config.rs:403
- **Description:** Both `add_reference_to_config` and `remove_reference_from_config` use `config_path.with_extension("toml.tmp")` — a completely static, predictable temp file name. While protected by the file lock on the config file itself, the `std::fs::write(&tmp_path, ...)` call follows symlinks. If an attacker places a symlink at `~/.config/cqs/config.toml.tmp` pointing to another file, the write would overwrite the symlink target. The user config directory is user-owned so this requires the attacker to already have the user's UID, making practical exploitation unlikely.
- **Suggested fix:** Same options as SEC-3. The simplest fix is to open with `O_NOFOLLOW` (via `OpenOptions`) or check `symlink_metadata()` before writing. Alternatively, accept the risk and document it — the lock file protects against the concurrent-process case, and the attacker-has-your-UID case is already game over.

#### SEC-5: `search_chunks_by_signature` LIKE pattern accepts user-derived wildcards without escaping
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:805
- **Description:** `search_chunks_by_signature(type_name)` constructs `format!("%{}%", type_name)` for a `LIKE ?1` query. While the query uses parameterized binding (safe from SQL injection), the `type_name` value may contain SQL LIKE wildcards `%` and `_`. Currently `type_name` comes from `extract_type_names()` parsing Rust/Python/Go/etc signatures, so it contains identifier characters only. However, the function is `pub` — any caller could pass arbitrary strings. A `type_name` of `%` would match every function signature, returning up to 100 results. This is a data-over-fetching issue, not a data-breach issue, since results are limited to 100.
- **Suggested fix:** Escape LIKE wildcards in `type_name`: replace `%` with `\%` and `_` with `\_`, and add `ESCAPE '\'` to the LIKE clause. This is defense-in-depth since current callers only pass safe identifiers.

#### SEC-6: `project.rs` atomic write uses static predictable temp name
- **Difficulty:** easy
- **Location:** src/project.rs:69
- **Description:** `ProjectRegistry.save()` uses `path.with_extension("toml.tmp")` for its temp file, same pattern as SEC-4. The project registry file (`~/.local/share/cqs/projects.toml`) records which project directories have been indexed. Same symlink-following risk as SEC-4, with the same low practical severity (attacker needs user's UID to write to `~/.local/share/cqs/`).
- **Suggested fix:** Same as SEC-4 — either use `O_NOFOLLOW`, use `tempfile::NamedTempFile`, or document the accepted risk.

#### SEC-7: No prior-audit regressions found (positive finding)
- **Difficulty:** n/a
- **Location:** n/a
- **Description:** Prior v0.9.7 security findings S1-S6 were all properly addressed:
  - **S1 (FTS5 injection):** `sanitize_fts_query` strips all FTS5 special characters. Verified at `src/store/mod.rs:115-128`. `search_fts` and `search_by_name` both call `sanitize_fts_query(normalize_for_fts(input))` before constructing FTS queries. Test coverage at `tests/store_test.rs:920-938`.
  - **S2 (path traversal in read):** `cmd_read` canonicalizes paths and checks `starts_with(project_canonical)` at `src/cli/commands/read.rs:33-38`. Uses `dunce::canonicalize` for Windows compatibility.
  - **S3 (sanitize_error_message):** Removed with MCP server (v0.10.0).
  - **S4 (MCP header reflection):** Removed with MCP server (v0.10.0).
  - **S5 (symlink reference paths):** `load_references` rejects symlinks at `src/reference.rs:40-53`.
  - **S6 (project config override):** `Config::override_with` warns on reference override at `src/config.rs:255-258`.
  All SQL queries use parameterized bindings. `std::process::Command` is used (not shell) for the one external command (`git diff`). File permissions are set to 0o600 across all state files.
- **Suggested fix:** No action needed. This is a positive verification of prior hardening.

## Data Safety

#### DS-1: Watch mode chunks and call graph not atomically consistent
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:342-371
- **Description:** In `reindex_files()`, chunks are replaced atomically per-file via `store.replace_file_chunks()` (line 352), but the call graph is updated in a separate loop via `store.upsert_function_calls()` (lines 356-371). These are independent transactions. If the process crashes or is killed between the two operations, the index will have new chunks but a stale call graph for those files. The pipeline (`src/cli/pipeline.rs`) correctly uses `upsert_chunks_and_calls()` which combines both in a single transaction, but watch mode doesn't use this path. Note: this is distinct from prior DS3 (which was about delete+reinsert atomicity within chunks, now fixed by `replace_file_chunks`). This finding is about cross-table atomicity between chunks and function_calls.
- **Suggested fix:** Either (a) add a `replace_file_chunks_and_calls()` method that wraps both operations in a single transaction, or (b) have `reindex_files()` collect parsed calls alongside chunks and use the existing `upsert_chunks_and_calls()` pattern. Option (b) requires restructuring the function to interleave parsing and call extraction in the same file loop.

#### DS-2: `function_calls` table missing path normalization (Windows)
- **Difficulty:** easy
- **Location:** src/store/calls.rs:294
- **Description:** `upsert_function_calls()` converts the file path with `file.to_string_lossy().into_owned()` (line 294), which preserves native path separators. On Windows, this stores backslash paths (`src\main.rs`). However, the `chunks` table normalizes all paths via `normalize_origin()` which converts backslashes to forward slashes (`src/main.rs`). This means cross-table joins or lookups between `chunks.origin` and `function_calls.file` will fail to match on Windows. The pipeline's `upsert_chunks_and_calls()` uses the `calls` table (chunk-level), not `function_calls`, so this mismatch only affects watch mode's function-level call graph.
- **Suggested fix:** Apply the same `normalize_origin()` call (or inline `path.to_string_lossy().replace('\\', "/")`) at `calls.rs:294` before storing the file path. Also audit all other `function_calls` queries that accept file paths to ensure they normalize consistently.

#### DS-3: `save_audit_state()` non-atomic write
- **Difficulty:** easy
- **Location:** src/audit.rs:112
- **Description:** `save_audit_state()` uses `std::fs::write(&path, content)` which is not atomic — a crash mid-write leaves a truncated or corrupt `audit-mode.json`. All other state-persisting functions in the codebase (config, notes, project registry, HNSW) use the temp-file-then-rename pattern for atomic writes. This is inconsistent and could leave audit mode in an unparseable state, causing `load_audit_state()` to return the default (disabled) even if the user had audit mode enabled.
- **Suggested fix:** Use the same temp+rename pattern: write to `audit-mode.json.tmp` in the same directory, then `std::fs::rename()` to the final path. This matches the pattern used in `config.rs`, `note.rs`, `project.rs`, and `hnsw/persist.rs`.

#### DS-4: HNSW multi-file save not atomically consistent
- **Difficulty:** hard
- **Location:** src/hnsw/persist.rs:214-232
- **Description:** HNSW save writes 4 files (graph, data, ids, checksum) by individually renaming from a temp directory. Each individual rename is atomic, but the set of 4 files is not. If the process crashes after renaming `hnsw.graph` but before `hnsw.checksum`, the next load will detect the mismatch via checksum verification (checksum is written last, so a partial write will fail verification). This is a reasonable mitigation — the failure mode is "can't load, must rebuild" rather than silent corruption. However, the `copy` fallback path (for cross-device scenarios at line 220) is NOT atomic even for individual files, so on Docker overlayfs or NFS, a crash mid-copy could leave a truncated file with a valid checksum from a previous save.
- **Suggested fix:** For the copy fallback, write to a temp file in the target directory first, then rename within the same filesystem. This ensures individual-file atomicity even on cross-device mounts. The multi-file consistency gap is acceptable given the checksum-last ordering — document this explicitly in the save method's doc comment.

#### DS-5: HNSW load TOCTOU between checksum verification and deserialization
- **Difficulty:** hard
- **Location:** src/hnsw/persist.rs:249-259
- **Description:** `HnswIndex::load()` first verifies checksums by reading all files (line 259: `verify_hnsw_checksums`), then reads the same files again for deserialization (line 304+). A concurrent `cqs watch` process could rebuild and overwrite the HNSW files between verification and deserialization. The failure mode is either a checksum mismatch error (caught) or corrupt deserialization (likely panics/errors from bincode). There is no file-level locking between HNSW readers and writers.
- **Suggested fix:** Use advisory file locking on the checksum file: writer acquires exclusive lock before rename, reader acquires shared lock before verify+load. This prevents the TOCTOU gap. Alternatively, load all file data into memory first (graph + data + ids), verify checksums on the in-memory buffers, then deserialize — this eliminates the re-read entirely.

#### DS-6: Watch mode `reindex_files` records mtime after indexing, not before
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:177-183
- **Description:** After calling `reindex_files()`, the mtime of each file is recorded in `last_indexed_mtime` (lines 177-183) by reading the file's current mtime. If a file was modified again *during* the reindex operation (between the initial read at line 128 and the mtime record at line 178), the second modification's mtime will be recorded as "already indexed" even though only the first version was actually indexed. The next event for that file will be skipped because its mtime matches `last_indexed_mtime`, causing the second modification to be silently lost.
- **Suggested fix:** Capture the mtime at the same time as the initial dedup check (line 128-134), store it alongside the pending file, and use that captured mtime for `last_indexed_mtime` instead of re-reading it after indexing.


## Resource Management

#### RM-1: `last_indexed_mtime` HashMap grows without bound in watch mode
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:97
- **Description:** `last_indexed_mtime: HashMap<PathBuf, SystemTime>` tracks the last mtime of every file processed by watch mode. Entries are never removed, even when files are deleted from disk. In projects with frequent file renames or branch switches (common in active development), this HashMap accumulates stale entries indefinitely. With 10,000 files over a long session, this is ~1MB — negligible for most users, but the unbounded growth pattern is the real concern. There's no `shrink_to_fit` or eviction.
- **Suggested fix:** Periodically prune `last_indexed_mtime` entries whose keys no longer exist on disk. A simple approach: every N reindex cycles (e.g., 100), remove entries for paths that don't exist. Alternatively, cap the map at `MAX_PENDING_FILES` and evict oldest entries.

#### RM-2: `reference.rs` uses `Store::open` (read-write) for read-only reference indexes
- **Difficulty:** easy
- **Location:** src/reference.rs:56
- **Description:** `load_references()` opens each reference store with `Store::open(&db_path)` — the full read-write path. This allocates a multi-threaded tokio runtime, 4-connection pool, 16MB page cache per connection, 256MB mmap per connection, and WAL checkpoint on drop. References are only searched, never written. With 3 references: 3 runtimes + up to 12 connections + 48MB page cache + 768MB mmap reservation. The `Store::open_readonly()` method already exists (line 260 of store/mod.rs) with 1 connection, 4MB cache, 64MB mmap, and single-threaded runtime — a perfect fit.
- **Suggested fix:** Change line 56 from `Store::open(&db_path)` to `Store::open_readonly(&db_path)`. No behavioral change since references are never written to during search.

#### RM-3: `search_across_projects` opens new read-write Store per project per search
- **Difficulty:** medium
- **Location:** src/project.rs:180
- **Description:** Each call to `search_across_projects` opens a new `Store::open(&index_path)` per registered project. This creates a new tokio runtime + connection pool + mmap + integrity check + WAL checkpoint on drop for each. The same applies when loading HNSW at line 183. For a user with 5 registered projects, every `cqs search --across` invocation creates 5 runtimes. This is the same class of issue as RM-2 but worse because it happens on every search invocation, not just at startup. The `Store::open_readonly` variant would be appropriate here since cross-project search is read-only.
- **Suggested fix:** Change `crate::Store::open(&index_path)` to `crate::Store::open_readonly(&index_path)`.

#### RM-4: `scout()` loads full call graph + all test chunks per invocation
- **Difficulty:** medium
- **Location:** src/scout.rs:129-130
- **Description:** `scout()` calls `store.get_call_graph()` (loads entire `function_calls` table into two HashMaps) and `store.find_test_chunks()` (loads all test chunk summaries including full content) on every invocation. For a project with 5000 call edges and 200 test functions, this is ~500KB per call. The call graph is the same data that `gather()` loads (line 154 of gather.rs), and both modules note this should be pre-loaded if called in a loop. Additionally, both `scout` and `gather` would benefit from sharing a pre-loaded graph in scenarios where multiple analysis functions are called on the same Store (e.g., `cqs scout` then `cqs gather`).
- **Suggested fix:** Accept an optional `&CallGraph` parameter (pre-loaded by the caller). The CLI command can load it once and pass it in. Same for `test_chunks` — accept `&[ChunkSummary]` or a test-count lookup function to avoid loading full content.

#### RM-5: `find_test_chunks()` loads full `content` column for all test functions
- **Difficulty:** easy
- **Location:** src/store/calls.rs:672-683
- **Description:** `find_test_chunks_async()` selects `content` in its query (via the full column list at line 673). The content is included in `ChunkSummary` but consumers typically only use `name` and `file` for test identification. `scout()` (line 152) only needs `name` for `compute_hints_with_graph`. `find_dead_code` (its other caller) already demonstrates a two-phase approach: lightweight metadata first, content only for candidates. For 200 test functions averaging 500 bytes of content, that's ~100KB of unnecessary data transfer from SQLite.
- **Suggested fix:** Create a `find_test_chunk_names()` variant that only returns `(name, file, line_start)` tuples — sufficient for `scout()` and `compute_hints_with_graph()`. Keep `find_test_chunks()` for callers that need full content.

#### RM-6: `where_to_add` loads all chunks for pattern extraction per file suggestion
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:107-108
- **Description:** For each file suggestion (up to `limit`), `suggest_placement()` calls `store.get_chunks_by_origin(&file)` which loads all chunks from that file including full content. It then concatenates all content into a single `all_content` string (line 153) for pattern analysis (imports, error handling, visibility, test presence). For a file with 50 functions averaging 500 bytes content, that's 25KB loaded and concatenated just to detect surface patterns. With `limit=5` file suggestions, this loads 5 files' worth of chunks.
- **Suggested fix:** Only load signatures and first few content lines for pattern extraction. Or introduce a lightweight `get_chunk_signatures_by_origin()` that returns only `(name, signature, chunk_type)` tuples, with a flag for whether `#[cfg(test)]` appears anywhere in the file.

#### RM-7: CAGRA `dataset` array retained in memory permanently (duplicates SQLite)
- **Difficulty:** medium
- **Location:** src/cagra.rs:62
- **Description:** `CagraIndex` keeps `dataset: Array2<f32>` as a field, holding all embeddings in a contiguous ndarray. For 50K chunks: 50000 * 769 * 4 = ~146MB. This is necessary because cuVS `search()` consumes the index and `rebuild_index_with_resources` needs the dataset to reconstruct it. However, this means embeddings exist in three places simultaneously: SQLite (on disk + page cache), CAGRA dataset (CPU memory), and CUDA device memory (after build). Prior audit RM2/P4-5 covered OOM during build; this finding is about the sustained ~146MB after build. For CLI use (single search then exit), this is harmless. For any future long-running scenario with CAGRA, the sustained memory is wasteful.
- **Suggested fix:** For CLI use: no change needed. If CAGRA is ever used in a persistent process, consider rebuilding from SQLite instead of caching the ndarray (trade ~100ms rebuild time for ~146MB memory savings).

#### RM-8: `Embedder::clear_session()` requires `&mut self` — unusable in watch mode
- **Difficulty:** easy
- **Location:** src/embedder.rs:434
- **Description:** `clear_session(&mut self)` is documented as the way to release ~500MB of ONNX model memory during idle periods. However, watch mode stores the Embedder in `OnceCell<Embedder>` (watch.rs:89), and `OnceCell::get()` returns `&Embedder` (immutable reference). There is no way to call `clear_session` without mutable access. The method was added responding to prior audit finding RM8 ("Embedder ~500MB persists forever via OnceLock"), marked "Fixed PR #343". But the fix is unreachable in practice — the only long-running consumer (watch mode) can't obtain `&mut Embedder`. The session field is `OnceCell<Mutex<Session>>`, but the method replaces the entire `OnceCell` which requires `&mut self`.
- **Suggested fix:** Replace `session: OnceCell<Mutex<Session>>` with `session: Mutex<Option<Session>>`. Then `clear_session(&self)` can lock the mutex and set the option to `None`. The lazy-load logic in `session()` already handles re-initialization. Then add idle-timeout logic in watch mode: after N minutes of no file changes, call `embedder.clear_session()`.

#### RM-9: `reindex_files` clones Chunk + Embedding in `by_file` grouping
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:336-341
- **Description:** In `reindex_files()`, after computing all embeddings, the code groups chunks by file for `replace_file_chunks`. At line 340: `.push((chunk.clone(), embedding.clone()))`. Each Chunk contains multiple Strings (id, name, signature, content, content_hash, doc) and each Embedding is 769 floats (3076 bytes). For a batch of 50 chunks, this doubles memory for the chunks + embeddings during grouping. The pipeline (pipeline.rs:602) handles this better by consuming the batch with `for (chunk, embedding) in batch.chunk_embeddings`.
- **Suggested fix:** Consume the `chunks` and `embeddings` vectors by using `into_iter()` instead of borrowing and cloning. Change the loop to: `for (chunk, embedding) in chunks.into_iter().zip(embeddings)`. Build the `mtime_cache` beforehand while still borrowing.

#### RM-10: Pipeline parses all files into one Vec before sending to embed channel
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:229, 273-316
- **Description:** The pipeline's parser thread uses `file_batch_size = 100_000` — effectively parsing all files in one batch. The `par_iter().flat_map().collect()` at line 273-316 collects all parsed chunks into a single Vec before sending to the embed channel in batches of 32. For a 10,000-file project with 5 chunks/file averaging 500 bytes content, that's ~25MB held simultaneously. The embed channel (bounded at 256) provides backpressure, but the parser accumulates the full parsed output before chunking it into sends.
- **Suggested fix:** Reduce `file_batch_size` to 1000-5000 so parsing happens in digestible batches. The outer `for file_batch in files.chunks(file_batch_size)` loop already supports this — just lower the constant. This keeps parser-thread memory proportional to `file_batch_size * chunks_per_file` instead of `total_files * chunks_per_file`.
