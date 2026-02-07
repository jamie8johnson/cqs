# Audit Findings — v0.5.3

Full 20-category audit. Collection phase — no fixes until all batches complete.

## Batch 1: Code Hygiene

#### Duplicated JSON formatting in MCP search tool
- **Difficulty:** medium
- **Location:** `src/mcp/tools/search.rs:248-358`
- **Description:** `format_unified_results` and `format_tagged_results` have nearly identical code for rendering `UnifiedResult::Code` and `UnifiedResult::Note` as JSON. The code/note JSON serialization block is copy-pasted between the two functions (lines 256-268 vs 311-333). This means any change to the output format must be made in two places. `format_code_result` (line 221) also duplicates the code path JSON but with slightly different path stripping logic.
- **Suggested fix:** Extract a shared `unified_result_to_json(r: &UnifiedResult) -> Value` helper, have all three functions call it. Add source/path-stripping as optional parameters.

#### Duplicated glob pattern compilation in search
- **Difficulty:** easy
- **Location:** `src/search.rs:250-258` and `src/search.rs:417-425`
- **Description:** The glob pattern compilation logic (`filter.path_pattern.as_ref().and_then(|p| match globset::Glob::new(p) { ... })`) is duplicated between `search_filtered` and `search_by_candidate_ids`. Both blocks are structurally identical, compiling the same pattern with the same warn-and-ignore error handling.
- **Suggested fix:** Extract a `compile_glob_filter(filter: &SearchFilter) -> Option<GlobMatcher>` helper function. Could live on `SearchFilter` itself.

#### Duplicated note insert logic in store/notes.rs
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:56-108` and `src/store/notes.rs:169-234`
- **Description:** `upsert_notes_batch` and `replace_notes_for_file` contain identical per-note INSERT + FTS DELETE/INSERT logic. The only difference is that `replace_notes_for_file` does a bulk DELETE first. The individual note insertion code (SQL query, mentions serialization, FTS update) is duplicated line-for-line.
- **Suggested fix:** Extract the per-note insertion into an `insert_note_in_tx(tx, note, embedding, source_str, now)` async helper and call it from both methods.

#### Long function: `run_index_pipeline` (416 lines)
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:183-599`
- **Description:** `run_index_pipeline` is a single 416-line function that sets up channels, spawns 3 threads (parser, GPU embedder, CPU embedder), runs the writer loop, and joins threads. While well-commented, it's difficult to review or modify any single stage because the function scope contains all of them. The GPU embedder thread body alone (lines 331-462) is 130 lines.
- **Suggested fix:** Extract each pipeline stage into its own function: `spawn_parser_thread(...)`, `spawn_gpu_embedder_thread(...)`, `spawn_cpu_embedder_thread(...)`, `run_writer_loop(...)`. This also makes unit testing individual stages possible.

#### GPU embedder thread has duplicated "send cached + requeue to CPU" pattern
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:364-396` and `src/cli/pipeline.rs:419-455`
- **Description:** The GPU embedder thread has two places where it sends cached results to the writer and requeues un-embedded chunks to the CPU fallback channel. The pattern (check `!prepared.cached.is_empty()`, send EmbeddedBatch, send failed batch to `fail_tx`) is duplicated for the "long batch" pre-filter case and the "GPU error" case.
- **Suggested fix:** Extract a `requeue_to_cpu(cached, to_embed, file_mtime, embed_tx, fail_tx)` helper.

#### `hnsw.rs` save method has duplicated file_dump block
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:516-537`
- **Description:** The match on `HnswInner::Owned` vs `HnswInner::Loaded` to call `file_dump` is duplicated — both arms do exactly the same thing, just accessing the HNSW through different paths. This is a minor instance of the broader pattern where `HnswInner`'s two variants require matching to access the inner `Hnsw`.
- **Suggested fix:** Add a `fn hnsw(&self) -> &Hnsw<'static, f32, DistCosine>` method on `HnswIndex` or `HnswInner` to avoid repeating the match in `save`, `search`, and `get_nb_point`. This consolidates 3 match blocks.

#### `NlTemplate` variants `NoPrefix`, `BodyKeywords`, `Compact`, `DocFirst` only used in model_eval tests
- **Difficulty:** easy
- **Location:** `src/nl.rs:266-277`
- **Description:** The `NlTemplate` enum has 5 variants, but only `Standard` is used in production code (`generate_nl_description` at line 311). The other 4 variants (`NoPrefix`, `BodyKeywords`, `Compact`, `DocFirst`) are only used in `tests/model_eval.rs` for evaluation experiments. This dead code adds complexity to `generate_nl_with_template` with unused branches.
- **Suggested fix:** Move the experimental variants and `generate_nl_with_template` behind a `#[cfg(test)]` gate, or into the test module. If these are intended for future use, add a doc comment explaining the intended usage.

#### `make_embedding` helper duplicated in hnsw.rs test modules
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:818-832` and `src/hnsw.rs:987-1000`
- **Description:** The `make_embedding` helper function is defined twice in hnsw.rs — once in `mod tests` (line 818) and once in `mod safety_tests` (line 987). Both create deterministic test embeddings, but with different seed multipliers (0.1 vs 10.0). The naming collision is confusing and the intent divergence (subtle vs clear separation) is not documented.
- **Suggested fix:** Move `make_embedding` to a shared `#[cfg(test)]` module or parameterize the seed multiplier. At minimum, rename one to clarify intent (e.g., `make_embedding_wide_spread`).

#### `Embedding::new()` bypasses dimension validation
- **Difficulty:** medium
- **Location:** `src/embedder.rs:87-89`
- **Description:** `Embedding::new()` accepts any `Vec<f32>` without validation, while `Embedding::try_new()` validates 769-dim. The unchecked constructor is used extensively in production (hnsw.rs, store/chunks.rs, store/notes.rs, search tests). This creates a latent risk: wrong-sized embeddings silently enter the system and only fail later at `embedding_to_bytes` (which panics) or `cosine_similarity` (which returns None). The docstring says "unchecked" but there's no enforcement elsewhere.
- **Suggested fix:** Consider making `new()` private or `pub(crate)` and requiring `try_new()` at public API boundaries. Internal code that constructs from known-good sources (model output, DB reads) can use `new()` but external callers should go through validation.

## Batch 1: Module Boundaries

#### search.rs implements Store methods outside the store module
- **Difficulty:** medium
- **Location:** `src/search.rs:184`
- **Description:** `search.rs` contains an `impl Store` block that directly accesses `self.pool` (lines 242, 325) and imports `pub(crate)` types from `store::helpers` (line 16: `embedding_slice`, `ChunkRow`, `ChunkSummary`). This splits the Store implementation across two modules, making it hard to discover the full Store API surface. The `pub(crate)` visibility on `helpers` exists primarily to serve this one external consumer.
- **Suggested fix:** Move search methods into `store/search.rs` (a new submodule of `store`), or create a `SearchEngine` struct that takes a `&Store` reference and uses only public Store methods. This would let `helpers` stay truly private to `store`.

#### index_notes helper in lib.rs couples lib root to domain logic
- **Difficulty:** easy
- **Location:** `src/lib.rs:129-181`
- **Description:** `lib.rs` contains a 50-line `index_notes` function with embedding, mtime extraction, and database transaction logic. This is domain logic that belongs in a domain module (e.g., `note.rs` or a new `indexing.rs`), not in the crate root. The crate root should only have module declarations and re-exports. Called by both `mcp/server.rs:252` and `cli/watch.rs:296`.
- **Suggested fix:** Move `index_notes` to `src/note.rs` or `src/store/notes.rs` as a free function or associated method. Both callers already import from those modules.

#### MCP server uses anyhow instead of typed errors
- **Difficulty:** medium
- **Location:** `src/mcp/server.rs:8`, `src/mcp/tools/*.rs` (all 7 files)
- **Description:** Project convention is "thiserror for library errors, anyhow in CLI." The MCP module is library code (exposed via `pub use mcp::{serve_http, serve_stdio}` in lib.rs), yet every MCP file uses `anyhow::Result`. This means library consumers get opaque errors they can't match on, and the MCP boundary doesn't have typed error variants.
- **Suggested fix:** Create `McpError` enum in `mcp/types.rs` using thiserror, covering tool-not-found, validation, store, and embedder errors. Keep anyhow only at the transport boundary (stdio/http) where errors are serialized to JSON-RPC.

#### pub(crate) NoteEntry directly constructed by MCP tools
- **Difficulty:** easy
- **Location:** `src/note.rs:42`, `src/mcp/tools/notes.rs:82`
- **Description:** `NoteEntry` is `pub(crate)` and constructed directly by `mcp/tools/notes.rs:82` (`crate::note::NoteEntry { sentiment, text, mentions }`). The MCP tool builds a NoteEntry to serialize it as TOML for file append. This couples the MCP module to the internal serialization format of notes. If `NoteEntry` fields change, the MCP tool would break.
- **Suggested fix:** Add a `NoteEntry::new(text, sentiment, mentions)` constructor or `note::format_toml_entry(text, sentiment, mentions) -> String` to encapsulate the serialization, so MCP doesn't need to know the struct layout.

#### All 14 modules declared pub in lib.rs — no enforced API boundary
- **Difficulty:** medium
- **Location:** `src/lib.rs:57-73`
- **Description:** Every module is `pub mod`, meaning external consumers can reach into `cqs::cli::*`, `cqs::mcp::server::McpServer`, `cqs::store::helpers::*`, etc. The re-exports on lines 75-84 define the intended public API (Embedder, Parser, Store, etc.), but module visibility doesn't enforce it. The `cli` module is particularly concerning — it's the binary's private implementation but publicly accessible from lib.rs. Modules like `math`, `nl`, and `source` are internal utilities with no clear external use case.
- **Suggested fix:** Change `cli` to `pub(crate) mod cli` (only `main.rs` uses it). Consider `pub(crate)` for `math`, `nl`, `source`, and internal `mcp` submodules, keeping only the re-exported items public.

#### reference.rs duplicates score extraction already on UnifiedResult
- **Difficulty:** easy
- **Location:** `src/reference.rs:161-166`
- **Description:** `tagged_score()` extracts scores by matching `UnifiedResult::Code` and `UnifiedResult::Note` — but `UnifiedResult::score()` (defined at `store/helpers.rs:215`) does exactly the same thing. The free function is redundant. (Also flagged under Code Hygiene for the duplication angle.)
- **Suggested fix:** Replace `tagged_score(t)` with `t.result.score()` in `merge_results` sort closure (line 149). Remove the `tagged_score` function.

#### store::pool and store::rt are pub(crate) — leaked to search.rs
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:97-98`
- **Description:** `Store.pool` and `Store.rt` are `pub(crate)`, which allows `search.rs` to directly execute raw SQL queries via `self.pool` (lines 242, 325). This bypasses any abstraction the Store module provides. If you rename `pool`, search.rs breaks. The internal async runtime is also exposed, meaning any crate module could call `self.rt.block_on()`.
- **Suggested fix:** If search methods move into `store/` (see finding #1), these can become `pub(super)` or private. Otherwise, add specific async query methods to Store that search.rs calls instead of raw pool access.

#### cli::commands::reference imports build_hnsw_index from cli::commands::index
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:14`
- **Description:** `reference.rs` imports `build_hnsw_index` from `commands::index`, creating an intra-CLI coupling where the `ref add` command depends on the `index` command's internal implementation. If `build_hnsw_index` changes signature or behavior, both commands break.
- **Suggested fix:** Extract `build_hnsw_index` into a shared CLI utility (e.g., `cli/pipeline.rs` or a new `cli/hnsw.rs`) so both commands depend on a shared utility rather than one command depending on another.

## Batch 1: Documentation

#### SECURITY.md says user config is "Not yet implemented" but it works
- **Difficulty:** easy
- **Location:** `SECURITY.md:87`
- **Description:** The Filesystem Access table says `~/.config/cqs/` has purpose "User config (future)" and when "Not yet implemented". However, `src/config.rs:69-71` actually loads `~/.config/cqs/config.toml` as the user defaults config file, and this path is documented correctly in both `README.md` and `config.rs` module docs. SECURITY.md is stale — this feature shipped in v0.5.3 with the config/reference system.
- **Suggested fix:** Update SECURITY.md read access table: change purpose to "User default config" and when to "All operations (if file exists)". Also add `~/.local/share/cqs/refs/` for reference index storage.

#### StoreError messages reference `cq` instead of `cqs`
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:32-43`
- **Description:** Four `StoreError` variants display messages referencing `cq` (the old binary name) instead of `cqs`: `SchemaMismatch` says "Run 'cq index --force'", `SchemaNewerThanCq` says "upgrade cq", `MigrationNotSupported` says "'cq index --force'", `ModelMismatch` says "'cq index --force'". Users following these instructions would get "command not found". The binary was renamed to `cqs` but these error messages weren't updated.
- **Suggested fix:** Replace all `cq` references with `cqs` in the error message strings. Also rename `SchemaNewerThanCq` to `SchemaNewerThanCqs` for consistency.

#### `search.rs` module doc references nonexistent `SearchEngine`
- **Difficulty:** easy
- **Location:** `src/search.rs:3`
- **Description:** The module doc says "This module contains the SearchEngine for code and note search" but there is no `SearchEngine` struct in this module. Search methods are implemented as `impl Store` blocks. The doc also claims "helper functions for similarity scoring" but `cosine_similarity` was moved to `src/math.rs`. The module doc is stale from a prior refactor.
- **Suggested fix:** Update to: "Search algorithms and name matching. Implements search methods on Store for semantic, hybrid, and index-guided search. See math.rs for similarity scoring."

#### `language/mod.rs` feature flag docs missing `lang-c` and `lang-java`
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:8-15`
- **Description:** The module-level doc lists feature flags but omits `lang-c` and `lang-java`, which were added in v0.5.0. The doc lists 5 flags (rust, python, typescript, javascript, go) plus `lang-all`, but `Cargo.toml` defines 7 language features with C and Java in the default set. Users reading the module docs would not know C and Java support exists.
- **Suggested fix:** Add `- \`lang-c\` - C support (enabled by default)` and `- \`lang-java\` - Java support (enabled by default)` to the feature flag list.

#### `lib.rs` Quick Start example uses unnecessary `mut` on Embedder
- **Difficulty:** easy
- **Location:** `src/lib.rs:23`
- **Description:** The crate-level Quick Start example declares `let mut embedder = Embedder::new()?;` but all `Embedder` methods (`embed_documents`, `embed_query`) take `&self`, not `&mut self`. The `mut` is unnecessary and misleading — it suggests the embedder has mutable state that callers need to manage, when in fact interior mutability (OnceCell, Mutex) handles it.
- **Suggested fix:** Change to `let embedder = Embedder::new()?;` (remove `mut`).

#### `embedder.rs` module doc is minimal — one line for 1072-line file
- **Difficulty:** easy
- **Location:** `src/embedder.rs:1`
- **Description:** The module doc is just `//! Embedding generation with ort + tokenizers` for a 1072-line file that handles model downloading, checksum verification, ONNX session management, GPU/CPU provider selection, LRU caching, batch embedding, and tokenization. Key information is buried deep in the file: provider selection logic (line ~233), cache behavior (line ~220), batch size strategy (line ~236). The `Embedder` struct doc (line ~199) is better but not discoverable from the module overview.
- **Suggested fix:** Expand module doc to mention key design decisions: lazy model loading, provider selection order (CUDA > TensorRT > CPU), query cache (LRU), batch processing, and the 769-dim output format.

#### CONTRIBUTING.md architecture lists `search.rs` as "cosine similarity" — moved to `math.rs`
- **Difficulty:** easy
- **Location:** `CONTRIBUTING.md:120`
- **Description:** The Architecture Overview says `search.rs — Search algorithms, cosine similarity, HNSW-guided search`. However, `cosine_similarity` lives in `src/math.rs` (since the math module extraction). The architecture overview is stale for this entry. `math.rs` is not listed in the architecture at all.
- **Suggested fix:** Update `search.rs` line to "Search algorithms, name matching, HNSW-guided search". Add `math.rs — Vector math utilities (cosine similarity, SIMD)` to the source tree listing.

#### `note.rs` module doc references obsolete types
- **Difficulty:** easy
- **Location:** `src/note.rs:3-4`
- **Description:** The module doc says "Replaces separate Scar and Hunch types with a simpler schema." These types (`Scar`, `Hunch`) no longer exist anywhere in the codebase — they were removed in an earlier version. This historical note is confusing for new contributors who have no context for what Scars and Hunches were.
- **Suggested fix:** Remove the "Replaces separate Scar and Hunch types" sentence. Replace with a description of what Notes actually are: "Notes are developer observations with sentiment, stored in TOML and indexed for semantic search."

#### `store/mod.rs` module doc lists only 3 of 5 submodules
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:8-11`
- **Description:** The module structure doc says it has `helpers`, `chunks`, `notes`, and `calls` submodules. But the actual module declaration (lines 13-22) also includes `migrations` (line 15). The `migrations` submodule handles schema upgrades and is non-trivial (has its own error variants in StoreError). Omitting it from the doc makes it less discoverable.
- **Suggested fix:** Add `- \`migrations\` - Schema version upgrades` to the module structure list.

## Batch 1: Error Propagation

#### 1. `serde_json::to_string(&note.mentions).unwrap_or_default()` silently drops serialization errors
- **Difficulty:** easy
- **Location:** `src/store/notes.rs:74` and `src/store/notes.rs:201`
- **Description:** If `serde_json::to_string` fails when serializing note mentions, `unwrap_or_default()` returns an empty string `""`, which is not valid JSON for an array. This corrupts the `mentions` column — the note is stored with empty mentions, and later reads via `serde_json::from_str` in `score_note_row` (notes.rs:31) will parse `""` as invalid JSON and fall back to an empty Vec. The note's mentions are permanently lost without any error indication. `Vec<String>` serialization should never realistically fail, but the pattern is still incorrect — if it did fail, the transaction should abort rather than store garbage.
- **Suggested fix:** Use `?` operator instead. `serde_json::to_string` returning `Err` on `Vec<String>` is effectively impossible, but `?` is both correct and no more complex than `unwrap_or_default()`.

#### 2. `Config::load_file` returns `None` for TOML parse errors — caller can't distinguish missing from malformed
- **Difficulty:** medium
- **Location:** `src/config.rs:91-120`
- **Description:** `load_file` returns `Option<Self>`, mapping both "file not found" and "parse error" to `None`. The caller in `Config::load` (line 68-74) uses `unwrap_or_default()` for both cases. If a user has a malformed `.cqs.toml`, their config is silently ignored and defaults are used. The only indication is a `tracing::warn`, which users may never see. This is especially problematic for reference configs — a typo in `[[reference]]` means references silently don't load. Existing #241 covers config validation more broadly, but this specific load_file conflation is a distinct error propagation issue.
- **Suggested fix:** Return `Result<Option<Self>, ConfigError>` where `None` = not found and `Err` = parse error. Or at minimum, print to stderr so the user sees the warning even without tracing.

#### 3. `search_reference` and `search_reference_by_name` swallow search errors, returning empty results
- **Difficulty:** medium
- **Location:** `src/reference.rs:78-97` and `src/reference.rs:100-119`
- **Description:** When a reference search fails (database error, corruption, etc.), the error is logged at `warn` level but the function returns an empty `vec![]`. The caller has no way to distinguish "no results" from "search failed." In multi-index search, this means a broken reference silently drops results rather than surfacing the error. The user sees fewer results without knowing why.
- **Suggested fix:** Return `Result<Vec<SearchResult>, StoreError>` and let the MCP tool layer decide whether to degrade gracefully. The tool_search function can then include a warning in the response when a reference fails.

#### 4. `get_by_content_hash` returns `None` for database errors — caller can't distinguish missing from failure
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:217-235`
- **Description:** `get_by_content_hash` returns `Option<Embedding>`, mapping database errors to `None` via warn+return. The caller treats `None` as "not cached, need to re-embed." On a transient SQLite error (SQLITE_BUSY, lock timeout), this causes unnecessary re-embedding work but won't corrupt data. The signature conflates two semantically different outcomes.
- **Suggested fix:** Return `Result<Option<Embedding>, StoreError>`. The pipeline can then decide to retry or skip.

#### 5. `get_embeddings_by_hashes` silently continues on batch fetch errors
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:264-269`
- **Description:** In the batch loop, if a `fetch_all` fails, the error is logged and the loop `continue`s to the next batch. This means partial results are returned without any indication that some hashes couldn't be checked. During indexing, affected chunks will be re-embedded unnecessarily (performance hit, not correctness).
- **Suggested fix:** Either accumulate errors and return them, or return `Result` and let the caller decide.

#### 6. `serde_json::to_value(&response).unwrap_or_default()` in HTTP transport produces null on serialization failure
- **Difficulty:** medium
- **Location:** `src/mcp/transports/http.rs:302`
- **Description:** If the JSON-RPC response fails to serialize (shouldn't happen in practice, but...), `unwrap_or_default()` returns `Value::Null`. The HTTP response will be `200 OK` with body `null`, which is not valid JSON-RPC. Clients will get a confusing null response instead of a proper error.
- **Suggested fix:** Map serialization failure to a 500 Internal Server Error with a JSON-RPC error object.

#### 7. `check_model_version` silently ignores unparseable dimension values
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:323-332`
- **Description:** If the stored `dimensions` value exists but can't be parsed as `u32` (line 324: `if let Ok(stored_dim) = dim_str.parse::<u32>()`), the check silently passes. A corrupted dimensions value in metadata means the index will proceed without validating dimensions, potentially leading to incorrect search results if the dimensions actually don't match.
- **Suggested fix:** Log a warning when parse fails, or return an error. The `if let Ok` pattern here silently drops the parse error.

#### 8. `HnswError::Internal` is used as a catch-all that loses type information
- **Difficulty:** medium
- **Location:** `src/hnsw.rs` (throughout — lines 115, 488, 501, 509, 519, 529, 541, 544, 553, 561, 576, 651, 662, 668, 685, 701)
- **Description:** Many distinct error conditions in `hnsw.rs` are mapped to `HnswError::Internal(String)`, losing the original error type. IO errors during checksum computation are wrapped as `Internal` instead of `Io`. JSON parse errors, directory creation errors, and file rename errors are all flattened to the same variant. This makes programmatic error handling impossible — callers can't distinguish recoverable IO errors from corruption errors from out-of-memory conditions.
- **Suggested fix:** Add specific variants: `HnswError::Checksum(String)`, `HnswError::IdMapCorrupted(String)`, `HnswError::SaveFailed { source: std::io::Error, path: PathBuf }`. Keep `Internal` only for truly unexpected conditions.

#### 9. Language parse error in `tool_search` silently defaults to Rust
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:40`
- **Description:** `l.parse().unwrap_or(Language::Rust)` — if a client passes an unsupported language string (e.g., "kotlin"), it silently becomes a Rust filter. The user gets results filtered to Rust without knowing their language filter was ignored.
- **Suggested fix:** Return an error listing valid languages, or log a warning and include it in the response.

#### 10. Pipeline parser thread swallows parse errors per-file with no aggregate count
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:250-254`
- **Description:** When `parser.parse_file()` fails, it logs a warning and returns an empty vec. There's no counter for parse failures, so `PipelineStats` doesn't include how many files failed to parse. If a parser bug causes widespread failures, the user only sees "indexed 0 chunks" with no explanation. The warnings scroll by in the progress output.
- **Suggested fix:** Add a `parse_failures: AtomicUsize` counter to `PipelineStats` and increment it in the error branch.

## Batch 1: API Design

#### `language` parameter on `cqs_search` silently falls back to Rust on bad input (existing: Error Propagation #9)
- **Difficulty:** medium
- **Location:** `src/mcp/tools/search.rs:39-40`
- **Description:** When the MCP client passes an invalid language string, `l.parse().unwrap_or(Language::Rust)` silently treats an unrecognized language as Rust. `cqs_search(query="foo", language="ruby")` filters to Rust files with no indication the filter was remapped. The tool schema enumerates valid values, but LLM clients may pass values outside the enum.
- **Suggested fix:** Return an error for unrecognized language values so the caller knows the filter was rejected.

#### `SearchFilter` has silent field coupling — `note_weight` ignored by most search methods
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:227-265`
- **Description:** `SearchFilter` has implicit dependencies: `name_boost > 0.0` only works when `query_text` is non-empty; `enable_rrf` requires `query_text`; `note_weight` only matters in `search_unified_with_index` but is silently ignored by `search_filtered` and `search_by_candidate_ids`. A caller can set `note_weight = 0.5`, pass it to `search_filtered`, and get no effect — notes are never searched in that code path. The `validate()` method catches the `query_text` requirement but does not flag context-dependent fields.
- **Suggested fix:** Document per-field which search methods honor them, or split into method-specific option types.

#### `SearchFilter` conflates filtering with scoring configuration
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:227-265`
- **Description:** `SearchFilter` mixes two concerns: (1) what to filter (languages, path_pattern) and (2) how to score (name_boost, enable_rrf, note_weight, query_text). The `query_text` field is the raw query string, not a filter, and must match what was embedded. Callers pass both an `Embedding` (from the query) and the same query as `query_text` in the filter, duplicating query information. If these get out of sync, name_boost and RRF use different text than the semantic embedding.
- **Suggested fix:** Split into `SearchFilter` (languages, path_pattern) and `SearchScoring` (name_boost, enable_rrf, note_weight, query_text). Or accept `query_text` as a separate parameter on search methods.

#### Inconsistent return types for count-of-affected-rows across Store methods
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:24,83,120`, `src/store/notes.rs:56,169,237`
- **Description:** `delete_by_origin` returns `Result<u32>`, `delete_notes_by_file` returns `Result<u32>`, `upsert_chunks_batch` returns `Result<usize>`, `replace_notes_for_file` returns `Result<usize>`, and `upsert_chunk` returns `Result<()>` (discards count). The type for "how many rows affected" alternates between `u32` and `usize` with no pattern. `u32` comes from casting sqlx's `u64 rows_affected()` — a lossy cast.
- **Suggested fix:** Standardize on `usize` for all affected-row counts.

#### `search_reference` and `search_reference_by_name` are free functions instead of methods
- **Difficulty:** easy
- **Location:** `src/reference.rs:72-119`
- **Description:** Both take `&ReferenceIndex` as first argument but are standalone free functions. Callers write `reference::search_reference(ref_idx, ...)` instead of `ref_idx.search(...)`. `merge_results` and `load_references` are correctly free functions (operate on collections), but per-index search functions are natural methods.
- **Suggested fix:** Move to `impl ReferenceIndex` as `fn search(...)` and `fn search_by_name(...)`.

#### `Embedding::as_vec()` returns `&Vec<f32>` — Rust API anti-pattern
- **Difficulty:** easy
- **Location:** `src/embedder.rs:147-149`
- **Description:** `as_vec()` returns `&Vec<f32>`, while `as_slice()` already returns `&[f32]`. `&Vec<T>` as a public return type is a well-known Rust anti-pattern. This method exists solely because hnsw_rs's `parallel_insert_data` takes `&[(&Vec<f32>, usize)]`. Exposing `&Vec<f32>` leaks an implementation detail.
- **Suggested fix:** Deprecate or `#[doc(hidden)]` `as_vec()`. Convert at the hnsw_rs call site.

#### `HnswIndex::build` has contradictory documentation — "deprecated" yet recommended
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:212-236`
- **Description:** Doc says "This method is soft-deprecated" but also recommends it for indexes under 50k chunks. No `#[deprecated]` attribute, so no compiler warning. The "When to use" section contradicts the deprecation notice.
- **Suggested fix:** Either add `#[deprecated]` to make it real, or remove the deprecation notice and keep both as valid choices.

#### `serve_stdio` and `serve_http` have divergent parameter shapes
- **Difficulty:** easy
- **Location:** `src/mcp/transports/stdio.rs:22`, `src/mcp/transports/http.rs:61`
- **Description:** `serve_stdio(project_root, use_gpu)` takes 2 params. `serve_http(project_root, bind, port, auth_token, use_gpu)` takes 5. Both construct `McpServer` and run it. `use_gpu` appears at different positional slots. Switching transports requires restructuring all arguments.
- **Suggested fix:** Extract shared fields into `ServerConfig { project_root, use_gpu }`.

#### `Store::search()` convenience wrapper is effectively dead code
- **Difficulty:** easy
- **Location:** `src/search.rs:186-193`
- **Description:** `Store::search(query, limit, threshold)` delegates to `search_filtered()` with default filter. No production code calls it. The lib.rs doc example (line 34) uses it with bare positional numbers `(query, 5, 0.3)`, setting a wrong expectation that this is the normal entry point.
- **Suggested fix:** Update lib.rs example to show the actual recommended path, or remove `search()`.

#### `needs_reindex` vs `notes_need_reindex` — inconsistent naming for identical pattern
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:97` and `src/store/notes.rs:264`
- **Description:** Chunk check is `store.needs_reindex(path)`. Note check is `store.notes_need_reindex(source_file)`. Same signature, same semantics, different naming — one uses bare verb, the other prefixes with entity name and uses different verb form ("needs" vs "need"). Callers can't predict the name.
- **Suggested fix:** Standardize: either `needs_reindex` / `notes_needs_reindex` or `chunk_needs_reindex` / `note_needs_reindex`.

## Batch 2: Extensibility

#### Adding a new language requires touching 5 places across 3 files
- **Difficulty:** medium
- **Location:** `src/language/mod.rs:144-160`, `src/language/mod.rs:193-205`, `src/language/mod.rs:226-242`, `src/language/mod.rs:264-283`, `Cargo.toml:102-113`
- **Description:** To add a new language (e.g., Ruby), you must: (1) create `src/language/ruby.rs` with a `LanguageDef`, (2) add `Language::Ruby` variant to the enum, (3) add arms to `Display`, `FromStr`, and `from_extension`, (4) add `#[cfg(feature)]` + `mod ruby` + `reg.register(ruby::definition())` in `LanguageRegistry::new`, (5) add `lang-ruby` feature to `Cargo.toml`. Steps 2-4 are all in `mod.rs` but spread across 4 separate match/impl blocks that must stay in sync. The `Language` enum, `Display`, `FromStr`, and registry are all manually maintained — adding a variant without updating all four causes a compile error, but there's no macro or test asserting completeness.
- **Suggested fix:** Use a declarative macro to generate the `Language` enum, `Display`, `FromStr`, and feature-gated registry entries from a single definition table. Or use `strum` derive macros for `Display`/`FromStr` and a single registration macro for the feature-gated blocks.

#### MCP tool list is a hardcoded 200-line JSON blob with no tool trait or registration
- **Difficulty:** medium
- **Location:** `src/mcp/tools/mod.rs:19-213`
- **Description:** Adding a new MCP tool requires: (1) write a handler module, (2) add `mod` declaration, (3) manually add a `Tool { name, description, input_schema }` entry to the 200-line `vec![]` in `handle_tools_list`, (4) add a match arm in `handle_tools_call`. Steps 3 and 4 are in different functions with no compile-time check that they're in sync. If you add a tool to the list but forget the dispatch arm (or vice versa), you get a runtime "Unknown tool" error. The JSON schema for each tool is a raw `serde_json::json!` block — no validation that the schema matches the handler's argument parsing.
- **Suggested fix:** Define a `McpTool` trait with `fn name()`, `fn description()`, `fn schema()`, `fn execute(server, args)`. Each tool module implements the trait. Registration iterates over a `Vec<Box<dyn McpTool>>` for listing and dispatching. This guarantees schema and handler stay together and eliminates the manual dispatch match.

#### Embedding model is hardcoded — swapping requires editing 7 constants and recompiling
- **Difficulty:** hard
- **Location:** `src/embedder.rs:14-20`, `src/lib.rs:90`, `src/store/helpers.rs:18`
- **Description:** The embedding model is baked into constants: `MODEL_REPO` (line 14), `MODEL_FILE` (line 15), `TOKENIZER_FILE` (line 16), `MODEL_BLAKE3` (line 19), `TOKENIZER_BLAKE3` (line 20), `MODEL_DIM` (line 57), `EMBEDDING_DIM` in `lib.rs:90`, `MODEL_NAME` in `helpers.rs:18`. Swapping to a different model (e.g., `e5-large-v2` for 1024-dim) requires changing at least 7 constants, updating the HNSW index dimension check, and recompiling. There's no runtime model selection or config-driven model choice.
- **Suggested fix:** For now, this is by design (single model simplifies the system). If model swapping becomes a requirement, extract model constants into a `ModelConfig` struct loadable from config. The Store already validates model name and dimensions on open, so the persistence layer is prepared.

#### HNSW tuning parameters are compile-time constants — no runtime configuration
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:58-65`
- **Description:** `MAX_NB_CONNECTION` (M=24), `MAX_LAYER` (16), `EF_CONSTRUCTION` (200), and `EF_SEARCH` (100) are all `const`. The comments note that different workloads would benefit from different values (M=16 for small codebases, M=32 for large). Users cannot tune these without recompiling. In particular, `EF_SEARCH` is the main accuracy/speed knob that users might want to adjust at query time — higher values give better recall at the cost of latency.
- **Suggested fix:** Make `EF_SEARCH` configurable via `SearchFilter` or a new `IndexConfig`. Keep `M`, `MAX_LAYER`, and `EF_CONSTRUCTION` as compile-time constants since they affect the index structure (changing them requires rebuild anyway).

#### Chunk size limits (100 lines, 100KB) are hardcoded in parser with no override
- **Difficulty:** easy
- **Location:** `src/parser.rs:163-180`
- **Description:** Functions over 100 lines or 100KB are silently skipped during indexing. These limits are sensible defaults but not configurable. A project with legitimate large generated functions (e.g., parser tables, protocol buffers) would have them silently excluded. The user has no way to know chunks were skipped (only `tracing::debug`) or to override the limit.
- **Suggested fix:** Move limits to `Config` with defaults matching current values. Consider adding a `--max-chunk-lines` CLI flag. At minimum, surface the skip count in indexing stats.

#### `ChunkType` enum is closed — adding a new type requires core enum change
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:70-88`
- **Description:** `ChunkType` has 8 variants (Function, Method, Class, Struct, Enum, Trait, Interface, Constant). Adding a new type (e.g., `TypeAlias`, `Macro`, `Module`) requires modifying the core enum plus its `Display`, `FromStr`, and the type_map in every language definition that uses it. The type_map in each language file maps tree-sitter capture names to `ChunkType`, so new types need both an enum variant and capture names in queries.
- **Suggested fix:** This is acceptable for the current scope (8 types covers most use cases). If more types are needed, consider a string-based chunk type with validation, or use `strum` for auto-derived `Display`/`FromStr`.

#### `SignatureStyle` only supports 2 strategies — no custom extraction
- **Difficulty:** easy
- **Location:** `src/language/mod.rs:60-67`, `src/parser.rs:275-276`
- **Description:** `SignatureStyle` has `UntilBrace` and `UntilColon`. The parser match at `parser.rs:275` handles only these two. A language needing a different signature boundary (e.g., Ruby's `end` keyword, Haskell's `=` sign, or SQL's `AS BEGIN`) would require adding a new variant plus a match arm in the parser. The tight coupling between enum variant and parser logic means custom extraction strategies can't be plugged in without modifying core code.
- **Suggested fix:** Change `SignatureStyle` to accept a closure or char/string pattern: `SignatureStyle::UntilChar(char)` or `SignatureStyle::Custom(fn(&str) -> usize)`. This lets new languages define their own boundary without touching the parser match.

#### `Embedder` max_length (512 tokens) is hardcoded with no override
- **Difficulty:** easy
- **Location:** `src/embedder.rs:251,272`
- **Description:** The token limit `max_length: 512` is set in both `Embedder::new()` and `Embedder::new_cpu()`. E5-base-v2 supports 512 tokens, so this is correct for the current model. However, there's no way to override it (e.g., for a model that supports 1024 or 2048 tokens). The value is hardcoded rather than derived from model metadata or config.
- **Suggested fix:** Low priority — this only matters if the model is swapped. If model configurability is added (see finding #3), include `max_length` in the model config.

#### No plugin or extension points — all capabilities are compiled in
- **Difficulty:** hard
- **Location:** Project-wide
- **Description:** All languages, search modes, MCP tools, and storage backends are compiled into the binary. There's no mechanism for: (1) loading language definitions at runtime (e.g., from a TOML/YAML language spec), (2) registering custom MCP tools from external crates, (3) plugging in alternative storage backends (the schema.sql supports `source_type` field, suggesting multi-source was planned, but Store only handles files). The `VectorIndex` trait (index.rs) is the one extensibility point — it allows HNSW/CAGRA/mock backends. Everything else is closed.
- **Suggested fix:** This is a design choice that trades extensibility for simplicity and performance. Runtime language loading would require dynamic tree-sitter grammar loading (possible but complex). For MCP tools, a trait-based registration system (finding #2) would be a first step. For storage, the `source_type` column in schema.sql already exists — implementing additional source types (e.g., SQL Server procedures) would work within the existing schema.

## Batch 2: Observability

#### 1. MCP `handle_request` does not log request method or ID
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:155-189`
- **Description:** `handle_request` processes JSON-RPC requests but never logs which method was called or the request ID. The only log is at debug level on error (line 175). Successful requests produce no log at the handler level. The tool dispatch in `tools/mod.rs:233` logs tool name and timing, but non-tool methods (`initialize`, `initialized`, `tools/list`) produce no trace at all. Diagnosing "why did the server not respond" is impossible without request-level logging.
- **Suggested fix:** Add `tracing::debug!(method = %request.method, id = ?request.id, "MCP request received")` at the top of `handle_request`, and log the outcome (success/error) with the same fields.

#### 2. `search_filtered` brute-force path produces no timing or result count log
- **Difficulty:** easy
- **Location:** `src/search.rs:196-365`
- **Description:** `search_filtered` has a tracing span (line 203) but never logs the result count, search duration, or which search strategy was used (semantic-only vs hybrid vs RRF). The MCP search tool logs timing (search.rs:99-104), but CLI `cmd_query` calls `search_filtered` directly and gets no timing info at all. Compare to `search_filtered_with_index` which at least logs candidate counts (line 387).
- **Suggested fix:** Add `tracing::info!(results = results.len(), rows_scanned = rows.len(), rrf = use_rrf, "search_filtered completed")` before returning.

#### 3. Embedder model loading and provider selection have no structured logging
- **Difficulty:** easy
- **Location:** `src/embedder.rs:233-255` and `src/embedder.rs:686-710`
- **Description:** `select_provider` / `detect_provider` determine whether CUDA, TensorRT, or CPU is used for inference, but never log the result. The `Embedder::new()` constructor also produces no log. Users debugging slow embedding have no way to confirm which provider is active without instrumenting the code. The first visible log is from `embed_batch` span (line 441), which occurs only when embedding actually happens.
- **Suggested fix:** Add `tracing::info!(provider = %provider, "Execution provider selected")` in `detect_provider`, and `tracing::info!(provider = %self.provider, batch_size = self.batch_size, "Embedder initialized")` after construction.

#### 4. Watch mode file changes logged to stdout but not to tracing
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:130-135`
- **Description:** When files change, watch mode prints to stdout (`println!`) but does not emit tracing events. The stdout output is ephemeral — if watch runs as a background service or with `--quiet`, file change activity is lost. The `reindex_files` function (line 215) has a proper tracing span, but the file change detection loop (lines 87-201) has no tracing at all. The debounce logic, pending file counts, and note change detection are invisible to structured logging.
- **Suggested fix:** Add `tracing::info!(files = files.len(), "Processing file changes")` before reindexing, and `tracing::debug!(pending = pending_files.len(), "File change detected")` in the event handler.

#### 5. `search_by_candidate_ids` has no tracing span or timing
- **Difficulty:** easy
- **Location:** `src/search.rs:397-497`
- **Description:** `search_by_candidate_ids` is the core HNSW-guided search path, called by both `search_filtered_with_index` and `search_unified_with_index`. It has no tracing span, no timing, and no result count log. The parent `search_filtered_with_index` logs candidate count at debug level (line 387), but the actual scoring, filtering, and deduplication within `search_by_candidate_ids` is invisible. If candidate scoring is slow or dedup removes many results, there is no way to diagnose it.
- **Suggested fix:** Add `tracing::info_span!("search_by_candidate_ids", candidates = candidate_ids.len(), limit = limit)` and log result count before returning.

#### 6. Pipeline thread panics produce generic error with no thread identity
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:584-592`
- **Description:** When pipeline threads panic, the join error is mapped to a generic message like "Parser thread panicked" (line 586). The original panic message (which could contain the root cause) is discarded — `std::thread::JoinHandle::join()` returns the panic payload as `Box<dyn Any>`, but the code throws it away with `map_err(|_|...)`. The GPU and CPU embedder thread panic messages are equally uninformative.
- **Suggested fix:** Extract the panic message: `map_err(|e| anyhow::anyhow!("Parser thread panicked: {:?}", e.downcast_ref::<&str>().unwrap_or(&"unknown")))`.

#### 7. `load_references` does not log which references were loaded or their weights
- **Difficulty:** easy
- **Location:** `src/reference.rs:36-69`
- **Description:** On success, `load_references` logs "Loaded N reference indexes" (line 65) but not which ones, their weights, or chunk counts. When references are empty (no config), nothing is logged at all. A user with misconfigured reference names has no diagnostic info — they'd need to add a stats call to see whether their references loaded.
- **Suggested fix:** Log each successfully loaded reference: `tracing::info!(name = %cfg.name, weight = cfg.weight, hnsw = index.is_some(), "Loaded reference index")`.

#### 8. MCP tool call does not log success vs failure status
- **Difficulty:** easy
- **Location:** `src/mcp/tools/mod.rs:232-258`
- **Description:** `handle_tools_call` logs tool name and elapsed time (lines 252-256) regardless of whether the tool succeeded or failed. The log message "MCP tool call completed" is identical for success and error cases. On error, the actual error message is not in the tool-call log — it only appears later in `handle_request` at debug level (server.rs:175). Correlating a slow tool call with its error requires matching timestamps manually.
- **Suggested fix:** Log `success = result.is_ok()` in the existing info log, and on error also log `error = %e` at warn level.

#### 9. `Store::open` does not log connection pool configuration
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:101-179`
- **Description:** `Store::open` logs "Database connected" (line 169) but not the pool configuration: max connections (4), idle timeout (300s), busy timeout (5000ms), mmap size (256MB). These are performance-critical settings that differ from SQLite defaults. When diagnosing connection pool exhaustion or SQLITE_BUSY errors, the first question is "what are the pool settings?" — currently unanswerable from logs.
- **Suggested fix:** Add `tracing::debug!(max_connections = 4, idle_timeout_s = 300, busy_timeout_ms = 5000, "SQLite pool configured")`.

#### 10. `reindex_files` in watch mode does not log which files failed to parse
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:220-240`
- **Description:** When `parser.parse_file` fails in watch mode (line 236), it logs a warning per file. However, the outer function `reindex_files` returns `Ok(chunks.len())` even if all files failed to parse. The caller (line 148) prints "Indexed 0 chunk(s)" with no indication that files were attempted and failed. There is no aggregate failure count in the return value.
- **Suggested fix:** Return a struct with both `indexed_count` and `parse_failures` count, or log a summary like `tracing::warn!(failed = failed_count, succeeded = chunks.len(), "Reindex completed with failures")`.

## Batch 2: Panic Paths

#### 1. `embedding_to_bytes` panics on dimension mismatch via `assert_eq!`
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:442-448`
- **Description:** `embedding_to_bytes` uses `assert_eq!(embedding.len(), EXPECTED_DIMENSIONS as usize, ...)` which panics at runtime if the embedding has the wrong number of dimensions. This is called from `store/chunks.rs:50` and `store/notes.rs:84,211` during database writes. While the docstring documents this panic ("# Panics"), it's the only defense — `Embedding::new()` accepts any `Vec<f32>` without validation (see Batch 1 Code Hygiene finding). A bug in the embedder returning 768 instead of 769 dims would crash the indexing pipeline mid-transaction.
- **Suggested fix:** Return `Result<Vec<u8>, StoreError>` instead of panicking. Or enforce dimension validation in `Embedding::new()` so this assert is truly redundant.

#### 2. `Language::def()` panics with `expect("language not in registry")` in production code
- **Difficulty:** medium
- **Location:** `src/language/mod.rs:167`
- **Description:** `Language::def()` calls `REGISTRY.get(&self.to_string()).expect("language not in registry — check feature flags")`. This is production code called by the parser during indexing (e.g., `language.def().tree_sitter_language`). If a language enum variant exists but its feature flag is disabled at compile time, this panics. Currently all language features are in the default set, but adding a new language without the feature flag or building with `--no-default-features` would hit this.
- **Suggested fix:** Return `Option<&LanguageDef>` and handle the `None` case gracefully, or document the panic contract more visibly.

#### 3. `Parser::new()` uses `expect("registry/enum mismatch")` during initialization
- **Difficulty:** easy
- **Location:** `src/parser.rs:61`
- **Description:** `Parser::new()` iterates the registry and calls `def.name.parse().expect("registry/enum mismatch")` for each language definition. This panic would fire if a registry entry has a name that doesn't parse to a `Language` enum variant — a developer error. Since `Parser::new()` is called early in the CLI and MCP startup, this would crash the entire process.
- **Suggested fix:** Map to `ParserError` and propagate via `?`. The function already returns `Result<Self, ParserError>`.

#### 4. `sanitize_error_message` uses `expect("hardcoded regex")` for LazyLock regex compilation
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:198,202` and `src/nl.rs:21,23`
- **Description:** Four `LazyLock<Regex>` statics use `.expect("hardcoded regex")` or `.expect("valid regex")`. These are hardcoded patterns, so the expect should never fire — but if it did, it would crash the MCP server on the first sanitize call or crash during NL generation.
- **Suggested fix:** Acceptable risk — hardcoded regex patterns are compile-time invariants. Could add unit tests that force LazyLock initialization to catch regex syntax errors at test time rather than runtime.

#### 5. `unreachable!` in `mcp/tools/search.rs:208` — relies on implicit invariant
- **Difficulty:** medium
- **Location:** `src/mcp/tools/search.rs:208`
- **Description:** `UnifiedResult::Note(_) => unreachable!("name_only search doesn't return notes")` would panic if `search_by_name` or `search_reference_by_name` ever returned note results. Currently this is correct — `search_by_name` only queries the chunks table. But the invariant is implicit and not enforced by the type system. If someone adds note name-search support, this panics at runtime in the MCP server.
- **Suggested fix:** Replace with a graceful skip or return an error. `unreachable!` should be reserved for truly impossible branches enforced by the type system.

#### 6. `unreachable!` in `mcp/tools/audit.rs:43` — guarded by early return but fragile
- **Difficulty:** easy
- **Location:** `src/mcp/tools/audit.rs:43`
- **Description:** `unreachable!("enabled checked above")` after an early return that checks `args.enabled.is_none()`. The `let Some(enabled) = args.enabled else { unreachable!(...) }` is logically safe but depends on the early return 23 lines above not being refactored away.
- **Suggested fix:** Replace with `let Some(enabled) = args.enabled else { return Err(anyhow::anyhow!("enabled is required")); }`. Defensive coding is cheap here.

#### 7. `search.rs:316` — `unwrap()` guarded by conditional but fragile coupling
- **Difficulty:** easy
- **Location:** `src/search.rs:316`
- **Description:** `normalized_query.as_ref().unwrap()` is inside an `if use_rrf` block, and `normalized_query` is set to `Some(...)` only when `use_rrf` is true (line 308). The unwrap is currently safe, but the invariant is maintained by two separate code blocks 8 lines apart. Restructuring either block would cause a panic during search.
- **Suggested fix:** Inline the normalization or use `expect("normalized_query is Some when use_rrf is true")` for a better panic message.

#### 8. `parser.rs:235-236` — tree-sitter row index cast `as u32` with `+ 1` overflow potential
- **Difficulty:** easy
- **Location:** `src/parser.rs:235-236` (also lines 395, 512, 524)
- **Description:** `node.start_position().row as u32 + 1` casts tree-sitter's `usize` row index to `u32`. If `row` exceeds `u32::MAX` (impossible for real files but technically a truncation), the value wraps. The `+ 1` could also overflow if `row` is exactly `u32::MAX - 1` after truncation. Same pattern repeated in 4 locations.
- **Suggested fix:** Very low practical risk. Could use `u32::try_from(row).unwrap_or(u32::MAX)` for defensive completeness.

#### 9. `cagra.rs:379` — `u64` to `usize` cast truncates on 32-bit platforms
- **Difficulty:** easy
- **Location:** `src/cagra.rs:379` (also `cli/commands/index.rs:231`)
- **Description:** `store.chunk_count()` returns `u64`, cast to `usize` for Vec capacity. On 32-bit platforms (where `usize` is 32 bits), this silently truncates if chunk_count exceeds 4.3 billion. The project targets 64-bit (GPU requirement), making this theoretical.
- **Suggested fix:** Low risk on 64-bit. Use `usize::try_from(count).map_err(...)` if cross-platform support matters.

#### 10. HNSW `unsafe` blocks — transmute lifetime extension with manual drop ordering
- **Difficulty:** hard
- **Location:** `src/hnsw.rs:680-706`
- **Description:** The HNSW load path uses `unsafe { std::mem::transmute(hnsw) }` to extend a borrow lifetime from `'_` to `'static` (line 692). Safety relies on `LoadedHnsw` drop order ensuring `HnswIo` outlives `Hnsw`. While the safety comments are thorough and the id_map size validation exists (line 696), there is no validation that loaded HNSW data has internally consistent neighbor indices. The `unsafe impl Send` and `unsafe impl Sync` (lines 186-187) rely on external `RwLock` synchronization — if someone accesses `LoadedHnsw` without the lock, data races are possible.
- **Suggested fix:** Highest-risk area in the codebase. Consider adding a post-load validation step that spot-checks neighbor list indices are in-bounds. Wrap the unsafe in a dedicated `load_hnsw_with_lifetime_extension` function with the safety proof in its doc comment. Reference the specific `RwLock` in the `Send`/`Sync` safety comments.

## Batch 2: Test Coverage

Total test count: 420 functions across 24 test modules (unit + integration).

#### 1. `search_reference` and `search_reference_by_name` have no unit or integration tests
- **Difficulty:** medium
- **Location:** `src/reference.rs:72-119`
- **Description:** The two core reference search functions are untested. `merge_results` has 7 tests and `load_references` has 1 test, but neither `search_reference` nor `search_reference_by_name` has any test coverage. These functions apply weight multipliers and threshold filters — weight application logic (multiply then filter vs filter then multiply) and the error-to-empty-vec degradation are not verified. The only callers are in `src/cli/commands/query.rs:120` and `src/mcp/tools/search.rs:115,184`, both of which are hard to exercise in integration tests without a real reference index on disk.
- **Suggested fix:** Add unit tests using a test Store with known embeddings. Test: weight multiplication correctness, threshold filtering after weighting, error degradation returning empty vec, name search scoring with weight applied.

#### 2. `BoundedScoreHeap` has no unit tests (existing #239)
- **Difficulty:** easy
- **Location:** `src/search.rs:123-182`
- **Description:** `BoundedScoreHeap` is a private data structure used by `search_filtered` for memory-bounded result tracking. It has `new`, `push`, and `into_sorted_vec` methods with specific behavior: evict lowest when at capacity, handle equal scores, maintain sort order. None of these are directly tested. The struct is only exercised indirectly through `search_filtered` integration tests. Edge cases — push to empty heap, push when score equals minimum, NaN scores, capacity=0 — are not covered.
- **Suggested fix:** Add unit tests for `BoundedScoreHeap` directly: empty heap returns empty vec, at capacity evicts lowest, below threshold not inserted, equal scores handled, output is sorted descending.

#### 3. `NameMatcher` struct not tested — only `name_match_score` wrapper tested (existing #239)
- **Difficulty:** easy
- **Location:** `src/search.rs:23-109`
- **Description:** Tests exercise `name_match_score()` (the convenience wrapper that creates a new `NameMatcher` per call), but `NameMatcher::new()` + `NameMatcher::score()` are never tested directly. The whole point of `NameMatcher` is to pre-tokenize the query for repeated use — this optimization path is untested. If `new()` incorrectly tokenizes, the bug wouldn't be caught because tests use the wrapper. Edge cases like empty query, single-char query, unicode identifiers, and CamelCase tokenization are not covered.
- **Suggested fix:** Test `NameMatcher::new("parseConfig").score("config_parser")` directly. Test reuse: create once, call `score()` multiple times, verify consistency. Test edge cases: empty string, single character, unicode.

#### 4. No integration tests for `ref` CLI commands
- **Difficulty:** medium
- **Location:** `src/cli/commands/reference.rs`
- **Description:** `cli_test.rs` has 19 integration tests covering `init`, `index`, `search`, `stats`, `doctor`, `callers`, `callees`, and `completions`. But `ref add`, `ref list`, `ref remove`, and `ref update` have zero integration tests. These are new commands (v0.5.3) that exercise the full path: CLI argument parsing -> config file write -> reference loading -> multi-index search. The only tests are unit tests in `config.rs` (config file read/write) and `reference.rs` (merge_results). The end-to-end path from "user runs `cqs ref add`" to "config is persisted" is untested.
- **Suggested fix:** Add integration tests in `cli_test.rs`: `test_ref_add_creates_config`, `test_ref_list_shows_references`, `test_ref_remove_deletes_config`. Use tempdir with a minimal indexed reference.

#### 5. `Store::search()` convenience method is only tested indirectly
- **Difficulty:** easy
- **Location:** `src/search.rs:186-193`
- **Description:** `Store::search(query, limit, threshold)` delegates to `search_filtered` with a default filter. No test calls `store.search()` directly — all search tests use `search_filtered`, `search_filtered_with_index`, or `search_unified_with_index`. While this is a trivial wrapper, it is a public API entry point documented in the lib.rs Quick Start example. A test confirming it delegates correctly would prevent accidental breakage.
- **Suggested fix:** Add one test calling `store.search(query, 5, 0.3)` directly and asserting it returns results.

#### 6. `store/notes.rs` lifecycle functions untested: `notes_need_reindex`, `replace_notes_for_file`, `delete_notes_by_file`
- **Difficulty:** medium
- **Location:** `src/store/notes.rs:169-287`
- **Description:** `store_notes_test.rs` has 4 tests covering `note_embeddings` and `note_stats`. The note write path (`replace_notes_for_file`) and delete path (`delete_notes_by_file`) have no integration tests. `notes_need_reindex` — which checks file mtimes to decide if notes need re-embedding — has no tests either. These are used by `lib.rs:index_notes` and `cli/watch.rs`, and bugs in the mtime comparison or FTS sync would cause notes to never reindex or to reindex every time.
- **Suggested fix:** Add tests: `test_replace_notes_updates_existing`, `test_delete_notes_by_file_removes_all`, `test_notes_need_reindex_returns_none_when_fresh`.

#### 7. `Embedding::try_new` error case not tested
- **Difficulty:** easy
- **Location:** `src/embedder.rs:106-114`
- **Description:** `Embedding::try_new()` validates that the input has exactly 769 dimensions and returns `EmbeddingDimensionError` on mismatch. While the docstring has a doctest showing the error case, there is no dedicated unit test for: wrong dimension returning error, correct dimension returning Ok, boundary dimensions (768, 770, 0). The doc example uses `assert!(invalid.is_err())` without checking the error contents.
- **Suggested fix:** Add unit tests: `try_new` with 769 dims succeeds, with 768 fails with correct `actual`/`expected` fields, with 0 fails, with u16::MAX fails.

#### 8. `SearchFilter::validate()` does not test NaN and infinity inputs
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:326-376`
- **Description:** `validate()` uses `!(0.0..=1.0).contains()` which correctly rejects NaN (because NaN is not contained in any range). There are tests for negative values and values > 1.0, but no test confirms NaN is rejected. Similarly, `f32::INFINITY` and `f32::NEG_INFINITY` are not tested. The correctness of NaN handling relies on `contains()` behavior which is non-obvious — a test would document this intentional behavior.
- **Suggested fix:** Add tests: `validate` with `name_boost = f32::NAN`, `note_weight = f32::INFINITY`, `name_boost = f32::NEG_INFINITY`. All should return `Err`.

#### 9. `search_unified_with_index` note slot allocation logic untested for edge cases (existing #239)
- **Difficulty:** medium
- **Location:** `src/search.rs:552-556`
- **Description:** The note slot allocation formula (`min_code_slots = (limit * 3) / 5`, `note_slots = limit - reserved_code`) determines how many note results are returned vs code results. No test verifies behavior when: all results are notes (0 code results), limit is 1, limit is very large, code results exceed limit. The formula has integer division which could produce unexpected results at small limits (e.g., limit=1: `min_code_slots=0`, so notes could take the only slot). Only `test_search_unified_with_index_returns_both` and `test_search_unified_note_weight_zero_excludes_notes` exist, and they use limit=10 with simple cases.
- **Suggested fix:** Add tests with limit=1, limit=2, limit=100 with varying code/note result counts to verify slot allocation edge cases.

#### 10. `Store::close()` has no test verifying WAL checkpoint behavior
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:537-547`
- **Description:** `Store::close()` runs a WAL checkpoint (TRUNCATE mode) before closing the pool. No test verifies this works — that after close(), the WAL file is actually cleaned up, or that subsequent opens don't see stale WAL data. The `Drop` implementation also attempts a checkpoint but failures are silently ignored. There's no test confirming the Drop path doesn't panic or leave the database in an inconsistent state.
- **Suggested fix:** Add test: open store, write data, call `close()`, verify WAL file is truncated or absent, reopen and verify data is intact.

## Batch 2: Algorithm Correctness

#### 1. Glob filter extracts wrong file path from chunk ID in brute-force search
- **Difficulty:** medium
- **Location:** `src/search.rs:293-294`
- **Description:** The glob path extraction uses `id.rfind(':').map(|i| &id[..i])` to strip the file path from chunk IDs. The comment says the format is `path:start-end`, but the actual format (set in `cli/pipeline.rs:247`) is `path:line_start:hash_prefix`. With `rfind(':')`, for ID `src/foo.rs:42:abcd1234`, the result is `src/foo.rs:42` — which includes the line number. A glob pattern like `src/**/*.rs` will NOT match `src/foo.rs:42`. For windowed chunks (`src/foo.rs:42:abcd1234:w0`), rfind gives `src/foo.rs:42:abcd1234` — even worse. Meanwhile, `search_by_candidate_ids` (line 450) correctly uses `chunk_row.origin` for glob matching. This means glob filtering works with HNSW-guided search but is broken for brute-force search.
- **Suggested fix:** The path has exactly two colons after the file path portion (`line_start:hash`). Either: (a) extract by finding the second-to-last colon, (b) store the origin path separately during scoring, or (c) use the same `chunk_row.origin` approach as `search_by_candidate_ids` by fetching origin data in phase 1.

#### 2. Pipeline assigns wrong file_mtime to chunks from different files in same batch
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:299-304`
- **Description:** All chunks in a parsed batch share a single `file_mtime`, taken from the first chunk's file. But chunks come from multiple files parsed in parallel (`par_iter` over `file_batch`). After `rayon::par_iter` collects all chunks, they're batched by `batch_size=32`. A batch of 32 chunks may span many source files, yet all get the first file's mtime. This corrupts the `source_mtime` column in the database, causing `needs_reindex` (which compares file mtime against stored mtime) to make wrong decisions: files with newer mtimes than the stored value won't be re-indexed, and files with older mtimes may be unnecessarily re-indexed.
- **Suggested fix:** Track per-chunk mtime. The `file_mtimes` HashMap already maps file paths to mtimes. Either: (a) change `ParsedBatch` to carry per-chunk mtimes, or (b) change `EmbeddedBatch` and `upsert_chunks_batch` to accept per-chunk mtimes instead of a single value.

#### 3. Dedup after RRF/truncation can return fewer results than limit without backfill
- **Difficulty:** medium
- **Location:** `src/search.rs:314-361`
- **Description:** In `search_filtered`, the RRF fusion (or non-RRF truncation) produces `final_scored` capped at `limit`. Parent-based dedup then removes duplicate windows from this already-limited set. If the top 5 results (limit=5) are all windows of the same function, dedup removes 4, returning only 1 result — even though positions 6-10 might have had unique functions. The `search_by_candidate_ids` path partially mitigates this by over-fetching candidates (5x limit from HNSW), but the brute-force path has no over-fetch before dedup.
- **Suggested fix:** Over-fetch before dedup in the brute-force path: use `limit * 2` (or similar) for the initial scoring/RRF phase, then dedup, then truncate to `limit`. The `semantic_limit = limit * 3` when RRF is enabled helps but only covers the semantic input to RRF, not the final output.

#### 4. Reference semantic search applies threshold BEFORE weight, inconsistent with name search
- **Difficulty:** easy
- **Location:** `src/reference.rs:72-97` vs `src/reference.rs:100-119`
- **Description:** `search_reference` (semantic) passes the caller's `threshold` into `search_filtered_with_index`, which applies it to RAW scores. Weight is then applied after, so a result with raw score 0.35 and weight 0.8 passes threshold=0.3 but has effective score 0.28 — below threshold. `search_reference_by_name` correctly checks `r.score * ref_idx.weight >= threshold` (the weighted score). This inconsistency means semantic reference search can return results below the intended threshold.
- **Suggested fix:** In `search_reference`, pass `threshold / ref_idx.weight` to the inner search (adjusting for weight), or apply threshold after weighting like `search_reference_by_name` does. Alternatively, pass threshold=0.0 to the inner search and filter afterward.

#### 5. BoundedScoreHeap tie-breaking is first-come-first-served, arbitrary by DB row order
- **Difficulty:** easy
- **Location:** `src/search.rs:163-169`
- **Description:** When the heap is at capacity and a new item has `score == min_score`, it is NOT inserted (the comparison is strict `>`). Tie-breaking is determined by database iteration order (insertion order), which is arbitrary. For threshold-boundary scores where many chunks cluster near the same similarity, this systematically excludes later-indexed files. Exact f32 ties are rare for cosine similarity, so practical impact is low, but the policy is undocumented.
- **Suggested fix:** Document the tie-breaking policy with a comment. If deterministic tie-breaking matters, add chunk ID as a secondary sort key.

#### 6. Unified search note_slots soft cap does not enforce 60% code guarantee
- **Difficulty:** easy
- **Location:** `src/search.rs:552-579`
- **Description:** The note_slots calculation intends to reserve 60% of slots for code (`min_code_slots = (limit * 3) / 5`). All code results (up to `limit`) are added to the unified list, but notes are pre-limited to `note_slots`. After sort+truncate to `limit`, if notes have higher scores than code, notes can push code results below the 60% minimum. The pre-limiting provides a soft cap but not a hard guarantee. The intent and implementation diverge.
- **Suggested fix:** If the 60% guarantee is important, enforce it during final truncation. If the current soft-preference behavior is intentional, document it as such.

#### 7. `cosine_similarity` returns dot product without validating input normalization
- **Difficulty:** easy
- **Location:** `src/math.rs:12-29`
- **Description:** The function computes a dot product but is named `cosine_similarity`. The doc comment correctly states "for L2-normalized vectors," but callers use it without normalization checks. If un-normalized embeddings enter the system (model change, embedding corruption, sentiment dimension out of expected range), the dot product returns values outside [-1, 1], silently producing wrong similarity scores. The `is_finite()` check catches NaN/Inf but not out-of-range values.
- **Suggested fix:** Add a `debug_assert!` that the result is in [-1.01, 1.01] (with epsilon for float imprecision). This catches normalization bugs in development without runtime cost in release builds.

#### 8. `search_by_name` FTS query interpolates normalized text into FTS5 syntax
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:427`
- **Description:** The FTS query is built as `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. While `normalize_for_fts` currently strips non-alphanumeric characters, this creates a coupling: the safety of the FTS query depends on the implementation details of `normalize_for_fts`. If `normalize_for_fts` is ever changed to preserve certain characters (e.g., hyphens for kebab-case identifiers), the double-quoting strategy could break. The function mixes content normalization with query construction concerns.
- **Suggested fix:** Add explicit FTS-escaping (strip or escape double-quotes) at the query construction site, independent of `normalize_for_fts`.

#### 9. `extract_return_nl` for Java fails on generic return types with internal spaces
- **Difficulty:** easy
- **Location:** `src/nl.rs:575-598`
- **Description:** For Java, the code takes `words[words.len() - 2]` as the return type. This works for simple types (`int`, `String`) but fails for generic types with spaces: `Map<String, Integer>` is split by whitespace into `["Map<String,", "Integer>", ...]`, and `words[len-2]` picks up a partial generic like `"Integer>"`. The algorithm cannot handle multi-word return types, which occur with generics that have spaces after commas.
- **Suggested fix:** Accept the limitation with a doc comment noting generic return types may not parse correctly, matching the existing TypeScript caveat at line 524. Alternatively, join all words between the last modifier and the method name as the return type.

## Batch 3: Memory Management

#### 1. Brute-force search loads ALL embeddings into memory at once
- **Difficulty:** hard
- **Location:** `src/search.rs:237-242`
- **Description:** `search_filtered` executes `SELECT id, embedding FROM chunks` with `fetch_all`, loading every chunk's embedding into memory simultaneously. Each embedding is 769 * 4 = 3076 bytes. For 100k chunks, this is ~300MB of raw embedding data plus SQLite row overhead, string allocations for IDs, and Vec overhead — easily 500MB+. This is the fallback path when HNSW is unavailable or returns no candidates. The `BoundedScoreHeap` mitigates the result collection phase but not the initial load. With HNSW enabled this path rarely triggers, but when it does (new index, empty HNSW, language/path filter fallback), memory usage spikes to the full dataset.
- **Suggested fix:** Use cursor-based pagination (`LIMIT/OFFSET`) like `embedding_batches`, scoring each batch and keeping only the bounded heap. This would cap memory at O(batch_size + limit) instead of O(total_chunks).

#### 2. Pipeline channels can buffer up to 768 batches of chunks + embeddings in memory
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:192-199`
- **Description:** Three bounded channels each with `channel_depth = 256`: `parse_tx/rx` (ParsedBatch), `embed_tx/rx` (EmbeddedBatch), and `fail_tx/rx` (ParsedBatch). Each `ParsedBatch` contains up to 32 chunks (strings: id, content, signature, doc, etc.). Each `EmbeddedBatch` contains 32 `(Chunk, Embedding)` pairs — each embedding is ~3KB. In the worst case, all three channels are full: 256 * 32 * ~3KB (embeddings) + chunk strings ≈ 25MB for embeddings alone, plus chunk content (which can be large for code). The `embed_rx` channel is most concerning: 256 * 32 embedded chunks can hold ~8192 chunks with full embeddings in flight. If the writer thread stalls (slow SQLite), backpressure fills these buffers.
- **Suggested fix:** Reduce `channel_depth` for `embed_tx/rx` (carrying heavier payloads) or reduce to a single shared depth. Consider profiling actual utilization — 256 may be overkill for pipeline smoothing.

#### 3. `all_embeddings()` loads entire index into memory without size guard
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:488-522`
- **Description:** `all_embeddings()` calls `fetch_all` on all chunks, then collects into a `Vec<(String, Embedding)>`. For 100k chunks: 100k * (avg 50-char ID string + 769 * 4 bytes embedding) ≈ 350MB. The method has a doc warning ("Warning: This loads all embeddings into memory at once") and recommends `embedding_batches()`, but it is still called from `HnswIndex::build()` callers. No runtime size check prevents OOM — unlike `HnswIndex::load()` which has `MAX_ID_MAP_SIZE`.
- **Suggested fix:** Add a size check: query `COUNT(*)` first and warn/error if above a threshold (e.g., 50k). Or deprecate with `#[deprecated]` and force callers through `embedding_batches()`.

#### 4. CAGRA `build_from_store` accumulates full flat_data Vec without streaming to GPU
- **Difficulty:** medium
- **Location:** `src/cagra.rs:375-425`
- **Description:** `build_from_store` streams from SQLite in batches of 10k, but accumulates all data into `flat_data: Vec<f32>` and `id_map: Vec<String>` before calling `build_from_flat`. For 100k chunks: `flat_data` = 100k * 769 * 4 = ~307MB of f32 values. The comment says "cuVS requires all data upfront for GPU index building" — true, but the streaming from SQLite is misleading since it all ends up in one allocation. The Vec will reallocate as it grows since the initial capacity is based on `chunk_count` (correct), but a miscount would cause multiple reallocations each copying hundreds of MB.
- **Suggested fix:** Low priority since `with_capacity` is correctly sized. Document that streaming here only avoids double-buffering (SQLite rows + flat_data), not total memory. The real fix would require cuVS streaming API support.

#### 5. `count_vectors` deserializes entire JSON id_map file just to count entries
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:731-762`
- **Description:** `count_vectors()` reads the HNSW id map file into a `String`, then parses the entire JSON array into `Vec<String>`, just to return `.len()`. For a 50k-entry index, the id_map JSON file is ~2.5MB; the parsed Vec allocates ~50k Strings. This is used for `cqs stats` display and on every `cqs watch` cycle. The `MAX_ID_MAP_SIZE` guard (100MB) prevents extreme cases, but the normal case still allocates unnecessarily.
- **Suggested fix:** Count the number of commas in the JSON string (entries = commas + 1 for non-empty), or store the count as a separate metadata field in the checksum file.

#### 6. `extract_body_keywords` creates unbounded HashMap from entire function body
- **Difficulty:** easy
- **Location:** `src/nl.rs:606-745`
- **Description:** `extract_body_keywords` calls `tokenize_identifier(content)` on the full chunk content, then builds a `HashMap<String, usize>` of word frequencies. For a large function body (up to the parser's 100-line/100KB limit), this creates thousands of token strings and HashMap entries. The function is called per-chunk during NL description generation (pipeline.rs prepare_for_embedding), so for 10k chunks it runs 10k times. Only used when template is `BodyKeywords` or `Compact`, which are currently only test templates — so no production impact currently. But if the template is ever made default, this becomes a hot-path allocation.
- **Suggested fix:** Since only test templates use this, no immediate fix needed. If enabling in production, add a content length cap (e.g., first 2KB only) and use a pre-allocated HashMap with bounded capacity.

#### 7. `normalize_for_fts` builds intermediate strings per-token with no pre-allocation
- **Difficulty:** easy
- **Location:** `src/nl.rs:148-198`
- **Description:** `normalize_for_fts` builds the result string by repeatedly pushing tokens separated by spaces. The inner `tokenize_identifier_iter` creates a new `String` per token (via `std::mem::take`). For a 10KB function body, this might create 500+ small String allocations. The function is called 4 times per chunk upsert (name, signature, content, doc — see `chunks.rs:69-72`), so for an index of 10k chunks: ~40k calls, potentially millions of small String allocations. The `MAX_FTS_OUTPUT_LEN` cap (16KB) bounds output size but not allocation count.
- **Suggested fix:** Pre-allocate `result` with `String::with_capacity(text.len())` (output is roughly same size as input). The tokenize iterator could reuse a buffer instead of allocating per token.

#### 8. `id_map: Vec<String>` in HNSW duplicates chunk IDs already stored in SQLite
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:200-201`
- **Description:** `HnswIndex` stores a `Vec<String>` mapping internal indices to chunk IDs. For 50k chunks with average 40-char IDs: ~2MB on the heap, plus the JSON serialization on disk (~2.5MB). This is an in-memory copy of data that already exists in the chunks table. The id_map exists because hnsw_rs uses integer indices internally and the mapping must be fast (O(1) by index). But the strings are allocated on every load and live for the process lifetime (the HNSW index is held in an Arc).
- **Suggested fix:** Consider using interned strings or a string arena to reduce per-string allocation overhead. Or store a hash-based compact representation and look up full IDs from SQLite on result retrieval (trading memory for a DB query per search result).

#### 9. Pipeline `file_mtimes` HashMap grows unbounded within file_batch
- **Difficulty:** easy
- **Location:** `src/cli/pipeline.rs:260-261`
- **Description:** `file_mtimes: HashMap<PathBuf, i64>` is created per `file_batch` (currently 100,000 files per batch). For 100k files with average 50-char paths: ~5MB in HashMap entries. The HashMap is used to avoid double stat() calls, which is correct. However, `file_batch_size = 100_000` means ALL files are processed in a single batch (the comment says "all at once"), so the HashMap holds mtimes for every file. This HashMap is then dropped at the end of the batch loop iteration, creating a large single allocation that's held for the duration of parsing.
- **Suggested fix:** Low priority — 5MB for 100k files is modest. If memory-constrained, reduce `file_batch_size` or defer mtime lookup to the embedding stage.

#### 10. Search result cloning in `search_by_candidate_ids` allocates full content for scoring
- **Difficulty:** medium
- **Location:** `src/search.rs:434-474` and `src/store/chunks.rs:445-479`
- **Description:** `fetch_chunks_with_embeddings_by_ids_async` fetches full chunk rows (including `content`, `signature`, `doc`) for ALL candidate IDs, even though only the embedding is needed for scoring. With HNSW returning up to `(limit * 5).max(100)` candidates (search.rs:379), that's typically 25-100 full chunk rows loaded into memory for scoring. Each chunk's `content` field can be up to 100KB. The `ChunkRow` is then collected into a Vec for scoring, and after scoring only the top `limit` results are kept. For limit=5 with 100 candidates, 95 chunks' content strings are allocated and immediately discarded.
- **Suggested fix:** Split into two queries: (1) fetch only id + embedding for candidate scoring, (2) fetch full chunk rows only for the top-N results that pass scoring. This matches how `search_filtered` does it (phase 1: score with embeddings, phase 2: fetch content for top-N).

## Batch 3: Data Integrity

#### 1. `metadata.updated_at` is never written — stats show stale `created_at` value
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:204-228`, `src/store/chunks.rs:354-380`
- **Description:** The `init()` method writes `created_at` but never writes `updated_at` to the metadata table. The `stats()` method (chunks.rs:364-367) reads `updated_at` from metadata and falls back to `created_at` if missing. Since no code path ever writes an `updated_at` key to metadata, the stats always show the original creation time. Users querying `cqs stats` or the MCP `cqs_stats` tool see `last_indexed` as the index creation time, not the last update time. Incremental re-indexes (via `cqs index` or `cqs watch`) never update this timestamp.
- **Suggested fix:** Add `UPDATE metadata SET value = ?1 WHERE key = 'updated_at'` (or `INSERT OR REPLACE`) at the end of `upsert_chunks_batch` or in the pipeline completion path. Also insert `updated_at` in `init()`.

#### 2. Schema `init()` is not wrapped in a transaction — crash mid-init leaves partial schema
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:182-237`
- **Description:** `init()` executes each SQL statement from `schema.sql` individually in a loop, followed by 5 separate metadata INSERT statements. None of this is wrapped in a transaction. If the process crashes mid-way (e.g., after creating `chunks` table but before `notes` table), the database has a partial schema with no `schema_version` metadata. On next open, `check_schema_version` reads `schema_version` as 0 (missing) and may attempt migrations from v0 which also don't exist. The partially-created tables would then conflict with `CREATE TABLE IF NOT EXISTS` on retry (safe) but orphaned indexes from the first attempt may cause confusing errors.
- **Suggested fix:** Wrap the entire `init()` body in a single transaction: `let mut tx = self.pool.begin().await?;` ... `tx.commit().await?;`.

#### 3. Migration steps are not individually wrapped in transactions
- **Difficulty:** medium
- **Location:** `src/store/migrations.rs:29-54`
- **Description:** The `migrate()` function runs each step via `run_migration(pool, version, version+1)` then updates `schema_version` in metadata. But neither `run_migration` nor the individual steps use transactions. If a multi-step migration (e.g., v10->v12 via v10->v11 then v11->v12) crashes after completing step 1 but before updating the metadata version, the database has v11 schema but metadata still says v10. On next open, step v10->v11 runs again on an already-migrated schema. The `IF NOT EXISTS` guideline in the doc comment partially mitigates this, but not all migrations can be made idempotent (e.g., data transformations).
- **Suggested fix:** Wrap each `run_migration` call + its `schema_version` update in a transaction, so the schema version always matches the actual schema state.

#### 4. HNSW index can be stale after `cqs watch` incremental updates (existing #236)
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:256-272`, `src/cli/commands/index.rs:143-154`
- **Description:** `cqs index` rebuilds the HNSW index after all database writes. But `cqs watch` only updates SQLite (delete old chunks, insert new chunks) without rebuilding HNSW. The HNSW index on disk still contains the old chunk IDs and embeddings. Search via `search_filtered_with_index` uses the stale HNSW to get candidate IDs, then hydrates from SQLite. If a chunk ID from HNSW no longer exists in SQLite (deleted file), it silently gets no results for that candidate. If a chunk was re-embedded with different content, HNSW returns the old similarity score but SQLite returns the new content — the displayed score doesn't match the actual similarity. Overlap with existing #236 but with specific stale-score detail.
- **Suggested fix:** Either rebuild HNSW in watch mode after batched updates (expensive but correct), or add staleness detection (compare HNSW vector count vs SQLite chunk count on search, warn if diverged).

#### 5. `ref update` does not prune deleted files from reference index
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:219-275`
- **Description:** `cmd_ref_update` runs the indexing pipeline and rebuilds HNSW, but never calls `store.prune_missing()`. Compare with `cmd_index` (index.rs:86-87) which explicitly collects `existing_files` and calls `store.prune_missing(&existing_files)`. If files are deleted from the reference source directory, their chunks persist in the reference index database. These stale chunks appear in search results, pointing to files that no longer exist in the source.
- **Suggested fix:** Add `store.prune_missing(&existing_files)` to `cmd_ref_update` after the pipeline runs, matching the pattern in `cmd_index`.

#### 6. Config `add_reference_to_config` does not check for duplicate names
- **Difficulty:** easy
- **Location:** `src/config.rs:180-207`
- **Description:** `add_reference_to_config` appends a new `[[reference]]` entry to the config file unconditionally. While `cmd_ref_add` (reference.rs:68) checks for duplicates before calling this function, the library function itself has no guard. If called directly (e.g., from a future API or test), it can create multiple `[[reference]]` entries with the same name. On load, `Config::load` gets both entries in the `references` vec. The `override_with` merge (config.rs:127) replaces by name, so the second entry wins in a user+project merge, but within a single file both entries survive. `load_references` (reference.rs:36) then opens the same Store twice, wasting resources and returning duplicate results.
- **Suggested fix:** Check for existing reference with the same name in `add_reference_to_config` and return an error or update in place.

#### 7. Note ID collision risk when text is modified via `tool_update_note`
- **Difficulty:** easy
- **Location:** `src/note.rs:162-163`, `src/mcp/tools/notes.rs:215-237`
- **Description:** Note IDs are generated by hashing the note text: `blake3::hash(entry.text.as_bytes())`. When `tool_update_note` changes the text via `rewrite_notes_file`, the note gets a NEW ID (because text changed, so hash changed). The old note ID in the database is then orphaned — `replace_notes_for_file` deletes by source_file and re-inserts, so the old ID is cleaned up. But the HNSW index (if notes were included) would still reference the old ID. More importantly, any external system caching note IDs (e.g., an LLM conversation referencing `note:abc123`) will find the ID no longer exists after an update.
- **Suggested fix:** Document this behavior: note IDs are content-addressed and change when text changes. If stable IDs are needed, consider using a UUID or sequence-based ID instead of content hash.

#### 8. `rewrite_notes_file` atomic rename can fail cross-device on some filesystems
- **Difficulty:** easy
- **Location:** `src/note.rs:141`
- **Description:** `rewrite_notes_file` writes to `notes.toml.tmp` then does `std::fs::rename(&tmp_path, notes_path)`. The rename is atomic only on the same filesystem/mount. Since `tmp_path` uses `with_extension("toml.tmp")` on the same directory, this is normally fine. However, if `notes_path` is on a networked filesystem (NFS, SMB), the rename may not be atomic or may fail. On Windows WSL, cross-mount renames between the WSL filesystem and Windows filesystem fail.
- **Suggested fix:** Low risk in practice since temp file is in the same directory. Add a fallback: if rename fails, try copy+delete. Or document that notes.toml must be on a local filesystem.

#### 9. `watch` mode delete-then-insert is not atomic — crash between them loses chunks
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:256-272`
- **Description:** In `reindex_files`, the code first deletes old chunks for each file (`store.delete_by_origin(rel_path)?`) in a loop, then inserts new chunks one at a time (`store.upsert_chunk`). The deletes and inserts are separate operations, not wrapped in a single transaction. If the process crashes after deleting chunks for file A but before inserting new chunks, file A's data is lost from the index. The next `cqs watch` cycle would re-index file A (since its chunks are missing), so this is self-healing but temporarily loses search results. Compare with `replace_notes_for_file` (notes.rs:169) which correctly wraps delete+insert in a single transaction.
- **Suggested fix:** Wrap the delete+insert for each file in a transaction, or use `delete_by_origin` + `upsert_chunks_batch` within a single transaction scope.

#### 10. `prune_missing` uses per-batch transactions — cross-batch crash leaves partial prune
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:151-212`
- **Description:** `prune_missing` batches deletions in groups of 100, each in its own transaction. If the process crashes after committing batch 1 but before batch 2, some stale chunks are deleted and some remain. On next run, `prune_missing` re-checks all origins, so the remaining stale chunks are deleted. This is self-healing, but between the crash and next full index, search results include a mix of pruned and un-pruned stale files. The per-batch approach is necessary to stay within SQLite's parameter limit, but the inconsistency window exists.
- **Suggested fix:** Acceptable — the self-healing behavior makes this a minor concern. Could add a single outer transaction wrapping all batches if atomicity matters, but this risks longer lock hold times on large prunes.

## Batch 3: Concurrency Safety

#### 1. Notes file I/O has no locking — concurrent MCP requests can corrupt notes.toml (existing #231)
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:119-129` (add_note append), `src/note.rs:125-143` (rewrite_notes_file)
- **Description:** `tool_add_note` opens notes.toml in append mode and writes, then calls `reindex_notes` which re-parses the whole file. `tool_update_note` and `tool_remove_note` call `rewrite_notes_file` which reads the file, mutates in memory, and writes back (via temp file + rename). The MCP HTTP server handles requests concurrently (axum async handlers), so two simultaneous `cqs_add_note` calls can interleave appends, and a concurrent `cqs_add_note` + `cqs_update_note` can cause the update to overwrite the append (read-modify-write race). The atomic temp+rename in `rewrite_notes_file` prevents partial writes but not lost updates. Note: existing #231 tracks this.
- **Suggested fix:** Add a Mutex on the McpServer for notes file operations, or use file-level advisory locking (fs4) around all notes.toml I/O.

#### 2. MCP HTTP handler calls blocking `handle_request` on tokio async thread
- **Difficulty:** medium
- **Location:** `src/mcp/transports/http.rs:287`
- **Description:** `handle_mcp_post` is an axum async handler that calls `state.server.handle_request(request)` synchronously. `handle_request` can trigger `ensure_embedder()` which loads a 500MB ONNX model (blocking I/O + GPU init), and `embed_query` which does ONNX inference (CPU/GPU-bound, ~100ms). Store methods call `self.rt.block_on(async { ... })` which blocks the current thread waiting on a *different* tokio runtime. All of this happens on the axum tokio runtime's worker threads. Under concurrent load, blocking worker threads can starve the async executor — other HTTP connections stop progressing. With 4 concurrent search requests each embedding for ~100ms, all worker threads are blocked.
- **Suggested fix:** Wrap the `handle_request` call in `tokio::task::spawn_blocking(move || { ... })` so blocking work moves to the blocking thread pool. Alternatively, since the HTTP transport creates its own runtime (line 118), ensure it has enough threads.

#### 3. Store's internal tokio runtime called from HTTP handler's tokio runtime — nested runtime risk
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:97-99` (Store owns Runtime), `src/mcp/transports/http.rs:118` (serve_http creates Runtime)
- **Description:** Store has its own `tokio::runtime::Runtime` (used for sqlx async ops via `self.rt.block_on()`). The HTTP transport creates a second `tokio::runtime::Runtime`. When an HTTP request calls store methods, it calls `self.rt.block_on()` from within an axum handler running on the HTTP runtime. Calling `block_on` from within an async context panics on multi-threaded tokio runtimes if the current thread is a tokio worker. Currently this works because axum runs the handler function to completion on one thread and the store's runtime is separate, but this is fragile — if axum changes its execution model or if the handler is wrapped in `spawn_blocking`, the nested runtime call would panic.
- **Suggested fix:** Document the two-runtime design explicitly. Consider sharing a single runtime (pass the runtime handle to Store instead of creating a new one), or make Store methods truly async.

#### 4. `Embedder` session Mutex held during GPU/CPU inference — serializes all embedding requests
- **Difficulty:** easy
- **Location:** `src/embedder.rs:284-289`
- **Description:** `Embedder::session()` returns a `MutexGuard<Session>`, held for the entire `embed_batch` call (line 485-491: tokenize, prepare tensors, run inference, extract output). ONNX inference takes 10-100ms+ depending on batch size. All concurrent embedding requests are serialized through this single Mutex, meaning the MCP server can only embed one query at a time. For the HTTP transport with concurrent search requests, this becomes a bottleneck.
- **Suggested fix:** This is intentional — ONNX Runtime's `Session::run()` requires `&mut Session`. The serialization is correct for safety. To improve throughput, could create a pool of Sessions (one per CPU core), or document that concurrent search throughput is limited by single-threaded embedding.

#### 5. CAGRA search holds two Mutex locks simultaneously — potential deadlock risk
- **Difficulty:** medium
- **Location:** `src/cagra.rs:169-181`
- **Description:** `CagraIndex::search()` first locks `self.resources` (line 169), then locks `self.index` (line 176) while still holding the resources lock. If any other code path locked these in reverse order (index first, then resources), this would deadlock. Currently no code takes them in reverse — `rebuild_index_with_resources` requires `resources` but is called after `index.take()` releases the index lock, and `build` constructs both locks fresh. However, the dual-lock pattern is fragile and undocumented.
- **Suggested fix:** Document the lock ordering invariant: "Always acquire `resources` before `index`." Consider restructuring to use a single Mutex protecting both state, since they're always used together during search.

#### 6. Pipeline shares Store across 3 threads via Arc — concurrent `block_on` calls on same Runtime
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:211-215`
- **Description:** Three threads (parser, GPU embedder, CPU embedder) share the Store via `Arc<Store>`. Each calls Store methods like `get_embeddings_by_hashes` and `needs_reindex`, which internally call `self.rt.block_on()`. The Store owns a single `tokio::runtime::Runtime`. Multiple threads calling `block_on` on the same runtime is safe (the runtime is multi-threaded by default), but all three threads plus the main writer thread are competing for the runtime's thread pool and the SQLite connection pool (4 connections). Under heavy load with all threads hitting the DB simultaneously, they can exhaust the pool and wait on `busy_timeout` (5 seconds).
- **Suggested fix:** The current design works correctly — SQLite WAL mode handles concurrent reads and the pool busy_timeout prevents deadlocks. Consider increasing pool size from 4 to 8 to match the pipeline's parallelism, or document the 4-connection limit as intentional.

#### 7. `HnswIndex` marked Send+Sync via unsafe impl — thread safety depends on undocumented `hnsw_rs` internals
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:186-187`
- **Description:** `LoadedHnsw` has `unsafe impl Send` and `unsafe impl Sync` with a safety comment referencing "external synchronization (RwLock in HnswIndex)." However, `HnswIndex` itself has no RwLock — it's the `Arc<RwLock<Option<Box<dyn VectorIndex>>>>` in `McpServer` that provides synchronization. The HnswIndex could be used without a RwLock (e.g., in tests, in CLI code via `Arc::new(index)` in `hnsw.rs:1088`). The `hnsw_rs` library's `search_neighbours` takes `&self` and appears to be read-only, so concurrent reads are likely safe, but the safety proof references a lock that isn't part of the type. The `Hnsw` struct's thread safety depends on `hnsw_rs` internals that aren't documented.
- **Suggested fix:** Either add an internal RwLock to HnswIndex (making the unsafe impls provably sound), or update the safety comment to accurately describe the contract: "`Hnsw::search_neighbours` is read-only and safe for concurrent access. All mutation (build, insert) happens before sharing."

#### 8. `check_interrupted` flag is never reset — cannot reuse pipeline after interruption in same process
- **Difficulty:** easy
- **Location:** `src/cli/signal.rs:19` (static AtomicBool), `src/cli/pipeline.rs:228,337,470,546`
- **Description:** `INTERRUPTED` is a process-global `AtomicBool` that is set to `true` on Ctrl+C and never reset. If the pipeline is interrupted, any subsequent pipeline invocation in the same process would see `check_interrupted() == true` and immediately skip all work. Currently the CLI exits after pipeline completes, so this doesn't matter. But if the pipeline were reused (e.g., in a library context, in watch mode triggering reindex after interrupt), the stuck flag would silently prevent all future indexing.
- **Suggested fix:** Reset `INTERRUPTED` to `false` at the start of `run_index_pipeline`, or before each command execution in the CLI.

#### 9. `OnceLock<Embedder>` race in `ensure_embedder` — two threads may both create expensive Embedder
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:130-150`
- **Description:** `ensure_embedder` uses `OnceLock` for thread-safe lazy init. Two concurrent requests hitting `ensure_embedder` simultaneously will both miss the `get()` check (line 132), both create an Embedder (~500ms model load, ~500MB memory), and one's Embedder gets discarded by `set()`. This is documented as intentional (line 143: "another thread might have raced us, that's OK") and is correct — no data corruption. But creating two 500MB ONNX sessions simultaneously doubles peak memory and initialization time.
- **Suggested fix:** Acceptable as-is — the race is rare (only on first search requests arriving simultaneously) and self-healing. Could use `get_or_init` with a blocking initializer to serialize, but `OnceLock::get_or_try_init` is not stabilized.

#### 10. `acquire_index_lock` stale lock detection has TOCTOU race and unbounded recursion
- **Difficulty:** easy
- **Location:** `src/cli/files.rs:158-178`
- **Description:** When the file lock is held, the code reads the PID from the lock file (line 160), checks if the process exists (line 162), and if not, removes the lock file and retries (lines 165-168). Between reading the PID and removing the file, another process could have acquired the lock legitimately. The remove+retry would then interfere with the new lock holder. Additionally, the recursive retry (line 168: `return acquire_index_lock(cq_dir)`) has no depth guard — if two processes race on stale lock cleanup, both could call `acquire_index_lock` recursively without bound (in theory, though in practice the race window is tiny).
- **Suggested fix:** Low practical risk since this only triggers on stale locks from crashed processes. Add a retry counter to prevent infinite recursion. Could also re-read the PID after re-acquiring the file handle to confirm it's still stale before removing.

## Batch 3: Platform Behavior

#### 1. Watch mode uses `==` on paths without canonicalization — case sensitivity mismatch on /mnt/c
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:97`
- **Description:** The notes_path comparison `if path == notes_path` compares raw `PathBuf` values. On WSL's `/mnt/c/` filesystem (case-insensitive NTFS), the `notify` watcher may report paths with different casing than what was constructed (e.g., `docs/Notes.toml` vs `docs/notes.toml`). The raw `==` comparison is case-sensitive on Linux, so note changes could be silently missed on case-insensitive filesystems accessed via WSL. The same issue applies to the `path.starts_with(&cq_dir)` check at line 92, though this is less likely to trigger since `.cq` is always lowercase.
- **Suggested fix:** Canonicalize both paths before comparing, or compare via `to_string_lossy().to_lowercase()` on platforms where the FS is case-insensitive.

#### 2. SQLite `mmap_size` 256MB PRAGMA may not work on WSL /mnt/c/ paths
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:142`
- **Description:** The `PRAGMA mmap_size = 268435456` (256MB) requests memory-mapped I/O. On WSL2 accessing Windows filesystems via the 9P protocol (`/mnt/c/`), mmap behavior may be degraded or silently fall back to traditional I/O. This is not a correctness bug, but the performance benefit documented in the comment ("256MB memory-mapped I/O for faster reads") may not materialize when the database is on a Windows mount. No error will be returned — SQLite silently degrades. The `.cargo/config.toml` already routes build artifacts to Linux-native paths for performance, but the runtime database stays on /mnt/c/.
- **Suggested fix:** Document the limitation. Optionally detect /mnt/ paths and skip mmap_size, or recommend placing the `.cq/` directory on a native Linux path for best performance.

#### 3. `delete_by_origin` and watch mode path separator inconsistency
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:39,121`, `src/cli/watch.rs:231,258`
- **Description:** When storing `origin` paths, `upsert_chunks_batch` uses `chunk.file.to_string_lossy().into_owned()` which preserves platform path separators. Meanwhile, `filesystem.rs:132` explicitly normalizes to forward slashes: `rel_path.to_string_lossy().replace('\\', "/")`. Watch mode (`watch.rs:231`) sets `chunk.file = rel_path.clone()` without normalizing separators before passing to `upsert_chunk`. On a cross-platform scenario, `delete_by_origin` (chunks.rs:121) converts `origin.to_string_lossy()` and does exact string match in SQL. If the stored origin uses `/` (from initial index) but watch passes `\` (from a Windows path event), the DELETE misses and stale+new chunks coexist. In practice on WSL the path separator is always `/`, but the inconsistency between the two code paths is fragile.
- **Suggested fix:** Normalize path separators to `/` in `delete_by_origin` and `upsert_chunk`/`upsert_chunks_batch`, or ensure all callers normalize before storing.

#### 4. `find_project_root()` does not canonicalize — may return inconsistent paths
- **Difficulty:** easy
- **Location:** `src/cli/config.rs:10-42`
- **Description:** `find_project_root()` returns `current.to_path_buf()` without canonicalization. On WSL, `current_dir()` can return paths with or without symlink resolution depending on how the directory was reached. Other code paths like `enumerate_files` (files.rs:27) canonicalize the root. This means `strip_prefix(&root)` in different code paths may see inconsistent root paths. If the CWD is reached via a symlink, `find_project_root` returns the symlink path while `enumerate_files` works with the canonical (resolved) path, causing `strip_prefix` to fail and files being stored with absolute paths as origins.
- **Suggested fix:** Add `canonicalize()` in `find_project_root()` with a fallback to the raw path if canonicalization fails.

#### 5. `display.rs:read_context_lines` may return lines with trailing `\r` on CRLF files
- **Difficulty:** easy
- **Location:** `src/cli/display.rs:22-23`
- **Description:** `read_context_lines` calls `std::fs::read_to_string(file)` then `content.lines().collect()`. While `str::lines()` splits on both LF and CRLF, it does NOT strip trailing `\r` from split output when the file uses CRLF endings. This means context lines returned for display will include trailing `\r` characters. In a terminal, `\r` causes the cursor to return to line start, potentially corrupting the visual output of `cqs search --context`. The parser normalizes CRLF to LF for indexing, but the display path re-reads the raw file without normalization.
- **Suggested fix:** Add `.replace("\r\n", "\n")` before splitting, or `.trim_end_matches('\r')` on each output line.

#### 6. HNSW `save()` multi-file rename is not atomic as a group
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:596-614`
- **Description:** The save function renames four files (graph, data, ids, checksum) individually from temp to final. Each individual rename is atomic, but the group is not. A crash after renaming `hnsw.graph` but before `hnsw.checksum` leaves new graph data with old or missing checksums. The checksum verification on load catches this (load will fail with `ChecksumMismatch`), so the user is never served corrupted results — but they must re-index. On WSL/NTFS specifically, `rename()` across mount points would fail with `EXDEV`, though this doesn't occur here because temp and final are in the same directory.
- **Suggested fix:** The current design is crash-safe via detection (checksum mismatch). Document: "On crash during save, re-run `cqs index --force`." No code change needed.

#### 7. `notify::RecommendedWatcher` uses inotify which doesn't work on /mnt/c/ in WSL2
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:69`
- **Description:** `RecommendedWatcher` on Linux uses inotify. inotify does NOT reliably detect changes on WSL2's `/mnt/c/` (9P filesystem) — file modifications made by Windows-side editors do not generate inotify events. Users editing files in VS Code or other Windows editors while running `cqs watch` in WSL will see no reindexing. The `with_poll_interval` config at line 67 only affects poll-based watchers; `RecommendedWatcher` ignores it when inotify is available. Watch mode works correctly for edits made from within WSL or on native Linux filesystems. The gap is specifically: Windows-side edits to files on Windows mounts.
- **Suggested fix:** Detect `/mnt/` paths and warn users that watch mode may not detect changes from Windows editors. Offer a `--poll` flag that forces `PollWatcher` instead of `RecommendedWatcher`, which works on any filesystem at the cost of CPU usage.

#### 8. Reference paths in config use platform-dependent `PathBuf` serialization
- **Difficulty:** easy
- **Location:** `src/config.rs:17-18`
- **Description:** `ReferenceConfig.path` and `.source` are `PathBuf` with serde `Serialize`/`Deserialize`. TOML serialization of `PathBuf` uses the platform's native representation. A config file written by a Windows-native tool or process (backslash paths like `C:\Users\...`) would not load correctly on Linux, and vice versa. Since the project runs on WSL and config files may be edited by Windows tools or shared across environments (e.g., checked into git), users may encounter "reference not found" errors when the path separator doesn't match.
- **Suggested fix:** Normalize to forward slashes on deserialization, or document that reference paths must use forward slashes. Alternatively, normalize in `load_references` before calling `Store::open`.

#### 9. `strip_unc_prefix` is a no-op on WSL builds — UNC paths from Windows tools could leak through
- **Difficulty:** easy
- **Location:** `src/lib.rs:96-110`
- **Description:** `strip_unc_prefix` is gated by `#[cfg(windows)]` and `#[cfg(not(windows))]`. On WSL (Linux binary), the `not(windows)` no-op path is compiled. If a path from a Windows tool arrives via stdin, config file, or MCP argument with a `\\?\` prefix, it passes through unstripped. For example, the MCP `cqs_read` tool receives a path argument — if an MCP client on Windows sends a UNC-prefixed path, `server.project_root.join(path)` produces an invalid Linux path. The `file_path.canonicalize()` in `tool_read` would then fail with "No such file," which is handled gracefully, but the error message shows a confusing UNC path.
- **Suggested fix:** Make `strip_unc_prefix` unconditional (always strip `\\?\` prefix regardless of build target), since the function may receive Windows-origin paths on any platform via MCP.

#### 10. `ensure_ort_provider_libs` silently returns on unrecognized platform with no diagnostic
- **Difficulty:** easy
- **Location:** `src/embedder.rs:589-595`
- **Description:** The function matches `(ARCH, OS)` tuples for 4 platforms (x86_64/aarch64 x linux/macos) and returns early with no log for unrecognized combinations. On architectures like `riscv64` or `s390x`, or on FreeBSD, the function silently skips GPU provider setup. The user sees "CPU" as the provider with no hint that GPU detection was skipped because their platform wasn't handled. With WSL2, `std::env::consts::OS` reports `"linux"` so the x86_64 Linux path works correctly — this finding is about other platforms.
- **Suggested fix:** Add a debug-level log message on the early return: `tracing::debug!("GPU provider libs not supported for {}/{}", ARCH, OS)`. Helps users troubleshoot GPU setup without adding noise.

### Category: Edge Cases

#### 1. `BoundedScoreHeap::new(0)` silently drops all items — callers get empty results with no error
- **Difficulty:** easy
- **Location:** `src/search.rs:148-169`
- **Description:** `BoundedScoreHeap` with capacity 0 makes `push()` a no-op — the `self.0.len() >= capacity` check is immediately true, and since there's no existing minimum to compare against, every item is silently dropped. No caller currently passes 0 (MCP clamps to 1, CLI defaults to 5), but the library function `search_filtered` accepts `limit: usize` with no lower-bound check. A library consumer calling `store.search_filtered(..., 0, ...)` gets `Ok(vec![])` with no indication that the limit was invalid.
- **Suggested fix:** Either assert/error on capacity 0 in `BoundedScoreHeap::new`, or clamp limit to `max(1, limit)` at the `search_filtered` entry point.

#### 2. `add_reference_to_config` allows duplicate reference names when called from library code
- **Difficulty:** easy
- **Location:** `src/config.rs:180-207`
- **Description:** The CLI command `cmd_ref_add` checks for duplicates (line 68 of `cli/commands/reference.rs`), but the library function `add_reference_to_config` does not. It reads the TOML, appends the new `[[reference]]` entry, and writes back — no duplicate name check. If a library consumer (or a future CLI path) calls `add_reference_to_config` directly, the config file will have two references with the same name. `load_references` would load both, and search results would contain duplicated hits from the same index with the same source tag.
- **Suggested fix:** Move the duplicate check into `add_reference_to_config` itself, or document that callers must check first.

#### 3. `ref_path` allows path traversal via reference name — `../../etc/passwd` joins to escape refs directory
- **Difficulty:** medium
- **Location:** `src/reference.rs:174-176`
- **Description:** `ref_path(name)` does `refs_dir().map(|d| d.join(name))`. If `name` is `"../../etc/passwd"`, the result is `~/.local/share/cqs/refs/../../etc/passwd` which resolves to `/etc/passwd`. The CLI `cmd_ref_add` does not sanitize the name argument. While `Store::open` would fail on `/etc/passwd` (not a valid SQLite DB), `cmd_ref_remove` calls `std::fs::remove_dir_all(&cfg.path)` on the stored path — if a malicious config entry stored a traversal path, remove would delete arbitrary directories.
- **Suggested fix:** Validate reference names: reject any name containing `/`, `\`, `..`, or non-alphanumeric characters beyond `-` and `_`.

#### 4. Config silently accepts invalid values — `limit: 0`, `threshold: -5.0`, `batch_size: 0` all load without error
- **Difficulty:** easy
- **Location:** `src/config.rs:50-53`
- **Description:** `Config` deserializes `limit: Option<usize>`, `threshold: Option<f32>`, and `batch_size: Option<usize>` directly from TOML with no validation. A `.cqs.toml` with `limit = 0` would pass through to `search_filtered` hitting the `BoundedScoreHeap(0)` issue. `threshold = -5.0` would return all results regardless of quality. `batch_size = 0` would cause division-by-zero or infinite loops in the embedding pipeline. The MCP layer clamps its own values, but CLI and library callers trust config values directly.
- **Suggested fix:** Add a `Config::validate()` method called after `Config::load()`, or validate at deserialization time with serde attributes.

#### 5. `search_unified_with_index` with limit=1 gives 0 code slots — returns only notes
- **Difficulty:** easy
- **Location:** `src/search.rs:552-556`
- **Description:** The slot allocation formula `min_code_slots = (limit * 3) / 5` uses integer division. When `limit = 1`: `(1 * 3) / 5 = 0`. Zero code slots means the unified search returns only notes, never code — the opposite of what a "search for 1 result" caller expects. The MCP layer clamps limit to [1, 20], so limit=1 is a valid input that silently produces note-only results.
- **Suggested fix:** Use `max(1, (limit * 3) / 5)` to ensure at least one code slot, or switch to ceiling division: `(limit * 3 + 4) / 5`.

#### 6. `rewrite_notes_file` gives opaque IO error when notes.toml doesn't exist
- **Difficulty:** easy
- **Location:** `src/note.rs:125-144`
- **Description:** `rewrite_notes_file` calls `std::fs::read_to_string(path)` as its first operation. If the notes file doesn't exist (fresh project, deleted file), the user gets a raw IO error like "No such file or directory" with no context about which file or what to do about it. The `add_note` path handles missing files correctly (creates the file), but `remove_note` and `update_note` both call `rewrite_notes_file` which assumes the file exists.
- **Suggested fix:** Either create an empty notes file if missing, or return a descriptive error: "Notes file not found at {path}. Run `cqs add_note` first."

#### 7. `prune_missing` compares PathBuf by byte equality — fails on symlinks and non-canonical paths
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:151-162`
- **Description:** `prune_missing` builds a `HashSet<PathBuf>` of existing files and removes chunks whose file path isn't in the set. `HashSet::contains` uses `PathBuf`'s `PartialEq`, which compares bytes — no canonicalization. If a file was indexed via a symlink or relative path that differs from the enumerated absolute path, prune would delete the chunk even though the file still exists. On WSL, paths through `/mnt/c/` vs Windows native paths would also mismatch.
- **Suggested fix:** Canonicalize paths before comparison, or at minimum document that the enumeration and stored paths must use the same normalization.

#### 8. Empty query string bypasses semantic search short-circuit — embeds empty string and returns meaningless scores
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:34-36`
- **Description:** The `name_only` path checks for empty query (line 141: `args.query.trim().is_empty()`) and returns early. But the semantic search path (line 34) calls `embedder.embed_query(&args.query)` without any empty-query check. An empty string is a valid embedding input (the model produces a vector), but the resulting similarity scores are meaningless — they measure distance from a zero-content query. `validate_query_length` only checks max length (8192), not min length.
- **Suggested fix:** Add `if args.query.trim().is_empty()` check before the semantic path, returning empty results.

#### 9. NaN score rejection in BoundedScoreHeap relies on undocumented IEEE 754 comparison behavior
- **Difficulty:** easy
- **Location:** `src/search.rs:158-162`
- **Description:** `BoundedScoreHeap::push` uses `if score >= self.0.peek().unwrap().score` to decide whether to replace the minimum. If `score` is NaN, IEEE 754 guarantees this comparison returns false, so NaN scores are silently dropped — which is the correct behavior. However, this relies on an implicit property of floating-point comparison that isn't documented. If someone "optimizes" the comparison to use `!(score < min)` (which is true for NaN), NaN scores would be inserted, potentially corrupting the heap's ordering invariant.
- **Suggested fix:** Add a comment documenting the NaN-rejection property, or add an explicit `if score.is_nan() { return; }` guard for clarity.

#### 10. `embedding_batches` iterator uses recursive `self.next()` — stack depth proportional to filtered-out rows
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:539-613`
- **Description:** `EmbeddingBatchIterator::next()` calls itself recursively when the current batch is exhausted and there are more rows to fetch (line ~590: `return self.next()`). Each recursive call processes one batch. If the database has many rows that are all filtered out (e.g., all cached), each `next()` fetches a batch, finds nothing to return, and recurses. With 100K cached rows and batch_size=100, that's 1000 levels of recursion. Rust's default stack is 8MB, and each frame here is relatively small, so ~1000 levels is likely safe — but it's unbounded and could overflow with larger datasets.
- **Suggested fix:** Convert the recursive call to a loop: `loop { ... }` with `continue` instead of `return self.next()`.

## Batch 4: Algorithmic Complexity

#### 1. Brute-force search loads ALL chunk embeddings from SQLite when no HNSW index
- **Difficulty:** hard
- **Location:** `src/search.rs:237-303`
- **Description:** `search_filtered` fetches all rows from chunks (`SELECT id, embedding FROM chunks`) and iterates over every one, computing cosine similarity per row. Without an HNSW index, this is O(n) in the number of chunks, loading every embedding into memory. For a 50K chunk index, that's ~150MB of embedding data loaded and scored per query. The `BoundedScoreHeap` limits memory for the results, but the full table scan still happens. This is the expected fallback when no HNSW exists, but there's no warning at the search entry point when falling back, so users may not realize they're on the slow path.
- **Suggested fix:** Already mitigated by HNSW. Consider logging a one-time warning when brute-force is used beyond a threshold (e.g., >5K chunks without HNSW). No algorithm change needed — HNSW is the fix.

#### 2. `extract_body_keywords` tokenizes entire function content for keyword extraction
- **Difficulty:** medium
- **Location:** `src/nl.rs:606-745`
- **Description:** `extract_body_keywords` calls `tokenize_identifier(content)` on the full content of every chunk during NL description generation. For a 100-line function (~3KB), this tokenizes every character, builds a HashMap of word frequencies, sorts by frequency, and takes top 10. This runs during indexing for every chunk when `BodyKeywords` or `Compact` templates are active. The `tokenize_identifier` function allocates a new `String` per token and a `Vec<String>` for the result. For large function bodies, this produces hundreds of small string allocations. The sorting step (line 743) is O(m log m) where m is unique tokens — potentially hundreds per function.
- **Suggested fix:** Cap the input to `extract_body_keywords` at the first ~2KB of content (function bodies beyond that add noise, not signal). Alternatively, use a fixed-size array for the top-10 tracking instead of sorting the entire HashMap.

#### 3. `prune_missing` creates PathBuf from every distinct origin for HashSet lookup
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:160-163`
- **Description:** The filter on line 162 creates a `PathBuf::from(origin)` for every distinct origin in the database to check membership in `existing_files`. PathBuf allocation is cheap individually, but with 10K+ files this creates 10K+ allocations that are immediately discarded. The `existing_files` HashSet contains `PathBuf` values, so each comparison requires constructing a PathBuf from the string origin.
- **Suggested fix:** Convert `existing_files` to `HashSet<String>` (or `HashSet<&str>`) at the call site, or use `.to_string_lossy()` on the PathBuf set entries for comparison. Minor optimization — allocation cost is small relative to the SQL query.

#### 4. `merge_results` in reference.rs clones source name per result
- **Difficulty:** easy
- **Location:** `src/reference.rs:138-145`
- **Description:** In the inner loop at line 142, `name.clone()` is called for every search result from each reference. If a reference returns 50 results, that's 50 string clones of the same reference name. With multiple references, this multiplies.
- **Suggested fix:** Use `Arc<String>` or `Rc<String>` for the source name, or change `source: Option<String>` to `source: Option<&str>` with appropriate lifetimes. Minor — the clones are small strings.

#### 5. `search_by_name` in store/mod.rs re-scores every FTS result with string operations
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:461-471`
- **Description:** After FTS returns results (already ranked by BM25), the code re-scores each result with `to_lowercase()` + `starts_with()` + `contains()` string comparisons. This duplicates work that BM25 already accounts for (term proximity and exact match boosting). The `to_lowercase()` call allocates a new string per result. With large result sets (limit=100+), this adds unnecessary allocation overhead.
- **Suggested fix:** Trust BM25 scoring and use it directly, or at minimum cache the `query_lower` computation outside the closure. The re-scoring provides slightly different semantics (exact > prefix > contains > other) which may be intentional — if so, document why BM25 alone isn't sufficient.

#### 6. `normalize_for_fts` re-tokenizes every word boundary — quadratic on deeply nested identifiers
- **Difficulty:** easy
- **Location:** `src/nl.rs:148-198`
- **Description:** `normalize_for_fts` iterates character by character and calls `tokenize_identifier_iter` on each accumulated word. For typical code this is fine, but the function processes entire chunk content (up to 100KB before the MAX_FTS_OUTPUT_LEN cap). Each identifier goes through the iterator-based tokenizer which checks for CJK, uppercase boundaries, etc. The overall complexity is O(n) in content length which is acceptable, but the constant factor is high due to per-character branching and String allocation per token. The `MAX_FTS_OUTPUT_LEN` cap at 16KB prevents worst-case blowup.
- **Suggested fix:** This is adequately bounded by MAX_FTS_OUTPUT_LEN. No change needed unless profiling shows this as a bottleneck during indexing.

#### 7. `upsert_calls_batch` issues N individual DELETE queries for N unique chunk IDs
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:60-71`
- **Description:** Before batch-inserting calls, the code loops over unique chunk IDs and issues a separate `DELETE FROM calls WHERE caller_id = ?1` for each one. With 32 chunks per embedding batch (the default batch_size in pipeline.rs), that's up to 32 individual DELETE statements per batch, each requiring a SQLite round-trip within the transaction. The INSERT is properly batched via `QueryBuilder::push_values`, but the DELETE is not.
- **Suggested fix:** Batch the DELETE using `WHERE caller_id IN (?, ?, ...)` similar to how `prune_missing` batches its deletes. Collect unique IDs into a Vec, build a single DELETE with placeholders.

#### 8. `search_filtered` re-fetches full content for top-N results after scoring
- **Difficulty:** medium
- **Location:** `src/search.rs:341-363`
- **Description:** The two-phase search first scores all embeddings (phase 1), then fetches full chunk content for the top-N IDs (phase 2 at line 343: `fetch_chunks_by_ids_async`). This second query builds a dynamic `WHERE id IN (...)` clause and fetches from SQLite. While this is the right design (avoiding loading full content during scoring), the `fetch_chunks_by_ids_async` function doesn't batch its IDs — if `limit * 3` is large (e.g., 15 for RRF mode), SQLite handles it fine, but the function creates unbatched placeholder strings. For typical limits (5-20), this is fine. The real cost is the two round-trips to SQLite per search.
- **Suggested fix:** No change needed for current usage. If limit ever grows large, add batching similar to `get_embeddings_by_hashes` (batch size 500).

#### 9. `reference.rs::merge_results` sorts all results even when most will be truncated
- **Difficulty:** easy
- **Location:** `src/reference.rs:148-157`
- **Description:** `merge_results` collects all primary + all reference results into a single Vec, sorts the entire Vec by score, then truncates to `limit`. If there are 5 references each returning 20 results plus 20 primary results, that's 120 items being fully sorted when only the top 5-20 are needed. A partial sort (select top-k) would be O(n) instead of O(n log n).
- **Suggested fix:** Use `select_nth_unstable_by` to partition around the limit-th element, then sort only the top partition. Or use a `BinaryHeap` bounded to `limit` (similar to `BoundedScoreHeap` in search.rs). Minor — n is typically <200.

#### 10. `needs_reindex` per-file during indexing — N queries for N files
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:97-117` called from `src/cli/pipeline.rs:282-294`
- **Description:** In the parser thread (pipeline.rs:282), `store.needs_reindex(&abs_path)` is called for every chunk's file. This issues a separate `SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1` query per file. With 10K files, that's 10K individual SQLite queries during the mtime check phase. The queries are fast (indexed by origin), but the overhead of 10K round-trips adds up.
- **Suggested fix:** Batch the mtime check: `SELECT origin, source_mtime FROM chunks WHERE origin IN (...)` for each file batch, then check in-memory. This would reduce 10K queries to ~10 batched queries (at batch_size=1000).

## Batch 4: Data Security

#### 1. SSE endpoint missing origin validation
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:326-353`
- **Description:** The `handle_mcp_sse` (GET /mcp) handler validates the API key and `Accept` header, but does NOT call `validate_origin_header()`. The POST handler at line 263 does call it. This means a DNS rebinding attack could establish an SSE connection from a non-localhost origin. While the current SSE endpoint only sends a priming event and keep-alives (no sensitive data yet), this is inconsistent with the POST endpoint's security posture and will become a real vulnerability if server-initiated messages carrying search results are added later.
- **Suggested fix:** Add `validate_origin_header(&headers)?;` to `handle_mcp_sse` after the API key check, matching the POST handler's pattern.

#### 2. Reference name allows path traversal in storage directory
- **Difficulty:** medium
- **Location:** `src/reference.rs:174-175`, `src/cli/commands/reference.rs:82-84`
- **Description:** `ref_path(name)` joins the user-supplied reference name directly into the storage path: `refs_dir().map(|d| d.join(name))`. A name like `../../etc/evil` would create directories outside the intended `~/.local/share/cqs/refs/` tree. The `create_dir_all` at reference.rs:84 then creates whatever directory structure the attacker specified. While this is a CLI command (requires local access), it violates the principle of validating user input at system boundaries.
- **Suggested fix:** Validate the reference name contains only alphanumeric characters, hyphens, and underscores. Reject names containing `/`, `\`, or `..`.

#### 3. Reference directory created without restrictive permissions
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:84`
- **Description:** When creating reference index directories via `std::fs::create_dir_all(&ref_dir)`, no Unix permissions are set. Compare with `init.rs:28-29` which sets 0o700 on `.cq/`, `store/mod.rs:159` which sets 0o600 on database files, and `hnsw.rs:587` which sets 0o600 on HNSW files. The reference directory and its database get default umask permissions (typically 0o755 for dirs, 0o644 for files), making the indexed code embeddings world-readable.
- **Suggested fix:** After `create_dir_all`, set permissions to 0o700 for the directory. The Store::open and HnswIndex::save already handle file-level permissions for their files within the directory.

#### 4. Config file written without restrictive permissions
- **Difficulty:** easy
- **Location:** `src/config.rs:205`, `src/config.rs:240`
- **Description:** `add_reference_to_config` and `remove_reference_from_config` write to `.cqs.toml` using `std::fs::write()` without setting file permissions. The config file contains filesystem paths (reference source directories, storage paths) that reveal the user's directory structure. While not directly secrets, this is information leakage and inconsistent with the permission hardening applied to other cqs-managed files (database, HNSW, notes, lock file).
- **Suggested fix:** After writing the config file, set permissions to 0o600 on Unix, matching the pattern used in `notes.rs:112-114`.

#### 5. API key visible in process listing via `--api-key` CLI flag
- **Difficulty:** medium
- **Location:** `src/cli/mod.rs:131`, `SECURITY.md:59-61`
- **Description:** The `--api-key SECRET` CLI flag is documented as "visible in process list" in SECURITY.md. While `--api-key-file` and `CQS_API_KEY` env var alternatives exist, there is no runtime warning when using `--api-key` directly. The `CQS_API_KEY` env var (line 131: `env = "CQS_API_KEY"`) is also visible in `/proc/*/environ` on Linux. SECURITY.md documents this but users may not read it. The recommended `--api-key-file` approach is properly implemented with `zeroize`.
- **Suggested fix:** Emit a tracing::warn when `--api-key` is used directly (not via file), advising to use `--api-key-file` instead. This is a UX improvement rather than a code fix.

#### 6. Health endpoint exposes version without authentication
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:317-323`
- **Description:** The `/health` endpoint returns the exact cqs version (`env!("CARGO_PKG_VERSION")`) without requiring any authentication. While the code has a security note acknowledging this is intentional for localhost, when bound to a network address (with `--dangerously-allow-network-bind`), this allows unauthenticated version fingerprinting. An attacker can determine the exact version to check for known vulnerabilities.
- **Suggested fix:** Either require API key auth on `/health` when bound to non-localhost, or remove the version field from the response when not on localhost.

#### 7. Error sanitization regex misses some path patterns
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:195-211`
- **Description:** The `sanitize_error_message` regex for Unix paths only matches paths starting with specific prefixes: `/home`, `/Users`, `/tmp`, `/var`, `/usr`, `/opt`, `/etc`, `/mnt`, `/root`. Paths starting with other directories (e.g., `/data/`, `/srv/`, `/media/`, `/run/`) will leak through. The Windows regex similarly only catches `Users`, `Windows`, `Program Files`. This means some internal filesystem paths could be exposed to MCP clients in error messages.
- **Suggested fix:** Use a broader regex like `r"/[a-zA-Z][^\s:]*/"` for Unix or strip all absolute paths. Alternatively, maintain an allowlist approach but add the missing common prefixes (`/srv`, `/data`, `/media`, `/run`, `/snap`, `/nix`).

#### 8. Database file not encrypted - code embeddings persist in plaintext
- **Difficulty:** hard
- **Location:** `src/store/mod.rs:108`, `SECURITY.md:141-142`
- **Description:** SECURITY.md explicitly documents "Database is not encrypted - it contains your code." The SQLite database stores code chunks, embeddings, and full function/class content in plaintext. File permissions (0o600) provide OS-level access control, but if the disk is accessed directly (stolen laptop, backup system, shared filesystem), the code is exposed. This is documented and accepted as a design decision for a local tool, but worth noting for environments with compliance requirements.
- **Suggested fix:** Consider SQLCipher or similar for at-rest encryption as an opt-in feature for sensitive codebases. Low priority for current use case.

#### 9. Model download over HTTPS but no certificate pinning
- **Difficulty:** hard
- **Location:** `src/embedder.rs:532-553`
- **Description:** The model is downloaded from HuggingFace Hub over HTTPS via the `hf_hub` crate. Post-download blake3 checksums verify integrity. However, there is no certificate pinning, meaning a MITM attack with a compromised CA could serve a malicious model. The blake3 checksums in `MODEL_BLAKE3` and `TOKENIZER_BLAKE3` (lines 19-20) mitigate this since they're compiled into the binary and must match. This is actually well-designed - the hardcoded checksums effectively serve as a pin against supply chain attacks.
- **Suggested fix:** No fix needed. The hardcoded blake3 checksums provide strong integrity verification. This finding is informational - the current design is sound.

## Batch 4: Resource Footprint

#### 1. Each Store instance creates its own tokio Runtime — references multiply this
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:104`, `src/reference.rs:41-52`
- **Description:** Every `Store::open()` creates a new `tokio::runtime::Runtime`. The MCP server opens one Store for the primary project, one per reference index (via `load_references`), and potentially another in the CAGRA background thread (line `src/mcp/server.rs:105`). Each Runtime spawns worker threads (default = CPU cores). With 3 references on an 8-core machine, that's 4 Runtimes × 8 threads = 32 threads, mostly idle. The HTTP transport creates yet another Runtime (`src/mcp/transports/http.rs:118`), bringing it to 5 Runtimes × 8 threads = 40 threads.
- **Suggested fix:** Share a single Runtime across all Store instances, either by accepting an external `Handle` or by using a global/scoped Runtime. The HTTP transport's Runtime could also be shared.

#### 2. SQLite 256MB mmap_size per connection, 4 connections per Store
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:142-144`
- **Description:** Each connection sets `PRAGMA mmap_size = 268435456` (256MB). With 4 connections per pool and multiple Store instances (primary + references + CAGRA background), the theoretical mmap reservation is 256MB × 4 × N stores. While mmap is virtual memory (not RSS), on 32-bit systems or memory-constrained environments, this address space reservation can fail. For a typical cqs index (10-50MB), 256MB per connection is massively oversized.
- **Suggested fix:** Scale mmap_size to actual database file size (e.g., `min(file_size * 2, 256MB)`) or reduce to a more modest default like 64MB.

#### 3. 16MB SQLite page cache per connection, multiplied across pools
- **Difficulty:** easy
- **Location:** `src/store/mod.rs:134-136`
- **Description:** `PRAGMA cache_size = -16384` sets 16MB page cache per connection. With 4 connections × N stores, that's 64MB × N of page cache. For the primary store alone that's 64MB. With 3 references, it's 256MB of SQLite page cache total — for what are typically small databases. The default SQLite cache (2MB) is usually sufficient for cqs workloads.
- **Suggested fix:** Reduce to 4MB (`-4096`) or make it configurable. For read-heavy search workloads, a smaller cache is adequate since the working set is small.

#### 4. CAGRA duplicates full dataset in memory alongside HNSW
- **Difficulty:** hard
- **Location:** `src/cagra.rs:62-63`, `src/mcp/server.rs:64-79`
- **Description:** When GPU is available, the MCP server loads HNSW into memory (for immediate search), then spawns a background thread that opens a second Store, fetches ALL embeddings, and builds a CAGRA index that holds a full copy of the dataset (`dataset: Array2<f32>`) permanently in memory — because cuVS's `search()` consumes the index and needs to rebuild. For 50K chunks at 769 dims × 4 bytes = ~150MB, plus the HNSW index in parallel, plus the background Store's own Runtime/connections. Total: HNSW (~200MB) + CAGRA dataset (~150MB) + CAGRA index (GPU mem) + extra Store overhead.
- **Suggested fix:** Drop the HNSW index after CAGRA is ready (the RwLock swap already handles this, but HNSW memory isn't freed — verify it's replaced, not accumulated). Document the ~350MB+ RAM cost of GPU mode.

#### 5. CAGRA background Store is never explicitly closed
- **Difficulty:** easy
- **Location:** `src/mcp/server.rs:105-123`
- **Description:** The background CAGRA build thread opens a Store (`Store::open(index_path)`), uses it to fetch embeddings, then the Store is dropped when `build_cagra_background` returns. The Drop impl attempts a WAL checkpoint via `block_on`, which can panic if called from within a tokio async context (though `catch_unwind` guards this). More importantly, the Store's Runtime and 4 SQLite connections stay alive for the entire build duration (which could be minutes for large indexes), then are all cleaned up at once.
- **Suggested fix:** Call `store.close()` explicitly after `build_from_store` completes, before the index swap. This checkpoints the WAL and releases resources immediately.

#### 6. `tokenizers` crate `http` feature enables unnecessary networking dependencies
- **Difficulty:** easy
- **Location:** `Cargo.toml:42`
- **Description:** The `tokenizers` crate is configured with `features = ["http"]`, which enables the `hf-hub` dependency within tokenizers for downloading tokenizer files from HuggingFace. However, cqs already has its own `hf-hub` dependency (line 43) that handles model+tokenizer downloads. The `http` feature in tokenizers enables a second download path that's never used (cqs loads tokenizers from local files via `Tokenizer::from_file()`). This adds unnecessary code to the binary.
- **Suggested fix:** Remove the `http` feature: `tokenizers = { version = "0.22" }`. The default features should suffice since cqs loads tokenizer files from disk.

#### 7. `once_cell` dependency is redundant with Rust 1.88 stdlib
- **Difficulty:** easy
- **Location:** `Cargo.toml:97`, `src/embedder.rs:5`, `src/parser.rs:3`
- **Description:** The project requires `rust-version = "1.88"` (Cargo.toml:5) which ships `std::sync::OnceLock` and `std::sync::LazyLock` as stable. The crate uses `once_cell::sync::OnceCell` in two files (`embedder.rs` and `parser.rs`). Meanwhile, other files already use `std::sync::OnceLock` (e.g., `mcp/server.rs:6`) and `std::sync::LazyLock` (e.g., `mcp/transports/http.rs` via lazy regex). This is a redundant dependency that adds binary bloat.
- **Suggested fix:** Replace `once_cell::sync::OnceCell` with `std::sync::OnceLock` in `embedder.rs` and `parser.rs`, then remove `once_cell` from `Cargo.toml`.

#### 8. Watch mode holds file watcher on entire project tree recursively
- **Difficulty:** medium
- **Location:** `src/cli/watch.rs:70`
- **Description:** `watcher.watch(&root, RecursiveMode::Recursive)` watches the entire project directory tree. For large repos with `node_modules/`, `target/`, `.git/`, or vendor directories, this can consume significant inotify watches (Linux default limit: 8192). The `ignore` crate is available in dependencies but not used for filtering watch targets — only for filtering events after the fact. Each inotify watch is a kernel resource.
- **Suggested fix:** Use `ignore::WalkBuilder` to enumerate directories respecting `.gitignore`, then watch only those directories individually. Or at minimum, exclude known heavy directories (`target/`, `node_modules/`, `.git/`).

#### 9. SSE keep-alive stream stays open indefinitely with no idle timeout
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:348-353`
- **Description:** The SSE endpoint (`GET /mcp`) creates a stream with `KeepAlive` that sends pings every 15 seconds forever. There's no idle timeout or connection limit. A client that connects and never sends requests holds a TCP connection, a stream task, and keep-alive timer indefinitely. While the connection itself is lightweight, accumulating stale SSE connections (e.g., from crashed clients that don't send TCP RST) consumes file descriptors and memory.
- **Suggested fix:** Add an idle timeout (e.g., 30 minutes) using `tokio::time::timeout` wrapping the stream, or limit the maximum number of concurrent SSE connections.

#### 10. `crossbeam-channel` dependency only used in one file, `mpsc` would suffice
- **Difficulty:** easy
- **Location:** `Cargo.toml:85`, `src/cli/pipeline.rs:14`
- **Description:** `crossbeam-channel` is only imported in `src/cli/pipeline.rs` for the indexing pipeline. The pipeline uses `bounded` channels for backpressure between parser and embedder threads. While `crossbeam-channel` has better performance characteristics than `std::sync::mpsc`, the pipeline throughput is dominated by ML inference (~100ms per batch), making channel performance negligible. This dependency adds ~50KB to the binary.
- **Suggested fix:** The `select!` macro IS used (pipeline.rs:475), so stdlib `mpsc` isn't a drop-in. Consider whether the select is essential — if only one channel is being selected on, it can be replaced with a simple `recv`. Otherwise, this dependency is justified and this finding is low priority.


## Batch 4: Input Security

#### 1. Reference name path traversal in `ref_path` and `cmd_ref_add`
- **Difficulty:** easy
- **Location:** `src/reference.rs:174-176`, `src/cli/commands/reference.rs:82`
- **Description:** `ref_path(name)` joins user-provided `name` directly into a filesystem path via `refs_dir().map(|d| d.join(name))`. A name like `../../etc` or `../../../tmp/evil` would escape the refs directory. `cmd_ref_add` calls `ref_path(name)` at line 82 and then creates directories at that path (`std::fs::create_dir_all(&ref_dir)`). While `source` is canonicalized and validated at line 77-79, the `name` parameter has zero sanitization — no check for `/`, `..`, `\`, or other path-special characters. An attacker (or careless user) could create directories and SQLite databases in arbitrary filesystem locations.
- **Suggested fix:** Validate reference names to allow only `[a-zA-Z0-9_-]` characters, rejecting anything containing `/`, `\`, `..`, or path separators. Apply this validation in both the CLI (`cmd_ref_add`) and `ref_path`.

#### 2. Reference name not validated in `cmd_ref_remove` — arbitrary directory deletion
- **Difficulty:** easy
- **Location:** `src/cli/commands/reference.rs:194-217`
- **Description:** `cmd_ref_remove` loads the reference config, finds the entry by name, removes it from config, then at line 210-212 does `std::fs::remove_dir_all(&cfg.path)` on whatever path was stored in the config. If a previous `ref add` with a traversal name wrote to an unintended location, `ref remove` would `remove_dir_all` that location. Even without a prior traversal attack, a manually-edited `.cqs.toml` with `path = "/important/data"` would cause `ref remove` to delete `/important/data`. There is no confirmation and no check that the path is within the expected refs directory.
- **Suggested fix:** Before `remove_dir_all`, verify the path is within `refs_dir()`. Alternatively, only allow deletion of paths under the canonical refs storage directory.

#### 3. `tool_read` TOCTOU between canonicalize and file read
- **Difficulty:** medium
- **Location:** `src/mcp/tools/read.rs:19-51`
- **Description:** The read tool checks `file_path.exists()` at line 19, then canonicalizes at line 24-27, checks `starts_with` at line 35, then reads the file at line 51. Between these steps, a symlink target could change (TOCTOU race). On systems where an attacker controls files within the project directory, they could create a symlink pointing inside the project (passing the canonicalize check), then quickly retarget it to `/etc/shadow` before the `read_to_string` call. In practice this requires a compromised project directory, but the window exists. The use of `canonicalize` (which follows symlinks) correctly rejects out-of-project symlinks, but the gap between canonicalize and read is the vulnerability window.
- **Suggested fix:** Open the file first (getting a file descriptor), then use `fstat` on the descriptor to verify the path, then read from the descriptor. Or use `O_NOFOLLOW` if symlink following is not desired.

#### 4. FTS query construction in `search_by_name` uses string formatting with user input
- **Difficulty:** medium
- **Location:** `src/store/mod.rs:427`
- **Description:** `search_by_name` constructs an FTS5 query: `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. The `normalized` value comes from `normalize_for_fts()` which only emits alphanumeric characters, underscores, and spaces — so FTS5 operators and quotes are stripped. This provides **implicit** protection against FTS5 injection. However, the defense is fragile: if `normalize_for_fts` is ever modified to pass through additional characters (especially `"`, `*`, `OR`, `AND`, `NOT`, `NEAR`, `(`), the FTS query could be manipulated. The security property depends on a function whose documented purpose is text normalization, not security sanitization.
- **Suggested fix:** Add an explicit FTS5 sanitization step (or an assertion that the normalized string contains no FTS5 special characters) before interpolating into the query string. Add a comment documenting that `normalize_for_fts` is security-load-bearing here.

#### 5. Config file read-modify-write in `add_reference_to_config` is not atomic
- **Difficulty:** medium
- **Location:** `src/config.rs:183-207`
- **Description:** `add_reference_to_config` reads the config file, parses it, modifies it, then writes back with `std::fs::write`. This is a read-modify-write pattern without file locking. Two concurrent `cqs ref add` commands could race: both read the same config, both add their reference, and the second write overwrites the first's addition. Unlike `rewrite_notes_file` which uses atomic write (temp + rename), `config.rs` overwrites in place. A crash during `std::fs::write` could leave the config file truncated or empty.
- **Suggested fix:** Use atomic write (write to temp file, then rename) like `rewrite_notes_file` does. Consider file locking for concurrent access protection.

#### 6. HNSW checksum verification is optional — warns-only on missing checksums
- **Difficulty:** medium
- **Location:** `src/hnsw.rs:104-155`
- **Description:** `verify_hnsw_checksums` at line 107-111 returns `Ok(())` with only a warning if no checksum file exists. Older indexes without checksums bypass integrity verification entirely. The `load` function at line 638 then deserializes index files using bincode (RUSTSEC-2025-0141, unmaintained) without integrity guarantee. A malicious or corrupted index file could exploit bincode deserialization vulnerabilities. The checksum-on-save approach means checksums are only present for indexes created after the checksum feature was added.
- **Suggested fix:** Consider making checksums mandatory for loading (fail instead of warn). Provide migration: `cqs index --force` rebuilds with checksums. At minimum, log at a user-visible level.

#### 7. HTTP server allows binding to any interface without restriction
- **Difficulty:** easy
- **Location:** `src/mcp/transports/http.rs:97-107`
- **Description:** `serve_http` accepts a `bind` parameter and binds directly. When bound to `0.0.0.0` without `--api-key`, anyone on the network can access the MCP server, which can read files within the project (via `cqs_read`) and execute searches. The origin validation (`is_localhost_origin`) only blocks browser requests with `Origin` headers — direct HTTP clients (curl, scripts) don't send Origin headers and bypass this protection entirely.
- **Suggested fix:** Default to `127.0.0.1` only. When binding to non-localhost without an API key, either refuse to start or require an explicit `--insecure` flag.

#### 8. `cqs_read` tool exposes all project files to MCP clients without granular authorization
- **Difficulty:** medium
- **Location:** `src/mcp/tools/read.rs:12-131`
- **Description:** The `tool_read` function reads any file within the project directory and returns its full content. Over HTTP transport without an API key, any client can read source code. The path traversal protection prevents reading outside the project, but within the project there is no authorization granularity — a client can read `.env`, `*.pem`, `*.key`, or credential files. The 10MB limit prevents memory exhaustion but not information disclosure.
- **Suggested fix:** Consider an allowlist/denylist for sensitive file patterns (`.env`, `*.pem`, `*.key`, `credentials.*`) or add a configuration option to disable the read tool for HTTP transport.

#### 9. `note_weight` and other float params — inconsistent error vs clamp behavior
- **Difficulty:** easy
- **Location:** `src/mcp/tools/search.rs:45`, `src/store/helpers.rs:332-335`, `src/mcp/tools/notes.rs:56`
- **Description:** Float parameters are handled inconsistently: `sentiment` in notes is silently clamped to `[-1.0, 1.0]` (notes.rs:56), while `note_weight` and `name_boost` in search reject out-of-range values with errors (helpers.rs:328-335). This means a client sending `note_weight: 1.5` gets an error, but `sentiment: 5.0` is silently accepted as `1.0`. The inconsistency can confuse API consumers and may mask bugs where a client sends unexpected values.
- **Suggested fix:** Pick one approach (clamp or reject) and apply consistently across all bounded float parameters.

#### 10. SQL queries use parameterized bindings (positive finding)
- **Difficulty:** n/a
- **Location:** `src/store/chunks.rs`, `src/store/notes.rs`, `src/store/mod.rs`
- **Description:** All SQL queries across the store module use parameterized bindings (`?1`, `?2`, etc.) via sqlx. No string interpolation of user input into SQL. The dynamic placeholder generation for batch operations (e.g., `chunks.rs:178-180`) constructs only the placeholder string (e.g., `?1,?2,?3`) from integer indices, not from user data. This is correct and prevents SQL injection. The one exception is the FTS5 MATCH query in `search_by_name` (finding #4 above) which is protected by `normalize_for_fts`.

## Batch 4: I/O Efficiency

#### 1. notes.toml re-parsed from disk on every MCP note mutation and file read (existing #233)
- **Difficulty:** medium
- **Location:** `src/mcp/tools/notes.rs:13-28` and `src/mcp/tools/read.rs:69-116`
- **Description:** Every `add_note`, `update_note`, and `remove_note` call triggers `reindex_notes()` which calls `parse_notes()` — a full disk read + TOML parse of the entire notes.toml. Similarly, `tool_read` re-parses notes.toml from disk for every file read to inject context comments. For a notes file with 50+ entries, this means redundant disk I/O and TOML parsing on every single MCP tool call. The parsed result is never cached.
- **Suggested fix:** Cache the parsed notes in the McpServer struct with mtime-based invalidation. Existing #233.

#### 2. `search_filtered` brute-force loads ALL chunk embeddings into memory via `fetch_all`
- **Difficulty:** hard
- **Location:** `src/search.rs:196-365`
- **Description:** When no HNSW index is available (or it returns no candidates), `search_filtered` executes `SELECT id, embedding FROM chunks` with an optional WHERE clause — loading every single embedding (~3KB each) into memory via `fetch_all`. For a 10K chunk index, that's ~30MB of data transferred from SQLite in one call. For 50K chunks, ~150MB. This full table scan is the O(n) fallback that should only happen before HNSW is built, but it also triggers whenever the HNSW index returns empty results (line 383-384 in search.rs falls back to brute-force).
- **Suggested fix:** For the no-index fallback, consider cursor-based streaming (`fetch()` with async iteration and the BoundedScoreHeap) instead of `fetch_all()` to avoid materializing all embeddings at once. Alternatively, always ensure HNSW is built before enabling search.

#### 3. `reindex_files` in watch mode uses individual `upsert_chunk` instead of batch
- **Difficulty:** easy
- **Location:** `src/cli/watch.rs:262-273`
- **Description:** The `reindex_files` function in watch mode calls `store.upsert_chunk()` in a per-item loop for each (chunk, embedding) pair. `upsert_chunk` delegates to `upsert_chunks_batch` with a single-element slice, meaning each chunk gets its own transaction (BEGIN/COMMIT) and separate FTS operations. The pipeline (`run_index_pipeline`) correctly uses batch upserts with batches of 32. For a file with 20 chunks, watch mode does 20 separate transactions instead of 1.
- **Suggested fix:** Collect all `(chunk, embedding)` pairs into a Vec and call `store.upsert_chunks_batch()` once, with a shared mtime. Matches the pipeline pattern.

#### 4. `rewrite_notes_file` result discarded, file immediately re-read by `reindex_notes`
- **Difficulty:** easy
- **Location:** `src/note.rs:125-144` called from `src/mcp/tools/notes.rs:239`, `src/mcp/tools/notes.rs:301`
- **Description:** `rewrite_notes_file` reads notes.toml, parses TOML, applies a mutation, serializes, writes atomically, and returns the final `Vec<NoteEntry>`. But callers (`tool_update_note`, `tool_remove_note`) discard this return value. Then `reindex_notes` immediately re-reads the same file from disk, re-parses TOML, and re-generates notes — duplicating the I/O and parsing just performed. The `tool_add_note` path is slightly better (append-only, no rewrite) but still triggers a full re-read via `reindex_notes`.
- **Suggested fix:** Pass the already-parsed entries from `rewrite_notes_file` into `reindex_notes` instead of having it re-read from disk.

#### 5. `count_vectors` deserializes full HNSW ID map JSON just to count entries
- **Difficulty:** easy
- **Location:** `src/hnsw.rs:731-762`
- **Description:** `HnswIndex::count_vectors()` reads the entire `.hnsw.ids` JSON file into a String, deserializes the full `Vec<String>` (allocating every chunk ID string), then calls `.len()` on it. For a 50K chunk index, the IDs file could be several MB of JSON. All that memory is allocated just to count elements, then immediately freed.
- **Suggested fix:** Store the vector count as a simple integer in a separate small file (e.g., `.hnsw.count`) written alongside IDs during `save()`. Or count JSON array elements by scanning for commas without full deserialization.

#### 6. Each reference index opens a separate tokio Runtime + SQLite connection pool
- **Difficulty:** medium
- **Location:** `src/reference.rs:36-69` via `Store::open`
- **Description:** `load_references` opens a new `Store` per reference, each with its own tokio Runtime + SQLite connection pool (max 4 connections per pool, per `Store::open`). With 3 references, that's 3 runtimes + up to 12 idle SQLite connections + 3x the PRAGMA setup overhead (WAL, mmap 256MB, cache 16MB). References are read-only during search but get the full read-write connection pool treatment including WAL checkpoint on Drop.
- **Suggested fix:** Create a `Store::open_readonly` variant that uses `mode=ro`, smaller pool (1 connection), skips WAL checkpoint on drop, and optionally shares an existing tokio RuntimeHandle instead of creating a new one.

#### 7. `search_unified_with_index` always brute-force scans ALL notes from SQLite
- **Difficulty:** medium
- **Location:** `src/search.rs:517-521` and `src/store/notes.rs:118-163`
- **Description:** Even when an HNSW index is available (which includes notes with `note:` prefix), `search_unified_with_index` ignores the index for notes and always falls back to `search_notes()` — a brute-force O(n) scan loading up to 1000 note embeddings (~3KB each = 3MB) and computing cosine similarity for each. The code comment explains this is intentional for immediate searchability of new notes. But the HNSW index already contains notes (they're explicitly filtered out at lines 533-541 and discarded). This means note embeddings are loaded from SQLite on every single search.
- **Suggested fix:** Use HNSW for notes when available, supplementing with a brute-force scan of only notes added since the last HNSW build (based on `created_at > last_hnsw_build_time`).

#### 8. `tool_add_note` calls `sync_all()` (full fsync + metadata) on every append
- **Difficulty:** easy
- **Location:** `src/mcp/tools/notes.rs:128`
- **Description:** `tool_add_note` calls `file.sync_all()` after appending a note to notes.toml. `sync_all` forces both data and metadata to disk, which can take 5-50ms on spinning disks. `sync_data()` would suffice since we don't need metadata durability for this path — the file is git-tracked, and the subsequent `reindex_notes` read-back implicitly verifies the write succeeded. The notes.toml file is not crash-critical.
- **Suggested fix:** Replace `sync_all()` with `sync_data()`, or remove the fsync entirely since the file is git-tracked and re-read immediately after.

#### 9. Pipeline `needs_reindex` issues one SQLite query per file during parsing
- **Difficulty:** medium
- **Location:** `src/cli/pipeline.rs:278-294` calling `src/store/chunks.rs:97-117`
- **Description:** In `run_index_pipeline`, the parser thread calls `store.needs_reindex(&abs_path)` per file, issuing `SELECT source_mtime FROM chunks WHERE origin = ?1 LIMIT 1` individually. For a 5K file codebase, that's 5000 SQLite round-trips. Since all files are known upfront (the `files` Vec is passed in), a single `SELECT DISTINCT origin, source_mtime FROM chunks WHERE source_type = 'file'` could fetch all mtimes at once into a HashMap.
- **Suggested fix:** Bulk-fetch all (origin, source_mtime) pairs into a HashMap before the parsing loop, then check the HashMap instead of per-file queries.

#### 10. `all_embeddings` loads entire embedding table into memory at once
- **Difficulty:** medium
- **Location:** `src/store/chunks.rs:488-522`
- **Description:** `all_embeddings()` does `SELECT id, embedding FROM chunks` with `fetch_all()`, loading every embedding simultaneously. For 50K chunks, that's ~150MB. The method has a deprecation notice pointing to `embedding_batches()`, but `HnswIndex::build()` (the non-batched version) still exists as public API. Any caller reaching `build()` instead of `build_batched()` triggers this full load. The `build()` method itself is only soft-deprecated ("prefer `build_batched`") with no compile-time guard.
- **Suggested fix:** Verify all production callers use `build_batched()`. If `all_embeddings()` has no non-test callers, gate it with `#[cfg(test)]` to prevent accidental use. Consider making `build()` call `build_batched()` internally.
