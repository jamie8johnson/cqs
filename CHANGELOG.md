# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`cqs stale`**: New command to check index freshness. Lists files modified since last index and files in the index that no longer exist on disk. Supports `--json`, `--count-only`.
- **Proactive staleness warnings**: Search, explain, gather, and context commands now warn on stderr when results come from stale files. Suppressed with `-q`.
- **`cqs-stale` skill**: Agent skill for index freshness checks.

## [0.11.0] - 2026-02-11

### Added
- **Proactive hints** (#362): `cqs explain` and `cqs read --focus` now show caller count and test count for function/method chunks. JSON output includes `hints` object with `caller_count`, `test_count`, `no_callers`, `no_tests`.
- **`cqs impact-diff`** (#362): New command maps git diff hunks to indexed functions and runs aggregated impact analysis. Shows changed functions, affected callers, and tests to re-run. Supports `--base`, `--stdin`, `--json`.
- **Table-aware Markdown chunking** (#361): Markdown tables are chunked row-wise when exceeding 1500 characters. Parent retrieval via `--expand` flag.
- **Markdown RAG improvements** (#360): Richer embeddings with cross-document reference linking and heading hierarchy preservation.
- **`cqs-impact-diff` skill**: Agent skill for diff-aware impact analysis.

### Fixed
- **Suppress ort warning** (#363): Filter benign "nodes not assigned to preferred execution providers" warning from ONNX Runtime.
- **Double compute_hints in read.rs**: JSON mode was calling `compute_hints()` twice; now stores result and reuses.

## [0.10.2] - 2026-02-10

### Fixed
- **Stale MCP documentation**: Removed references to `cqs serve`, HTTP transport, and MCP setup from README, CONTRIBUTING, and PRIVACY. MCP server was removed in v0.10.0.

## [0.10.1] - 2026-02-10

### Added
- **CLI integration test harness** (#300): 27 new integration tests covering trace, impact, test-map, context, gather, explain, similar, audit-mode, notes, project, and read commands.
- **Embedding pipeline tests** (#344): 9 integration tests for document embedding, batch processing, determinism, and query vs document prefix differentiation.
- **Cross-store dedup** (#256): Reference search results deduplicated by content hash (blake3) — identical code from multiple indexes no longer appears twice.
- **Parallel reference search** (#257): Reference indexes searched concurrently via rayon instead of sequentially.
- **Streaming brute-force search** (#269): Cursor-based batching (5000 rows) replaces `fetch_all()` in brute-force path, reducing peak memory from O(total chunks) to O(batch size).
- **HNSW file size guards** (#303): Graph (500MB) and data (1GB) file size checks before deserialization prevent OOM on corrupted/malicious index files.
- **CAGRA OOM guard** (#302): 2GB allocation limit check before `Vec::with_capacity()` in GPU index building.

### Fixed
- **FTS5 injection defense-in-depth**: RRF search path now sanitizes FTS queries after normalization, closing a gap where special characters could reach MATCH.
- **HNSW checksum enforcement**: Missing checksum file now returns an error instead of silently loading unverified data.
- **Reference removal containment**: `ref remove` uses `dunce::canonicalize` + `starts_with` to verify deletion target is inside refs root directory.
- **Symlink reference rejection**: Symlink reference paths are skipped instead of loaded, preventing trust boundary bypass.
- **Display file size guard**: 10MB limit on files read for display, preventing accidental large file reads.
- **Config/notes size guards**: 1MB limit on config files, 10MB on notes files before `read_to_string`.
- **Similar command overflow**: `limit + 1` uses `saturating_add` to prevent overflow on `usize::MAX`.
- **Predictable temp file paths**: Notes temp files include PID suffix to prevent predictable path attacks.
- **Call graph edge cap**: 500K edge limit on call graph queries prevents unbounded memory on enormous codebases.
- **Trace depth validation**: `--max-depth` clamped to 1..50 via clap value parser.

## [0.10.0] - 2026-02-10

### Removed
- MCP server (`src/mcp/`, `cqs serve` command). All functionality available via CLI + skills.
- `cqs batch` command (was MCP-only, no CLI equivalent).
- Dependencies: axum, tower, tower-http, futures, tokio-stream, subtle, zeroize.
- Tokio slimmed from 6 features to 2 (`rt-multi-thread`, `time`).

### Changed
- `parse_duration()` moved from `src/mcp/validation.rs` to `src/audit.rs`.

## [0.9.9] - 2026-02-10

### Fixed
- **HNSW staleness in watch mode** (#236): Watch mode now rebuilds the HNSW index after reindexing changed files, so searches immediately find newly indexed code.
- **MCP server HNSW staleness** (#236): MCP server lazy-reloads the HNSW index when the on-disk checksum file changes, using mtime-based staleness detection.

### Changed
- **MSRV bumped to 1.93**: Minimum supported Rust version raised from 1.88 to 1.93.
- **Removed `fs4` dependency**: File locking now uses `std::fs::File::lock()` / `lock_shared()` / `try_lock()` (stable since Rust 1.89).
- **Removed custom `floor_char_boundary`**: Uses `str::floor_char_boundary()` from std (stable since Rust 1.91).
- **MSRV CI job**: New CI check validates compilation on the minimum supported Rust version.

## [0.9.8] - 2026-02-11

### Added
- **SQLite integrity check**: `PRAGMA quick_check` on every `Store::open()` catches B-tree corruption early with a clear `StoreError::Corruption` error.
- **Embedder session management**: `clear_session()` method releases ~500MB ONNX session memory during idle periods in long-running processes.
- **75 new tests** across search, store, reference, CLI, and MCP modules. Total: 339 lib + 243 integration tests.
- **FTS5 query sanitization**: Special characters and reserved words stripped before MATCH queries, preventing query syntax errors on user input.
- **Cursor-based embedding pagination**: `EmbeddingBatchIterator` uses `WHERE rowid > N` instead of `LIMIT/OFFSET` for stable iteration under concurrent writes.
- **GatherOptions builder API**: Fluent builder methods for configuring gather operations programmatically.
- **Store schema downgrade guard**: `migrate()` returns `StoreError::SchemaNewerThanCq` when index was created by a newer version.
- **WSL path detection**: Permission checks skip chmod on WSL-mounted filesystems where it silently fails.

### Fixed
- **125 audit fixes** from comprehensive 14-category code audit (9 PRs, P1-P3 priorities).
- **Byte truncation panics**: `normalize_for_fts` and notes list use `floor_char_boundary` for safe multi-byte string truncation.
- **Dead code false positives**: Trait impl detection checks parent chunk type instead of method body content.
- **Search fairness**: `BoundedScoreHeap` uses `>=` for equal-score entries, preventing iteration-order bias.
- **Gather determinism**: Tiebreak by name when scores are equal for reproducible results.
- **CLI limit validation**: `--limit` clamped to 1..100 range.
- **Config/project file locking**: Read-modify-write operations use file locks to prevent concurrent corruption.
- **Atomic watch mode updates**: Delete-then-reinsert wrapped in transactions for crash safety.
- **Pipeline transaction safety**: Chunk and call graph inserts in single transaction.
- **HNSW cross-device rename**: Fallback to copy+delete when temp file is on different filesystem.
- **Reference config trust boundary**: Warnings when reference config overrides project settings.
- **Path traversal protection**: `tool_context` validates paths before file access.
- **Protocol version truncation**: HTTP transport truncates version header to prevent abuse.
- **Embedding dimension validation**: `Embedding::new()` validates vector dimensions on construction.
- **Language::def() returns Option**: No more panics on unknown language variants.

### Changed
- **Shared library modules**: Extracted `resolve_target`, focused-read, note injection, impact analysis, and JSON serialization from duplicated CLI/MCP implementations into shared library code.
- **Observability**: 15+ tracing spans added across search, reference, embedder, and store operations. `eprintln` calls migrated to structured `tracing` logging.
- **Error handling**: Silent `.ok()` calls replaced with proper error propagation or degradation warnings.
- **Performance**: Watch mode batch upserts, embedding cache (hash-based skip), `search_by_names_batch` batched FTS, `bytemuck` for embedding serialization, lazy dead code content loading.
- **Dependencies**: `rand` 0.10, `cuvs` 26.2, `colored` 3.1.

## [0.9.7] - 2026-02-08

### Added
- **CLI-first migration**: All cqs features now available via CLI without MCP server. New commands: `cqs notes add/update/remove`, `cqs audit-mode on/off`, `cqs read <path> [--focus fn]`. New search flags: `--name-only`, `--semantic-only`. File-based audit mode persistence (`.cqs/audit-mode.json`) shared between CLI and MCP.
- **Hot-reload reference indexes**: MCP server detects config file changes and reloads reference indexes automatically. No restart needed after `cqs ref add/remove`.

### Fixed
- **Renamed `.cq/` index directory to `.cqs/`** for consistency with binary name, config directory, and config file. Auto-migration renames existing `.cq/` directories on first access. Cross-project search falls back to `.cq/` for unmigrated projects.

## [0.9.6] - 2026-02-08

### Added
- **Markdown language support**: 9th language. Indexes `.md` and `.mdx` files with heading-based chunking, adaptive heading detection (handles both standard and inverted hierarchies), and cross-reference extraction from links and backtick function patterns.
- `ChunkType::Section` for documentation chunks
- `SignatureStyle::Breadcrumb` for heading-path signatures (e.g., "Doc Title > Chapter > Subsection")
- `scripts/clean_md.py` for one-time PDF-to-markdown artifact preprocessing
- `lang-markdown` feature flag (enabled by default)
- Optional `grammar` field on `LanguageDef` for non-tree-sitter languages

## [0.9.5] - 2026-02-08

### Fixed
- **T-SQL name extraction**: `ALTER PROCEDURE` and `ALTER FUNCTION` now indexed (previously only `CREATE` variants)
- **Tree-sitter error recovery**: Position-based validation detects when `@name` capture matched wrong node; falls back to regex extraction from content text
- **Multi-line names**: Truncate to first line when tree-sitter error recovery extends name nodes past actual identifier
- Bump `tree-sitter-sequel-tsql` to 0.4.2 (bracket-quoted identifier support)

## [0.9.4] - 2026-02-07

### Added
- **SQL language support**: 8th language. Parses stored procedures, functions, and views from `.sql` files via forked [tree-sitter-sql](https://github.com/jamie8johnson/tree-sitter-sql) grammar with `CREATE PROCEDURE`, `GO` batch separator, and `EXEC` statement support.
- `SignatureStyle::UntilAs` for SQL's `AS BEGIN...END` pattern
- Schema-qualified name preservation (`dbo.usp_GetOrders`)
- SQL call graph extraction (function invocations + `EXEC`/`EXECUTE` statements)

## [0.9.3] - 2026-02-07

### Fixed
- **Gather search quality**: `gather()` and `search_across_projects()` now use RRF hybrid search instead of raw embedding-only cosine similarity. Previously missed results that keyword matching would find.

### Added
- `cqs_search` `note_only` parameter to search notes exclusively
- `cqs_context` `--summary` mode for condensed file overview
- `cqs_impact` `--format mermaid` output for dependency diagrams

## [0.9.2] - 2026-02-07

### Fixed
- **96 audit fixes** across P1 (43), P2 (23), P3 (30) from 14-category code audit
- **Config safety**: `add_reference_to_config` no longer destroys config on I/O errors
- **Watch mode**: call graph now updates during incremental reindex
- **Gather**: results sorted by score before truncation (was file order)
- **Diff**: language filter uses stored language field instead of file extension matching
- **Search robustness**: limit=0 early return, NaN score defense in BoundedScoreHeap, max_tokens=0 guard
- **Migration safety**: schema migrations wrapped in single transaction
- **Watch paths**: `dunce::canonicalize` for Windows UNC path handling
- **Config validation**: reference weights clamped to [0.0, 1.0], reference count limited to 20
- **Error propagation**: unwrap → Result throughout CLI and MCP tools
- **N+1 queries**: batched embedding lookups in diff and pipeline
- **Code consolidation**: DRY refactors in explain.rs, search.rs, notes.rs

### Added
- Tracing spans on `search_unified` and `search_by_candidates` for performance visibility
- MCP observability: tool entry/exit logging, client info on connect, pipeline stats
- Docstrings for `cosine_similarity` variants and `tool_stats` response fields
- Integration tests: dead code, semantic diff, gather BFS, call graph, reference search, MCP format
- `ChunkIdentity.language` field for language-aware operations
- MCP tool count corrected: 20 (was documented as 21)

### Changed
- `run_migration` accepts `&mut SqliteConnection` instead of `&SqlitePool` for transaction safety
- Context dedup uses typed struct instead of JSON string comparison

## [0.9.1] - 2026-02-06

### Changed
- **Refactor**: Split `parser.rs` (1072 lines) into `src/parser/` directory — mod.rs, types.rs, chunk.rs, calls.rs
- **Refactor**: Split `hnsw.rs` (1150 lines) into `src/hnsw/` directory — mod.rs, build.rs, search.rs, persist.rs, safety.rs
- Updated public-facing messaging to lead with token savings for AI agents
- Enhanced `groom-notes` skill with Phase 2 (suggest new notes from git history)
- Updated CONTRIBUTING.md architecture tree for new directory layout

### Fixed
- Flaky `test_loaded_index_multiple_searches` — replaced sin-based test embeddings with well-separated one-hot vectors

## [0.9.0] - 2026-02-06

### Added
- **`--chunk-type` filter** (CLI + MCP): narrow search to function/method/class/struct/enum/trait/interface/constant
- **`--pattern` filter** (CLI + MCP): post-search structural matching — builder, error_swallow, async, mutex, unsafe, recursion
- **`cqs dead`** (CLI + MCP): find functions/methods never called by indexed code. Excludes main, tests, trait impls. `--include-pub` for full audit
- **`cqs gc`** (CLI + MCP): prune chunks for deleted files, clean orphan call graph entries, rebuild HNSW. MCP reports staleness without modifying
- **`cqs gather`** (CLI + MCP): smart context assembly — BFS call graph expansion from semantic seed results. `--expand`, `--direction`, `--limit` params
- **`cqs project`** (CLI): cross-project search via `~/.config/cqs/projects.toml` registry. `register`, `list`, `remove`, `search` subcommands
- **`--format mermaid`** on `cqs trace`: generate Mermaid diagrams from call paths
- **Index staleness warnings**: `cqs stats` and MCP stats report stale/missing file counts
- 31 new unit tests (structural patterns, gather algorithm, project registry)
- MCP tool count: 17 → 21

## [0.8.0] - 2026-02-07

### Added
- **`cqs trace`** (CLI + MCP): follow a call chain between two functions — BFS shortest path through the call graph with file/line/signature enrichment
- **`cqs impact`** (CLI + MCP): impact analysis — what breaks if you change a function. Returns callers with call-site snippets, transitive callers (with `--depth`), and affected tests via reverse BFS
- **`cqs test-map`** (CLI + MCP): map functions to tests that exercise them — finds tests reachable via reverse call graph traversal with full call chains
- **`cqs batch`** (MCP-only): execute multiple queries in a single tool call — supports search, callers, callees, explain, similar, stats. Max 10 queries per batch
- **`cqs context`** (CLI + MCP): module-level understanding — lists all chunks, external callers/callees, dependent files, and related notes for a given file
- **Focused `cqs_read`**: new `focus` parameter on `cqs_read` MCP tool — returns target function + type dependencies instead of the whole file, cutting tokens by 50-80%
- Store methods: `get_call_graph()`, `get_callers_with_context()`, `find_test_chunks()`, `get_chunks_by_origin()`
- Shared `resolve.rs` modules for CLI and MCP target resolution (deduplicates parse_target/resolve_target from explain/similar)
- `CallGraph` and `CallerWithContext` types in store helpers
- MCP tool count: 12 → 17

## [0.7.0] - 2026-02-06

### Added
- **`cqs similar`** (CLI + MCP): find semantically similar functions by using a stored embedding as the query vector — search by example instead of by text
- **`cqs explain`** (CLI + MCP): generate a function card with signature, docs, callers, callees, and top-3 similar functions in one call
- **`cqs diff`** (CLI + MCP): semantic diff between indexed snapshots — compare project vs reference or two references, reports added/removed/modified with similarity scores
- **Workspace-aware indexing**: detect Cargo workspace root from member crates so `cqs index` indexes the whole workspace
- Store methods: `get_chunk_with_embedding()`, `all_chunk_identities()`, `ChunkIdentity` type

## [0.6.0] - 2026-02-06

### Added
- **Multi-index search**: search across project + reference codebases simultaneously
  - `cqs ref add <name> <source>` — index an external codebase as a reference
  - `cqs ref list` — show configured references with chunk/vector counts
  - `cqs ref remove <name>` — remove a reference and its index files
  - `cqs ref update <name>` — re-index a reference from its source
  - MCP `cqs_search` with `sources` parameter to filter which indexes to search
  - Score-based merge with configurable weight multiplier (default 0.8)
  - `cqs doctor` validates reference index health
  - `[[reference]]` config entries in `.cqs.toml`

### Fixed
- **P1 audit fixes** (12 items): path traversal in glob filter, pipeline mtime race, threshold consistency, SSE origin validation, stale documentation, error message leaks
- **P2 audit fixes** (5 items): dead `search_unified()` removal, CAGRA streaming gap, brute-force note search O(n) elimination, call graph error propagation, config parse error surfacing
- **P3 audit fixes** (11 items): `check_interrupted` stale flag, `unreachable!()` in name_only search, duplicated glob compilation, empty query bypass, CRLF handling, config file permissions (0o600), duplicated note insert SQL, HNSW match duplication, pipeline parse error reporting, panic payload extraction, IO error context in note rewrite

## [0.5.3] - 2026-02-06

### Added
- CJK tokenization: Chinese, Japanese, Korean characters split into individual FTS tokens
- `ChunkRow::from_row()` centralized SQLite row mapping in store layer
- `fetch_chunks_by_ids_async()` and `fetch_chunks_with_embeddings_by_ids_async()` store methods

### Changed
- `tool_add_note` uses `toml::to_string()` via serde instead of manual string escaping
- `search.rs` no longer constructs `ChunkRow` directly from raw SQLite rows

## [0.5.2] - 2026-02-06

### Added
- `cqs stats` now shows note count and call graph summary (total calls, unique callers, unique callees)
- `cqs notes list` CLI command to display all project notes with sentiment
- `cqs_update_note` and `cqs_remove_note` MCP tools for managing notes
- 8 Claude Code skills: audit, bootstrap, docs-review, groom-notes, pr, reindex, release, update-tears

### Changed
- Notes excluded from HNSW/CAGRA index; always brute-force from SQLite for freshness
- 4 safe skills (update-tears, groom-notes, docs-review, reindex) auto-invoke without `/` prefix

### Fixed
- README: documented `cqs_update_note`, `cqs_remove_note` MCP tools
- SECURITY: documented `docs/notes.toml` as MCP write path
- CONTRIBUTING: architecture overview updated with all skills

## [0.5.1] - 2026-02-05

### Fixed
- Algorithm correctness: glob filter applied BEFORE heap in brute-force search (was producing wrong results)
- `note_weight=0` now correctly excludes notes from unified search (was only zeroing scores)
- Windows path extraction in brute-force search uses `origin` column instead of string splitting
- GPU-to-CPU fallback no longer double-windows chunks
- Atomic note replacement (single transaction instead of delete+insert)
- Error propagation: 6 silent error swallowing sites now propagate errors
- Non-finite score validation (NaN/infinity checks in cosine similarity and search filters)
- FTS5 name query: terms now quoted to prevent syntax errors
- Empty query guard for `search_by_name`
- `split_into_windows` returns Result instead of panicking via assert
- Store Drop: `catch_unwind` around `block_on` to prevent panic in async contexts
- Stdio transport: line reads capped at 1MB
- `follow_links(false)` on filesystem walker (prevents symlink loops)
- `.cq/` directory created with 0o700 permissions
- `parse_file_calls` file size guard matching `parse_file`
- HNSW `count_vectors` size guard matching `load()`
- SQL IN clause batching for `get_embeddings_by_hashes` (chunks of 500)
- SQLite cache_size reduced from 64MB to 16MB per connection
- Path normalization gaps fixed in call_graph, graph, stats, filesystem source

### Changed
- `strip_unc_prefix` deduplicated into shared `path_utils` module
- `load_hnsw_index` deduplicated into `HnswIndex::try_load()`
- `index_notes_from_file` deduplicated — CLI now calls `cqs::index_notes()`
- MCP JSON-RPC types restricted to `pub(crate)` visibility
- Regex in `sanitize_error_message` compiled once via `LazyLock`
- `EMBEDDING_DIM` consolidated to single constant in `lib.rs`
- MCP stats uses `count_vectors()` instead of full HNSW load
- `note_stats` returns named struct instead of tuple
- Pipeline call graph upserts batched into single transaction
- HTTP server logging: `eprintln!` replaced with `tracing`
- MCP search: timing span added for observability
- GPU/CPU thread termination now logged
- Error sanitization regex covers `/mnt/` paths
- Watch mode: mtime cached per-file for efficiency
- Batch metadata checks on Store::open (single query)
- Consolidated note_stats and call_stats into fewer queries
- Dead code removed from `cli::run()`
- HNSW save uses streaming checksum (BufReader)
- Model BLAKE3 checksums populated for E5-base-v2

### Added
- 15 new search tests (HNSW-guided, brute-force, glob, language, unified, FTS)
- Test count: 379 (no GPU) up from 364

### Documentation
- `lib.rs` language list updated (C, Java)
- HNSW params corrected (M=24, ef_search=100)
- Cache size corrected (32 not 100)
- Roadmap phase updated
- Chunk cap documented as 100 lines
- Architecture tree updated with CLI/MCP submodules

## [0.5.0] - 2026-02-05

### Added
- **C and Java language support** (#222)
  - tree-sitter-c and tree-sitter-java grammars
  - 7 languages total (Rust, Python, TypeScript, JavaScript, Go, C, Java)
- **Test coverage expansion** (#224)
  - 50 new tests across 6 modules (cagra, index, MCP tools, pipeline, CLI)
  - Total: 375 tests (GPU) / 364 (no GPU)

### Changed
- **Model evaluation complete** (#221)
  - E5-base-v2 confirmed as best option: 100% Recall@5 (50/50 eval queries)
- **Parser/registry consolidation** (#223)
  - parser.rs reduced from 1469 to 1056 lines (28% reduction)
  - Parser re-exports Language, ChunkType from language module

## [0.4.6] - 2026-02-05

### Added
- **Schema migration framework** (#188, #215)
  - Migrations run automatically when opening older indexes
  - Falls back to error if no migration path exists
  - Framework ready for future schema changes
- **CLI integration tests** (#206, #213)
  - 12 end-to-end tests using `assert_cmd`
  - Tests for init, index, search, stats, completions
- **Server transport tests** (#205, #213)
  - 3 tests for stdio transport (initialize, tools/list, invalid JSON)
- **Stress tests** (#207, #213)
  - 5 ignored tests for heavy load scenarios
  - Run with `cargo test --test stress_test -- --ignored`
- **`--api-key-file` option** for secure API key loading (#202, #213)
  - Reads key from file, keeps secret out of process list
  - Uses `zeroize` crate for secure memory wiping

### Changed
- **Lazy grammar loading** (#208, #213)
  - Tree-sitter queries compile on first use, not at startup
  - Reduces startup time by 50-200ms
- **Pipeline resource sharing** (#204, #213)
  - Store shared via `Arc` across pipeline threads
  - Single Tokio runtime instead of 3 separate ones
- Note search warning now logs at WARN level when hitting 1000-note limit (#203, #213)

### Fixed
- **Atomic HNSW writes** (#186, #213)
  - Uses temp directory + rename pattern for crash safety
  - All 4 files written atomically together
- CLI test serialization to prevent HuggingFace Hub lock contention in CI

## [0.4.5] - 2026-02-05

### Added
- **20-category audit complete** - All P1-P4 items addressed (#199, #200, #201, #209)
  - ~243 findings across security, correctness, maintainability, and test coverage
  - Future improvements tracked in issues #202-208

### Changed
- FTS errors now propagate instead of silently failing (#201)
- Note scan capped at 1000 entries for memory safety (#201)
- HNSW build progress logging shows chunk/note breakdown (#201)

### Fixed
- Unicode/emoji handling in FTS5 search (#201)
- Go return type extraction for multiple returns (#201)
- CAGRA batch progress logging (#201)

## [0.4.4] - 2026-02-05

### Added
- **`note_weight` parameter** for controlling note prominence in search results (#183)
  - CLI: `--note-weight 0.5` (0.0-1.0, default 1.0)
  - MCP: `note_weight` parameter in cqs_search
  - Lower values make notes rank below code with similar semantic scores

### Changed
- CAGRA GPU index now uses streaming embeddings and includes notes (#180)
- Removed dead `search_unified()` function (#182) - only `search_unified_with_index()` was used

## [0.4.3] - 2026-02-05

### Added
- **Streaming HNSW build** for large repos (#107)
  - `Store::embedding_batches()` streams embeddings in 10k batches via LIMIT/OFFSET
  - `HnswIndex::build_batched()` builds index incrementally
  - Memory: O(batch_size) instead of O(n) - ~30MB peak instead of ~300MB for 100k chunks
- **Notes in HNSW index** for O(log n) search (#103)
  - Note IDs prefixed with `note:` in unified HNSW index
  - `Store::note_embeddings()` and `search_notes_by_ids()` for indexed note search
  - Index output now shows: `HNSW index: N vectors (X chunks, Y notes)`

### Changed
- HNSW build moved after note indexing to include notes in unified index

### Fixed
- O(n) brute-force note search eliminated - now uses HNSW candidates

## [0.4.2] - 2026-02-05

### Added
- GPU failures counter in index summary output
- `VectorIndex::name()` method for HNSW/CAGRA identification
- `active_index` field in cqs_stats showing which vector index is in use

### Changed
- `Config::merge` renamed to `override_with` for clarity
- `Language::FromStr` now returns `ParserError::UnknownLanguage` (thiserror) instead of anyhow
- `--verbose` flag now sets tracing subscriber to debug level
- Note indexing logic deduplicated into shared `cqs::index_notes()` function

### Fixed
- `check_cq_version` now logs errors at debug level instead of silently discarding
- Doc comments added for `IndexStats`, `UnifiedResult`, `CURRENT_SCHEMA_VERSION`

## [0.4.1] - 2026-02-05

### Changed
- Updated crates.io keywords for discoverability: added `mcp-server`, `vector-search`
- Added GitHub topics: `model-context-protocol`, `ai-coding`, `vector-search`, `onnx`

## [0.4.0] - 2026-02-05

### Added
- **Definition search mode** (`name_only`) for cqs_search (#165)
  - Use `name_only=true` for "where is X defined?" queries
  - Skips semantic embedding, searches function/struct names directly
  - Scoring: exact match 1.0, prefix 0.9, contains 0.7
  - Faster than glob for definition lookups
- `count_vectors()` method for fast HNSW stats without loading full index

### Changed
- CLI refactoring: extracted `watch.rs` from `mod.rs` (274 lines)
  - `cli/mod.rs` reduced from 2167 to 1893 lines

### Fixed
- P2 audit fixes (PRs #161-163):
  - HNSW checksum efficiency (hash from memory, not re-read file)
  - TOML injection prevention in note mentions
  - Memory caps for watch mode and note parsing (10k limits)
  - Platform-specific libc dependency (cfg(unix))

## [0.3.0] - 2026-02-04

### Added
- `cqs_audit_mode` MCP tool for bias-free code reviews (#101)
  - Excludes notes from search/read results during audits
  - Auto-expires after configurable duration (default 30m)
- Error path test coverage (#126, #149)
  - HNSW corruption tests: checksum mismatch, truncation, missing files
  - Schema validation tests: future/old version rejection, model mismatch
  - MCP edge cases: unicode queries, concurrent requests, nested JSON
- Unit tests for embedder.rs and cli.rs (#62, #132)
  - `pad_2d_i64` edge cases (4 tests)
  - `EmbedderError` display formatting (2 tests)
  - `apply_config_defaults` behavior (3 tests)
  - `ExitCode` values (1 test)
- Doc comments for CLI command functions (#70, #137)
- Test helper module `tests/common/mod.rs` (#137)
  - `TestStore` for automatic temp directory setup
  - `test_chunk()` and `mock_embedding()` utilities

### Changed
- Refactored `cmd_serve` to use `ServeConfig` struct (#138)
  - Removes clippy `too_many_arguments` warning
- Removed unused `ExitCode` variants (`IndexMissing`, `ModelMissing`) (#138)
- **Refactored Store module** (#125, #133): Split 1,916-line god object into focused modules
  - `src/store/mod.rs` (468 lines) - Store struct, open/init, FTS5, RRF
  - `src/store/chunks.rs` (352 lines) - Chunk CRUD operations
  - `src/store/notes.rs` (197 lines) - Note CRUD and search
  - `src/store/calls.rs` (220 lines) - Call graph storage/queries
  - `src/store/helpers.rs` (245 lines) - Types, embedding conversion
  - `src/search.rs` (531 lines) - Search algorithms, scoring
  - Largest file reduced from 1,916 to 531 lines (3.6x reduction)

### Fixed
- **CRITICAL**: MCP server concurrency issues (#128)
  - Embedder: `Option<T>` → `OnceLock<T>` for thread-safe lazy init
  - Audit mode: direct field → `Mutex<T>` for safe concurrent access
  - HTTP handler: `write()` → `read()` lock (concurrent reads safe)
- `name_match_score` now preserves camelCase boundaries (#131, #133)
  - Tokenizes before lowercasing instead of after

### Closed Issues
- #62, #70, #101, #102-#114, #121-#126, #142-#146, #148

## [0.2.1] - 2026-02-04

### Added
- Minimum Supported Rust Version (MSRV) declared: 1.88 (required by `ort` dependency)
- `homepage` and `readme` fields in Cargo.toml

### Changed
- Exclude internal files from crate package (AI context, audit docs, dev tooling)

## [0.2.0] - 2026-02-03

### Security
- **CRITICAL**: Fixed timing attack in API key validation using `subtle::ConstantTimeEq`
- Removed `rsa` vulnerability (RUSTSEC-2023-0071) by disabling unused sqlx default features

### Added
- IPv6 localhost support in origin validation (`http://[::1]`, `https://[::1]`)
- Property-based tests (9 total) for RRF fusion, embedder normalization, search bounds
- Fuzz tests (17 total) across nl.rs, note.rs, store.rs, mcp.rs for parser robustness
- MCP protocol edge case tests (malformed JSON-RPC, oversized payloads, unicode)
- FTS5 special character tests (wildcards, quotes, colons)
- Expanded SECURITY.md with threat model, trust boundaries, attack surface documentation
- Discrete sentiment scale documentation in CLAUDE.md

### Changed
- Split cli.rs into cli/ module (mod.rs + display.rs) for maintainability
- Test count: 75 → 162 (2x+ increase)
- `proptest` added to dev-dependencies

### Fixed
- RRF score bound calculation (duplicates can boost scores above naive maximum)
- `unwrap()` → `expect()` with descriptive messages (10 locations)
- CAGRA initialization returns empty vec instead of panic on failure
- Symlink logging in embedder (warns instead of silently skipping)
- clamp fix in `get_chunk_by_id` for edge cases

### Closed Issues
- #64, #66, #67, #68, #69, #74, #75, #76, #77, #78, #79, #80, #81, #82, #83, #84, #85, #86

## [0.1.18] - 2026-02-03

### Added
- `--api-key` flag and `CQS_API_KEY` env var for HTTP transport authentication
  - Required for non-localhost network exposure
  - Constant-time comparison to prevent timing attacks
- `--bind` flag to specify listen address (default: 127.0.0.1)
  - Non-localhost binding requires `--dangerously-allow-network-bind` and `--api-key`

### Changed
- Migrated from rusqlite to sqlx async SQLite (schema v10)
- Extracted validation functions for better code discoverability
  - `validate_api_key`, `validate_origin_header`, `validate_query_length`
  - `verify_hnsw_checksums` with extension allowlist
- Replaced `unwrap()` with `expect()` for better panic messages
- Added SAFETY comments to all unsafe blocks

### Fixed
- Path traversal vulnerability in HNSW checksum verification
- Integer overflow in saturating i64→u32 casts for database fields

### Security
- Updated `bytes` to 1.11.1 (RUSTSEC-2026-0007 integer overflow fix)
- HNSW checksum verification now validates extensions against allowlist

## [0.1.17] - 2026-02-01

### Added
- `--gpu` flag for `cqs serve` to enable GPU-accelerated query embedding
  - CPU (default): cold 0.52s, warm 22ms
  - GPU: cold 1.15s, warm 12ms (~45% faster warm queries)

### Changed
- Hybrid CAGRA/HNSW startup: HNSW loads instantly (~30ms), CAGRA builds in background
  - Server ready immediately, upgrades to GPU index transparently
  - Eliminates 1.2s blocking startup delay

### Fixed
- Search results now prioritize code over notes (60/40 split)
  - Notes enhance but don't dominate results
  - Reserve 60% of slots for code, notes fill the rest

## [0.1.16] - 2026-02-01

### Added
- Tracing spans for major operations (`cmd_index`, `cmd_query`, `embed_batch`, `search_filtered`)
- Version check warning when index was created by different cqs version
- `Embedding` type encapsulation with `as_slice()`, `as_vec()`, `len()` methods

### Fixed
- README: Corrected call graph documentation (cross-file works, not within-file only)
- Bug report template: Updated version placeholder

### Documentation
- Added security doc comment for MCP origin validation behavior

## [0.1.15] - 2026-02-01

### Added
- Full call graph coverage for large functions (>100 lines)
  - Separate `function_calls` table captures all calls regardless of chunk size limits
  - CLI handlers like `cmd_index` now have call graph entries
  - 1889 calls captured vs ~200 previously

### Changed
- Schema version: 4 → 5 (requires `cqs index --force` to rebuild)

## [0.1.14] - 2026-01-31

### Added
- Call graph analysis (`cqs callers`, `cqs callees`)
  - Extract function call relationships from source code
  - Find what calls a function and what a function calls
  - MCP tools: `cqs_callers`, `cqs_callees`
  - tree-sitter queries for call extraction across all 5 languages

### Changed
- Schema version: 3 → 4 (adds `calls` table)

## [0.1.13] - 2026-01-31

### Added
- NL module extraction (src/nl.rs)
  - `generate_nl_description()` for code→NL→embed pipeline
  - `tokenize_identifier()` for camelCase/snake_case splitting
  - JSDoc parsing for JavaScript (@param, @returns tags)
- Eval improvements
  - Eval suite uses NL pipeline (matches production)
  - Runs in CI on tagged releases

## [0.1.12] - 2026-01-31

### Added
- Code→NL embedding pipeline (Greptile approach)
  - Embeds natural language descriptions instead of raw code
  - Generates: "A function named X. Takes parameters Y. Returns Z."
  - Doc comments prioritized as human-written NL
  - Identifier normalization: `parseConfig` → "parse config"

### Changed
- Schema version: 2 → 3 (requires `cqs index --force` to rebuild)

### Breaking Changes
- Existing indexes must be rebuilt with `--force`

## [0.1.11] - 2026-01-31

### Added
- MCP: `semantic_only` parameter to disable RRF hybrid search when needed
- MCP: HNSW index status in `cqs_stats` output

### Changed
- tree-sitter-rust: 0.23 -> 0.24
- tree-sitter-python: 0.23 -> 0.25
- Raised brute-force warning threshold from 50k to 100k chunks

### Documentation
- Simplified CLAUDE.md and tears system
- Added docs/SCARS.md for failed approaches
- Consolidated PROJECT_CONTINUITY.md (removed dated files)

## [0.1.10] - 2026-01-31

### Added
- RRF (Reciprocal Rank Fusion) hybrid search combining semantic + FTS5 keyword search
- FTS5 virtual table for full-text keyword search
- `normalize_for_fts()` for splitting camelCase/snake_case identifiers into searchable words
- Chunk-level incremental indexing (skip re-embedding unchanged chunks via content_hash)
- `Store::get_embeddings_by_hashes()` for batch embedding lookup

### Changed
- Schema version bumped from 1 to 2 (FTS5 support)
- RRF enabled by default in CLI and MCP for improved recall

## [0.1.9] - 2026-01-31

### Added
- HNSW-guided filtered search (10-100x faster for filtered queries)
- SIMD-accelerated cosine similarity via simsimd crate
- Shell completion generation (`cqs completions bash/zsh/fish/powershell`)
- Config file support (`.cqs.toml` in project, `~/.config/cqs/config.toml` for user)
- Lock file with PID for stale lock detection
- Rustdoc documentation for public API

### Changed
- Error messages now include actionable hints
- Improved unknown language/tool error messages

## [0.1.8] - 2026-01-31

### Added
- HNSW index for O(log n) search on large codebases (>50k chunks)
- Automatic HNSW index build after indexing
- Query embedding LRU cache (32 entries)

### Fixed
- RwLock poison recovery in HTTP handler
- LRU cache poison recovery in embedder
- Query length validation (8KB max)
- Embedding byte validation with warning

## [0.1.7] - 2026-01-31

### Fixed
- Removed `Parser::default()` panic risk
- Added logging for silent search errors
- Clarified embedder unwrap with expect()
- Added parse error logging in watch mode
- Added 100KB chunk byte limit (handles minified files)
- Graceful HTTP shutdown with Ctrl+C handler
- Protocol version constant consistency

## [0.1.6] - 2026-01-31

### Added
- Connection pooling with r2d2-sqlite (4 max connections)
- Request body limit (1MB) via tower middleware
- Secure UUID generation (timestamp + random)

### Fixed
- lru crate vulnerability (0.12 -> 0.16, GHSA-rhfx-m35p-ff5j)

### Changed
- Store methods now take `&self` instead of `&mut self`

## [0.1.5] - 2026-01-31

### Added
- SSE stream support via GET /mcp
- GitHub Actions CI workflow (build, test, clippy, fmt)
- Issue templates for bug reports and feature requests
- GitHub releases with changelogs

## [0.1.4] - 2026-01-31

### Changed
- MCP 2025-11-25 compliance (Origin validation, Protocol-Version header)
- Batching removed per MCP spec update

## [0.1.3] - 2026-01-31

### Added
- Watch mode (`cqs watch`) with debounce
- HTTP transport (MCP Streamable HTTP spec)
- .gitignore support via ignore crate

### Changed
- CLI restructured (query as positional arg, flags work anywhere)
- Replaced walkdir with ignore crate

### Fixed
- Compiler warnings

## [0.1.2] - 2026-01-31

### Added
- New chunk types: Class, Struct, Enum, Trait, Interface, Constant
- Hybrid search with `--name-boost` flag
- Context display with `-C N` flag
- Doc comments included in embeddings

## [0.1.1] - 2026-01-31

### Fixed
- Path pattern filtering (relative paths)
- Invalid language error handling

## [0.1.0] - 2026-01-31

### Added
- Initial release
- Semantic code search for 5 languages (Rust, Python, TypeScript, JavaScript, Go)
- tree-sitter parsing for function/method extraction
- nomic-embed-text-v1.5 embeddings (768-dim) [later changed to E5-base-v2 in v0.1.16]
- GPU acceleration (CUDA/TensorRT) with CPU fallback
- SQLite storage with WAL mode
- MCP server (stdio transport)
- CLI commands: init, doctor, index, stats, serve
- Filter by language (`-l`) and path pattern (`-p`)

[0.9.3]: https://github.com/jamie8johnson/cqs/compare/v0.9.2...v0.9.3
[0.9.2]: https://github.com/jamie8johnson/cqs/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/jamie8johnson/cqs/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/jamie8johnson/cqs/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/jamie8johnson/cqs/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/jamie8johnson/cqs/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/jamie8johnson/cqs/compare/v0.5.3...v0.6.0
[0.5.3]: https://github.com/jamie8johnson/cqs/compare/v0.5.2...v0.5.3
[0.5.2]: https://github.com/jamie8johnson/cqs/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/jamie8johnson/cqs/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/jamie8johnson/cqs/compare/v0.4.6...v0.5.0
[0.4.6]: https://github.com/jamie8johnson/cqs/compare/v0.4.5...v0.4.6
[0.4.5]: https://github.com/jamie8johnson/cqs/compare/v0.4.4...v0.4.5
[0.4.4]: https://github.com/jamie8johnson/cqs/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/jamie8johnson/cqs/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/jamie8johnson/cqs/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/jamie8johnson/cqs/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/jamie8johnson/cqs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/jamie8johnson/cqs/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/jamie8johnson/cqs/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/jamie8johnson/cqs/compare/v0.1.18...v0.2.0
[0.1.18]: https://github.com/jamie8johnson/cqs/compare/v0.1.17...v0.1.18
[0.1.17]: https://github.com/jamie8johnson/cqs/compare/v0.1.16...v0.1.17
[0.1.16]: https://github.com/jamie8johnson/cqs/compare/v0.1.15...v0.1.16
[0.1.15]: https://github.com/jamie8johnson/cqs/compare/v0.1.14...v0.1.15
[0.1.14]: https://github.com/jamie8johnson/cqs/compare/v0.1.13...v0.1.14
[0.1.13]: https://github.com/jamie8johnson/cqs/compare/v0.1.12...v0.1.13
[0.1.12]: https://github.com/jamie8johnson/cqs/compare/v0.1.11...v0.1.12
[0.1.11]: https://github.com/jamie8johnson/cqs/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/jamie8johnson/cqs/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/jamie8johnson/cqs/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/jamie8johnson/cqs/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/jamie8johnson/cqs/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/jamie8johnson/cqs/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/jamie8johnson/cqs/compare/v0.1.4...v0.1.5
[0.1.4]: https://github.com/jamie8johnson/cqs/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/jamie8johnson/cqs/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/jamie8johnson/cqs/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/jamie8johnson/cqs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/jamie8johnson/cqs/releases/tag/v0.1.0
