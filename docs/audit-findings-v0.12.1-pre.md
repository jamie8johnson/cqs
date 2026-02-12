# Audit Findings — v0.9.7

## Observability

#### O1: gather() has no logging or tracing spans
- **Difficulty:** easy
- **Location:** `src/gather.rs:78-192`
- **Description:** The `gather()` function is the core of the `cqs gather` command — a multi-step operation (seed search, BFS expansion, batch fetch, dedup, sort). It completes silently with zero tracing spans, no timing, and no structured logging. The only log is the fallback warning at line 143. When gather returns unexpected results (too few, wrong functions), there's no diagnostic trail: how many seeds were found, how many nodes BFS expanded, whether expansion was capped, how many chunks the batch fetch returned vs requested.
- **Suggested fix:** Add an `info_span!("gather")` around the whole function. Log seed result count, BFS expansion count, whether capped, and final chunk count at info level.

#### O2: semantic_diff() has no logging or timing
- **Difficulty:** easy
- **Location:** `src/diff.rs:88-222`
- **Description:** `semantic_diff()` loads all chunk identities from two stores, batch-fetches all embeddings, computes pairwise cosine similarity, and produces a diff report. No tracing spans, no timing, no counts. For large indexes (10k+ chunks), this operation can take seconds. If it returns unexpected results (e.g., everything "modified"), there's nothing to tell you how many source/target chunks were loaded, how many matched, or if embeddings were missing.
- **Suggested fix:** Add `info_span!("semantic_diff")`, log source/target chunk counts, matched pair count, and missing embedding count.

#### O3: Config loading uses eprintln instead of tracing
- **Difficulty:** easy
- **Location:** `src/config.rs:74,83`
- **Description:** `Config::load()` uses `eprintln!("Warning: {}", e)` for config file errors instead of `tracing::warn!`. This bypasses the structured logging pipeline — these errors won't appear in MCP server logs (which capture tracing output) and can't be filtered/queried. Already flagged as O12 in v0.9.1 triage, but still not fixed.
- **Suggested fix:** Replace `eprintln!("Warning: {}", e)` with `tracing::warn!(error = %e, "Failed to load config")` at both locations.

#### O4: resolve_index_dir migration uses eprintln
- **Difficulty:** easy
- **Location:** `src/lib.rs:124-125`
- **Description:** The `.cq` to `.cqs` directory migration at `resolve_index_dir()` uses `eprintln!` to notify about the migration. This is a one-time important event that should be captured in structured logs, not just stderr.
- **Suggested fix:** Replace `eprintln!("Migrated...")` with `tracing::info!("Migrated index directory from .cq/ to .cqs/")`.

#### O5: CLI notes commands use eprintln for warnings
- **Difficulty:** easy
- **Location:** `src/cli/commands/notes.rs:221,317,380`
- **Description:** Three places in the notes CLI commands use `eprintln!("Warning: {}", err)` instead of structured tracing. These warnings (from note add/update/remove operations) are invisible to log aggregation.
- **Suggested fix:** Replace with `tracing::warn!(error = %err, "Note operation warning")`.

#### O6: cmd_gather has no tracing span
- **Difficulty:** easy
- **Location:** `src/cli/commands/gather.rs:11-104`
- **Description:** The `cmd_gather` CLI command has no tracing span, unlike `cmd_index` and `cmd_query` which both have `info_span!`. When debugging slow or empty gather results, there's no parent span to correlate child operations.
- **Suggested fix:** Add `let _span = tracing::info_span!("cmd_gather", query_len = query.len(), expand, limit).entered();` at the start.

#### O7: cmd_gc has no tracing span or timing
- **Difficulty:** easy
- **Location:** `src/cli/commands/gc.rs:17-96`
- **Description:** The `cmd_gc` function does file enumeration, chunk pruning, call graph cleanup, and optional HNSW rebuild — all with zero tracing. For large indexes, GC can take seconds and the user has no visibility into which phase is slow.
- **Suggested fix:** Add `info_span!("cmd_gc")` and log pruned_chunks/pruned_calls counts at info level.

#### O8: extract_call_graph has no timing or progress
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:172-194`
- **Description:** `extract_call_graph()` iterates all files and parses call relationships. For large codebases (1000+ files), this takes several seconds with no progress indication and no tracing span. The user sees "Extracting call graph..." then silence until it finishes.
- **Suggested fix:** Add `info_span!("extract_call_graph", file_count = files.len())` and log the total call count at completion.

#### O9: build_hnsw_index has no timing
- **Difficulty:** easy
- **Location:** `src/cli/commands/index.rs:239-254`
- **Description:** `build_hnsw_index()` streams all embeddings from SQLite and builds an HNSW graph. For 10k+ chunks, this takes 2-5 seconds. No tracing span wraps the whole operation — individual HNSW build has logging, but the "stream from DB + build + save" pipeline has no overall timing.
- **Suggested fix:** Add `info_span!("build_hnsw_index", chunk_count)` with elapsed timing at completion.

#### O10: Reference search has no per-reference timing
- **Difficulty:** medium
- **Location:** `src/reference.rs:72-93`
- **Description:** `search_reference()` searches a single reference index and applies weight. When multi-index search is slow, there's no way to tell which reference is the bottleneck — only `load_references` logs the count. Individual reference searches complete silently.
- **Suggested fix:** Add `debug_span!("search_reference", name = %ref_idx.name, weight = ref_idx.weight)` and log result count at debug level.

#### O11: Embedding cache hit/miss ratio not observable
- **Difficulty:** medium
- **Location:** `src/embedder.rs:386-413`
- **Description:** The query embedding LRU cache in `embed_query()` has no visibility into hit/miss ratios. The cache is a critical performance optimization (avoiding 100ms+ re-computation), but operators can't tell if it's effective. The MCP server can make dozens of repeated queries per session.
- **Suggested fix:** Add `tracing::debug!("query cache hit")` on line 392 and `tracing::debug!("query cache miss")` before the compute path.

#### O12: Dead code detection has no logging
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:352-434`
- **Description:** `find_dead_code()` performs a complex multi-step analysis (SQL query, test exclusion, trait impl detection, pub analysis) but has zero logging. When users report false positives/negatives, there's no trail of how many candidates were found, how many were excluded per category (tests, traits, no_mangle), or how many ended up in each output bucket.
- **Suggested fix:** Add `tracing::debug!` for total uncalled count, excluded counts per category, and final confident/possibly_dead counts.

#### O13: prune_stale_calls has no logging
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:469-478`
- **Description:** `prune_stale_calls()` deletes orphaned call graph entries but only returns the count — no logging when rows are actually deleted. The caller in `cmd_gc` prints the count, but the MCP/library path has no visibility.
- **Suggested fix:** Add `tracing::info!(pruned = result.rows_affected(), "Pruned stale call graph entries")` when count > 0.

#### O14: Note replace_notes_for_file has no completion log
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:185-224`
- **Description:** `replace_notes_for_file()` logs at the start ("replacing notes for file") at debug level but has no completion log. If the transaction commits successfully after deleting old + inserting new notes, there's no confirmation. The caller has to infer success from no error.
- **Suggested fix:** Add `tracing::info!(source = %source_str, count = notes.len(), "Notes replaced successfully")` after commit.

#### O15: eprintln in signal handler instead of tracing
- **Difficulty:** easy
- **Location:** `src/cli/signal.rs:31,33`
- **Description:** The Ctrl+C handler uses `eprintln!("\nInterrupted...")` and `eprintln!("Warning: Failed to set...")`. The interrupt notification is fine for CLI UX, but the handler setup failure should use tracing since it indicates a real problem.
- **Suggested fix:** Replace the setup failure `eprintln!` at line 33 with `tracing::warn!`.

#### O16: reference commands use eprintln instead of tracing
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:314,320`
- **Description:** Two `eprintln!` calls in reference CLI commands for warning messages. These bypass structured logging.
- **Suggested fix:** Replace with `tracing::warn!`.

## Documentation

#### D1: ROADMAP lists Markdown support as "Parked" but it shipped in v0.9.6
- **Difficulty:** easy
- **Location:** `ROADMAP.md:360`
- **Description:** The "Parked" section at the bottom of ROADMAP.md says "Markdown support" is parked, but Markdown was fully implemented and shipped in v0.9.6 (CHANGELOG entry, `lang-markdown` feature flag, `src/language/markdown.rs`, `src/parser/markdown.rs`). The parked entry also incorrectly says it would use `tree-sitter-markdown` — the actual implementation is a custom parser with no external dependency (`lang-markdown = []  # No external deps — custom parser` in Cargo.toml).
- **Suggested fix:** Remove the Markdown entry from the "Parked" section. It's already documented in the CHANGELOG under v0.9.6 and listed in ROADMAP's "New Languages > Done" section as the SQL entry mentions "8 languages total" (implying only SQL was new there, but the count includes Markdown at 9).

#### D2: CONTRIBUTING.md architecture tree missing cli/commands/audit_mode.rs and cli/commands/read.rs
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:89`
- **Description:** The architecture tree lists CLI command files but omits `audit_mode.rs` and `read.rs` from `src/cli/commands/`. Both files exist and are imported in `src/cli/mod.rs:17-21`. These were added in v0.9.7 (CLI-first migration) but the architecture tree was not updated.
- **Suggested fix:** Add `audit_mode.rs, read.rs` to the commands file list on line 89.

#### D3: CONTRIBUTING.md architecture tree missing src/audit.rs
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:124-143`
- **Description:** The top-level `src/` file listing omits `audit.rs`, which is declared as `pub mod audit` in `lib.rs` and exists on disk. It handles the file-based audit mode persistence (`.cqs/audit-mode.json`).
- **Suggested fix:** Add `audit.rs    - Audit mode persistence (file-based, shared CLI/MCP)` to the top-level source file listing.

#### D4: README "Call Graph" section includes misplaced `cqs notes list`
- **Difficulty:** easy
- **Location:** `README.md:141`
- **Description:** The "Call Graph" section (lines 136-149) lists three commands: `cqs callers`, `cqs callees`, and `cqs notes list`. The notes command has nothing to do with call graphs — it manages project notes with sentiment. It appears to have been added to the wrong section.
- **Suggested fix:** Move `cqs notes list` to a "Notes" section or the existing "Maintenance" section. Or add a separate "Notes" subsection between "Call Graph" and "Discovery Tools".

#### D5: README `--sources` documented as CLI flag but it's MCP-only
- **Difficulty:** easy
- **Location:** `README.md:231-234`
- **Description:** The "Reference Indexes" section documents `--sources` with CLI-style flag syntax (`--sources project`, `--sources tokio`), but `--sources` is not a CLI flag — it's an MCP tool parameter for `cqs_search`. The `src/cli/mod.rs` `Cli` struct has no `sources` field. Users who try `cqs "query" --sources project` on the command line will get an unrecognized flag error.
- **Suggested fix:** Clarify that source filtering is available via the MCP `cqs_search` tool's `sources` parameter. Either add a note "(MCP only)" or document it under the Claude Code Integration section instead.

#### D6: lib.rs Quick Start imports unused `ModelInfo`
- **Difficulty:** easy
- **Location:** `src/lib.rs:18`
- **Description:** The Quick Start example in the crate-level doc imports `use cqs::store::ModelInfo;` but never uses it in the example code. This produces a dead-code warning if anyone tries to compile the example.
- **Suggested fix:** Remove the unused `use cqs::store::ModelInfo;` import from the Quick Start example.

#### D7: ROADMAP "New Languages" section says "8 languages total" but Markdown makes it 9
- **Difficulty:** easy
- **Location:** `ROADMAP.md:329`
- **Description:** Under "New Languages > Done", the SQL entry says "8 languages total." But with Markdown support shipped in v0.9.6, the actual count is 9 (Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, Markdown). The rest of the codebase (lib.rs, README, CONTRIBUTING.md) correctly says 9 languages.
- **Suggested fix:** Update to "9 languages total" or remove the total count since it's tracked elsewhere.

## API Design

#### A1: ChunkIdentity and DiffEntry use String where enums exist

- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:201`, `src/diff.rs:12`
- **Description:** `ChunkIdentity` has `chunk_type: String` and `language: String` fields, while the sibling type `ChunkSummary` uses the proper `ChunkType` and `Language` enums for the same data. Same issue in `DiffEntry` which has `chunk_type: String` and `file: String` instead of `ChunkType` and `PathBuf`. Using strings loses type safety and forces callers to do string comparisons instead of pattern matching.
- **Suggested fix:** Change `ChunkIdentity.chunk_type` to `ChunkType`, `ChunkIdentity.language` to `Language`, `DiffEntry.chunk_type` to `ChunkType`, and `DiffEntry.file` to `PathBuf`. Update construction sites and any consumers.

#### A2: Asymmetric callers/callees return types

- **Difficulty:** medium
- **Location:** `src/store/calls.rs:98`, `src/store/calls.rs:138`, `src/store/calls.rs:257`
- **Description:** `get_callers()` returns `Vec<ChunkSummary>` (rich type with all chunk metadata) while `get_callees()` returns `Vec<String>` (just names). `get_callees_full()` returns `Vec<(String, u32)>` — an unnamed tuple where the u32 is a line number. The asymmetry is confusing: callers get full context, callees get bare strings. The unnamed tuple forces positional access with no documentation at the call site.
- **Suggested fix:** Add `get_callees()` overload returning `Vec<ChunkSummary>` (matching `get_callers`). Replace the `(String, u32)` tuple in `get_callees_full` with a named struct like `CalleeInfo { name: String, line_number: u32 }` or reuse `CallerInfo` if fields align.

#### A3: Stats methods return unnamed tuples

- **Difficulty:** easy
- **Location:** `src/store/calls.rs:152`, `src/store/calls.rs:493`
- **Description:** `call_stats()` returns `(u64, u64)` and `function_call_stats()` returns `(u64, u64, u64)`. At the call site, the meaning of each position is invisible — callers must destructure with made-up names. This is fragile if field order changes and unreadable in context.
- **Suggested fix:** Define named structs, e.g. `CallStats { total_calls: u64, unique_callees: u64 }` and `FunctionCallStats { total_calls: u64, unique_callers: u64, unique_callees: u64 }`.

#### A4: Duplicated name scoring logic

- **Difficulty:** easy
- **Location:** `src/store/mod.rs:479-487`, `src/store/chunks.rs:592-600`
- **Description:** `search_by_name()` and `search_by_names_batch()` contain identical score-calculation blocks: exact match = 1.0, prefix = 0.9, contains = 0.7, FTS fallback = 0.5. The entire FTS query construction and ChunkRow-to-SearchResult mapping is also duplicated between the two methods (~30 lines each). If scoring heuristics change, both must be updated in lockstep.
- **Suggested fix:** Extract a `name_match_score(chunk_name: &str, query: &str) -> f32` helper. Consider having `search_by_name()` delegate to `search_by_names_batch(&[name])` to eliminate the query duplication entirely.

#### A5: Duplicate cosine_similarity implementations

- **Difficulty:** easy
- **Location:** `src/math.rs:13`, `src/diff.rs:62`
- **Description:** Two implementations of cosine similarity exist. `math.rs` has `pub fn cosine_similarity(a, b) -> Option<f32>` (returns None on zero-magnitude) while `diff.rs` has a private `fn cosine_similarity(a, b) -> f32` (returns 0.0 on zero-magnitude). Different return types, different zero-handling, same core algorithm.
- **Suggested fix:** Remove the private copy in `diff.rs` and use `crate::math::cosine_similarity` with `.unwrap_or(0.0)` at the call site.

#### A6: SearchFilter has mixed encapsulation

- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:279`
- **Description:** `SearchFilter` exposes all fields as `pub` (allowing struct literal construction) but also provides a `with_query()` builder method. This is half-builder, half-raw-struct. Nothing prevents constructing a filter with inconsistent state (e.g. `enable_rrf: true` but empty `query_text`). The API sends mixed signals about how to construct the type.
- **Suggested fix:** Pick one pattern. Since SearchFilter is widely constructed via struct literals in tests and MCP tools, keeping pub fields is pragmatic — remove `with_query()` to avoid the mixed signal, or document it as the "quick path for RRF with defaults."

#### A7: Embedding::new() skips dimension validation

- **Difficulty:** easy
- **Location:** `src/embedder.rs:87`
- **Description:** `Embedding::new(data)` accepts any `Vec<f32>` with no dimension check. `Embedding::try_new(data)` validates that `data.len() == EMBEDDING_DIM`. The unchecked constructor is the simpler, more discoverable name, making it the path of least resistance. A wrong-dimensioned embedding silently corrupts search results.
- **Suggested fix:** Make `new()` validate (panic or return Result) and keep `try_new()` for the Result variant. Or rename `new()` to `new_unchecked()` to signal the risk, and make `try_new()` the primary constructor.

#### A8: serve_stdio and serve_http have inconsistent path parameter types

- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:61`, `src/lib.rs:48`
- **Description:** `serve_http` takes `project_root: impl AsRef<Path>` while `serve_stdio` takes `PathBuf`. The lib.rs doc comment shows `serve_http(".", ...)` with a string literal. The inconsistency between the two transport entry points is a minor ergonomic issue.
- **Suggested fix:** Unify both to `impl AsRef<Path>` for maximum flexibility and consistency.

#### A9: Note vs NoteEntry vs NoteSummary naming overload

- **Difficulty:** medium
- **Location:** `src/note.rs`, `src/store/helpers.rs`
- **Description:** Three note representations exist: `Note` (parsed from TOML, has `embedding_text()` and `sentiment()`), `NoteEntry` (display format with `id`, `text`, `sentiment`, `mentions`, `created_at`), and `NoteSummary` (from store, has `text`, `path`, `sentiment`). The naming doesn't convey the layer: `Note` is the parse model, `NoteEntry` is the API response model, `NoteSummary` is the search result model. A developer encountering these for the first time can't tell which to use where.
- **Suggested fix:** Consider renaming to make the layer explicit: `ParsedNote` / `NoteResponse` / `NoteSearchHit`, or at minimum add doc comments on each type stating when to use it.

#### A10: GatherOptions lacks builder methods

- **Difficulty:** easy
- **Location:** `src/gather.rs:19`
- **Description:** `GatherOptions` has a `Default` impl but no builder methods. Callers must construct the entire struct with `..Default::default()` to change one field. The struct is always constructed this way in practice.
- **Suggested fix:** Add chainable builder methods: `GatherOptions::default().with_limit(20).with_direction(GatherDirection::Callers)`. Low priority since the struct only has 3 fields.

## Code Quality

### CQ-1: resolve.rs duplicated identically between CLI and MCP
- **Difficulty:** Easy
- **Location:** `src/cli/commands/resolve.rs` (52 lines) and `src/mcp/tools/resolve.rs` (52 lines)
- **Description:** Both files contain identical implementations of `parse_target()` and `resolve_target()` — same logic, same signatures, same doc comments. The only difference is import paths (`cqs::store::` vs `crate::store::`). This is a copy-paste duplicate that will drift over time.
- **Suggested fix:** Move `parse_target()` and `resolve_target()` into the library crate (e.g., `src/search.rs` or a new `src/resolve.rs`) and have both CLI and MCP import from there.

### CQ-2: Focused-read logic duplicated between CLI and MCP (with TYPE_NAME_RE and COMMON_TYPES)
- **Difficulty:** Medium
- **Location:** `src/cli/commands/read.rs:139-207` and `src/mcp/tools/read.rs:147-215`
- **Description:** Both files define identical `TYPE_NAME_RE` regex, `COMMON_TYPES` set (40 entries), and `extract_type_names()` function. The `cmd_read_focused` (CLI) and `tool_read_focused` (MCP) functions share the same algorithm — resolve target, inject notes, append type dependencies. Each has its own copy.
- **Suggested fix:** Extract `extract_type_names()`, `TYPE_NAME_RE`, and `COMMON_TYPES` into the library crate. Consider a shared `focused_read()` function that both CLI and MCP call.

### CQ-3: Note injection logic duplicated in four places
- **Difficulty:** Medium
- **Location:** `src/cli/commands/read.rs:73-118` (CLI full read), `src/cli/commands/read.rs:242-271` (CLI focused read), `src/mcp/tools/read.rs:78-129` (MCP full read), `src/mcp/tools/read.rs:247-276` (MCP focused read)
- **Description:** The pattern of "load notes.toml, filter by mentions matching file path, classify sentiment as WARNING/PATTERN/NOTE, format as comment header" is repeated four times across two files. The sentiment thresholds (`-0.3` / `0.3`) are hard-coded at each site rather than using the `SENTIMENT_NEGATIVE_THRESHOLD` / `SENTIMENT_POSITIVE_THRESHOLD` constants already defined in `note.rs`.
- **Suggested fix:** Extract a `build_note_context_header(path, notes) -> String` function into the library crate. Use the constants from `note.rs` for thresholds.

### CQ-4: Impact command logic duplicated between CLI and MCP
- **Difficulty:** Medium
- **Location:** `src/cli/commands/impact.rs` (377 lines) and `src/mcp/tools/impact.rs` (256 lines)
- **Description:** Both implement the same algorithm: resolve target, get callers with context + snippets, BFS ancestors for test discovery, transitive callers for depth>1, mermaid output. The core logic (BFS, snippet extraction, test intersection) is duplicated. The `node_letter()` and `mermaid_escape()` helper functions are also copied verbatim between the two files.
- **Suggested fix:** Extract the shared core (BFS traversal, snippet extraction, test discovery) into a library function. Keep formatting in CLI/MCP. Move `node_letter()` and `mermaid_escape()` to a shared utility.

### CQ-5: JSON result formatting duplicated between CLI display and MCP search
- **Difficulty:** Medium
- **Location:** `src/cli/display.rs:174-210`, `src/cli/display.rs:363-404`, `src/mcp/tools/search.rs:330-356`, `src/mcp/tools/similar.rs:116-133`
- **Description:** The pattern of serializing a `SearchResult` or `UnifiedResult` into a JSON object with keys `file`, `line_start`, `line_end`, `name`, `signature`, `language`, `chunk_type`, `score`, `content` is repeated in at least 5 locations. Each has slight variations (some add `source`, some include `id` for notes, some apply `strip_prefix`). The core mapping is the same but inconsistently applied — `display_unified_results_json` includes `note.id` while `format_unified_result_json` does not.
- **Suggested fix:** Add a `to_json()` method on `SearchResult` and `NoteSearchResult` (or `UnifiedResult`) in the library crate. Callers pass optional root for path stripping.

### CQ-6: Duplicate cosine_similarity in diff.rs (see also A5)
- **Difficulty:** Easy
- **Location:** `src/diff.rs:62-85`
- **Description:** `diff.rs` defines its own `cosine_similarity()` with manual dot product and norm computation. The shared `math::cosine_similarity()` in `src/math.rs:13-30` uses SIMD-accelerated `simsimd`. The diff version returns `f32` (default 0.0) while `math.rs` returns `Option<f32>`. Also flagged as A5 from the API perspective.
- **Suggested fix:** Remove the private copy in `diff.rs` and use `crate::math::cosine_similarity` with `.unwrap_or(0.0)` at the call site. If cross-store normalization differs, add `cosine_similarity_full()` to `math.rs`.

### CQ-7: Duplicated name scoring logic in store (see also A4)
- **Difficulty:** Easy
- **Location:** `src/store/chunks.rs` (`search_by_names_batch`) and `src/store/mod.rs` (`search_by_name`)
- **Description:** Both functions implement the same exact/prefix/contains/FTS scoring tiers (1.0 / 0.85 / 0.7 / 0.5). Also flagged as A4 from the API perspective.
- **Suggested fix:** Extract a `score_name_match(name: &str, query: &str) -> f32` function into `store/helpers.rs`.

### CQ-8: Per-call Regex compilation in markdown parser
- **Difficulty:** Easy
- **Location:** `src/parser/markdown.rs` (inside `extract_references_from_text`)
- **Description:** Two `Regex::new()` calls compile patterns on every invocation of `extract_references_from_text()`. This function is called per-chunk during parsing and per-file during call extraction. Other modules (`nl.rs`, `mcp/server.rs`, `cli/commands/read.rs`) all use `LazyLock<Regex>` for regex compilation.
- **Suggested fix:** Move both regexes to `static LazyLock<Regex>` at module level, matching the project convention.

### CQ-9: Duplicated tokenization implementations in nl.rs
- **Difficulty:** Easy
- **Location:** `src/nl.rs` — `tokenize_identifier()` function and `TokenizeIdentifierIter` struct
- **Description:** The function `tokenize_identifier()` and the `TokenizeIdentifierIter` iterator implement the same camelCase/snake_case splitting algorithm independently. The function collects into a `Vec<String>`, the iterator yields items lazily. Two implementations of the same algorithm risk divergence.
- **Suggested fix:** Implement only the iterator version and have `tokenize_identifier()` call `.collect()` on it, or vice versa.

### CQ-10: make_embedding test helper duplicated across HNSW modules
- **Difficulty:** Easy
- **Location:** `src/hnsw/build.rs:205-217` and `src/hnsw/persist.rs:406-418`
- **Description:** Identical `make_embedding(seed: u32) -> Embedding` test helper function (sin-based deterministic embedding generation with normalization) is defined in both HNSW test modules.
- **Suggested fix:** Move to a shared `#[cfg(test)]` utility in `src/hnsw/mod.rs` and import from both test modules.

## Error Handling

#### EH1: Config::load() uses eprintln instead of tracing for errors
- **Difficulty:** easy
- **Location:** `src/config.rs:74,83`
- **Description:** `Config::load()` uses `eprintln!("Warning: {}", e)` for both config parse and read errors. The rest of the config module correctly uses `tracing::warn!`. These errors are invisible to MCP server logs. Overlaps with O3 (observability perspective); this finding focuses on the error-handling aspect — the errors are swallowed (returning `Config::default()`) with no structured trace.
- **Suggested fix:** Replace both `eprintln!` calls with `tracing::warn!(error = %e, "...")`. The `Config::default()` fallback is fine, but the warning should be structured.

#### EH2: CLI notes commands swallow index warnings via eprintln
- **Difficulty:** easy
- **Location:** `src/cli/commands/notes.rs:221,317,380`
- **Description:** After note add/update/remove, the code catches index-refresh errors with `eprintln!("Warning: {}", err)` and continues. The note mutation succeeds but the index may be stale. Overlaps with O5; from error-handling perspective, these errors are caught but routed to unstructured stderr instead of the tracing pipeline.
- **Suggested fix:** Replace with `tracing::warn!(error = %err, "Failed to refresh index after note mutation")`.

#### EH3: ref update prune guard warnings use eprintln
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:314-326`
- **Description:** `cmd_ref_update()` has two `eprintln!` warning paths for pruning anomalies (all chunks pruned, >50% pruned). These are important diagnostic signals that get lost in MCP context. Overlaps with O16; from error-handling perspective, the warnings about data loss (entire index emptied) should be structured and capturable.
- **Suggested fix:** Replace with `tracing::warn!` calls. Keep the `eprintln!` for CLI UX if desired, but also emit structured logs.

#### EH4: gather() silently falls back to empty on batch search failure
- **Difficulty:** medium
- **Location:** `src/gather.rs:140-146`
- **Description:** When `search_by_names_batch()` fails, `gather()` logs a warning but falls back to an empty `HashMap`. This means the entire BFS expansion phase produces zero results — the gather returns only seed results with no expansion. The caller has no indication that expansion was skipped. For a function whose purpose is "search + expand," silently returning un-expanded results is misleading.
- **Suggested fix:** Either propagate the error (let the caller decide), or add a flag/field to the result indicating expansion was degraded. At minimum, log at `warn!` level with the number of names that were requested but not resolved.

#### EH5: impact/test_map MCP tools swallow search_by_name errors with .ok()
- **Difficulty:** medium
- **Location:** `src/mcp/tools/impact.rs:46,144`
- **Description:** Both `cqs_impact` and `cqs_test_map` MCP tools use `store.search_by_name(&function).ok().and_then(|r| r)` to look up the target function. If the database query fails (corruption, locked, schema mismatch), `.ok()` converts the error to `None`, and the tool returns "Function not found" — indistinguishable from a genuinely missing function. The user gets a misleading "not found" instead of a real error.
- **Suggested fix:** Propagate the error with `?` and let the MCP error handler return it. Or match on the error and return a distinct message like "Database error looking up function."

#### EH6: CLI impact/test_map swallow search_by_name errors with .ok()
- **Difficulty:** medium
- **Location:** `src/cli/commands/impact.rs:50,144`
- **Description:** Same pattern as EH5 but in CLI commands. `store.search_by_name(&function).ok().and_then(|r| r)` — database errors become "Function not found" messages.
- **Suggested fix:** Use `?` to propagate database errors. Keep `None` handling for genuinely missing functions.

#### EH7: check_model_version silently ignores dimension parse failure
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:340-348`
- **Description:** `check_model_version()` parses the stored dimension string with `dim_str.parse::<u32>()`. If parsing fails (corrupted metadata), the `if let Ok(...)` silently skips validation. The function returns `Ok(())` as if everything is fine, then later operations may fail with confusing dimension mismatch errors.
- **Suggested fix:** Add an `else` branch that logs a warning: `tracing::warn!(dim = %dim_str, "Failed to parse stored dimension")`.

#### EH8: resolve_index_dir migration uses eprintln
- **Difficulty:** easy
- **Location:** `src/lib.rs:124-125`
- **Description:** The `.cq` to `.cqs` directory migration uses `eprintln!` for both the rename error (line 124, correctly returns `Err`) and the success notification (line 125). The success path uses `eprintln!` instead of `tracing::info!`. Overlaps with O4; from error-handling view, the rename failure path correctly propagates, but the success notification bypasses structured logging.
- **Suggested fix:** Replace `eprintln!("Migrated...")` with `tracing::info!("Migrated index directory from .cq/ to .cqs/")`.

#### EH9: MCP batch tool serializes errors but never logs them
- **Difficulty:** easy
- **Location:** `src/mcp/tools/batch.rs:53`
- **Description:** The batch tool catches per-tool errors and serializes them into the JSON response: `Err(e) => json!({"tool": tool, "error": e.to_string()})`. The error is returned to the caller but never logged. If 3 of 5 batch operations fail, there's no server-side record. The MCP client sees the errors, but operators reviewing logs see nothing.
- **Suggested fix:** Add `tracing::warn!(tool = %tool, error = %e, "Batch tool execution failed")` before the JSON serialization.

#### EH10: set_permissions errors silently discarded at 7 locations
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:177-182`, `src/config.rs:281,323`, `src/hnsw/persist.rs:206`, `src/cli/commands/init.rs:29`, `src/cli/commands/reference.rs:90`
- **Description:** Seven `let _ = std::fs::set_permissions(...)` calls silently discard errors. On non-Unix platforms or restricted filesystems, permission setting will fail. While the operation is best-effort (the file/dir was already created), a debug-level log would help diagnose "why is my index world-readable?" questions.
- **Suggested fix:** Replace `let _ =` with `if let Err(e) = ... { tracing::debug!(error = %e, "Failed to set permissions"); }` at each location.

#### EH11: resolve_target silently returns wrong-file result on filter miss
- **Difficulty:** medium
- **Location:** `src/mcp/tools/resolve.rs:39-48`
- **Description:** `resolve_target()` searches for a function, then tries to filter results to a specific file. If the file filter doesn't match any result, `matched` is `None` and `unwrap_or(0)` silently returns the first result — which is from a different file than requested. The caller asked "find X in file Y" and got "X in file Z" with no indication of the mismatch.
- **Suggested fix:** When `matched` is `None` and a `file` filter was provided, either return an error or log a warning. Consider returning the unfiltered result with a note that the file filter didn't match.

#### EH12: StoreError::Runtime is a catch-all string variant
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:31`
- **Description:** `StoreError::Runtime(String)` is used as a catch-all for various database and I/O errors that don't fit other variants. It carries only a string message with no structured error info (no source error chain, no error code). Callers can't match on specific runtime failure modes. Currently used for: connection failures, query errors, schema mismatches, and file I/O errors — all lumped together.
- **Suggested fix:** Consider splitting into `StoreError::Connection`, `StoreError::Schema`, `StoreError::Io` variants for the most common cases. At minimum, add `#[from]` conversions for `sqlx::Error` and `std::io::Error` so the source chain is preserved.

#### EH13: notes text truncation uses byte indexing (potential panic)
- **Difficulty:** medium
- **Location:** `src/cli/commands/notes.rs:449-452`
- **Description:** The `cmd_notes_list` function truncates long note text with `&note.text[..117]` (byte indexing). If the note contains multi-byte UTF-8 characters and byte 117 falls mid-character, this panics. The same file already has a safe `text_preview()` helper at line 119 that uses `.char_indices().nth(100)` for safe truncation.
- **Suggested fix:** Replace `&note.text[..117]` with a call to the existing `text_preview()` helper, or use the same `.char_indices()` pattern.

#### EH14: cosine_similarity returns 0.0 for mismatched dimensions with no warning
- **Difficulty:** easy
- **Location:** `src/diff.rs:63`
- **Description:** The local `cosine_similarity()` function in `diff.rs` returns `0.0` when vectors have different lengths. This is a silent data corruption signal — if embeddings have wrong dimensions, all similarity scores collapse to zero and the diff reports everything as "completely different." No warning, no error, no log.
- **Suggested fix:** Add `tracing::warn!("cosine_similarity: dimension mismatch ({} vs {})", a.len(), b.len())` before returning 0.0. Or return `Option<f32>` so the caller can handle it.

#### EH15: watch mode canonicalize silently falls back to original path
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:78-79,93`
- **Description:** `dunce::canonicalize(path).unwrap_or(path.to_path_buf())` silently uses the original path if canonicalization fails. If the path doesn't exist or has permission issues, the watcher proceeds with a potentially wrong path. This can cause the watcher to miss file changes (watching wrong directory) or index files with incorrect paths.
- **Suggested fix:** Log a debug-level warning when canonicalization fails: `tracing::debug!(path = %path.display(), "canonicalize failed, using original")`.

#### EH16: tmp file cleanup gap on notes serialization failure
- **Difficulty:** easy
- **Location:** `src/note.rs:182-184`
- **Description:** In `save_notes()`, a temp file is created (line 175), then notes are serialized (line 182). If `toml::to_string_pretty()` fails, the function returns `Err` but the temp file is left on disk. The temp file was created with a predictable name pattern in the same directory as the notes file.
- **Suggested fix:** Add cleanup in the error path: `let serialized = toml::to_string_pretty(...).map_err(|e| { let _ = std::fs::remove_file(&tmp_path); e })?;`

#### EH17: signal handler setup failure uses eprintln
- **Difficulty:** easy
- **Location:** `src/cli/signal.rs:33`
- **Description:** `eprintln!("Warning: Failed to set Ctrl+C handler: {}", e)` for signal handler setup failure. This is a real operational problem (the process won't handle SIGINT gracefully) but the warning bypasses structured logging. Overlaps with O15; from error-handling perspective, this should be a structured warning since it affects process behavior.
- **Suggested fix:** Replace with `tracing::warn!(error = %e, "Failed to set Ctrl+C handler")`.

## Extensibility

#### X1: cqs_batch tool only supports 6 of 20 MCP tools
- **Difficulty:** easy
- **Location:** `src/mcp/tools/batch.rs:27-37`
- **Description:** The batch tool's match arm supports only `search`, `callers`, `callees`, `explain`, `similar`, `stats` — 6 of the 20 available MCP tools. Missing: `read`, `dead`, `gc`, `audit_mode`, `diff`, `impact`, `trace`, `test_map`, `gather`, `context`, `add_note`, `update_note`, `remove_note`. The valid tool list in the error message at line 35 and the `cqs_search` schema `enum` at `mod.rs:389` must be updated manually each time a tool is added. This was noted in v0.9.1 triage as "works, trait system is overengineering" but 14 tools are now missing — the gap has grown from 8/14 to 6/20.
- **Suggested fix:** Either expand the match arm to cover all tools (forward to their `tool_*` handlers with the same pattern), or add a registry approach where each tool module registers itself. At minimum, add `gather`, `impact`, `trace`, `test_map`, `context`, and `dead` since these are pure query tools with no side effects.

#### X2: Structural Pattern enum requires 3 code changes + MCP schema update to add a pattern
- **Difficulty:** medium
- **Location:** `src/structural.rs:10-17`, `src/structural.rs:22-33`, `src/mcp/tools/mod.rs:108`
- **Description:** Adding a new structural pattern (e.g., `singleton`, `callback`, `retry`) requires: (1) add variant to `Pattern` enum, (2) add `FromStr` match arm, (3) add `Display` match arm, (4) add `matches()` match arm, (5) write the detector function, (6) update the hardcoded `"enum"` array in the MCP tool schema at `mod.rs:108`. The enum lacks a `valid_names()` method like `Language` has, so the MCP schema hardcodes `["builder", "error_swallow", "async", "mutex", "unsafe", "recursion"]` separately from the Rust enum.
- **Suggested fix:** Apply the same macro pattern used for `Language` — `define_patterns!` macro that generates the enum, `FromStr`, `Display`, and `valid_names()`. Use `Pattern::valid_names()` in the MCP schema instead of a hardcoded JSON array. This would reduce adding a pattern to: (1) one line in the macro, (2) write the detector function.

#### X3: Config file does not support note_weight or note_only defaults
- **Difficulty:** easy
- **Location:** `src/config.rs:50-63`
- **Description:** `Config` struct has `limit`, `threshold`, `name_boost`, `quiet`, `verbose`, and `references` — but not `note_weight` or `note_only`. These are available as CLI flags (`--note-weight`, `--note-only`) and MCP parameters, but cannot be set as project defaults in `.cqs.toml`. Users who always want notes suppressed (e.g., `note_weight = 0.0`) must pass the flag on every invocation.
- **Suggested fix:** Add `note_weight: Option<f32>` and `note_only: Option<bool>` to `Config`. Apply them in `apply_config_defaults()` the same way other fields are handled. Clamp `note_weight` to `[0.0, 1.0]` in `Config::load()`.

#### X4: Dead code test-file detection patterns are hardcoded in SQL
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:443-454`
- **Description:** `find_test_chunks_async()` uses hardcoded SQL LIKE patterns to detect test files: `%/tests/%`, `%_test.%`, `%.test.%`, `%.spec.%`, `%_test.go`, `%_test.py`. These are language-specific conventions baked into a single SQL query. Adding a new language (e.g., C# with `.Tests.` namespace convention, or Kotlin with `Test` suffix) requires editing raw SQL. The same patterns are partially duplicated in `find_dead_code()` at line 399-404 (Rust code, not SQL). The language definition (`LanguageDef`) has no field for test file patterns.
- **Suggested fix:** Add a `test_patterns: &'static [&'static str]` field to `LanguageDef` for per-language test file globs. Build the SQL dynamically from the registry. This keeps test detection extensible through the same macro as everything else.

#### X5: gather() hardcodes seed search parameters (5 results, 0.3 threshold)
- **Difficulty:** easy
- **Location:** `src/gather.rs:91`
- **Description:** `gather()` always searches for 5 seed results with 0.3 threshold (`store.search_filtered(query_embedding, &filter, 5, 0.3)`). These values are not part of `GatherOptions` and cannot be tuned by callers. For narrow queries, 5 seeds may be too many (wasting expansion budget); for broad queries, 5 seeds may miss relevant entry points. The MCP `cqs_gather` tool and CLI `gather` command have no way to control seed count.
- **Suggested fix:** Add `seed_limit: usize` and `seed_threshold: f32` fields to `GatherOptions` with defaults of 5 and 0.3. Pass them through from CLI/MCP.

#### X6: BFS expansion decay factor (0.8) hardcoded in gather
- **Difficulty:** easy
- **Location:** `src/gather.rs:130`
- **Description:** The BFS score decay factor `0.8_f32.powi((depth + 1) as i32)` is hardcoded. This controls how quickly expanded nodes lose relevance as distance from seed increases. Users doing shallow exploration (depth=1) want higher decay, deep exploration (depth=5) might want lower decay. Not configurable.
- **Suggested fix:** Add `decay_factor: f32` to `GatherOptions` with default 0.8. Low priority since the current value works well for depth 1-2.

#### X7: doctor command has no extension points for custom checks
- **Difficulty:** medium
- **Location:** `src/cli/commands/doctor.rs:15-131`
- **Description:** `cmd_doctor()` runs a fixed sequence of checks: model, parser, index, references. Each check is inline code with hardcoded output. There's no way for plugins/extensions to add custom diagnostic checks (e.g., checking GPU availability, verifying HNSW integrity, checking disk space). The function is 130 lines of sequential println! calls with no structure.
- **Suggested fix:** Extract each check into a trait or function returning a `DiagnosticResult { name, status, detail }`. This would allow the doctor command to iterate over a list of checks and also make individual checks testable. Low priority — the current check set is comprehensive for the tool's scope.

#### X8: MCP tool schema definitions use inline JSON, not generated from types
- **Difficulty:** hard
- **Location:** `src/mcp/tools/mod.rs:41-482`
- **Description:** All 20 MCP tool schemas are handwritten `serde_json::json!()` blocks (~440 lines). The schemas are not derived from the actual Rust types used to deserialize arguments (e.g., `SearchArgs` in `types.rs`). This means: (1) adding a parameter requires editing both the schema JSON and the Rust struct, (2) defaults in the schema can drift from defaults in the code (e.g., schema says `"default": 0.3` but code uses `unwrap_or(0.3)` — same now but not guaranteed), (3) enum values in schemas (like `chunk_type`, `pattern`, `language`) must be manually kept in sync with Rust enums. The `language` enum is partially automated via `language_enum_schema()` but `chunk_type` and `pattern` are hardcoded.
- **Suggested fix:** Use `schemars` or a similar crate to derive JSON Schema from the Rust types. Or at minimum, add `valid_names()` methods to `ChunkType` and `Pattern` (matching `Language::valid_names()`) and use them in schema generation. This is a large change but prevents the most common class of drift bugs.

#### X9: apply_config_defaults uses magic numbers to detect "user didn't set this"
- **Difficulty:** medium
- **Location:** `src/cli/config.rs:88-112`
- **Description:** `apply_config_defaults()` detects whether CLI flags were explicitly set by comparing against clap default values: `cli.limit == 5`, `(cli.threshold - 0.3).abs() < f32::EPSILON`, `(cli.name_boost - 0.2).abs() < f32::EPSILON`. If the default value changes in the `Cli` struct, this function silently breaks — config overrides stop working for that field. Adding a new configurable field requires knowing and hardcoding its default in two places.
- **Suggested fix:** Use clap's `ArgMatches::value_source()` to detect whether a flag was explicitly set vs using its default. This eliminates the duplicated magic numbers and works correctly if defaults change. Example: `matches.value_source("limit") == Some(ValueSource::DefaultValue)`.

## Algorithm Correctness

#### AC1: Unified search note slots over-allocated when code results are sparse
- **Difficulty:** medium
- **Location:** `src/search.rs:600-629`
- **Description:** The slot allocation computes `reserved_code = code_count.min(min_code_slots)`. When code returns fewer results than the 60% minimum (e.g., limit=10, code returns 2), `reserved_code = 2` and `note_slots = 10 - 2 = 8`. But line 607-611 adds ALL code results (up to `limit`), and notes get up to `note_slots`. After sorting and truncating, notes can dominate (8 of 10 slots). The "60% minimum for code" guarantee inverts when code is scarce: the min-reservation becomes a max-reservation, and notes get up to 80% of results. For queries like "retry logic" in a small codebase with many notes, notes can drown out the 2 relevant code results.
- **Suggested fix:** Either: (1) document that the 60% is a minimum reservation not a minimum representation, or (2) cap `note_slots` at `limit - min_code_slots` regardless of actual code count, so notes never exceed 40% even when code is sparse.

#### AC2: gather() BFS produces non-deterministic results due to HashMap iteration
- **Difficulty:** easy
- **Location:** `src/gather.rs:150-177`
- **Description:** At line 151, `for (name, (score, depth)) in &name_scores` iterates a `HashMap` with non-deterministic order. When chunks have equal scores after decay (common for same-depth neighbors of the same seed), the order they appear in `chunks` varies between runs. After sort-by-score and truncation at `opts.limit` (lines 180-185), different chunks can be included/excluded across identical runs on the same data.
- **Suggested fix:** Add a tiebreaker to the sort: `.then(a.name.cmp(&b.name))` on line 182. This makes gather output deterministic for equal scores.

#### AC3: gather() BFS assigns suboptimal scores when multiple seeds connect to the same neighbor
- **Difficulty:** medium
- **Location:** `src/gather.rs:126-134`
- **Description:** BFS uses `name_scores.contains_key(&neighbor)` to skip already-visited nodes. If seed A (score 0.5) discovers neighbor B at depth 1 first, B gets score `0.5 * 0.8 = 0.4`. Later, seed C (score 0.9) also connects to B but B is skipped. B could have gotten `0.9 * 0.8 = 0.72` through C. The BFS finds B at the shortest depth (correct) but assigns the first-discovered score rather than the best available score. For hub functions connected to multiple seeds, this can significantly undervalue important nodes.
- **Suggested fix:** When a neighbor is already in `name_scores`, check if the new path offers a better score and update if so (without re-queuing for BFS).

#### AC4: diff's ChunkKey includes line_start, causing false add+remove for reordered functions
- **Difficulty:** medium
- **Location:** `src/diff.rs:42-57`
- **Description:** `ChunkKey` uses `(origin, name, chunk_type, line_start)` for matching chunks across stores. If a function is moved to a different line (e.g., reordering functions, adding code above), the source key `(foo.rs, bar, function, 10)` doesn't match target `(foo.rs, bar, function, 25)`. The diff reports `bar` as both "removed" and "added" instead of "unchanged" or "modified." For typical refactoring (reordering functions, inserting new functions that push existing ones down), this inflates added/removed counts and obscures actual semantic changes.
- **Suggested fix:** Use a two-pass matching: first exact key match (with line_start), then relaxed match (name + origin + chunk_type) for unmatched entries. This handles both Java overloads (same name, different lines) and function reordering.

#### AC5: Dead code detection's trait impl check operates on wrong content
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:407-411`
- **Description:** The trait impl exclusion uses `TRAIT_IMPL_RE.is_match(&chunk.content)` with regex `r"impl\s+\w+\s+for\s+"`. For method chunks, `content` is the method body text (e.g., `fn fmt(&self, f: &mut Formatter) -> Result { ... }`), NOT the surrounding `impl Display for MyType` block. The regex only matches if "impl X for" appears literally inside the method body (e.g., in a comment or nested impl). This means trait impl methods are NOT reliably excluded from dead code, producing false positives. Functions like `fmt`, `from`, `into`, `deref`, `index` will frequently appear as "dead" even though they're called via trait dispatch.
- **Suggested fix:** During parsing, tag methods inside `impl Trait for Type` blocks with a flag (e.g., `is_trait_impl: bool` on Chunk). Alternatively, check the parent chunk's content/signature for the trait impl pattern, since method chunks often have a `parent_id` pointing to the impl block chunk.

#### AC6: note_stats thresholds assume discrete sentiment, fragile if continuous values accepted
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:265-274`, `src/note.rs:17-18`
- **Description:** The SQL uses `sentiment < -0.3` and `sentiment > 0.3` for counting warnings/patterns. The note system defines only 5 discrete values (-1, -0.5, 0, 0.5, 1), so the thresholds at -0.3 and 0.3 correctly separate them. However, the `sentiment` column in SQLite is a plain REAL with no CHECK constraint — if a note is inserted with a continuous value like -0.3 exactly (from a buggy caller or manual DB edit), it would be classified as neutral despite being clearly negative. The `Note` struct uses `f32` with no validation on construction. The sentinel threshold values (-0.3, 0.3) only work correctly because all legitimate callers use the 5 discrete values.
- **Suggested fix:** Add a CHECK constraint on the `notes` table: `CHECK (sentiment IN (-1.0, -0.5, 0.0, 0.5, 1.0))`. Or validate sentiment values in `Note::new()` / `upsert_notes_batch`. This makes the discrete-only assumption explicit rather than relying on caller discipline.

#### AC7: search_across_projects does not use HNSW index
- **Difficulty:** medium
- **Location:** `src/project.rs:133-140`
- **Description:** `search_across_projects()` calls `store.search_filtered()` — brute-force O(n) cosine similarity — for each registered project. It never loads or uses the HNSW index even if one exists. For projects with 10k+ chunks, cross-project search is O(n) per project while single-project search uses O(log n) via `search_filtered_with_index`. This was partially fixed in PR #305 (added RRF instead of raw `store.search()`), but the HNSW optimization was not included.
- **Suggested fix:** Try loading the HNSW index with `HnswIndex::try_load` for each project and call `store.search_filtered_with_index()`.

#### AC8: BoundedScoreHeap drops equal-score newcomers, creating iteration-order bias
- **Difficulty:** easy
- **Location:** `src/search.rs:183-188`
- **Description:** When the heap is at capacity and a new item's score equals the minimum (`score > *min_score` is false for equal), the new item is silently dropped. Search results become biased toward chunks that appear earlier in the SQLite row iteration. While identical cosine similarity is rare for diverse embeddings, it can occur for very similar code (e.g., getter/setter methods, boilerplate patterns). The bias is invisible — users see deterministic-looking results that subtly depend on SQLite row ordering.
- **Suggested fix:** For search correctness, `>` is fine (stable tie resolution). But if determinism matters, add a secondary sort key (e.g., chunk ID) to the heap element so ties are resolved consistently regardless of iteration order.

#### AC9: diff cosine_similarity doesn't check dimension match against EMBEDDING_DIM
- **Difficulty:** easy
- **Location:** `src/diff.rs:62-85`
- **Description:** The private `cosine_similarity` in diff.rs checks `a.len() != b.len()` but does not validate against `EMBEDDING_DIM`. If both stores have embeddings with the same wrong dimension (e.g., both migrated from an older 768-dim model), the function computes similarity between wrong-dimension vectors and returns a result. The shared `math::cosine_similarity` validates against `EMBEDDING_DIM` and returns `None` for wrong dimensions. This means diff silently computes potentially-meaningless similarity scores for mismatched-dimension embeddings, while regular search would reject them.
- **Suggested fix:** Replace with `crate::math::cosine_similarity(a, b).unwrap_or(0.0)` as noted in A5/CQ-6. This adds the dimension check for free.

#### AC10: Chunk ID path extraction via rfind(':') is fragile for IDs with extra colons
- **Difficulty:** easy
- **Location:** `src/search.rs:314-319`
- **Description:** The glob filter extracts the file path from a chunk ID using two `rfind(':')` calls to strip `:hash_prefix` then `:line_start`. The ID format is `"path:line_start:hash_prefix"`. This works correctly for typical paths. However, windowed chunks have IDs like `"path:line_start:hash_prefix:wN"` (format from pipeline.rs:51: `format!("{}:w{}", parent_id, window_idx)`). For these, the first `rfind(':')` finds `:wN`, and the second finds `:hash_prefix`, giving the path as `"path:line_start"` instead of `"path"`. The glob matcher then tries to match `"src/foo.rs:10"` against patterns like `"src/**/*.rs"`, which will fail.
- **Suggested fix:** Parse the ID format more carefully. Since window IDs append `:wN` to the parent ID, and the parent ID is `"path:line_start:hash"`, window IDs have 4 colon-separated segments. Strip from the right: if the last segment starts with 'w' and is followed by digits, strip it first, then strip hash and line_start. Or store the origin path alongside the ID in the scoring phase to avoid re-extraction.


## Platform Behavior

#### PB1: Watch mode delete_by_origin uses native path separators, but pipeline stores forward slashes
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:261`, `src/store/chunks.rs:122`
- **Description:** In `reindex_files()`, `store.delete_by_origin(rel_path)` passes a `PathBuf` which `delete_by_origin` converts via `origin.to_string_lossy()`. On native Windows, this produces backslash paths (e.g., `src\main.rs`). However, the index pipeline (`src/cli/pipeline.rs:285`) normalizes origins to forward slashes when storing chunk IDs. And `FileSystemSource` (`src/source/filesystem.rs:132`) normalizes origins to forward slashes. So on native Windows, `delete_by_origin` in watch mode would look for `src\main.rs` but the stored origin is `src/main.rs` — the DELETE WHERE clause wouldn't match. Chunks would accumulate instead of being replaced. On WSL this is a non-issue because WSL uses forward slashes natively.
- **Suggested fix:** Normalize the origin in `delete_by_origin` to forward slashes: `let origin_str = origin.to_string_lossy().replace('\\', "/");`. Or centralize origin normalization into a `normalize_origin(path) -> String` helper used by both pipeline and watch.

#### PB2: upsert_chunks_batch stores chunk.file with native separators
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:40`
- **Description:** `upsert_chunks_batch()` stores the origin as `chunk.file.to_string_lossy().into_owned()` (line 40). The index pipeline (`cli/pipeline.rs:285`) normalizes the chunk ID to forward slashes but does NOT normalize `chunk.file` itself — it only normalizes the ID string. So on native Windows, `chunk.file` retains backslashes when passed to `upsert_chunks_batch`. But `FileSystemSource` (`src/source/filesystem.rs:132`) normalizes with `.replace('\\', "/")`. Depending on which code path creates the chunk, the stored origin format differs on Windows. This creates inconsistent data: some rows have `src\main.rs`, others have `src/main.rs`.
- **Suggested fix:** Normalize `chunk.file` to forward slashes at the point where file paths are set (after `chunk.file = rel_path.clone()` in pipeline.rs:281), or add normalization in `upsert_chunks_batch` itself.

#### PB3: HNSW save uses rename without cross-device fallback
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:211-225`
- **Description:** `save()` creates files in a temp subdirectory (`.{basename}.tmp/`) then renames each file to the final location. The temp dir is a subdirectory of the target, so same-device rename should always work. However, `note.rs` (`src/note.rs:194-210`) already handles the EXDEV case with a copy+remove fallback. Container environments with overlayfs or bind mounts can make same-directory renames fail unexpectedly. The error message would be "Failed to rename" with no recovery.
- **Suggested fix:** Add the same copy+remove fallback pattern used in `note.rs:196-210`. Low priority since temp dir is by construction in the same parent directory.

#### PB4: Inconsistent canonicalization strategies across codebase
- **Difficulty:** easy
- **Location:** `src/cli/commands/read.rs:34-44`, `src/mcp/tools/read.rs:36-43`, `src/cli/watch.rs:78-93`
- **Description:** Three different canonicalization patterns coexist: (1) Watch mode uses `dunce::canonicalize()` which strips UNC prefixes automatically. (2) CLI read uses `std::fs::canonicalize()` + manual `#[cfg(windows)] strip_unc_prefix()`. (3) MCP read uses `strip_unc_prefix(path.canonicalize()?)` unconditionally (strip_unc_prefix is a no-op on non-Windows). They all achieve the same result, but the inconsistency increases maintenance burden and makes it easy to forget UNC handling when adding new code paths. `dunce` is already a dependency.
- **Suggested fix:** Standardize on `dunce::canonicalize()` everywhere. Remove the manual `strip_unc_prefix` pattern from `cli/commands/read.rs` and `mcp/tools/read.rs`.

#### PB5: save_audit_state writes without restrictive permissions
- **Difficulty:** easy
- **Location:** `src/audit.rs:112`
- **Description:** `save_audit_state()` writes `audit-mode.json` via `std::fs::write()` with no explicit permission setting. Every other write in the `.cqs/` directory (`store/mod.rs`, `hnsw/persist.rs`, `config.rs`, `cli/commands/init.rs`, `cli/commands/notes.rs`, `mcp/tools/notes.rs`) sets `0o600` on Unix. The audit-mode file is the only exception. While it contains no secrets, it breaks the defense-in-depth pattern.
- **Suggested fix:** Add `#[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)); }` after the write.

#### PB6: ProjectRegistry.save() writes without restrictive permissions
- **Difficulty:** easy
- **Location:** `src/project.rs:44-46`
- **Description:** `save()` writes `~/.config/cqs/projects.toml` via `std::fs::write()` with no permission restriction. This file contains absolute project paths which reveal directory structure. The config file (`config.rs:278-282`) and notes file (`cli/commands/notes.rs:133-137`) both set 0o600. The registry file follows the same pattern but lacks the permission setting.
- **Suggested fix:** Add `#[cfg(unix)] { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)); }` after the write.

#### PB7: Config permission check's WSL detection heuristic is narrow
- **Difficulty:** easy
- **Location:** `src/config.rs:171`
- **Description:** The WSL mount detection uses `p.starts_with("/mnt/")` to skip the 0o077 permission warning on NTFS-backed files. This misses WSL2 with custom `automount.root` in `/etc/wsl.conf` (e.g., `/win/`). A user with a custom mount root would get spurious warnings on every config load because NTFS always reports 0o777.
- **Suggested fix:** Also check `/proc/version` for "microsoft" or "WSL" to detect the WSL environment. Or accept the heuristic as good-enough since custom mount roots are uncommon.

#### PB8: project.rs registry tests use Unix-only paths
- **Difficulty:** easy
- **Location:** `src/project.rs:198-266`
- **Description:** All test paths use Unix-style absolute paths (`/tmp/foo`, `/home/user/project`). On native Windows, `Path::new("/tmp/foo").is_absolute()` returns `false`. The `test_make_project_relative` test would silently produce wrong results because `strip_prefix` would fail on non-absolute Windows paths. Tests pass on WSL/Linux but would fail on native Windows.
- **Suggested fix:** Use `tempfile::TempDir` for paths that need to be platform-valid, or gate with `#[cfg(unix)]`. Low priority since the project targets WSL.

#### PB9: HNSW temp directory not cleaned up on partial rename failure
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:229`
- **Description:** After renaming files from the temp directory to the final location, cleanup uses `std::fs::remove_dir(&temp_dir)` (line 229). `remove_dir` only removes empty directories. If a rename fails partway through the loop (line 217), the function returns an error but the temp directory still contains un-renamed files. The directory is cleaned up on the next `save()` call (line 124-131), but between failures the stale files occupy disk space. On disk-full scenarios, this compounds the problem.
- **Suggested fix:** Use `std::fs::remove_dir_all(&temp_dir)` instead of `remove_dir` at line 229, or add error-path cleanup using a scope guard.

## Test Coverage

#### TC1: gather_test.rs has a tautological assertion (always true)
- **Difficulty:** easy
- **Location:** `tests/gather_test.rs:69-72`
- **Description:** `test_gather_basic` contains the assertion `assert!(!gather_result.chunks.is_empty() || gather_result.chunks.is_empty(), ...)` — this is `assert!(true)` regardless of the result. The comment says "Should return results (or empty if search found nothing)" which confirms it was never meant to validate anything. The test only checks that `gather()` doesn't panic/error, not that it returns correct results. Two more tests (`test_gather_callers_only`, `test_gather_callees_only`) only assert `result.is_ok()` without examining the returned chunks at all.
- **Suggested fix:** Replace the tautological assertion with meaningful checks: verify that `gather_result.chunks` contains at least the seed results (since we inserted chunks with the same embedding as the query), and check that expanded chunks have `depth > 0`.

#### TC2: 11 of 20 MCP tools have no dedicated integration tests
- **Difficulty:** medium
- **Location:** `tests/mcp_test.rs`
- **Description:** The MCP integration test file covers: `cqs_search`, `cqs_stats`, `cqs_read`, `cqs_add_note`, `cqs_callers`, `cqs_callees`, `cqs_audit_mode`. The remaining 13 tools have no dedicated test: `cqs_gather`, `cqs_dead`, `cqs_gc`, `cqs_diff`, `cqs_explain`, `cqs_similar`, `cqs_impact`, `cqs_trace`, `cqs_test_map`, `cqs_batch`, `cqs_context`, `cqs_update_note`, `cqs_remove_note`. Some of these are reached indirectly via `test_concurrent_requests` (which tests dispatching, not correctness), but none have tests that verify their output format, error handling for missing arguments, or edge cases.
- **Suggested fix:** Add at minimum "happy path + missing required arg" test pairs for each untested tool. Priority tools: `cqs_gather` (complex multi-step), `cqs_batch` (orchestration), `cqs_impact` (BFS traversal), `cqs_context` (multi-query aggregation).

#### TC3: 11 CLI commands have no integration tests
- **Difficulty:** medium
- **Location:** `tests/cli_test.rs`
- **Description:** `cli_test.rs` covers: `init`, `stats`, `index`, `search`, `completions`, `doctor`, `callers`, `callees`. Missing entirely: `gather`, `dead`, `gc`, `diff`, `explain`, `similar`, `impact`, `trace`, `test-map`, `context`, `read`, `audit-mode`, `ref`, `notes`, `project`, `watch`. These commands have zero CLI-level testing — no verification of exit codes, output format, JSON serialization, or error messages. Several commands (`ref add/remove/update`, `notes add/update/remove`, `project add/remove`) perform mutations and have complex error paths.
- **Suggested fix:** Add CLI integration tests for at least the query commands (`gather`, `dead`, `explain`, `similar`, `impact`, `trace`, `test-map`, `context`). These are side-effect-free and easy to test against a pre-indexed fixture. Mutation commands (`ref`, `notes`, `gc`) need setup/teardown but should also be tested.

#### TC4: search_filtered() and search_by_candidate_ids() have no direct unit tests
- **Difficulty:** medium
- **Location:** `src/search.rs:89`, `src/search.rs:144`
- **Description:** `search_filtered()` is the primary search entry point used by all user-facing search paths (CLI, MCP). It orchestrates HNSW/brute-force selection, glob filtering, language filtering, and RRF hybrid search. It has no direct unit tests — only indirect coverage through integration tests. `search_by_candidate_ids()` (the HNSW-accelerated path) has integration tests in `search_test.rs` but those test through `TestStore` which may not exercise all filter combinations. Neither function is tested for: empty query, glob-only search, language + glob combined, RRF disabled, threshold edge cases.
- **Suggested fix:** Add unit tests in `src/search.rs`'s `#[cfg(test)]` module for `search_filtered` covering: basic search, glob filtering, language filtering, RRF on/off, threshold=0.0 and threshold=1.0, empty store, name-only mode. Already flagged as T10/T11 in v0.9.1 triage (still TODO).

#### TC5: search_across_projects() has zero tests
- **Difficulty:** medium
- **Location:** `src/project.rs:115-173`
- **Description:** `search_across_projects()` is the cross-project search function that iterates registered projects, searches each one, and merges results. It has no tests at any level — no unit tests, no integration tests. The function handles project discovery, store opening, search execution, path relativization, and result merging. A regression here (e.g., the store.search() vs search_filtered() bug from PR #305) would go undetected.
- **Suggested fix:** Add integration tests using `TestStore` instances registered as projects. Test: basic cross-project search, project with missing index (should skip gracefully), result path prefixing, limit enforcement across projects.

#### TC6: embed_documents() has no tests
- **Difficulty:** hard
- **Location:** `src/embedder.rs:273-353`
- **Description:** `embed_documents()` is the batch embedding function that processes chunks through the ONNX model. It has no tests (requires a real model file, ~65MB). The function handles: E5 query prefix prepending, batch chunking at 256 tokens, ONNX session execution, dimension validation, and sentiment dimension appending. Any of these steps could silently produce wrong embeddings. The query cache in `embed_query()` is also untested directly.
- **Suggested fix:** Add a test fixture with a small mock model, or add integration tests that use `ensure_model()` to download the real model and verify that: (1) embeddings have correct dimensions (769), (2) semantically similar texts produce high cosine similarity, (3) batch size doesn't affect results. Mark as `#[ignore]` for CI if model download is too slow.

#### TC7: save_notes() / load_notes() in note.rs have no round-trip test
- **Difficulty:** easy
- **Location:** `src/note.rs:117-220`
- **Description:** `parse_notes()` (load) and `rewrite_notes_file()` (save/mutate) are tested individually, but there is no round-trip test that verifies: write notes → read notes → verify identical content. The existing tests verify that `rewrite_notes_file` can update and remove notes, and `parse_notes_str` can parse TOML, but no test verifies that the full file I/O path (with locking, temp file, rename) preserves all fields including mentions, sentiment, and text with special characters.
- **Suggested fix:** Add a test that: creates a temp dir, writes notes via `rewrite_notes_file`, reads back via `parse_notes`, and asserts all fields match. Include edge cases: empty mentions, unicode text, sentiment at boundary values (-1.0, 1.0).

#### TC8: store/chunks.rs (817 lines) has no inline tests
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs`
- **Description:** The largest store module (817 lines, 16 public methods) has no `#[cfg(test)]` module. All testing comes from integration tests in `tests/store_test.rs`. While integration tests cover the basic CRUD operations, they don't test internal edge cases: `upsert_chunks_batch` with empty batch, `prune_missing` with all files missing, `get_embeddings_by_ids` with duplicate IDs, `search_by_names_batch` with names that match FTS but not exact/prefix/contains tiers, `embedding_batches` with batch_size=0 or batch_size > total.
- **Suggested fix:** Add a `#[cfg(test)]` module in `chunks.rs` with edge-case tests for the most complex methods. Integration tests provide the "happy path" coverage; inline tests should focus on boundary conditions and error paths.

#### TC9: reference.rs load_references() and search_reference() have no direct tests
- **Difficulty:** medium
- **Location:** `src/reference.rs:42-93`
- **Description:** `load_references()` reads config, validates paths, opens stores, and loads HNSW indexes — complex initialization with multiple failure modes. `search_reference()` applies weight multipliers and delegates to store search. Both are only tested indirectly through `search_test.rs::test_search_reference_by_name`. There are no tests for: missing reference path (graceful skip), invalid weight in config (clamping), reference with no HNSW index (brute-force fallback), reference search with filters.
- **Suggested fix:** Add tests for `load_references` error paths (missing dir, invalid config) and `search_reference` with weight=0.0, weight=1.0, and empty reference store.

#### TC10: cmd_gc has zero tests at any level
- **Difficulty:** easy
- **Location:** `src/cli/commands/gc.rs:17-96`
- **Description:** `cmd_gc` performs file enumeration, chunk pruning, call graph pruning, and optional HNSW rebuild. It has no CLI integration tests and no unit tests. The MCP `tool_gc` is never directly invoked in tests either. A regression in GC (e.g., pruning too aggressively, failing to rebuild HNSW) would only be caught by manual testing.
- **Suggested fix:** Add a CLI integration test: index a fixture, delete a source file, run `cqs gc --json`, verify that stale chunks are pruned and counts match expected.

#### TC11: cmd_dead CLI command has no integration test
- **Difficulty:** easy
- **Location:** `src/cli/commands/dead.rs`
- **Description:** The `dead` command has unit-level tests via `tests/dead_code_test.rs` (which tests `find_dead_code()` directly), but no CLI integration test. The CLI command adds output formatting (JSON, text), entry point exclusion display, and the `--include-pub` flag behavior. None of these CLI-specific behaviors are tested.
- **Suggested fix:** Add CLI integration tests: `cqs dead --json` on an indexed fixture, verify JSON structure and that known uncalled functions appear.

#### TC12: diff integration tests don't verify the modified list
- **Difficulty:** easy
- **Location:** `tests/diff_test.rs:72-85`
- **Description:** `test_semantic_diff_basic` sets up a scenario where `func_b` has a different embedding in source vs target, which should produce a "modified" entry. But the test only asserts that `func_c` is removed, `func_d` is added, and `func_a` is not in any list. The `modified` list is never directly asserted — the test doesn't verify that `func_b` appears in `diff.modified`. The `test_semantic_diff_threshold` test similarly avoids asserting on `modified`, only checking that `added` and `removed` are empty.
- **Suggested fix:** Add `assert!(diff.modified.iter().any(|c| c.name == "func_b"), "func_b should be in modified list")` to `test_semantic_diff_basic`. In `test_semantic_diff_threshold`, assert that `modified` contains the function when threshold is high enough to detect the difference.

#### TC13: find_project_root() has no tests
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:370-420` (approximate)
- **Description:** `find_project_root()` walks up from the current directory looking for project root markers (`.git`, `Cargo.toml`, `package.json`, etc.). It has no tests. The function handles: multiple markers, preference order, symlinked directories, and the fallback to current directory. Incorrect root detection causes all subsequent operations to use the wrong `.cqs/` directory.
- **Suggested fix:** Add tests using `tempfile::TempDir` with nested directories and various root markers. Verify correct root is found from subdirectories.

#### TC14: 127 functions flagged as dead code, most have no tests
- **Difficulty:** medium
- **Location:** Various (output of `cqs dead`)
- **Description:** `cqs dead` reports 127 functions with no callers. While many are legitimate entry points (test fixtures, trait impls, CLI handlers), several are genuinely dead: `text_preview` (duplicated in CLI notes and MCP notes — one copy is unused), `panic_message` (src/cli/pipeline.rs:634), `default_ref_weight` (src/config.rs:26), `ensure_model` (src/embedder.rs:523), `detect_provider` (src/embedder.rs:685), `source_type` (src/source/filesystem.rs:92). Dead functions with no tests represent double waste — code that's never called AND never tested.
- **Suggested fix:** First, audit the 127 items to determine which are genuinely dead vs. entry points. Remove genuinely dead code. For functions that should be called but aren't (indicating incomplete wiring), wire them up and add tests. The `extract_return` functions across all 8 language modules are suspicious — if nothing calls them, they may be vestigial from a removed feature.

#### TC15: MCP error response assertions only check is_some(), not error content
- **Difficulty:** easy
- **Location:** `tests/mcp_test.rs:145,164,176,277,444,461,482`
- **Description:** Seven MCP tests assert `assert!(response.error.is_some())` without checking the error code or message. For example, `test_tools_call_unknown_tool` verifies that calling a nonexistent tool produces an error, but doesn't verify the error code is `-32601` (Method Not Found) or that the message mentions the tool name. If the error handling regressed to return a generic error, these tests would still pass.
- **Suggested fix:** Change to `assert_eq!(response.error.as_ref().unwrap()["code"], -32601)` or similar, checking both error code and that the message contains the expected substring.

## Robustness

#### R1: Regex::new().unwrap() in markdown reference extraction (non-test, non-LazyLock)
- **Difficulty:** easy
- **Location:** `src/parser/markdown.rs:538,557`
- **Description:** `extract_references_from_text()` compiles two regexes with `Regex::new(...).unwrap()` on every invocation. Unlike the rest of the codebase which uses `LazyLock<Regex>` for compile-once patterns, these are compiled per-call AND use `.unwrap()`. If the regex syntax were ever malformed (e.g., during a refactor), the unwrap would panic in production. The per-call compilation aspect is already noted in CQ-8, but the unwrap is the robustness concern.
- **Suggested fix:** Move both regexes to `static LazyLock<Regex>` at module level with `.expect("hardcoded regex")` (matching the convention in `mcp/server.rs:218,222`, `nl.rs:21,23`, `store/calls.rs:17`, `cli/commands/read.rs:140`). This eliminates both the per-call cost and the unwrap-in-loop risk.

#### R2: Language::def() panics if registry/enum desync
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:331`
- **Description:** `Language::def()` calls `REGISTRY.get(&self.to_string()).expect("language not in registry — check feature flags")`. This is called from multiple production code paths (parser init, chunk extraction, FTS normalization). If a `Language` enum variant exists but the corresponding `LanguageDef` isn't registered (e.g., feature flag misconfiguration, or a new variant added to the enum without a registry entry), this panics. The panic message is helpful, but it crashes the MCP server or CLI. The `parser/mod.rs:56` has the same pattern: `.expect("registry/enum mismatch")`.
- **Suggested fix:** Return `Option<&'static LanguageDef>` or `Result` instead of panicking. Callers that need the def can `.ok_or()` with a contextual error. Alternatively, add a compile-time or startup-time assertion that all `Language` variants have registry entries (a test exists but startup validation would catch feature-flag issues earlier).

#### R3: as_object_mut().unwrap() in CLI impact JSON output
- **Difficulty:** easy
- **Location:** `src/cli/commands/impact.rs:278-279`
- **Description:** `output.as_object_mut().unwrap()` on a `serde_json::Value` that was just constructed with `json!({...})`. While technically always safe (the `json!` macro with `{...}` always produces a JSON object), it violates the project convention of no `.unwrap()` outside tests. A future refactor that changes `output` to come from a function return or deserialization could make this panic.
- **Suggested fix:** Use `if let Some(obj) = output.as_object_mut() { obj.insert(...); }` or a dedicated struct with `#[derive(Serialize)]` for the JSON output.

#### R4: CLI --limit not clamped, flows to SQL LIMIT as i64
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:43`, `src/store/mod.rs:419,455`
- **Description:** The CLI `--limit` argument is a raw `usize` with default 5 but no upper bound. It flows to `store.search_fts(query, limit)` and `store.search_by_name(name, limit)` where it's cast as `limit as i64` for the SQL LIMIT clause. On a 64-bit platform, a user passing `--limit 18446744073709551615` (usize::MAX) causes the `as i64` cast to produce `-1`, and SQLite treats `LIMIT -1` as "no limit", returning all rows. For large indexes this could exhaust memory. The MCP path correctly clamps to `[1, 20]` (search.rs:22), and the config path clamps to `[1, 100]` (config.rs:116), but the direct CLI argument bypasses both.
- **Suggested fix:** Add `.clamp(1, 100)` or `.min(100)` to the CLI limit after parsing, similar to the config clamping. Or use `u32` instead of `usize` for the CLI arg (clap will reject values > u32::MAX).

#### R5: cap.get(0).unwrap() in markdown reference extraction
- **Difficulty:** easy
- **Location:** `src/parser/markdown.rs:540,563`
- **Description:** `cap.get(0).unwrap().start()` is used twice in `extract_references_from_text()` to get the match start position. While regex capture group 0 (the overall match) is guaranteed to exist by the regex crate's API contract, `.unwrap()` in non-test code is a project policy violation.
- **Suggested fix:** Use `cap.get(0).map(|m| m.start()).unwrap_or(0)` or extract into a local variable with a comment noting the API guarantee.

#### R6: embed_batch discards output tensor shape, trusts hardcoded dim=768
- **Difficulty:** medium
- **Location:** `src/embedder.rs:485-503`
- **Description:** `outputs["last_hidden_state"].try_extract_tensor::<f32>()` returns `(_shape, data)` but `_shape` is discarded. The code then indexes `data` with `offset = i * seq_len * embedding_dim + j * embedding_dim` where `embedding_dim` is hardcoded to 768. If the model outputs a different dimension (model corruption, wrong model loaded, ONNX version mismatch), `data[offset + k]` panics with index out of bounds. The actual output shape is available in `_shape` but never validated against the expected 768. (Previously triaged as Robust#3 in v0.9.1 but still unfixed.)
- **Suggested fix:** Validate: `let shape = _shape; if shape.len() < 3 || shape[2] != 768 { return Err(EmbedderError::InferenceFailed(...)); }`.

#### R7: normalize_for_fts byte-slices result string at MAX_FTS_OUTPUT_LEN boundary
- **Difficulty:** medium
- **Location:** `src/nl.rs:169,192`
- **Description:** `result[..MAX_FTS_OUTPUT_LEN]` slices a String at byte position 16384. If the result contains multi-byte UTF-8 characters (CJK text passes through `is_alphanumeric()` and `tokenize_identifier`), the slice panics when the boundary falls mid-character. The `rfind(' ')` that follows would find a safe boundary, but the byte slice happens first and can panic before `rfind` executes. The same pattern appears at line 192 for the post-loop truncation. (Previously triaged as AC13 in v0.9.1 but still unfixed.)
- **Suggested fix:** Use `result.floor_char_boundary(MAX_FTS_OUTPUT_LEN)` (stabilized in Rust 1.80), or find the space boundary without slicing first: `if let Some(pos) = result.as_bytes()[..MAX_FTS_OUTPUT_LEN.min(result.len())].iter().rposition(|&b| b == b' ') { result.truncate(pos); }`.

#### R8: notes list byte-truncation of note text at position 117
- **Difficulty:** easy
- **Location:** `src/cli/commands/notes.rs:449-450`
- **Description:** `&note.text[..117]` slices note text at byte position 117 for display preview. If the note contains multi-byte UTF-8 characters, this panics. The same file already has a safe `text_preview()` helper at line 118 that uses `.char_indices().nth(100)` for safe truncation. (Overlaps with EH13.)
- **Suggested fix:** Replace `&note.text[..117]` with `text_preview(&note.text)` (the safe helper defined in the same file).

#### R9: HNSW save uses assert_eq! which panics in MCP server context
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs:104-110`
- **Description:** `assert_eq!(hnsw_count, self.id_map.len(), ...)` before HNSW save. In the MCP server, this panic is caught by the thread boundary but crashes the current request and poisons any mutex held. A Result-returning check would allow the caller to log the mismatch and skip the save gracefully. (Previously triaged as Robust#2/DS#9 in v0.9.1 but still unfixed.)
- **Suggested fix:** Replace `assert_eq!` with `if hnsw_count != self.id_map.len() { return Err(HnswError::Internal(format!(...))); }`.

#### R10: embedding_to_bytes uses assert_eq! which panics on dimension mismatch
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:471-477`
- **Description:** `assert_eq!(embedding.len(), EXPECTED_DIMENSIONS, ...)` panics if an embedding has wrong dimensions. In the MCP server context, this brings down the current request thread. The comment says "This is intentional — storing wrong-sized embeddings corrupts the index", but a Result return would prevent corruption equally well while avoiding a panic. (Previously triaged as Robust#1 in v0.9.1 but still unfixed — deferred as P4 issue #300.)
- **Suggested fix:** Return `Result<Vec<u8>, StoreError>` instead of panicking. The caller can propagate the error cleanly.

## Security

#### S1: FTS5 injection via search_by_name double-quote escape
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:443`, `src/store/chunks.rs:558-559`
- **Description:** `search_by_name` and `search_by_names_batch` build FTS5 MATCH queries with `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. The `normalized` value comes from `normalize_for_fts()`, which strips most special characters but preserves alphanumeric tokens joined by spaces. However, these tokens are interpolated inside double-quoted FTS5 phrases. If `normalize_for_fts` produces a token containing a double quote (currently impossible since `normalize_for_fts` strips non-alphanumeric chars), the FTS5 query would be malformed. The actual risk is low because `normalize_for_fts` is a solid sanitizer, but the defense relies on an implicit property of a function in a different module rather than explicit sanitization at the point of query construction. A future change to `normalize_for_fts` (e.g., allowing hyphens or periods) could silently break this assumption.
- **Suggested fix:** Add explicit double-quote escaping at the FTS query construction site: `normalized.replace('"', "")` before interpolation. This makes the defense explicit and local, independent of `normalize_for_fts` internals.

#### S2: tool_context has no path traversal validation
- **Difficulty:** medium
- **Location:** `src/mcp/tools/context.rs:23-24`
- **Description:** `tool_context` builds `abs_path = server.project_root.join(path)` and passes the resulting string to `store.get_chunks_by_origin()`. Unlike `tool_read` (which canonicalizes and validates `starts_with(project_root)`), `tool_context` performs no path traversal check. The `path` parameter comes from MCP client input. While `get_chunks_by_origin` only returns data already in the index (so it can't read arbitrary files), a path like `../../etc/passwd` would be joined and converted to a string that's used as a database query. If a cross-project index happens to contain chunks with matching origins, the tool could leak information about indexed files outside the current project. More practically, the inconsistency with `tool_read`'s validation is a defense-in-depth gap.
- **Suggested fix:** Add the same canonicalize + `starts_with(project_root)` check used in `tool_read`, or at minimum sanitize `..` components from the path before joining.

#### S3: sanitize_error_message misses several common path prefixes
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:216-222`
- **Description:** The Unix regex covers `/home`, `/Users`, `/tmp`, `/var`, `/usr`, `/opt`, `/etc`, `/mnt`, `/root`, `/run`, `/srv`, `/proc`, `/snap`, `/Library`, `/Applications`, `/private`. It misses `/data` (Android, some containers), `/workspace` (GitHub Codespaces, Gitpod), `/workspaces` (Codespaces), `/nix` (NixOS), `/gnu` (Guix), and `/app` (common Docker convention). The Windows regex misses `D:\` through `Z:\` drive letters in the pattern's prefix requirement (it only matches when the path component after the drive is `Users`, `Windows`, etc. — `D:\Projects\secret\...` would pass through unsanitized). Already flagged as Sec#6 in v0.9.1 triage but the specific gaps were not enumerated.
- **Suggested fix:** Broaden the Unix regex to include `/data`, `/workspace`, `/workspaces`, `/nix`, `/app`. For Windows, add a catch-all for any drive letter followed by `:\` and a non-trivial path. Or take a simpler approach: strip any absolute path (starting with `/` or `X:\`) that contains 3+ path components.

#### S4: MCP protocol version header reflected in error without sanitization
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:297`
- **Description:** When a client sends an unsupported `MCP-Protocol-Version` header, the error message includes the client-provided version string verbatim: `format!("Unsupported protocol version: {}. Supported: {}", version_str, ...)`. The `version_str` comes from `headers.get("mcp-protocol-version").to_str()`. While this is a JSON-RPC error response (not HTML), a malicious client could send a crafted version string that appears confusing in logs or downstream tools. The response bypasses `sanitize_error_message` because it's returned directly from the handler, not through the error path.
- **Suggested fix:** Truncate `version_str` to a reasonable length (e.g., 64 chars) and strip non-printable characters before including in the error message.

#### S5: Reference config `path` field allows arbitrary filesystem access
- **Difficulty:** medium
- **Location:** `src/config.rs:18`, `src/reference.rs:40-41`
- **Description:** `ReferenceConfig.path` is a `PathBuf` deserialized directly from TOML config (`~/.config/cqs/config.toml` or `.cqs.toml`). There is no validation that the path points to a legitimate cqs index directory. A malicious `.cqs.toml` in a cloned repository could set `path = "/etc"` or `path = "/home/victim/.ssh"` — `load_references` would attempt to `Store::open(path.join("index.db"))` on that path. While `Store::open` would fail (no valid SQLite DB), the attempt reveals directory existence via error timing differences. More importantly, if an attacker places a crafted `index.db` SQLite file at a known path, the Store would open it and execute the schema/version checks, potentially triggering SQLite parsing of attacker-controlled data. The `validate_ref_name` function validates the name but NOT the path.
- **Suggested fix:** Validate that reference paths either: (1) live under the refs_dir (`~/.local/share/cqs/refs/`), or (2) are explicitly allowed by the user config (not the project config). The project `.cqs.toml` is attacker-controlled in a clone scenario and should not be trusted for arbitrary paths. At minimum, verify the path contains an `index.db` file before opening.

#### S6: Project .cqs.toml can override user config references (trust boundary)
- **Difficulty:** medium
- **Location:** `src/config.rs:209-215`
- **Description:** `Config::override_with()` allows project config to replace user config references by name. If a user has `[[reference]] name = "stdlib"` pointing to their trusted index, a malicious `.cqs.toml` in a cloned repo can define `[[reference]] name = "stdlib"` with a different `path`, redirecting stdlib searches to attacker-controlled data. The override is silent — no warning when a project config replaces a user-level reference. This is a trust boundary violation: project config (untrusted, from cloned repo) overrides user config (trusted, user-created).
- **Suggested fix:** When a project reference replaces a user reference by name, log a warning: `tracing::warn!("Project config overrides user reference '{}' — path changed from {} to {}", name, old_path, new_path)`. Or require explicit opt-in for project-level reference overrides.

#### S7: Windows tasklist command injection via PID
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:28-33`
- **Description:** On Windows, `process_exists` passes a PID to `Command::new("tasklist").args(["/FI", &format!("PID eq {}", pid)])`. The PID comes from parsing the lock file content (`content.trim().parse::<u32>()`). Since it's parsed as `u32`, injection via the PID value itself is impossible (it's always a number). However, the `String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())` check could produce false positives: if PID is `1`, it would match any line containing "1" (e.g., PID 10, 100, 1000). This is not a security vulnerability per se, but it could cause the stale lock detection to believe a dead process is still alive, preventing indexing. Already partially triaged as Sec#4/Robust#5 in v0.9.1.
- **Suggested fix:** Change the contains check to match on a word boundary: look for the PID surrounded by whitespace in the tasklist output. Or use `pid.to_string() == field` after splitting on whitespace.

#### S8: HNSW ID map deserialization trusts file contents without size limits
- **Difficulty:** medium
- **Location:** `src/hnsw/persist.rs` (load path)
- **Description:** The HNSW load path reads `{basename}.hnsw.ids` (a JSON file mapping internal HNSW IDs to chunk string IDs). The JSON deserialization via `serde_json::from_reader` has no size limit — a malicious or corrupted IDs file could contain an extremely large JSON object causing OOM. While the file is local (attacker needs filesystem access), a corrupted or intentionally oversized file would crash the MCP server on startup. The checksum verification (if present) would catch random corruption, but not a deliberately crafted large file with a matching checksum.
- **Suggested fix:** Check file size before deserialization. If `ids_file.metadata().len() > MAX_IDS_FILE_SIZE` (e.g., 100MB), return an error. Or use a size-limited reader wrapper.

#### S9: add_reference_to_config writes config without exclusive file lock
- **Difficulty:** easy
- **Location:** `src/config.rs:229-285`
- **Description:** `add_reference_to_config` performs a read-modify-write on the config file without holding a lock. If two `cqs ref add` commands run concurrently, one's changes can be lost (last writer wins). While `rewrite_notes_file` correctly uses exclusive file locking for the same pattern, config writes don't. The `remove_reference_from_config` function has the same issue. In practice, concurrent ref add/remove is unlikely, but it's an inconsistency with the note file's TOCTOU protection.
- **Suggested fix:** Add exclusive file locking (using `fs4::FileExt::lock_exclusive`) around the read-modify-write cycle, matching the pattern in `rewrite_notes_file`.

#### S10: normalize_for_fts truncation can split multi-byte UTF-8 (potential panic)
- **Difficulty:** medium
- **Location:** `src/nl.rs:168-173`, `src/nl.rs:190-195`
- **Description:** `result[..MAX_FTS_OUTPUT_LEN].rfind(' ')` byte-slices the result string at position 16384. If the string contains multi-byte UTF-8 characters (CJK text passes through the `is_cjk_or_kana` check and `is_alphanumeric`), position 16384 could fall mid-character, causing a panic. The `rfind(' ')` call on a byte-sliced `&str` is the problem — Rust's string slicing panics on non-char boundaries. Already flagged as AC13/R7 in prior audits but noted here because it's also a security issue: user-controlled query text flowing through MCP → `normalize_for_fts` could crash the server with a crafted CJK string of exactly the right length.
- **Suggested fix:** Use `result.floor_char_boundary(MAX_FTS_OUTPUT_LEN)` (stable since Rust 1.80) instead of direct byte indexing. This is the same fix suggested in R7 but emphasized here for the security (crash via MCP input) angle.

## Performance

#### P1: search_filtered brute-force loads all embeddings into memory for every search
- **Difficulty:** hard
- **Location:** `src/search.rs:266-272`
- **Description:** When no HNSW index is available (or HNSW returns empty), `search_filtered` executes `SELECT id, embedding FROM chunks` which fetches ALL rows into a Vec via `fetch_all()`. For a 10k-chunk index, this is ~10k rows x ~3KB/embedding = ~30MB allocated and deserialized per search. The bounded heap (line 290) correctly limits scoring memory, but the full embedding data is already materialized before scoring starts. On the MCP server handling concurrent requests, this creates burst allocations. The HNSW-guided path (`search_by_candidate_ids`) avoids this by fetching only candidate rows, but the brute-force fallback has no mitigation. Previously triaged as P4/Perf#4 ("hard - streaming rewrite") in v0.9.1.
- **Suggested fix:** Use SQLite streaming via `fetch()` (returns a Stream) instead of `fetch_all()`. Process rows one at a time through the BoundedScoreHeap, discarding embeddings immediately after scoring. This eliminates the O(n) allocation. Alternatively, batch-stream with LIMIT/OFFSET like `embedding_batches()` does for HNSW building.

#### P2: watch mode reindex_files calls upsert_chunk per-chunk instead of upsert_chunks_batch
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:265-276`
- **Description:** `reindex_files()` iterates chunks and calls `store.upsert_chunk()` one at a time in a loop. `upsert_chunk` delegates to `upsert_chunks_batch` with a single-element slice, meaning each chunk gets its own SQLite transaction (BEGIN + 3 INSERTs + COMMIT). For a file with 50 chunks, this is 50 separate transactions instead of 1. The pipeline (`pipeline.rs:553`) correctly batches. The difference: pipeline uses batch upsert with a single mtime; watch needs per-file mtime but still iterates per-chunk within a file.
- **Suggested fix:** Group chunks by file, then call `store.upsert_chunks_batch()` once per file group with that file's mtime. The mtime is already cached per-file in `mtime_cache`. This reduces 50 transactions to 1 per file, which is the dominant I/O cost in watch mode reindexing.

#### P3: watch mode embeds all chunks in one batch, no content-hash cache check
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:251-257`
- **Description:** `reindex_files()` generates NL descriptions and calls `embedder.embed_documents()` for ALL parsed chunks from changed files, even if their content hasn't changed. The pipeline has `prepare_for_embedding()` which checks `store.get_embeddings_by_hashes()` to skip embedding unchanged chunks. Watch mode skips this optimization entirely. For files where only one function changed, all 50 functions in the file get re-embedded (~100ms each on CPU), even though 49 have identical content hashes in the store.
- **Suggested fix:** Add the same hash-based cache check from `pipeline.rs:133-150`. Before calling `embed_documents`, check `store.get_embeddings_by_hashes()` and only embed chunks with new content hashes. This could skip 90%+ of embeddings for typical edits.

#### P4: search_by_names_batch issues one FTS query per name (N+1 pattern)
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:552-608`
- **Description:** `search_by_names_batch()` loops over each name and issues an individual FTS query (`chunks_fts MATCH ?1`). For gather BFS expansion with 200 expanded names, this is 200 sequential SQLite FTS queries within a single `block_on`. FTS5 doesn't efficiently support OR across column-filtered terms, but the current approach serializes all queries. Each query involves SQLite's B-tree traversal + JOIN + sort. Previously triaged as P4/Perf#2 in v0.9.1 ("gather N+1 FTS").
- **Suggested fix:** Batch names into groups and use FTS OR queries: `name:"foo" OR name:"bar" OR name:"baz"`. FTS5 handles OR at the query level. Then post-filter results by name to assign correct scores. This reduces 200 queries to ~10-20 batched queries. Alternatively, use a single query with `name IN (...)` against the chunks table directly (bypassing FTS) since name lookup uses exact/prefix matching, not full-text semantics.

#### P5: diff loads all chunk identities even when language filter applies
- **Difficulty:** medium
- **Location:** `src/diff.rs:97-98, 111-127`
- **Description:** `semantic_diff()` calls `all_chunk_identities()` on both stores, which runs `SELECT id, origin, name, chunk_type, language, line_start, parent_id, window_idx FROM chunks` — returning ALL chunks. When using a language filter (lines 111-127), all identities are loaded first, then filtered in Rust. For a diff between a project (10k chunks) and a reference (10k chunks), identity loading alone fetches 20k rows. The language filter could be pushed down to SQL.
- **Suggested fix:** Add an optional `language` parameter to `all_chunk_identities()` and push the filter to the SQL WHERE clause: `WHERE language = ?1` when specified. For large polyglot codebases, this could reduce fetched rows by 50-80%.

#### P6: normalize_for_fts called 4 times per chunk during upsert
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:70-73`
- **Description:** `upsert_chunks_batch()` calls `normalize_for_fts()` separately for `chunk.name`, `chunk.signature`, `chunk.content`, and `chunk.doc`. For `chunk.content` (which can be hundreds of lines of code), tokenization is significant work — each character is inspected, identifiers split on camelCase/snake_case boundaries. Previously triaged as P4/Perf#7 ("low impact") in v0.9.1. For batch indexing of 10k chunks this adds up, and the work happens on the writer thread (the pipeline bottleneck).
- **Suggested fix:** Pre-compute normalized values during pipeline preparation (alongside NL description generation), passing them to the writer. This moves FTS normalization from the writer thread to the parser threads which have spare capacity.

#### P7: embedding_to_bytes uses per-float iterator chain instead of memcpy
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:478-483`
- **Description:** `embedding_to_bytes()` uses `embedding.as_slice().iter().flat_map(|f| f.to_le_bytes()).collect()`. This creates an iterator chain that collects byte-by-byte. For 769 floats, this iterates 3076 times through a flat_map adapter. The inverse operation (`embedding_slice` at line 489) already uses `bytemuck::cast_slice` for zero-copy view. Called once per chunk during upsert — for 10k chunks during batch indexing, the overhead accumulates.
- **Suggested fix:** Use `bytemuck::cast_slice::<f32, u8>(embedding.as_slice()).to_vec()` for a single memcpy. This mirrors the `embedding_slice` function for consistency and eliminates per-float iteration.

#### P8: search_across_projects opens a new Store per project per search
- **Difficulty:** medium
- **Location:** `src/project.rs:133`
- **Description:** `search_across_projects()` calls `Store::open()` for each registered project on every search. `Store::open` creates a new SQLite connection pool and tokio runtime. For 5 registered projects, each cross-project search creates 5 connection pools and 5 runtimes, runs 5 brute-force searches, then drops everything. The pool/runtime creation overhead is ~5-10ms per store. Additionally, no HNSW index is loaded (already noted in AC7), so each search is O(n) per project.
- **Suggested fix:** Cache opened stores in a `HashMap<PathBuf, Store>` at the module level or in `ProjectRegistry`. Load HNSW indexes when available. For the MCP server (which may call cross-project search repeatedly), this eliminates repeated pool creation.

#### P9: pipeline writer clones all chunk+embedding pairs for multi-file batches
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:556-565`
- **Description:** When a batch spans multiple files (multi-file path at line 554), the writer groups by file then calls `pairs.into_iter().cloned().collect()` — cloning every Chunk and Embedding. A Chunk contains content (hundreds of bytes), signature, doc, etc. An Embedding is 3076 bytes. For a 32-chunk batch across 5 files, all 32 pairs get cloned. The single-file fast path (line 551) correctly avoids this.
- **Suggested fix:** Change `upsert_chunks_batch` to accept per-chunk mtimes (e.g., `&[Option<i64>]` parallel to chunks) instead of a single mtime. This eliminates the need to group by file entirely, and the multi-file path can use the same zero-copy approach as the single-file path.

#### P10: find_dead_code loads full content for all uncalled functions
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:358-368`
- **Description:** The dead code query selects `c.content` for ALL uncalled functions. Content is needed for trait impl detection (line 408: `TRAIT_IMPL_RE.is_match(&chunk.content)`) and `no_mangle` detection (line 415). However, most candidates are filtered out by simpler checks first (name == "main", test membership, test file path). For codebases with 500+ uncalled functions (common for library code with many pub items), loading full content for all of them upfront is wasteful.
- **Suggested fix:** Reorder: load a lightweight query first (without content), apply name/test/path filters, then do a second query for content only for the remaining candidates needing trait/no_mangle checks. Or push the checks to SQL: `AND c.content NOT LIKE '%impl%for%' AND c.content NOT LIKE '%no_mangle%'` as pre-filters.

#### P11: gather loads entire call graph for every invocation
- **Difficulty:** medium
- **Location:** `src/gather.rs:100`
- **Description:** `gather()` calls `store.get_call_graph()` which loads ALL edges from `function_calls` into two HashMaps. For a codebase with 2000+ call edges, this builds the full graph even when BFS only visits 10-20 nodes. The graph is not cached between calls. In the MCP server context, repeated gather queries rebuild the graph from scratch each time.
- **Suggested fix:** Two options: (1) Cache the call graph in the Store (small ~200KB for 2000 edges, changes infrequently). Invalidate on `upsert_function_calls`. (2) Replace in-memory graph with per-node SQL queries during BFS: `SELECT callee_name FROM function_calls WHERE caller_name = ?1`. For shallow BFS (depth=1, ~5 seeds), ~25 indexed lookups may be faster than loading the entire table.

#### P12: get_call_graph clones all strings into both forward and reverse maps
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:297-303`
- **Description:** For each `(caller, callee)` row, the code does `forward.entry(caller.clone()).push(callee.clone())` then `reverse.entry(callee).push(caller)`. The first line clones `caller`, then the second moves it. For 2000 edges at ~50 bytes/name, this creates ~4000 string allocations where ~2000 would suffice. Previously triaged as P4/Perf#5 ("micro-optimization") in v0.9.1.
- **Suggested fix:** Use `Rc<str>` or interning to share string allocations. Or accept the current approach since total overhead is ~200KB — negligible. Noted for completeness.

## Resource Management

#### RM1: semantic_diff loads all matched-pair embeddings into memory at once
- **Difficulty:** medium
- **Location:** `src/diff.rs:168-173`
- **Description:** `semantic_diff()` calls `get_embeddings_by_ids()` for ALL matched pairs from both source and target stores simultaneously. Each embedding is 769 x 4 = 3,076 bytes. For a reference with 50k chunks where 40k match, this loads ~40k x 3KB x 2 stores = ~234MB of embeddings into two HashMaps at the same time. The identities (loaded at lines 97-98 via `all_chunk_identities`) add another ~10MB each. Total peak for a 50k-chunk diff: ~250MB. There is no limit on how many matched pairs are loaded, and no streaming comparison.
- **Suggested fix:** Process matched pairs in batches (e.g., 5000 at a time). Fetch embeddings for each batch, compute similarities, discard batch before loading next. Reduces peak memory from O(all_matches) to O(batch_size).

#### RM2: CAGRA build_from_store pre-allocates full flat_data Vec for all embeddings
- **Difficulty:** medium
- **Location:** `src/cagra.rs:438-439`
- **Description:** `build_from_store()` calls `Vec::with_capacity(chunk_count * EMBEDDING_DIM)` upfront, pre-allocating the full buffer. For 50k chunks: 50k x 769 x 4 bytes = ~146MB. For 200k chunks (large monorepo): ~585MB. The streaming from SQLite in 10k batches helps avoid reading everything at once from the DB, but the destination Vec holds everything in CPU memory simultaneously before `build_from_flat()` copies it to GPU. This is inherent to CAGRA's design (requires all data upfront), but the pre-allocation can cause OOM without warning.
- **Suggested fix:** Add a size guard before allocation: `let estimated_bytes = chunk_count * EMBEDDING_DIM * 4; if estimated_bytes > MAX_CAGRA_CPU_BYTES { return Err(...); }` with a configurable limit (default 1GB). Log the estimated size at info level.

#### RM3: Reference hot-reload drops old Stores under write lock (blocks all search)
- **Difficulty:** medium
- **Location:** `src/mcp/server.rs:302-319`
- **Description:** When `ensure_references_fresh()` detects a config change, it replaces `*guard` with new `ReferenceState` under a write lock. The old `Vec<ReferenceIndex>` is dropped while the lock is held. Each `ReferenceIndex.store` drop triggers a WAL checkpoint via `Store::drop` (using `catch_unwind + block_on`). With 5 references, that's 5 sequential WAL checkpoints under the write lock. All concurrent search requests are blocked waiting for the write lock to release. If WAL files are large, checkpoints can take hundreds of milliseconds each.
- **Suggested fix:** Move old references out before dropping: `let old = std::mem::replace(&mut guard.references, new_refs); drop(guard); drop(old);` This releases the write lock before the expensive Store cleanup runs.

#### RM4: Each Store creates its own tokio Runtime — 7+ runtimes in MCP server with references
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:116`
- **Description:** Every `Store::open()` creates a new `tokio::runtime::Runtime`. The MCP server has: 1 main project Store, N reference Stores (one per configured reference), 1 background CAGRA Store, and the HTTP transport creates a separate Runtime. With 5 references: 7 tokio Runtimes, each with a default thread pool (num_cpus). On a 16-core machine, this spawns up to 112 threads, most idle. Each Runtime also allocates its own timer wheel, I/O driver, and scheduler. The reference Stores handle minimal concurrent load (single queries), making full Runtimes wasteful.
- **Suggested fix:** For reference Stores, use `tokio::runtime::Builder::new_current_thread()` instead of the multi-threaded default. This creates single-threaded runtimes with minimal overhead. Longer term, share a single multi-threaded Runtime across all Stores.

#### RM5: Background CAGRA build opens a second Store with full connection pool
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:119-126`
- **Description:** `build_cagra_background()` calls `Store::open(index_path)` creating a second Store for the same database. This allocates a second connection pool (4 connections, up to 64MB page cache), second tokio Runtime, and duplicates the schema/model version checks. The background thread only needs sequential read access to stream embeddings. After CAGRA build completes, this Store is dropped (triggering another WAL checkpoint).
- **Suggested fix:** Use `max_connections(1)` for the background Store since it only reads sequentially. Or pre-stream embeddings into a buffer before spawning the background thread, avoiding the second Store entirely.

#### RM6: Store page cache = 16MB per connection x 4 connections x N stores = up to 384MB
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:146`
- **Description:** `PRAGMA cache_size = -16384` sets 16MB page cache per SQLite connection. With `max_connections(4)`, each Store allocates up to 64MB of page cache. The MCP server with 5 references = 6 Stores x 64MB = 384MB of potential page cache. In practice, idle connections don't fully fill their caches, and the 300s idle timeout eventually closes some. But during query bursts touching all references, all connections activate and allocate caches. Reference stores are read-only with low concurrency — 4 connections and 16MB cache each is excessive.
- **Suggested fix:** Add a `Store::open_readonly()` or `Store::open_with_options()` that uses `max_connections(1)` and `cache_size = -4096` (4MB). Use this for reference stores. This reduces worst-case reference cache from 5 x 64MB = 320MB to 5 x 4MB = 20MB.

#### RM7: HNSW ID map load doubles memory during JSON parse
- **Difficulty:** easy
- **Location:** `src/hnsw/persist.rs:275-283`
- **Description:** HNSW load reads the entire ID map into a `String` via `read_to_string()`, then `serde_json::from_str()` parses into `Vec<String>`. During parsing, both the raw JSON string and the parsed Vec exist in memory. For 50k chunks with ~80-byte IDs, the JSON file is ~5MB, peak ~10MB. The 500MB file size limit (line 256) was added to prevent OOM, but during parse the actual peak is 2x the file size — so a ~400MB file could peak at ~800MB.
- **Suggested fix:** Use `serde_json::from_reader(BufReader::new(File::open(...)))` instead of `read_to_string + from_str`. This streams the JSON parse without holding the entire raw string, reducing peak from ~2x to ~1.1x file size.

#### RM8: Embedder ONNX Session (~500MB) persists for MCP server lifetime via OnceLock
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:44`, `src/embedder.rs:207-218`
- **Description:** Once the `Embedder` is lazily initialized via `OnceLock`, it persists for the entire MCP server lifetime. The ONNX Session holds model weights (~500MB). For MCP, embedding happens only for search queries (~100ms each), but the model occupies ~500MB continuously. A Claude Code session may run for hours with the MCP server idle, holding 500MB the entire time. `OnceLock` makes release impossible — once set, it stays forever.
- **Suggested fix:** Replace `OnceLock<Embedder>` with `Mutex<Option<Embedder>>` and add inactivity timeout that sets `None` after N minutes, freeing ~500MB. Re-initialize on next query (~500ms cost). Low priority — 500MB is acceptable for a dev tool, and this is already a documented trade-off.

#### RM9: Watch mode pending_files HashSet retains capacity after burst
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:73,132`
- **Description:** `pending_files: HashSet<PathBuf>` is capped at `MAX_PENDING_FILES` (10,000). After a burst (e.g., `git checkout` triggering 10k file events), `pending_files.drain()` removes entries but the HashSet retains its allocated bucket memory (~800KB at 10k capacity). Subsequent normal operation (1-2 files at a time) never shrinks it. Over days of running, the high-water mark persists.
- **Suggested fix:** After drain, call `pending_files.shrink_to(64)` to release excess capacity. Or replace `drain()` with `std::mem::take(&mut pending_files)` which drops the old allocation entirely.

#### RM10: MCP server holds HNSW + CAGRA simultaneously during GPU upgrade window
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:128-133`
- **Description:** During CAGRA background build (5-30 seconds), the HNSW index occupies memory for serving queries. When CAGRA finishes, the write lock replaces HNSW with CAGRA, dropping HNSW. But the CAGRA `dataset` field (`Array2<f32>`) holds all embedding data in CPU memory (~150MB for 50k vectors) even though search uses the GPU index. During the swap, both exist briefly: HNSW graph (~50-100MB) + CAGRA CPU dataset (~150MB) + GPU memory. Peak: ~300MB+ for 50k vectors.
- **Suggested fix:** Document the peak memory in module docs. Consider whether CAGRA's CPU-side `dataset` can be dropped after GPU build — if cuVS doesn't reference it post-build, this saves ~150MB. Low priority since the overlap is brief.

#### RM11: all_chunk_identities loads entire table with no SQL-level filtering
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:618-623`
- **Description:** `all_chunk_identities()` uses `fetch_all()` to load every chunk's metadata. For 50k chunks, each identity is ~200 bytes = ~10MB. The function is called twice in `semantic_diff()` (once per store). Both results are then post-filtered in Rust: windowed chunks removed (lines 101-108), language filter applied (lines 111-127), then mapped into HashMaps. Rows that will be immediately discarded are still allocated and deserialized.
- **Suggested fix:** Move filtering into SQL: `WHERE (window_idx IS NULL OR window_idx = 0)` and `AND language = ?` when a language filter is provided. Or add a `chunk_identities_filtered()` method that accepts optional language filter. This avoids fetching rows that will be immediately discarded.

#### RM12: Pipeline creates 2 Embedder instances (GPU + CPU) simultaneously = ~1GB model memory
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:373,450`
- **Description:** The index pipeline spawns a GPU embedder thread (line 373: `Embedder::new()`) and a CPU fallback thread (line 450: `Embedder::new_cpu()`). Each Embedder lazily loads an ONNX Session (~500MB for the model). If both are active, two ONNX sessions exist simultaneously: ~1GB for model weights alone. The CPU fallback thread calls `embedder.warm()` (line 452) which triggers a dummy inference, pre-allocating buffers. In practice, GPU failures are rare, so the CPU session is loaded but rarely used.
- **Suggested fix:** Make the CPU embedder lazy: only initialize it when the first GPU failure is received on `fail_rx`, not at thread startup. Change the CPU thread to check for a failure message before calling `Embedder::new_cpu()`. This avoids the ~500MB CPU model allocation when GPU works reliably.

## Data Safety

#### DS1: Store::init() executes DDL statements without a transaction
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:198-253`
- **Description:** `Store::init()` reads `schema.sql`, splits on `;`, and executes each DDL statement individually via `sqlx::query(stmt).execute(&self.pool)`. These statements are not wrapped in a transaction. If the process crashes mid-init (e.g., after creating `chunks` but before `chunks_fts`), the database is left in a partial state: some tables exist, others don't, and the `cqs_meta` version row may be missing. On next startup, `init()` will re-run all statements — `CREATE TABLE IF NOT EXISTS` handles the already-created tables, but any partial FTS state or missing triggers could cause subtle failures. Previously triaged as DS#6/P2 in v0.9.1 (marked "PRAGMAs idempotent") but the issue is the non-PRAGMA statements, not PRAGMAs.
- **Suggested fix:** Wrap the DDL execution loop in a single transaction. SQLite supports transactional DDL. This makes init all-or-nothing. The PRAGMAs (`journal_mode`, `busy_timeout`, etc.) should remain outside the transaction since they affect connection state, not schema.

#### DS2: Pipeline chunks and call graph stored in separate transactions
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:549-583`
- **Description:** The pipeline writer stage calls `upsert_chunks_batch()` (which runs in its own transaction), then separately calls `upsert_calls_batch()` (another transaction). A crash or error between these two calls leaves chunks in the database without their corresponding call graph edges. Subsequent searches find the functions, but `callers`/`callees`/`gather` return incomplete results for those chunks. The inconsistency persists until the next full re-index. Previously triaged as DS#3/P2 in v0.9.1 — not marked fixed.
- **Suggested fix:** Combine chunk upsert and call graph upsert into a single transaction. Both operations are SQLite writes on the same connection — wrapping them in one `BEGIN`/`COMMIT` is straightforward. The challenge is that `upsert_chunks_batch()` and `upsert_calls_batch()` each currently manage their own transactions internally, so they'd need a variant that accepts an existing transaction handle.

#### DS3: Watch mode delete + reinsert not atomic across operations
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:259-276`
- **Description:** When a file changes, `reindex_files()` calls `store.delete_by_origin(&origin)` (one transaction), then loops calling `store.upsert_chunk()` per chunk (each a separate transaction — see P2). If the process crashes after delete but before all upserts complete, the file's chunks are partially or fully lost. A concurrent search during the window between delete and upserts sees zero results for that file. The window is proportional to the number of chunks x embedding time.
- **Suggested fix:** Wrap the entire per-file operation (delete + batch upsert) in a single transaction. This requires passing a transaction handle through to both operations. Alternatively, batch all chunks per file and call `upsert_chunks_batch` (which already uses a transaction internally) after delete, within a shared transaction.

#### DS4: Config read-modify-write race (no file locking)
- **Difficulty:** medium
- **Location:** `src/config.rs:229-285`, `src/config.rs:288-327`
- **Description:** `add_reference_to_config()` and `remove_reference_from_config()` read the config file, modify it in memory, and write it back without holding a file lock. Two concurrent `cqs ref add` commands can race: both read the same config, each adds its reference, and the last writer silently drops the first's change. Also reported as S9 from a security angle. Previously triaged as DS#2/P2 in v0.9.1 — not marked fixed. The note file (`src/note.rs:rewrite_notes_file`) correctly uses `fs4::FileExt::lock_exclusive` for the same pattern.
- **Suggested fix:** Add exclusive file locking around the read-modify-write cycle, matching the pattern in `rewrite_notes_file()`. Use `fs4::FileExt::lock_exclusive` on the config file before reading, hold through write, release after.

#### DS5: ProjectRegistry.save() has no file locking or atomic write
- **Difficulty:** easy
- **Location:** `src/project.rs:38-47`
- **Description:** `ProjectRegistry::save()` calls `std::fs::write(&path, content)` directly — no file lock and no atomic temp-file-rename pattern. Concurrent `cqs project register` commands can corrupt the TOML file (partial writes) or silently drop entries (last-writer-wins race). The `register()` method at line 50 calls `save()` immediately after modifying the in-memory vec, so the race window includes the `retain` + `push` + serialize + write sequence. Unlike notes.toml (which has locking) and HNSW files (which use temp-dir + rename), the project registry has no write safety.
- **Suggested fix:** Use the same atomic write pattern as `rewrite_notes_file`: write to a temp file in the same directory, then `fs::rename` over the target. Add `fs4::FileExt::lock_exclusive` for concurrent-access safety.

#### DS6: HNSW index not rebuilt after watch mode updates
- **Difficulty:** hard
- **Location:** `src/cli/watch.rs:151-160`
- **Description:** Watch mode calls `store.upsert_chunk()` per chunk but never rebuilds the HNSW index. After watch-mode indexing, new/modified chunks are in SQLite but not in the HNSW graph. Searches using the HNSW path won't find them until `cqs index` or `cqs gc --rebuild` is run. If the user relies on watch mode exclusively (no periodic re-index), the HNSW index silently diverges from SQLite — search results degrade over time without any warning. Existing issue #236. Previously triaged as DS#5/P4 in v0.9.1.
- **Suggested fix:** After a batch of watch-mode upserts, incrementally insert new points into the loaded HNSW graph (hnsw-rs supports `insert`). Alternatively, trigger a full HNSW rebuild periodically (e.g., after N cumulative watch-mode changes) or warn the user that watch mode doesn't update the HNSW index.

#### DS7: note.rs parse_notes reads file content after releasing shared lock
- **Difficulty:** easy
- **Location:** `src/note.rs:117-143`
- **Description:** `parse_notes()` opens the notes file, acquires a shared lock via `FileExt::lock_shared(&lock_file)`, then opens a SECOND file handle (`fs::read_to_string(&path)`) to read the content, then releases the lock. The read happens through a different file handle than the one holding the lock. On most OSes this still provides the intended protection (the lock prevents concurrent exclusive writers), but the pattern is fragile: the lock is on `lock_file` while the read is on `path` — if these ever diverge (e.g., symlinks, different canonicalization), the lock provides no protection. Additionally, the shared lock is released (via `drop`) before the content is parsed, so a writer could modify the file between read and parse (though since parsing is in-memory from the already-read string, this is a theoretical rather than practical concern).
- **Suggested fix:** Read the file content through the same file handle that holds the lock: `lock_file.seek(SeekFrom::Start(0)); let mut content = String::new(); lock_file.read_to_string(&mut content);`. This guarantees the lock covers the actual read. Or use `fs::read_to_string` while the lock is still held (current code — just ensure drop order is correct).

#### DS8: notes.toml temp file not cleaned up on serialization failure
- **Difficulty:** easy
- **Location:** `src/note.rs:182-184`
- **Description:** `rewrite_notes_file()` creates a temp file, then calls `toml::to_string_pretty(&notes_file)`. If serialization fails (e.g., a note contains invalid TOML characters), the function returns the error but leaves the temp file on disk. Repeated failures accumulate orphan temp files in the notes directory. The `rename` cleanup only runs on the success path.
- **Suggested fix:** Use a `scopeguard` or manual cleanup in the error path: if `toml::to_string_pretty` fails, delete the temp file before returning the error. Or restructure to serialize into a String first (before creating the temp file), so the temp file is only created when serialization has already succeeded.

#### DS9: embedding_batches LIMIT/OFFSET pagination unstable under concurrent writes
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:739-813`
- **Description:** `EmbeddingBatchIterator` uses `SELECT ... LIMIT ? OFFSET ?` to paginate through chunks for HNSW building. If another process (watch mode, concurrent index) inserts or deletes rows between pagination calls, OFFSET shifts: rows can be skipped or duplicated. A skipped row means its embedding is missing from the HNSW index. A duplicated row wastes work but is harmless. In practice, this only matters when HNSW build runs concurrently with watch mode — full `cqs index` holds a process lock preventing concurrent indexing. Previously triaged as DS#10/P2 in v0.9.1 — not marked fixed.
- **Suggested fix:** Use cursor-based pagination instead of OFFSET: `WHERE id > ?last_seen_id ORDER BY id LIMIT ?`. This is stable regardless of concurrent modifications because it keys on the row's immutable ID rather than its position. Alternatively, snapshot the IDs first (`SELECT id FROM chunks ORDER BY id`), then fetch embeddings in batches by ID range.

#### DS10: Schema migration framework has no version-range guard
- **Difficulty:** medium
- **Location:** `src/store/migrations.rs:29-54`
- **Description:** `migrate()` matches on `current_version` to determine which migration to run. Currently there's only a placeholder comment and no actual migrations. The framework correctly uses a transaction for each migration step. However, there's no guard against downgrade scenarios: if a newer cqs binary creates a v11 schema and the user downgrades to a binary that only knows v10, the `match` falls through to the default arm which returns `Ok(())` silently — the binary operates against a schema it doesn't understand. The version check at `Store::open()` (line 170-175) does catch `> SCHEMA_VERSION` and returns an error, so this is defense-in-depth. Informational finding.
- **Suggested fix:** No immediate action needed — the `Store::open()` version check is the primary guard. For robustness, `migrate()` could explicitly reject `current_version > target_version` with a clear error message rather than silently succeeding.

#### DS11: Watch mode path normalization differs from pipeline
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:233-234`, `src/cli/pipeline.rs:281-285`
- **Description:** Watch mode uses `dunce::canonicalize(path)` for file paths (line 233), while the pipeline uses `dunce::canonicalize` on the project root but `strip_prefix` + relative path for individual files (line 281-285). If these produce different `origin` strings for the same file, watch mode's `delete_by_origin` won't match the pipeline's original `origin`, leaving stale chunks. The pipeline stores origins as relative paths (`src/foo.rs`); watch mode stores them after canonicalization which may produce absolute paths depending on the input. Related to PB1 (Windows path normalization). The `reindex_files` function at line 236 converts to `origin` via `path.strip_prefix(&self.root)` which should produce relative paths, but edge cases around symlinks or mount points could diverge.
- **Suggested fix:** Centralize origin computation into a single `compute_origin(project_root: &Path, file_path: &Path) -> String` function used by both pipeline and watch mode. This eliminates the risk of divergent path representations. The function should canonicalize both paths, then strip prefix, ensuring consistent relative output.

#### DS12: No SQLite database integrity verification on open
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:113-195`
- **Description:** `Store::open()` connects to the SQLite database, checks the schema version, and returns. There's no `PRAGMA integrity_check` or `PRAGMA quick_check` on open. A corrupted database (from a crash during WAL checkpoint, filesystem corruption, or partial copy) is silently used, potentially returning wrong results or causing runtime errors deep in query execution. The HNSW persistence layer has checksum verification (`blake3` in `persist.rs`), but the SQLite database — which holds all chunks, call graph, notes, and FTS data — has no equivalent check. Running `PRAGMA integrity_check` on every open is expensive for large databases, but `PRAGMA quick_check` (which skips content verification) completes in milliseconds.
- **Suggested fix:** Add `PRAGMA quick_check` after opening the database. If it fails, return a clear error suggesting `cqs index --rebuild`. For a lighter approach, check `PRAGMA integrity_check(1)` which stops after the first error. Alternatively, add a `cqs doctor` command that runs full integrity checks on demand.

#### DS13: store.close() WAL checkpoint failure silently returns Ok
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:568-579`
- **Description:** `Store::close()` attempts a WAL checkpoint via `PRAGMA wal_checkpoint(TRUNCATE)`. If this fails (e.g., another connection holds a read lock), the error is logged via `tracing::warn!` but the pool is still closed normally. The WAL file remains on disk, which is valid SQLite behavior (next open will replay it). However, the function signature returns `Result<()>` and the checkpoint failure path still returns `Ok(())` — the caller has no indication that the checkpoint failed. For the MCP server shutdown path, this means the server reports clean shutdown while leaving a potentially large WAL file that must be replayed on next startup, adding latency.
- **Suggested fix:** Return the checkpoint error so callers can decide how to handle it. Or attempt the checkpoint, log the warning, and proceed — but document that `close()` is best-effort for checkpointing. The current behavior is functionally correct (WAL replay on next open is safe), so this is low severity.

#### DS14: FTS index and chunks table consistency (informational — confirmed sound)
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:127-132`, `src/store/chunks.rs:60-64`
- **Description:** Verified that `upsert_chunks_batch()` correctly wraps both `chunks` table INSERT and `chunks_fts` INSERT/DELETE within a single transaction (lines 50-132). The FTS sync pattern is: delete old FTS entry by rowid, insert new FTS entry, all within the same transaction as the chunk upsert. `delete_by_origin()` similarly wraps FTS delete + chunk delete in one transaction (lines 134-156). No consistency gap found between FTS and chunks tables. Informational — confirmed sound.
