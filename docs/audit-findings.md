# Audit Findings

Generated: 2026-02-05 (fresh audit)

See design: `docs/plans/2026-02-04-20-category-audit-design.md`

---

### Documentation

#### 1. lib.rs doc comment lists 5 languages, should be 7
- **Difficulty:** easy
- **Location:** `src/lib.rs:10`
- **Description:** The module-level doc comment says `Multi-language: Rust, Python, TypeScript, JavaScript, Go` but C and Java were added in v0.5.0. This actively misleads anyone reading the library docs or crates.io page.
- **Suggested fix:** Update line 10 to include C and Java: `Rust, Python, TypeScript, JavaScript, Go, C, Java`

#### 2. README HNSW tuning table shows M=16, actual code uses M=24
- **Difficulty:** easy
- **Location:** `README.md:284` and `src/hnsw.rs:58`
- **Description:** README's HNSW tuning table says `M (connections) | 16` but the actual constant `MAX_NB_CONNECTION` in `hnsw.rs:58` is 24. Similarly, the README says `ef_search | 50` but the code uses `EF_SEARCH = 100` at `hnsw.rs:66`. These numbers directly mislead users trying to understand or tune performance.
- **Suggested fix:** Update README table to match actual values: M=24, ef_search=100.

#### 3. CHANGELOG says "Query embedding LRU cache (100 entries)" - actual is 32
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:387` / `src/embedder.rs:222`
- **Description:** The CHANGELOG for v0.1.8 says "Query embedding LRU cache (100 entries)" but `DEFAULT_QUERY_CACHE_SIZE` in `embedder.rs:222` is 32.
- **Suggested fix:** Update CHANGELOG v0.1.8 entry to say 32 entries, or note it was later changed.

#### 4. ROADMAP "Current Phase" label points to completed Phase 4
- **Difficulty:** easy
- **Location:** `ROADMAP.md:75`
- **Description:** Line 75 says `Current Phase: 4 (Scale)` with `Status: Complete` -- but Phase 5 is clearly the active phase with work ongoing. The "Current Phase" label is misleading.
- **Suggested fix:** Update the "Current Phase" marker to Phase 5 since Phase 4 is complete.

#### 5. CONTRIBUTING.md says "Chunks capped at 500 lines" - actual is 100 lines
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:125`
- **Description:** The Architecture Overview says "Chunks capped at 500 lines" but `src/parser.rs:168` shows the actual limit is 100 lines (`if lines > 100`). This directly misleads contributors about the chunk size limit.
- **Suggested fix:** Change to "Chunks capped at 100 lines"

#### 6. CONTRIBUTING.md architecture tree missing CLI and MCP submodules
- **Difficulty:** medium
- **Location:** `CONTRIBUTING.md:84-119`
- **Description:** The architecture tree is significantly incomplete for the CLI and MCP modules. CLI is missing `files.rs`, `signal.rs`, and `commands/` only lists `serve.rs` when it actually contains 8 files (doctor.rs, graph.rs, index.rs, init.rs, mod.rs, query.rs, serve.rs, stats.rs). MCP is missing `server.rs`, `types.rs`, `validation.rs`, `audit_mode.rs`, and the entire `tools/` subdirectory (7 files: audit.rs, call_graph.rs, mod.rs, notes.rs, read.rs, search.rs, stats.rs). Contributors looking at this tree will have a wrong mental model of the codebase.
- **Suggested fix:** Update the architecture tree to reflect the actual file layout. Key additions: CLI `commands/` expanded, CLI `files.rs` and `signal.rs` added, MCP `server.rs`, `types.rs`, `validation.rs`, `audit_mode.rs` added, MCP `tools/` directory added.

### Module Boundaries

**Overall:** The module split (CLI and MCP into submodules) was done cleanly. Dependency direction is correct: CLI -> lib, MCP -> lib. No circular dependencies. No CLI<->MCP cross-imports. `pub(crate)` used well in most places.

**Cross-reference:** Prior audit found 11 module boundary issues (M1-M11), all fixed. Verified still fixed. M3 (`index_notes` in `lib.rs`) was accepted as a shared coordination point used by both CLI watch and MCP server -- not re-reported. Findings below are **new issues** or issues not previously caught.

#### 1. Duplicated `strip_unc_prefix` across CLI and MCP *(NEW - from split)*
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:20-33` and `src/mcp/validation.rs:100-118`
- **Description:** Identical `strip_unc_prefix` function (with `#[cfg(windows)]` / `#[cfg(not(windows))]` variants) is copy-pasted in two places. Both strip `\\?\` prefix from Windows paths. If one gets a fix, the other won't. This duplication was introduced when CLI and MCP were split into separate modules -- the function was needed in both but not lifted to a shared location.
- **Suggested fix:** Move to a shared location (e.g., `src/path_utils.rs` or a `utils` module in `lib.rs`) and import from both CLI and MCP.

#### 2. `search.rs` reaches into `store::helpers` internals
- **Difficulty:** medium
- **Location:** `src/search.rs:16-18`
- **Description:** `search.rs` imports `ChunkRow`, `clamp_line_number`, `embedding_slice` from `crate::store::helpers` (a `pub(crate)` module). It manually constructs `ChunkRow` from SQL results (lines 364-378, 491-503), duplicating row-mapping logic that belongs in the store layer. If `ChunkRow` fields change, `search.rs` breaks.
- **Suggested fix:** Store should expose `fetch_chunks_by_ids()` returning `ChunkSummary` (public type). Let `ChunkRow` become truly private to `store`.

#### 3. Duplicated `load_hnsw_index` across CLI and MCP *(NEW - from split)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:14-30` and `src/mcp/server.rs:119-134`
- **Description:** Two nearly identical implementations of HNSW index loading. Both check `HnswIndex::exists()`, call `HnswIndex::load()`, log the result, and return `Option<Box<dyn VectorIndex>>`. The only difference is the log prefix. This duplication appeared when the CLI and MCP were split -- each needed to load the index independently but the logic wasn't factored out.
- **Suggested fix:** Move to a shared function in `src/hnsw.rs` (e.g., `pub fn try_load(cq_dir: &Path) -> Option<Box<dyn VectorIndex>>`).

#### 4. CLI `index_notes_from_file` duplicates `cqs::index_notes` *(NEW - from split)*
- **Difficulty:** medium
- **Location:** `src/cli/commands/index.rs:193-251` duplicates `src/lib.rs:85-155`
- **Description:** `index_notes_from_file()` in the CLI index command re-implements the note indexing logic (parse notes, embed with sentiment dimension, upsert to store, add to HNSW) that already exists in the shared `cqs::index_notes()`. The CLI watch module correctly uses `cqs::index_notes()` at `src/cli/watch.rs:293`. This duplication means bug fixes to note indexing must be applied in two places.
- **Suggested fix:** Refactor `index_notes_from_file` to call `cqs::index_notes()` instead of reimplementing the logic. The CLI-specific part (file path resolution) can remain, but embedding + store + HNSW operations should delegate to the shared function.

#### 5. MCP JSON-RPC types over-exposed as `pub`
- **Difficulty:** easy
- **Location:** `src/mcp/mod.rs:19`
- **Description:** `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError` are re-exported as `pub` from `mcp/mod.rs` and leak through `pub mod mcp` in `lib.rs`. These are internal protocol details -- no external consumer constructs them. Most other MCP types correctly use `pub(crate)`.
- **Suggested fix:** Change to `pub(crate) use types::{JsonRpcError, JsonRpcRequest, JsonRpcResponse}` in `src/mcp/mod.rs:19`.

#### 6. MCP stats loads full HNSW index; CLI stats uses lightweight count *(NEW - inconsistency from split)*
- **Difficulty:** easy
- **Location:** `src/mcp/tools/stats.rs:25-28` vs `src/cli/commands/stats.rs:25`
- **Description:** MCP `tool_stats` calls `HnswIndex::load()` (deserializes the entire index into memory) just to report vector count. CLI `stats` command uses `HnswIndex::count_vectors()` (reads only metadata). On large indices, the MCP path wastes significant memory and time for a single integer.
- **Suggested fix:** Use `HnswIndex::count_vectors()` in MCP stats, matching the CLI pattern.

---

### Error Propagation

**Cross-reference:** Previous audit found 20 Error Propagation issues (E1-E20), all marked fixed. Verified: E5 (language/chunk_type warnings in helpers.rs), E14/E15 (file enumeration logging) confirmed fixed. E7 ("HNSW bare ? all have .context() now") is inaccurate -- `hnsw.rs` uses `map_err` with custom error types, not `.context()`. This provides equivalent context but through a different mechanism; not a bug but a documentation inaccuracy in the previous audit. All findings below are **NEW** issues not covered by E1-E20.

#### 1. MCP regex compilation silently swallowed with `.ok()` *(NEW - MCP server.rs from module split)*
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214-216`
- **Description:** `sanitize_error_message()` compiles two regexes using `.ok()` to swallow compilation errors. If either regex fails to compile, the function silently skips sanitization, potentially leaking internal filesystem paths to MCP clients. These are hardcoded constant patterns, so they *should* always compile, but the `.ok()` means a typo would silently disable path sanitization with no indication.
- **Suggested fix:** Use `OnceLock` to compile these at startup and panic if invalid (they're constants). Or at minimum `expect("hardcoded regex")`.

#### 2. `get_embeddings_by_hashes` silently returns empty map on DB error *(NEW)*
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:259-264`
- **Description:** If the SQL query to fetch cached embeddings fails, the error is logged at `warn` level but the function returns an empty HashMap. Callers (the indexing pipeline) will then re-embed every chunk unnecessarily, potentially spending hours of GPU time re-computing what's already in the database. The caller has no way to distinguish "no cached embeddings" from "database error."
- **Suggested fix:** Change return type to `Result<HashMap<...>, StoreError>` and let callers decide whether to continue without cache.

#### 3. `get_by_content_hash` silently returns None on DB error *(NEW - distinct from E3 which covered `get_caller_chunk_ids`)*
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:226-232`
- **Description:** Same pattern as above -- DB errors are logged but return `None`, indistinguishable from "hash not found." Note says "Prefer `get_embeddings_by_hashes` for batch lookups" so this is lower priority, but the pattern is still incorrect.
- **Suggested fix:** Change return type to `Result<Option<Embedding>, StoreError>`.

#### 4. `serde_json::to_string(&note.mentions).unwrap_or_default()` in note upsert *(NEW)*
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:74`
- **Description:** If serializing mentions to JSON fails (unlikely but possible with weird strings), it silently stores an empty string. When the note is later loaded, the mentions will be missing with no indication that data was lost. The surrounding code properly uses `?` for all other DB operations.
- **Suggested fix:** Use `?` to propagate the error, or at minimum `map_err` to log a warning before defaulting.

#### 5. MCP HTTP response serialization uses `unwrap_or_default()` *(NEW - HTTP transport from module split)*
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:302`
- **Description:** `serde_json::to_value(&response).unwrap_or_default()` -- if the JSON-RPC response fails to serialize, the client receives an empty JSON object `{}` with no error indication. This would be very confusing to debug since the HTTP status is 200 OK.
- **Suggested fix:** Handle the error and return a proper JSON-RPC error response, or use `expect()` since serialization of the response type should always succeed.

#### 6. Language parse fallback silently defaults to Rust *(NEW - distinct from E5 which covers DB-stored values; this is user input from MCP tool args)*
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:64`
- **Description:** `.parse().unwrap_or(Language::Rust)` -- if a user passes an invalid language filter string (e.g., `"ruby"`), it silently falls back to Rust instead of returning an error. The user gets Rust results thinking they searched Ruby.
- **Suggested fix:** Return an error for unrecognized language strings in the MCP tool handler.

#### 7. Pipeline parser errors silently produce empty chunks *(NEW - pipeline.rs from CLI split)*
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:253-256`
- **Description:** When `parser.parse_file()` fails, the error is logged at `warn` but the file is silently skipped. In a large index run, hundreds of parse errors could go unnoticed. The pipeline returns `PipelineStats` but has no field for parse errors/skipped files.
- **Suggested fix:** Add a `parse_errors: usize` counter to `PipelineStats` and display it in the summary. Users should know if files were skipped.

#### 8. Config load errors silently fall back to defaults *(NEW)*
- **Difficulty:** easy
- **Location:** `src/config.rs:44-47`
- **Description:** Both user and project config files use `.unwrap_or_default()` when loading fails. A malformed `.cqs.toml` is silently ignored -- the user thinks their settings are applied but they aren't. The warn log exists but is easy to miss in normal CLI output.
- **Suggested fix:** This is arguably intentional (graceful degradation), but consider printing to stderr when a project config file exists but fails to parse, since the user explicitly created it.

#### 9. Store `check_schema_version` swallows parse error via `.ok()` chain *(overlaps E6 - refinement)*
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:253-263`
- **Description:** Previous audit (E6) added warn logging here, but the error path still defaults to version 0, triggering a confusing `MigrationNotSupported` instead of a clear corruption error. When parsing the schema version string from metadata, a parse failure is logged at `warn` but then `.ok()` converts it to `None`, defaulting to version 0. This means a corrupted schema_version field would trigger a migration from v0, which will fail with `MigrationNotSupported` -- an indirect and confusing error path instead of a clear "corrupted metadata" error.
- **Suggested fix:** Return a dedicated `StoreError::CorruptedMetadata` variant when schema_version exists but can't be parsed as integer.

#### 10. `notes_need_reindex` DB error swallowed with `.unwrap_or(Some(0))` *(NEW - from CLI commands/ split)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:203`
- **Description:** `store.notes_need_reindex()` returns `Result<Option<u64>>`, but the call site uses `.unwrap_or(Some(0))`, converting any DB error into "0 notes need reindexing." This silently skips note reindexing when the database has issues, with no log or indication.
- **Suggested fix:** Use `?` to propagate the error, or log at warn level before defaulting.

#### 11. `extract_call_graph` parse errors silently swallowed *(NEW - from CLI commands/ split)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:182-184`
- **Description:** When `parser.extract_call_graph()` fails for a file, the error is silently discarded via the `.ok()` in the filter_map chain. Unlike pipeline.rs which at least logs parse errors at `warn`, this path drops them entirely. Users won't know if call graph extraction failed for some files.
- **Suggested fix:** Log at `warn` or `debug` level before filtering out, consistent with how pipeline.rs handles parse errors.

### Code Hygiene

#### 1. GPU embedder thread duplicates `prepare_for_embedding` logic
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:333-447` (GPU thread) vs `src/cli/pipeline.rs:122-160` + `src/cli/pipeline.rs:452-508` (CPU thread)
- **Description:** The GPU embedder thread (Stage 2a) manually inlines the windowing, cache-check, and text-generation logic that was already extracted into `prepare_for_embedding()` for the CPU thread (Stage 2b). The GPU thread duplicates ~60 lines: `apply_windowing`, hash lookup, cached/to_embed separation, and `generate_nl_description` calls. The CPU thread correctly uses the `prepare_for_embedding()` and `create_embedded_batch()` helpers. If windowing or cache logic changes, only the CPU path gets updated.
- **Suggested fix:** Refactor the GPU thread to use `prepare_for_embedding()`. The only GPU-specific logic is the `max_len > 8000` pre-filter and the GPU-vs-CPU embed call; the preparation is identical. This would cut ~40 lines and ensure both paths stay in sync.

#### 2. `load_hnsw_index` duplicated between CLI and MCP
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:14-30` and `src/mcp/server.rs:119-134`
- **Description:** Two nearly identical implementations of HNSW index loading. Both check `HnswIndex::exists()`, call `HnswIndex::load()`, log the result, and return `Option<Box<dyn VectorIndex>>`. The only difference is the log prefix ("Using HNSW" vs "MCP: Loaded HNSW"). (Also overlaps with Module Boundaries finding on `strip_unc_prefix`; this is a separate duplication.)
- **Suggested fix:** Move to a shared function in `src/hnsw.rs` or `src/index.rs`, parameterize the log prefix or leave it generic.

#### 3. `read_context_lines` called twice per result for the same file
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:88-101` and `src/cli/display.rs:115-129`
- **Description:** When context is requested, `read_context_lines()` is called twice for the same result -- once to get `before` (ignoring `after`), and again to get `after` (ignoring `before`). Each call reads the entire file from disk via `std::fs::read_to_string`. For N results each with context, this doubles file I/O unnecessarily.
- **Suggested fix:** Call `read_context_lines` once, destructure both `before` and `after`, and use them in their respective positions.

#### 4. `cli::run()` marked `#[allow(dead_code)]`
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:168-171`
- **Description:** `pub fn run()` is annotated with `#[allow(dead_code)]` and has a comment saying it's "kept for library users who want simpler invocation." However, `main.rs` uses `run_with()` directly, and no external consumer calls `run()`. This is dead code hidden by an allow attribute.
- **Suggested fix:** Either remove the function entirely (it's a one-liner wrapper around `run_with(Cli::parse())`), or if it's genuinely intended as public API, add a doc test that exercises it so the allow isn't needed.

#### 5. Regex recompilation on every error in MCP server
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214-216`
- **Description:** `sanitize_error_message()` compiles two regex patterns (`Regex::new()`) on every call. While errors aren't the hot path, this is unnecessary work. The `nl.rs` module demonstrates the correct pattern with `LazyLock<Regex>`.
- **Suggested fix:** Use `static LazyLock<Regex>` for both patterns, matching the project's existing convention in `src/nl.rs:20-23`.

**Cross-reference verification (H1-H12):**

| ID | Issue | Status |
|----|-------|--------|
| H1 | ExitCode enum | FIXED - proper enum at `signal.rs:11`, used at 2 call sites |
| H2 | `run()` dead code | NOT FIXED - still has `#[allow(dead_code)]`, no callers (= finding #4 above) |
| H3 | InitializeParams `#[allow(dead_code)]` | ACCEPTABLE - protocol compliance, doc comment explains |
| H4 | `_no_ignore` unused param | FIXED - parameter used |
| H5 | `run_index_pipeline` length | PARTIALLY ADDRESSED - ~393 lines but is pipeline orchestrator with thread closures; hard to decompose further without losing thread locality |
| H6 | `cmd_index` length | FIXED - reduced to ~139 lines with extracted subfunctions |
| H7 | GPU/CPU consolidation | PARTIALLY FIXED - helpers exist but GPU thread still inlines (= finding #1 above) |
| H8 | Batch processing | FIXED - proper `create_embedded_batch` helper |
| H9 | `score_note_row` extraction | FIXED - extracted, used at 2 call sites |
| H10 | Source trait | FIXED - proper trait at `source/mod.rs:56` |
| H11 | Unnecessary `.to_string()` | FIXED - no unnecessary conversions remain |
| H12 | Magic sentiment thresholds | FIXED - named constants at `note.rs:16-17` |

**Summary:** 9/12 fixed, 1 acceptable, 2 partially fixed (captured in findings #1 and #4). No additional NEW Code Hygiene issues found beyond the 5 already listed.

---

### API Design

#### 1. Delete methods return `u32` while count methods return `u64`
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:121` (`delete_by_origin`), `src/store/chunks.rs:282` (`chunk_count`)
- **Description:** `chunk_count()` and `note_count()` return `Result<u64>`, but `delete_by_origin()`, `delete_notes_by_file()`, and `prune_missing()` return `Result<u32>`, truncating `rows_affected()` (which is u64). Callers deal with mixed integer types.
- **Suggested fix:** Make all count-returning methods use `u64` consistently.

#### 2. `get_by_content_hash` returns `Option` while similar methods return `Result` *(dedup with Error Propagation #3)*
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:218`
- **Description:** Returns `Option<Embedding>`, swallowing DB errors with a warning log. All other Store methods return `Result<T, StoreError>`. Can't distinguish "not found" from "database error."
- **Suggested fix:** Return `Result<Option<Embedding>, StoreError>`.

#### 3. `search_fts` returns `Vec<String>` while `search_by_name` returns `Vec<SearchResult>`
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:385` vs `src/store/mod.rs:412`
- **Description:** Inconsistent return types for FTS search methods. `search_fts()` returns bare chunk IDs, `search_by_name()` returns full SearchResult with scores. Callers of search_fts need a second query for details.
- **Suggested fix:** If `search_fts` is internal, make it `pub(crate)`. If public, return `SearchResult`.

#### 4. Duplicate `EMBEDDING_DIM` constants across modules
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:63`, `src/cagra.rs:33`, `src/embedder.rs:59`, `src/store/helpers.rs` (`EXPECTED_DIMENSIONS` as `u32`)
- **Description:** Embedding dimension 769 defined in 4 places with different names and types (`usize` vs `u32`). If one changes and others don't, silent data corruption.
- **Suggested fix:** Export single canonical `EMBEDDING_DIM: usize` from `embedder.rs`, import elsewhere.

#### 5. `get_callers` vs `get_callers_full` naming misleading
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:47` vs `src/store/calls.rs:177`
- **Description:** "full" returns *less* data per result (lightweight name/file/line from `function_calls` table) while non-"full" returns full chunk data. Naming inverts expectation.
- **Suggested fix:** Rename to clarify data source: `get_callers_from_chunks()` / `get_callers_from_call_graph()`.

#### 6. `note_stats` returns unnamed tuple `(u64, u64, u64)`
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:226`
- **Description:** Returns `(total, warnings, patterns)` positionally. Swap total and warnings -- compiles fine. Meanwhile `stats()` returns proper `IndexStats` struct.
- **Suggested fix:** Create `NoteStats { total, warnings, patterns }` struct.

#### 7. `SearchFilter::validate` returns `Result<(), &'static str>` instead of proper error type
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:294`
- **Description:** Raw string error that can't be matched, categorized, or composed. Every other module uses `thiserror`-derived error types. Can't include runtime data.
- **Suggested fix:** Add `ValidationError` variant to `StoreError` or create `SearchFilterError` enum.

---

## De-duplication Notes

These findings appear across multiple categories and should be fixed once:

1. **strip_unc_prefix duplication** - Module Boundaries #1 = Code Hygiene (if reported)
2. **load_hnsw_index duplication** - Module Boundaries #3 = Code Hygiene #2
3. **Regex recompilation / `.ok()` on regex** - Code Hygiene #5 = Error Propagation #1
4. **get_by_content_hash error swallowing** - API Design #2 = Error Propagation #3

After de-duplication: **~31 unique findings** in Batch 1.

---

### Extensibility

**Scope:** Hardcoded values that should be configurable, closed enums that prevent extension, adding features requiring surgery in many files, tight coupling that prevents swap-out.

**What IS configurable (via `.cqs.toml` / CLI flags):**
- `limit` (default result count)
- `threshold` (similarity threshold)
- `name_boost` (hybrid search name weight)
- `quiet` / `verbose` (output control)

**What is NOT configurable but arguably should be:** See findings below, rated by whether they block real use cases.

**Cross-reference with prior audit (X1-X18):**

| ID | Issue | Status | Re-assessment |
|----|-------|--------|---------------|
| X1 | Hardcoded embedding model | Accepted | Still accepted. Single-model design is intentional. |
| X2 | Hardcoded embedding dimensions (769) | Accepted | Still accepted. Centralized as `EMBEDDING_DIM` / `EXPECTED_DIMENSIONS`. |
| X3 | Hardcoded HNSW parameters | Accepted | Still accepted. Well-documented in comments at `hnsw.rs:47-57`. |
| X4 | Closed Language enum | Moderate friction | **Still moderate friction - see finding #1 below.** |
| X5 | Language enum duplicate | Fixed | Confirmed fixed. Single `Language` enum in `src/language/mod.rs`. |
| X6 | Closed ChunkType enum | Easy | **Unchanged. Still closed. See finding #2 below.** |
| X7 | Hardcoded query patterns | Accepted | Still accepted. Tree-sitter queries are per-language, stable. |
| X8 | Hardcoded chunk size (100 lines) | Accepted | Still accepted. `parser.rs:168`. Reasonable default. |
| X9 | Hardcoded file size (1MB CLI / 50MB parser) | Accepted | Still accepted. Two separate limits: `cli/files.rs:13` (1MB for indexing walk) and `parser.rs:110` (50MB OOM guard). |
| X10 | Hardcoded token window (480/64) | Accepted | Still accepted. `pipeline.rs:27-28`. Tied to E5-base-v2's 512 limit. |
| X11 | Hardcoded SQLite pragmas | Accepted | Still accepted. `store/mod.rs:114-141`. Well-tuned for the workload. |
| X12 | Hardcoded RRF K=60 | Accepted | Still accepted. `store/mod.rs:490`. Standard constant from the RRF paper. |
| X13 | Hardcoded note limits (10k) | Accepted | Still accepted. `note.rs:21`. Safety cap, not a practical limit. |
| X14 | Hardcoded sentiment thresholds | Fixed | Confirmed fixed. Named constants at `note.rs:16-17`. |
| X15 | Hardcoded cache size (32) | Accepted | Still accepted. `embedder.rs:222`. ~96KB total, appropriate for CLI tool. |
| X16 | Hardcoded batch sizes | Accepted | Still accepted. `embedder.rs:237-239` (4 CPU / 16 GPU), `pipeline.rs:192` (32). Tuned for hardware. |
| X17 | Hardcoded root markers | Accepted | Still accepted. `cli/config.rs:17-24`. Standard project markers, extensible list. |
| X18 | Hardcoded config path | Accepted | Still accepted. `~/.config/cqs/config.toml` follows XDG convention. |

**New / changed findings:**

#### 1. Closed Language enum - moderate friction for adding languages *(X4 - unchanged)*
- **Difficulty:** moderate
- **Location:** `src/language/mod.rs:144-160` (enum), `mod.rs:226-241` (FromStr), `mod.rs:193-205` (Display)
- **Description:** Adding a new language requires changes in 5 places within `src/language/mod.rs` (enum variant, Display, FromStr, feature flag in `new()`, `mod` declaration) plus `Cargo.toml` (feature flag + tree-sitter dependency). The `LanguageRegistry` pattern already handles extension well -- languages register via `LanguageDef` structs -- but the `Language` enum forces manual sync across multiple match arms. The previous audit noted this was moderate friction; it remains so. Each of the 7 current languages follows an identical boilerplate pattern.
- **Impact:** Blocks nobody today. When the 8th language is added, it will require the same ~20 lines of boilerplate across 5 locations. Not urgent but friction grows linearly with language count.
- **Recommendation:** Accept for now. The registry pattern is the right abstraction; the enum is the pain point. A macro could generate the enum + impls from the registry, but adds complexity. Revisit if language count exceeds ~10.

#### 2. Closed ChunkType enum - easy friction for new code constructs *(X6 - unchanged)*
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:70-88` (enum), `mod.rs:90-103` (Display), `mod.rs:124-141` (FromStr)
- **Description:** Adding a new chunk type (e.g., `Module`, `TypeAlias`, `Macro`) requires changes in 3 match arms plus the `type_map` in every language definition that uses it. Currently 8 variants. Impact is lower than Language since chunk types change less frequently.
- **Impact:** Low. The current 8 types cover the vast majority of code constructs. New types would be rare additions (e.g., Rust macros, Go type aliases).
- **Recommendation:** Accept. Current coverage is adequate. The `type_map` in `LanguageDef` already provides the extension point for mapping tree-sitter captures to chunk types.

#### 3. Config system covers search but not indexing parameters *(NEW)*
- **Difficulty:** medium
- **Location:** `src/config.rs:26-37` (Config struct)
- **Description:** The config system (`~/.config/cqs/config.toml` and `.cqs.toml`) only exposes 5 search-related parameters. Users cannot configure any indexing behavior without code changes: file size limits, chunk size limits, HNSW parameters, batch sizes, connection pool size, SQLite pragmas, or token window parameters. For a local-first tool where users have very different hardware (8GB laptop vs 64GB workstation), the inability to tune memory-related parameters (cache_size, mmap_size, batch_size) can matter.
- **Impact:** Low-to-moderate. Most users will never need to tune these. Power users with very large codebases (>100k files) or constrained hardware would benefit from tunable `max_file_size`, `max_chunk_lines`, and HNSW `ef_search` (the tradeoff between speed and accuracy at query time).
- **Recommendation:** Consider adding `ef_search` to Config as the single highest-value extensibility improvement. It directly affects query speed vs accuracy and varies by workload. The other parameters are reasonable defaults that rarely need changing.

#### 4. `EMBEDDING_DIM` defined in 4 places with different types *(dedup with API Design #4)*
- **Difficulty:** easy
- **Location:** `src/embedder.rs:59` (`EMBEDDING_DIM: usize = 769`), `src/hnsw.rs:63` (`EMBEDDING_DIM: usize = 769`), `src/cagra.rs:33` (`EMBEDDING_DIM: usize = 769`), `src/store/helpers.rs:20` (`EXPECTED_DIMENSIONS: u32 = 769`)
- **Description:** The embedding dimension is defined 4 times across the codebase with two different names and two different types (`usize` vs `u32`). This is an extensibility concern because changing the embedding model (or even just the dimension) requires finding and updating all 4 definitions. If one is missed, the mismatch causes silent data corruption or panics at runtime.
- **Impact:** Low today (model is intentionally fixed), but this is a landmine for any future model change. The store's `u32` type is additionally inconsistent with the `usize` used everywhere else.
- **Recommendation:** Export `EMBEDDING_DIM` from `embedder.rs` as the single source of truth. Import in `hnsw.rs`, `cagra.rs`. Convert store's `EXPECTED_DIMENSIONS` to use the same constant (cast to `u32` at the one comparison site in `check_model_version`).

**Summary:** No new blockers. The extensibility posture is appropriate for a focused CLI tool. The main actionable items are:
1. **Unify `EMBEDDING_DIM`** (easy, prevents future bugs) - dedup with API Design #4
2. **Consider `ef_search` in config** (medium, highest-value extensibility gain for power users)
3. **Language/ChunkType enums** remain moderate friction but are acceptable given the low frequency of changes

---

### Panic Paths

**Scope:** unwrap/expect outside tests, assert macros in library code, array indexing without bounds checks, unreachable! usage, unsafe blocks, mutex poisoning, places where a panic could crash the process or leave state corrupted.

**Cross-reference with prior audit (PP1-PP7) -- all verified FIXED:**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| PP1 | cosine_similarity assert | FIXED | Returns `Option<f32>`, no assert. `src/math.rs:10` |
| PP2 | CAGRA array indexing | FIXED | Bounds check at `src/cagra.rs:319` (`if idx < self.id_map.len()`) |
| PP3 | MCP audit unreachable! | FIXED | Early return on line 20 guarantees `enabled` is `Some`. `src/mcp/tools/audit.rs:42-43` |
| PP4 | Embedder init expect | FIXED | `Embedder::new()` uses `?` throughout. `src/embedder.rs:222-254` |
| PP5 | HNSW id_map index access | FIXED | Bounds check at `src/hnsw.rs:439` (`if idx < self.id_map.len()`) |
| PP6 | Ctrl+C handler expect | FIXED | `if let Err(e)` with eprintln warning. `src/cli/signal.rs:26-34` |
| PP7 | Progress bar template expect | FIXED | `unwrap_or_else` with fallback to `default_bar()`. `src/cli/pipeline.rs:523-526` |

**New findings:**

#### 1. `split_into_windows` uses `assert!` in public library function
- **Difficulty:** easy
- **Location:** `src/embedder.rs:325-329`
- **Description:** `assert!(overlap < max_tokens / 2, ...)` in `pub fn split_into_windows()`. This panics instead of returning an error when called with invalid parameters. Currently only called from `cli/pipeline.rs` with hardcoded safe constants (overlap=64, max_tokens=480, so 64 < 240), but the function is `pub` on `Embedder` -- any future caller passing bad values gets a panic instead of an `Err`.
- **Risk:** Low (only one caller with safe constants), but violates the project convention of no unwrap/assert outside tests.
- **Suggested fix:** Replace `assert!` with `if overlap >= max_tokens / 2 { return Err(EmbedderError::...) }`.

#### 2. `search.rs` unwrap on `normalized_query` inside `if use_rrf` block
- **Difficulty:** easy
- **Location:** `src/search.rs:316`
- **Description:** `normalized_query.as_ref().unwrap()` -- logically safe because `normalized_query` is set to `Some(...)` when `use_rrf` is true (line 308-309), and the unwrap is inside `if use_rrf` (line 314). However, this is fragile: if someone refactors the condition logic, the unwrap could fire. The two branches are 6 lines apart with no comment linking them.
- **Risk:** Low (logic currently correct), but a maintenance hazard.
- **Suggested fix:** Use `let Some(ref nq) = normalized_query else { unreachable!(...) }` with a comment, or restructure to avoid the Option entirely by computing inside the `if use_rrf` block.

#### 3. Parser::new() `expect` on Language parse from registry
- **Difficulty:** easy
- **Location:** `src/parser.rs:61`
- **Description:** `def.name.parse().expect("registry/enum mismatch")` -- panics if a language is added to `REGISTRY` without a corresponding `Language` enum variant. This is an invariant between two data structures maintained by hand. Currently correct for all 7 languages, and any break would be caught by tests immediately, but it panics during `Parser::new()` which is called in the normal startup path.
- **Risk:** Very low (would be caught by CI immediately), but technically a runtime panic in library code.
- **Suggested fix:** Accept as-is. This is a compile-time-checkable invariant that catches developer errors instantly. Converting to `Result` would just push the error to every caller for no real benefit.

#### 4. `Language::def()` `expect` on registry lookup
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:167`
- **Description:** `REGISTRY.get(&self.to_string()).expect("language not in registry")` -- panics if a Language enum variant exists without a corresponding registry entry. Same invariant as finding #3, opposite direction.
- **Risk:** Very low (same reasoning as #3).
- **Suggested fix:** Accept as-is. Same rationale -- this guards an invariant that would be caught immediately by any test.

#### 5. Regex `expect` in LazyLock initialization
- **Difficulty:** easy
- **Location:** `src/nl.rs:21,23`
- **Description:** `Regex::new(r"...").expect("valid regex")` inside `LazyLock::new()`. Panics if the hardcoded regex pattern is invalid. The patterns are compile-time constants.
- **Risk:** Negligible. These are constant string literals. If they're wrong, they're wrong at compile time, and the `LazyLock` only fires once.
- **Suggested fix:** Accept as-is. Standard Rust pattern for `LazyLock<Regex>`.

#### 6. `NonZeroUsize::new(DEFAULT_QUERY_CACHE_SIZE).expect(...)` in Embedder constructors
- **Difficulty:** easy
- **Location:** `src/embedder.rs:242-243`, `src/embedder.rs:263-264`
- **Description:** `expect("DEFAULT_QUERY_CACHE_SIZE is non-zero")` where `DEFAULT_QUERY_CACHE_SIZE = 32`. Panics if the constant is changed to 0.
- **Risk:** Negligible. The constant is defined 20 lines above and is obviously non-zero.
- **Suggested fix:** Accept as-is. Standard pattern for `NonZeroUsize` with a const.

**Unsafe code review:**

| Location | Pattern | Assessment |
|----------|---------|------------|
| `src/hnsw.rs:176-179` | `ManuallyDrop::drop` + `Box::from_raw` in `LoadedHnsw::drop()` | Safe. Well-documented drop order. SAFETY comments present. |
| `src/hnsw.rs:187-188` | `unsafe impl Send/Sync for LoadedHnsw` | Justified. Data immutable after construction, external sync via RwLock. |
| `src/hnsw.rs:673,685` | `transmute` lifetime in HNSW load path | Sound. LoadedHnsw ensures HnswIo outlives Hnsw. SAFETY comments thorough. |
| `src/cagra.rs:359,361` | `unsafe impl Send/Sync for CagraIndex` | Justified. Internal Mutex protects all mutable state. |
| `src/cli/files.rs:124` | `libc::kill(pid, 0)` process check | Safe. Signal 0 is standard existence check, no side effects. |

**Mutex poisoning:**

All non-test mutex locks use `unwrap_or_else(|poisoned| poisoned.into_inner())` to recover from poisoning:
- `src/embedder.rs:289` (session), `390,411` (query cache)
- `src/cagra.rs:170,177,210,229,243,261,275` (CAGRA resources + index)
- `src/mcp/tools/audit.rs:14`, `search.rs:86`, `read.rs:54` (audit mode)

**Summary:** All 7 previously-reported panic path issues confirmed fixed. 6 new findings, of which:
- **1 actionable** (#1 - assert in public library function, easy fix)
- **1 minor maintenance hazard** (#2 - unwrap in logically safe but fragile location)
- **4 accepted** (#3-#6 - standard Rust patterns for compile-time invariants and constants)

No unsafe soundness issues. Mutex poisoning handled consistently.

---

### Observability

**Scope:** Logging coverage, tracing spans, debuggability, eprintln vs tracing, log levels, structured logging, ability to diagnose issues in production.

**Cross-reference:** Previous audit found 17 Observability issues (O1-O17). Verification of old findings and new findings below.

#### Old Findings Verification

| ID | Issue | Status |
|----|-------|--------|
| O2 | Watch mode tracing spans | FIXED - `info_span!("reindex_files")` at watch.rs:215, `info_span!("reindex_notes")` at watch.rs:281 |
| O3 | Parser timing spans | FIXED - `info_span!("parse_file")` at parser.rs:107 |
| O4 | Database pool creation silent | FIXED - `tracing::info!(path, "Database connected")` at store/mod.rs:166 |
| O5 | GPU failures use eprintln | FIXED - `tracing::warn!` in pipeline.rs:394,433 |
| O6 | Index fallback at debug level | FIXED - fallback logged at `info` level in search.rs:419,586 |
| O11 | Call graph ops at trace only | PARTIALLY FIXED - upserts at trace (calls.rs:17,125), queries at debug (calls.rs:48,178). Reasonable split. Batch completion at info (calls.rs:163). |
| O12 | Config loading errors not structured | FIXED - `tracing::warn!` with path at config.rs:68,87; structured `tracing::debug!` at config.rs:51-58,75-83 |
| O13 | index_notes has no logging | FIXED - `tracing::info!(path, count)` at lib.rs:108 |
| O16 | Schema migration silent on success | FIXED - `tracing::info!` at migrations.rs:34,41,51 for start, steps, and completion |
| O17 | Prune operation progress not visible | PARTIAL - pruned count printed in CLI (index.rs:104), no tracing inside `prune_missing` itself. Low impact since MCP doesn't prune. |

#### 1. MCP stats loads full HNSW index just to report vector count *(overlap with Module Boundaries #6)*
- **Difficulty:** easy
- **Location:** `src/mcp/tools/stats.rs:25-28`
- **Description:** `tool_stats` calls `HnswIndex::load()` to get the HNSW vector count, deserializing the entire index into memory (potentially hundreds of MB) just to call `.len()`. The CLI stats command uses the lightweight `HnswIndex::count_vectors()` (reads only the ID map file). Additionally, the active index is already loaded in `server.index` -- reporting both `hnsw_status` (re-loaded from disk) and `active_index` (from memory) provides redundant information.
- **Suggested fix:** Use `HnswIndex::count_vectors()` instead of `HnswIndex::load()`.

#### 2. Watch mode uses `eprintln!` for --no-ignore warning instead of tracing
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:41`
- **Description:** `eprintln!("Warning: --no-ignore is not yet implemented for watch mode")` bypasses the tracing framework. Won't appear in log files or structured logging.
- **Suggested fix:** Use `tracing::warn!` alongside or instead of the eprintln.

#### 3. HTTP server startup/status uses `eprintln!` exclusively *(NEW)*
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:103-123`
- **Description:** Six `eprintln!` calls for server startup: bind address, protocol version, auth status, security warning, shutdown. These bypass tracing entirely. The security warning ("Binding to {} WITHOUT authentication!") is particularly important to capture in structured logs for production deployments.
- **Suggested fix:** Add `tracing::warn!` for the no-auth binding warning. Add `tracing::info!` for bind address and protocol version alongside the eprintln calls.

#### 4. No timing breakdown within MCP search tool *(NEW)*
- **Difficulty:** medium
- **Location:** `src/mcp/tools/mod.rs:186-208` (dispatcher), `src/mcp/tools/search.rs` (handler)
- **Description:** The tool dispatcher logs overall elapsed_ms per tool call, which is good. But for the search tool (most complex path), there's no breakdown of where time is spent: embedder initialization, query embedding, index search, database fetch. When a search is slow, the outer timing says "slow" but not why. The `search_filtered` method has an `info_span` but no timing emission within it.
- **Suggested fix:** Add `tracing::debug!` with elapsed times for key phases in `tool_search`: embedder init, query embedding, index search, result fetch.

#### 5. CPU embedder thread errors terminate silently *(NEW)*
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:487-488`
- **Description:** In the CPU embedder thread (Stage 2b), if `embedder.embed_documents()` fails, the `?` propagates and terminates the thread. The GPU thread logs its failures (`tracing::warn` at pipeline.rs:433), but CPU failures only surface as "CPU embedder thread panicked" at join time (pipeline.rs:567) without the actual error message. If GPU fails for the same batch, chunks are silently lost with no diagnostic trail in the logs.
- **Suggested fix:** Wrap the CPU embed_documents call in a match with `tracing::error!` before returning the error, consistent with how the GPU thread handles failures.

#### 6. `extract_call_graph` has no span or progress indication *(NEW)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:166-188`
- **Description:** Iterates over all files extracting call relationships with no tracing span and no progress logging. For large codebases this can take significant time. Individual file errors are logged at warn (line 183) but there's no span to correlate them or measure total extraction time.
- **Suggested fix:** Add `tracing::info_span!("extract_call_graph", file_count = files.len())`.

#### 7. `build_hnsw_index` has no tracing span *(NEW)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:257-280`
- **Description:** Builds HNSW index with no outer tracing span. The inner `build_batched` logs progress but there's no span for the overall operation. Build timing isn't available in structured logs.
- **Suggested fix:** Add `tracing::info_span!("build_hnsw_index", chunks = chunk_count, notes = note_count)`.

#### 8. No observability for query embedding cache hits/misses *(NEW)*
- **Difficulty:** easy
- **Location:** `src/embedder.rs` (query embedding LRU cache)
- **Description:** The embedder has an LRU cache for query embeddings (32 entries) but no logging of hit/miss. For MCP server usage where queries may repeat, understanding cache effectiveness would help tune the size. Currently no way to tell if the cache is helping.
- **Suggested fix:** Add `tracing::trace!` for cache hits and `tracing::debug!` for misses. Low priority.

#### 9. CAGRA test helper uses `eprintln!` for skip messages
- **Difficulty:** trivial
- **Location:** `src/cagra.rs:510`
- **Description:** `eprintln!("Skipping CAGRA test: no GPU available")` in test helper. This is in test code so it's minor, but `println!` or a test framework mechanism would be more appropriate than stderr for test skip notifications.
- **Suggested fix:** Accept as-is (test code only).

**Summary:** 9 findings (1 overlap with Module Boundaries, 8 new). Most old findings (O2-O17) confirmed fixed. The codebase has good overall observability -- tracing is used consistently in core paths with structured fields. Main gaps: timing granularity for MCP search diagnosis, silent CPU thread termination in pipeline, missing spans for call graph extraction and HNSW build, and `eprintln!` in HTTP transport startup.

---

### Test Coverage

**Cross-reference:** Previous audit found T1-T17. Verification status of each below. New findings follow.

**Previous findings status:**

| ID | Issue | Status |
|----|-------|--------|
| T1 | `index_notes()` no tests | **STILL OPEN** - `lib.rs:102` `index_notes()` is tested indirectly via MCP `cqs_add_note` tests (mcp_test.rs:877) but has no direct unit test. Requires `Embedder` (ML model) so integration-only. |
| T3 | Store call graph methods untested | **FIXED** - 10 tests in `tests/store_calls_test.rs` covering upsert, callers, callees, call_stats, edge cases |
| T4-T6 | Note search/embeddings/stats untested | **FIXED** - 8 tests in `tests/store_notes_test.rs` covering search_notes_by_ids, note_embeddings, note_stats |
| T7 | `embedding_batches()` no direct test | **FIXED** - 3 tests in `tests/store_test.rs:571-632` (normal, empty, exact multiple) |
| T8 | `prune_missing()` edge cases | **FIXED** - test at `tests/store_test.rs:164-191` covers prune with existing/missing files |
| T10 | `search_filtered` untested | **PARTIALLY FIXED** - tested via `test_search_filtered_by_language` (store_test.rs:84), `test_rrf_search` (store_test.rs:338), and Unicode tests. But no test for glob path_pattern filtering or name_boost hybrid scoring |
| T11 | `search_by_candidate_ids` untested | **STILL OPEN** - zero direct tests. Called only by `search_filtered_with_index` which is also untested |
| T14 | HNSW error paths | **FIXED** - 7 tests in `tests/hnsw_test.rs` covering truncation, corruption, missing files, dimension mismatches, batched errors |
| T15 | Tests use weak assertions | **MOSTLY FIXED** - only 4 `.is_ok()` assertions remain (2 in Unicode FTS tests where crash-prevention is the goal, 1 in stress test, 1 in TOML parse check). All critical paths use strong assertions |
| T16 | Unicode handling | **FIXED** - 4 tests in `tests/store_test.rs:636-683` plus MCP unicode query test |
| T17 | Empty input edge cases | **FIXED** - empty tests across store_calls, store_notes, MCP test (empty query, whitespace-only query) |

**New findings:**

#### 1. `search_by_candidate_ids()` has zero tests (CRITICAL)
- **Difficulty:** medium
- **Location:** `src/search.rs:433-566`
- **Description:** This is the HNSW-guided search path -- the primary search codepath when an HNSW index is loaded. It handles candidate filtering, language filters, glob matching, name boosting, parent deduplication, and scoring. None of these behaviors have any test coverage. This function is called by `search_filtered_with_index()` and `search_unified_with_index()`, both also untested.
- **Impact:** The main production search path (HNSW-backed) is completely untested. Bugs in filtering, scoring, or dedup would go undetected.
- **Suggested fix:** Add tests using TestStore: insert chunks, then call `search_by_candidate_ids` with known candidate IDs and verify filtering, scoring, and ordering.

#### 2. `search_unified_with_index()` has zero tests (CRITICAL)
- **Difficulty:** medium
- **Location:** `src/search.rs:572-653`
- **Description:** The unified search (code + notes merged) has no tests. This handles the core search orchestration: index-guided search, note/chunk partitioning from HNSW candidates, slot allocation (60/40 code/notes), note_weight attenuation, and final merge-sort. The slot allocation math at lines 623-626 is non-obvious and untested.
- **Suggested fix:** Test with a mock VectorIndex returning mixed chunk and note IDs. Verify slot allocation, note_weight attenuation, and merge ordering.

#### 3. `search_filtered_with_index()` has zero tests (MEDIUM)
- **Difficulty:** medium
- **Location:** `src/search.rs:404-430`
- **Description:** The index-guided search dispatch (calls HNSW, falls back to brute-force) has no tests. The fallback path when index returns empty candidates is untested.
- **Suggested fix:** Test both paths: with mock index returning results, and with mock index returning empty (should fall back to brute-force).

#### 4. `search_filtered()` glob path filtering untested (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/search.rs:252-260` (glob compilation), `src/search.rs:293-298` (glob matching)
- **Description:** The glob path_pattern filter in `search_filtered()` is never tested. Tests exist for language filtering and RRF, but not for path patterns. Invalid glob behavior (silently ignored) is also untested.
- **Suggested fix:** Insert chunks with different file paths, search with `path_pattern: Some("src/**")`, verify only matching paths returned.

#### 5. `search_filtered()` name_boost hybrid scoring untested (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/search.rs:229`, `src/search.rs:285-291`
- **Description:** The hybrid scoring path (`name_boost > 0.0`) in `search_filtered()` is never tested at the Store level. The `NameMatcher` itself has unit tests in `search.rs` but the integration (embedding_score blended with name_score) is untested.
- **Suggested fix:** Insert chunks with different names, search with name_boost=0.5 and query_text matching one name, verify ranking.

#### 6. `Store::search_notes()` has zero tests (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:118-164`
- **Description:** The brute-force note search (no HNSW, iterates all notes) has no direct test. `search_notes_by_ids` is well-tested, but the unguided version is only exercised indirectly through `search_unified_with_index`. Threshold filtering, score ordering, and limit enforcement for notes are untested.
- **Suggested fix:** Add tests in `store_notes_test.rs` for `search_notes()` with threshold and limit.

#### 7. `Store::search_by_name()` has zero tests (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:412-534`
- **Description:** The FTS name-only search method (used by MCP `name_only` mode) has no direct tests. It constructs a custom FTS5 column-scoped query (`name:X OR name:X*`), does embedding scoring, and merges with FTS ranking. This is the `cqs_search(name_only=true)` codepath.
- **Suggested fix:** Add tests in `store_test.rs`: insert named chunks, call `search_by_name("parseConfig", 5)`, verify results contain matching names.

#### 8. Full call graph methods (`upsert_function_calls`, `get_callers_full`, `get_callees_full`, `function_call_stats`) untested (MEDIUM)
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:118-245`
- **Description:** The v5 full call graph methods (which operate on the `function_calls` table, separate from the chunk-level `calls` table) have zero tests. The chunk-level call graph (T3) was fixed with 10 tests, but these newer methods are completely untested. They are used by `src/cli/commands/graph.rs` and `src/mcp/tools/call_graph.rs`.
- **Suggested fix:** Add tests in `store_calls_test.rs` for the function_calls table operations.

#### 9. `Store::close()` untested (LOW)
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:534`
- **Description:** `close(self)` consumes the Store and closes the connection pool. Never tested. Could leak connections if broken.
- **Suggested fix:** Add a simple test that opens, uses, and closes a store without error.

#### 10. `Store::search_fts()` empty-after-normalize path untested (LOW)
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:385-410`
- **Description:** When the query normalizes to empty string (e.g., all special characters), `search_fts` should return empty. The `normalize_for_fts` tests cover the normalization, but the `search_fts` early-return on empty is never tested end-to-end.
- **Suggested fix:** Call `search_fts("***", 5)` and verify empty result.

#### 11. `prune_missing()` batch boundary untested (LOW)
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:176`
- **Description:** `prune_missing` batches deletes in groups of 100. Tests only cover 1 missing file. The batch boundary (>100 missing files) and the FTS cleanup within the batch are untested.
- **Suggested fix:** Insert chunks from 150+ distinct files, prune with only 1 existing, verify all pruned correctly.

#### 12. No C/Java parser integration tests (LOW)
- **Difficulty:** easy
- **Location:** `src/language/c.rs`, `src/language/java.rs`
- **Description:** `parser_test.rs` has tests for Rust, Python, TypeScript, JavaScript, Go but none for C or Java (added in v0.5.0). The languages have their own inline unit tests in `src/language/c.rs` and `src/language/java.rs`, but no integration tests via `Parser::parse_file()` with fixture files.
- **Suggested fix:** Add `sample.c` and `sample.java` to `tests/fixtures/` and add parser integration tests.

**Summary:** 3 critical gaps (index-guided search paths completely untested), 5 medium gaps (name search, note search, full call graph, glob filtering, hybrid scoring), 4 low gaps (close, FTS edge case, prune batching, C/Java fixtures). The most impactful fix would be testing `search_by_candidate_ids` and `search_unified_with_index` since they are the primary production search paths when HNSW is loaded.

---

### Algorithm Correctness

**Scope:** Off-by-one errors, boundary conditions, logic bugs, incorrect formulas, edge case handling in search/scoring/parsing/indexing.

**Cross-reference with prior audit (AC1-AC13):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| AC1 | RRF constant K=60 | ACCEPTED | `store/mod.rs:490`. Standard constant from the RRF paper. |
| AC2 | Cosine similarity NaN on zero vectors | FIXED | `math.rs:14-17` returns `None` when either norm is zero. |
| AC3 | HNSW search returns fewer than K results | FIXED | `hnsw.rs:114` uses `ef_search.max(k)` to ensure enough candidates. |
| AC4 | CAGRA itopk_size too small | FIXED | `cagra.rs:167` uses `(k*2).max(128)` with documented rationale. |
| AC5 | display.rs off-by-one in context lines | NOT A BUG | Traced multiple scenarios: `start_line.saturating_sub(context)` and `end_line + context` produce correct 0-indexed ranges. The `lines()` iterator naturally handles the bounds. |
| AC6 | Embedding window overlap validation | FIXED | `embedder.rs:325-329` asserts `overlap < max_tokens / 2`. |
| AC7 | BoundedScoreHeap ordering | FIXED | Min-heap correctly evicts lowest score. `push()` at `search.rs:44-55` properly maintains heap invariant. |
| AC8 | FTS ranking sign | FIXED | `search.rs:316` correctly negates `bm25()` (SQLite returns negative BM25 scores, negation makes higher = better). |
| AC9 | Note sentiment dimension | FIXED | Sentiment appended as 769th dimension at `lib.rs:131`, `store/notes.rs:68`. Consistent with `math.rs` 769-dim check. |
| AC10 | Parent dedup in search | FIXED | `search.rs:533-547` correctly keeps highest-scoring child per parent, with proper parent_id extraction from `origin:start-end` format. |
| AC11 | Batch iterator OFFSET bug | FIXED | `store/chunks.rs:299-310` uses `rows_fetched` as offset, not batch index multiplication. |
| AC12 | clamp_line_number edge cases | FIXED | `store/helpers.rs:172` uses `n.clamp(1, u32::MAX as i64)` correctly handling negative and overflow. |
| AC13 | FTS mid-word truncation | FIXED | `nl.rs:185-191` uses `rfind(' ')` to truncate at word boundary before `MAX_FTS_OUTPUT_LEN`. |

**All 13 previously reported issues confirmed resolved or accepted.**

**New findings:**

#### 1. `search_filtered` applies glob filter AFTER heap insertion -- can return fewer results than limit
- **Difficulty:** medium
- **Location:** `src/search.rs:271-302`
- **Description:** The brute-force search path processes ALL chunks: scores each one, pushes to `BoundedScoreHeap(limit)`, then AFTER collecting the top-N, filters by glob pattern (lines 293-302). If 8 of the top-10 results have non-matching paths, the user gets only 2 results despite there being many more matching chunks below the heap cutoff. The heap evicts potentially-matching lower-scored results to make room for higher-scored non-matching ones.
- **Impact:** Users with `path_pattern` filters may see unexpectedly few results. Worsens as the codebase grows and path-filtered results become a smaller fraction.
- **Suggested fix:** Apply glob filter BEFORE heap insertion (between scoring and push), so only matching results enter the heap. This ensures the heap always contains the top-N matching results.

#### 2. Path extraction inconsistency between brute-force and HNSW search paths
- **Difficulty:** medium
- **Location:** `src/search.rs:294` vs `src/search.rs:519`
- **Description:** Brute-force `search_filtered` extracts the file path for glob matching via `id.split(':').next()` (line 294), where `id` is the chunk's composite ID (format: `path:start-end`). HNSW-guided `search_by_candidate_ids` uses `chunk_row.origin` (line 519), which is the raw `origin` column from the database. On Windows, paths contain colons (e.g., `C:\foo\bar.rs`), so `split(':').next()` returns just `C` instead of the full path. The HNSW path uses `origin` directly and handles this correctly.
- **Impact:** Glob path filtering in brute-force search silently fails on Windows for absolute paths. HNSW path works correctly.
- **Suggested fix:** Use `chunk_row.origin` (or equivalent) for path extraction in both code paths. Alternatively, split on the LAST colon-number pattern rather than the first colon.

#### 3. `cosine_similarity` hardcodes 769-dimension check
- **Difficulty:** easy
- **Location:** `src/math.rs:10-11`
- **Description:** `if a.len() != 769 || b.len() != 769 { return None; }` -- the dimension check is hardcoded to 769 rather than using the `EMBEDDING_DIM` constant. If the constant changes (defined in 4 places: `embedder.rs:59`, `hnsw.rs:63`, `cagra.rs:33`, `store/helpers.rs:20`), this function silently returns `None` for all comparisons, making every search return zero results with no error.
- **Impact:** Maintenance hazard. Works correctly today but a silent total failure if dimension changes.
- **Suggested fix:** Import and use `EMBEDDING_DIM` constant, or better: check `a.len() == b.len()` since cosine similarity is valid for any matching dimension.

#### 4. `extract_calls` line offset can produce wrong line numbers
- **Difficulty:** easy
- **Location:** `src/parser.rs:395-397`
- **Description:** `extract_calls()` adds `line_offset` to each call's row: `row: (call.start_position().row + line_offset) as u32`. The `line_offset` parameter is `usize` and `start_position().row` is also `usize`, so the addition can't overflow on 64-bit. However, the cast to `u32` truncates silently if the sum exceeds `u32::MAX` (4 billion lines -- not realistic). More concerning: `line_offset` is computed by callers as `chunk.line_start` (1-indexed from the parser) but `start_position().row` is 0-indexed from tree-sitter. If `line_start` is 1-indexed and `row` is 0-indexed, the result is off-by-one high. Checking callers: `parser.rs:271` passes `chunk.line_start` which comes from `node.start_position().row` at line 157 -- also 0-indexed. So the offset is correct (0-indexed + 0-indexed = correct absolute position). No bug, but fragile.
- **Impact:** No current bug. Fragile if anyone passes a 1-indexed line number as offset.
- **Suggested fix:** Add a doc comment to `extract_calls` clarifying that `line_offset` must be 0-indexed.

#### 5. BoundedScoreHeap tie-breaking is insertion-order dependent (non-deterministic)
- **Difficulty:** easy
- **Location:** `src/search.rs:44-55`
- **Description:** When two results have the same score, `BoundedScoreHeap` keeps whichever was inserted first (since the heap only evicts when `new_score > min_score`, equal scores don't evict). With brute-force search iterating database rows, the order depends on SQLite's internal row ordering, which can change after VACUUM or schema changes. This means search results with tied scores are non-deterministic across database operations.
- **Impact:** Low -- exact score ties are rare with floating-point cosine similarity. But for name-only searches or FTS-boosted results, ties are more common.
- **Suggested fix:** Accept as-is. Tie-breaking by insertion order is standard for heap-based top-K. Document the behavior if deterministic ordering is ever needed.

#### 6. Parser line count uses span instead of count (off-by-one in chunk limit)
- **Difficulty:** easy
- **Location:** `src/parser.rs:166-168`
- **Description:** `let lines = line_end - line_start;` computes the span (difference) rather than the count (difference + 1). Then `if lines > 100` triggers chunking. This means a function spanning lines 0-100 has `lines = 100`, passes the check, and is NOT chunked -- but it's actually 101 lines (0 through 100 inclusive). The effective limit is 101 lines, not 100.
- **Impact:** Very low. The difference is 1 line on the boundary. Functions of exactly 101 lines are kept whole instead of being split.
- **Suggested fix:** Change to `let lines = line_end - line_start + 1;` or change the comparison to `if lines >= 100`. Low priority.

#### 7. RRF fusion does not deduplicate inputs
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:492-504`
- **Description:** The RRF fusion loop iterates over `embedding_results` and `fts_ids`, summing reciprocal ranks. If a result appears in both lists (which is expected -- that's the point of fusion), it gets scores from both. However, if a result appears TWICE within the same input list (e.g., `fts_ids` contains duplicates), it gets double the FTS score. Currently `search_fts` returns results from a SQL query with no DISTINCT, so duplicates could theoretically occur if FTS5 returns them.
- **Impact:** Theoretical. FTS5 should not return duplicates for a single query, but the code doesn't guard against it.
- **Suggested fix:** Accept as-is. FTS5 doesn't produce duplicates for simple queries. A `DISTINCT` in the FTS query would be the safest fix if this ever becomes a concern.

**Summary:** 7 new findings. 2 actionable (#1 glob-after-heap is the most impactful, #2 Windows path extraction is a real bug on Windows). 1 maintenance hazard (#3 hardcoded dimension). 4 minor/accepted (#4 doc comment, #5 tie-breaking, #6 off-by-one-at-boundary, #7 theoretical dedup).

---

### Platform Behavior

**Scope:** OS differences, path handling, WSL issues, Windows compatibility, Unix-specific code without cfg guards, path separators, line endings, file permissions.

**Cross-reference with prior audit (PB1-PB10):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| PB1 | Unix-only symlink | FIXED | `embedder.rs:579` has `#[cfg(unix)]`, `embedder.rs:673` has `#[cfg(not(unix))]` no-op stub. Confirmed. |
| PB2 | Hardcoded Linux cache path | FIXED | `embedder.rs:582` uses `dirs::cache_dir()`. Confirmed. |
| PB3 | $HOME assumption | FIXED | All cache paths go through `dirs::cache_dir()`. Confirmed. |
| PB4 | LD_LIBRARY_PATH | FIXED | Entire `ensure_ort_provider_libs()` at `embedder.rs:579` is `#[cfg(unix)]` guarded. Colon separator at line 622 is safe within the unix guard. Confirmed. |
| PB5 | Colon path separator | FIXED | Colon split at `embedder.rs:622` is inside `#[cfg(unix)]` block. Confirmed. |
| PB6 | Path in DB URL | FIXED | `store/mod.rs:104` normalizes backslashes to forward slashes for SQLite URL. Confirmed with comment. |
| PB7 | Chunk ID path separators | FIXED | `cli/pipeline.rs:247` normalizes `rel_path.to_string_lossy().replace('\\', "/")` before building chunk ID. Confirmed. |
| PB8 | JSON output path slashes | FIXED | All JSON output points normalize with `.replace('\\', "/")`: `cli/display.rs:178`, `mcp/tools/search.rs:36,123`. Confirmed. |
| PB9 | WSL file watching | OK | `cli/watch.rs:67-69` uses `notify::RecommendedWatcher` with `Config::with_poll_interval()`. On WSL, `RecommendedWatcher` automatically falls back to polling for cross-filesystem access. Confirmed adequate. |
| PB10 | UNC paths | FIXED | `strip_unc_prefix` implemented at `cli/files.rs:19-33` and `mcp/validation.rs:104-118`, both with `#[cfg(windows)]`/`#[cfg(not(windows))]` pairs. Confirmed. |

**All 10 previously-reported issues confirmed fixed.**

**New findings:**

#### 1. `call_graph.rs` and `graph.rs` JSON output paths not normalized *(NEW)*
- **Difficulty:** easy
- **Location:** `src/mcp/tools/call_graph.rs:28`, `src/cli/commands/graph.rs:40`
- **Description:** Both the MCP `tool_callers` and CLI `cmd_callers` functions output `c.file.to_string_lossy()` in JSON without normalizing backslashes to forward slashes. All other JSON output paths (`cli/display.rs:178`, `mcp/tools/search.rs:36,123`) consistently apply `.replace('\\', "/")`. On Windows, callers results will contain backslash paths in JSON while search results use forward slashes, creating inconsistency for consumers parsing the JSON.
- **Impact:** Low on Unix (backslashes don't appear). On Windows/WSL, JSON output is inconsistent between search and call graph results.
- **Suggested fix:** Add `.replace('\\', "/")` to both `c.file.to_string_lossy()` calls, matching the pattern used everywhere else.

#### 2. `stats.rs` `index_path` not normalized *(NEW)*
- **Difficulty:** easy
- **Location:** `src/mcp/tools/stats.rs:55`
- **Description:** `server.project_root.join(".cq/index.db").to_string_lossy()` outputs the index path without backslash normalization. On Windows, this produces `C:\project\.cq\index.db` while all other paths in the same JSON response use forward slashes.
- **Impact:** Cosmetic inconsistency in MCP stats output on Windows.
- **Suggested fix:** Add `.replace('\\', "/")` to match the convention.

#### 3. `FileSystemSource::enumerate()` stores origin with platform-native separators *(NEW)*
- **Difficulty:** medium
- **Location:** `src/source/filesystem.rs:131`
- **Description:** `origin: rel_path.to_string_lossy().into_owned()` stores the origin field with platform-native path separators. On Windows, this means backslash-separated origins stored in the database. The `cli/pipeline.rs:247` normalizes paths before storing chunk IDs, but the Source abstraction doesn't. If `FileSystemSource` is used on Windows, stored origins will have backslashes while the CLI pipeline normalizes to forward slashes, creating a mismatch between the two indexing paths.
- **Impact:** Medium. The Source abstraction (`src/source/mod.rs`) is designed for future extensibility. If `FileSystemSource` is used as the primary indexing path on Windows (instead of the CLI pipeline), origins will be stored inconsistently.
- **Suggested fix:** Normalize origin to forward slashes: `origin: rel_path.to_string_lossy().replace('\\', "/")`.

#### 4. `process_exists` Windows implementation uses shell-out to `tasklist` *(ACCEPTED - noting risk)*
- **Difficulty:** low
- **Location:** `src/cli/files.rs:127-135`
- **Description:** The Windows `process_exists()` implementation shells out to `tasklist` and parses stdout. This has several platform behavior concerns: (a) `tasklist` may not be available on Windows Server Core or minimal installations, (b) the string-matching `contains(&pid.to_string())` could false-positive if a PID substring matches another column (e.g., PID 23 matching "2300" or a memory value), (c) the `Command::new("tasklist")` creates a visible process on each lock check. The Unix implementation (`libc::kill(pid, 0)`) is a single syscall with no such issues.
- **Impact:** Low. Lock contention is rare. False positives would just delay reporting a stale lock, not cause data loss.
- **Suggested fix:** Accept as-is. A more robust Windows implementation would use `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION`, but this adds `winapi` dependency for a rare edge case. If this becomes a concern, consider the `sysinfo` crate.

#### 5. `sanitize_error_message` regex doesn't handle WSL `/mnt/c/` paths *(NEW)*
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214`
- **Description:** The Unix path sanitization regex matches `/home/`, `/Users/`, `/tmp/`, `/var/`, `/usr/`, `/opt/`, `/etc/` but NOT `/mnt/` -- the standard WSL mount point for Windows drives. In WSL (the primary development environment per CLAUDE.md), paths like `/mnt/c/Projects/cq/src/main.rs` would leak through unsanitized in MCP error messages. The `project_root` substitution (line 208) catches the project path specifically, but any error referencing paths outside the project (e.g., model files, temp files) would expose the `/mnt/c/` prefix.
- **Impact:** Low security concern (localhost-only service). More of a information hygiene issue.
- **Suggested fix:** Add `/mnt/` to the regex pattern: `r"/(?:home|Users|tmp|var|usr|opt|etc|mnt)/[^\s:]+"`.

#### 6. CRLF normalization absent in watch mode reindexing *(NEW)*
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:220-241`
- **Description:** The `reindex_files()` function in watch mode reads files via `parser.parse_file()`, which internally calls `std::fs::read_to_string()` at `parser.rs:105`. The parser then normalizes CRLF to LF at `parser.rs:135` (`source.replace("\r\n", "\n")`). However, the content hashes computed during initial indexing (via `cli/pipeline.rs`) use the parsed content (post-normalization), while watch mode reindexing also goes through the parser. So CRLF normalization IS consistently handled through the parser.
- **Status:** NOT A BUG. Traced the full call path. Both the pipeline and watch mode go through `parser.parse_file()` which normalizes CRLF. `FileSystemSource` also normalizes at `source/filesystem.rs:117`. All three indexing paths are consistent.

#### 7. `notify` watcher behavior differs between native and WSL filesystems *(INFORMATIONAL)*
- **Difficulty:** N/A
- **Location:** `src/cli/watch.rs:67-69`
- **Description:** On WSL2, `notify::RecommendedWatcher` uses `inotify` for native Linux filesystems but must fall back to polling for Windows-mounted filesystems (`/mnt/c/`). The `Config::with_poll_interval()` at line 67 sets the poll interval, which is used for both polling and `inotify` debouncing. For projects on `/mnt/c/`, watch mode may have higher latency and CPU usage compared to native Linux filesystems. This is inherent to WSL2's 9P filesystem driver and not a bug in cqs.
- **Impact:** Performance only. Functionally correct on both native and mounted filesystems.
- **Suggested fix:** Accept as-is. Consider documenting in README that watch mode works best on native Linux filesystems in WSL.

**Summary:** 5 actionable findings (#1-#3 path normalization gaps in JSON output, #5 WSL path sanitization). 1 accepted with noted risk (#4 Windows process_exists). 1 confirmed not-a-bug (#6 CRLF). 1 informational (#7 WSL watch behavior). All 10 previously-reported issues confirmed still fixed. The codebase has good overall platform behavior -- cfg guards are consistently applied, path separators are normalized in most places. The remaining gaps are minor inconsistencies in JSON output normalization for call graph and stats endpoints.

---

### Data Integrity

**Scope:** Corruption detection, validation, transactions, migrations, atomic writes, data consistency between SQLite and HNSW index files.

**Cross-reference with prior audit (DI1-DI15):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| DI1 | Non-atomic HNSW writes | FIXED | `hnsw.rs:498-607` writes to temp dir, then renames each file atomically. Checksum written last. |
| DI2-4 | Transactions on prune/upsert_calls | FIXED | `chunks.rs:125,177` (prune_missing), `calls.rs:20,41` (upsert_calls) all use `pool.begin()` + `tx.commit()`. |
| DI5 | Schema init not transactional | ACCEPTED | PRAGMAs are idempotent. `init()` at `store/mod.rs:179-233` executes schema statements individually. See finding #1 below for nuance. |
| DI6 | No embedding size validation | FIXED | `helpers.rs:409-416` asserts exact 769 dimensions on write. `helpers.rs:428-438,446-456` validates on read. |
| DI7 | Corrupted embeddings silently filtered | FIXED | `chunks.rs:442-448` logs warning when skipping corrupted embeddings during `all_embeddings()`. |
| DI8 | ID map/HNSW count mismatch | FIXED | `hnsw.rs:688-698` validates on load, `hnsw.rs:475-485` asserts on save. |
| DI9 | No foreign key enforcement | FIXED | `store/mod.rs:115` `PRAGMA foreign_keys = ON` in `after_connect`, runs for every connection. |
| DI10 | notes.toml ID collision (8 hex chars) | UPDATED | Now uses 16 hex chars = 64 bits. `note.rs:123-124`. Collision probability ~0.003% at 10k notes. |
| DI11 | No schema migration support | FIXED | `migrations.rs:29-53` implements step-by-step migration with version update. |
| DI13 | Checksum doc limitation | FIXED | `hnsw.rs:98-102` documents checksums detect corruption only, not tampering. |
| DI14 | Missing WAL checkpoint | FIXED | `store/mod.rs:547-560` `impl Drop for Store` does best-effort checkpoint. `close()` at line 534 does explicit TRUNCATE checkpoint. |
| DI15 | FTS/main table sync | FIXED | All FTS operations in same transaction as main table ops. `chunks.rs:60-75`, `notes.rs:92-102` both do FTS delete+insert within the transaction. |

**New findings:**

#### 1. `Store::init()` schema creation is not transactional
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:179-233`
- **Description:** The `init()` method executes each SQL statement from `schema.sql` individually without a transaction, then inserts metadata rows individually without a transaction. If the process crashes mid-init (e.g., after creating the `chunks` table but before `metadata`), the database is left in a partially initialized state: tables exist but `schema_version` metadata doesn't. On next `open()`, `check_schema_version()` at line 236 handles the "no such table" case but if `metadata` exists without `schema_version`, it defaults to version 0 and attempts migration from v0, which fails with `MigrationNotSupported`. The user would need to manually delete the corrupted database.
- **Impact:** Low. Init only runs once at `cq init` time. Crash during init is rare. But the recovery path is confusing.
- **Suggested fix:** Wrap the entire schema creation + metadata insertion in a single transaction. Since these are DDL + DML statements, SQLite allows them in a transaction.

#### 2. `index_notes()` delete + insert across two separate transactions
- **Difficulty:** medium
- **Location:** `src/lib.rs:146-152`
- **Description:** `index_notes()` calls `store.delete_notes_by_file(notes_path)?` (which runs its own transaction internally at `notes.rs:169-185`), then calls `store.upsert_notes_batch(...)` (which runs a separate transaction at `notes.rs:69-107`). If the process crashes between delete and insert, all notes from that file are lost with no way to recover except re-running `cq index`. The same pattern exists in `cli/commands/index.rs:238-242`.
- **Impact:** Low probability (crash between two fast operations), but data loss when it occurs. Notes disappear until next full reindex.
- **Suggested fix:** Add a `replace_notes_for_file()` method to Store that performs delete + insert in a single transaction. Both `index_notes()` and `index_notes_from_file()` should call it.

#### 3. HNSW index becomes stale after MCP note additions
- **Difficulty:** medium
- **Location:** `src/mcp/server.rs:61-63` (HNSW loaded once at startup), `src/mcp/tools/notes.rs:133` (notes added via MCP)
- **Description:** The MCP server loads the HNSW index once at startup (`server.rs:62`) and stores it in `Arc<RwLock<Option<Box<dyn VectorIndex>>>>`. When notes are added via `cqs_add_note` (MCP tool), they are stored in SQLite but the HNSW index is NOT rebuilt. The HNSW index becomes stale: new notes exist in SQLite but are missing from the HNSW vector index. Searches that use HNSW for candidate retrieval will never find these new notes. The brute-force fallback path (`search_notes()`) does scan all notes from SQLite, so `search_unified_with_index` at `search.rs:611-612` independently fetches notes by ID from SQLite after HNSW candidate retrieval -- but only for note IDs that HNSW returned. New notes not in HNSW are invisible.
- **Impact:** Medium. Notes added via MCP during an active session are invisible to HNSW-guided search until the user manually runs `cq index` to rebuild HNSW.
- **Suggested fix:** Either: (a) always fall back to brute-force `search_notes()` for the notes portion of unified search (note count is capped at 10k, brute-force is acceptable), or (b) document that HNSW rebuild is needed after MCP note additions, or (c) invalidate the HNSW index when notes change, forcing brute-force until next rebuild.

#### 4. `embedding_batches()` LIMIT/OFFSET pagination unstable during concurrent writes
- **Difficulty:** low
- **Location:** `src/store/chunks.rs:499`
- **Description:** `embedding_batches()` uses `SELECT id, embedding FROM chunks LIMIT ?1 OFFSET ?2` for paginated reads. In WAL mode with concurrent writes (e.g., watch mode inserting new chunks while HNSW build reads embeddings), rows can be inserted or deleted between batch fetches. LIMIT/OFFSET pagination sees a snapshot per query, not per iteration, so: (a) a row inserted after batch N but before batch N+1 might be missed if it falls in already-read offset range, (b) a deleted row shifts offsets, potentially duplicating or skipping subsequent rows.
- **Impact:** Low. HNSW building typically happens during `cq index` which holds exclusive control. Watch mode does not rebuild HNSW. But the code doesn't enforce this -- concurrent build + insert is theoretically possible.
- **Suggested fix:** Add `ORDER BY rowid` to ensure stable pagination order (SQLite rowid is monotonically increasing for non-vacuumed tables). Or use cursor-based pagination with `WHERE rowid > ?` instead of OFFSET. Alternatively, document that `embedding_batches` must not be called concurrently with writes.

#### 5. `cosine_similarity` accepts NaN/Infinity without validation
- **Difficulty:** easy
- **Location:** `src/math.rs:10-22`
- **Description:** `cosine_similarity()` performs a dot product but does not check if either input vector contains NaN or Infinity values. If a corrupted embedding with NaN values is stored in SQLite (e.g., due to a bug in the embedder or a bitflip in the BLOB), the dot product will return NaN, which propagates silently through scoring and sorting. NaN comparisons in `partial_cmp` return `None`, which the code handles as `Ordering::Equal` -- meaning a NaN-scored result can appear at any position in results, displacing valid results.
- **Impact:** Low probability (embedder outputs are validated by dimension, but not by value). If it occurs, search results become unreliable with no error indication.
- **Suggested fix:** Check `score.is_finite()` after computing cosine similarity and return `None` for non-finite results. This would filter corrupted embeddings at the scoring layer.

#### 6. `bytes_to_embedding` and `embedding_slice` use `trace` level logging for corruption
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:431-436, 449-454`
- **Description:** When an embedding has the wrong byte length (indicating corruption or schema change), both `embedding_slice` and `bytes_to_embedding` log at `trace` level. Trace is the lowest level and is typically disabled in production. The comment says "Uses trace level logging to avoid impacting search performance" but this means actual data corruption goes unnoticed unless trace-level logging is explicitly enabled. The `all_embeddings()` wrapper at `chunks.rs:442` does aggregate and log at `warn`, but the per-row path during search (`search_filtered` at `search.rs:278-280`) silently drops corrupted rows with no aggregation or warning.
- **Impact:** Low. Corruption detection works (bad embeddings are skipped, not used). But the user gets no indication that results are degraded due to corrupted data.
- **Suggested fix:** Keep trace-level per-row logging for hot paths. Add an aggregated warning counter in `search_filtered` -- count skipped rows and log once at `warn` after the scan if `skipped > 0`.

#### 7. No validation that HNSW index was built from current SQLite data
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:621-711` (load), `src/mcp/server.rs:119-134` (load at startup)
- **Description:** When the HNSW index is loaded from disk, it is validated for internal consistency (checksum verification, id_map/vector count match). However, there is no check that the HNSW index corresponds to the current SQLite database content. If the user runs `cq index` to rebuild SQLite but the HNSW build step fails or is interrupted, the old HNSW index remains on disk while SQLite has new data. On next startup, the MCP server loads the stale HNSW index. Search returns results for chunks that no longer exist (HNSW has IDs not in SQLite) or misses chunks that were added (SQLite has IDs not in HNSW). The `search_by_candidate_ids` path at `search.rs:458-463` handles missing IDs gracefully (returns fewer results), but excess stale IDs waste search budget.
- **Impact:** Medium. Occurs when HNSW build is interrupted. Recovery requires manual `cq index` rerun. No diagnostic indicates the index is stale.
- **Suggested fix:** Store a hash or timestamp of the SQLite state (e.g., `chunk_count + note_count + max_updated_at`) in the HNSW checksum file. On load, compare against current SQLite state. Warn or auto-rebuild if mismatched.

**Summary:** 7 new findings. All 15 previously-reported DI issues confirmed fixed or accepted.

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| 1 | init() not transactional | medium | low |
| 2 | index_notes() non-atomic delete+insert | medium | low-medium |
| 3 | HNSW stale after MCP note additions | medium | medium |
| 4 | embedding_batches() unstable pagination | low | low |
| 5 | NaN/Infinity not validated in cosine_similarity | easy | low |
| 6 | Corruption logging at trace level during search | easy | low |
| 7 | No HNSW-SQLite freshness validation | medium | medium |

Most impactful: #3 (HNSW stale after MCP changes) and #7 (no HNSW-SQLite freshness check) are the most likely to affect real users. #2 (non-atomic note replacement) is the most likely to cause data loss.

---

### Memory Management

**Scope:** Unbounded allocations, OOM paths, memory scaling at 100k+ chunks, streaming vs bulk loading, buffer management, cache sizing.

**Cross-reference with prior audit (MM1-MM10):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| MM1 | Notes scan unbounded | FIXED | `store/notes.rs:125` defines `MAX_NOTES_SCAN: i64 = 1000` and line 140 applies `LIMIT ?` in SQL query. Confirmed. |
| MM2 | CAGRA build loads all embeddings | FIXED | `cagra.rs:373-451` `build_from_store()` uses `embedding_batches(10_000)` streaming iterator. Confirmed. |
| MM3 | HnswIndex::build takes full Vec | DOCUMENTED | `hnsw.rs:192` `build()` accepts `Vec<(String, Vec<f32>)>`. Comment at line 190 documents this is intentional -- HNSW requires random access during construction. Confirmed. |
| MM4 | (not in scope) | -- | -- |
| MM5 | Search results unbounded | FIXED | `search.rs:125-184` `BoundedScoreHeap` caps output to `limit` entries using a min-heap with O(limit) memory. Confirmed. |
| MM6 | FileSystemSource collects all paths | DOCUMENTED | `source/filesystem.rs` collects all paths into `Vec<SourceFile>`. Documented as acceptable (~7MB for Linux kernel scale). Confirmed. |
| MM7 | HNSW checksum reads entire file | FIXED | `hnsw.rs:127-143` `verify_hnsw_checksums()` uses `BufReader` + `io::copy` for streaming checksum verification. Confirmed. |
| MM8 | (not in scope) | -- | -- |
| MM9 | MCP read no size limit | FIXED | `mcp/tools/read.rs:40` defines `MAX_FILE_SIZE: u64 = 10 * 1024 * 1024` (10MB) guard. Confirmed. |
| MM10 | embed_documents temp strings | ACCEPTED | `embedder.rs` `embed_documents()` creates `format!("passage: {}", t)` per doc. Batch size is 32, so at most 32 temporary strings. Acceptable. |

**All 8 previously-reported issues confirmed fixed/accepted/documented.**

**New findings:**

#### 1. `search_filtered()` loads ALL embeddings into memory *(NEW)*
- **Difficulty:** medium
- **Location:** `src/search.rs:239-244`
- **Description:** `search_filtered()` calls `store.get_embeddings_by_hashes(candidates)` which returns ALL matching embeddings as a `Vec<(String, Vec<f32>)>`. With a filter that matches many chunks (e.g., broad language filter), this loads the entire embedding set into memory. At 100k chunks with 769-dim f32 embeddings: 100,000 * 769 * 4 bytes = ~292MB, plus String IDs and Vec overhead. This is the brute-force fallback path used when HNSW can't serve filtered queries.
- **Impact:** High memory spike on filtered searches at scale. Mitigated by HNSW being the normal search path -- `search_filtered` is only used when filters exclude enough vectors that brute-force is needed. However, there's no memory budget or streaming fallback.
- **Suggested fix:** Stream embeddings in batches through the brute-force scorer rather than loading all at once. Use `EmbeddingBatchIterator` (already exists in `store/chunks.rs`) with a filter predicate, scoring each batch against `BoundedScoreHeap` incrementally.

#### 2. HNSW `save()` reads entire files for checksumming *(NEW)*
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:553-563`
- **Description:** `save()` computes checksums by calling `std::fs::read(&graph_path)?` and `std::fs::read(&data_path)?`, reading entire files into memory. The HNSW data file at 100k chunks is ~292MB (100k * 769 * 4). This is inconsistent with `verify_hnsw_checksums()` (line 127-143) which correctly uses streaming `BufReader` + `io::copy` for the same operation. The save path allocates the full file as a `Vec<u8>` just to hash it.
- **Impact:** ~300MB transient allocation during index save. Happens once per index build, not per search. Mitigated by being a one-time operation, but still a significant spike.
- **Suggested fix:** Use the same streaming pattern as `verify_hnsw_checksums`: `BufReader::new(File::open(&path)?)` with `io::copy(&mut reader, &mut hasher)`. Direct port of the existing load-side code.

#### 3. `count_vectors()` deserializes entire JSON ID map *(NEW)*
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:724-745`
- **Description:** `count_vectors()` reads and fully deserializes the JSON ID map file (`serde_json::from_str::<HashMap<u32, String>>(&content)`) just to call `.len()` on the result. The ID map at 100k chunks is a JSON object with 100k entries -- roughly 5-10MB of JSON text, deserialized into a `HashMap` with 100k String allocations, only to count entries and drop everything.
- **Impact:** Wasteful but bounded. The ID map has a `MAX_ID_MAP_SIZE: u64 = 500 * 1024 * 1024` guard (line 634), so it won't exceed 500MB of JSON text. Still, deserializing 10MB of JSON into a HashMap just to count entries is unnecessary.
- **Suggested fix:** Count `"` or `:` delimiters in the raw JSON string, or store the count separately in a metadata file. Alternatively, use a streaming JSON parser to count top-level keys without full deserialization.

#### 4. `note_embeddings()` loads all note embeddings unbounded *(ACCEPTED)*
- **Difficulty:** low
- **Location:** `src/store/notes.rs:251-269`
- **Description:** `note_embeddings()` calls `fetch_all()` loading all note embeddings into a `Vec`. Unlike chunk embeddings which have `embedding_batches()` for streaming, notes have no batch iterator. However, notes are inherently bounded by `MAX_NOTES_SCAN = 1000` and each note embedding is ~3KB (769 * 4 bytes), so maximum memory is ~3MB.
- **Impact:** Negligible. Notes are bounded and small.
- **Suggested fix:** Accept as-is. The 1000-note cap makes this safe.

#### 5. GPU-to-CPU failure path double-windows chunks *(NEW)*
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:172-195`
- **Description:** In the GPU embedding thread, chunks are first windowed via `prepare_for_embedding()` (line 159-163), which splits long chunks into overlapping token windows. If GPU embedding fails, the batch is sent to the CPU thread (line 185-188) via `cpu_tx`. The CPU thread then calls `prepare_for_embedding()` AGAIN (line 197-205) on the already-windowed chunks. This means chunks get double-windowed: a 200-line function is split into 2 windows, then each window is split again (likely no-op for windows that fit, but the splitting logic runs on already-split text). This is primarily a correctness bug, but has a memory side effect: the intermediate windowed representations are allocated twice.
- **Impact:** Correctness bug with minor memory waste. Double-windowing may produce different embeddings than single-windowing if the window boundaries differ. Memory impact is transient (only during GPU failure fallback) and bounded by batch size (32).
- **Suggested fix:** Send the original pre-windowed chunks to the CPU fallback path, not the already-windowed versions. Store the original batch before windowing so the CPU path can apply its own windowing.

#### 6. `get_embeddings_by_hashes()` unbounded SQL IN clause *(NEW)*
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:239-279`
- **Description:** `get_embeddings_by_hashes()` generates a SQL query with `WHERE content_hash IN (?,?,?,...,?)` with one placeholder per hash. At 100k chunks, this creates a SQL string with 100k placeholders and binds 100k parameters. SQLite has a default `SQLITE_MAX_VARIABLE_NUMBER` of 999 (can be compiled higher, but the Rust `sqlx` driver may use the default). Exceeding this limit causes a runtime SQL error.
- **Impact:** Will fail at scale if called with >999 hashes. Currently called from `search_filtered()` which passes candidates from FTS/filter results -- could be large with broad filters.
- **Suggested fix:** Batch the query into chunks of 500 hashes, executing multiple queries and concatenating results. Pattern: `for batch in hashes.chunks(500) { ... }`.

#### 7. CAGRA dataset retained permanently in memory *(ACCEPTED)*
- **Difficulty:** N/A
- **Location:** `src/cagra.rs:44-48`
- **Description:** `CagraIndex` holds `dataset: Array2<f32>` permanently because `cagra::SearchIndex::search()` consumes `self` and must be rebuilt. The dataset is needed for rebuilding after each search. At 100k chunks: 100k * 769 * 4 = ~292MB held permanently while the GPU index is active.
- **Impact:** Significant but by design. The CAGRA API requires this pattern. Users opt into GPU mode knowing it requires more memory.
- **Suggested fix:** Accept as-is. Document the memory requirement in README or help output for `--gpu` flag.

**Summary:** 7 new findings. 2 actionable (#1 search_filtered bulk load is the most impactful, #2 HNSW save checksumming is an easy fix mirroring existing streaming code). 1 moderate (#5 double-windowing is a correctness bug with memory side effect). 1 scaling concern (#6 SQL IN clause limit). 1 minor waste (#3 JSON count). 2 accepted (#4 notes bounded, #7 CAGRA by design). All 8 previously-reported MM fixes confirmed intact.

---

### Edge Cases

**Scope:** Empty inputs, huge inputs, unicode/CJK/emoji, malformed data, boundary conditions, zero-length strings, null bytes, very long paths.

**Cross-reference with prior audit (EC1-EC12):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| EC1 | Empty query string | FIXED | `embedder.rs:390-394` returns `EmptyQuery` for empty/whitespace. MCP `validate_query_length` at `validation.rs:14` checks `> 8192`. |
| EC2 | Non-UTF8 files | FIXED | `parser.rs:125-131` returns `Ok(Vec::new())` for non-UTF8. |
| EC3 | Very long identifiers | FIXED | `nl.rs:163-170` caps FTS output at 16KB (`MAX_FTS_OUTPUT_LEN`). |
| EC4 | Zero-length embedding | FIXED | `math.rs:14-17` returns `None` when norm is zero. |
| EC5 | Empty notes file | FIXED | `note.rs:113-120` returns empty vec for empty/whitespace TOML. |
| EC6 | Unicode in queries | FIXED | `store_test.rs:636-683` has 4 unicode tests (CJK, emoji, mixed, RTL). |
| EC7 | Null bytes in file paths | FIXED | Tree-sitter handles null bytes; `parser.rs:135` replaces CRLF but null bytes pass through as valid content. Path handling uses `OsStr`/`PathBuf` which handle null-terminated strings correctly. |
| EC8 | Empty chunk list | FIXED | `store/chunks.rs:25-29` early returns on empty batch. |
| EC9 | Single-character query | FIXED | Passes through embedding normally. FTS normalization handles single chars. |
| EC10 | Max path length | FIXED | `validation.rs:71` caps path_pattern at 500 chars. |
| EC11 | Duplicate chunk IDs | FIXED | `upsert_chunks_batch` uses `INSERT OR REPLACE` at `chunks.rs:38`. |
| EC12 | Empty search results | FIXED | All search paths return empty Vec, no unwrap on empty. |

**All 12 previously-reported edge case issues confirmed fixed.**

**New findings:**

#### 1. `parse_file_calls()` has no file size limit (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/parser.rs:433-534`
- **Description:** `parse_file()` has a 50MB file size guard at line 110 (`if metadata.len() > MAX_FILE_SIZE`), but `parse_file_calls()` at line 433 calls `std::fs::read_to_string(path)` with no size check. A multi-GB generated file (e.g., bundled JavaScript, protobuf output) would be read entirely into memory, then parsed by tree-sitter, potentially causing OOM. The call graph extraction path (`cli/commands/index.rs:182`) iterates ALL files and calls `parse_file_calls()` on each.
- **Impact:** OOM crash on very large files during call graph extraction. The main parsing path is protected but the call graph path is not.
- **Suggested fix:** Add `let metadata = std::fs::metadata(path)?; if metadata.len() > MAX_FILE_SIZE { return Ok(Vec::new()); }` before `read_to_string` in `parse_file_calls()`, matching the guard in `parse_file()`.

#### 2. `tokenize_identifier` produces no useful tokens for CJK/emoji-only identifiers (LOW)
- **Difficulty:** medium
- **Location:** `src/nl.rs:75-96`
- **Description:** `tokenize_identifier()` splits on `_`, `-`, ` `, and uppercase boundaries. A CJK identifier like `数据库连接` (database connection) passes through as a single unsplit token because CJK characters have no uppercase/lowercase distinction and contain no delimiters. This means name-matching for CJK identifiers only works on exact full-name match, not partial matches. For emoji identifiers (rare but valid in some languages), same behavior.
- **Impact:** Low. CJK identifiers are rare in practice. Name-matching for them won't benefit from token-level partial matching, but embedding-based search still works.
- **Suggested fix:** Accept as-is for now. CJK tokenization is a complex problem (word boundaries require dictionary lookup). The current behavior is correct (doesn't crash, doesn't corrupt), just less useful for CJK names.

#### 3. `normalize_for_fts` passes CJK characters through without splitting (LOW)
- **Difficulty:** medium
- **Location:** `src/nl.rs:126-176`
- **Description:** `normalize_for_fts()` strips non-alphanumeric characters, lowercases, and splits on whitespace. CJK characters pass `is_alphanumeric()` so they survive normalization, but they're not split into individual characters or words. A chunk with CJK comments produces a long string of concatenated CJK characters in the FTS index, which won't match FTS prefix queries well. FTS5 tokenizes by whitespace, so `数据库连接处理器` becomes one FTS token.
- **Impact:** Low. FTS search for CJK content works only on exact substring match, not word-level. Embedding-based search is unaffected.
- **Suggested fix:** Accept as-is. Proper CJK FTS tokenization requires ICU or jieba integration, which is significant complexity for a low-frequency use case.

#### 4. `search_by_name` constructs FTS5 query without escaping special characters (MEDIUM)
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:441-443`
- **Description:** `search_by_name()` constructs `"name:{normalized} OR name:{normalized}*"` where `normalized` comes from `normalize_for_fts()`. While `normalize_for_fts` strips most special characters, it preserves alphanumeric content including FTS5 operator keywords. If a user searches for `not`, `or`, or `and` (all valid function names), the normalized query becomes `name:not OR name:not*` where `not` is interpreted as an FTS5 operator, potentially causing unexpected results or query errors. The `nl.rs:109-116` comment acknowledges this gap: "Lowercase words that happen to match operators (e.g., 'or', 'not') survive normalization."
- **Impact:** Users searching for functions named `not`, `or`, `and`, `near` may get unexpected FTS5 behavior. These are uncommon but valid function names (especially `not` in Python, `and`/`or` in Ruby-style code).
- **Suggested fix:** Wrap each FTS5 term in double quotes: `"name:\"{normalized}\" OR name:\"{normalized}\"*"` to force literal interpretation. Or prefix-check for FTS5 operators and quote them specifically.

#### 5. Empty query string reaches `search_by_name` but not `embed_query` (LOW)
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:80-89`
- **Description:** In `name_only` mode, the MCP search handler skips `embed_query` (which has `EmptyQuery` validation) and calls `search_by_name` directly. An empty query string normalizes to empty via `normalize_for_fts`, producing FTS query `"name: OR name:*"` which is malformed FTS5 syntax. This would cause a SQLite error returned to the MCP client.
- **Impact:** Low. An empty `name_only` search returns a SQLite error instead of a clean "empty query" error message.
- **Suggested fix:** Add empty/whitespace check before calling `search_by_name` in the `name_only` path at `mcp/tools/search.rs:80`.

#### 6. `note_weight` and `name_boost` not validated for NaN/infinity (LOW)
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:294-344`
- **Description:** `SearchFilter::validate()` checks `name_boost` is in `[0.0, 1.0]` and `note_weight` is in `[0.0, 1.0]`, but NaN comparisons always return false in IEEE 754. If `name_boost = f64::NAN`, then `name_boost < 0.0` is false and `name_boost > 1.0` is false, so validation passes. NaN then propagates through scoring arithmetic, causing all search results to have NaN scores, which corrupts ordering.
- **Impact:** Low. NaN can only arrive via MCP JSON (which deserializes to f64). JSON doesn't have a NaN literal, but some JSON libraries produce `NaN` for certain edge cases.
- **Suggested fix:** Add `if name_boost.is_nan() || name_boost.is_infinite()` check, or use `!(0.0..=1.0).contains(&name_boost)` which correctly rejects NaN.

#### 7. Very long note mentions are unbounded (LOW)
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:34-39`
- **Description:** The MCP `tool_add_note` validates note text (2000 byte limit at line 22) and filters empty mentions (line 38-39), but doesn't limit the number of mentions or the length of individual mention strings. A client could send thousands of mentions or a single mention string of arbitrary length. These are serialized to JSON and stored in SQLite. While SQLite handles large strings, the JSON serialization at `store/notes.rs:74` and deserialization at `store/notes.rs:40` process unbounded data.
- **Impact:** Low. MCP clients are typically AI tools that send reasonable inputs. Malicious input could bloat the database.
- **Suggested fix:** Cap mentions to 20 items and 200 bytes each, matching the practical usage pattern.

#### 8. `Config` accepts extreme values without validation (LOW)
- **Difficulty:** easy
- **Location:** `src/config.rs:26-37`
- **Description:** The `Config` struct has optional `limit`, `threshold`, and `name_boost` fields with no validation on load. A user could set `limit = 999999999` or `threshold = -100.0` in `.cqs.toml`. While `SearchFilter::validate()` checks these at search time for MCP, the CLI path (`cli/commands/query.rs`) reads config values and passes them to search without going through `SearchFilter::validate()`.
- **Impact:** Low. Invalid config values would cause odd search behavior (too many results, no results) but no crash or data corruption.
- **Suggested fix:** Add validation in `Config::load_file()` or document valid ranges in the config file comments.

#### 9. `count_vectors` reads full ID map JSON with no size guard (LOW)
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:724-745`
- **Description:** `HnswIndex::count_vectors()` reads the HNSW ID map file (`std::fs::read_to_string`) and parses it as JSON with no file size check. The `HnswIndex::load()` method at line 634 has a 500MB size guard (`if metadata.len() > MAX_ID_MAP_SIZE`), but `count_vectors()` bypasses this check entirely. A corrupted or maliciously large ID map file would be read fully into memory.
- **Impact:** Low. The ID map is an internal file not exposed to user input. Would only matter if the file was corrupted on disk.
- **Suggested fix:** Add the same `MAX_ID_MAP_SIZE` check that `load()` uses: `let metadata = std::fs::metadata(&id_map_path)?; if metadata.len() > MAX_ID_MAP_SIZE { return Err(...); }`.

**Summary:** 9 new findings. 2 medium severity (#1 `parse_file_calls` missing size limit, #4 FTS5 operator keywords in name search). 7 low severity. All 12 previously-reported edge case issues confirmed fixed. The codebase handles edge cases well overall -- empty inputs, unicode, and boundary conditions are mostly covered. The main gaps are in the call graph extraction path (missing the file size guard that the main parser has) and FTS5 query construction (operator keywords surviving normalization).


---

### Concurrency Safety

**Scope:** Thread safety, data races, deadlocks, lock ordering, atomic operations, channel usage, unsafe Send/Sync implementations, mutex poisoning handling.

**Cross-reference with prior audit (CS1-CS7):**

| ID | Issue | Status | Verification |
|----|-------|--------|-------------|
| CS1 | CagraIndex unsafe Send/Sync | DOCUMENTED | `cagra.rs:354-361` has `// SAFETY:` comment explaining cuvsResources/cuvsIndex are heap-allocated opaque C pointers not bound to a thread. All access through Mutex. Confirmed sound. |
| CS2 | LoadedHnsw lifetime transmute | DOCUMENTED | `hnsw.rs:673-685` has detailed comment: transmute extends lifetime from `'a` to `'static` so `Hnsw` can be stored in a struct. Raw pointer `io_ptr` tied to `ManuallyDrop<MmapIo>`, Drop order is correct (Hnsw dropped before MmapIo). Confirmed sound. |
| CS3 | CagraIndex nested mutex (resources + index) | OK | `cagra.rs:155-332` always acquires `resources` first, then `index`. All 5 error-recovery paths correctly restore the index to the mutex. Lock ordering is consistent. Confirmed no deadlock. |
| CS4 | Audit mode TOCTOU | FIXED | `mcp/tools/search.rs:78-91` acquires audit_mode lock once, reads both `enabled` and `expires_at` in a single critical section, then drops the lock. No window between check and use. Confirmed. |
| CS5 | block_on() in sync iterator | DOCUMENTED | `store/chunks.rs:466-479` has comment explaining `block_on()` is safe because `embedding_batches()` is only called from sync CLI code (`cli/commands/index.rs`), never from within an async runtime. The Store's `Runtime` is dedicated. Confirmed. |
| CS6 | Pipeline channel race | OK | `cli/pipeline.rs:197-508` uses bounded crossbeam channels. Parser thread sends to `embed_tx`, GPU thread receives from `embed_rx` and sends failures to `fail_tx`. CPU thread uses `select!` on both `embed_rx` and `fail_rx`. `drop(fail_tx)` at line 447 signals CPU thread that GPU is done. Channel ownership is correct -- each channel has exactly one sender and one receiver (or one sender with explicit drop). Confirmed no race. |
| CS7 | RwLock writer starvation | ACCEPTED | `mcp/server.rs:49-86` uses `std::sync::RwLock` which does not guarantee writer priority. The background CAGRA thread (`server.rs:89-116`) acquires a write lock once at startup. MCP tool calls acquire read locks on each request. Since the write lock is only acquired once (during CAGRA build), writer starvation is not a practical concern. Confirmed acceptable. |

**All 7 previously-reported issues confirmed fixed/documented/acceptable.**

**New findings:**

#### 1. CAGRA error-recovery lock restoration is correct but fragile
- **Difficulty:** easy
- **Location:** `src/cagra.rs:155-332`
- **Description:** The `search()` method takes the index out of its Mutex (`self.index.lock().unwrap_or_else(|p| p.into_inner()).take()`), performs operations on it, then restores it in all code paths. There are 5 error-recovery paths (lines 196, 207, 234, 266, 290) that each manually restore the index before returning an error. If a future code change adds a new early return without restoring the index, the `CagraIndex` becomes permanently unusable (subsequent calls find `None` in the mutex). The pattern is functionally correct today but relies on developers remembering to restore in every exit path.
- **Impact:** No current bug. Maintenance hazard if `search()` is modified.
- **Suggested fix:** Use a guard pattern (RAII) that automatically restores the index on drop, similar to `MutexGuard`. A simple wrapper: `struct IndexGuard<'a> { mutex: &'a Mutex<Option<Index>>, index: Option<Index> }` with `Drop` that restores.

#### 2. Embedder cache miss race leads to duplicate work (intentional)
- **Difficulty:** N/A (accepted)
- **Location:** `src/embedder.rs:382-419`
- **Description:** `embed_query()` acquires the cache lock, checks for a cache hit, releases the lock, computes the embedding (slow), then re-acquires the lock to insert. Between release and re-acquire, another thread could compute the same embedding. This is intentional: holding the lock during embedding would serialize all embedding requests. The duplicate work is bounded (cache is checked first, duplicates only occur on near-simultaneous identical queries).
- **Impact:** None. Correct design tradeoff. Documenting for completeness.

#### 3. Pipeline shutdown ordering is correct
- **Difficulty:** N/A (verified correct)
- **Location:** `src/cli/pipeline.rs:440-508`
- **Description:** Verified the dual-channel shutdown sequence: (1) GPU thread finishes, sends remaining failures to `fail_tx`, then drops `fail_tx` at line 447. (2) CPU thread's `select!` sees `fail_rx` disconnected and `embed_rx` disconnected (parser dropped `embed_tx` when done), exits the loop. (3) Both threads join. The ordering is correct -- no lost messages, no hang.
- **Impact:** None. Confirming correctness.

#### 4. Notes file I/O has no file-level locking
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:120-129`
- **Description:** `tool_add_note()` reads `docs/notes.toml` via `std::fs::read_to_string()`, appends a new note entry, then writes the entire file back via `std::fs::write()`. There is no file lock. If two MCP clients (or an MCP client and a human editor) modify `notes.toml` simultaneously, one write will silently overwrite the other. The MCP server is single-process with stdio transport, so concurrent MCP calls are serialized by the protocol. But the HTTP transport (`src/mcp/transports/http.rs`) could handle concurrent requests from multiple clients.
- **Impact:** Moderate. With stdio transport (current default), serialization prevents races. With HTTP transport, concurrent note additions could lose data.
- **Suggested fix:** Use `fs2::FileExt::lock_exclusive()` or `flock()` around the read-modify-write cycle. Or use an atomic write pattern (write to temp, rename).

#### 5. Note re-indexing is non-atomic (delete then insert in separate transactions)
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:132-145`, `src/lib.rs:146-152`
- **Description:** After adding a note to the TOML file, `tool_add_note()` calls `index_notes()` which: (1) calls `store.delete_notes_by_file()` (own transaction at `store/notes.rs:169-185`), then (2) calls `store.upsert_notes_batch()` (separate transaction at `store/notes.rs:69-107`). If the process crashes between (1) and (2), all notes from that file are deleted from the index with no recovery. This is the same issue as Data Integrity finding #2 -- noting it here as a concurrency concern because the HTTP transport could trigger this during concurrent requests.
- **Impact:** Moderate. Same root cause as DI finding #2. Cross-referencing, not duplicating.
- **Suggested fix:** See Data Integrity finding #2 (single transaction for delete+insert).

#### 6. Store Drop `block_on` can panic if runtime is shutting down
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:547-560`
- **Description:** `impl Drop for Store` calls `self.runtime.block_on(async { ... })` to flush WAL and close connections. If the runtime is already shutting down (e.g., during a panic unwind that is dropping the Store), `block_on()` can panic with "Cannot start a runtime from within a runtime" or "Cannot block on a runtime that is being dropped". The `close()` method at line 534 has the same pattern but is called explicitly, not during unwind.
- **Impact:** Low. The panic during Drop only occurs if the Store is dropped during an existing panic (double-panic, which aborts). The WAL data is not lost -- SQLite recovers WAL on next open.
- **Suggested fix:** Wrap the `block_on` in `std::panic::catch_unwind()` or check if the runtime handle is still valid. Or use `try_block_on` if available.

#### 7. Detached CAGRA build thread has no join handle
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:73-76`
- **Description:** The background CAGRA build thread is spawned with `std::thread::spawn(move || { ... })` and the `JoinHandle` is dropped immediately (not stored). This means: (a) if the CAGRA build panics, the panic is silently swallowed (no crash propagation), (b) there is no way to wait for CAGRA build completion or check its status, (c) the thread runs detached. The thread does have proper error handling (logs errors, returns early), so panics should not occur. But a GPU driver crash could cause a panic in the CUVS FFI layer.
- **Impact:** Low. Silent failure means HNSW stays active (correct fallback). No user-visible impact unless debugging GPU issues.
- **Suggested fix:** Store the `JoinHandle` and check it in a health/status endpoint, or add `catch_unwind` inside the thread.

#### 8. OnceLock embedder initialization race is standard
- **Difficulty:** N/A (accepted)
- **Location:** `src/mcp/server.rs:140-160`
- **Description:** `ensure_embedder()` uses `OnceLock::get_or_init()` which guarantees exactly-once initialization even under concurrent calls. Multiple threads calling `ensure_embedder()` simultaneously will block on the `OnceLock` until the first caller completes initialization. This is the intended behavior of `OnceLock`. The error handling wraps the `OnceLock` with a manual check-and-init pattern because `get_or_try_init` was unstable -- this is correct.
- **Impact:** None. Standard pattern. Confirming correctness.

**Mutex poisoning handling:**
All mutex locks across the codebase use the recovery pattern `unwrap_or_else(|p| p.into_inner())`:
- `cagra.rs:167,171` (resources, index)
- `embedder.rs:256,390,406` (session, cache)
- `mcp/server.rs:153` (embedder init)
- `mcp/tools/audit.rs:19,41,68` (audit_mode)
- `mcp/tools/search.rs:85` (audit_mode)
- `mcp/tools/read.rs:56` (audit_mode)

This is consistent and correct -- the system recovers from poisoned mutexes rather than propagating panics.

**Unsafe Send/Sync re-verification:**
- `LoadedHnsw` (`hnsw.rs:185-188`): Sound. The struct owns its data (`ManuallyDrop<MmapIo>`, `ManuallyDrop<Hnsw<'static>>`), uses raw pointer only for self-referential storage, Drop order is enforced. No shared mutable state across threads.
- `CagraIndex` (`cagra.rs:354-361`): Sound. All fields are behind Mutex. The CUVS opaque C pointers (`cuvs::Resources`, `cuvs::cagra::Index`) are heap-allocated and not thread-affine. Mutex serializes all access.

**Lock ordering analysis:**
The MCP server has two lockable resources: `index: Arc<RwLock<...>>` and `audit_mode: Mutex<...>`. Examining all lock acquisition sites:
- `search.rs:78-91`: index read -> audit_mode (consistent)
- `read.rs:54-65`: audit_mode only (no index)
- `audit.rs:19-84`: audit_mode only (no index)
- `stats.rs:35-44`: index read only (no audit_mode)
- `call_graph.rs:13-47`: index read only (no audit_mode)
- `notes.rs:43-145`: index read only (no audit_mode, but index not actually used for notes -- only for embedder)

Lock ordering is consistent: when both locks are needed, index is always acquired first. No deadlock possible.

**Summary:** 8 new findings. 0 critical. All 7 previously-reported CS issues confirmed resolved.

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| 1 | CAGRA error-recovery fragile pattern | easy | maintenance hazard |
| 2 | Embedder cache miss race | N/A | accepted (intentional) |
| 3 | Pipeline shutdown ordering | N/A | verified correct |
| 4 | Notes file I/O no file lock | medium | moderate (HTTP transport) |
| 5 | Non-atomic note re-index | medium | moderate (cross-ref DI #2) |
| 6 | Store Drop block_on can panic | easy | low |
| 7 | Detached CAGRA build thread | easy | low |
| 8 | OnceLock embedder race | N/A | accepted (standard) |

Most actionable: #4 (file-level lock for notes.toml) is the most likely to cause real data loss with HTTP transport. #1 (CAGRA guard pattern) is the best effort-to-safety improvement. #6 (Drop block_on) is an easy defensive fix.

---

### Algorithmic Complexity

**Cross-reference:** AC2-AC10 all verified as fixed/acceptable per prior audit. The following are NEW findings.

#### AC11. Pipeline writer N+1 upsert_calls per chunk
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:539-543`
- **Description:** The writer stage loops over every chunk in a batch and calls `store.upsert_calls()` individually. Each `upsert_calls()` opens a new SQLite transaction (BEGIN, DELETE, INSERT, COMMIT). For a batch of 32 chunks, this is 32 separate transactions. While each transaction is fast, the overhead compounds: with ~5000 chunks, that is 5000 transaction round-trips. This is O(n) in transactions where it could be O(n/batch_size). The `upsert_chunks_batch` method already batches chunk inserts into a single transaction, but call graph inserts are not.
- **Suggested fix:** Add a `upsert_calls_batch(&[(chunk_id, &[CallSite])])` method that wraps all DELETE+INSERT pairs in a single transaction, mirroring how `upsert_chunks_batch` works. The writer loop would collect all (chunk_id, calls) pairs, then make one call.

#### AC12. extract_body_keywords tokenizes entire function body
- **Difficulty:** easy
- **Location:** `src/nl.rs:698-699`
- **Description:** `extract_body_keywords()` calls `tokenize_identifier(content)` on the ENTIRE function body string. For a 100-line, ~5KB function, this iterates every character, building a Vec of all tokens, then counts them in a HashMap. The function is called during NL description generation which happens for every chunk during indexing. The complexity is O(content_length) per chunk. While individual invocations are fast, it is called for every chunk being embedded (potentially thousands). The bigger concern is that `tokenize_identifier` treats the entire function body as a single identifier - splitting on camelCase boundaries character-by-character through the entire body including syntax like braces, semicolons, etc. The function was designed for short identifiers (1-50 chars), not multi-KB content.
- **Suggested fix:** Limit input to first N lines (e.g., 20) or first 2KB of content before tokenizing. Most meaningful keywords appear in the first portion of a function. Alternatively, split on whitespace first and then tokenize each word - this avoids treating the entire body as one giant identifier.

#### AC13. Brute-force search loads ALL embeddings from SQLite
- **Difficulty:** hard
- **Location:** `src/search.rs:239-244`
- **Description:** When HNSW index is not available, `search_filtered()` executes `SELECT id, embedding FROM chunks` (with optional WHERE clause for language filter). This loads EVERY embedding row into memory as a `Vec<SqliteRow>`. For 10k chunks at 769 dims * 4 bytes = ~30MB of embedding data, plus SQLite row overhead. The cosine similarity is then computed for each row in a loop. This is the expected O(n) brute-force behavior and is documented as the fallback path. However, the issue is that ALL rows are materialized into a Vec first (`.fetch_all()`), rather than streaming via `.fetch()`. For 50k+ chunks, this means ~150MB+ peak memory for the row Vec alone.
- **Suggested fix:** This is mitigated by the HNSW index path (which is used for any index with >0 vectors). The brute-force path only activates when no HNSW index exists. For new installs or small repos, this is fine. For large repos, the `build_batched` + HNSW path handles it. Mark as accepted/low-priority since the HNSW path is the standard code path.

#### AC14. HNSW save reads entire graph/data files into memory for checksumming
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:554`
- **Description:** During `HnswIndex::save()`, the graph and data files are read entirely into memory with `std::fs::read(&path)` to compute blake3 checksums. For a 50k-vector index, the graph file can be ~50MB and the data file ~150MB. This means save temporarily requires ~200MB additional memory on top of the HNSW structure itself. The `verify_hnsw_checksums()` function in `load()` correctly uses streaming (BufReader + io::copy into hasher), but `save()` does not.
- **Suggested fix:** Replace `std::fs::read(&path)` + `blake3::hash(&data)` with the same streaming pattern used in `verify_hnsw_checksums`: open file with BufReader, stream through `blake3::Hasher` via `io::copy`. This eliminates the ~200MB temporary allocation.

#### AC15. GPU embedder thread duplicates windowing + cache logic
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:344-402`
- **Description:** The GPU embedder thread at lines 344-402 manually duplicates the windowing, cache check, and text generation logic that `prepare_for_embedding()` (line 122) already consolidates. The CPU thread correctly calls `prepare_for_embedding()`. This is not a runtime complexity issue per se, but the duplication means the GPU path does an extra clone: at line 412, `to_embed.into_iter().cloned()` clones each Chunk because `to_embed` is `Vec<&Chunk>` (borrowed from `batch.chunks`), whereas the CPU path's `prepare_for_embedding` owns the chunks outright and avoids the clone. For a batch of 32 chunks with ~5KB content each, this is ~160KB of unnecessary allocation per batch.
- **Suggested fix:** Refactor the GPU thread to also use `prepare_for_embedding()`, which already handles windowing, cache lookup, and text generation. This eliminates both the code duplication and the extra clone.

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| AC11 | Pipeline N+1 upsert_calls transactions | medium | moderate (5000 extra transactions) |
| AC12 | extract_body_keywords tokenizes full body | easy | low-moderate (wasteful on hot path) |
| AC13 | Brute-force search materializes all rows | hard | accepted (HNSW is standard path) |
| AC14 | HNSW save reads files into memory for checksum | easy | moderate (~200MB temp alloc) |
| AC15 | GPU embedder duplicates logic + extra clones | easy | low (code quality + minor alloc) |

Most impactful: AC14 (streaming checksum in HNSW save) is the easiest high-impact fix - eliminates ~200MB temporary allocation with a ~5 line change. AC11 (batched call upserts) is the best throughput improvement for indexing. AC12 (body keyword tokenization) is a minor hot-path optimization.

---

### Resource Footprint

**Cross-reference:** All 7 prior RF findings (RF2, RF3, RF4, RF5, RF6, RF10, RF12, RF13) verified as fixed/acceptable during fresh scan. No regressions observed.

#### RF-NEW-1. Regex compiled on every error in sanitize_error_message
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214-216`
- **Description:** `sanitize_error_message()` compiles two regexes (`re_unix` and `re_windows`) via `regex::Regex::new()` on every call. Regex compilation is relatively expensive (~microseconds each). While errors are not the hot path, during bursts (e.g., malformed batch queries, store connection issues) this adds unnecessary latency and allocation pressure. The `nl.rs` module already demonstrates the correct pattern using `LazyLock` for regex caching.
- **Suggested fix:** Move both regexes to `static LazyLock<Regex>` constants (or `OnceLock`), matching the pattern used in `src/nl.rs:21-23`.

#### RF-NEW-2. Dual tokio runtimes in HTTP serve path
- **Difficulty:** medium
- **Location:** `src/mcp/transports/http.rs:118` and `src/store/mod.rs:101`
- **Description:** When running `cqs serve --transport http`, two separate tokio runtimes are created: one in `Store::open()` (for sqlx async operations) and one in `serve_http()` (for axum HTTP serving). Each runtime spins up its own thread pool (default: number of CPU cores). On an 8-core machine this means ~16 OS threads for two runtimes, doubling stack memory (~16MB with default 1MB stacks) and scheduler overhead. The code has a comment acknowledging this: "Two runtimes is acceptable - one for SQLx ops, one for HTTP serving." For the stdio transport this is fine (only 1 runtime), but HTTP transport pays the overhead.
- **Suggested fix:** For HTTP transport, share a single runtime between axum and sqlx. This would require either passing the runtime into `Store::open()` or restructuring the API to accept an external runtime handle.

#### RF-NEW-3. SQLite 64MB page cache per connection (256MB total for pool of 4)
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:131`
- **Description:** `PRAGMA cache_size = -65536` sets a 64MB page cache per SQLite connection. With `max_connections(4)`, the total page cache can reach 256MB at peak. For a typical cqs index with a few thousand chunks, this is over-provisioned. Additionally, `PRAGMA mmap_size = 268435456` (line 139) sets 256MB of memory-mapped I/O on top of the page cache. Together, a single Store can theoretically address up to 512MB of virtual memory for caching alone. For CLI commands that open the store briefly (stats, query), this reservation is immediately discarded. The defaults are tuned for indexing throughput, not for typical interactive use.
- **Suggested fix:** Consider reducing `cache_size` to `-16384` (16MB per connection, 64MB total) for most operations. The mmap_size setting is virtual memory (not resident), so it's less concerning but worth documenting. For the indexing pipeline, a higher cache could be passed as a parameter.

#### RF-NEW-4. CAGRA holds full embedding dataset copy in RAM
- **Difficulty:** hard / accepted
- **Location:** `src/cagra.rs:63`
- **Description:** `CagraIndex` stores `dataset: Array2<f32>` containing all embeddings in RAM (for rebuilding the GPU index after search, since cuVS `search()` consumes the index). For a 50k-chunk index: 50,000 x 769 x 4 bytes = ~146MB resident in host RAM, in addition to the GPU memory copy. This is architecturally required by cuVS's consuming API, but means CAGRA uses ~2x the memory of HNSW for the same data. For MCP server mode (long-running), this memory is held indefinitely.
- **Suggested fix:** This is a documented trade-off. A potential improvement would be to re-read embeddings from the Store when rebuilding instead of caching, trading disk I/O for RAM. However, given rebuild frequency after every search, the current approach is likely the better trade-off for latency. Mark as informational/accepted.

#### RF-NEW-5. Brute-force search loads all embeddings into memory
- **Difficulty:** medium
- **Location:** `src/search.rs:239-245`
- **Description:** `search_filtered()` executes `SELECT id, embedding FROM chunks` (with optional WHERE) and materializes all rows into memory via `fetch_all()`. For a 50k-chunk index, each embedding is 769 x 4 = 3,076 bytes, so the full result set is ~150MB. This happens when no HNSW index is available (fallback path) or during the first query before the index is loaded. The HNSW-guided path (`search_by_candidate_ids`) correctly fetches only candidates. The brute-force path is expected to be rare in normal operation but can spike memory during `cqs query` if HNSW files are missing.
- **Suggested fix:** For the brute-force path, consider streaming rows with `fetch()` instead of `fetch_all()`, computing cosine similarity incrementally. This would cap memory at O(limit) instead of O(n). However, this requires restructuring the async block and may not be worth the complexity given the path is a fallback. Cross-references AC13 from algorithmic complexity audit.

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| RF-NEW-1 | Regex compiled on every error | easy | low (not hot path, but wasteful) |
| RF-NEW-2 | Dual tokio runtimes in HTTP mode | medium | moderate (16 extra threads, ~16MB stacks) |
| RF-NEW-3 | 64MB page cache per connection | easy | moderate (256MB over-provisioned) |
| RF-NEW-4 | CAGRA holds dataset copy in RAM | hard / accepted | informational (146MB at 50k) |
| RF-NEW-5 | Brute-force search loads all embeddings | medium | moderate (150MB spike, fallback only) |

Most actionable: RF-NEW-1 (regex caching) is trivial and matches existing patterns. RF-NEW-3 (cache_size tuning) is easy and has the largest practical impact for CLI users who run brief commands. RF-NEW-2 (dual runtimes) matters for HTTP server deployments.

---

### Data Security

**Cross-reference:** DS4 (notes file permissions 0o600) and DS5 (lock file permissions 0o600) from prior audit are confirmed fixed:
- `src/mcp/tools/notes.rs:113` sets 0o600 on notes.toml creation
- `src/cli/files.rs:155` sets mode 0o600 on lock file creation
- `src/store/mod.rs:156` sets 0o600 on DB/WAL/SHM files
- `src/hnsw.rs:580` sets 0o600 on HNSW index files

#### DS-1. `.cq/` directory created with default permissions (world-readable)
- **Difficulty:** easy
- **Location:** `src/cli/commands/init.rs:23`, `src/cli/commands/index.rs:28`
- **Description:** `std::fs::create_dir_all(&cq_dir)` creates the `.cq/` directory with the process umask (typically 0o755 on Linux, meaning world-readable). While individual files inside (DB, HNSW, lock) are set to 0o600, the directory itself remains world-listable. An attacker with local access can enumerate filenames and observe index activity patterns (WAL file size changes, lock file timing). On multi-user systems this is an information leak.
- **Suggested fix:** On Unix, after `create_dir_all`, set directory permissions to 0o700:
  ```rust
  #[cfg(unix)] {
      use std::os::unix::fs::PermissionsExt;
      let _ = std::fs::set_permissions(&cq_dir, std::fs::Permissions::from_mode(0o700));
  }
  ```

#### DS-2. `cqs_stats` tool leaks absolute filesystem path to MCP clients
- **Difficulty:** easy
- **Location:** `src/mcp/tools/stats.rs:55`
- **Description:** The stats response includes `"index_path": server.project_root.join(".cq/index.db").to_string_lossy()` which sends the full absolute path (e.g., `/home/user/myproject/.cq/index.db`) to the MCP client. While MCP clients are typically trusted, the server already has `sanitize_error_message()` for error paths. The stats response bypasses that by deliberately including the path. This reveals the user's home directory, username, and filesystem layout.
- **Suggested fix:** Use a relative path instead: `"index_path": ".cq/index.db"`. If the absolute path is needed for debugging, gate it behind verbose mode.

#### DS-3. Model integrity checksums disabled (empty BLAKE3 constants)
- **Difficulty:** medium
- **Location:** `src/embedder.rs:19-20`
- **Description:** `MODEL_BLAKE3` and `TOKENIZER_BLAKE3` are empty strings, causing `ensure_model()` to skip checksum verification entirely. The model is downloaded from HuggingFace Hub over HTTPS (transport-layer integrity only). If the HuggingFace CDN is compromised or a local cache is tampered with, a malicious ONNX model could execute arbitrary code during inference (ONNX models can contain custom operators). This is a supply-chain risk. The checksum infrastructure exists but is unused.
- **Suggested fix:** Compute blake3 checksums for the current model version and populate the constants. The verification code is already written and tested.

#### DS-4. Config file `.cqs.toml` read without permission checks
- **Difficulty:** easy
- **Location:** `src/config.rs:47`, `src/config.rs:64`
- **Description:** Project config is loaded from `.cqs.toml` in the working directory without checking who owns the file. In a shared repository, any contributor can commit a `.cqs.toml` that changes cqs behavior for all users (e.g., setting `quiet = true` to hide indexing output). More importantly, the user-level config at `~/.config/cqs/config.toml` has no permission check either, so another process running as the same user can modify it.
- **Suggested fix:** Low priority. Document that `.cqs.toml` is untrusted project-level config in SECURITY.md. For user config, consider warning if permissions are not 0o600.

#### DS-5. HNSW temp directory not cleaned up on save failure
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:498-607`
- **Description:** During `HnswIndex::save()`, a temp directory `.{basename}.tmp` is created. If the save fails after writing files to temp but before renaming them to final location, the temp directory with its files (which have 0o600 permissions) is left behind. On the next save, line 500-506 cleans up the stale temp dir, so this is self-healing. However, between the crash and next save, the partial index data sits in an unexpected location that other tools won't know about.
- **Suggested fix:** Add cleanup in error paths using a guard/drop pattern, or document this as expected behavior since it self-heals.

#### DS-6. Error sanitization regex misses some path patterns
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:214-216`
- **Description:** `sanitize_error_message()` strips paths matching `/home/`, `/Users/`, `/tmp/`, etc. But it misses: (1) Windows WSL paths like `/mnt/c/Users/...`, (2) custom home directories not under `/home/` (e.g., `/data/users/`), (3) paths with colons in them (the regex stops at `:`). While the project root is stripped first, errors from dependencies (SQLx, hnsw_rs, ort) may contain paths that don't start with the project root.
- **Suggested fix:** Add `/mnt/` to the Unix regex pattern. Consider a broader approach: strip any path segment that looks like an absolute path (`/[a-z]+/...` with 3+ segments).

#### DS-7. Notes `index_error` field may leak internal paths
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:167`
- **Description:** When note indexing fails, the raw error string is included in the MCP response: `result["index_error"] = serde_json::json!(err)`. This error comes from the embedder or store and may contain absolute filesystem paths (model path, database path). Unlike `handle_request()` which calls `sanitize_error_message()`, tool result values bypass sanitization.
- **Suggested fix:** Pass `index_error` through `sanitize_error_message()` before including in the response, or return a generic error message.

#### DS-8. HTTP health endpoint exposes version without authentication
- **Difficulty:** easy (already documented)
- **Location:** `src/mcp/transports/http.rs:317-323`
- **Description:** The `/health` endpoint returns the service version (`env!("CARGO_PKG_VERSION")`) without requiring API key authentication. The code has a comment acknowledging this is intentional for a localhost service. However, if the server is bound to `0.0.0.0` (which the code allows with a warning), the version is exposed to the network. Version disclosure helps attackers identify known vulnerabilities.
- **Suggested fix:** Already documented as accepted risk. Consider requiring auth for `/health` when bound to non-localhost, or removing version from unauthenticated responses.

#### Data Security Summary

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| DS-1 | `.cq/` directory world-readable | easy | low (information leak on multi-user) |
| DS-2 | Stats leaks absolute path | easy | low (information disclosure) |
| DS-3 | Model checksums disabled | medium | medium (supply-chain risk) |
| DS-4 | Config no permission checks | easy | low (social engineering in shared repos) |
| DS-5 | HNSW temp dir not cleaned on failure | easy | low (self-healing) |
| DS-6 | Error sanitization misses WSL/custom paths | easy | low (partial bypass) |
| DS-7 | Notes index_error leaks paths | easy | low (information disclosure) |
| DS-8 | Health endpoint version without auth | easy | accepted (documented) |

Prior findings DS4/DS5 (notes and lock file permissions) are confirmed fixed. The most impactful new finding is DS-3 (model checksums disabled) -- the only finding with a meaningful attack vector beyond information disclosure. DS-1, DS-2, DS-6, and DS-7 are easy wins for defense-in-depth.

---

### I/O Efficiency

**Cross-reference:** IO4 (FTS batching), IO6 (watch Store reopen), IO7 (enumerate_files metadata), IO9 (FTS normalized twice) -- all verified as fixed in v0.5.0. AC14 (HNSW save checksum into memory) and AC15 (GPU embedder duplicated I/O logic) overlap with this category and are not repeated here.

#### IO10. note_stats uses 3 separate queries instead of 1
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:226-245`
- **Description:** `note_stats()` executes 3 separate SQL round-trips to get total count, warning count, and pattern count. Each round-trip incurs connection pool checkout + SQLite query overhead (~0.1ms each). These can be combined into a single query using conditional aggregation -- the same pattern already used in `Store::stats()` at `src/store/chunks.rs:298` which uses CTEs to batch multiple counts.
- **Suggested fix:** Replace with: `SELECT COUNT(*), SUM(CASE WHEN sentiment < ?1 THEN 1 ELSE 0 END), SUM(CASE WHEN sentiment > ?2 THEN 1 ELSE 0 END) FROM notes`. Saves 2 round-trips per invocation.

#### IO11. call_stats and function_call_stats use separate queries
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:101-113` and `src/store/calls.rs:225-245`
- **Description:** `call_stats()` runs 2 queries (total count + distinct callees). `function_call_stats()` runs 3 queries (total + distinct callers + distinct callees). Both can be collapsed into single queries using subquery patterns, matching the CTE approach in `Store::stats()`. `function_call_stats()` is called during `cqs stats` output, so the 3 round-trips add ~0.3ms latency to an interactive command.
- **Suggested fix:** `call_stats`: `SELECT (SELECT COUNT(*) FROM calls), (SELECT COUNT(DISTINCT callee_name) FROM calls)`. Same subquery pattern for `function_call_stats` with 3 subqueries.

#### IO12. count_vectors deserializes entire JSON id_map just to count
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:724-745`
- **Description:** `HnswIndex::count_vectors()` reads the entire `.hnsw.ids` JSON file and fully deserializes it into `Vec<String>` just to return `.len()`. For a 50k-chunk index, this file is ~2MB of JSON string array. All those strings are allocated, parsed, and immediately dropped. This function is called from the stats path (not hot), but it's wasteful.
- **Suggested fix:** Accept current behavior since it's a stats-only path (called once per `cqs stats`). If optimization is desired later, could store a count in a separate metadata file or use a streaming JSON parser. Add a doc comment noting the tradeoff.

#### IO13. cqs_read re-parses notes.toml on every file read
- **Difficulty:** medium
- **Location:** `src/mcp/tools/read.rs:69-117`
- **Description:** Every `cqs_read` MCP tool call re-reads and re-parses the entire `docs/notes.toml` file to find relevant notes for context injection. In a typical MCP session, an AI agent issues 10-50 read calls in rapid succession. Each call does: `open()` + `read_to_string()` + TOML parse + iterate all notes. With 100 notes this is <1ms, but it's pure repeated I/O for a file that changes at most once per session (when `cqs_add_note` is called).
- **Suggested fix:** Cache parsed notes in `McpServer` behind a `Mutex<(SystemTime, Vec<Note>)>`. On `cqs_read`, check mtime with one `stat()` call; if unchanged, reuse cached notes. Invalidate on `cqs_add_note`. This replaces N full file reads with N stat calls + 1 read.

#### IO14. Brute-force search loads all embeddings in single fetch_all
- **Difficulty:** hard
- **Location:** `src/search.rs:237-245`
- **Description:** The brute-force `search_filtered()` path (no HNSW index) executes `SELECT id, embedding FROM chunks` with `fetch_all`, loading ALL chunk embeddings into memory simultaneously. For 50k chunks at 3KB/embedding, this is ~150MB materialized in a single Vec. The bounded score heap (line 271) correctly limits result memory to O(limit), but the input is still fully materialized. This path is the fallback when HNSW index doesn't exist or returns empty results.
- **Suggested fix:** This is architecturally accepted -- HNSW is the standard search path. For defense in depth, could use `fetch()` (streaming cursor) instead of `fetch_all()` and score rows as they arrive, discarding embeddings immediately after scoring. This would reduce peak memory from O(N*3KB) to O(1) for the embedding scan, while keeping O(limit) for results. Low priority since HNSW handles normal operation.

#### IO15. Pipeline watch reindex_files computes mtime per-chunk
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:262-268`
- **Description:** In `reindex_files()`, the mtime is computed for every chunk by calling `abs_path.metadata()` on line 263. When a file produces N chunks (e.g., a file with 10 functions), this is N stat calls for the same file. The pipeline's parser thread (`src/cli/pipeline.rs:262-296`) correctly uses a `file_mtimes` HashMap to cache mtime per-file, but the watch path doesn't.
- **Suggested fix:** Cache mtime per-file in `reindex_files()` using a HashMap like the pipeline does. Minor optimization since OS caches stat results, but cleaner code.

#### IO16. Store::open runs 3 sequential metadata checks on startup
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:169-173`
- **Description:** `Store::open()` calls `check_schema_version()`, `check_model_version()`, and `check_cq_version()` sequentially. Each method queries the metadata table separately: `check_schema_version` does 1 query, `check_model_version` does 2 queries (model_name + dimensions), `check_cq_version` does 1 query. That's 4 separate queries to a 5-row metadata table. The `stats()` method already demonstrates the pattern of fetching all metadata in a single query.
- **Suggested fix:** Fetch all metadata rows once (`SELECT key, value FROM metadata`) and validate schema_version, model_name, dimensions, and cq_version from the in-memory HashMap. Saves 3 queries on every `Store::open()` invocation.

#### I/O Efficiency Summary

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| IO10 | note_stats 3 queries -> 1 | easy | low (0.3ms per call) |
| IO11 | call_stats/function_call_stats multi-query | easy | low (0.3ms per call) |
| IO12 | count_vectors full JSON parse | easy | low (stats-only path) |
| IO13 | cqs_read re-parses notes.toml every call | medium | moderate (MCP hot path, N reads) |
| IO14 | Brute-force loads all embeddings at once | hard | accepted (HNSW standard path) |
| IO15 | Watch reindex_files mtime per-chunk | easy | low (OS caches stat) |
| IO16 | Store::open 3+ sequential metadata queries | easy | low-moderate (startup latency) |

Cross-category overlaps: AC14 covers HNSW save streaming checksum (highest-impact I/O fix). AC15 covers GPU embedder duplicated I/O logic.

Most actionable: IO13 (notes.toml caching in MCP server) is the best user-facing improvement -- eliminates repeated file I/O on the most common MCP operation. IO16 (batched metadata on open) and IO10/IO11 (query consolidation) are trivial mechanical fixes. IO14 is accepted by design.

---

### Input Security

**Cross-reference:** Prior audit identified IS1-IS4. IS4 (duration parsing 24h cap) confirmed fixed in `src/mcp/validation.rs:87-95`. IS1 (FTS5 injection), IS2 (path traversal), IS3 (error path leakage) re-examined from scratch below.

#### IS-1. FTS5 column-scoped query with multi-word input produces unintended semantics
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:424`
- **Description:** `search_by_name` constructs the FTS5 MATCH query via `format!("name:{} OR name:{}*", normalized, normalized)`. When `normalized` contains spaces (e.g., `normalize_for_fts("parseConfig")` yields `"parse config"`), the query becomes `name:parse config OR name:parse config*`. In FTS5, the `name:` column filter applies only to the immediately following token. So this actually searches: `name:parse` AND `config` (all columns) OR `name:parse` AND `config*` (all columns). This is a semantic correctness issue (returns imprecise results from all columns instead of name-only), not a security vulnerability, because `normalize_for_fts` strips all FTS5 operators. FTS5 keywords are case-sensitive (uppercase `OR` is operator, lowercase `or` is token). Since `normalize_for_fts` lowercases all output, operators are neutralized. The semantic issue remains: multi-word names search all columns, not just `name`.
- **Impact:** Low. Causes broader-than-expected search results. Not exploitable for injection.
- **Suggested fix:** Quote the normalized term: `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)` or build per-word queries: `name:word1 name:word2`.

#### IS-2. Stdio transport has no line-length limit
- **Difficulty:** easy
- **Location:** `src/mcp/transports/stdio.rs:28`
- **Description:** The stdio transport reads lines with `stdin.lock().lines()` which uses `BufRead::read_line()` internally. Rust's `read_line()` reads until `\n` with no built-in size limit, allocating as much memory as needed. A malicious or buggy client could send a single line of arbitrary size causing OOM. The HTTP transport has a 1MB body limit (`RequestBodyLimitLayer` at `src/mcp/transports/http.rs:88`), but stdio has no equivalent. Code comments note "trusted client (Claude Code)" but this is a defense-in-depth gap.
- **Impact:** Low-medium. Requires a malicious stdio client. Claude Code is trusted, but any process piping to stdin could exploit this.
- **Suggested fix:** Use `BufReader::with_capacity()` and a custom `read_line` that caps at 1MB (matching HTTP limit), or use `take()` on the reader.

#### IS-3. Error sanitization regex misses some absolute path patterns
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:205-226`
- **Description:** `sanitize_error_message` strips paths matching `/home/...`, `/Users/...`, `/tmp/...`, `/var/...`, `/usr/...`, `/opt/...`, `/etc/...` and Windows `C:\Users\...` patterns. However, it misses: (a) paths under `/root/` (common for root users), (b) paths like `/build/...`, `/workspace/...`, (c) paths under `/mnt/...` (WSL environments -- relevant since this project uses WSL), (d) paths like `/run/...`, `/proc/...`. The project root itself is stripped first, so the main risk is from dependency error messages leaking unfamiliar absolute paths.
- **Impact:** Low. Information leakage to MCP clients. The project root is already stripped. MCP clients are semi-trusted. But in HTTP transport with network exposure, internal paths could aid further attacks.
- **Suggested fix:** Use a more aggressive regex: strip any path starting with `/` followed by a word character, e.g., `r"/[a-zA-Z][^\s:]*"`. Or maintain a broader allowlist of common path prefixes including `/root`, `/mnt`, `/run`, `/proc`, `/build`, `/workspace`.

#### IS-4. No length validation on callers/callees name parameter
- **Difficulty:** easy
- **Location:** `src/mcp/tools/call_graph.rs:10-13, 49-51`
- **Description:** The `cqs_callers` and `cqs_callees` tools accept a `name` parameter passed directly to SQL queries via parameterized bind (`?1`). SQL injection is not possible. However, there is no length validation on the name parameter. An extremely long name (e.g., 1MB) would be sent as a bind parameter. SQLite handles this gracefully (no match, returns empty), but it consumes memory. The `cqs_search` tool validates query length via `validate_query_length()`, but callers/callees do not.
- **Impact:** Very low. SQLite handles large bind values safely. HTTP transport's 1MB body limit provides an outer bound.
- **Suggested fix:** Add `validate_query_length(name)?` or a simpler length check (e.g., 500 chars for function names).

#### IS-5. Manual TOML escaping in add_note is incomplete for control characters
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:76-85`
- **Description:** When adding a note, the text is manually escaped for TOML: backslashes, quotes, newlines, carriage returns, and tabs are replaced with escape sequences, then wrapped in double quotes. This escaping misses other control characters that must be escaped in TOML basic strings: U+0000-U+001F (except handled `\n`/`\r`/`\t`) and U+007F must be escaped as `\uXXXX`. If note text contains a form feed (U+000C) or backspace (U+0008), the resulting TOML is technically invalid per spec, though the `toml` crate parser is lenient. The mentions escaping at lines 62-69 has the same issue. A TOML injection attack (e.g., text containing `"\nsentiment = -1\n[[note]]\ntext = "pwned"`) is neutralized by the `\n` -> `\\n` escaping.
- **Impact:** Low. The TOML is re-parsed by the same lenient `toml` crate. Text length is capped at 2000 bytes. The dangerous characters (quotes, newlines, backslashes) are properly handled.
- **Suggested fix:** Use `toml_edit` or the `toml` crate's serializer to generate TOML entries instead of manual string construction. Alternatively, strip control characters before escaping: `text.chars().filter(|c| !c.is_control() || matches!(*c, '\n' | '\r' | '\t'))`.

#### IS-6. Read tool TOCTOU race between exists check and canonicalize
- **Difficulty:** hard
- **Location:** `src/mcp/tools/read.rs:19-37`
- **Description:** `tool_read` checks `file_path.exists()` at line 19, then calls `file_path.canonicalize()` at line 25. Between these calls, the file could be replaced with a symlink pointing outside the project root. The `canonicalize()` resolves symlinks, and `starts_with` at line 35 correctly rejects out-of-project targets. The real TOCTOU risk is between `canonicalize()` (which resolves to an in-project path) and `read_to_string()` at line 51 (where a symlink could be swapped to point outside). Exploitation requires precise timing and write access to the project directory.
- **Impact:** Very low. Attacker needs write access to the project directory, at which point they can read files directly. Race window is microseconds.
- **Suggested fix:** Open the file first (getting fd), use `fstat` on fd to verify path, then read from fd. Or use `O_NOFOLLOW`. Practically, this is defense-in-depth since the tool serves project files to project collaborators.

#### IS-7. FileSystemSource follows symlinks during enumeration
- **Difficulty:** easy
- **Location:** `src/source/filesystem.rs:53-58`
- **Description:** `FileSystemSource::enumerate_files()` uses `WalkBuilder` with default settings. The `ignore` crate's `WalkBuilder` follows symlinks by default. In contrast, the CLI's `enumerate_files` in `src/cli/files.rs:54` explicitly sets `.follow_links(false)`. This means the MCP indexing path could follow symlinks pointing outside the project directory and index files from outside the project tree. These files would then appear in search results with their relative path (computed via `strip_prefix` at line 128, which falls back to the absolute path if stripping fails).
- **Impact:** Low. This is the indexing path (reads code, doesn't expose arbitrary files). Symlinks within a project to external directories are unusual but possible. The indexed content becomes searchable but not directly readable (the `cqs_read` tool has its own path traversal protection).
- **Suggested fix:** Add `.follow_links(false)` to the `WalkBuilder` in `FileSystemSource::enumerate_files()`, matching the CLI pattern.

#### IS-8. Config values not range-validated at parse time
- **Difficulty:** easy
- **Location:** `src/config.rs:24-37`
- **Description:** The `Config` struct uses `#[serde(default)]` and accepts arbitrary values for `limit`, `threshold`, `name_boost`, etc. There is no range validation at parse time. A malicious `.cqs.toml` could set `limit = 999999999` or `threshold = -100.0`. The `SearchFilter::validate()` method validates some parameters later, and `tool_search` clamps limit to `[1, 20]`. But the config values flow into CLI commands where they may not be clamped. For example, `cmd_query` at `src/cli/commands/query.rs:107` passes `cli.limit` directly to `search_unified_with_index` without clamping. If `limit` came from config and was very large, it could cause excessive memory usage during search.
- **Impact:** Low. Config files are user-controlled (not untrusted input). A user misconfiguring their own tool is not a security threat. However, if `.cqs.toml` is committed to a shared repository, other developers would inherit the values.
- **Suggested fix:** Add range validation in `Config` accessor methods: clamp `limit` to `[1, 100]`, `threshold` to `[0.0, 1.0]`, `name_boost` to `[0.0, 1.0]`.

**Summary:** 8 findings. 0 critical. The codebase has strong input security posture:
- All SQL queries use parameterized binds (no SQL injection)
- FTS5 queries protected by `normalize_for_fts` which strips special chars and lowercases (neutralizing FTS5 operators)
- Path traversal properly guarded by canonicalize + starts_with in read tool
- HTTP transport has body size limits, origin validation, constant-time API key comparison
- Error messages sanitized before returning to clients
- Note text length-limited and escaped against TOML injection

Most actionable: IS-2 (stdio line limit, easy defense-in-depth), IS-7 (follow_links false, one-line fix matching CLI), IS-5 (use TOML serializer instead of manual escaping).

| # | Finding | Difficulty | Impact |
|---|---------|------------|--------|
| IS-1 | FTS5 multi-word name query semantics | easy | low (correctness) |
| IS-2 | Stdio transport no line-length limit | easy | low-medium |
| IS-3 | Error sanitization misses some paths | easy | low |
| IS-4 | No length validation on callers/callees | easy | very low |
| IS-5 | Manual TOML escaping incomplete for control chars | medium | low |
| IS-6 | Read tool TOCTOU race | hard | very low |
| IS-7 | FileSystemSource follows symlinks | easy | low |
| IS-8 | Config values not range-validated | easy | low |
