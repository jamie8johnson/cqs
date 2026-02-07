# Audit Findings — v0.9.1

Full 14-category audit. Collection phase — no fixes until all batches complete.

## Batch 1: Documentation

#### MCP tool count says 21 but there are only 20 tools
- **Difficulty:** easy
- **Location:** `CLAUDE.md:54`, `ROADMAP.md:315`, `CHANGELOG.md:32`, `PROJECT_CONTINUITY.md:56`
- **Description:** Multiple docs claim "21 MCP tools" but there are only 20 tool definitions in `src/mcp/tools/mod.rs` (lines 32-462) and 20 handler match arms (lines 484-504). Counting from the CLAUDE.md list also yields 20.
- **Suggested fix:** Change "21" to "20" in CLAUDE.md, ROADMAP.md, CHANGELOG.md, and PROJECT_CONTINUITY.md.

#### CHANGELOG missing comparison links for v0.7.0 through v0.9.1
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:624` (bottom of file)
- **Description:** Versions 0.9.1, 0.9.0, 0.8.0, and 0.7.0 have entries at the top of the changelog but no corresponding `[version]: URL` comparison links at the bottom. The last link is for v0.6.0.
- **Suggested fix:** Add comparison links for v0.7.0, v0.8.0, v0.9.0, and v0.9.1.

#### Error messages still say 'cq' instead of 'cqs' in 5 places
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:58`, `src/cli/commands/query.rs:23`, `src/cli/commands/stats.rs:19`, `src/cli/commands/init.rs:57`, `src/cli/commands/doctor.rs:84`
- **Description:** The v0.5.3 audit fixed StoreError strings that said "cq" but missed these user-facing messages. Messages like "Run 'cq init && cq index' first" should say "cqs".
- **Suggested fix:** Replace `'cq ` with `'cqs ` in all 5 locations.

#### CLI module doc says "cq" not "cqs"
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1`
- **Description:** The module-level doc comment reads `//! CLI implementation for cq` but the binary is `cqs`.
- **Suggested fix:** Change to `//! CLI implementation for cqs`.

#### PRIVACY.md missing data deletion paths
- **Difficulty:** easy
- **Location:** `PRIVACY.md:53-57`
- **Description:** The "Deleting Your Data" section only mentions `.cq/` and `~/.cache/huggingface/`. It omits `~/.config/cqs/` (user config with note_weight, threshold defaults) and `~/.local/share/cqs/` (reference index storage). Both are documented in SECURITY.md as read/write paths.
- **Suggested fix:** Add `rm -rf ~/.config/cqs/` and `rm -rf ~/.local/share/cqs/` to the deletion instructions.

#### Lib test count is 262 but docs say 258
- **Difficulty:** easy
- **Location:** `ROADMAP.md:315`, `PROJECT_CONTINUITY.md:55`
- **Description:** `cargo test --lib -- --list` reports 262 lib tests, but ROADMAP.md and PROJECT_CONTINUITY.md say "258 lib tests". The count may have drifted after adding tests in v0.9.1.
- **Suggested fix:** Update to 262 (or whatever the current count is after verification).

#### ROADMAP Phase 5 "Quality" still says "In Progress"
- **Difficulty:** easy
- **Location:** `ROADMAP.md:136`
- **Description:** Phase 5 (Quality) at line 136 says `### Status: In Progress` but all items in it are checked off. Phases 6, 7, and post-v0.8.0 are all marked "Complete". Phase 5 appears to be done.
- **Suggested fix:** Change to `### Status: Complete` with appropriate version range.

#### ROADMAP "Current Phase: 5 (Multi-index)" is stale
- **Difficulty:** easy
- **Location:** `ROADMAP.md:75`
- **Description:** Line 75 says "Current Phase: 5 (Multi-index)" but: (1) Multi-index was part of Phase 5 Quality which is done, (2) Phases 6, 7, and post-v0.8.0 are all complete, (3) the actual current work is "Next: New Languages (SQL + VB.NET)" at line 325. The "Current Phase" label is confusing.
- **Suggested fix:** Remove the "Current Phase" marker from the completed phase and/or rename the section headers. The actual next work is clearly labeled at line 325.

#### cqs_read tool schema missing "required" for path
- **Difficulty:** easy
- **Location:** `src/mcp/tools/mod.rs:141-153`
- **Description:** The `cqs_read` tool JSON schema has no `"required"` field. While the implementation (read.rs:19-24) correctly returns an error if both `path` and `focus` are missing, the schema doesn't tell LLM clients that at least one of `path` or `focus` is needed. LLMs may call it with empty arguments and get a confusing error.
- **Suggested fix:** Add a note in the tool description like "Requires either 'path' or 'focus' parameter" or add `"required": ["path"]` (since `focus` is an alternative path, and the error message already explains you can use focus instead).

#### CHANGELOG v0.9.0 date is same as v0.8.0 (both 2026-02-07)
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:20,34`
- **Description:** v0.9.0 and v0.8.0 both show date `2026-02-07`. This may be correct (same-day releases) but looks like a copy error since v0.9.1 at line 8 also says `2026-02-06`, meaning three versions in two days. If the dates are accurate, no fix needed.
- **Suggested fix:** Verify actual release dates. If any are wrong, correct them.

## Batch 1: Error Handling

#### 1. `rewrite_notes_file` atomic write loses context on temp file errors
- **Difficulty:** easy
- **Location:** `src/note.rs:145-146`
- **Description:** `std::fs::write(&tmp_path, output)?` and `std::fs::rename(&tmp_path, notes_path)?` use bare `?` without `.context()` or `.map_err()`. If the temp file write fails (e.g., disk full, permissions), the error is a raw `io::Error` with no indication of which file or operation failed. The `rewrite_notes_file` function wraps the read with `map_err` (line 129-133) for context, but doesn't do the same for write/rename.
- **Suggested fix:** Add `.map_err()` wrapping like the read path: `std::fs::write(&tmp_path, output).map_err(|e| NoteError::Io(std::io::Error::new(e.kind(), format!("{}: {}", tmp_path.display(), e))))?;`

#### 2. `parse_notes` bare `?` on file read loses path context
- **Difficulty:** easy
- **Location:** `src/note.rs:117`
- **Description:** `parse_notes()` calls `std::fs::read_to_string(path)?` with bare `?`. The error only contains the OS message (e.g., "No such file or directory") without the path. This contrasts with `rewrite_notes_file` (line 129-133) which carefully wraps the same operation with path context.
- **Suggested fix:** `std::fs::read_to_string(path).map_err(|e| NoteError::Io(std::io::Error::new(e.kind(), format!("{}: {}", path.display(), e))))?;`

#### 3. `cmd_index` bare `?` on `create_dir_all` and `remove_file` loses path
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:29,69`
- **Description:** `std::fs::create_dir_all(&cq_dir)?` and `std::fs::remove_file(&index_path)?` have bare `?`. If `.cq/` creation fails (permissions) or removing the old index fails (locked by another process), the user gets a raw OS error without the path. Compare with `cmd_init` which uses `.context("Failed to create .cq directory")`.
- **Suggested fix:** Add `.context()` like `cmd_init`: `std::fs::create_dir_all(&cq_dir).context("Failed to create .cq directory")?;`

#### 4. `cmd_ref_add` bare `?` on filesystem operations
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:85,217`
- **Description:** `std::fs::create_dir_all(&ref_dir)?` (line 85) and `std::fs::remove_dir_all(&cfg.path)?` (line 217) have bare `?`. If reference directory creation or deletion fails, the error lacks the path and context.
- **Suggested fix:** `std::fs::create_dir_all(&ref_dir).context(format!("Failed to create reference directory: {}", ref_dir.display()))?;`

#### 5. `extract_calls_in_chunk` silently discards query compilation errors
- **Difficulty:** medium
- **Location:** `src/parser/calls.rs:32-34`
- **Description:** `get_call_query(language)` error is matched with `Err(_) => return vec![]` — the actual error reason (e.g., query syntax error, unsupported language) is discarded. Since call graph extraction is important for `cqs_callers`/`cqs_callees`/`cqs_gather` tools, silently returning empty results hides problems. This could mask issues when adding new language support.
- **Suggested fix:** Log the error: `Err(e) => { tracing::debug!(language = %language, error = %e, "Failed to compile call query"); return vec![]; }`

#### 6. `cli/display.rs:read_context_lines` bare `?` on file read
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:22`
- **Description:** `std::fs::read_to_string(file)?` propagates a bare `io::Error` without the file path. When displaying search results, if a file has been deleted or moved since indexing, the user sees "No such file or directory" without knowing which file. This is a user-facing CLI function.
- **Suggested fix:** `std::fs::read_to_string(file).with_context(|| format!("Failed to read {}", file.display()))?;`

#### 7. `config.rs:add_reference_to_config` uses `unwrap_or_default` masking non-NotFound errors
- **Difficulty:** easy
- **Location:** `src/config.rs:184`
- **Description:** `std::fs::read_to_string(config_path).unwrap_or_default()` silently treats any read error (permissions, I/O error, encoding error) the same as "file doesn't exist". If the config file exists but is unreadable (wrong permissions, disk error), this silently creates an empty config and overwrites the file on the next write (line 222), destroying the existing config. Contrast with `remove_reference_from_config` (line 236) which properly matches on the error kind.
- **Suggested fix:** Match on the error kind: `match std::fs::read_to_string(config_path) { Ok(s) => s, Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(), Err(e) => return Err(e.into()) }`

#### 8. `MCP http.rs` response serialization swallows failure with `unwrap_or_default`
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:302`
- **Description:** `serde_json::to_value(&response).unwrap_or_default()` silently converts serialization failures into JSON `null`. If `JsonRpcResponse` fails to serialize, the client receives a null response with 200 OK status — making the failure invisible. While unlikely for simple types, custom data in the response could trigger this.
- **Suggested fix:** `serde_json::to_value(&response).unwrap_or_else(|e| { tracing::warn!("Failed to serialize response: {}", e); serde_json::json!({"error": "internal serialization error"}) })`

#### 9. `get_callers_full`/`get_callees_full` errors silently become empty results in 6 MCP/CLI call sites
- **Difficulty:** medium
- **Location:** `src/mcp/tools/explain.rs:54,60`, `src/mcp/tools/context.rs:56,83`, `src/cli/commands/explain.rs:55,58`, `src/cli/commands/context.rs:41,66`
- **Description:** Six call sites use `.unwrap_or_default()` on `get_callers_full`/`get_callees_full`. If the database query fails (e.g., corrupted DB, schema mismatch), the user sees functions with "no callers" and "no callees" instead of an error. For `cqs_explain` and `cqs_context`, this gives misleading results — a function appears to have no call graph when really the query failed.
- **Suggested fix:** At minimum log the error: `server.store.get_callers_full(&chunk.name).unwrap_or_else(|e| { tracing::warn!(name = %chunk.name, error = %e, "Failed to get callers"); vec![] })`

#### 10. `pipeline.rs` needs_reindex error silently forces re-embedding
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:294`
- **Description:** `Err(_) => true` on `store.needs_reindex()` means any database error causes the file to be re-embedded. While "reindex on error" is reasonable as a recovery strategy, discarding the error makes it invisible. If the store has a systematic issue (e.g., corrupted metadata table), every file gets re-embedded with no warning, wasting significant time on large codebases.
- **Suggested fix:** Log at debug level: `Err(e) => { tracing::debug!(file = %c.file.display(), error = %e, "needs_reindex check failed, will reindex"); true }`

#### 11. `embedder.rs` ORT library detection discards directory read error
- **Difficulty:** easy
- **Location:** `src/embedder.rs:610`
- **Description:** `Err(_) => return` on `std::fs::read_dir(&ort_cache)` silently abandons symlink setup when the cache directory is unreadable. Since this function sets up GPU acceleration paths, a silent failure means the user gets CPU-only performance without knowing why. The function already logs individual entry errors (line 603) but not the top-level directory error.
- **Suggested fix:** `Err(e) => { tracing::debug!(path = %ort_cache.display(), error = %e, "Cannot read ORT cache directory"); return; }`

#### 12. `gather.rs` silently skips `search_by_name` errors during BFS expansion
- **Difficulty:** easy
- **Location:** `src/gather.rs:137`
- **Description:** `if let Ok(results) = store.search_by_name(name, 1)` silently drops search failures. During BFS expansion in the `cqs_gather` tool, if a name lookup fails, the chunk is silently excluded from the gathered context. This could cause incomplete gather results with no indication of what was missed.
- **Suggested fix:** Log and continue: `match store.search_by_name(name, 1) { Ok(results) => { /* existing logic */ }, Err(e) => { tracing::debug!(name, error = %e, "Failed to look up chunk during gather expansion"); continue; } }`

#### 13. `cli/files.rs:acquire_index_lock` discards lock error details
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:76`
- **Description:** `Err(_)` on `lock_file.try_lock_exclusive()` discards the OS error. The error could distinguish between "already locked" (EWOULDBLOCK) and actual I/O errors (ENOLCK, EIO). Currently all lock failures are treated as "another process holds the lock", but an actual I/O error should be reported differently since PID-based stale lock recovery won't help.
- **Suggested fix:** Match on the error and log it: `Err(e) => { tracing::debug!(error = %e, "Lock acquisition failed"); /* existing stale lock recovery */ }`

## Batch 1: Code Quality

#### 1. Old call graph API is entirely dead (4 methods)
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:14-157` (`upsert_calls`, `get_callers`, `get_callees`, `call_stats`)
- **Description:** These four methods operate on the legacy `calls` table and have zero callers in production code. They were superseded by the `function_calls` table equivalents: `upsert_function_calls`, `get_callers_full`, `get_callees_full`, `function_call_stats`. The old `calls` table itself may also be dead (check schema.sql). `upsert_calls_batch` is the only old-table method still called (from pipeline.rs:584).
- **Suggested fix:** Remove `upsert_calls`, `get_callers`, `get_callees`, and `call_stats`. If `upsert_calls_batch` is the only survivor, consider migrating it to use `function_calls` table and removing the old `calls` table from schema.sql.

#### 2. Config accessor methods are dead (5 methods)
- **Difficulty:** easy
- **Location:** `src/config.rs:154-176` (`limit_or_default`, `threshold_or_default`, `name_boost_or_default`, `quiet_or_default`, `verbose_or_default`)
- **Description:** All five accessor methods have zero callers. The CLI uses `apply_config_defaults()` pattern instead, and MCP tools use `args.limit.unwrap_or(5)` inline. The associated constants (`DEFAULT_LIMIT`, `DEFAULT_THRESHOLD`, `DEFAULT_NAME_BOOST`) are also unused.
- **Suggested fix:** Remove all five methods and the three constants.

#### 3. `Store::get_chunk_by_id` is dead
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:460`
- **Description:** Zero callers. Likely added speculatively and never wired to any feature.
- **Suggested fix:** Remove.

#### 4. `Store::delete_notes_by_file` is dead
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:227`
- **Description:** Zero callers. Notes are managed by the `rewrite_notes_file` / `sync_notes` path, not by per-file deletion.
- **Suggested fix:** Remove.

#### 5. `Store::all_embeddings` is dead
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:613`
- **Description:** Zero callers (`.all_embeddings()` not called anywhere). Superseded by `embedding_batches()` which provides streaming/batched access and is used by HNSW build.
- **Suggested fix:** Remove. `embedding_batches` is the correct API.

#### 6. `Embedder::batch_size()` is dead
- **Difficulty:** easy
- **Location:** `src/embedder.rs:428`
- **Description:** Zero callers. Batch size is determined internally by the pipeline, not queried from the embedder.
- **Suggested fix:** Remove.

#### 7. `Embedding::try_new` is dead (doc-example only)
- **Difficulty:** easy
- **Location:** `src/embedder.rs:106`
- **Description:** Only appears in doc examples (lines 100, 103), never called in production or test code. The constructor `Embedding::new()` (which panics on wrong dimensions) is used everywhere instead.
- **Suggested fix:** Remove if the doc example isn't being tested (`cargo test --doc` would catch it). If keeping, consider adding `#[cfg(doc)]` or moving to a test.

#### 8. `find_test_chunks` and `find_test_chunks_async` duplicate identical SQL
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:421-510`
- **Description:** `find_test_chunks_async` (line 421, private async) and `find_test_chunks` (line 472, public sync) contain the exact same SQL query and mapping logic. The sync version wraps with `rt.block_on`, while `find_dead_code` calls the async version directly. This is copy-paste duplication.
- **Suggested fix:** Make `find_test_chunks` delegate to `find_test_chunks_async` via `self.rt.block_on(self.find_test_chunks_async())`, eliminating the duplicated SQL.

#### 9. Duplicated JSON construction in MCP search formatters
- **Difficulty:** medium
- **Location:** `src/mcp/tools/search.rs:293-403` (`format_unified_results` and `format_tagged_results`)
- **Description:** Both functions construct identical `serde_json::json!` objects for `UnifiedResult::Code` and `UnifiedResult::Note` variants. The only difference is that `format_tagged_results` adds an optional `"source"` field. Meanwhile, `format_code_result` (line 266) exists as a helper but is only used by `tool_search_name_only`, not by these two formatters.
- **Suggested fix:** Extract shared JSON construction into helpers (like `format_code_result` already is) and reuse them in both functions.

#### 10. GPU failure handling duplicated 3 times in pipeline.rs
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:373-396,434-456`
- **Description:** The "send cached to writer, requeue un-embedded to CPU" pattern appears 3 times in the GPU embedder thread: (1) long batch pre-filter at line 373, (2) GPU failure at line 434, and (3) the success path has similar but different structure. The first two are nearly identical: check cached not empty, send EmbeddedBatch, send ParsedBatch via fail_tx.
- **Suggested fix:** Extract a helper like `fn send_cached_and_requeue(cached, to_embed, embed_tx, fail_tx, file_mtimes, counter) -> Result<()>`.
## Batch 1: API Design

#### 1. Duplicated `parse_target` in explain.rs and similar.rs despite shared resolve.rs
- **Difficulty:** easy
- **Location:** `src/mcp/tools/explain.rs:11-20`, `src/mcp/tools/similar.rs:11-20`, `src/mcp/tools/resolve.rs:14-23`
- **Description:** `resolve.rs` was created to centralize the `parse_target` function and the full `resolve_target` flow. `trace.rs`, `impact.rs`, `test_map.rs`, and `read.rs` all use `resolve::resolve_target`. However, `explain.rs` and `similar.rs` still have their own identical private `parse_target` copies and inline the resolution logic (search_by_name + file filter + fallback to index 0). Any fix to target resolution must be applied in 3 places.
- **Suggested fix:** Replace the local `parse_target` + resolution logic in `explain.rs` and `similar.rs` with calls to `resolve::resolve_target`. `resolve_target` already returns `(ChunkSummary, Vec<SearchResult>)` which provides everything both tools need.

#### 2. Inconsistent response shape between callers and callees tools
- **Difficulty:** easy
- **Location:** `src/mcp/tools/call_graph.rs:18-34` (callers), `src/mcp/tools/call_graph.rs:54-60` (callees)
- **Description:** Three response shape differences: (1) Callers changes shape based on empty vs non-empty — adds `message` when empty, adds `count` when non-empty. Callees always returns the same shape with `count`. (2) Callers uses `"callers"` as the array key, callees uses `"calls"`. The natural pair would be `"callers"`/`"callees"`. (3) Callers omits the input function name; callees includes it as `"function"`. LLM clients parsing these must handle two different schemas for symmetric operations.
- **Suggested fix:** Normalize both to: `{ "function": name, "callers"|"callees": [...], "count": N }`. Always include `count`. Drop the conditional `message` — an empty array conveys "no results".

#### 3. name_only search returns bare array, semantic search returns wrapper object
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:204-213` (name_only), `src/mcp/tools/search.rs:327-342` (semantic)
- **Description:** Semantic search returns `{ "results": [...], "query": "...", "total": N }`. Name-only search (same `cqs_search` tool with `name_only=true`) returns a bare JSON array. A client consuming results from the same tool must check `name_only` to know which format to parse.
- **Suggested fix:** Wrap name_only results in the same `{ "results": [...], "query": "...", "total": N }` envelope.

#### 4. SearchFilter has pub fields alongside builder methods — dual construction paths
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:277-304` (struct), `src/store/helpers.rs:335-380` (builders)
- **Description:** `SearchFilter` has 7 `pub` fields AND 8 `with_*` builder methods. All internal callers (explain.rs:65-73, similar.rs:88-96, search.rs:49-64) construct the struct directly via field initialization, ignoring the builders. The `validate()` method is only called in one of several construction sites (search.rs:70-72). The builders add dead API surface that nobody uses internally.
- **Suggested fix:** Either remove the builders (since nobody uses them and the struct has pub fields anyway) or make fields private and enforce validation through builders. Removing builders is simpler given current usage.

#### 5. `batch` tool only supports 6 of 20 tools
- **Difficulty:** medium
- **Location:** `src/mcp/tools/batch.rs:27-37`, `src/mcp/tools/mod.rs:369` (schema enum)
- **Description:** `cqs_batch` accepts only `search`, `callers`, `callees`, `explain`, `similar`, and `stats` (6 of 20 tools). Notable omissions: `read`, `context`, `impact`, `trace`, `test_map`, `dead`, `gc`, `gather`. The description says "Eliminates round-trip overhead for independent lookups" without clarifying which tools are supported. LLM clients may try batching `context` or `impact` and get "Unknown batch tool" errors.
- **Suggested fix:** Either expand to support all stateless read-only tools or add the supported list to the description.

#### 6. Inconsistent parameter naming for function identifier across tools
- **Difficulty:** easy
- **Location:** `src/mcp/tools/mod.rs` (schema definitions)
- **Description:** Tools accepting a function identifier use different parameter names: `cqs_callers`, `cqs_callees`, `cqs_explain`, `cqs_impact`, `cqs_test_map` all use `"name"`. `cqs_similar` uses `"target"`. `cqs_trace` uses `"source"`/`"target"`. All accept the same `"file:function"` format. LLM must remember which tools use `name` vs `target` for the same concept.
- **Suggested fix:** Low severity — `similar` using `target` and `trace` using `source`/`target` make semantic sense. Document the format (`"name or file:name"`) consistently in all tool descriptions rather than renaming (breaking change).

#### 7. `Embedding::new()` bypasses dimension validation
- **Difficulty:** easy
- **Location:** `src/embedder.rs:87-89` (new), `src/embedder.rs:106-114` (try_new)
- **Description:** `Embedding::new(data)` accepts any `Vec<f32>` without checking length. `try_new()` validates against `EMBEDDING_DIM` (769). The type is `pub` and re-exported from `lib.rs`, so external consumers can create invalid Embeddings that fail later with confusing HNSW dimension mismatch errors. The v0.5.3 audit addressed `with_sentiment` validation (A16) but not the base constructor.
- **Suggested fix:** Add `debug_assert_eq!(data.len(), EMBEDDING_DIM)` to `new()` to catch misuse in development builds.

#### 8. `tool_stats` returns overlapping `hnsw_index` and `active_index` fields
- **Difficulty:** easy
- **Location:** `src/mcp/tools/stats.rs:34-53`
- **Description:** Stats response includes `hnsw_index` (checks on-disk) and `active_index` (checks in-memory). When HNSW is loaded, both report the same info in different formats: `"1234 vectors (O(log n) search)"` vs `"HNSW (1234 vectors)"`. The disk-vs-memory distinction is an implementation detail LLM consumers don't need. When CAGRA is active, `hnsw_index` shows stale disk info which is confusing.
- **Suggested fix:** Keep `active_index` and either remove or rename `hnsw_index` to `disk_index`.

#### 9. `node_letter` generates ambiguous Mermaid IDs after 26 nodes
- **Difficulty:** easy
- **Location:** `src/mcp/tools/trace.rs:215-221`
- **Description:** `node_letter(i)` generates A-Z for 0-25, then `A1`, `B1` for 26+. The pattern `(i % 26, i / 26)` means i=26 gives `A1` and i=52 gives `A2`. These could collide with user-defined IDs and don't scale cleanly. Unlikely to hit in practice (call chains >26 are rare) but is a latent bug.
- **Suggested fix:** Use sequential identifiers like `N0`, `N1`, `N2`... which scale indefinitely.

#### 10. `cqs_read` schema doesn't communicate path/focus requirement
- **Difficulty:** easy
- **Location:** `src/mcp/tools/mod.rs:141-153`
- **Description:** Cross-reference of Documentation finding. The JSON schema has no `required` field, so LLM clients don't know at least one of `path` or `focus` is needed. Implementation guards this but schema is misleading.
- **Suggested fix:** Add `"required": ["path"]` since path is the primary parameter and the error message explains focus as alternative.

## Batch 1: Observability

#### 1. MCP handle_request logs errors at debug level instead of warn
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:175`
- **Description:** When an MCP request fails, the full error is logged at `tracing::debug` level: `tracing::debug!(error = %full_error, "Request error")`. In a server handling LLM tool calls, errors should be visible at the default log level. Users running cqs with default log settings will never see that a tool call failed on the server side.
- **Suggested fix:** Change `tracing::debug!` to `tracing::warn!` on line 175.

#### 2. ensure_embedder() has no logging for expensive lazy initialization
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:130-150`
- **Description:** `ensure_embedder()` performs lazy model initialization via `Embedder::new()` or `Embedder::new_cpu()`, which takes ~500ms+ (model loading, ONNX session setup). There is zero logging — no span, no info log. The first MCP tool call that needs embeddings appears to hang with no explanation.
- **Suggested fix:** Add `tracing::info!(gpu = self.use_gpu, "Initializing embedder")` before the `Embedder::new` call and an info log after success.

#### 3. Watch mode uses eprintln! instead of tracing for warning
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:41`
- **Description:** Watch mode uses `eprintln!("Warning: --no-ignore is not yet implemented for watch mode")` instead of `tracing::warn!`. This bypasses the tracing subscriber, so it doesn't appear in structured log output and can interleave with progress output. Lines 55-63 also use `println!` for startup messages, which is less critical but inconsistent with tracing-based output elsewhere.
- **Suggested fix:** Change line 41 to `tracing::warn!("--no-ignore is not yet implemented for watch mode")`. Optionally convert lines 55-63 to `tracing::info!`.

#### 4. search_unified_with_index has no tracing span
- **Difficulty:** easy
- **Location:** `src/search.rs:526-605`
- **Description:** `search_unified_with_index` is the main entry point for MCP search — it orchestrates note search, HNSW lookup, candidate filtering, and result merging. It has no `info_span` or timing. The calling MCP tool handler logs completion, but the search function itself is invisible in span hierarchies. By contrast, `search_filtered` (line 216) and `search_filtered_with_index` (line 396) both have `info_span`.
- **Suggested fix:** Add `let _span = tracing::info_span!("search_unified_with_index", limit, threshold).entered();` at the top.

#### 5. search_by_candidate_ids has no tracing span
- **Difficulty:** easy
- **Location:** `src/search.rs:416-520`
- **Description:** `search_by_candidate_ids` is the HNSW-guided search path — fetches embeddings by ID, computes cosine similarity, applies filters, and does hybrid name scoring. No tracing span, no logging of candidate count vs result count. The only related log is `tracing::debug!("Index returned {} chunk candidates")` at the call site (line 567), not in this function.
- **Suggested fix:** Add `let _span = tracing::info_span!("search_by_candidate_ids", candidates = candidate_ids.len(), limit, threshold).entered();`

#### 6. MCP tool_read and tool_read_focused have zero logging
- **Difficulty:** easy
- **Location:** `src/mcp/tools/read.rs:13-310`
- **Description:** `tool_read` and `tool_read_focused` perform file reads with note injection and focused function extraction. Neither has any tracing calls. Other MCP tools (search, notes, explain) log their operations. For debugging MCP server behavior, it's useful to know what files are being read and whether focused reads find their target.
- **Suggested fix:** Add `tracing::debug!(path, "Reading file")` in `tool_read` and `tracing::debug!(focus, "Focused read")` in `tool_read_focused`.

#### 7. Name-only search path has no timing or completion logging
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:172-263`
- **Description:** `tool_search_name_only` performs name-based search across primary and reference indexes but has no timing or completion log. The semantic search path logs completion with timing at info level (lines 137-142, 161-166: "MCP search completed" / "MCP multi-index search completed"). The name-only path returns results silently.
- **Suggested fix:** Add timing and `tracing::info!(results = count, elapsed_ms, "MCP name-only search completed")` similar to the semantic path.

#### 8. handle_initialize doesn't log connecting client info
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214-243`
- **Description:** `handle_initialize` parses the client's name and version from the initialize request but discards them (`let _params`). Logging the connecting client (e.g., "Claude Code v1.2.3", "Cursor v0.45") would help debug MCP compatibility issues and understand which clients connect.
- **Suggested fix:** Add `tracing::info!(client = %_params.client_info.name, version = %_params.client_info.version, "MCP client connected")`.

#### 9. Note add/update/remove MCP tools don't log success
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:39-129` (add), `131-213` (update), `215-280` (remove)
- **Description:** `tool_add_note`, `tool_update_note`, and `tool_remove_note` perform file I/O and trigger reindexing, but none log the operation at any level on success. The `reindex_notes` helper logs failures (lines 18, 24) but not success. A debug log would help trace MCP tool activity.
- **Suggested fix:** Add `tracing::debug!(preview = %text_preview(text), "Note added")` after successful append in `tool_add_note`, and similar for update/remove.

#### 10. enumerate_files has no summary log of files found
- **Difficulty:** easy
- **Location:** `src/lib.rs:199-273`
- **Description:** `enumerate_files` walks the project directory, filters by extension and size, and returns matching paths. It logs individual canonicalization failures (warn for first 3, debug after), but never logs a summary count. For a function that determines the scope of indexing, a summary would help diagnose "why weren't my files indexed?" issues.
- **Suggested fix:** Add `tracing::info!(count = files.len(), "Enumerated files for indexing")` before the `Ok(files)` return.

#### 11. Pipeline completion stats not logged at info level
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:615-620`
- **Description:** `run_pipeline_concurrent` returns `PipelineStats { total_embedded, total_cached, gpu_failures, parse_errors }` but never logs these stats. The caller (CLI `cmd_index`) shows them in a progress bar, but the stats aren't in structured tracing output. When the pipeline runs from watch mode, the stats are lost entirely.
- **Suggested fix:** Add `tracing::info!(embedded = total_embedded, cached = total_cached, gpu_failures = ..., parse_errors = ..., "Pipeline completed")` before returning.

#### 12. touch_updated_at().ok() silently swallows errors
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:613`
- **Description:** `store.touch_updated_at().ok()` discards any error from updating the metadata timestamp. If this fails (database locked, disk full), the user gets no indication. The timestamp is used by `cqs gc` to detect staleness, so a silent failure means gc may incorrectly report stale indexes.
- **Suggested fix:** Change to `if let Err(e) = store.touch_updated_at() { tracing::warn!(error = %e, "Failed to update metadata timestamp"); }`

## Batch 2: Robustness

#### 1. `embedding_to_bytes` panics on dimension mismatch instead of returning error
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:500-507`
- **Description:** `embedding_to_bytes` uses `assert_eq!` to verify the embedding has exactly 769 dimensions. If an `Embedding::new()` was constructed with wrong dimensions (which the unchecked constructor allows), this panics at store insertion time with no recovery path. The panic occurs deep in the indexing pipeline where a graceful skip-and-continue would be safer. This was previously flagged as P3 #4 in the v0.5.3 triage but the assert remains.
- **Suggested fix:** Return `Result<Vec<u8>, StoreError>` instead of panicking: `if embedding.len() != EXPECTED_DIMENSIONS as usize { return Err(StoreError::DimensionMismatch(...)); }`. Callers can then skip the chunk with a warning.

#### 2. `HnswIndex::save` panics on HNSW/ID map count mismatch
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:104-110`
- **Description:** `HnswIndex::save()` uses `assert_eq!` to verify the HNSW point count matches the ID map length. While this invariant should always hold, if a bug causes divergence (e.g., a failed insert that partially mutates state), the index save panics instead of returning an error. This is especially problematic in the MCP server where a panic poisons the index RwLock, degrading all subsequent requests. An `Err` return would allow the caller to log and fall back to a stale-but-valid index.
- **Suggested fix:** Replace with `if hnsw_count != self.id_map.len() { return Err(HnswError::Internal(format!("HNSW/ID map count mismatch: ..."))); }`

#### 3. `embed_batch` uses unchecked tensor indexing for mean pooling
- **Difficulty:** medium
- **Location:** `src/embedder.rs:510-512`
- **Description:** `data[offset + k]` indexes into the ONNX output tensor without bounds checking. The offset calculation (`i * seq_len * embedding_dim + j * embedding_dim + k`) assumes the tensor shape is exactly `[batch, seq_len, 768]`. If the ONNX model returns an unexpected shape (e.g., model update, corrupted model file, or a different variant), this panics with an index-out-of-bounds. The shape `_shape` is extracted but never validated against expectations.
- **Suggested fix:** Add shape validation after line 494: `let shape = _shape; if shape.len() != 3 || shape[2] != embedding_dim { return Err(EmbedderError::InferenceError("Unexpected output tensor shape".into())); }` Or use `data.get(offset + k).copied().unwrap_or(0.0)` for defensive indexing.

#### 4. `acquire_index_lock` recursive retry has no depth limit
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:86`
- **Description:** When a stale lock is detected (dead PID), `acquire_index_lock` removes the lock file and calls itself recursively. If the removal succeeds but another process immediately re-creates the lock, the second call detects a stale lock again (if that process also dies), leading to unbounded recursion. While unlikely in practice (requires rapid PID reuse and process death), the function should limit retry depth.
- **Suggested fix:** Add a `retry: bool` parameter or use a loop with a single retry: `fn acquire_index_lock_inner(cq_dir: &Path, retried: bool) -> Result<File>` and refuse to retry twice.

#### 5. `process_exists` PID cast from u32 to i32 can produce negative values
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:23`
- **Description:** `libc::kill(pid as i32, 0)` casts u32 to i32. PIDs > 2^31 (i32::MAX) would become negative. Linux `pid_max` defaults to 32768 (max 4,194,304 on 64-bit), so this is unreachable via normal PIDs. However, the lock file is user-writable text — a crafted PID > i32::MAX would cast to a negative value, and `kill(-N, 0)` sends to process groups instead of individual processes. This could incorrectly report a stale lock as active (or vice versa).
- **Suggested fix:** Add `if pid > i32::MAX as u32 { return false; }` guard before the kill call.

#### 6. `parse_target` returns full string with colon for trailing-colon input
- **Difficulty:** easy
- **Location:** `src/mcp/tools/resolve.rs:14-23`
- **Description:** `parse_target("file.rs:")` finds `:` at the end, produces `file = "file.rs"` and `name = ""`. The `!name.is_empty()` guard catches this and falls through to `(None, "file.rs:")` — treating the full string including colon as the function name. This won't match any function in the index. The user gets "No function found matching 'file.rs:'" which is confusing.
- **Suggested fix:** Strip trailing colon before processing: `let target = target.trim_end_matches(':');` at the top. Then `"file.rs:"` becomes `"file.rs"` and falls through to search by name.

#### 7. MCP `tool_search` doesn't validate `threshold` range
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:23`
- **Description:** `threshold` is taken directly from user input with `unwrap_or(0.3)` but never validated. Values outside [0.0, 1.0] are nonsensical for cosine similarity: negative thresholds accept everything, values > 1.0 reject everything. `SearchFilter::validate()` checks `name_boost` and `note_weight` ranges but not `threshold`. A threshold of 2.0 produces zero results with no explanation.
- **Suggested fix:** Add `let threshold = args.threshold.unwrap_or(0.3).clamp(0.0, 1.0);` to silently clamp, or return an error for out-of-range values.

#### 8. `rewrite_notes_file` atomic write doesn't clean up temp file on rename failure
- **Difficulty:** easy
- **Location:** `src/note.rs:144-146`
- **Description:** If `std::fs::write(&tmp_path, output)` succeeds but `std::fs::rename(&tmp_path, notes_path)` fails (e.g., cross-device path, permissions), the temp file `notes.toml.tmp` is left on disk. Not blocking (next write overwrites it), but the orphan is confusing. More critically, if rename fails persistently, the user's note mutations are silently lost — the original file is unchanged but the function propagates the rename error, so the caller sees a failure but can't recover the written content.
- **Suggested fix:** Wrap in cleanup: `let result = std::fs::rename(&tmp_path, notes_path); if result.is_err() { let _ = std::fs::remove_file(&tmp_path); } result?;`

#### 9. `Regex::new` compiled on every `extract_type_names` call in focused read
- **Difficulty:** easy
- **Location:** `src/mcp/tools/read.rs:143`
- **Description:** `regex::Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").expect("hardcoded regex")` compiles on every `extract_type_names` invocation. The `expect` is safe (hardcoded pattern), but repeated compilation is wasteful. `tool_read_focused` calls this for each focused read request from the MCP client. Other modules (e.g., `nl.rs:21-23`) use `LazyLock<Regex>` for the same pattern.
- **Suggested fix:** Use `static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").unwrap());`

#### 10. `split_into_windows` lacks explicit `max_tokens == 0` guard
- **Difficulty:** easy
- **Location:** `src/embedder.rs:316-325`
- **Description:** `split_into_windows` validates `overlap >= max_tokens / 2` but has no explicit check for `max_tokens == 0`. When `max_tokens = 0`: `max_tokens / 2 = 0`, so `overlap >= 0` is always true, returning an error about overlap. The error message ("overlap (0) must be less than max_tokens/2 (0)") is confusing — it says nothing about max_tokens being zero. The function is only called internally with hardcoded values (128-token windows), so this is unreachable in practice.
- **Suggested fix:** Add `if max_tokens == 0 { return Err(EmbedderError::TokenizerError("max_tokens must be > 0".into())); }` for clarity.

#### 11. `get_callers_full`/`get_callees_full` errors silently become empty at 6 MCP/CLI sites
- **Difficulty:** medium
- **Location:** `src/mcp/tools/explain.rs:54,60`, `src/mcp/tools/context.rs:55,83`, `src/cli/commands/explain.rs:55,58`, `src/cli/commands/context.rs:41,66`
- **Description:** Six call sites use `.unwrap_or_default()` on `get_callers_full`/`get_callees_full`. If the database query fails (corrupted DB, schema mismatch), the user sees functions with "no callers" and "no callees" instead of an error. For `cqs_explain` and `cqs_context`, this gives misleading results — a function appears isolated when really the query failed. Already noted in Error Handling batch but included here for robustness overlap since it masks failures.
- **Suggested fix:** At minimum log: `.unwrap_or_else(|e| { tracing::warn!(name = %chunk.name, error = %e, "Failed to get callers"); vec![] })`

#### 12. `search_unified_with_index` limit=0 causes division issues in slot allocation
- **Difficulty:** easy
- **Location:** `src/search.rs:575-578`
- **Description:** If `limit = 0` reaches `search_unified_with_index`, `min_code_slots = ((0 * 3) / 5).max(1) = 1`, `code_count = code_results.len().min(0) = 0`, `reserved_code = 0.min(1) = 0`, `note_slots = 0 - 0 = 0`. This produces empty results, which is correct but wasteful (still runs the full search). The MCP search tool clamps to `limit >= 1`, but internal callers could pass 0. Not a crash, just unnecessary work.
- **Suggested fix:** Add early return: `if limit == 0 { return Ok(vec![]); }` at the top.

#### 13. `context.rs` unwrap_or on JSON value extraction could mask data issues
- **Difficulty:** easy
- **Location:** `src/mcp/tools/context.rs:100`
- **Description:** `c.get("callee").and_then(|v| v.as_str()).unwrap_or("")` extracts from JSON values that were just constructed 4 lines above. The `unwrap_or("")` is safe since the values are guaranteed to be strings (they were just serialized). However, this pattern constructs JSON objects and then immediately parses them back — a sign of unnecessary serialization/deserialization. Not a robustness issue per se, but the indirection makes the deduplication logic harder to reason about.
- **Suggested fix:** Use a HashSet of callee names directly instead of serializing to JSON and parsing back.

## Batch 2: Extensibility

#### 1. `lang_extension()` in diff.rs duplicates Language knowledge outside the registry
- **Difficulty:** easy
- **Location:** `src/diff.rs:219-230`
- **Description:** `lang_extension()` is a standalone match arm mapping language names to file extensions (`"rust" => "rs"`, `"python" => "py"`, etc.). This duplicates the mapping already in each `LanguageDef.extensions` in the language registry. Adding a new language requires updating this function separately, and the `_ => lang` fallback silently produces wrong results for languages where name != extension (like "rust" for ".rs" or "python" for ".py"). The function is only used in `semantic_diff` for filtering by language.
- **Suggested fix:** Replace with a registry lookup: `fn lang_extension(lang: &str) -> &str { REGISTRY.get(lang).and_then(|d| d.extensions.first()).copied().unwrap_or(lang) }`. This eliminates the duplication and auto-supports new languages.

#### 2. `extract_body_keywords` stopword lists are hardcoded per-language match arms
- **Difficulty:** medium
- **Location:** `src/nl.rs:609-669`
- **Description:** `extract_body_keywords()` has a `match language` with 7 arms, each containing a hardcoded `&[&str]` stopword list. Adding a new language requires adding a new match arm with a curated stopword list. The stopword lists aren't part of `LanguageDef`, so the language registry doesn't capture them. This is the only function in `nl.rs` that exhaustively matches on Language — `extract_return_nl` also matches but has a reasonable `_ =>` fallback for most patterns.
- **Suggested fix:** Add an optional `stopwords: &'static [&'static str]` field to `LanguageDef` and populate it in each language module's `definition()`. Then `extract_body_keywords` can do `language.def().stopwords` instead of matching.

#### 3. `structural.rs` pattern matchers have hardcoded Language match arms with no fallback strategy
- **Difficulty:** medium
- **Location:** `src/structural.rs:76-156` (`matches_error_swallow`, `matches_async`, `matches_mutex`, `matches_unsafe`)
- **Description:** Four structural pattern functions match on `Some(Language::Rust)`, `Some(Language::Python)`, etc. with an `_ =>` fallback that uses generic heuristics. Adding a new language (e.g., SQL) means the new language silently gets generic pattern matching, which may miss language-specific patterns (like SQL's `BEGIN...EXCEPTION WHEN OTHERS THEN NULL; END;` for error swallowing). This isn't a bug — the fallback works — but it's a hidden "new language checklist" item that's easy to miss because there's no compiler warning.
- **Suggested fix:** Low priority. Document in a "new language checklist" file or comment in `structural.rs` that new languages should audit these pattern matchers for language-specific heuristics. A `LanguageDef` field for structural patterns would be overengineering.

#### 4. MCP tool JSON schemas have 3 hardcoded language enum arrays
- **Difficulty:** easy
- **Location:** `src/mcp/tools/mod.rs:56,241,284`
- **Description:** The `cqs_search`, `cqs_diff`, and `cqs_similar` tool schemas each have `"enum": ["rust", "python", "typescript", "javascript", "go", "c", "java"]` hardcoded in JSON. Adding a new language requires updating 3 separate inline JSON blocks. The language registry already has `REGISTRY.all()` which could generate this list dynamically.
- **Suggested fix:** Generate the enum array from the registry at startup: `let lang_enum: Vec<String> = REGISTRY.all().map(|d| d.name.to_string()).collect();` and insert into each schema. Or define a `fn language_enum_schema() -> Value` helper and call it in all 3 places.

#### 5. `extract_return_nl` requires a new match arm for each language
- **Difficulty:** medium
- **Location:** `src/nl.rs:447-601`
- **Description:** `extract_return_nl()` has exhaustive match arms for all 7 languages with language-specific return type extraction logic. Unlike `extract_body_keywords` (stopwords can be data-driven), return type extraction is genuinely different per language (Rust uses `->`, Python uses `->` before `:`, Go uses position after `)`, C/Java put return type before function name). Adding a new language requires writing a new extraction case. No fallback — languages without a match arm return `None` (no return type info in NL description).
- **Suggested fix:** Low severity. The function is already well-structured with clear patterns. Consider adding a `return_type_hint: Option<fn(&str) -> Option<String>>` to `LanguageDef` for languages that want custom extraction, with the default being `None` (no extraction). This would let language modules self-contain their NL extraction logic.

#### 6. `ChunkSummary::from(ChunkRow)` defaults to `Language::Rust` on parse failure
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:122-134`
- **Description:** When loading chunks from the database, if the stored language string fails to parse, `ChunkSummary` defaults to `Language::Rust`. This was noted in the triage but the extensibility concern is different: when a new language is added and the user has an index from before that language was supported, chunks stored with the new language name will parse fine. But if a chunk was stored with a now-removed language, it silently becomes Rust. The warning is logged but the data is wrong — search results show Rust syntax highlighting and NL generation uses Rust patterns for what might be Python code.
- **Suggested fix:** Consider a `Language::Unknown` variant or return an error and skip the chunk rather than silently misattributing it.

#### 7. `cqs_batch` tool only supports 6 of 20 tools — no programmatic discovery
- **Difficulty:** medium
- **Location:** `src/mcp/tools/batch.rs:27-37`
- **Description:** The batch tool has a hardcoded match for 6 tools (`search`, `callers`, `callees`, `explain`, `similar`, `stats`). Adding a new MCP tool requires remembering to add it to the batch dispatcher too. The 14 unsupported tools (`read`, `context`, `impact`, `trace`, `test_map`, `dead`, `gc`, `gather`, `diff`, `add_note`, `update_note`, `remove_note`, `audit_mode`, `batch`) can't be batched. The error message lists the valid tools but doesn't match the tool list in `handle_tools_call`. This was partially noted in the API Design findings but the extensibility angle is new: every new tool requires updating both `handle_tools_call` and `tool_batch`.
- **Suggested fix:** Either (a) route batch through `handle_tools_call` directly (batch wraps the existing dispatcher), or (b) at minimum add `read`, `context`, `impact`, `trace`, `test_map`, `dead`, `gc`, `gather` since these are all read-only. Option (a) eliminates the maintenance burden entirely.

#### 8. Adding a new CLI command requires changes in 3 places with no compiler guidance
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs` (Commands enum ~line 97), `src/cli/mod.rs` (run_with match ~line 192), `src/cli/commands/mod.rs`
- **Description:** Adding a CLI command requires: (1) add variant to `Commands` enum, (2) add match arm in `run_with()`, (3) create handler in `src/cli/commands/`. The `Commands` enum is exhaustively matched, so the compiler catches missing arms in `run_with()` — that's good. But there's no link between the command module and the dispatch; you could create a handler file and forget to wire it. The real friction is that the `Commands` enum and `run_with()` are in the same file but grow independently — `run_with` is already ~60 lines of match arms.
- **Suggested fix:** Low severity — the compiler catches the critical case (missing match arm). Document the 3-step process in a comment above `Commands`.

## Batch 2: Algorithm Correctness

#### 1. `gather.rs` sorts by file order then truncates, discarding highest-scored chunks
- **Difficulty:** medium
- **Location:** `src/gather.rs:147-150`
- **Description:** After BFS expansion, `gather()` sorts chunks by file path and line number (reading order) and then truncates to `opts.limit`. This means the *limit* highest-scored chunks are not retained — instead, the first *limit* chunks in file order are. A high-relevance function at `src/z.rs:100` (score 0.9) would be discarded in favor of a low-relevance function at `src/a.rs:1` (score 0.1) simply because of alphabetical file ordering. The truncation should happen by score first, then the retained set can be sorted for display.
- **Suggested fix:** Sort by score descending, truncate to limit, *then* re-sort the survivors by file+line for display order:
  ```rust
  chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
  chunks.truncate(opts.limit);
  chunks.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));
  ```

#### 2. `diff.rs` has a duplicate cosine_similarity that doesn't match `math.rs`
- **Difficulty:** easy
- **Location:** `src/diff.rs:62-74`
- **Description:** `diff.rs` defines its own `cosine_similarity` function that is a full dot/norm/denom implementation, while the rest of the codebase uses `crate::math::cosine_similarity` which: (1) validates dimensions against `EMBEDDING_DIM`, (2) uses SIMD via `simsimd`, (3) returns `Option<f32>` for error handling, (4) filters non-finite results. The diff version accepts any-length vectors, does no NaN check, returns 0.0 on error (not `None`), and is ~4x slower without SIMD. If embeddings are corrupted or wrong-dimension, `math.rs` would return `None` (skip the pair), but `diff.rs` silently returns 0.0 (marks as "maximally modified").
- **Suggested fix:** Replace with `crate::math::cosine_similarity(a, b).unwrap_or(0.0)` or better, propagate the `None` as a separate "can't compare" case (which it already handles via the `(None, _) | (_, None)` arm).

#### 3. `gather.rs` BFS uses decayed score from arbitrary parent, not best parent
- **Difficulty:** medium
- **Location:** `src/gather.rs:125-129`
- **Description:** During BFS expansion, when a neighbor is first encountered, its score is computed as `base_score * 0.8^(depth+1)` where `base_score` comes from whichever parent happened to dequeue first. BFS doesn't guarantee the highest-scoring parent dequeues first — a low-score seed at depth 0 might discover node X before a high-score seed does. Once X is in `name_scores`, its score is fixed and won't be updated even if a higher-scoring path exists. For example: seed A (score=0.9) calls B calls X. Seed C (score=0.3) calls X directly. If C dequeues before A->B, X gets score 0.3*0.64=0.19 instead of 0.9*0.64*0.8=0.46.
- **Suggested fix:** Either update `name_scores` if a higher score path is found (`if new_score > existing_score`), or sort the seed queue by descending score before starting BFS.

#### 4. `impact.rs` test search depth is hardcoded to 5, ignoring user's `depth` parameter
- **Difficulty:** easy
- **Location:** `src/mcp/tools/impact.rs:58`
- **Description:** The user can pass `depth` (clamped to 1-10) to control transitive caller search depth. But the "find tests via reverse BFS" section at line 58 hardcodes `if d >= 5 { continue; }` regardless of the user's depth parameter. If a user requests `depth=10` to find distant callers, the test search still stops at depth 5. If they request `depth=2`, the test search wastefully explores to depth 5. The test search depth should be at least as deep as the user's `depth` parameter.
- **Suggested fix:** Change `if d >= 5` to `if d >= depth.max(5)` to honor the user's depth while keeping the minimum at 5.

#### 5. `search_reference_by_name` applies threshold filter before weight, inconsistent with `search_reference`
- **Difficulty:** easy
- **Location:** `src/reference.rs:89-95` vs `src/reference.rs:75-85`
- **Description:** `search_reference` (semantic) applies weight first, then filters: `r.score *= weight; results.retain(|r| r.score >= threshold)`. But `search_reference_by_name` filters by `r.score * weight >= threshold` before applying weight, then applies weight. While mathematically equivalent for the threshold check, the code pattern is inconsistent and fragile. More importantly, `search_by_name` returns scores from an entirely different scale (0.5/0.7/0.9/1.0 for FTS relevance) vs semantic search (0.0-1.0 cosine similarity). The same threshold value (e.g., 0.3) means very different things for the two search modes, and the weight multiplication interacts differently with the two scoring scales.
- **Suggested fix:** Make the code consistent: apply weight first, then filter (matching `search_reference`). Consider whether the FTS scores need a different default threshold when used with references.

#### 6. `diff.rs` language filter matches by file extension, misses stored language field
- **Difficulty:** easy
- **Location:** `src/diff.rs:100-113`
- **Description:** The language filter uses `c.origin.ends_with(&format!(".{}", lang_extension(lang)))` to match chunks by language. This has two problems: (1) it misses files with compound extensions like `.test.ts`, `.d.ts`, `.config.js` where the ending matches incorrectly; (2) it duplicates the extension-to-language mapping from `src/language/mod.rs` into `lang_extension()`. Meanwhile, the chunks already have a language stored in the database (the `ChunkIdentity` struct just doesn't include it). Using the stored language would be more reliable and eliminate the duplicate mapping.
- **Suggested fix:** Add a `language` field to `ChunkIdentity` (loaded from the chunks table in `all_chunk_identities()`), and filter by `c.language == lang` instead of file extension matching.

#### 7. `get_callees_full` matches by function name only, returns callees from all overloads
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:227-244`
- **Description:** `get_callees_full(caller_name)` queries `WHERE caller_name = ?1` from `function_calls`. If two functions in different files share a name (common in Go, Java, and even Rust across modules), this returns callees from ALL of them. For example, `get_callees_full("new")` returns callees from every `new()` across the project. The `function_calls` table has a `file` column that could disambiguate, but none of the callers pass file context. This affects `cqs_explain`, `cqs_context`, `cqs_trace`, and `cqs_gather` — all of which resolve to a specific chunk with a known file but then query callees by name alone.
- **Suggested fix:** Add an optional `file` parameter: `get_callees_full(caller_name: &str, file: Option<&Path>)` and add `AND file = ?2` when provided. Update MCP tool call sites to pass the resolved file.

#### 8. `find_dead_code` trait impl detection uses content heuristic that false-positives on `for` in body
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:367-372`
- **Description:** The trait implementation exclusion checks `chunk.content.contains(" for ") && chunk.chunk_type == Method`. This false-positives on any method whose body contains " for " — e.g., a `for` loop, a comment like "// iterate for each item", or a string literal. It also misses: (1) Rust trait impls where `for` is on a different line than the method, (2) Go interface implementations, (3) Java `@Override` methods. A method `fn process(&self) { for item in list { ... } }` would be incorrectly excluded from dead code results.
- **Suggested fix:** Check the signature or the parent chunk's signature for `impl.*for` pattern rather than the method content. Alternatively, add a `parent_signature` field or `is_trait_impl` boolean during parsing for reliable detection.

#### 9. `search_unified_with_index` note slot calculation allows notes to exceed intended 40%
- **Difficulty:** easy
- **Location:** `src/search.rs:588-591`
- **Description:** The code calculates `reserved_code = code_count.min(min_code_slots)` where `min_code_slots = ((limit * 3) / 5).max(1)`. Then `note_slots = limit.saturating_sub(reserved_code)`. If code search returns fewer results than min_code_slots (e.g., limit=10, min_code_slots=6, but only 2 code results), then reserved_code=2 and note_slots=8. Notes get 80% of slots instead of the intended 40% max. The comment says "60% code reserve" but only reserves as many code slots as there are actual code results. This may be intentional (fill empty code slots with notes) but contradicts the documented behavior.
- **Suggested fix:** If the 60% code guarantee matters, use `let note_slots = limit.saturating_sub(min_code_slots);` regardless of actual code count. If the current behavior is intentional, update the comment to reflect it: "60% code reserve when available, notes fill remaining slots."

#### 10. `BoundedScoreHeap` could get stuck if NaN leaks into min position
- **Difficulty:** easy
- **Location:** `src/search.rs:181-193`
- **Description:** `BoundedScoreHeap::push` compares `score > *min_score` to decide whether to insert. If a NaN ever enters the heap as the minimum element, the comparison `score > NaN` is always false, and no new items can be inserted — the heap becomes permanently stuck. The current code is safe because `cosine_similarity` filters NaN via `is_finite()`, but the heap has no self-defense against this. If a future code path bypasses the cosine_similarity filter, the heap silently produces wrong results.
- **Suggested fix:** Add `if score.is_nan() { return; }` at the top of `push()` as defensive guard.

#### 11. `cosine_similarity` in `math.rs` is actually dot product, not cosine similarity
- **Difficulty:** easy
- **Location:** `src/math.rs:15-28`
- **Description:** The doc comment correctly says "Cosine similarity for L2-normalized vectors (just dot product)" — this is mathematically sound for normalized vectors. However, the function is called from the entire codebase as the general-purpose similarity measure. If the embedding model is ever changed to one that doesn't L2-normalize outputs, all similarity computations silently become dot products (unbounded, not in [-1,1]), producing wrong search rankings. The `diff.rs` duplicate already handles this correctly with normalization. No production bug today (E5 normalizes), but a latent correctness hazard.
- **Suggested fix:** Add `debug_assert!` that inputs are approximately unit-norm: `debug_assert!((a.iter().map(|x| x*x).sum::<f32>() - 1.0).abs() < 0.1, "Expected L2-normalized");` Catches misuse in development.

#### 12. `node_letter` in trace.rs generates confusing Mermaid IDs after 26 nodes
- **Difficulty:** easy
- **Location:** `src/mcp/tools/trace.rs:215-221`
- **Description:** `node_letter(i)` generates `A`-`Z` for 0-25, then `A1`, `B1` etc. for 26+. The naming scheme is confusing: `A` and `A1` look like parent/child, not sequential peers. At 26*26+26=702 nodes, `node_letter` generates `A27` which is fine technically but doesn't follow standard base-26 conventions. While call chains >26 nodes are rare in practice, the tool has `max_depth` up to 50.
- **Suggested fix:** Use `N0`, `N1`, `N2`... which scales indefinitely and is unambiguous.

## Batch 2: Platform Behavior

#### 1. Watch mode notes path comparison fails with non-canonical event paths
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:77,97`
- **Description:** Watch mode constructs `notes_path = root.join("docs/notes.toml")` at line 77 and compares it with `path == notes_path` at line 97. The `root` comes from `find_project_root()` which returns `std::env::current_dir()` — not canonicalized. On macOS, the watcher can return paths through `/private/var/...` while `current_dir()` returns `/var/...` (symlink). On Windows/WSL, the watcher may return UNC paths (`\\?\C:\...`) while `root` has regular paths. The `==` comparison fails, notes changes are silently ignored and fall through to extension checking where `.toml` is not a supported code extension, so it's dropped entirely. Same issue applies to the `path.starts_with(&cq_dir)` check at line 92 — could fail to skip `.cq/` events.
- **Suggested fix:** Canonicalize `root` at startup: `let root = find_project_root().canonicalize().unwrap_or_else(|_| find_project_root());` and apply `strip_unc_prefix` on Windows. Or use `path.ends_with("docs/notes.toml")` as a more robust check.

#### 2. `process_exists` on Windows shells out to `tasklist` — slow and fragile
- **Difficulty:** medium
- **Location:** `src/cli/files.rs:27-34`
- **Description:** The Windows `process_exists` implementation spawns `tasklist /FI "PID eq N" /NH` and string-matches the output. This has several problems: (1) It's slow (~200ms per invocation vs microseconds for the Unix `kill(pid, 0)` check). (2) `tasklist` output format varies by locale — non-English Windows may format differently. (3) String matching `contains(&pid.to_string())` can false-positive: PID "12" matches in output containing PID "123" or "1234". (4) The function is called in the lock recovery path, so it blocks indexing when a stale lock is detected.
- **Suggested fix:** Use the `windows-sys` crate: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)` returns null if the process doesn't exist — instant and locale-independent. Falls back to the current tasklist approach if the API call fails.

#### 3. `cmd_init` prints "cq" instead of "cqs" in user-facing messages
- **Difficulty:** easy
- **Location:** `src/cli/commands/init.rs:11,19,57`
- **Description:** Line 11 doc comment says "Initialize cq", line 19 prints "Initializing cq...", and line 57 prints "Run 'cq index' to index your codebase." All should say "cqs". This was caught for other files in the Documentation and Error Handling batches but `init.rs` was missed. Users following the instructions will get "command not found".
- **Suggested fix:** Change all three occurrences to use "cqs".

#### 4. `note.rs` atomic write via `rename` can fail with `EXDEV` on overlay/Docker filesystems
- **Difficulty:** easy
- **Location:** `src/note.rs:144-146`
- **Description:** `rewrite_notes_file` creates a temp file with `notes_path.with_extension("toml.tmp")` and renames it to `notes_path`. The temp file is in the same directory, so `rename()` should work. However, on overlay filesystems (Docker bind mounts, some NFS/FUSE), the temp and target can be on different mount points despite appearing co-located, causing `rename()` to fail with `EXDEV` (cross-device link). The error from the bare `?` gives no path context. The HNSW persist code (`src/hnsw/persist.rs:211-226`) has the same pattern.
- **Suggested fix:** Add a copy+delete fallback: `std::fs::rename(&tmp_path, notes_path).or_else(|e| if e.raw_os_error() == Some(18 /* EXDEV */) { std::fs::copy(&tmp_path, notes_path)?; std::fs::remove_file(&tmp_path) } else { Err(e) })?;` At minimum, add `.map_err()` with path context.

#### 5. Watch mode `strip_prefix` on event paths fails when watcher returns differently-cased paths
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:110`
- **Description:** `path.strip_prefix(&root)` at line 110 converts watcher event paths to relative paths for indexing. If the watcher returns paths that don't match `root` exactly (different case on case-insensitive filesystems, or symlink resolution differences), `strip_prefix` returns `Err` and the file change is silently ignored via the `if let Ok(rel)` guard. On Windows, `C:\Projects\cq` vs `c:\projects\cq` would fail. On macOS, paths through `/private/var` vs `/var` would fail. Related to finding #1 but affects code file detection, not just notes.
- **Suggested fix:** Canonicalize `root` at startup (as in #1). The root cause is `find_project_root()` not canonicalizing (#6).

#### 6. `find_project_root` doesn't canonicalize, causing path mismatches downstream
- **Difficulty:** medium
- **Location:** `src/cli/config.rs:14-52`
- **Description:** `find_project_root()` returns a non-canonical path from `std::env::current_dir()`. This path is used as the base for watch mode (#1, #5), for `enumerate_files` (which canonicalizes internally at `src/lib.rs:207` but callers use the un-canonicalized root for display and lock paths), and for MCP server project root. When the CWD contains symlinks, the root and internally-canonicalized paths diverge. `enumerate_files` canonicalizes root at entry, but callers like watch mode use the original non-canonical root for `strip_prefix` and path comparisons.
- **Suggested fix:** Canonicalize in `find_project_root()`: `let canonical = current.canonicalize().unwrap_or_else(|_| current.to_path_buf()); cqs::strip_unc_prefix(canonical)`. Fixes root cause for #1 and #5.

#### 7. HNSW temp directory cleanup uses `remove_dir` instead of `remove_dir_all`
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:229`
- **Description:** `let _ = std::fs::remove_dir(&temp_dir);` silently fails if the temp directory isn't empty. On Windows, files recently written may still have OS-level handles (antivirus scanners, search indexers). If the rename loop at lines 213-226 fails partway, some files remain in the temp dir. `remove_dir` only removes empty directories, so cleanup fails. The pre-cleanup at line 124-132 correctly uses `remove_dir_all`. Between saves, orphaned temp dirs with partial HNSW data accumulate.
- **Suggested fix:** Change `remove_dir` to `remove_dir_all` for consistency with the pre-cleanup path.

#### 8. `refs_dir()` returns `None` in minimal environments with unhelpful error
- **Difficulty:** easy
- **Location:** `src/reference.rs:163-165`
- **Description:** `refs_dir()` calls `dirs::data_local_dir()` which returns `None` if the platform's local data directory can't be determined (`$XDG_DATA_HOME` and `$HOME` both unset, or minimal Docker containers). When `refs_dir()` returns `None`, `cmd_ref_add` bails with "Could not determine reference storage directory" — no guidance on WHY or how to fix. Users in Docker/CI environments hit this with no way forward.
- **Suggested fix:** Improve the error: "Could not determine reference storage directory. Ensure $HOME is set (Linux/macOS) or $LOCALAPPDATA (Windows)." Or fall back to `.cq/refs/` in the project directory.

#### 9. `mmap_size` pragma value assumes 64-bit address space
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:150-152`
- **Description:** `PRAGMA mmap_size = 268435456` (256MB) assumes 64-bit address space. On 32-bit platforms (~3GB usable), this consumes a significant chunk. SQLite handles this gracefully (falls back to read if mmap fails), so it won't crash, but could cause unexpected memory pressure on 32-bit ARM. The project targets 64-bit primarily but doesn't document this.
- **Suggested fix:** No code change required — SQLite degrades gracefully. Document "64-bit recommended" in README system requirements.

## Batch 2: Test Coverage

#### 1. 13 of 20 MCP tools have zero integration tests
- **Difficulty:** medium
- **Location:** `tests/mcp_test.rs`
- **Description:** Only 7 MCP tools have integration tests: `cqs_read`, `cqs_search`, `cqs_add_note`, `cqs_callers`, `cqs_callees`, `cqs_audit_mode`, `cqs_stats`. The following 13 tools have no tests at all: `cqs_update_note`, `cqs_remove_note`, `cqs_explain`, `cqs_similar`, `cqs_trace`, `cqs_impact`, `cqs_test_map`, `cqs_dead`, `cqs_gc`, `cqs_gather`, `cqs_context`, `cqs_diff`, `cqs_batch`. These are all invokable by LLM clients via the MCP protocol — untested means any regression in argument parsing, response shape, or error handling is invisible until a user hits it.
- **Suggested fix:** Add at minimum smoke tests for each tool: call with valid arguments and verify response has no error. Priority by usage: `cqs_explain`, `cqs_context`, `cqs_gather`, `cqs_dead`, `cqs_batch` (most commonly invoked by LLMs).

#### 2. `semantic_diff` has no integration test — only unit tests for helpers
- **Difficulty:** medium
- **Location:** `src/diff.rs:82-215`
- **Description:** `src/diff.rs` tests cover `cosine_similarity`, `ChunkKey`, and `lang_extension` (8 unit tests), but the main `semantic_diff()` function has zero tests. It performs: store queries for all chunk identities, filtering by language, matching by composite key, embedding comparison with similarity threshold, sorting modified results. None of these integration paths are tested. The function is called by `cqs_diff` MCP tool which also has no tests.
- **Suggested fix:** Create a test that sets up two stores with known chunks (some shared, some different), calls `semantic_diff()`, and verifies the added/removed/modified/unchanged counts.

#### 3. `gather()` function has no integration test — only unit tests for helpers
- **Difficulty:** medium
- **Location:** `src/gather.rs:78-174`
- **Description:** `gather.rs` tests cover `GatherDirection` parsing, `GatherOptions` defaults, and `get_neighbors()` behavior (7 unit tests). But the main `gather()` function — which orchestrates seed search, BFS expansion, chunk lookup, deduplication, sorting, and truncation — has zero tests. The BFS expansion capping at `MAX_EXPANDED_NODES=200` is untested. The score decay logic (`0.8^depth`) is untested. The deduplication by chunk ID is untested.
- **Suggested fix:** Create a test with a store containing chunks with known call graph edges, call `gather()` with different `expand_depth` values, and verify the returned chunks include expanded neighbors with decaying scores.

#### 4. `Store::find_dead_code()` has zero tests
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:336-415`
- **Description:** `find_dead_code()` is the core of the `cqs_dead` tool. It performs a complex SQL query (chunks NOT IN callee_name), then applies multiple exclusion heuristics: skip `main`, skip test functions, skip test files, skip trait implementations, skip `#[no_mangle]`. None of these heuristics are tested. If an exclusion regex changes (e.g., the `impl <Type> for` pattern at line ~394), false positives could flood the dead code report.
- **Suggested fix:** Create a store with: (1) a called function, (2) an uncalled function, (3) `main`, (4) a test function, (5) a trait impl. Verify `find_dead_code` returns only #2 as confident dead code.

#### 5. `Store::all_chunk_identities()` and `get_chunk_with_embedding()` have zero tests
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:478-525`
- **Description:** `all_chunk_identities()` is used by `semantic_diff()` and `get_chunk_with_embedding()` is used by `diff` and `similar`. Neither has any test — unit or integration. `all_chunk_identities()` returns `ChunkIdentity` structs with `window_idx` and `parent_id` fields extracted from the database — incorrect column mapping would silently return wrong data. `get_chunk_with_embedding()` reconstructs an `Embedding` from raw bytes — corrupt byte data returns `None` with a log, but the happy path is untested.
- **Suggested fix:** For `all_chunk_identities()`: insert a chunk, call the method, verify the returned identity matches. For `get_chunk_with_embedding()`: insert a chunk with a known embedding, call the method, verify the embedding values match.

#### 6. `Store::get_call_graph()` has zero tests
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:269-291`
- **Description:** `get_call_graph()` builds forward and reverse adjacency lists from the `function_calls` table. It's used by `gather`, `trace`, `impact`, and `test_map`. No test verifies that the forward/reverse maps are correctly populated. The `store_calls_test.rs` integration tests cover `upsert_function_calls`, `get_callers_full`, and `get_callees_full`, but not `get_call_graph()` which returns a different data structure (`CallGraph` with `HashMap<String, Vec<String>>` instead of `Vec<CallerFull>`).
- **Suggested fix:** Insert some function call edges, call `get_call_graph()`, verify forward and reverse maps contain the expected entries.

#### 7. `search_reference()` and `search_reference_by_name()` have zero tests
- **Difficulty:** medium
- **Location:** `src/reference.rs:72-122`
- **Description:** `reference.rs` has 12 tests covering `merge_results()`, `validate_ref_name()`, `ref_path()`, and `load_references()` with a missing path. But `search_reference()` and `search_reference_by_name()` — the functions that apply weight multipliers and post-weight threshold filtering — have no tests. The post-weight `retain` filter (line 92) was a v0.5.3 audit fix; if it regresses, weighted results that should be filtered out would leak through.
- **Suggested fix:** Create a `ReferenceIndex` with a known store and weight, call `search_reference()`, verify scores are multiplied and threshold filtering is applied after weighting.

#### 8. CLI commands `ref`, `watch`, `project`, `explain`, `context`, `trace`, `impact`, `test-map`, `dead` have no integration tests
- **Difficulty:** hard
- **Location:** `tests/cli_test.rs`
- **Description:** `cli_test.rs` covers: `help`, `version`, `init`, `stats`, `index`, search, `completions`, `doctor`, `callers`, `callees` (19 tests total). Missing CLI commands: `ref add/update/remove/list`, `watch`, `project register/remove/list/search`, `explain`, `context`, `trace`, `impact`, `test-map`, `dead`, `serve`. These are 14 untested commands. The `ref` and `project` commands involve filesystem state (reference indexes, project registry) and are high-value for correctness.
- **Suggested fix:** Priority additions: `ref add` + `ref list` (exercises config read/write), `dead` (exercises call graph), `explain` (exercises callers/callees/similar). Each test: set up project, index, run command, verify output.

#### 9. No tests for `Config::override_with` reference weight boundary behavior
- **Difficulty:** easy
- **Location:** `src/config.rs:123-142`
- **Description:** `Config` tests cover scalar field merging and reference replacement by name. But there's no test for weight boundary behavior: a reference with `weight = 0.0` or `weight = 1.5` passes through `override_with` without validation. The v0.5.3 fresh-eyes audit caught that weight >1.0 can amplify reference scores. The fix was at the CLI layer (`ref add` validates), but `Config::load()` still accepts any weight value from a hand-edited config file. A test that catches this would prevent regression.
- **Suggested fix:** Add test: config with `weight = 1.5` loads without error. Decide if `Config::load()` should clamp/validate weights or if the current CLI-only validation is sufficient. At minimum, document the design decision.

#### 10. MCP `tool_search` response format divergence between name_only and semantic paths is untested
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:172-342`
- **Description:** When `name_only=true`, `tool_search` returns a bare JSON array. When `name_only=false`, it returns `{"results": [...], "query": "...", "total": N}`. Both paths are invoked via the same `cqs_search` tool. The MCP test `test_cqs_search_name_only_mode` only checks that the tool returns successfully with no error — it doesn't verify the response shape. An LLM client that parses the response would break if the format diverged further.
- **Suggested fix:** In `test_cqs_search_name_only_mode`, parse the response content and verify it's an array (not wrapped in `{"results": ...}`). In semantic search tests, verify the `{"results": [...], "total": N}` wrapper.

#### 11. `validate_query_length` max length boundary not tested
- **Difficulty:** easy
- **Location:** `src/mcp/validation.rs:12-24`
- **Description:** `validate_query_length` rejects queries longer than `MAX_QUERY_LENGTH` (8192 bytes). The tests cover empty/whitespace rejection and normal strings, but don't test the boundary. A query of exactly 8192 bytes should pass; 8193 should fail. Since this is a security-relevant input validation function, boundary testing matters.
- **Suggested fix:** Add: `assert!(validate_query_length(&"a".repeat(8192)).is_ok()); assert!(validate_query_length(&"a".repeat(8193)).is_err());`

#### 12. `parse_duration` overflow with very large numbers not tested
- **Difficulty:** easy
- **Location:** `src/mcp/validation.rs:27-99`
- **Description:** `parse_duration("999999999999999999h")` will attempt `i64::parse` on a huge number. The function has a 24-hour cap (line 90), but the intermediate `hours * 60` multiplication happens before the cap check (line 46). For a value like `i64::MAX / 60 + 1`, the multiplication overflows silently in release mode (wraps in debug mode). The cap would then compare against a wrapped value.
- **Suggested fix:** Add test: `assert!(parse_duration("999999999999999999h").is_err()); assert!(parse_duration("99999999m").is_err());` and consider checking the cap before multiplication or using `checked_mul`.

#### 13. `SearchFilter::validate()` is only called in 1 of 4 construction sites
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:335-380`, `src/mcp/tools/search.rs:70-72`, `src/mcp/tools/explain.rs:65-73`, `src/mcp/tools/similar.rs:88-96`
- **Description:** `SearchFilter` has a `validate()` method that checks `name_boost` is in [0.0, 1.0], `note_weight` is in [0.0, 2.0], and `threshold` is non-negative. This is called in `tool_search` (search.rs:70-72) but NOT in `explain.rs`, `similar.rs`, or any other tool that constructs a `SearchFilter`. A `name_boost` of 5.0 passed to `cqs_explain` would bypass validation. The existing test `test_search_filter_valid_with_name_boost` tests the validator in isolation but doesn't verify it's actually called.
- **Suggested fix:** Either make `validate()` mandatory (call in a constructor or make fields private) or add tests that verify invalid parameters are rejected at the tool level for `cqs_explain` and `cqs_similar`.

## Batch 3: Performance

### Performance

#### 1. `semantic_diff` fetches embeddings one-by-one per matched pair (N+1 query)
- **Difficulty:** medium
- **Location:** `src/diff.rs:169-196`
- **Description:** The `semantic_diff` function iterates over `matched_pairs` and calls `get_chunk_with_embedding` individually for each source and target chunk. Each call is a separate SQL query wrapped in `block_on`. For a diff with 500 matched pairs, that's 1000 individual database round-trips. The function already bulk-loads all chunk identities at the start (lines 85-96), but then switches to per-row fetching for embeddings. This is the dominant cost of `cqs_diff` for non-trivial comparisons.
- **Impact:** For a 500-pair diff, ~1000 SQLite queries instead of 2 batch queries. Estimated 10-50x slower than batched fetching depending on SQLite cache state.
- **Suggested fix:** Collect all needed chunk IDs from `matched_pairs`, batch-fetch embeddings from source and target stores (using `get_embeddings_by_hashes` or a new `get_embeddings_by_ids` method), then look up per pair from an in-memory HashMap.

#### 2. `gather()` calls `search_by_name` individually for each BFS-expanded name (N+1 query)
- **Difficulty:** medium
- **Location:** `src/gather.rs:136-161`
- **Description:** After BFS expansion collects up to `MAX_EXPANDED_NODES` (200) names, the function iterates and calls `store.search_by_name(name, 1)` for each one. Each call is a separate FTS5 query wrapped in `block_on`. For a gather with 200 expanded names, that's 200 individual database round-trips. The BFS expansion itself (lines 108-130) is fast (in-memory call graph traversal), but the chunk lookup dominates wall time.
- **Impact:** For 200 expanded nodes, ~200 FTS queries. Each query is fast individually (~0.1ms), but the overhead of 200 `block_on` + SQL prepare cycles adds up to 50-200ms total.
- **Suggested fix:** Collect all expanded names into a Vec, then batch-query with a single `WHERE name IN (?, ?, ...)` or use FTS5 `OR` queries to fetch all matching chunks in one round-trip.

#### 3. `needs_reindex` called per-chunk instead of per-file during incremental indexing
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:284-297`
- **Description:** The non-force indexing path calls `store.needs_reindex(&abs_path)` for every chunk emitted by the parser. When a file produces N chunks (e.g., a file with 15 functions produces 15 chunks), `needs_reindex` is called 15 times for the same file. Each call does both `path.metadata()` (filesystem syscall) and a SQL query (`SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1`). The force path correctly deduplicates with `file_mtimes.contains_key()` (line 269), but the non-force path does not check this cache before calling `needs_reindex`.
- **Impact:** For a project with 3000 chunks across 300 files, ~2700 redundant `needs_reindex` calls (each involving a stat syscall + SQL query). On a full re-scan (no changes), this is the dominant indexing cost.
- **Suggested fix:** Add `if file_mtimes.contains_key(&c.file) { return file_mtimes[&c.file].is_some(); }` before calling `needs_reindex`, or restructure to group chunks by file before the filter step. The `file_mtimes` HashMap already exists but only gets populated when `needs_reindex` returns `Some`.

#### 4. `search_filtered` transfers all embeddings from SQLite even when most are filtered out
- **Difficulty:** hard
- **Location:** `src/search.rs:254-266`
- **Description:** The brute-force search path builds a SQL query that selects embeddings (`SELECT id, embedding FROM chunks`) with optional WHERE clauses for language/chunk_type, then `fetch_all` loads every matching row into memory before iterating. For unfiltered searches on a 10,000-chunk index, this transfers ~30MB of embedding data (10K x 3KB per 769-dim f32 vector) from SQLite into Rust memory in one shot. The `BoundedScoreHeap` (line 284) bounds result memory to O(limit), but the input side is still O(n). This is the expected behavior for brute-force — the issue is that when HNSW is unavailable (e.g., fresh index, corrupted HNSW), every search pays this cost.
- **Impact:** ~30MB memory spike per search on a 10K-chunk index. On large projects (50K+ chunks), this could be 150MB+ per query. This is the fallback path, so HNSW availability is the real mitigation.
- **Suggested fix:** Use streaming iteration instead of `fetch_all` — SQLite supports row-by-row fetching via `fetch(&self.pool)` which returns a `Stream`. Process each row through the scoring pipeline and feed into `BoundedScoreHeap` without materializing all rows. This bounds memory to O(limit) on both input and output sides.

#### 5. `get_call_graph` clones caller/callee strings for both forward and reverse maps
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:281-287`
- **Description:** The loop at line 281 destructures `(caller, callee)` from SQL rows, then clones `caller` to insert into the forward map (line 283) and clones `callee` to insert into the reverse map (line 285). For a project with 2000 call edges, this produces 4000 string clones (caller cloned for forward key + callee cloned for forward value + callee used as reverse key + caller used as reverse value — actually 2 clones per edge). The strings are typically short function names (~30 bytes), so total extra allocation is ~120KB. Low absolute impact but avoidable.
- **Impact:** ~120KB extra allocation for a 2000-edge graph. Marginal — this is a micro-optimization. Only worth addressing if call graph loading shows up in profiling.
- **Suggested fix:** Use `Rc<String>` or process rows in two passes (first build forward, then iterate forward to build reverse without extra clones). Alternatively, accept the cost — it's small and the code is readable.

#### 6. `search_by_candidate_ids` parses Language and ChunkType from strings per candidate row
- **Difficulty:** easy
- **Location:** `src/search.rs:448-470`
- **Description:** When the HNSW-guided search path fetches candidate chunks, each row's `language` and `chunk_type` string fields are parsed via `.parse()` (which calls `FromStr` and does string matching) to check against filter criteria. For 200 candidate rows with language+type filters, that's 400 string parse operations. The `Language` and `ChunkType` enums have ~7 and ~8 variants respectively, so each `.parse()` does linear string comparisons. The parsing itself is fast, but it's unnecessary — the SQL WHERE clause could filter these columns before fetching.
- **Impact:** ~400 string comparisons for a typical filtered search. Microseconds of CPU time — negligible in isolation. The real cost is transferring embedding bytes for rows that will be filtered out.
- **Suggested fix:** Push language/chunk_type filters into the SQL query (add to WHERE clause) rather than filtering post-fetch. This would also reduce data transfer by excluding non-matching embeddings. The `search_filtered` path already builds dynamic WHERE clauses for these filters (line 214-235).

#### 7. `normalize_for_fts` called 4 times per chunk during upsert with no caching
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:70-73`
- **Description:** During `upsert_chunks_batch`, each chunk calls `normalize_for_fts` separately on `name`, `signature`, `content`, and `doc`. The function does per-character processing with regex-based camelCase splitting. For a chunk with a 2KB content field, this involves character-by-character iteration plus regex matching. The `name` and `signature` fields are typically short (<100 bytes), but `content` can be significant. There's no caching or memoization of the normalization.
- **Impact:** For a batch of 32 chunks averaging 1KB content each, ~128KB of text goes through character-by-character processing + regex splitting. Total CPU: ~1-5ms per batch. Modest but multiplied across thousands of chunks during full indexing.
- **Suggested fix:** Low priority. The normalization is inherently per-field work. Could pre-compute during chunk construction in the parser stage rather than at insert time, moving the work out of the SQLite transaction. This would reduce transaction hold time.

## Batch 3: Security

#### 1. `parse_duration` integer overflow bypasses 24-hour cap via wrapping multiplication
- **Difficulty:** easy
- **Location:** `src/mcp/validation.rs:46`
- **Description:** `parse_duration("153722867280912931h")` parses the hours to `i64`, then computes `hours * 60` at line 46 before the 24-hour cap check at line 90. For values near `i64::MAX / 60`, the multiplication overflows silently in release mode (Rust wraps i64 in release). The wrapped result can be a small positive number that passes the `> MAX_MINUTES` check, producing a bogus `chrono::Duration`. Example: `i64::MAX / 60 + 1 = 153722867280912931`, `153722867280912931 * 60` wraps to a small value. The attacker can set arbitrary audit mode durations, although impact is limited to audit mode expiry timing.
- **Attack vector:** MCP client sends `cqs_audit_mode` with `expires_in: "153722867280912931h"`. Audit mode silently gets a wrong duration.
- **Suggested fix:** Use `checked_mul`: `let minutes = hours.checked_mul(60).ok_or_else(|| anyhow::anyhow!("Duration overflow"))?;` Or check `hours > 24` before multiplication.

#### 2. `tool_read_focused` bypasses path traversal protection — reads file content from database
- **Difficulty:** medium
- **Location:** `src/mcp/tools/read.rs:207-310`
- **Description:** `tool_read` (regular read) canonicalizes the path and validates it's within `project_root` (lines 32-45). But `tool_read_focused` takes a `focus` parameter, calls `resolve_target()` which searches the database by name, and returns `chunk.content` — the stored file content from the database. No path validation occurs because no disk read happens. This is NOT a path traversal vulnerability per se (the database only contains chunks from files that were indexed, which are already within the project). However, it means: (1) If files are removed from the project but not re-indexed, `tool_read_focused` still returns their content from the stale database. (2) The focused read path returns raw code content without the file-size check (10MB limit at line 48-56) since chunks are smaller by nature. The actual security risk is minimal — an LLM client already has access to the project root — but the asymmetric protection (regular read validates, focused read doesn't) could surprise security reviewers.
- **Suggested fix:** Add a comment documenting that focused reads return indexed content, not live disk content, and that path validation is not needed because the index only contains project files.

#### 3. `add_reference_to_config` uses `unwrap_or_default()` masking permission/corruption errors
- **Difficulty:** easy
- **Location:** `src/config.rs:184`
- **Description:** `std::fs::read_to_string(config_path).unwrap_or_default()` treats ALL read errors (permissions denied, I/O error, encoding error) identically to "file doesn't exist" (empty string). If a config file exists but is unreadable due to wrong permissions or disk corruption, this silently creates an empty config table. The subsequent `std::fs::write` at line 222 then overwrites the existing config file, destroying all references. Contrast with `remove_reference_from_config` at line 236 which properly matches on error kind. Previously noted in Error Handling batch #7 but the security angle is new: reference configs containing `path` and `source` fields pointing to external directories would be silently destroyed.
- **Attack vector:** If another process temporarily changes config file permissions (e.g., backup software, security scanner), the next `ref add` silently destroys the config.
- **Suggested fix:** Match on error kind: `match std::fs::read_to_string(config_path) { Ok(s) => s, Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(), Err(e) => return Err(e.into()) }`

#### 4. `process_exists` PID u32-to-i32 cast can send signal to wrong process group
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:23`
- **Description:** `libc::kill(pid as i32, 0)` casts u32 to i32. PIDs > `i32::MAX` (2,147,483,647) wrap to negative values. `kill(-N, 0)` checks process GROUP `N`, not process `N`. The lock file (`cq_dir/index.lock`) is user-writable plaintext — it contains just a PID number. If a user (or malicious process with write access to `.cq/`) writes a PID value > `i32::MAX` into the lock file, `process_exists` would call `kill` with a negative PID, checking a process group instead. If that process group exists, the stale lock recovery path thinks the process is alive and refuses to acquire the lock, causing a denial-of-service (indexing permanently blocked). Linux `pid_max` is capped at 4,194,304 on 64-bit, so legitimate PIDs never reach `i32::MAX`.
- **Attack vector:** Write `2147483648` to `.cq/index.lock`. Indexing becomes permanently blocked until manual lock file deletion.
- **Suggested fix:** Add bounds check: `if pid > i32::MAX as u32 { return false; }` before the `kill` call.

#### 5. `serve_http` network bind check is warning-only — CLI enforces but library doesn't
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:99-107`, `src/cli/commands/serve.rs:28`
- **Description:** `serve_http` (the library function) only warns when binding to non-localhost addresses — it does not enforce `--dangerously-allow-network-bind`. The enforcement happens at the CLI layer (`serve.rs:28`) which bails before calling `serve_http`. This is a defense-in-depth gap: any code that calls `serve_http` directly (integration tests, programmatic use, future transports) bypasses the network bind protection. The library function accepts any bind address silently. Additionally, `serve_http` also requires API key for non-localhost binds at the CLI layer (`serve.rs:51`) but has no such check itself — only a cosmetic warning.
- **Attack vector:** A programmatic caller using `cqs::serve_http(".", "0.0.0.0", 3000, None, false)` would expose the server to the network without authentication and no error.
- **Suggested fix:** Add the same checks from `serve.rs` into `serve_http` as defense-in-depth: bail if binding to non-localhost without an explicit `allow_network: bool` parameter.

#### 6. `sanitize_error_message` regex misses common path prefixes
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:196-211`
- **Description:** The Unix path regex only matches paths starting with `/home/`, `/Users/`, `/tmp/`, `/var/`, `/usr/`, `/opt/`, `/etc/`, `/mnt/`, `/root/`. Paths starting with `/run/`, `/srv/`, `/snap/`, `/proc/`, `/dev/`, `/sys/`, or custom mount points (e.g., `/data/`, `/nfs/`, `/media/`) pass through unsanitized. The Windows regex only matches `Users`, `Windows`, `Program Files` — missing `AppData`, `ProgramData`, `Temp`, and custom drives. The project root itself is stripped first (line 207), which covers the primary case. Remaining paths in error messages could leak system layout info to MCP clients.
- **Attack vector:** A carefully crafted MCP request that triggers an error involving `/run/`, `/srv/`, or other unmatch paths leaks the path to the client. Low impact for localhost-only service.
- **Suggested fix:** Use a broader pattern: `/[a-zA-Z0-9_./-]+` for Unix (any absolute path) and `[A-Za-z]:\\[^\s:]+` for Windows (any drive path). Or strip all absolute paths regardless of prefix.

#### 7. No rate limiting on MCP tool calls — resource exhaustion via rapid requests
- **Difficulty:** medium
- **Location:** `src/mcp/transports/http.rs:87-89`, `src/mcp/transports/stdio.rs:28`
- **Description:** The HTTP transport has a 1MB body limit but no rate limiting. The stdio transport processes requests as fast as they arrive. A malicious or misbehaving MCP client can flood the server with `cqs_search` requests that each trigger embedding computation (~50ms+ per query) and database queries. With the HTTP transport, concurrent POST requests all get processed. The embedding model holds GPU memory. Rapid concurrent embedding requests could exhaust GPU/CPU resources and make the system unresponsive. For the intended threat model (trusted local tool), this is low severity, but documented for completeness.
- **Attack vector:** Localhost HTTP client sends 100 concurrent `cqs_search` requests. Each triggers embedding + HNSW search. CPU/memory spikes.
- **Suggested fix:** Low priority. Consider adding a semaphore or request queue for embedding operations: `Arc<Semaphore>` with max 4 concurrent embeddings. Or add Tower rate limiting middleware for the HTTP transport.

#### 8. `tool_add_note` writes to `docs/notes.toml` without verifying path is within project
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:93`
- **Description:** `tool_add_note` constructs `notes_path = server.project_root.join("docs/notes.toml")` and writes directly to it. The path is hardcoded relative to project root, so traversal isn't possible through the note content. However, `server.project_root` is set from the MCP initialization and is not re-validated. If `project_root` itself is manipulated (e.g., symlinked), notes could be written outside the intended project. The risk is theoretical — `project_root` is set at server startup from the CLI argument, not from MCP client input. This is consistent with the threat model (trusted user sets project root).
- **Suggested fix:** No code change needed. Document that `project_root` is trusted and set at server startup, not by the MCP client.

#### 9. `config.rs:load_file` returns `None` for parse errors, masking TOML injection
- **Difficulty:** easy
- **Location:** `src/config.rs:115-118`
- **Description:** When `toml::from_str` fails, `load_file` logs a warning and returns `None`, causing the caller to use `Default::default()`. A malformed config file (e.g., a file with a TOML injection that breaks parsing) silently disables all config-based settings including reference weights and search thresholds. The user gets default behavior with no clear indication that their config was ignored. While this doesn't enable direct exploitation, it means a config file modified by a malicious actor to include parsing-breaking content effectively resets all security-relevant settings (reference weights, thresholds) to defaults. Already noted as P3 #1 in v0.5.3 triage (issue #264).
- **Suggested fix:** Already tracked in issue #264. Consider promoting priority since it affects config integrity.

#### 10. `is_localhost_origin` doesn't handle uppercase or mixed-case origins
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:199-219`
- **Description:** `is_localhost_origin` performs case-sensitive prefix matching against lowercase strings like `"http://localhost"`. Per RFC 6454, the scheme and host components of an origin are case-insensitive. A browser sending `Origin: HTTP://LOCALHOST:3000` or `Origin: http://Localhost` would be rejected by the origin validation, even though it's a legitimate localhost request. In practice, browsers normalize origins to lowercase, so this is unlikely to cause real issues. But non-browser MCP clients (curl, custom HTTP clients) might send non-normalized origins and get 403 errors.
- **Suggested fix:** Lowercase the origin before matching: `let origin_str = origin.to_str().unwrap_or("").to_lowercase();`

#### 11. HNSW checksum file could be used for limited path information leak
- **Difficulty:** hard
- **Location:** `src/hnsw/persist.rs:42-77`
- **Description:** `verify_hnsw_checksums` reads a `.hnsw.checksum` file and processes `ext:hash` lines. The `HNSW_EXTENSIONS` whitelist prevents path traversal (only `hnsw.graph`, `hnsw.data`, `hnsw.ids` extensions are accepted). However, the error message for a checksum mismatch includes `path.display()` (line 70-73) which reveals the full filesystem path to the HNSW files. This path is returned in `HnswError::ChecksumMismatch` and could propagate to MCP clients. The `sanitize_error_message` function in server.rs would catch paths starting with common prefixes, but not all paths (see finding #6). Low impact — HNSW files are always in `.cq/` within the project root, which is already stripped by sanitization.
- **Suggested fix:** No action needed — the project root is stripped by `sanitize_error_message`, and `.cq/` relative paths don't leak sensitive information.

#### 12. `cqs_read` note matching uses substring containment — broad match risk
- **Difficulty:** easy
- **Location:** `src/mcp/tools/read.rs:86-91`
- **Description:** Note matching for file context injection uses `path.contains(m)` where `m` is a note's mention string. A note mentioning `"mod"` would match ANY file path containing "mod" — `src/module/foo.rs`, `src/model.rs`, `src/mod.rs`, etc. The matching also checks `m == file_name` and `m == path`, but the `path.contains(m)` fallback is overly broad. This isn't a security vulnerability per se, but it means notes intended for one file can "leak" into the context of unrelated files, potentially causing LLM tools to act on irrelevant warnings or patterns. With audit mode, this is irrelevant (notes suppressed). Without it, a note warning about `"auth"` would appear on every file with "auth" in the path.
- **Suggested fix:** Tighten matching: require the mention to match a path segment boundary. For example, use `path.contains(&format!("/{}", m))` or `path.split('/').any(|seg| seg == m || seg.starts_with(m))` instead of bare `contains`.

## Batch 3: Resource Management

#### 1. MCP server holds HNSW + CAGRA in memory simultaneously during GPU upgrade
- **Difficulty:** medium
- **Location:** `src/mcp/server.rs:73-90`
- **Description:** When GPU is available, `McpServer::new` loads the HNSW index immediately, then spawns a background thread that opens a separate `Store` and streams all embeddings to build CAGRA. At peak: HNSW graph (~50-100MB for 50k chunks) + CAGRA's flat data copy (50k * 769 * 4 = ~150MB) + CAGRA index on GPU + background Store's runtime and pool. For 100k chunks, peak memory during CAGRA build is ~500MB above baseline. The old HNSW is dropped after swap but the peak matters for memory-constrained systems.
- **Suggested fix:** Document the peak memory behavior. Consider dropping HNSW before CAGRA build (accepting brute-force during build), or gate CAGRA build on available system memory via `sysinfo` check.

#### 2. Each `ReferenceIndex` creates its own Tokio runtime + SQLite pool — linear scaling with reference count
- **Difficulty:** medium
- **Location:** `src/reference.rs:15-24`, `src/store/mod.rs:55-58`
- **Description:** Each `ReferenceIndex` contains a full `Store`, which creates its own `tokio::Runtime` (1-4 OS threads) and `SqlitePool` (up to 4 connections per pool). With N references, the MCP server holds N+1 runtimes and N+1 pools. A user with 5 references: 6 runtimes (~24 OS threads), 6 pools (~24 SQLite connections), ~6 * 16MB page cache = ~96MB just in SQLite caches. All references are read-only — they could share a single runtime. This is the same underlying issue as RF1 (issue #204/#257) but the reference dimension makes it worse.
- **Suggested fix:** Add `Store::open_with_runtime(path, rt: &Runtime)` that accepts an external runtime. `McpServer::new` passes its runtime to reference stores. Reduces N+1 runtimes to 1.

#### 3. Pipeline creates two full `Embedder` instances eagerly — ~1GB combined model memory
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:345,489`
- **Description:** `run_index_pipeline` spawns GPU and CPU embedder threads that each call `Embedder::new()` / `Embedder::new_cpu()`, loading a separate ONNX session (~500MB model memory each). The CPU embedder exists as GPU fallback but on systems where GPU works reliably, the CPU thread sits idle with ~500MB allocated. The CPU thread also races on `parse_rx` via `select!` (line 498), potentially stealing work from the faster GPU even when no failures occur.
- **Suggested fix:** Make CPU embedder lazy — only call `Embedder::new_cpu()` after the first batch arrives on `fail_rx` or `parse_rx_cpu`. This defers ~500MB allocation until actually needed. On GPU-capable systems that never fail, CPU embedder never initializes.

#### 4. `all_chunk_identities()` loads all chunk metadata into memory — `cqs_diff` calls it twice
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:502-520`, `src/diff.rs:91-92`
- **Description:** `semantic_diff()` calls `all_chunk_identities()` on both source and target stores. Each call loads all rows (minus content/embeddings) via `fetch_all`. For 100k chunks, each `ChunkIdentity` is ~200 bytes = ~20MB. Then `diff.rs` builds two `HashMap<ChunkKey, &ChunkIdentity>` for lookup. Total for diff of two 100k-chunk stores: ~80MB. Not OOM-dangerous on typical machines but notable for mono-repos.
- **Suggested fix:** Document the O(n) memory. For very large repos, consider paginated diff processing by file. Low priority — diff is an infrequent manual operation.

#### 5. `count_vectors()` parses entire HNSW ID map JSON just to count entries
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:346-377`
- **Description:** `count_vectors()` reads the entire `.hnsw.ids` JSON file, parses it into `Vec<String>`, returns `.len()`. For 100k chunks, the IDs file is ~5MB JSON. Allocates ~10MB (string + parsed vec) just to return a count. Called by `cqs stats` and `cqs_stats` MCP tool. The SQLite `store.chunk_count()` already provides the same number.
- **Suggested fix:** Use `store.chunk_count()` instead of parsing the ID map file. Or count `","` occurrences in the raw string. Or store count in SQLite metadata during HNSW save.

#### 6. Watch mode embedder (~500MB) never released once initialized
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:85`
- **Description:** Watch mode uses `OnceCell<Embedder>` — once a file change triggers embedding, the ~500MB ONNX model stays in memory for the session lifetime. In long-running watch sessions with infrequent edits, this holds significant resources. The module docs (lines 3-17) already document this trade-off as intentional.
- **Suggested fix:** Already documented. For improvement: replace `OnceCell` with `Option<Embedder>` and drop after an idle timeout (e.g., 10 minutes of no changes). Re-init costs ~500ms on next change. Low priority given documentation.

#### 7. HNSW `remove_dir` cleanup fails silently on non-empty temp directories
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:229`
- **Description:** After atomic rename of HNSW files from temp to final, `let _ = std::fs::remove_dir(&temp_dir)` attempts to remove the temp directory. `remove_dir` only removes empty directories. If the rename loop (lines 213-226) fails partway, some files remain in temp and `remove_dir` silently fails. The pre-cleanup at line 124-132 uses `remove_dir_all` correctly. Between saves, orphaned `.index.tmp/` directories with partial HNSW data accumulate in `.cq/`.
- **Suggested fix:** Change `remove_dir` to `remove_dir_all` for consistency with the pre-cleanup path. Already flagged in Platform Behavior batch but the resource leak angle is distinct.

#### 8. `Store::Drop` WAL checkpoint blocks process exit — no timeout
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:451-463`
- **Description:** `Store::Drop` calls `rt.block_on(PRAGMA wal_checkpoint(TRUNCATE))`. For large WALs (after indexing 50k+ chunks), this can take 1-5 seconds. During MCP server shutdown, each `Store` (primary + references) checkpoints sequentially. With 3 references: 4 stores * 1-5s = 4-20 seconds of blocked shutdown. The `catch_unwind` wrapper prevents panics but doesn't add a timeout. Users experience the MCP server "hanging" on exit.
- **Suggested fix:** Add a timeout: wrap the checkpoint in `tokio::time::timeout(Duration::from_secs(3), ...)`. If it times out, log a warning and skip — SQLite will replay WAL on next open.

#### 9. `FileSystemSource::enumerate()` reads all file contents eagerly into a Vec
- **Difficulty:** easy
- **Location:** `src/source/filesystem.rs:66-103`
- **Description:** `FileSystemSource::enumerate()` collects all file paths, then reads every file's full content via `std::fs::read_to_string()`, storing all `SourceItem` objects (with content) in a Vec. For 70k files averaging 5KB: ~350MB for contents + ~7MB for paths. The `Source` trait returns `Vec<SourceItem>` forcing eager collection.
- **Suggested fix:** This code path is NOT used by the main CLI pipeline (which uses `enumerate_files` + parser lazily) or by MCP. It's only reachable via the `Source::enumerate()` trait method. Verify no production caller uses it. If it's dead in practice, add a doc warning about O(total_file_size) memory.

#### 10. Pipeline channel buffers can hold ~105MB of parsed/embedded chunks at peak
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:235`
- **Description:** Pipeline uses `bounded(256)` channels. Each `ParsedBatch` has up to 32 chunks of parsed code (~5KB avg = ~160KB per batch, 256 batches = ~40MB). The `EmbeddedBatch` adds embeddings (~3KB each, ~256KB per batch, 256 batches = ~65MB). Combined peak: ~105MB. In practice, the writer drains fast (SQLite batch insert is quick on SSD), so channels rarely fill. Backpressure from slow storage could spike this.
- **Suggested fix:** Current depth (256) is reasonable for throughput. Consider reducing to 64 if memory matters — profiling shows negligible throughput difference on SSD. Document worst-case memory budget in code comment.

#### 11. No limit on number of references loaded — each adds ~20MB baseline memory
- **Difficulty:** easy
- **Location:** `src/reference.rs:36-69`
- **Description:** `load_references()` loads every reference from config without limit. Each `ReferenceIndex` holds a `Store` (~16MB page cache) + optional HNSW index (varies with reference size). A user with 10 references: ~200MB just in connection pools and page caches before any queries. There's no warning or cap.
- **Suggested fix:** Log a warning if >5 references: `if refs.len() > 5 { tracing::warn!("Loading {} references — consider reducing for lower memory usage", refs.len()); }`. Optionally add a config-level cap (default 10).

#### 12. No size guard on HNSW graph/data files before loading — hnsw_rs allocates unbounded
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:291-301`
- **Description:** `HnswIo::load_hnsw()` deserializes the HNSW graph using bincode. The ID map has a 500MB file size guard (line 256-272), but `.hnsw.graph` and `.hnsw.data` files have no size guard. A corrupted or crafted graph file could cause unbounded memory allocation during deserialization. The checksum verification (line 253) mitigates accidental corruption but not intentional attacks (attacker can update checksums). For 100k chunks: `.hnsw.data` ~300MB, `.hnsw.graph` ~50-100MB — both reasonable. Without a guard, a 10GB crafted file would be loaded.
- **Suggested fix:** Add file size guards before `load_hnsw()`: `const MAX_GRAPH_SIZE: u64 = 1_073_741_824; // 1GB` and `const MAX_DATA_SIZE: u64 = 2_147_483_648; // 2GB`. Check `std::fs::metadata(&path)?.len()` before loading. Covers ~350k chunks with headroom.

#### 13. MCP server pool idle timeout (300s) holds ~64MB+ in idle SQLite page caches
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:115`
- **Description:** SQLite pool has `idle_timeout(300s)`. Between LLM interactions, 4 connections stay open for 5 minutes each. Each holds ~16MB page cache (PRAGMA cache_size). Primary store + N references: (N+1) * 4 * 16MB = 64MB+ idle. The 300s timeout was chosen for CLI where the process exits quickly; for MCP server sessions it's excessive.
- **Suggested fix:** For MCP server, use shorter idle timeout (60s) since re-acquiring a connection is ~1ms. Or reduce `max_connections` to 2 for reference stores (read-only search queries only).

#### 14. `search_notes` fetches 1000 full embedding blobs (~3MB) even when limit is 2
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:141-156`
- **Description:** `search_notes` loads up to `MAX_NOTES_SCAN` (1000) rows including embedding blobs (~3KB each = ~3MB) via `fetch_all`. After scoring, only `limit` (typically 2-5) results are kept. The intermediate `rows` Vec holds all raw data. For limit=2 with 1000 notes: ~3MB loaded, 998 embeddings immediately discarded.
- **Suggested fix:** Low priority — the 1000-note cap bounds memory. The true fix requires HNSW-based note search (issue #203). Accept as intentional trade-off.

## Batch 3: Data Safety

#### 1. Notes file has no locking — concurrent MCP mutations can corrupt `notes.toml`
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:93-95` (add), `src/mcp/tools/notes.rs:176` (update), `src/mcp/tools/notes.rs:250` (remove), `src/note.rs:179-218` (rewrite)
- **Description:** All three note mutation tools (`tool_add_note`, `tool_update_note`, `tool_remove_note`) operate on `docs/notes.toml` without any file locking. In HTTP transport mode, concurrent requests can execute simultaneously. `tool_add_note` uses `OpenOptions::append()` which is atomic at the OS level for small writes — but `tool_update_note` and `tool_remove_note` both call `rewrite_notes_file`, which does read → parse → mutate → serialize → write-to-temp → rename. Two concurrent rewrite operations: (1) both read the same file, (2) both mutate in memory, (3) one renames its temp file over the original, (4) the other renames its temp file, overwriting the first's changes. The losing mutation is silently dropped. The index is then rebuilt from the file, so the in-memory state stays consistent with the (corrupted) file. Existing issue #231 tracks this.
- **Suggested fix:** Add file-level advisory locking (`fs4::FileExt::lock_exclusive`) around the read-mutate-write cycle in `rewrite_notes_file`. Or serialize all note mutations through a Mutex in the MCP server.

#### 2. Config `add_reference_to_config` read-modify-write race
- **Difficulty:** easy
- **Location:** `src/config.rs:175-222`
- **Description:** `add_reference_to_config` reads the config file, parses it, modifies the TOML table, and writes back. No locking. If two `cqs ref add` commands run concurrently (or a CLI `ref add` races with an MCP `ref add`), one write overwrites the other. The window is small for CLI usage but real for programmatic callers. `remove_reference_from_config` has the same pattern. Both functions also use non-atomic `std::fs::write` (not temp+rename).
- **Suggested fix:** Use temp-file + atomic rename (like `rewrite_notes_file` does) and add advisory file locking around the read-modify-write cycle.

#### 3. Pipeline writes chunks and call graph in separate transactions
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:148-165` (writer loop)
- **Description:** The indexing pipeline's writer loop calls `upsert_chunks_batch` (which runs in a single transaction) and then `upsert_calls_batch` (separate transaction) for each batch. If the process crashes or is interrupted between these two calls, the database will have chunks without their corresponding call graph entries. The `calls` table has `FOREIGN KEY (caller_id) REFERENCES chunks(id) ON DELETE CASCADE`, so orphaned calls can't exist — but missing calls for existing chunks means `cqs_callers`, `cqs_callees`, `cqs_trace`, and `cqs_dead` return incomplete results until the next full reindex. The same gap exists for `upsert_function_calls` (the full call graph table).
- **Suggested fix:** Wrap both `upsert_chunks_batch` and `upsert_calls_batch` (and `upsert_function_calls`) in a single transaction per batch. This requires passing an explicit transaction handle rather than each function creating its own.

#### 4. Watch mode never updates call graph
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:116-170` (`reindex_files` function)
- **Description:** When watch mode detects a file change, `reindex_files` parses the file, embeds chunks, and calls `store.upsert_chunks_batch()` — but never calls `store.upsert_calls_batch()` or `store.upsert_function_calls()`. The call graph data (`calls` and `function_calls` tables) becomes stale for any file modified through watch mode. Tools that depend on the call graph (`cqs_callers`, `cqs_callees`, `cqs_trace`, `cqs_impact`, `cqs_dead`, `cqs_test_map`) return incorrect results. Users must do a full reindex to fix. The parser already extracts call information — it's available in the `Chunk::calls` field — it's just not persisted in watch mode.
- **Suggested fix:** After `upsert_chunks_batch`, call `store.upsert_calls_batch()` and `store.upsert_function_calls()` with the parsed chunks/calls data.

#### 5. HNSW index stale after watch mode updates (known issue #236)
- **Difficulty:** hard
- **Location:** `src/cli/watch.rs:116-170`, `src/hnsw/mod.rs`
- **Description:** Watch mode updates chunks in SQLite but never rebuilds the HNSW index. New/modified chunks are in the database but not in the HNSW graph. Semantic search falls back to brute-force SQLite scan for these chunks (since `search_by_candidate_ids` only returns IDs from HNSW). The performance impact depends on how many chunks changed since last full index. Tracked as existing issue #236.
- **Suggested fix:** Incremental HNSW updates — add new vectors to the existing graph without full rebuild. Or periodically rebuild in the background.

#### 6. `Store::init` executes DDL without a transaction
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:73-93` (`init` method)
- **Description:** `Store::init` splits `schema.sql` by semicolons and executes each DDL statement individually (`sqlx::query(stmt).execute()`). If the process crashes mid-init (e.g., between creating `chunks` table and `chunks_fts`), the database is left in a partially-created state. The next `Store::open` call would find a schema version mismatch (or no version at all) and attempt migration, which may fail because some tables exist and others don't. SQLite supports transactional DDL, so wrapping all statements in a single transaction is straightforward.
- **Suggested fix:** Wrap all DDL statements in a single transaction: `BEGIN; ...all CREATE TABLE/INDEX...; INSERT metadata schema_version; COMMIT;`

#### 7. `prune_missing` batched deletes use separate transactions per batch
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:166-210` (`prune_missing` method)
- **Description:** `prune_missing` identifies stale chunks (files no longer on disk) and deletes them in batches of 100, each batch in its own transaction. If the process is interrupted mid-prune, some stale chunks are deleted and others remain. This is not data corruption — the remaining stale chunks will be caught on the next prune — but it means the database is in an inconsistent state where some files are partially pruned. The FTS shadow table is updated within each batch transaction (via triggers), so FTS stays consistent within each batch.
- **Suggested fix:** Low priority. The current behavior is safe (eventual consistency). For strict atomicity, wrap all batches in a single transaction, but this may hold a write lock for a long time on large prunes.

#### 8. `tool_add_note` appends raw TOML without verifying file parse integrity
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:93-122`
- **Description:** `tool_add_note` uses `OpenOptions::append()` to write a raw TOML snippet (`[[note]]\n...`) to `docs/notes.toml`. It does not read the existing file first, so it cannot verify that the file is valid TOML before appending. If the file is currently malformed (e.g., from a previous interrupted write or manual edit), the append succeeds at the OS level but the resulting file is still unparseable. The subsequent `reindex_notes` call will parse the file, fail, and the note appears to be added (no error returned to the user) but is not actually indexed. The next tool call that reads notes will silently have zero notes.
- **Suggested fix:** Before appending, parse the existing file to verify it's valid TOML. If not, return an error telling the user to fix the file. Or use the read-mutate-write pattern (like `tool_update_note` does) for all note mutations.

#### 9. HNSW `save()` uses `assert_eq!` — panics instead of returning error
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:101-104`
- **Description:** `HnswIndex::save()` calls `assert_eq!(hnsw_count, id_map.len(), ...)` to verify the HNSW graph and ID map are in sync. If they're not (which would indicate a bug elsewhere), this panics and crashes the process. In MCP server mode, this takes down the entire server. The condition should be an invariant, but defensive coding says return an error rather than crash a long-running server. The `load()` function handles this correctly — it returns `HnswError::CountMismatch` for the same condition.
- **Suggested fix:** Replace `assert_eq!` with an error return: `if hnsw_count != id_map.len() { return Err(HnswError::CountMismatch { ... }); }`

#### 10. `embedding_batches` uses LIMIT/OFFSET pagination — unstable under concurrent writes
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:130-164` (`embedding_batches` method)
- **Description:** `embedding_batches` paginates through all chunks using `LIMIT ? OFFSET ?` with incrementing offsets. This is used during HNSW rebuild to load all embeddings. If chunks are inserted or deleted between page fetches (e.g., by a concurrent MCP writer in HTTP mode), rows can be skipped or duplicated. Skipped rows won't be in the HNSW index; duplicated rows waste memory but are caught by the HNSW builder's ID deduplication. The impact is that the rebuilt HNSW index may miss some chunks. In practice, HNSW rebuilds happen during `cqs index` (CLI) while the index lock prevents concurrent CLI writers — but the MCP server can still write concurrently if running in HTTP mode.
- **Suggested fix:** Use keyset pagination (`WHERE id > ? ORDER BY id LIMIT ?`) instead of OFFSET, which is stable under concurrent modifications. Or acquire a shared read lock during the full rebuild.

#### 11. `rewrite_notes_file` temp file not cleaned on rename failure
- **Difficulty:** easy
- **Location:** `src/note.rs:210-217`
- **Description:** `rewrite_notes_file` writes to a temp file (`notes.toml.tmp`) and then renames it over the original. If the rename fails (e.g., cross-device move, permissions), the function returns an error but leaves the temp file on disk. The next call to `rewrite_notes_file` will overwrite the temp file anyway, so this is a cosmetic issue — but on repeated rename failures, the temp file contains the intended state while the original file has the old state, which could confuse users inspecting the directory.
- **Suggested fix:** Add cleanup in the error path: `if let Err(e) = std::fs::rename(&tmp_path, path) { let _ = std::fs::remove_file(&tmp_path); return Err(e.into()); }`

#### 12. `add_reference_to_config` uses `unwrap_or_default()` — clobbers unreadable config
- **Difficulty:** easy
- **Location:** `src/config.rs:184`
- **Description:** `std::fs::read_to_string(config_path).unwrap_or_default()` treats ALL read errors (permissions, I/O, encoding) as "file doesn't exist" and starts with an empty string. The subsequent `std::fs::write` at line 222 overwrites the config file. If the config was unreadable due to a transient error (locked by another process, temporary permission issue), this silently destroys all existing reference configurations. `remove_reference_from_config` correctly distinguishes `NotFound` from other errors. Also noted in Security finding #3 and Error Handling batch #7.
- **Suggested fix:** Match on error kind as `remove_reference_from_config` does: return error for non-NotFound errors.

#### 13. Schema migration runs steps without a wrapping transaction
- **Difficulty:** medium
- **Location:** `src/store/migrations.rs:24-50` (`migrate` function)
- **Description:** The `migrate()` function runs migration steps sequentially and updates `schema_version` at the end. Each step executes its own SQL. If a step fails or the process crashes mid-migration, the database is left at the old schema version but with partial DDL changes applied. SQLite supports transactional DDL, so a failed `ALTER TABLE` can be rolled back. Currently no active migrations exist (v10 is current and all migration functions return `MigrationNotSupported`), so this is a latent risk for future migrations rather than an active bug.
- **Suggested fix:** Wrap the entire migration sequence (all steps + version update) in a single transaction. If any step fails, the whole migration rolls back cleanly.

#### 14. HTTP mode concurrent tool calls race on note file mutations
- **Difficulty:** medium
- **Location:** `src/mcp/transports/http.rs:87-89`, `src/mcp/tools/notes.rs`
- **Description:** The HTTP transport processes requests concurrently via axum's async handler. Two concurrent `cqs_update_note` calls (or any combination of note mutation tools) both call `rewrite_notes_file` which does read→parse→mutate→write. This is the same race as finding #1, but specifically via the HTTP transport path. The stdio transport is naturally serialized (one request at a time). The HTTP transport has no request queuing or serialization for write operations. This also affects `cqs_add_note` if called concurrently — while individual appends are OS-atomic for small writes, the subsequent `reindex_notes` call does a full read-parse-embed-store cycle that can race with another append.
- **Suggested fix:** Add a `Mutex<()>` for note mutations in the MCP server, acquired before any note file operation and released after reindex completes. Or serialize all write operations through a dedicated write task with an mpsc channel.
