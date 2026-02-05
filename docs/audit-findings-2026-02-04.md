# Audit Findings

Generated: 2026-02-04

See design: `docs/plans/2026-02-04-20-category-audit-design.md`

---

## Batch 1: Readability Foundation

### Code Hygiene

#### 1. Duplicated Tree-Sitter Query Constants
- **Difficulty:** hard
- **Location:** `src/parser.rs:20-200` and `src/language/*.rs`
- **Description:** The parser.rs file contains RUST_QUERY, PYTHON_QUERY, etc. that are nearly identical to CHUNK_QUERY constants in language/*.rs. The LanguageRegistry infrastructure is sophisticated but largely unused.
- **Suggested fix:** Consolidate query definitions into one place. Either have parser.rs use the LanguageRegistry, or remove the duplicate constants.

#### 2. Repeated Line Number Clamping Pattern
- **Difficulty:** easy
- **Location:** `src/search.rs:125,126`, `src/store/calls.rs:67,68,111,112`, `src/store/chunks.rs:325,326`, `src/store/helpers.rs:182,183`
- **Description:** The pattern `.clamp(0, u32::MAX as i64) as u32` appears 10 times for converting SQLite i64 to u32.
- **Suggested fix:** Extract to a helper function like `fn i64_to_line_number(n: i64) -> u32`.

#### 3. Explicit Dead Code Markers
- **Difficulty:** easy
- **Location:** `src/mcp.rs:76,86`, `src/cli/mod.rs:47`
- **Description:** `#[allow(dead_code)]` markers on InitializeParams, ClientInfo, ExitCode enum.
- **Suggested fix:** Either remove the dead code, or actually use it.

#### 4. Unused LanguageRegistry Infrastructure
- **Difficulty:** hard
- **Location:** `src/language/mod.rs`, `src/language/*.rs`
- **Description:** The language module defines sophisticated LanguageRegistry that's barely used. Parser.rs has its own Language enum and queries, bypassing the registry.
- **Suggested fix:** Fully integrate LanguageRegistry or simplify to only what's used.

#### 5. Inconsistent Error Handling Style
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:147`, `src/store/notes.rs:147`
- **Description:** Some error paths use `std::io::Error::other()` instead of structured StoreError.
- **Suggested fix:** Define appropriate StoreError variants for time-related errors.

#### 6. Feature Flag Query Duplication
- **Difficulty:** medium
- **Location:** `src/parser.rs:315-400`
- **Description:** Large match statement with `#[cfg(feature = "lang-*")]` guards that repeat similar patterns.
- **Suggested fix:** Create a macro or helper function for the common work.

#### 7. Magic Numbers in Configuration
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:66,78,86`, `src/hnsw.rs`
- **Description:** SQLite tuning parameters and HNSW values are hardcoded without explanation.
- **Suggested fix:** Extract to named constants with documentation.

#### 8. Commented Debug Code Pattern
- **Difficulty:** easy
- **Location:** `src/search.rs:78-80`
- **Description:** Debug/trace logging is commented out instead of using proper tracing macros.
- **Suggested fix:** Use `tracing::debug!()` instead of commented code.

---

### Module Boundaries

#### 1. Duplicate Language Enum Definitions
- **Difficulty:** medium
- **Location:** `src/parser.rs:730-738` and `src/language/mod.rs`
- **Description:** Two parallel language systems: parser::Language is used throughout, while language::LanguageDef exists but is barely used.
- **Suggested fix:** Unify around a single language abstraction.

#### 2. Search Logic Split Across Store and Dedicated Module
- **Difficulty:** medium
- **Location:** `src/search.rs` (extends Store) and `src/store/mod.rs`
- **Description:** Search algorithms are directly part of Store via `impl Store`, mixing persistence and search concerns.
- **Suggested fix:** Introduce a SearchEngine struct that takes a &Store reference.

#### 3. CLI Module Imports Library Types Directly
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:22-27`
- **Description:** CLI reaches deep into internal implementations rather than going through public API in lib.rs.
- **Suggested fix:** Update CLI imports to use the public API from lib.rs.

#### 4. helpers.rs Exposes Internal Row Types Publicly
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:33-46`
- **Description:** ChunkRow (raw database row type) is exported publicly but should be internal.
- **Suggested fix:** Make ChunkRow `pub(crate)` instead of pub.

#### 5. nl Module Has Direct Dependency on Parser Types
- **Difficulty:** easy
- **Location:** `src/nl.rs:6`
- **Description:** The nl module imports Chunk, ChunkType, and Language directly from parser, creating tight coupling.
- **Suggested fix:** Consider defining a trait or simpler struct for nl's needs.

#### 6. MCP Module Reimplements Note Indexing Logic
- **Difficulty:** medium
- **Location:** `src/mcp.rs:1074-1115` and `src/cli/mod.rs:1173-1205`
- **Description:** Both MCP and CLI have duplicate note indexing logic.
- **Suggested fix:** Extract shared note indexing logic into a function.

#### 7. Store Module Knows About FTS Normalization
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:294-323`
- **Description:** normalize_for_fts() contains text processing logic that mixes storage with text processing.
- **Suggested fix:** Move normalize_for_fts() to the nl module.

#### 8. Parser Module Contains Query Constants That Belong in Language Modules
- **Difficulty:** medium
- **Location:** `src/parser.rs:25-179`
- **Description:** Tree-sitter query strings defined in parser.rs, but language/ modules exist for this purpose.
- **Suggested fix:** Move query constants to respective language modules or delete language/ submodule.

#### 9. Embedder Module Has ONNX Provider Setup Logic
- **Difficulty:** easy
- **Location:** `src/embedder.rs:492-567`
- **Description:** ensure_ort_provider_libs() handles filesystem operations unrelated to embedding.
- **Suggested fix:** Move provider library setup to a separate module.

#### 10. Source Trait Not Fully Utilized
- **Difficulty:** hard
- **Location:** `src/source/mod.rs:40-60`
- **Description:** Source trait exists but only FileSystemSource is implemented, and CLI doesn't actually use it.
- **Suggested fix:** Either use FileSystemSource through the trait, or remove the trait until needed.

#### 11. CAGRA Module Optional but Creates Conditional Complexity
- **Difficulty:** easy
- **Location:** `src/cagra.rs` and `src/cli/mod.rs:1281-1319`
- **Description:** Scattered `#[cfg(feature = "gpu-search")]` blocks duplicate CAGRA/HNSW selection logic.
- **Suggested fix:** Introduce a factory function that encapsulates the selection logic.

---

### Documentation

#### 1. Stale model name in lib.rs doc comment
- **Difficulty:** easy
- **Location:** `src/lib.rs:8`
- **Description:** Says "nomic-embed-text-v1.5" but actual model is E5-base-v2.
- **Suggested fix:** Change to "E5-base-v2".

#### 2. Stale model name in Embedder doc comment
- **Difficulty:** easy
- **Location:** `src/embedder.rs:135`
- **Description:** Embedder struct doc says "nomic-embed-text-v1.5" but code uses E5-base-v2.
- **Suggested fix:** Update doc comment.

#### 3. Stale model name in CHANGELOG v0.1.0
- **Difficulty:** easy
- **Location:** `CHANGELOG.md:324`
- **Description:** Historical reference to nomic-embed-text-v1.5 without noting later model change.
- **Suggested fix:** Add note about model switch to E5-base-v2.

#### 4. Config file documentation missing from README
- **Difficulty:** medium
- **Location:** `README.md`
- **Description:** README doesn't document config file format, locations, or available options.
- **Suggested fix:** Add "Configuration" section documenting config files.

#### 5. Missing doc comment on JsDocInfo struct
- **Difficulty:** easy
- **Location:** `src/nl.rs:11-12`
- **Description:** Public JsDocInfo struct has no doc comment.
- **Suggested fix:** Add doc comment explaining purpose.

#### 6. Missing doc comment on Language enum variants
- **Difficulty:** easy
- **Location:** `src/parser.rs:731-737`
- **Description:** Language enum variants have no doc comments about supported extensions.
- **Suggested fix:** Add doc comments to each variant.

#### 7. Missing doc comment on ParserError variants
- **Difficulty:** easy
- **Location:** `src/parser.rs:12-20`
- **Description:** ParserError enum variants lack doc comments.
- **Suggested fix:** Add brief doc comments.

#### 8. Missing doc comment on NoteError variants
- **Difficulty:** easy
- **Location:** `src/note.rs:11-16`
- **Description:** NoteError enum variants lack doc comments.
- **Suggested fix:** Add doc comments.

#### 9. Missing doc comment on Config struct
- **Difficulty:** easy
- **Location:** `src/config.rs:15`
- **Description:** Config struct lacks example showing TOML format.
- **Suggested fix:** Add doc example with sample content.

#### 10. Missing doc comment on SearchFilter struct
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:157-158`
- **Description:** SearchFilter struct has no doc comment.
- **Suggested fix:** Add doc comment explaining purpose.

#### 11. Missing doc comment on IndexStats struct fields
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:197-210`
- **Description:** IndexStats struct has no doc comment or field documentation.
- **Suggested fix:** Add struct and field doc comments.

#### 12. Missing doc comment on UnifiedResult enum
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:139`
- **Description:** UnifiedResult enum lacks documentation.
- **Suggested fix:** Add doc comment explaining Code vs Note variants.

#### 13. Incomplete CLI command documentation
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs`
- **Description:** Some CLI commands lack full examples showing expected output.
- **Suggested fix:** Audit CLI commands and ensure each has doc comments.

#### 14. Missing HTTP API documentation
- **Difficulty:** hard
- **Location:** `README.md` and `src/mcp.rs`
- **Description:** README doesn't document HTTP endpoints, request/response format, or authentication.
- **Suggested fix:** Add "HTTP API" section with endpoint documentation.

#### 15. Stale docs/DESIGN_SPEC_27k_tokens.md references
- **Difficulty:** easy
- **Location:** `docs/DESIGN_SPEC_27k_tokens.md`
- **Description:** Design spec extensively references nomic-embed-text-v1.5 (outdated).
- **Suggested fix:** Update or add note that document is historical.

#### 16. Missing doc comment on CURRENT_SCHEMA_VERSION constant
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:11`
- **Description:** Schema version constant has only brief inline comment.
- **Suggested fix:** Add full doc comment explaining schema versioning.

#### 17. Outdated GitHub workflow checking wrong model
- **Difficulty:** medium
- **Location:** `.github/workflows/dependency-review.yml:55-69`
- **Description:** CI checks nomic-ai/nomic-embed-text-v1.5 but codebase uses E5-base-v2.
- **Suggested fix:** Update workflow to check correct model.

---

### API Design

#### 1. Inconsistent Search Function Naming
- **Difficulty:** medium
- **Location:** `src/search.rs:80-499` and `src/store/mod.rs:235-252`
- **Description:** Multiple search methods with inconsistent naming: search(), search_filtered(), search_unified(), etc.
- **Suggested fix:** Consolidate into single search with SearchOptions struct or builder pattern.

#### 2. Redundant Result Types (HnswResult vs IndexResult)
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:78-84` and `src/index.rs:9-15`
- **Description:** HnswResult and IndexResult are identical structs, causing unnecessary duplication.
- **Suggested fix:** Remove HnswResult, have HnswIndex::search return Vec<IndexResult> directly.

#### 3. serve_http/serve_stdio Return Type Mismatch
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1119` and `src/mcp.rs:1190`
- **Description:** Doc comment shows async fn but these are sync. Doc implies sequential use but stdio blocks.
- **Suggested fix:** Fix doc comment to remove async, document blocking behavior.

#### 4. SearchFilter Has Required Field Without Default Enforcement
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:155-177`
- **Description:** query_text must be non-empty for enable_rrf but Default allows empty.
- **Suggested fix:** Add runtime validation or split into two types.

#### 5. Embedding Type Lacks Dimension Validation
- **Difficulty:** medium
- **Location:** `src/embedder.rs:53-110`
- **Description:** Embedding::new accepts any length, but system expects 768/769-dim. Only debug_assert checks.
- **Suggested fix:** Add dimension validation, make new() return Result.

#### 6. Chunk ID Format Is Implementation Detail Exposed as API
- **Difficulty:** medium
- **Location:** `src/parser.rs:700-727`
- **Description:** Chunk.id as String with format parsed manually in multiple places.
- **Suggested fix:** Make id opaque (newtype ChunkId) with accessor methods.

#### 7. MCP Tool Names Don't Follow Convention
- **Difficulty:** easy
- **Location:** `src/mcp.rs:389-527`
- **Description:** MCP tools use cqs_ prefix which may conflict with MCP naming conventions.
- **Suggested fix:** Consider unprefixed names since server is already namespaced.

#### 8. Language::FromStr Error Type Inconsistency
- **Difficulty:** easy
- **Location:** `src/parser.rs:793-808`
- **Description:** Language::from_str returns anyhow::Error while other errors use thiserror.
- **Suggested fix:** Define ParseLanguageError for consistency.

#### 9. Config Merge Behavior Is Surprising
- **Difficulty:** easy
- **Location:** `src/config.rs:63-71`
- **Description:** Config::merge name suggests combining but actually does layered override.
- **Suggested fix:** Rename to override_with or layer_on_top_of.

#### 10. Note Parsing Uses Index-Based IDs
- **Difficulty:** easy
- **Location:** `src/note.rs:95-112`
- **Description:** Notes get IDs like note:0, note:1 based on file index. Reordering breaks references.
- **Suggested fix:** Generate stable IDs based on content hash.

#### 11. Inconsistent Use of &Path vs PathBuf in Function Signatures
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:59`, `src/hnsw.rs:297`, `src/mcp.rs:1119`
- **Description:** Some functions take &Path, others PathBuf, forcing unnecessary allocations.
- **Suggested fix:** Change owned PathBuf parameters to impl AsRef<Path>.

#### 12. VectorIndex Trait Uses Embedding Reference
- **Difficulty:** easy
- **Location:** `src/index.rs:30` and `src/hnsw.rs:248`
- **Description:** VectorIndex::search takes &Embedding but only needs the slice.
- **Suggested fix:** Change trait to fn search(&self, query: &[f32], k: usize).

---

### Error Propagation

#### 1. Silently swallowed errors with `.ok()` in file metadata operations
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:728-732,1196-1199`, `src/source/filesystem.rs:109-110`
- **Description:** Metadata failures silently converted to Option with no logging.
- **Suggested fix:** Log warning before using .ok().

#### 2. Lost error context in get_by_content_hash
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:163-173`
- **Description:** Uses .ok()? to swallow database errors, making them indistinguishable from "not found".
- **Suggested fix:** Return Result<Option<Embedding>, StoreError>.

#### 3. Silent error swallowing in check_cq_version
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:211-232`
- **Description:** check_cq_version discards result with let _ =.
- **Suggested fix:** Add if let Err(e) with tracing::debug!.

#### 4. Parse failures silently default to Language::Rust and ChunkType::Function
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:80-81`
- **Description:** Failed parsing silently defaults, hiding database corruption.
- **Suggested fix:** Log warning when parsing fails.

#### 5. Missing .context() on bare ? propagation in CLI
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:989,995,1099`
- **Description:** Several ? operators propagate without adding context.
- **Suggested fix:** Add .context("Failed to...") for actionable messages.

#### 6. FTS delete errors silently ignored
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:53-56`, `src/store/notes.rs:49-52`
- **Description:** DELETE on FTS tables uses let _ = to ignore failures.
- **Suggested fix:** Log failures with tracing::warn!.

#### 7. embedding_slice returns None without explanation
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:222-228`
- **Description:** Wrong embedding byte length returns None silently.
- **Suggested fix:** Add tracing::warn! when byte length doesn't match.

#### 8. note_stats swallows query failures
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:176-194`
- **Description:** Uses .unwrap_or((0,)) to convert database errors to zero counts.
- **Suggested fix:** Propagate error or log warning.

#### 9. Inconsistent error handling in parse_duration
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1285-1297`
- **Description:** Uses .unwrap_or(0) for parse failures, making "30x" parse as 30.
- **Suggested fix:** Return error for unparseable numeric portions.

#### 10. CAGRA build failure logged but not surfaced
- **Difficulty:** medium
- **Location:** `src/mcp.rs:270-279`
- **Description:** CAGRA failure logged but client has no visibility.
- **Suggested fix:** Add status field to cqs_stats indicating GPU index state.

#### 11. get_embeddings_by_hashes silently returns empty on database error
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:197-204`
- **Description:** Query failures logged but return empty HashMap, causing re-embedding.
- **Suggested fix:** Return Result to let callers decide.

#### 12. GPU embedding failure silently routes to CPU without metrics
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:851-867`
- **Description:** GPU failures rerouted to CPU with eprintln! but no aggregated count.
- **Suggested fix:** Track and report total GPU failures in summary.

#### 13. HNSW checksum verification warns but doesn't fail
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:94-100`
- **Description:** Missing checksum file logs warning and returns Ok, loading untrusted index.
- **Suggested fix:** Consider returning Err when checksum missing for newer indexes.

#### 14. Thread panic converted to generic error message
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1014-1022`
- **Description:** Thread join failures lose panic payload.
- **Suggested fix:** Preserve panic information in error message.

---

## Batch 2: Understandable Behavior

### Observability

#### 1. Tracing subscriber has no log level configuration
- **Difficulty:** easy
- **Location:** `src/main.rs:7-9`
- **Description:** Tracing subscriber uses defaults, no way to configure log levels at runtime. --verbose flag isn't wired to subscriber.
- **Suggested fix:** Use EnvFilter with RUST_LOG support and wire --verbose flag.

#### 2. Store database operations have no tracing spans
- **Difficulty:** medium
- **Location:** `src/store/mod.rs`, `src/store/chunks.rs`, `src/store/notes.rs`, `src/store/calls.rs`
- **Description:** Database operations have no timing info. Can't identify bottlenecks when queries are slow.
- **Suggested fix:** Add info_span! to major database operations.

#### 3. HTTP request handling lacks request-level tracing
- **Difficulty:** medium
- **Location:** `src/mcp.rs:1428-1488`
- **Description:** HTTP requests have no correlation ID, timing, or visibility into processing.
- **Suggested fix:** Add tracing span per request with method name and timing.

#### 4. Embedder batch processing lacks per-batch timing
- **Difficulty:** easy
- **Location:** `src/embedder.rs:353-440`
- **Description:** Individual batch steps (tokenization, inference, pooling) not instrumented.
- **Suggested fix:** Add debug spans around processing steps.

#### 5. MCP tool calls don't log the tool name or execution time
- **Difficulty:** easy
- **Location:** `src/mcp.rs:530-556`
- **Description:** Tool calls routed without logging which tool or how long it took.
- **Suggested fix:** Add info! log with tool name and timing.

#### 6. Note indexing has no logging
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:17-64`
- **Description:** Note batch insertion is silent, failures hard to diagnose.
- **Suggested fix:** Add debug logging for note count and errors.

#### 7. Call graph extraction has no progress or timing info
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:110-149`
- **Description:** Call graph processing provides no visibility.
- **Suggested fix:** Add trace-level logging with file path and call count.

#### 8. Watch mode file change events aren't logged
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1567-1667`
- **Description:** Watch command uses print statements instead of tracing.
- **Suggested fix:** Add info-level tracing events for file changes.

#### 9. Search RRF fusion has no visibility into ranking contributions
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:255-281`
- **Description:** No way to see how semantic and FTS scores contribute to ranking.
- **Suggested fix:** Add debug logging showing semantic/FTS/RRF scores.

#### 10. Parser query compilation errors aren't detailed
- **Difficulty:** easy
- **Location:** `src/parser.rs:222-233`
- **Description:** Tree-sitter query compilation failures have minimal error messages.
- **Suggested fix:** Add debug logging with failing query pattern snippet.

#### 11. HNSW checksum verification has asymmetric logging
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:92-126`
- **Description:** Success only logs at debug, mismatch doesn't log before error.
- **Suggested fix:** Add info log on success, warn log on mismatch.

#### 12. Config file loading results are silent on success
- **Difficulty:** easy
- **Location:** `src/config.rs:30-40`
- **Description:** No visibility into which config files loaded or what values applied.
- **Suggested fix:** Add debug logging for loaded config files and values.

---

### Test Coverage

#### 1. Missing tests for Embedder.embed_documents batch processing
- **Difficulty:** hard
- **Location:** `src/embedder.rs:291`
- **Description:** Core embed_documents function has no integration tests.
- **Suggested fix:** Add integration tests for document embedding with known inputs.

#### 2. Missing tests for Embedder.split_into_windows
- **Difficulty:** medium
- **Location:** `src/embedder.rs:248`
- **Description:** Window splitting function with complex edge cases is untested.
- **Suggested fix:** Add unit tests for single window, multiple windows, edge cases.

#### 3. Missing tests for Embedder.token_count
- **Difficulty:** easy
- **Location:** `src/embedder.rs:237`
- **Description:** token_count function used for windowing decisions is untested.
- **Suggested fix:** Add unit test for known strings.

#### 4. No tests for cosine_similarity edge cases
- **Difficulty:** easy
- **Location:** `src/search.rs:17`
- **Description:** cosine_similarity has no unit tests for edge cases.
- **Suggested fix:** Add property tests for identical, orthogonal, self-similarity.

#### 5. Missing tests for name_match_score boundary cases
- **Difficulty:** easy
- **Location:** `src/search.rs:29`
- **Description:** name_match_score lacks tests for empty strings, unicode, long names.
- **Suggested fix:** Add proptest/fuzz tests for arbitrary inputs.

#### 6. Untested Store.delete_by_origin function
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:110`
- **Description:** delete_by_origin has no direct tests.
- **Suggested fix:** Add test that inserts, deletes, verifies gone.

#### 7. Missing tests for Store.needs_reindex
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:87`
- **Description:** needs_reindex mtime checking is untested.
- **Suggested fix:** Add integration test with file modification.

#### 8. Missing tests for note CRUD operations
- **Difficulty:** medium
- **Location:** `src/store/notes.rs`
- **Description:** Notes module functions have no direct unit tests.
- **Suggested fix:** Add dedicated tests for note operations.

#### 9. Missing tests for call graph operations
- **Difficulty:** medium
- **Location:** `src/store/calls.rs`
- **Description:** Call graph functions have zero test coverage.
- **Suggested fix:** Add tests for callers/callees queries.

#### 10. Missing tests for parse_duration in MCP
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1276`
- **Description:** Duration parsing function has no tests.
- **Suggested fix:** Add unit tests for valid and invalid inputs.

#### 11. Missing tests for AuditMode state management
- **Difficulty:** easy
- **Location:** `src/mcp.rs:151`
- **Description:** AuditMode time-dependent methods are untested.
- **Suggested fix:** Add unit tests for basic logic paths.

#### 12. No integration tests for CLI commands
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs`
- **Description:** CLI commands are completely untested end-to-end.
- **Suggested fix:** Add CLI integration tests using assert_cmd.

#### 13. Missing tests for FileSystemSource with gitignore
- **Difficulty:** medium
- **Location:** `src/source/filesystem.rs:39`
- **Description:** gitignore respecting behavior is not verified.
- **Suggested fix:** Add test with .gitignore file.

#### 14. Missing tests for Source trait error paths
- **Difficulty:** easy
- **Location:** `src/source/filesystem.rs`
- **Description:** Error paths (non-UTF8, permissions) are untested.
- **Suggested fix:** Add tests for error conditions.

#### 15. Missing edge case tests for parser with malformed files
- **Difficulty:** medium
- **Location:** `src/parser.rs`
- **Description:** No tests for empty files, syntax errors, binary files.
- **Suggested fix:** Add tests with malformed inputs.

#### 16. Untested Parser.parse_file_calls function
- **Difficulty:** easy
- **Location:** `src/parser.rs:589`
- **Description:** Full call graph extraction function has no tests.
- **Suggested fix:** Add test with fixture file having large functions.

#### 17. Missing tests for ChunkType.from_str
- **Difficulty:** easy
- **Location:** `src/language/mod.rs`
- **Description:** ChunkType parsing should have explicit tests.
- **Suggested fix:** Add unit tests for all variants and invalid input.

#### 18. Incomplete proptest coverage for FTS normalization
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:430`
- **Description:** Unicode fuzzing doesn't cover emoji, combining characters.
- **Suggested fix:** Expand proptest generators for broader unicode.

---

### Panic Paths

#### 1. Array slice bounds unchecked in display.rs
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:25`
- **Description:** Slicing without verifying indices <= lines.len(). File changes can cause panic.
- **Suggested fix:** Add bounds check with .min(lines.len()).

#### 2. Ctrl+C handler uses .expect()
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:70`
- **Description:** Signal handler registration can fail and would panic.
- **Suggested fix:** Use unwrap_or_else with warning log.

#### 3. Progress bar template .expect()
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:979`
- **Description:** Uses .expect() on template which violates guidelines.
- **Suggested fix:** Handle error or use static-validated template.

#### 4. Static regex .expect() in nl.rs
- **Difficulty:** easy
- **Location:** `src/nl.rs:21-23`
- **Description:** LazyLock regexes use .expect() on compile-time constants.
- **Suggested fix:** Use lazy_regex crate or document the exception.

#### 5. Embedder embed_batch .expect() on single-item result
- **Difficulty:** easy
- **Location:** `src/embedder.rs:320`
- **Description:** Assumes single-item batch always returns one result.
- **Suggested fix:** Use .ok_or_else() and propagate error.

#### 6. NonZeroUsize .expect() on literal values
- **Difficulty:** easy
- **Location:** `src/embedder.rs:182,205`
- **Description:** Uses .expect() on mathematically correct literals.
- **Suggested fix:** Use const initialization or document invariant.

#### 7. OnceCell embedder .expect() after initialization
- **Difficulty:** medium
- **Location:** `src/mcp.rs:322`
- **Description:** Potential race between set() and get() in concurrent scenario.
- **Suggested fix:** Use get_or_init() pattern consistently.

#### 8. SQLx row.get() panics on column mismatch
- **Difficulty:** medium
- **Location:** `src/search.rs:149-151`, `src/store/calls.rs:59-69`
- **Description:** row.get(N) panics if column N doesn't exist.
- **Suggested fix:** Use row.try_get() and handle errors.

---

### Algorithm Correctness

#### 1. Potential Underflow in Call Extraction Line Number
- **Difficulty:** easy
- **Location:** `src/parser.rs:532`
- **Description:** Line calculation can underflow if line_offset > row + 1.
- **Suggested fix:** Use saturating_sub instead of direct subtraction.

#### 2. RRF Property Test Incorrect Max
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:265`
- **Description:** Comment says max=1.0 but actual max is ~0.016 for single list.
- **Suggested fix:** Update comment and property test to reflect actual max.

#### 3. Unified Search Slot Allocation Confusing
- **Difficulty:** medium
- **Location:** `src/search.rs:432-441`
- **Description:** Code/note slot allocation doesn't match apparent intent of 60/40 split.
- **Suggested fix:** Clarify intended behavior and cap code results appropriately.

#### 4. Context Line Edge Case for Line 0
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:20-21`
- **Description:** Doesn't validate line_start >= 1 and line_end >= line_start.
- **Suggested fix:** Add validation at function start.

#### 5. TypeScript Return Type Nested Parens
- **Difficulty:** easy
- **Location:** `src/nl.rs:237`
- **Description:** rfind("): ") matches inside nested function types.
- **Suggested fix:** Use proper parsing or document limitation.

#### 6. Go Return Type Extraction Broken
- **Difficulty:** easy
- **Location:** `src/nl.rs:226`
- **Description:** Uses -> for Go but Go doesn't use arrows for return types.
- **Suggested fix:** Add Go-specific logic for return type extraction.

---

### Extensibility

#### 1. Duplicate Language Enum in parser.rs vs language/mod.rs
- **Difficulty:** medium
- **Location:** `src/parser.rs:731-737` and `src/language/mod.rs:32-49`
- **Description:** Adding a language requires updates in 7+ places due to dual systems.
- **Suggested fix:** Use REGISTRY as single source of truth.

#### 2. Hardcoded Language List in MCP Tool Schema
- **Difficulty:** easy
- **Location:** `src/mcp.rs:412`
- **Description:** Language filter enum is hardcoded, requires manual update.
- **Suggested fix:** Generate schema dynamically from REGISTRY.all().

#### 3. Hardcoded Chunk Size and Token Limits
- **Difficulty:** easy
- **Location:** `src/parser.rs:286-287`, `src/cli/mod.rs:30-32`
- **Description:** Multiple hardcoded limits scattered across files.
- **Suggested fix:** Move to Config struct with sensible defaults.

#### 4. HNSW Index Parameters Not Configurable
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:47-55`
- **Description:** HNSW parameters (M, EF) are hardcoded.
- **Suggested fix:** Add to Config or CLI flags.

#### 5. ChunkType Enum Requires Manual Updates
- **Difficulty:** medium
- **Location:** `src/language/mod.rs:62-80`, `src/parser.rs:320-328`
- **Description:** Adding chunk type requires updates in 5+ places.
- **Suggested fix:** Consolidate type mapping with language definitions.

#### 6. Project Root Markers Hardcoded
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:300-306`
- **Description:** Project detection markers hardcoded.
- **Suggested fix:** Make configurable in Config.

#### 7. SignatureStyle Enum is Closed
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:52-59`
- **Description:** Only two variants, new patterns need enum changes.
- **Suggested fix:** Make signature extraction a callback function.

#### 8. Embedding Model Hardcoded
- **Difficulty:** medium
- **Location:** `src/embedder.rs:14-16`, `src/store/helpers.rs:12`
- **Description:** Model hardcoded to e5-base-v2 in multiple places.
- **Suggested fix:** Document how to add models, make selection configurable.

#### 9. RRF K Constant Hardcoded
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:260`
- **Description:** RRF K=60 constant is hardcoded.
- **Suggested fix:** Add to SearchFilter or Config.

#### 10. Callee Skip List Hardcoded
- **Difficulty:** easy
- **Location:** `src/parser.rs:553-558`
- **Description:** Names to filter from call graph are hardcoded.
- **Suggested fix:** Move to language definitions or make configurable.

#### 11. MCP Tool Registration Not Plugin-Based
- **Difficulty:** hard
- **Location:** `src/mcp.rs:388-527`
- **Description:** Tools defined inline, no extension points.
- **Suggested fix:** Consider trait-based tool registration system.

#### 12. Sentiment Thresholds Hardcoded
- **Difficulty:** easy
- **Location:** `src/note.rs:59-65,75-82`
- **Description:** Warning/pattern thresholds hardcoded in multiple places.
- **Suggested fix:** Define as constants at module level.

#### 13. No Extension Points for Custom Indexing Hooks
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs:646-1028`
- **Description:** Indexing pipeline is monolithic, no hooks for custom processing.
- **Suggested fix:** Add trait-based indexing hooks.

#### 14. VectorIndex Trait Minimal Interface
- **Difficulty:** medium
- **Location:** `src/index.rs:21-39`
- **Description:** Trait is minimal, hard to add capabilities without expanding.
- **Suggested fix:** Add optional methods or capability queries.

---

## Batch 3: Data & Platform Correctness

### Data Integrity

#### 1. HNSW and SQLite Index Out-of-Sync Risk
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:1118-1133`
- **Description:** HNSW rebuilt only at end of indexing. If save fails, indexes become inconsistent.
- **Suggested fix:** Add version/epoch counter in SQLite validated against HNSW checksum.

#### 2. Non-Atomic File Writes in HNSW Save
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:297-342`
- **Description:** Sequential writes without atomicity. Interruption leaves partial files.
- **Suggested fix:** Write to temp files first, then atomically rename.

#### 3. Non-Atomic Note File Append in MCP
- **Difficulty:** easy
- **Location:** `src/mcp.rs:952-961`
- **Description:** Concurrent appends can interleave writes, corrupting TOML.
- **Suggested fix:** Use file locking around append operation.

#### 4. Delete + Insert Without Transaction
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:110-128`, `src/store/notes.rs:121-138`
- **Description:** Sequential DELETEs without transaction can leave inconsistent state.
- **Suggested fix:** Wrap both DELETE statements in single transaction.

#### 5. No Embedding Dimension Validation on Load from SQLite
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:231-245`
- **Description:** Dimension mismatch only logs warning, produces incorrect embeddings.
- **Suggested fix:** Make dimension mismatches a hard error.

#### 6. HNSW ID Map vs Graph Size Mismatch Not Validated
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:348-400`
- **Description:** No validation that id_map.len() matches HNSW graph size.
- **Suggested fix:** Compare and fail fast if they don't match.

#### 7. SQLite PRAGMA synchronous = NORMAL May Lose Data on Crash
- **Difficulty:** hard
- **Location:** `src/store/mod.rs:75`
- **Description:** Last few transactions may be lost on power failure.
- **Suggested fix:** Document tradeoff, add config option for FULL mode.

#### 8. No Schema Version Migration Path
- **Difficulty:** hard
- **Location:** `src/store/mod.rs:159-183`
- **Description:** Schema version change requires full --force rebuild, losing all data.
- **Suggested fix:** Implement incremental schema migrations.

#### 9. Model Dimensions Not Validated at Runtime
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:138-143`
- **Description:** check_model_version validates name but not dimensions.
- **Suggested fix:** Add dimension validation comparing with EMBEDDING_DIM.

#### 10. FTS5 Table Can Become Orphaned from Main Table
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:53-67`
- **Description:** FTS delete errors swallowed, can leave stale entries.
- **Suggested fix:** Propagate FTS delete error or log prominently.

---

### Edge Cases

#### 1. Unicode String Slicing Panic in MCP add_note Preview
- **Difficulty:** easy
- **Location:** `src/mcp.rs:991`
- **Description:** Byte slicing `&text[..100]` panics on multi-byte UTF-8.
- **Suggested fix:** Use .chars().take(100).collect::<String>().

#### 2. Debug-Only Assertion for Embedding Dimension Mismatch
- **Difficulty:** medium
- **Location:** `src/search.rs:18-20`
- **Description:** debug_assert for dimensions means release builds silently truncate.
- **Suggested fix:** Use proper assert or return Result.

#### 3. Signature Extraction Byte Slicing
- **Difficulty:** medium
- **Location:** `src/parser.rs:400`
- **Description:** Slicing assumes char boundary from ASCII find.
- **Suggested fix:** Add safety check that position is valid char boundary.

#### 4. Empty Query Returns Empty Without Feedback
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:237-238`
- **Description:** Queries normalizing to empty string give zero results silently.
- **Suggested fix:** Surface warning when query normalizes to empty.

#### 5. No Explicit Check for Maximum Query Length
- **Difficulty:** easy
- **Location:** `src/embedder.rs`
- **Description:** No maximum query length could cause tokenizer issues.
- **Suggested fix:** Add explicit maximum query length with clear error.

#### 6. HNSW Load Trusts File Without Bounds Checking
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:103`
- **Description:** Loaded neighbor indices not validated against id_map bounds.
- **Suggested fix:** Validate all neighbor indices within range.

#### 7. Content Hash Slicing Assumes Hex Format
- **Difficulty:** easy
- **Location:** `src/parser.rs:372`, `src/cli/mod.rs:696`
- **Description:** content_hash[..8] assumes BLAKE3 always produces 8+ chars.
- **Suggested fix:** Use .get(..8).unwrap_or(&content_hash).

#### 8. No Limit on Number of Notes in Memory During Search
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:74-77`
- **Description:** search_notes loads ALL notes into memory.
- **Suggested fix:** Add pagination or configurable limit.

#### 9. Parser Capture Index Cast Without Bounds Check
- **Difficulty:** easy
- **Location:** `src/parser.rs:631`
- **Description:** Direct indexing with c.index as usize could panic.
- **Suggested fix:** Use .get() with fallback.

#### 10. CAGRA Index Neighbor Index Cast
- **Difficulty:** medium
- **Location:** `src/cagra.rs:314-315`
- **Description:** Cast from potentially negative i32 values.
- **Suggested fix:** Use try_into with proper filtering.

#### 11. FTS Query Injection Risk with Special Characters
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:294-323`
- **Description:** FTS5 operators not explicitly escaped.
- **Suggested fix:** Consider explicit escaping of FTS5 operators.

#### 12. No Graceful Handling of Deep Directory Trees
- **Difficulty:** hard
- **Location:** `src/source/filesystem.rs`
- **Description:** No depth limit could cause stack overflow or memory issues.
- **Suggested fix:** Add configurable maximum directory depth.

#### 13. Missing Empty Result Handling Distinction
- **Difficulty:** easy
- **Location:** `src/search.rs:205-206`
- **Description:** Empty vec doesn't distinguish "no matches" from silent failure.
- **Suggested fix:** Consider result type with more information.

#### 14. Potential Integer Overflow in Window Calculation
- **Difficulty:** easy
- **Location:** `src/embedder.rs:265`
- **Description:** Very small step size could cause slow windowing.
- **Suggested fix:** Document expected relationship between max_tokens and overlap.

---

### Platform Behavior

#### 1. Unix-only ONNX Provider Library Handling
- **Difficulty:** medium
- **Location:** `src/embedder.rs:487-567`
- **Description:** Linux-specific paths, LD_LIBRARY_PATH, Unix symlinks.
- **Suggested fix:** Add Windows-specific implementation with proper paths.

#### 2. Windows process_exists Uses Shell Command
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:381-389`
- **Description:** Spawns tasklist subprocess instead of Win32 API.
- **Suggested fix:** Use Windows API directly via windows-sys crate.

#### 3. Path Separator Inconsistency in Stored Origins
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:33`, `src/store/calls.rs:115`
- **Description:** Native separators stored, cross-platform access fails.
- **Suggested fix:** Normalize all stored paths to forward slashes.

#### 4. Hardcoded Forward Slash in Path Joins
- **Difficulty:** easy
- **Location:** `src/mcp.rs:220`, `src/cli/mod.rs:1351`
- **Description:** Inconsistent separator expectations.
- **Suggested fix:** Consistently use Path::join() for all paths.

#### 5. SQLite URL Path Handling on Windows
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:62`
- **Description:** path.display() produces backslashes, SQLite URL expects forward.
- **Suggested fix:** Convert path to forward slashes for URL.

#### 6. No Line Ending Normalization for Source Files
- **Difficulty:** easy
- **Location:** `src/parser.rs:247`, `src/source/filesystem.rs:96`
- **Description:** CRLF vs LF affects content hash and embeddings.
- **Suggested fix:** Normalize line endings to LF after reading.

#### 7. Case Sensitivity Assumptions in Path Matching
- **Difficulty:** hard
- **Location:** `src/search.rs`, `src/cli/mod.rs:357-367`
- **Description:** Different behavior on case-sensitive vs case-insensitive filesystems.
- **Suggested fix:** Document behavior, use case-insensitive for security checks.

#### 8. libc Dependency Without Windows Alternative
- **Difficulty:** easy
- **Location:** `Cargo.toml:93`, `src/cli/mod.rs:378`
- **Description:** libc is unconditional dependency but only used on Unix.
- **Suggested fix:** Make libc a target-specific dependency.

#### 9. GPU Features Assume Linux
- **Difficulty:** hard
- **Location:** `src/embedder.rs:498`, `src/cagra.rs`
- **Description:** CUDA paths Linux-specific, Windows GPUs not supported.
- **Suggested fix:** Investigate Windows-specific paths, document limitations.

---

### Memory Management

#### 1. Unbounded file collection in enumerate_files
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:340-368`
- **Description:** All file paths collected into Vec without limit.
- **Suggested fix:** Add optional limit or streaming approach.

#### 2. All embeddings loaded into memory for HNSW build
- **Difficulty:** hard
- **Location:** `src/store/chunks.rs:334-350`, `src/cli/mod.rs:1124`
- **Description:** Fetches every embedding at once (~300MB for 100k chunks).
- **Suggested fix:** Implement batched iteration.

#### 3. Unbounded search result collection in search_filtered
- **Difficulty:** medium
- **Location:** `src/search.rs:131-177`
- **Description:** Fetches ALL rows including embeddings per search.
- **Suggested fix:** Pre-filter via SQL or use pagination.

#### 4. Full file content read into memory during parsing
- **Difficulty:** medium
- **Location:** `src/parser.rs:247`, `src/source/filesystem.rs:96`
- **Description:** Concurrent large file processing can spike memory.
- **Suggested fix:** Add concurrent file limit or memory budget.

#### 5. CAGRA dataset duplication in memory
- **Difficulty:** hard
- **Location:** `src/cagra.rs:60-68`, `src/cagra.rs:100-109`
- **Description:** Full copy of embeddings for rebuild doubles memory.
- **Suggested fix:** Document tradeoff, consider lazy rebuild.

#### 6. Intermediate Vec allocations in embedding batch
- **Difficulty:** easy
- **Location:** `src/embedder.rs:369-376`
- **Description:** Multiple intermediate Vecs per batch item.
- **Suggested fix:** Use pre-allocated reusable buffers.

#### 7. Clone-heavy search result path
- **Difficulty:** easy
- **Location:** `src/search.rs:254-264`
- **Description:** Each result clones entire content string.
- **Suggested fix:** Use references or Arc for shared content.

#### 8. Unbounded note accumulation in parse_notes
- **Difficulty:** easy
- **Location:** `src/note.rs:92-112`
- **Description:** No limit on number of notes parsed.
- **Suggested fix:** Add MAX_NOTES constant with validation.

#### 9. Pipeline channel buffers hold full chunk data
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:654-658`
- **Description:** 256 depth * 32 chunks * 100KB = 800MB potential.
- **Suggested fix:** Reduce channel depth or add memory-based backpressure.

#### 10. Watch mode pending_files grows unbounded
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1561`
- **Description:** HashSet grows without limit during debounce.
- **Suggested fix:** Add cap on pending_files count.

---

### Concurrency Safety

#### 1. CAGRA Index Not Restored After Search Error
- **Difficulty:** easy
- **Location:** `src/cagra.rs:281-303`
- **Description:** Index consumed on search failure, not restored.
- **Suggested fix:** Document intentional behavior or rebuild on error.

#### 2. Potential Lock Ordering Issue Between resources and index
- **Difficulty:** medium
- **Location:** `src/cagra.rs:155-196`
- **Description:** Lock ordering could cause deadlock if modified.
- **Suggested fix:** Document lock ordering invariant prominently.

#### 3. Race Condition Window in Embedder Cache
- **Difficulty:** easy
- **Location:** `src/embedder.rs:305-322`
- **Description:** Check-then-act pattern allows redundant computation.
- **Suggested fix:** Use lock-held pattern or dashmap entry API.

#### 4. HNSW LoadedHnsw Relies on Unsafe Send+Sync
- **Difficulty:** hard
- **Location:** `src/hnsw.rs:157-158`
- **Description:** Safety depends on hnsw_rs internals, uses lifetime transmute.
- **Suggested fix:** Add compile-time version check, pin hnsw_rs version.

#### 5. CLI Pipeline Uses Multiple Producers
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:753-873`
- **Description:** GPU/CPU threads both consume from channels, complex flow.
- **Suggested fix:** Add documentation about expected flow.

#### 6. MCP Background CAGRA Build Opens Separate Store
- **Difficulty:** easy
- **Location:** `src/mcp.rs:254-280`
- **Description:** Separate connection could see stale data under write load.
- **Suggested fix:** Document snapshot semantics.

#### 7. Global INTERRUPTED Without Explicit Memory Ordering
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:56-76`
- **Description:** SeqCst is stronger than needed for signal handling.
- **Suggested fix:** Low priority: change to Release/Acquire.

#### 8. HTTP Server RwLock Around McpServer
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1179-1204`
- **Description:** Outer RwLock may be unnecessary given interior mutability.
- **Suggested fix:** Consider removing outer RwLock.

---

## Batch 4: Security & Performance

### Input Security

#### 1. TOML Injection in cqs_add_note Mention Escaping
- **Difficulty:** easy
- **Location:** `src/mcp.rs:916-919`
- **Description:** Backslashes before quotes not handled in TOML escaping.
- **Suggested fix:** Use proper TOML escaping or library serialization.

#### 2. No Input Validation on Glob Patterns
- **Difficulty:** easy
- **Location:** `src/search.rs:140-144`
- **Description:** Complex glob patterns could cause excessive CPU during compilation.
- **Suggested fix:** Add validation for pattern complexity, cache compiled patterns.

#### 3. Unbounded FTS Query Normalization
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:299-323`
- **Description:** Normalized output could be larger than input.
- **Suggested fix:** Add length check on normalized output.

#### 4. HNSW ID Map Deserialization Size Unlimited
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:361-363`
- **Description:** JSON deserialization without size limits could cause memory exhaustion.
- **Suggested fix:** Add sanity check on id_map size.

#### 5. Windows tasklist Command Argument Injection
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:383-389`
- **Description:** PID from lock file used in command without full validation.
- **Suggested fix:** Validate PID is numeric only and within valid range.

#### 6. Config File Loaded from User-Controlled Path
- **Difficulty:** easy
- **Location:** `src/config.rs:30-36`
- **Description:** Malicious .cqs.toml in cloned repo could set unexpected defaults.
- **Suggested fix:** Document behavior, consider flag to ignore project config.

#### 7. Notes File Path Not Separately Validated
- **Difficulty:** easy
- **Location:** `src/mcp.rs:936`
- **Description:** Hardcoded path but no explicit validation as defense-in-depth.
- **Suggested fix:** Add path validation.

---

### Data Security

#### 1. No Explicit File Permission Controls
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:321`, `src/mcp.rs:945-959`, `src/cli/mod.rs:456`
- **Description:** Created files inherit umask, may be world-readable.
- **Suggested fix:** Set permissions to 0600 for .cq/ files.

#### 2. Database Path Exposure in Error Messages
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:62`
- **Description:** Full filesystem path could leak in connection errors.
- **Suggested fix:** Wrap errors to show only relative paths.

#### 3. API Key Stored in Plaintext Memory
- **Difficulty:** medium
- **Location:** `src/mcp.rs:1182`, `src/cli/mod.rs:41`
- **Description:** Key remains in memory throughout process lifetime.
- **Suggested fix:** Use secrecy or zeroize crate to clear when done.

#### 4. CORS Allows Any Origin
- **Difficulty:** medium
- **Location:** `src/mcp.rs:1206-1209`
- **Description:** CorsLayer uses Any, relies on application-level origin check.
- **Suggested fix:** Make CORS more restrictive as defense in depth.

#### 5. Race Condition Window in Lock File Creation
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs:400-435`
- **Description:** Window between creation and lock acquisition.
- **Suggested fix:** Use O_EXCL equivalent or atomic creation.

---

### Algorithmic Complexity

#### 1. O(n*m) Word Overlap in name_match_score
- **Difficulty:** easy
- **Location:** `src/search.rs:63-72`
- **Description:** Nested iteration with substring contains() for each word pair.
- **Suggested fix:** Use HashSet for exact matching if acceptable.

#### 2. O(n) Brute-Force Note Search
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:66-118`
- **Description:** Always loads all notes and computes similarity against each.
- **Suggested fix:** Add notes to HNSW index or use FTS pre-filtering.

#### 3. Repeated tokenize_identifier Calls
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:299-323`
- **Description:** Allocates new vectors for each identifier tokenization.
- **Suggested fix:** Use streaming tokenizer or memoize common identifiers.

#### 4. Query Word Substring Contains O(q*n*len)
- **Difficulty:** easy
- **Location:** `src/search.rs:68-69`
- **Description:** Both directions of contains() checked in inner loop.
- **Suggested fix:** Use HashSet if exact matching acceptable.

#### 5. Linear Scan in prune_missing with Individual Deletes
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:131-159`
- **Description:** O(deleted * 2 SQL queries) for missing files.
- **Suggested fix:** Batch deletes using WHERE NOT IN.

#### 6. Cosine Similarity Fallback is O(dim)
- **Difficulty:** easy
- **Location:** `src/search.rs:22-25`
- **Description:** Manual dot product in fallback path.
- **Suggested fix:** No action needed - SIMD handles common case.

#### 7. Multiple Database Queries for stats()
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:227-301`
- **Description:** 7 separate sequential queries for stats.
- **Suggested fix:** Combine into single query using CTEs.

#### 8. HashSet Created Per Function in parse_file_calls
- **Difficulty:** easy
- **Location:** `src/parser.rs:675-677`
- **Description:** Many small HashSet allocations during parsing.
- **Suggested fix:** Reuse single HashSet by clearing between functions.

---

### I/O Efficiency

#### 1. Note Search Full Table Scan
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:67-117`
- **Description:** Fetches ALL notes for every search, no vector index.
- **Suggested fix:** Add notes to HNSW or create separate index.

#### 2. File Metadata Read Twice During Indexing
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:726-732`, `src/store/chunks.rs:87-93`
- **Description:** mtime retrieved twice for same file.
- **Suggested fix:** Cache mtime during enumeration.

#### 3. Checksum Verification Reads HNSW Files Twice
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:92-126`, `src/hnsw.rs:348-399`
- **Description:** Files read for checksum then read again for loading.
- **Suggested fix:** Verify checksum during loading or use mmap.

#### 4. HNSW Save Re-reads Files to Compute Checksums
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:324-334`
- **Description:** Files written then immediately read back for hashing.
- **Suggested fix:** Compute checksums from in-memory data.

#### 5. No Embedder Caching Across CLI Commands
- **Difficulty:** hard
- **Location:** `src/cli/mod.rs:1681,1757`
- **Description:** Each command re-initializes ONNX session (~500ms).
- **Suggested fix:** Consider daemon mode or warm flag.

#### 6. Call Graph Re-parses Files Already Parsed
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:1136-1161`
- **Description:** parse_file_calls re-reads and re-parses after chunk extraction.
- **Suggested fix:** Extract both in single parsing pass.

#### 7. Stats Command Loads HNSW Just for Length
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1361-1365`
- **Description:** Loads entire index to get vector count.
- **Suggested fix:** Store count in metadata file.

#### 8. Individual upsert_calls Per Chunk
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:992-997`
- **Description:** Calls upserted one chunk at a time.
- **Suggested fix:** Batch calls in single transaction.

#### 9. FileSystemSource Reads Content Eagerly
- **Difficulty:** medium
- **Location:** `src/source/filesystem.rs:88-127`
- **Description:** Reads all file content before checking if reindex needed.
- **Suggested fix:** Make content loading lazy.

#### 10. Database Metadata Queries Not Batched
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:258-289`
- **Description:** 6 separate queries for metadata.
- **Suggested fix:** Combine into single WHERE IN query.

---

### Resource Footprint

#### 1. Tokio Runtime Created Per-Store Instance
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:60`
- **Description:** Multiple stores create redundant runtimes.
- **Suggested fix:** Use shared global runtime.

#### 2. Separate HTTP Runtime Created
- **Difficulty:** easy
- **Location:** `src/mcp.rs:1240`
- **Description:** serve_http creates own runtime despite Store having one.
- **Suggested fix:** Reuse Store's runtime.

#### 3. ONNX Session Cold Start (~500ms)
- **Difficulty:** hard
- **Location:** `src/embedder.rs:151-152`
- **Description:** First query incurs model loading delay.
- **Suggested fix:** Pre-warm during MCP init, add --warm CLI flag.

#### 4. Background CAGRA Thread Not Tracked
- **Difficulty:** easy
- **Location:** `src/mcp.rs:238`
- **Description:** Detached thread continues on shutdown.
- **Suggested fix:** Store handle, join on Drop.

#### 5. Large Binary Size (34MB)
- **Difficulty:** hard
- **Location:** `Cargo.toml`
- **Description:** Tree-sitter grammars, ONNX runtime, tokio contribute.
- **Suggested fix:** Profile with cargo-bloat, lazy-load grammars.

#### 6. SQLite MMAP 256MB Unconditional
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:84`
- **Description:** High mmap size regardless of database size.
- **Suggested fix:** Make configurable, lower default.

#### 7. 4 Connections Per Store Pool
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:66`
- **Description:** Up to 12 connections with pipeline's 3 stores.
- **Suggested fix:** Share Store or reduce pool size.

#### 8. Query Cache Size Hardcoded
- **Difficulty:** easy
- **Location:** `src/embedder.rs:181-183`
- **Description:** 100 entries (~300KB) not configurable.
- **Suggested fix:** Make configurable via config file.

#### 9. Three Threads During Indexing
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:672,759,876`
- **Description:** Each has own Store and embedder instance.
- **Suggested fix:** Consider streaming or lazy CPU embedder init.

#### 10. HNSW Loaded Per CLI Query
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:1230-1246`
- **Description:** Index loaded from disk every invocation.
- **Suggested fix:** Cache in temp file or use MCP for interactive use.

#### 11. File Watcher Creates Embedder Per Reindex
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:1681`
- **Description:** ~500ms delay on first reindex.
- **Suggested fix:** Keep embedder alive in watch loop.

---

## Summary

| Batch | Category | Findings |
|-------|----------|----------|
| 1 | Code Hygiene | 8 |
| 1 | Module Boundaries | 11 |
| 1 | Documentation | 17 |
| 1 | API Design | 12 |
| 1 | Error Propagation | 14 |
| 2 | Observability | 12 |
| 2 | Test Coverage | 18 |
| 2 | Panic Paths | 8 |
| 2 | Algorithm Correctness | 6 |
| 2 | Extensibility | 14 |
| 3 | Data Integrity | 10 |
| 3 | Edge Cases | 14 |
| 3 | Platform Behavior | 9 |
| 3 | Memory Management | 10 |
| 3 | Concurrency Safety | 8 |
| 4 | Input Security | 7 |
| 4 | Data Security | 5 |
| 4 | Algorithmic Complexity | 8 |
| 4 | I/O Efficiency | 10 |
| 4 | Resource Footprint | 11 |
| **Total** | | **202** |

### By Difficulty

| Difficulty | Count |
|------------|-------|
| Easy | ~130 |
| Medium | ~55 |
| Hard | ~17 |

### Priority Tiers (per design)

**P1 - Fix immediately (Easy + Batch 1-2):**
- Documentation: model name mismatches (5 locations)
- Code Hygiene: line number clamping helper
- Panic Paths: display.rs bounds check
- Algorithm Correctness: Go return type broken

**P2 - Fix next (Easy + Batch 3-4, Medium + Batch 1):**
- Edge Cases: Unicode string slicing panic
- Module Boundaries: duplicate Language enum
- Error Propagation: swallowed .ok() patterns

**P3 - Fix if time permits:**
- Memory Management: unbounded collections
- I/O Efficiency: double parsing, double file reads

**P4 - Create issues, defer:**
- Hard items: schema migrations, platform GPU support, binary size

