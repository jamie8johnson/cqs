# Audit Findings

Generated: 2026-02-05

See design: `docs/plans/2026-02-04-20-category-audit-design.md`

---

## Batch 1: Readability Foundation

### Code Hygiene

#### 1. ExitCode enum marked dead but unused
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:49`
- **Description:** The `ExitCode` enum is marked with `#[allow(dead_code)]` and defines exit codes but CLI uses `anyhow::Result` and `process::exit(1)` directly. The enum was planned but never integrated.
- **Suggested fix:** Either integrate ExitCode into error handling or remove the unused enum.

#### 2. run() function incorrectly marked dead code
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:217`
- **Description:** The `run()` function has `#[allow(dead_code)]` but IS called from main.rs. This marker is incorrect.
- **Suggested fix:** Remove the `#[allow(dead_code)]` attribute.

#### 3. InitializeParams fields marked dead but deserialized
- **Difficulty:** easy
- **Location:** `src/mcp.rs:76-87`
- **Description:** Fields `protocol_version`, `capabilities`, `client_info` are deserialized from JSON but never read, suggesting incomplete MCP protocol implementation.
- **Suggested fix:** Either use these fields for protocol validation/logging or document why they're intentionally ignored.

#### 4. _no_ignore parameter never used
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:198`
- **Description:** The `reindex_files` function accepts `_no_ignore: bool` parameter that has no effect on behavior.
- **Suggested fix:** Either implement no_ignore logic or remove the parameter.

#### 5. run_index_pipeline is ~400 lines
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs:450-850`
- **Description:** Handles file discovery, embedding generation, GPU/CPU thread management, progress display, and database operations all in one function. Violates single responsibility.
- **Suggested fix:** Extract into smaller functions: `discover_files()`, `spawn_embedding_workers()`, `process_batches()`, `finalize_index()`.

#### 6. cmd_index is ~200 lines with deep nesting
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:280-480`
- **Description:** Multiple levels of match statements and conditional logic for handling different index modes.
- **Suggested fix:** Use early returns and extract mode-specific logic into helper functions.

#### 7. GPU/CPU embedder thread patterns duplicated
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs` (embedder thread spawning)
- **Description:** GPU and CPU embedding worker thread setup has similar patterns with duplicated error handling and channel setup.
- **Suggested fix:** Create a generic `spawn_embedding_worker<E: Embedder>()` function.

#### 8. Embedding batch processing duplicated between index and watch
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs` and `src/cli/watch.rs`
- **Description:** Both commands have similar logic for collecting files, checking mtime, generating embeddings, upserting to store.
- **Suggested fix:** Extract shared indexing logic into a common module.

#### 9. Note search scoring logic duplicated
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:75-128` and `src/store/notes.rs:235-310`
- **Description:** `search_notes` and `search_notes_by_ids` have identical scoring/ranking logic.
- **Suggested fix:** Extract common `score_notes()` function.

#### 10. Source trait abstraction may be over-engineered
- **Difficulty:** medium
- **Location:** `src/source/mod.rs` and `src/source/filesystem.rs`
- **Description:** `Source` trait exists but `FileSystemSource` is the only implementation.
- **Suggested fix:** If no other sources are planned, consider inlining filesystem logic directly.

#### 11. Redundant .to_string() calls
- **Difficulty:** easy
- **Location:** Multiple files (store/chunks.rs, store/notes.rs)
- **Description:** Pattern `path.to_string_lossy().to_string()` appears frequently.
- **Suggested fix:** Consider a helper function `path_to_string(p: &Path) -> String`.

#### 12. Magic numbers in sentiment thresholds
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:196-203`
- **Description:** Hardcoded `-0.3` and `0.3` sentiment thresholds instead of using constants from `crate::note`.
- **Suggested fix:** Use the `SENTIMENT_*_THRESHOLD` constants.

---

### Module Boundaries

#### 1. CLI Module is a Monolith
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs:1-1960`
- **Description:** ~1960 lines handling command parsing, signal handling, file enumeration, indexing pipeline, progress reporting, configuration, watch mode, serve mode, and all commands.
- **Suggested fix:** Split into submodules: `cli/commands/`, `cli/indexing.rs`, `cli/watch.rs`, `cli/serve.rs`.

#### 2. MCP Module is a Monolith
- **Difficulty:** hard
- **Location:** `src/mcp.rs:1-2000`
- **Description:** ~2000 lines containing JSON-RPC types, request/response handling, tool definitions, audit mode state, validation, and all tool implementations.
- **Suggested fix:** Split into `mcp/types.rs`, `mcp/tools/`, `mcp/validation.rs`, `mcp/server.rs`.

#### 3. lib.rs Contains Application Logic
- **Difficulty:** easy
- **Location:** `src/lib.rs:100-141`
- **Description:** `index_notes` function is application-level orchestration rather than library API.
- **Suggested fix:** Move to dedicated `note_indexing.rs` module or into `note` module.

#### 4. Store Depends on Higher-Level Search Module
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:14`
- **Description:** `store/notes.rs` imports `crate::search::cosine_similarity`. Store is lower-level; search is higher-level. Inverted dependency.
- **Suggested fix:** Move `cosine_similarity` to a shared `util` or `math` module.

#### 5. Store Depends on NL Module
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:14` and `src/store/notes.rs:12`
- **Description:** Store modules import `crate::nl::normalize_for_fts`. NL is higher-level text processing.
- **Suggested fix:** Have callers normalize text before passing to store, or move to low-level `text` utility module.

#### 6. Store Helpers Module Exposes Internal Types
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:8`
- **Description:** `pub mod helpers` exposes internal types to external consumers.
- **Suggested fix:** Change to `pub(crate) mod helpers` and re-export only public types in `store/mod.rs`.

#### 7. Parser Re-exports Internal Language Types
- **Difficulty:** easy
- **Location:** `src/parser.rs:9`
- **Description:** `pub use crate::language::ChunkType` re-exports an internal type.
- **Suggested fix:** Move `ChunkType` to parser.rs as canonical location or document `language::ChunkType` is canonical.

#### 8. Parallel Language Definitions
- **Difficulty:** medium
- **Location:** `src/parser.rs:760-772` and `src/language/mod.rs`
- **Description:** Two parallel systems for representing languages: `Language` enum in parser.rs and `LanguageRegistry`/`LanguageDef` in language module.
- **Suggested fix:** Consolidate into a single `Language` type in the `language` module.

#### 9. CLI Directly Imports Library Internals
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:9-16`
- **Description:** CLI imports directly from library submodules rather than public API.
- **Suggested fix:** Have `lib.rs` provide curated public API surface.

#### 10. Search Module Implements on Store Type
- **Difficulty:** medium
- **Location:** `src/search.rs:1-300`
- **Description:** Search module implements methods directly on `Store` type via `impl Store`. Conflates data access with search algorithms.
- **Suggested fix:** Create dedicated `SearchEngine` struct that holds reference to `Store`.

#### 11. Index Module is Minimal
- **Difficulty:** easy
- **Location:** `src/index.rs:1-30`
- **Description:** Only 30 lines defining `VectorIndex` trait and `IndexResult`. Could be inlined.
- **Suggested fix:** Either expand the module or inline the trait into `hnsw.rs`.

---

### Documentation

#### 1. PRIVACY.md: Embedding dimensions incorrect
- **Difficulty:** easy
- **Location:** `PRIVACY.md:16`
- **Description:** States "768-dimensional floats" but actual is 769 (768 + 1 sentiment).
- **Suggested fix:** Update to "769-dimensional floats (768 from E5-base-v2 + 1 sentiment dimension)"

#### 2. README.md: Upgrade instructions reference outdated version
- **Difficulty:** easy
- **Location:** `README.md:34-36`
- **Description:** States "Run after upgrading from v0.1.11 or earlier" but schema is now v10.
- **Suggested fix:** Update to reference current schema version boundary.

#### 3. SECURITY.md: Protocol version shows wrong value
- **Difficulty:** easy
- **Location:** `SECURITY.md:56`
- **Description:** States "Protocol version: 2024-11-05" but actual is "2025-11-25".
- **Suggested fix:** Update to "2025-11-25"

#### 4. ROADMAP.md: Schema version listed as v9
- **Difficulty:** easy
- **Location:** `ROADMAP.md:227`
- **Description:** States "currently v9" but current schema is v10.
- **Suggested fix:** Update to "currently v10"

#### 5. Embedder docstring shows wrong output dimension
- **Difficulty:** easy
- **Location:** `src/embedder.rs:147`
- **Description:** Doc example says `// 768` but `embed_query` returns 769-dim embeddings.
- **Suggested fix:** Change comment to `// 769`

#### 6. CHANGELOG.md: E5-base-v2 adoption version mismatch
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:398`
- **Description:** v0.1.0 note says changed in v0.2.0 but ROADMAP says v0.1.16.
- **Suggested fix:** Verify which version switched and update consistently.

#### 7. Missing Store public re-export doc comments
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:27-31`
- **Description:** Re-exported public types lack doc comments at module level.
- **Suggested fix:** Add `/// Re-exported from helpers` style comments.

#### 8. ModelInfo default version is stale
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:278-279`
- **Description:** `ModelInfo::default()` sets `version: "1.5"` referencing old nomic model.
- **Suggested fix:** Update to match E5-base-v2 versioning.

#### 9. Chunk.file doc comment says "relative to project root" but path can be absolute
- **Difficulty:** easy
- **Location:** `src/parser.rs:733`
- **Description:** Doc states "relative to project root" but parser stores absolute paths.
- **Suggested fix:** Update to "typically absolute; may be displayed relative to project root"

#### 10. ChunkSummary.file doc comment similarly misleading
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:69`
- **Description:** Same issue - says relative but stores absolute.
- **Suggested fix:** Clarify paths are typically absolute.

#### 11. README.md: HTTP endpoint list incomplete
- **Difficulty:** easy
- **Location:** `README.md:206-209`
- **Description:** Lists endpoints without explaining what each does.
- **Suggested fix:** Add brief descriptions for each endpoint.

#### 12. HNSW tuning parameters not in user docs
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:46-57`
- **Description:** Excellent inline docs exist but not exposed to end users.
- **Suggested fix:** Add "Performance Tuning" section to README.

#### 13. Missing cqs_read tool in README MCP section
- **Difficulty:** easy
- **Location:** `README.md:188-195`
- **Description:** Tool list omits `cqs_read` and `cqs_add_note`.
- **Suggested fix:** Add missing tools with descriptions.

#### 14. Missing cqs_audit_mode tool in README
- **Difficulty:** easy
- **Location:** `README.md:188-195`
- **Description:** `cqs_audit_mode` tool not mentioned.
- **Suggested fix:** Add to tool list.

#### 15. Config file options missing note_weight
- **Difficulty:** easy
- **Location:** `README.md:91-106` and `src/config.rs:11-37`
- **Description:** Missing `note_weight` option in example and Config struct.
- **Suggested fix:** Add `note_weight` to both.

#### 16. README GPU timing estimates may be outdated
- **Difficulty:** medium
- **Location:** `README.md:175-176` and `ROADMAP.md:284-288`
- **Description:** Different timing numbers in different docs.
- **Suggested fix:** Run benchmarks and update all timing references consistently.

#### 17. nl.rs tokenize_identifier XMLParser example demonstrates poor behavior
- **Difficulty:** easy
- **Location:** `src/nl.rs:69`
- **Description:** Example shows consecutive uppercase breaking into single letters.
- **Suggested fix:** Replace with better example or add clarifying note.

---

### API Design

#### 1. Inconsistent return types: usize vs u64 for counts
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:264`, `src/store/notes.rs:178`
- **Description:** `chunk_count()` returns `usize`, `note_count()` returns `u64`. Inconsistent.
- **Suggested fix:** Standardize on one type.

#### 2. needs_reindex vs notes_need_reindex return type mismatch
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:94`, `src/store/notes.rs:155`
- **Description:** `needs_reindex` returns `Option<i64>`, `notes_need_reindex` returns `bool`.
- **Suggested fix:** Make both return same type (mtime approach is more useful).

#### 3. &Path vs PathBuf inconsistency in function signatures
- **Difficulty:** medium
- **Location:** Multiple files
- **Description:** Mixed use of `&Path`, `PathBuf`, and `impl AsRef<Path>`.
- **Suggested fix:** Standardize on `impl AsRef<Path>` for public functions.

#### 4. Two Language enum definitions
- **Difficulty:** medium
- **Location:** `src/parser.rs:760`, `src/language/mod.rs`
- **Description:** `Language` enum in parser.rs and `LanguageDef`/`LanguageRegistry` in language module duplicate functionality.
- **Suggested fix:** Consolidate into single system.

#### 5. Error type inconsistency across modules
- **Difficulty:** medium
- **Location:** Multiple modules
- **Description:** Library exposes both specific error types (thiserror) AND uses `anyhow` in some public APIs like `index_notes()`.
- **Suggested fix:** Public library APIs should use specific error types.

#### 6. SearchFilter missing builder pattern
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:189-218`
- **Description:** Multiple fields require verbose struct syntax to construct.
- **Suggested fix:** Add builder methods like `SearchFilter::new().with_language(...)`.

#### 7. ChunkType::from_str returns anyhow::Error
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:97-114`
- **Description:** Returns `anyhow::Error` while other `FromStr` impls use module-specific errors.
- **Suggested fix:** Create dedicated error type for parse failures.

#### 8. Inconsistent naming: search_by_name vs search_fts vs search_filtered
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:271-361`
- **Description:** Search methods have inconsistent naming and return types.
- **Suggested fix:** Establish naming convention and document differences.

#### 9. VectorIndex trait method shadows inherent method
- **Difficulty:** easy
- **Location:** `src/index.rs:30`, `src/hnsw.rs:360`
- **Description:** Both trait and inherent `search` methods exist. Confusing.
- **Suggested fix:** Remove inherent methods or rename them.

#### 10. serve_http parameter ordering awkward
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1261`
- **Description:** `bind` and `port` as separate parameters instead of combined address.
- **Suggested fix:** Accept `impl ToSocketAddrs` or config struct.

#### 11. embedding_batches returns non-fused iterator
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:405-415`
- **Description:** `EmbeddingBatchIterator` doesn't impl `FusedIterator`.
- **Suggested fix:** Add `impl FusedIterator for EmbeddingBatchIterator`.

#### 12. Exposed internal types in public re-exports
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:27-31`
- **Description:** Internal types like `bytes_to_embedding`, `ChunkRow` exposed via helpers module.
- **Suggested fix:** Make `helpers` module `pub(crate)`.

#### 13. HnswIndex::build vs build_batched API asymmetry
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:195`, `src/hnsw.rs:268`
- **Description:** Batched version requires iterator to yield Results, coupling error handling.
- **Suggested fix:** Consider `build_batched_infallible` for simpler cases.

#### 14. Config fields all Option<T> but no way to get defaults
- **Difficulty:** easy
- **Location:** `src/config.rs:24-37`
- **Description:** All `Option` fields but no methods to get resolved values with defaults.
- **Suggested fix:** Add methods like `Config::limit_or_default(&self) -> usize`.

#### 15. cosine_similarity panics on wrong dimensions
- **Difficulty:** easy
- **Location:** `src/search.rs:23-25`
- **Description:** Contains `assert_eq!` that panics. Poor API for library function.
- **Suggested fix:** Return `Option<f32>` or `Result<f32, DimensionError>`.

#### 16. Embedding::new() accepts any vector length
- **Difficulty:** easy
- **Location:** `src/embedder.rs:62-65`
- **Description:** Accepts any vector without validation. Newtype should validate invariants.
- **Suggested fix:** Add `try_new()` that validates or make `new()` validate.

---

### Error Propagation

#### 1. Glob pattern parsing silently fails
- **Difficulty:** easy
- **Location:** `src/search.rs:184`
- **Description:** Invalid glob patterns silently converted to `None` via `.ok()`. Users don't know filter isn't applied.
- **Suggested fix:** Return error or log warning when glob compilation fails.

#### 2. Second glob pattern silent failure
- **Difficulty:** easy
- **Location:** `src/search.rs:386`
- **Description:** Same issue in `search_by_candidate_ids`.
- **Suggested fix:** Unify glob validation logic and surface errors.

#### 3. Directory iteration errors silently filtered
- **Difficulty:** easy
- **Location:** `src/embedder.rs:514`
- **Description:** `.filter_map(|e| e.ok())` loses information about why files couldn't be read.
- **Suggested fix:** Log at debug level when entries fail.

#### 4. File mtime retrieval swallows errors
- **Difficulty:** easy
- **Location:** `src/lib.rs:126-129`
- **Description:** Chained `.ok()` calls lose specific failure reason.
- **Suggested fix:** Log at trace level when mtime retrieval fails.

#### 5. Language/chunk_type parsing errors silently discarded
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:296, 306`
- **Description:** Invalid strings filtered with `.parse().ok()`. Silently drops corrupted entries from stats.
- **Suggested fix:** Log warning when parsing fails.

#### 6. Schema version parsing silently defaults to 0
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:183`
- **Description:** Corrupted version string silently becomes version 0.
- **Suggested fix:** Log warning when schema_version doesn't parse.

#### 7. Multiple bare ? in HNSW load path
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:107, 117, 433, 442, 448, 475`
- **Description:** File operations use bare `?` without `.context()`. Errors don't indicate which file.
- **Suggested fix:** Add `.context(format!("reading {}", path.display()))`.

#### 8. Tensor creation errors lack context
- **Difficulty:** easy
- **Location:** `src/embedder.rs:403-405`
- **Description:** `Tensor::from_array()` calls use bare `?`. No indication which tensor failed.
- **Suggested fix:** Add `.context("creating input_ids tensor")` etc.

#### 9. Database operations missing context
- **Difficulty:** hard
- **Location:** `src/store/*.rs` (multiple)
- **Description:** Many SQLx operations use bare `?` without indicating which table/operation failed.
- **Suggested fix:** Add `.context()` to key database operations.

#### 10. CAGRA index rebuild errors become empty results
- **Difficulty:** medium
- **Location:** `src/cagra.rs:188-195`
- **Description:** Search returns empty on error. Callers can't distinguish "no matches" from "index error".
- **Suggested fix:** Return `Result<Vec<IndexResult>>` instead.

#### 11. HNSW search dimension mismatch returns empty
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:364-372`
- **Description:** Dimension mismatch logged but returns empty. Callers don't know if error or no results.
- **Suggested fix:** Consider returning `Result`.

#### 12. MCP notes parse/index failures logged but success assumed
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1053-1066`
- **Description:** In `tool_add_note`, indexing failures logged but response doesn't indicate it.
- **Suggested fix:** Include `index_error` field in response.

#### 13. lib.rs index_notes returns anyhow::Result
- **Difficulty:** medium
- **Location:** `src/lib.rs:105`
- **Description:** Library function returns `anyhow::Result` instead of typed error.
- **Suggested fix:** Create top-level library error type.

#### 14. File enumeration quietly skips canonicalization failures
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:374-379`
- **Description:** Failures logged at debug but file silently skipped.
- **Suggested fix:** Consider warning level for first few failures.

#### 15. Walker entry errors silently filtered
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:356`
- **Description:** `walker.filter_map(|e| e.ok())` drops directory walk errors.
- **Suggested fix:** Log at debug level when walk entries fail.

#### 16. Embedding byte length mismatch inconsistent logging
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:339-344` vs `src/store/helpers.rs:356`
- **Description:** `embedding_slice()` logs at trace, `bytes_to_embedding()` logs at warn. Inconsistent.
- **Suggested fix:** Unify logging levels.

#### 17. Poisoned mutex recovery logs at debug, not warn
- **Difficulty:** easy
- **Location:** `src/embedder.rs:314-316, 335-337`
- **Description:** Mutex poisoning indicates a panic occurred. Should be more notable.
- **Suggested fix:** Log at warn level on first occurrence.

#### 18. Index guard poisoning recovery not logged
- **Difficulty:** easy
- **Location:** `src/mcp.rs:646, 652, 716, 756, 878, 1096`
- **Description:** Multiple places recover from poisoned locks with no logging.
- **Suggested fix:** Add debug-level logging.

#### 19. Generic "Failed to open index" missing path
- **Difficulty:** easy
- **Location:** `src/mcp.rs:234`
- **Description:** Error context doesn't include the path attempted.
- **Suggested fix:** Include `index_path` in context message.

#### 20. Store schema mismatch error missing path
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:32-35`
- **Description:** Error tells user to run `--force` but doesn't say which index.
- **Suggested fix:** Add index path to error message.

---

## Batch 2: Understandable Behavior

### Observability

#### 1. No Request Correlation IDs in MCP Server
- **Difficulty:** medium
- **Location:** `src/mcp.rs`
- **Description:** MCP server has no request ID correlation. Tool calls can't be traced back to specific requests in concurrent scenarios.
- **Suggested fix:** Generate request ID per JSON-RPC message, include in tracing span, propagate through tool execution.

#### 2. Watch Mode Lacks Tracing Spans
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:90-150`
- **Description:** `reindex_file` has no tracing span. Events printed to stderr via `eprintln!` rather than structured logging.
- **Suggested fix:** Wrap in `info_span!("reindex_file")` and replace `eprintln!` with `tracing::info!`.

#### 3. Parser Has No Timing Spans
- **Difficulty:** easy
- **Location:** `src/parser.rs`
- **Description:** Parsing operations have no tracing spans. Can't identify parsing bottlenecks.
- **Suggested fix:** Add `info_span!("parse_file", path = %path, language = %lang)`.

#### 4. Database Pool Creation is Silent
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:50-80`
- **Description:** Connection pool creation has no logging. Hard to verify database connectivity.
- **Suggested fix:** Add `tracing::info!("database connected", path = %db_path)`.

#### 5. GPU Failures Use eprintln Instead of Tracing
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:580-590`
- **Description:** CAGRA GPU build failure uses `eprintln!` rather than `tracing::warn!`.
- **Suggested fix:** Replace with `tracing::warn!(error = %e, "GPU index disabled")`.

#### 6. Index Fallback Logged at Debug Level
- **Difficulty:** easy
- **Location:** `src/search.rs:180-200`
- **Description:** Search fallback from HNSW to brute-force (significant perf degradation) only logged at debug.
- **Suggested fix:** Log at info level with reason.

#### 7. Silent Embedding Dimension Mismatch
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:45-60`
- **Description:** `embedding_slice` returns `None` on dimension mismatch, logs at trace. Could mask data integrity issues.
- **Suggested fix:** Log at warn level with actual dimension found.

#### 8. No Metrics for Search Performance
- **Difficulty:** hard
- **Location:** `src/search.rs`
- **Description:** No metrics emission. Can't track query latency, cache hit rates, or result counts.
- **Suggested fix:** Add metrics using `metrics` crate.

#### 9. No Metrics for Embedding Generation
- **Difficulty:** hard
- **Location:** `src/embedder.rs`
- **Description:** Embedding generation has no metrics. Can't track tokens/second or inference latency.
- **Suggested fix:** Add metrics for embed_duration, tokens_processed, batch_size.

#### 10. HNSW Build Progress Not Logged
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:100-200`
- **Description:** Building HNSW index can take significant time but only logs start/completion.
- **Suggested fix:** Add periodic progress logging.

#### 11. Call Graph Operations at Trace Level Only
- **Difficulty:** easy
- **Location:** `src/store/calls.rs`
- **Description:** Call graph upserts log at trace only. No way to see activity at normal log levels.
- **Suggested fix:** Add info-level logging for batch operations.

#### 12. Config Loading Errors Not Structured
- **Difficulty:** easy
- **Location:** `src/config.rs:80-120`
- **Description:** Config parse errors logged at debug. Invalid config should be visible without debug logging.
- **Suggested fix:** Log at warn level with file path and error.

#### 13. index_notes Function Has No Logging
- **Difficulty:** easy
- **Location:** `src/lib.rs:15-60`
- **Description:** `index_notes` parses and indexes notes but has no logging.
- **Suggested fix:** Add `tracing::info!("indexing notes", path = %path)`.

#### 14. No Span for Database Transactions
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs`, `src/store/notes.rs`
- **Description:** Batch upserts use transactions but don't wrap in tracing spans. Can't measure transaction duration.
- **Suggested fix:** Add `info_span!("db_transaction")` around transaction blocks.

#### 15. CAGRA Stream Build Has No Progress
- **Difficulty:** medium
- **Location:** `src/cagra.rs:150-250`
- **Description:** CAGRA streaming build only logs completion. No visibility into progress.
- **Suggested fix:** Log after each batch processed.

#### 16. Schema Migration Silent on Success
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:100-150`
- **Description:** Schema migrations don't log success. Can't verify active schema version.
- **Suggested fix:** Log schema version after migrations.

#### 17. Prune Operation Progress Not Visible
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:140-195`
- **Description:** `prune_missing` returns count but no logging during operation.
- **Suggested fix:** Log at info level for total deleted.

---

### Test Coverage

#### 1. index_notes() function has no tests
- **Difficulty:** medium
- **Location:** `src/lib.rs:37`
- **Description:** Critical path for notes feature has zero test coverage.
- **Suggested fix:** Add integration test with temp notes.toml.

#### 2. serve_stdio() and serve_http() have no tests
- **Difficulty:** hard
- **Location:** `src/lib.rs:70,91`
- **Description:** MCP server entry points have no integration tests.
- **Suggested fix:** Add integration tests that spawn servers and send JSON-RPC requests.

#### 3. Store call graph methods have no tests
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:1-119`
- **Description:** `upsert_calls()`, `get_callers()`, `get_callees()` all untested.
- **Suggested fix:** Add unit tests for each method.

#### 4. search_notes_by_ids() has no tests
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:235`
- **Description:** HNSW-accelerated note search is untested.
- **Suggested fix:** Add test that inserts notes, queries by candidate IDs.

#### 5. note_embeddings() has no tests
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:212`
- **Description:** Method for HNSW building is untested.
- **Suggested fix:** Add test verifying `note:` prefix on IDs.

#### 6. note_stats() has no tests
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:188`
- **Description:** Stats method returning counts has no coverage.
- **Suggested fix:** Add test with notes of varying sentiment.

#### 7. embedding_batches() iterator has no direct test
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:405`
- **Description:** Streaming embeddings iterator only indirectly tested.
- **Suggested fix:** Add explicit test for batch sizes, termination.

#### 8. prune_missing() edge cases untested
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:143`
- **Description:** No tests for empty set, all missing, batch boundaries.
- **Suggested fix:** Add edge case tests.

#### 9. CLI commands have no integration tests
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs`
- **Description:** No end-to-end tests for `cqs index`, `cqs search`, etc.
- **Suggested fix:** Add integration tests using `assert_cmd` crate.

#### 10. search_filtered() has no unit tests
- **Difficulty:** medium
- **Location:** `src/search.rs:89`
- **Description:** Language and path filtering only tested indirectly.
- **Suggested fix:** Add unit tests with mock data.

#### 11. search_by_candidate_ids() has no unit tests
- **Difficulty:** medium
- **Location:** `src/search.rs:144`
- **Description:** HNSW-accelerated search path is untested.
- **Suggested fix:** Add unit tests for scoring, threshold, limit.

#### 12. search_unified_with_index() has no unit tests
- **Difficulty:** hard
- **Location:** `src/search.rs:186`
- **Description:** Main unified search function is untested.
- **Suggested fix:** Add comprehensive tests for chunks, notes, note_weight.

#### 13. Embedder methods require model download
- **Difficulty:** hard
- **Location:** `src/embedder.rs:198-250`
- **Description:** Tests marked `#[ignore]` because they need model. No CI coverage.
- **Suggested fix:** CI job that downloads model, or mock embedder.

#### 14. HNSW search error paths untested
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:103`
- **Description:** Empty index, invalid dimension, k=0 not tested.
- **Suggested fix:** Add error path tests.

#### 15. Tests use weak assertions
- **Difficulty:** medium
- **Location:** `tests/store_test.rs`
- **Description:** Several tests only assert `result.is_ok()` without verifying values.
- **Suggested fix:** Strengthen assertions to verify actual values.

#### 16. Unicode handling untested in FTS
- **Difficulty:** medium
- **Location:** `src/nl.rs`, `src/store/mod.rs`
- **Description:** CJK, emoji, RTL text not tested in FTS.
- **Suggested fix:** Add unicode tests.

#### 17. Empty input edge cases missing
- **Difficulty:** easy
- **Location:** Multiple
- **Description:** Many functions lack empty input tests.
- **Suggested fix:** Add tests for empty query, empty file, empty notes.

#### 18. Large data handling untested
- **Difficulty:** hard
- **Location:** `src/hnsw.rs`, `src/store/`
- **Description:** No tests for 100k+ chunks, performance untested.
- **Suggested fix:** Add benchmark tests with criterion.

#### 19. LoadedHnsw concurrent access untested
- **Difficulty:** hard
- **Location:** `src/hnsw.rs:210`
- **Description:** Uses `unsafe` with Send/Sync claims. No concurrent tests.
- **Suggested fix:** Add multi-threaded stress tests.

#### 20. Parser call extraction coverage gaps
- **Difficulty:** medium
- **Location:** `src/parser.rs`
- **Description:** Missing tests for method chaining, async/await, macros.
- **Suggested fix:** Add parser tests for complex call patterns.

---

### Panic Paths

#### 1. Assert macros in cosine_similarity
- **Difficulty:** medium
- **Location:** `src/search.rs:24-25`
- **Description:** `assert_eq!` panics if embedding dimensions don't match. Corrupted data causes crash.
- **Suggested fix:** Return `Result<f32, Error>` instead of panicking.

#### 2. CAGRA array indexing without bounds check
- **Difficulty:** medium
- **Location:** `src/cagra.rs:314,318,321`
- **Description:** `neighbor_row[i]` could exceed bounds if cuVS returns fewer results than k.
- **Suggested fix:** Use `.get(i)` or check bounds before accessing.

#### 3. Unwrap on enabled field in MCP
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1120`
- **Description:** `args.enabled.unwrap()` after None check. Fragile if control flow changes.
- **Suggested fix:** Use `if let Some(enabled) = args.enabled` pattern.

#### 4. Embedder initialization expect
- **Difficulty:** easy
- **Location:** `src/mcp.rs:332`
- **Description:** `self.embedder.get().expect("embedder just initialized")` fragile if code changes.
- **Suggested fix:** Use `unwrap_or_else` with better error context.

#### 5. HNSW id_map index access
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:392`
- **Description:** Array access after bounds check. Safe but fragile pattern.
- **Suggested fix:** Consider using `.get(idx)` for defense-in-depth.

#### 6. Ctrl+C handler expect
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:72`
- **Description:** Panics if signal handler setup fails.
- **Suggested fix:** Consider graceful degradation with warning.

#### 7. Progress bar template expect
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1028`
- **Description:** Panics if template string invalid. Template is hardcoded so should be safe.
- **Suggested fix:** Add unit test to validate template.

---

### Algorithm Correctness

#### 1. RRF formula documentation unclear
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:376`
- **Description:** RRF uses `+ 1.0` to convert 0-indexed to 1-indexed ranks. Comment doesn't explain.
- **Suggested fix:** Document why `+ 1.0` is needed.

#### 2. Line offset subtraction can produce line 0
- **Difficulty:** medium
- **Location:** `src/parser.rs:547-548`
- **Description:** `saturating_sub` can return 0, which is invalid for 1-indexed line numbers.
- **Suggested fix:** Clamp result to minimum 1: `.max(1)`.

#### 3. Unified search note slot calculation asymmetry
- **Difficulty:** medium
- **Location:** `src/search.rs:531-534`
- **Description:** Code/note allocation doesn't match 60/40 intent when code results are few.
- **Suggested fix:** Fix `take(limit)` to respect reserved allocation.

#### 4. CAGRA itopk_size uses arbitrary constant
- **Difficulty:** easy
- **Location:** `src/cagra.rs:200`
- **Description:** `(k * 2).max(128)` undocumented. May not provide sufficient candidates for large k.
- **Suggested fix:** Document trade-off or use larger multiplier.

#### 5. Context line boundary check off-by-one
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:30-31`
- **Description:** End index clamping logic is convoluted and fragile.
- **Suggested fix:** Simplify with consistent bounds checking.

#### 6. Window splitting pathological case
- **Difficulty:** easy
- **Location:** `src/embedder.rs:268`
- **Description:** `overlap >= max_tokens` creates exponential window explosion.
- **Suggested fix:** Validate `overlap < max_tokens/2` with error.

#### 7. Name matching excludes equal-length substrings
- **Difficulty:** easy
- **Location:** `src/search.rs:100-102`
- **Description:** Substring matching requires length inequality. Intentional but unclear.
- **Suggested fix:** Add comment explaining the design decision.

#### 8. Cosine similarity panics for wrong dimensions
- **Difficulty:** medium
- **Location:** `src/search.rs:23-25`
- **Description:** `assert_eq!` panics in hot path. Could crash on corrupted data.
- **Suggested fix:** Return Result or sentinel value with warning.

#### 9. Parser chunk size check boundary
- **Difficulty:** easy
- **Location:** `src/parser.rs:300`
- **Description:** Check `> 100` but message says "100 max". Minor inconsistency.
- **Suggested fix:** Align message with code behavior.

#### 10. Go return type extraction fails for complex signatures
- **Difficulty:** medium
- **Location:** `src/nl.rs:296-347`
- **Description:** Inner function types confuse parenthesis depth counting.
- **Suggested fix:** Document limitation or use more robust parsing.

#### 11. Embedding batch iterator offset bug
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:459`
- **Description:** Offset increments by filtered batch.len(), not rows fetched. Could loop on corruption.
- **Suggested fix:** Track actual rows fetched for offset.

#### 12. clamp_line_number allows 0
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:317-319`
- **Description:** Clamps to 0 but line numbers are 1-indexed throughout codebase.
- **Suggested fix:** Clamp to minimum 1.

#### 13. FTS normalization truncates mid-word
- **Difficulty:** easy
- **Location:** `src/nl.rs:130-133`
- **Description:** Truncation at byte limit can split words, producing incomplete tokens.
- **Suggested fix:** Truncate at last space boundary.

---

### Extensibility

#### 1. Hardcoded Embedding Model
- **Difficulty:** hard
- **Location:** `src/embedder.rs:14-16`
- **Description:** Model paths hardcoded. Changing model requires code changes in multiple places.
- **Suggested fix:** Make model configurable via config file.

#### 2. Hardcoded Embedding Dimensions
- **Difficulty:** medium
- **Location:** `src/embedder.rs`, `src/hnsw.rs`, `src/store/helpers.rs`
- **Description:** MODEL_DIM and EMBEDDING_DIM duplicated across modules.
- **Suggested fix:** Single source of truth, export from one module.

#### 3. Hardcoded HNSW Parameters
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:46-66`
- **Description:** Tuning parameters require recompilation to change.
- **Suggested fix:** Add HNSW config section to `.cqs.toml`.

#### 4. Closed Language Enum
- **Difficulty:** medium
- **Location:** `src/parser.rs:759-773`
- **Description:** Adding language requires code changes in 5+ places.
- **Suggested fix:** Document process clearly or consider plugin system.

#### 5. Duplicate Language Enum
- **Difficulty:** easy
- **Location:** `src/parser.rs` and `src/language/mod.rs`
- **Description:** Two separate language handling systems.
- **Suggested fix:** Consolidate to single language registry.

#### 6. Closed ChunkType Enum
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:62-80`
- **Description:** Adding code element type requires code changes.
- **Suggested fix:** Consider `ChunkType::Other(String)` variant.

#### 7. Hardcoded Query Patterns
- **Difficulty:** medium
- **Location:** `src/parser.rs:33-138`
- **Description:** Tree-sitter queries hardcoded. Customizing requires source changes.
- **Suggested fix:** Load from config/files, fall back to built-in.

#### 8. Hardcoded Chunk Size Limits
- **Difficulty:** easy
- **Location:** `src/parser.rs:299-301`
- **Description:** 100 lines / 100KB limits not configurable.
- **Suggested fix:** Add to config file.

#### 9. Hardcoded File Size Limit
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:32`
- **Description:** 1MB limit not configurable.
- **Suggested fix:** Add `max_file_size` to config.

#### 10. Hardcoded Token Window Parameters
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:33-34`
- **Description:** MAX_TOKENS_PER_WINDOW and WINDOW_OVERLAP not configurable.
- **Suggested fix:** Add to config file.

#### 11. Hardcoded SQLite Pragmas
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:69-96`
- **Description:** Database tuning pragmas not configurable.
- **Suggested fix:** Add `sqlite` section to config.

#### 12. Hardcoded RRF Constant
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:371`
- **Description:** K=60 affects result blending, not configurable.
- **Suggested fix:** Add `rrf_k` to config.

#### 13. Hardcoded Note Limits
- **Difficulty:** easy
- **Location:** `src/note.rs:21`
- **Description:** MAX_NOTES = 10,000 not configurable.
- **Suggested fix:** Add to config.

#### 14. Hardcoded Sentiment Thresholds
- **Difficulty:** easy
- **Location:** `src/note.rs:16-17`
- **Description:** Warning/pattern thresholds not configurable.
- **Suggested fix:** Add to config.

#### 15. Hardcoded Query Cache Size
- **Difficulty:** easy
- **Location:** `src/embedder.rs:181-183`
- **Description:** LRU cache size = 100 not configurable.
- **Suggested fix:** Add to config.

#### 16. Hardcoded Batch Sizes
- **Difficulty:** easy
- **Location:** `src/embedder.rs:176-179`, `src/cli/mod.rs:671-673`
- **Description:** CPU/GPU batch sizes not configurable.
- **Suggested fix:** Add to config.

#### 17. Hardcoded Project Root Markers
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:315-322`
- **Description:** Users cannot add custom project root markers.
- **Suggested fix:** Allow custom markers in config.

#### 18. Config File Path Hardcoded
- **Difficulty:** easy
- **Location:** `src/config.rs:43-47`
- **Description:** Cannot specify alternate config location.
- **Suggested fix:** Support `CQS_CONFIG` environment variable.

---


## Batch 3: Data & Platform Correctness

### Data Integrity

#### 1. Non-atomic HNSW file writes during save
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:409-448`
- **Description:** Multiple files written sequentially. Crash mid-save leaves partial files. Checksum written last, so crash before completion means corrupted index loads.
- **Suggested fix:** Write to temp files first, atomically rename all at end.

#### 2. prune_missing operations not transactional
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:162-194`
- **Description:** Deletes from chunks_fts and chunks separately. Crash between can leave orphaned FTS entries.
- **Suggested fix:** Wrap each batch in a transaction.

#### 3. upsert_calls not transactional
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:17-40`
- **Description:** DELETE followed by INSERT without transaction. Crash between loses all call data.
- **Suggested fix:** Wrap in transaction.

#### 4. upsert_function_calls not transactional
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:114-161`
- **Description:** Same issue - DELETE then INSERT without transaction.
- **Suggested fix:** Wrap in transaction.

#### 5. Schema init not transactional
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:117-167`
- **Description:** Multiple SQL statements without transaction. Partial failure leaves undefined state.
- **Suggested fix:** Wrap all schema creation in single transaction.

#### 6. No embedding size validation on database insert
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:324-329`, `src/store/chunks.rs:28-52`
- **Description:** `embedding_to_bytes()` converts any size. Invalid embeddings could be stored.
- **Suggested fix:** Validate 769 dimensions before storing.

#### 7. Corrupted embeddings silently filtered on load
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:382-392`
- **Description:** Invalid embeddings filtered with only warning log. HNSW index missing entries silently.
- **Suggested fix:** Return count of skipped entries or make it an error.

#### 8. ID map / HNSW count mismatch only checked on load
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:503-515`
- **Description:** Validated on load but not save. Corrupted data could be persisted.
- **Suggested fix:** Add assertion in save() that counts match.

#### 9. No foreign key enforcement on SQLite
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:68-96`
- **Description:** SQLite foreign keys disabled by default. Cascading deletes don't work.
- **Suggested fix:** Add `PRAGMA foreign_keys = ON` to connection setup.

#### 10. notes.toml ID collision with hash truncation
- **Difficulty:** easy
- **Location:** `src/note.rs:119-123`
- **Description:** Note IDs from 8 hex chars (32 bits). With 10k notes, ~1% collision probability.
- **Suggested fix:** Use 16 hex chars or detect collisions.

#### 11. No schema migration support
- **Difficulty:** hard
- **Location:** `src/store/mod.rs:169-193`
- **Description:** Schema mismatch requires full rebuild. No automated migration path.
- **Suggested fix:** Implement incremental migrations for compatible changes.

#### 12. CAGRA build has no checkpoint recovery
- **Difficulty:** medium
- **Location:** `src/cagra.rs:369-431`
- **Description:** Crash during build loses all work. Rebuilt from scratch each time.
- **Suggested fix:** Consider checkpointing for very large indexes.

#### 13. Checksum provides corruption detection, not tamper-proofing
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:97-131`
- **Description:** Attacker with write access can modify checksum to match corrupted data.
- **Suggested fix:** Document limitation. Security requires separate trusted storage.

#### 14. Missing WAL checkpoint on clean shutdown
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:55-114`
- **Description:** No explicit checkpoint on shutdown. WAL may be large on restart.
- **Suggested fix:** Add `PRAGMA wal_checkpoint(TRUNCATE)` on graceful exit.

#### 15. FTS and main table can become out of sync
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:54-71`
- **Description:** FTS delete errors ignored but insert errors abort. Asymmetric error handling.
- **Suggested fix:** Use SQLite triggers or consistent error handling.

---

### Edge Cases

#### 1. Signature extraction slices without checking byte boundaries
- **Difficulty:** medium
- **Location:** `src/nl.rs:241-247`
- **Description:** `find('(')` returns byte position. `start + 1` could land mid-character for non-ASCII.
- **Suggested fix:** Validate char boundary before slicing.

#### 2. Return type extraction similarly vulnerable
- **Difficulty:** medium
- **Location:** `src/nl.rs:283-288, 353-358`
- **Description:** Same pattern - byte index from find() used for slicing.
- **Suggested fix:** Use safer string slicing that accounts for UTF-8.

#### 3. Large file content loaded entirely into memory
- **Difficulty:** medium
- **Location:** `src/parser.rs:255-262`
- **Description:** Files up to 1MB loaded entirely. Memory could spike for large files.
- **Suggested fix:** Document memory requirement or consider streaming.

#### 4. Unbounded recursion in extract_doc_comment
- **Difficulty:** hard
- **Location:** `src/parser.rs:427-449`
- **Description:** Walks siblings backward without limit. Thousands of comments could be slow.
- **Suggested fix:** Add maximum iteration count.

#### 5. ID map JSON parsing could exceed memory
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:475-477`
- **Description:** Malicious `.hnsw.ids` with billions of entries could exhaust memory.
- **Suggested fix:** Limit maximum IDs or validate file size before parsing.

#### 6. Parse duration allows arbitrarily large values
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1347-1408`
- **Description:** Large hour values could overflow i64 in minute calculation.
- **Suggested fix:** Add bounds checking or use checked arithmetic.

#### 7. normalize_for_fts truncates mid-word
- **Difficulty:** easy
- **Location:** `src/nl.rs:130-133`
- **Description:** Truncation at byte limit can split words, affecting FTS quality.
- **Suggested fix:** Truncate at last space boundary.

#### 8. Zero limit produces confusing results
- **Difficulty:** easy
- **Location:** `src/mcp.rs:595`
- **Description:** `limit: 0` returns 0 results with no explanation.
- **Suggested fix:** Validate limit >= 1 or return explanatory error.

#### 9. Empty mentions deserialization error silently dropped
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:36`
- **Description:** Invalid JSON silently becomes empty vec.
- **Suggested fix:** Log warning when deserialization fails.

#### 10. all_embeddings() could cause OOM
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:376-392`
- **Description:** Loads all embeddings into memory. 100k chunks = ~300MB.
- **Suggested fix:** Deprecate in favor of embedding_batches().

#### 11. SearchFilter doesn't check path_pattern for control chars
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:254-260`
- **Description:** Control characters or null bytes could cause issues.
- **Suggested fix:** Reject patterns with control characters.

#### 12. Tokenizer creates many allocations for uppercase strings
- **Difficulty:** medium
- **Location:** `src/nl.rs:71-93`
- **Description:** All-uppercase 10k char string creates 10k String allocations.
- **Suggested fix:** Add input length cap or single-pass approach.

---

### Platform Behavior

#### 1. Unix-only Symlink Creation
- **Difficulty:** medium
- **Location:** `src/embedder.rs:572`
- **Description:** `std::os::unix::fs::symlink` doesn't exist on Windows.
- **Suggested fix:** Add `#[cfg(unix)]` guard or cross-platform approach.

#### 2. Hardcoded Linux Cache Path
- **Difficulty:** easy
- **Location:** `src/embedder.rs:509`
- **Description:** Path hardcoded to `~/.cache/ort.pyke.io/...` Linux pattern.
- **Suggested fix:** Use `dirs::cache_dir()` for cross-platform.

#### 3. $HOME Environment Variable Assumption
- **Difficulty:** easy
- **Location:** `src/embedder.rs:505`
- **Description:** `$HOME` is Unix-specific. Windows uses `USERPROFILE`.
- **Suggested fix:** Use `dirs::home_dir()`.

#### 4. LD_LIBRARY_PATH Unix-specific
- **Difficulty:** medium
- **Location:** `src/embedder.rs:527`
- **Description:** Linux-specific. Windows uses `PATH` for DLLs.
- **Suggested fix:** Add proper `#[cfg(target_os)]` guards.

#### 5. Colon Path Separator in Library Path
- **Difficulty:** easy
- **Location:** `src/embedder.rs:529`
- **Description:** Uses `:` which is Unix-specific. Windows uses `;`.
- **Suggested fix:** Use `std::env::split_paths()`.

#### 6. Path Display in Database URL
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:65`
- **Description:** Windows backslashes in SQLite URL may not work correctly.
- **Suggested fix:** Convert to forward slashes for URL.

#### 7. Chunk ID Path Separators
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:717-723`
- **Description:** `display()` uses backslashes on Windows. IDs won't match cross-platform.
- **Suggested fix:** Normalize to forward slashes consistently.

#### 8. JSON Output Path Slashes
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:176`, `src/mcp.rs:608`
- **Description:** Windows backslashes in JSON may confuse tools.
- **Suggested fix:** Normalize to forward slashes in JSON.

#### 9. WSL File Watching Reliability
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:49`
- **Description:** Watching `/mnt/c/` paths in WSL has latency and reliability issues.
- **Suggested fix:** Document limitation. Consider polling fallback for WSL.

#### 10. Path Canonicalization Edge Cases
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:344`, `src/mcp.rs:862-865`
- **Description:** Windows may return UNC paths (`\\?\C:\...`). Network drives may fail.
- **Suggested fix:** Use `dunce::canonicalize` to remove UNC prefix.

---

### Memory Management

#### 1. All notes loaded into memory for search
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:84-127`
- **Description:** `search_notes()` loads ALL notes. Thousands of notes causes unbounded allocation.
- **Suggested fix:** Use HNSW index to pre-filter candidates.

#### 2. CAGRA requires all embeddings in memory
- **Difficulty:** hard
- **Location:** `src/cagra.rs:369-431`
- **Description:** 100k chunks = ~300MB. Inherent to GPU batch build.
- **Suggested fix:** Document requirement. Add OOM guard that falls back to HNSW.

#### 3. HnswIndex::build() loads all embeddings
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:195-246`
- **Description:** Requires all embeddings in memory. build_batched() exists but may be overlooked.
- **Suggested fix:** Deprecate build() or add runtime warning for large indexes.

#### 4. all_embeddings() loads entire database
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:376-393`
- **Description:** Can cause OOM for large indexes.
- **Suggested fix:** Deprecate in favor of embedding_batches().

#### 5. Unbounded Vec growth in search results
- **Difficulty:** easy
- **Location:** `src/search.rs:194-228`
- **Description:** `scored` Vec grows without bound before truncation.
- **Suggested fix:** Use bounded heap maintaining only top N.

#### 6. FileSystemSource collects all files into memory
- **Difficulty:** easy
- **Location:** `src/source/filesystem.rs:39-76`
- **Description:** All paths collected into Vec. Large codebases use significant memory.
- **Suggested fix:** Return iterator instead of Vec.

#### 7. HNSW checksum reads entire file into memory
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:117`
- **Description:** Large indexes (>100MB) create memory pressure.
- **Suggested fix:** Use streaming hash with blake3::Hasher::update_reader().

#### 8. HNSW save reads files back for checksums
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:439-448`
- **Description:** Files read after write to compute checksums. Duplicates memory.
- **Suggested fix:** Compute checksums during serialization.

#### 9. MCP tool_read() loads entire file
- **Difficulty:** easy
- **Location:** `src/mcp.rs:874`
- **Description:** No file size limit. Large files could cause memory issues.
- **Suggested fix:** Add maximum file size check (e.g., 10MB).

#### 10. embed_documents creates temporary Strings
- **Difficulty:** easy
- **Location:** `src/embedder.rs:294-296`
- **Description:** Prefixed copies of all texts doubles string memory temporarily.
- **Suggested fix:** Low priority - batch sizes typically small.

---

### Concurrency Safety

#### 1. CagraIndex unsafe Send/Sync with mutable dataset
- **Difficulty:** hard
- **Location:** `src/cagra.rs:354-357`
- **Description:** `dataset` is `Array2<f32>` (mutable). Type-level immutability not enforced.
- **Suggested fix:** Use `Arc<[f32]>` or similar immutable container.

#### 2. LoadedHnsw lifetime transmute version-dependent
- **Difficulty:** hard
- **Location:** `src/hnsw.rs:139-163, 489-501`
- **Description:** Safety depends on hnsw_rs internals. Update could break assumptions.
- **Suggested fix:** Pin hnsw_rs version. Add test that fails on API changes.

#### 3. CAGRA nested mutex locks
- **Difficulty:** medium
- **Location:** `src/cagra.rs:169-213`
- **Description:** Holds resources lock for entire search. Serializes all searches.
- **Suggested fix:** Review if cuVS supports parallel searches with separate streams.

#### 4. Audit mode TOCTOU
- **Difficulty:** easy
- **Location:** `src/mcp.rs:649-653, 713-718`
- **Description:** Lock released between is_active() and status_line(). State could change.
- **Suggested fix:** Capture both values in single lock acquisition.

#### 5. Store runtime blocking in iterator
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:418-468`
- **Description:** `block_on()` in iterator. Panics if called from async context.
- **Suggested fix:** Document that Store methods can't be called from async runtime.

#### 6. Pipeline channel work-stealing race
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:934-950`
- **Description:** GPU and CPU both grab from parse_rx. No priority specification.
- **Suggested fix:** Document expected behavior or use select_biased!.

#### 7. McpServer index RwLock writer starvation
- **Difficulty:** medium
- **Location:** `src/mcp.rs:213, 236-251, 283`
- **Description:** Repeated reads can starve CAGRA upgrade write lock.
- **Suggested fix:** Likely acceptable. Consider write-preference lock if issue arises.

---

## Batch 4: Security & Performance

### Input Security

#### 1. FTS5 sanitization is implicit
- **Difficulty:** easy
- **Location:** `src/nl.rs:114-149`
- **Description:** `normalize_for_fts()` removes FTS5 operators as side effect of tokenization (keeping only alphanumeric). Works but not explicitly documented as security control.
- **Suggested fix:** Document that normalize_for_fts serves as FTS5 sanitization, or add explicit escaping.

#### 2. Glob pattern complexity not limited
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:254-259`
- **Description:** path_pattern limited to 500 chars but not complexity. Deeply nested patterns could slow compilation.
- **Suggested fix:** Consider limits on nesting depth or alternatives count.

#### 3. path_pattern not validated before search
- **Difficulty:** easy
- **Location:** `src/search.rs:181-185`
- **Description:** Invalid globs silently ignored via `.ok()`. Could cause unexpected results.
- **Suggested fix:** Call validate() at search start or document silent ignore behavior.

#### 4. Duration parsing has no upper bound
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1347-1409`
- **Description:** "999999999h" could overflow or create impractical audit mode durations.
- **Suggested fix:** Cap duration to reasonable maximum (e.g., 24 hours).

#### 5. TOML escaping is manual
- **Difficulty:** medium
- **Location:** `src/mcp.rs:985-1021`
- **Description:** Manual escaping of \, ", \n, \r, \t. Correct but brittle if TOML spec changes.
- **Suggested fix:** Use TOML library serialization or add fuzz testing.

**Positive findings:** SQL injection prevented (parameterized queries), path traversal protected (canonicalization), HTTP origin validation robust, command injection not possible.

---

### Data Security

#### 1. CORS allows any origin
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1274-1277`
- **Description:** CorsLayer uses `allow_origin(Any)`. Handler validates localhost, but CORS preflight says `*`.
- **Suggested fix:** Configure CORS to allow only localhost origins.

#### 2. Index files created without explicit permissions
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:413, 433, 448`
- **Description:** HNSW files may be world-readable with permissive umask. Exposes code embeddings.
- **Suggested fix:** Use `OpenOptionsExt::mode(0o600)` on Unix.

#### 3. SQLite database created without explicit permissions
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:66`
- **Description:** Database and WAL files inherit default permissions.
- **Suggested fix:** Set umask to 0o077 before creation.

#### 4. Notes file created without explicit permissions
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1028-1037`
- **Description:** notes.toml may contain sensitive security observations.
- **Suggested fix:** Set restrictive permissions (0o600).

#### 5. Lock file may leak PID
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:421-435`
- **Description:** Minor info disclosure. Lock file also lacks restricted permissions.
- **Suggested fix:** Create with 0o600 permissions.

#### 6. API key visible in environment/process list
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:183`
- **Description:** Environment variables visible via /proc, CLI args visible via ps.
- **Suggested fix:** Support reading API key from file.

#### 7. Error messages may expose internal paths
- **Difficulty:** easy
- **Location:** `src/mcp.rs:354-364`
- **Description:** Full error details returned to clients including file paths.
- **Suggested fix:** Sanitize errors. Log full server-side, return generic to clients.

#### 8. Stdio transport has no authentication
- **Difficulty:** hard
- **Location:** `src/mcp.rs:1181-1234`
- **Description:** By design for trusted clients, but document security implications.
- **Suggested fix:** Document that stdio should only be used with trusted clients.

#### 9. Health endpoint exposes version
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1580-1586`
- **Description:** Allows attackers to identify exact version for targeting vulnerabilities.
- **Suggested fix:** Require auth or return only status without version.

#### 10. API key stored in plain memory
- **Difficulty:** hard
- **Location:** `src/mcp.rs:1247`
- **Description:** Memory dumps would expose API key.
- **Suggested fix:** Use `secrecy` crate to zero memory on drop.

**Positive findings:** Constant-time API key comparison, DNS rebinding protection, path traversal protection, request body limits, checksum validation.

---

### Algorithmic Complexity

#### 1. O(n) brute-force note search
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:74-128`
- **Description:** Fetches ALL notes and computes similarity for each. 10k notes = 10k computations per search.
- **Suggested fix:** Remove direct search_notes calls or add warning for large sets without index.

#### 2. NameMatcher O(m*n) substring matching
- **Difficulty:** easy
- **Location:** `src/search.rs:93-105`
- **Description:** Nested iteration checking contains() for each word pair. Runs per-chunk.
- **Suggested fix:** Remove substring fallback if exact match is sufficient.

#### 3. normalize_for_fts intermediate allocations
- **Difficulty:** easy
- **Location:** `src/nl.rs:114-149`
- **Description:** Creates Vec<String> per word. Called for every chunk during indexing.
- **Suggested fix:** Use streaming approach writing directly to output.

#### 4. tokenize_identifier unnecessary clone
- **Difficulty:** easy
- **Location:** `src/nl.rs:71-93`
- **Description:** `current.clone()` on every word boundary wastes allocations.
- **Suggested fix:** Use `std::mem::take(&mut current)` instead.

#### 5. extract_params_nl multiple intermediate allocations
- **Difficulty:** easy
- **Location:** `src/nl.rs:241-277`
- **Description:** Splits, filters, collects to Vec per parameter.
- **Suggested fix:** Use iterator chain collecting once at end.

#### 6. Brute-force search fallback O(n)
- **Difficulty:** hard
- **Location:** `src/search.rs:166-228`
- **Description:** Without HNSW, loads ALL embeddings (~150MB for 50k chunks) per search.
- **Suggested fix:** Add warning when brute-force triggered on large indexes.

#### 7. HashSet rebuilt per search result
- **Difficulty:** easy
- **Location:** `src/search.rs:78-88`
- **Description:** New HashSet from name words for each result. Thousands per search.
- **Suggested fix:** Pre-index names during storage instead of query-time.

#### 8. Call graph extraction re-reads files
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:1172-1198`
- **Description:** Files read twice: once for chunks, once for calls.
- **Suggested fix:** Extract calls during initial parse pass.

#### 9. RRF allocates HashMap per search
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:364-392`
- **Description:** New HashMap for each RRF fusion. Acceptable but could optimize.
- **Suggested fix:** Consider pre-allocated buffer for hot path.

#### 10. prune_missing O(n) HashSet check
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:140-195`
- **Description:** Fetches all origins from SQL, checks each against HashSet.
- **Suggested fix:** Use SQL WHERE NOT IN with batched values.

---

### I/O Efficiency

#### 1. Note search O(n) full table scan
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:75-128`
- **Description:** Fetches ALL notes for every search when HNSW unavailable.
- **Suggested fix:** Require HNSW index or add FTS pre-filtering for notes.

#### 2. HNSW save reads files twice for checksums
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:436-449`
- **Description:** Writes file then reads it back to compute checksum. Doubles I/O.
- **Suggested fix:** Compute checksum during write using hashing writer.

#### 3. Call graph extraction re-reads parsed files
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:1173-1197`
- **Description:** Files read again for calls after already being parsed for chunks.
- **Suggested fix:** Extract calls during initial parse pass.

#### 4. FTS operations not batched
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:54-71`
- **Description:** Individual DELETE then INSERT per chunk for FTS table.
- **Suggested fix:** Batch with WHERE IN and push_values like calls table.

#### 5. HNSW checksum reads entire file into memory
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:117`
- **Description:** Large indexes cause memory spikes during verification.
- **Suggested fix:** Use streaming hash with blake3::Hasher::update_reader().

#### 6. Watch mode re-opens Store on every reindex
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:115-124`
- **Description:** New Store (runtime + pool) for each file change.
- **Suggested fix:** Keep single Store instance in watch loop.

#### 7. enumerate_files reads metadata twice
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:356-375`
- **Description:** Size check and canonicalize may both stat the file.
- **Suggested fix:** Cache metadata from size check.

#### 8. No connection reuse between pipeline stages
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:696-1016`
- **Description:** Each stage opens own Store with own runtime and pool.
- **Suggested fix:** Share single Store via Arc across threads.

#### 9. FTS query normalized twice for RRF
- **Difficulty:** easy
- **Location:** `src/search.rs:232`
- **Description:** Same normalization may happen multiple times.
- **Suggested fix:** Normalize once at search start and reuse.

---

### Resource Footprint

#### 1. Multiple Tokio Runtimes
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:63`, `src/mcp.rs:1311`
- **Description:** Each Store creates runtime. MCP server has at least 2 separate runtimes.
- **Suggested fix:** Share single application-wide runtime.

#### 2. Eager model path resolution
- **Difficulty:** easy
- **Location:** `src/embedder.rs:172-174`
- **Description:** HuggingFace API calls even for commands that don't need embeddings.
- **Suggested fix:** Make ensure_model() lazy.

#### 3. GPU provider detection on every Embedder
- **Difficulty:** easy
- **Location:** `src/embedder.rs:584-599`
- **Description:** CUDA/TensorRT detection runs for each Embedder::new().
- **Suggested fix:** Cache provider in static OnceCell.

#### 4. Duplicate Embedder instances in pipeline
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:807-809, 925-927`
- **Description:** GPU and CPU embedders each have own tokenizer and cache.
- **Suggested fix:** Share tokenizer, or use single Embedder with dynamic provider.

#### 5. Large query cache default (100 entries)
- **Difficulty:** easy
- **Location:** `src/embedder.rs:181-183`
- **Description:** ~300KB cache wasted for batch indexing which doesn't repeat queries.
- **Suggested fix:** Make cache size configurable or disable for batch ops.

#### 6. Parser recreated multiple times
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:695-1109`
- **Description:** Multiple Parser instances compile same queries (~50ms wasted).
- **Suggested fix:** Share single Parser via Arc.

#### 7. Store opened multiple times during indexing
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:697-1126`
- **Description:** 4+ Store instances with separate runtimes and pools.
- **Suggested fix:** Share single Store via Arc.

#### 8. 64MB SQLite page cache per connection
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:86`
- **Description:** With 4 connections × 4 Stores = potentially 1GB+ page cache.
- **Suggested fix:** Reduce for write-heavy indexing, ensure single Store.

#### 9. 256MB mmap per connection
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:94`
- **Description:** Reserves address space even for small indexes.
- **Suggested fix:** Make proportional to index size.

#### 10. HNSW loaded just for stats count
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1474-1479`
- **Description:** Full graph loaded to display vector count.
- **Suggested fix:** Use count_vectors() which only reads ID map.

#### 11. All tree-sitter grammars compiled upfront
- **Difficulty:** hard
- **Location:** `src/parser.rs:214-246`
- **Description:** All 5 languages compiled even if only one used.
- **Suggested fix:** Lazy-compile queries on first use per language.

#### 12. No connection pool idle timeout
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:69-70`
- **Description:** Connections held indefinitely even when idle.
- **Suggested fix:** Add idle_timeout(Duration::from_secs(300)).

#### 13. Watch mode holds resources when idle
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:60`
- **Description:** Embedder stays loaded during long idle periods.
- **Suggested fix:** Unload after inactivity period or document memory use.

---

## Summary

| Batch | Category | Findings |
|-------|----------|----------|
| 1 | Code Hygiene | 12 |
| 1 | Module Boundaries | 11 |
| 1 | Documentation | 17 |
| 1 | API Design | 16 |
| 1 | Error Propagation | 20 |
| 2 | Observability | 17 |
| 2 | Test Coverage | 20 |
| 2 | Panic Paths | 7 |
| 2 | Algorithm Correctness | 13 |
| 2 | Extensibility | 18 |
| 3 | Data Integrity | 15 |
| 3 | Edge Cases | 12 |
| 3 | Platform Behavior | 10 |
| 3 | Memory Management | 10 |
| 3 | Concurrency Safety | 7 |
| 4 | Input Security | 5 |
| 4 | Data Security | 10 |
| 4 | Algorithmic Complexity | 10 |
| 4 | I/O Efficiency | 9 |
| 4 | Resource Footprint | 13 |
| **Total** | | **~242** |
