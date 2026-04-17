# Audit Findings — v0.28.1+uncommitted

Date: 2026-03-06
Codebase: v0.28.1 + Vue + markdown fenced blocks + flaky CI fix (uncommitted)

## Observability

#### OB-1: `store/calls.rs` — 15 public functions missing `info_span!` at entry
- **Difficulty:** medium
- **Location:** src/store/calls.rs (multiple: `upsert_calls`:160, `upsert_calls_batch`:201, `get_callees`:252, `call_stats`:266, `upsert_function_calls`:283, `get_callers_full`:346, `get_callees_full`:378, `get_callers_with_context`:459, `get_callers_with_context_batch`:491, `get_callers_full_batch`:542, `get_callees_full_batch`:596, `prune_stale_calls`:1031, `find_shared_callers`:1138, `find_shared_callees`:1169, `function_call_stats`:1197)
- **Description:** Only 5 of 20 public functions in `store/calls.rs` have `info_span!` entry (`get_call_graph`, `find_dead_code`, `find_test_chunks`, `get_caller_counts_batch`, `get_callee_counts_batch`). The remaining 15 public methods — including write-path functions like `upsert_calls_batch` and `upsert_function_calls`, and frequently-called read-path functions like `get_callers_full_batch` and `get_callees_full_batch` — have zero tracing. This makes it impossible to correlate slow queries or indexing bottlenecks with specific store operations.
- **Suggested fix:** Add `let _span = tracing::info_span!("function_name", relevant_param).entered();` to each. For write paths, include count; for read paths, include the query key. Use `debug_span!` for simple getters that are called in hot loops (e.g., `get_callees`).

#### OB-2: `store/types.rs` — All 9 public functions missing `info_span!`
- **Difficulty:** easy
- **Location:** src/store/types.rs (all of: `upsert_type_edges`:54, `upsert_type_edges_for_file`:106, `get_type_users`:227, `get_types_used_by`:254, `get_type_users_batch`:282, `get_types_used_by_batch`:343, `type_edge_stats`:394, `find_shared_type_users`:455, `prune_stale_type_edges`:489)
- **Description:** Only `get_type_graph` (line 415) has a span. The entire type-edge subsystem — upsert, query, batch, stats, pruning — is invisible to tracing. `get_type_graph` was fixed in v0.26.0 audit but the rest were missed.
- **Suggested fix:** Add `info_span!` to each function. Batch functions should include count. The `prune_stale_type_edges` and `upsert_type_edges_for_file` write paths are especially important for diagnosing indexing performance.

#### OB-3: `store/notes.rs` — 7 of 8 public functions missing `info_span!`
- **Difficulty:** easy
- **Location:** src/store/notes.rs (`upsert_notes_batch`:99, `replace_notes_for_file`:187, `notes_need_reindex`:234, `note_count`:257, `note_stats`:272, `list_notes_summaries`:297, `note_embeddings`:330)
- **Description:** Only `search_notes` (line 141) has a span. The write path (`upsert_notes_batch`, `replace_notes_for_file`) and stats functions are untraceable. Note indexing failures are hard to diagnose because there's no span context showing which operation was happening.
- **Suggested fix:** Add spans with relevant structured fields (count for batch ops, path for file ops).

#### OB-4: `store/chunks.rs` — 16 public functions missing `info_span!`
- **Difficulty:** medium
- **Location:** src/store/chunks.rs (multiple: `needs_reindex`:171, `upsert_chunks_and_calls`:323, `count_stale_files`:546, `list_stale_files`:592, `get_by_content_hash`:727, `get_embeddings_by_hashes`:750, `chunk_count`:793, `stats`:807, `get_chunks_by_origin`:902, `get_chunks_by_origins_batch`:926, `get_chunks_by_name`:970, `get_chunks_by_names_batch`:994, `search_chunks_by_signature`:1041, `get_chunk_with_embedding`:1080, `get_chunks_by_ids`:1104, `get_embeddings_by_ids`:1124, `all_chunk_identities`:1270, `all_chunk_identities_filtered`:1278, `embedding_batches`:1475)
- **Description:** 6 of 22 public functions have spans. The remaining 16 include critical operations: `upsert_chunks_and_calls` (the combined write path used by `parse_file_all`), `stats` (used by `cmd_stats`), multiple batch getters. The `embedding_batches` iterator is used during HNSW index build and has no visibility.
- **Suggested fix:** Add spans. Use `debug_span!` for simple getters called in tight loops (`chunk_count`, `get_by_content_hash`), `info_span!` for batch operations and write paths.

#### OB-5: `parse_markdown_chunks` and `parse_markdown_references` missing spans
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:39, src/parser/markdown.rs:178
- **Description:** The markdown parser's two main public functions have no tracing whatsoever (zero `info_span!`, zero `warn!` in the entire 1017-line file). Markdown parsing can be slow for large files with many headings, and failures in heading detection or reference extraction are silent. Compare with `parse_file` in `parser/mod.rs` which has full span coverage.
- **Suggested fix:** Add `info_span!("parse_markdown_chunks", path = %path.display())` and `info_span!("parse_markdown_references", path = %path.display())`. Add `warn!` for degenerate cases (zero headings on large files, heading hierarchy inversions).

#### OB-6: `parse_notes` and `rewrite_notes_file` missing spans
- **Difficulty:** easy
- **Location:** src/note.rs:133, src/note.rs:183
- **Description:** Both public note I/O functions (`parse_notes`, `rewrite_notes_file`) lack entry spans. Lock contention, file size guard rejections, and TOML parse failures all happen without span context. The `rewrite_notes_file` function holds an exclusive lock for an entire read-modify-write cycle — tracing would help diagnose lock-wait bottlenecks.
- **Suggested fix:** Add `info_span!("parse_notes", path = %path.display())` and `info_span!("rewrite_notes_file", path = %notes_path.display())`.

#### OB-7: `HnswIndex::build`, `HnswIndex::save`, `HnswIndex::load` missing spans
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs:43, src/hnsw/persist.rs:117, src/hnsw/persist.rs:305
- **Description:** The three core HNSW lifecycle operations — build, save, load — have `tracing::info!` log messages but no `info_span!` entry spans. This means they show up in logs as isolated messages without timing or nesting context. `build_batched` (line 108) also lacks a span. The `HnswIndex::save` and `HnswIndex::load` do disk I/O and checksum verification — timing spans would help diagnose slow index save/load operations.
- **Suggested fix:** Add `info_span!` at entry to each. Include `n_vectors` for build, `basename` and `dir` for save/load.

#### OB-8: `CagraIndex::build` and `CagraIndex::build_from_store` missing spans
- **Difficulty:** easy
- **Location:** src/cagra.rs:79, src/cagra.rs:406
- **Description:** Both GPU index build functions use `tracing::info!` messages but no entry span. GPU operations can be slow (cuVS initialization, data transfer, build) — spans would provide timing visibility. The `search` method (line 152) also lacks a span, making GPU search latency invisible to tracing.
- **Suggested fix:** Add `info_span!("cagra_build", n_vectors)` and `info_span!("cagra_search", k)`.

#### OB-9: `load_references` missing entry span
- **Difficulty:** easy
- **Location:** src/reference.rs:42
- **Description:** `load_references` opens and loads all reference Store+HNSW indexes at startup. It has `warn!` for individual reference failures but no entry span. For projects with multiple large references, this can be a significant startup cost with no timing visibility.
- **Suggested fix:** Add `info_span!("load_references", count = configs.len())`.

#### OB-10: `enumerate_files` missing entry span
- **Difficulty:** easy
- **Location:** src/cli/files.rs:11
- **Description:** `enumerate_files` walks the project directory to find indexable files. This is called during `cqs index` and `cqs watch`, and can be slow on large repos or network mounts. No span means slow file enumeration is invisible.
- **Suggested fix:** Add `info_span!("enumerate_files")` and log the file count on completion.

#### OB-11: `extract_fenced_blocks` missing span — markdown injection is silent
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:1017
- **Description:** `extract_fenced_blocks` is the entry point for parsing fenced code blocks from markdown (the new multi-grammar injection feature). It has no tracing. The downstream `parse_fenced_blocks` (line 600 in `parser/mod.rs`) has a span, but blocks that fail language detection or are skipped are silent.
- **Suggested fix:** Add `info_span!("extract_fenced_blocks")` and `debug!` for skipped blocks with unrecognized languages.

#### OB-12: Inconsistent span levels — `find_injection_ranges` uses `info_span!` but is a hot inner loop
- **Difficulty:** easy
- **Location:** src/parser/injection.rs:55
- **Description:** `find_injection_ranges` uses `info_span!` despite being called per-file during parsing (inside the rayon parallel iterator in `parse_file`). For a codebase with 1000 HTML files, this emits 1000 info-level spans. Compare with `extract_types` in `calls.rs:125` which correctly uses `info_span!` because it's called once per file. The issue is that `find_injection_ranges` is called per injection rule per file, not per file.
- **Suggested fix:** Change to `debug_span!` to reduce noise at info level. Callers already have `info_span!` (e.g., `parse_file`, `parse_file_all`).

## API Design

#### AD-1: CLI vs batch default `--limit` divergence — Scout (5 vs 10), Where (3 vs 5)
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:619 (Scout limit=5), src/cli/batch/commands.rs:202 (Scout limit=10), src/cli/mod.rs:608 (Where limit=3), src/cli/batch/commands.rs:213 (Where limit=5)
- **Description:** The `scout` command defaults to `--limit 5` in CLI mode but `--limit 10` in batch mode. The `where` command defaults to `--limit 3` in CLI and `--limit 5` in batch. Users switching between `cqs scout` and batch `scout` get different result counts with no flag change. This is confusing and makes scripted vs interactive use inconsistent.
- **Suggested fix:** Align defaults. Use `5` for both Scout and `3` for both Where (CLI values are the more conservative/tested defaults).

#### AD-2: `SearchResult` dual serialization — `#[derive(Serialize)]` nests, `to_json()` flattens
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:193 (`#[derive(Serialize)]`), src/store/helpers.rs:206 (`to_json()`)
- **Description:** `SearchResult` has both `#[derive(serde::Serialize)]` and a manual `to_json()` method that produce different JSON shapes. The derive nests: `{ "chunk": { "file": ..., "name": ... }, "score": ... }`. The manual method flattens: `{ "file": ..., "name": ..., "score": ... }`. All current callers use the manual `to_json()` / `to_json_relative()`, so the `Serialize` derive produces a shape nobody consumes. Same issue on `NoteSearchResult` (line 333).
- **Suggested fix:** Add `#[serde(flatten)]` on the `chunk` field in `SearchResult` so the `Serialize` output matches the flat shape. Then callers can use `serde_json::to_value()` instead of the manual `to_json()`. Or remove the `Serialize` derive if it's truly unused.

#### AD-3: 5 store types missing `Serialize` — manual JSON assembly required
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:310 (`NoteStats`), src/store/helpers.rs:357 (`StaleFile`), src/store/helpers.rs:368 (`StaleReport`), src/store/helpers.rs:379 (`ParentContext`), src/store/calls.rs:77 (`CallStats`)
- **Description:** The v0.19.4 audit (AD-3) added `Serialize` to core types (`ChunkSummary`, `CallerInfo`, etc.) but missed these 5. `StaleFile` and `StaleReport` force `cmd_stale` to build JSON manually (stale.rs:29-53). `NoteStats` forces `cmd_health` to destructure fields. `CallStats` forces `cmd_stats` to build JSON manually. This was the exact pattern the previous audit sought to eliminate.
- **Suggested fix:** Add `#[derive(serde::Serialize)]` to all 5 types. For `StaleFile`, add `#[serde(serialize_with = "...")]` to normalize the `origin` path, or rename to `file` with a `#[serde(rename)]`.

#### AD-4: Module visibility inconsistency — `diff_parse`, `drift`, `review` are `pub mod` but peers are `pub(crate)`
- **Difficulty:** easy
- **Location:** src/lib.rs:67 (`diff_parse`), src/lib.rs:68 (`drift`), src/lib.rs:78 (`review`)
- **Description:** `diff_parse`, `drift`, and `review` are declared as `pub mod` while their functional peers (`gather`, `scout`, `onboard`, `related`, `where_to_add`, `task`, `impact`, `diff`) are `pub(crate)`. There's no external consumer (CLAUDE.md: "nobody else is using cqs but us"), so these should be `pub(crate)` with selective re-exports. `drift` already has selective re-exports via `pub use drift::{...}` making the `pub mod` redundant.
- **Suggested fix:** Change `pub mod diff_parse` → `pub(crate) mod diff_parse`, `pub mod drift` → `pub(crate) mod drift`, `pub mod review` → `pub(crate) mod review`. Add `pub use` re-exports for any items needed by the binary crate.

#### AD-5: 5 command handlers still accept unused `_cli: &Cli` parameter
- **Difficulty:** easy
- **Location:** src/cli/commands/task.rs:19, src/cli/commands/impact_diff.rs:18, src/cli/commands/related.rs:24, src/cli/commands/onboard.rs:9, src/cli/commands/scout.rs:9
- **Description:** Prior audits (v0.19.2 AD-5, v0.19.4 AD-6) fixed 6+ handlers that accepted unused `_cli: &Cli`. 5 remain. These parameters add noise to the API and complicate refactoring (callers must pass `&cli` even though the handler ignores it).
- **Suggested fix:** Remove the `_cli` parameter from all 5 handlers. Update the dispatch in `cli/mod.rs` to not pass `&cli` to these commands.

#### AD-6: Inconsistent `_with_*` naming across library functions
- **Difficulty:** medium
- **Location:** src/gather.rs:313 (`_with_graph`), src/scout.rs:135 (`_with_options`), src/scout.rs:170 (`_with_resources`), src/task.rs:91 (`_with_resources`), src/where_to_add.rs:125 (`_with_embedding`), src/where_to_add.rs:147 (`_with_options`), src/impact/diff.rs:86 (`_with_graph`), src/impact/hints.rs:21 (`_with_graph`), src/impact/hints.rs:37 (`_with_graph_depth`)
- **Description:** The "bring your own dependency" pattern uses inconsistent suffixes: `_with_graph` (pre-loaded call graph), `_with_resources` (pre-loaded embedder + graph), `_with_options` (configurable parameters), `_with_embedding` (pre-computed embedding), `_with_graph_depth` (graph + depth override). There's no pattern a new contributor can follow. `_with_resources` and `_with_graph` serve different abstraction levels but their names don't indicate this.
- **Suggested fix:** Standardize on a convention. Suggestion: `_with_graph` when only the call graph is pre-loaded, `_with_context` when multiple shared resources (embedder, graph, etc.) are pre-loaded, `_with_options` when the function accepts a config struct. Document the convention in CONTRIBUTING.md. Not urgent — functional correctness is fine.

#### AD-7: `DiffHunk` missing `Serialize` derive
- **Difficulty:** easy
- **Location:** src/diff_parse.rs:14
- **Description:** `DiffHunk` is a `pub` type in a `pub mod` with no `Serialize`. The `impact-diff` command manually constructs JSON for diff hunks. Since `impact-diff` outputs changed functions (not raw hunks) this is low-impact, but it breaks the pattern established by other output types.
- **Suggested fix:** Add `#[derive(serde::Serialize)]` to `DiffHunk`.

## Error Handling

#### EH-1: `parse_fenced_blocks` silently skips `set_language`, `parse`, and `get_query` failures
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:618-630
- **Description:** Three `continue` statements silently skip fenced code blocks when `set_language` fails (grammar/ABI mismatch), `parse` returns None (tree-sitter timeout/cancellation), or `get_query` fails (query compilation error). None of these emit a `tracing::warn!` or `debug!`. Compare with `parse_file` at line 252 which logs `warn!` for chunk extraction failures, and `parse_injected_chunks` at line 294 which logs injection failures. Fenced block failures are completely invisible.
- **Suggested fix:** Add `tracing::debug!` for each `continue` path. Use `debug!` (not `warn!`) because fenced blocks with invalid content are common in documentation.

#### EH-2: `serde_json::to_value(c).ok()` silently drops chunks from JSON output (4 locations)
- **Difficulty:** easy
- **Location:** src/task.rs:249, src/cli/batch/handlers.rs:414, src/cli/commands/gather.rs:117, src/cli/commands/task.rs:375
- **Description:** Four locations use `.filter_map(|c| serde_json::to_value(c).ok())` to serialize chunks for JSON output. If serialization fails, the chunk is silently dropped from results. Since `ChunkSummary` derives `Serialize`, failures are unlikely in practice -- but when they do happen (e.g., NaN float values, invalid UTF-8 in cached data), the user gets fewer results than expected with no diagnostic.
- **Suggested fix:** Replace `.ok()` with `.map_err(|e| tracing::warn!(error = %e, "Failed to serialize chunk")).ok()` so failures are visible in tracing output.

#### EH-3: `config.rs` uses `map_err(|e| anyhow!(..., e))` instead of `.with_context()` -- loses error chain
- **Difficulty:** easy
- **Location:** src/config.rs:294, src/config.rs:315
- **Description:** `add_reference_to_config` converts TOML parse and serialize errors via `.map_err(|e| anyhow::anyhow!("Failed to parse {}: {}", config_path.display(), e))`. This format-stringifies the source error, losing the `#[source]` chain. `anyhow`'s display already appends source errors, so `.with_context()` would show both the context message AND the source error with proper chain formatting. Compare with `project.rs:44-45` which correctly uses `.with_context(|| format!(...))` for the same pattern.
- **Suggested fix:** Replace with `.with_context(|| format!("Failed to parse {}", config_path.display()))` and `.with_context(|| "Failed to serialize reference config")`.

#### EH-4: `StoreError::Runtime(String)` catch-all stringifies source errors (8 locations)
- **Difficulty:** hard
- **Location:** src/store/mod.rs:196, 291, 738, 749; src/store/notes.rs:27; src/store/helpers.rs:699
- **Description:** `StoreError::Runtime(String)` is used as a catch-all for heterogeneous error types (tokio `io::Error`, `serde_json::Error`, poisoned lock `PoisonError`, embedding dimension mismatches). Each is converted via `.map_err(|e| StoreError::Runtime(e.to_string()))` or `format!`, losing the source error chain. Callers cannot downcast to determine the root cause. This is a design-level issue -- the variant covers 4+ distinct error types that could benefit from dedicated variants.
- **Suggested fix:** Wrap as `Runtime(Box<dyn std::error::Error + Send + Sync>)` to preserve the source error chain while remaining backward-compatible. Or add dedicated variants for `io::Error` (already distinct from `Io(sqlx::Error)`) and `serde_json::Error`. Low priority since cqs has no external consumers.

## Code Quality

#### CQ-1: Dead code — `Store::get_chunks_by_name` (singular) superseded by batch version
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:970
- **Description:** `get_chunks_by_name` has zero callers in production code. It was superseded by `get_chunks_by_names_batch` (line 994), which explicitly documents it exists "to avoid N+1 `get_chunks_by_name` calls." The singular version is only referenced in doc comments. No tests call it either.
- **Suggested fix:** Delete the method. Update doc comments on `get_chunks_by_names_batch` to remove the reference.

#### CQ-2: Dead code — `Store::search_chunks_by_signature` has no production callers
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1041
- **Description:** `search_chunks_by_signature` has zero callers in any CLI command, batch handler, or library function. It exists only as a definition + 3 test functions in `tests/store_test.rs` (lines 991-1084). The method was likely written for the `deps` command but never wired in — `deps` uses type-edge queries instead.
- **Suggested fix:** Delete the method and its 3 tests. If signature search is needed later, it can be re-added with proper integration.

#### CQ-3: CAGRA/HNSW index selection logic duplicated between `cmd_query` and `batch::build_vector_index`
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:116-154, src/cli/batch/mod.rs:276-306
- **Description:** The CAGRA threshold check, GPU availability test, fallback-to-HNSW logic, and associated log messages are duplicated verbatim. Both define `const CAGRA_THRESHOLD: u64 = 5000` locally. The batch module already has this extracted into `build_vector_index()`, but `cmd_query` inlines the same 38-line block. If the threshold or fallback logic changes, both sites must be updated independently.
- **Suggested fix:** Move `build_vector_index` to a shared location (e.g., `cli/mod.rs` or a new `cli/index_util.rs`) and call it from both `cmd_query` and `cmd_batch`. The function signature `fn build_vector_index(store: &Store, cqs_dir: &Path) -> Result<Option<Box<dyn VectorIndex>>>` already works for both callers.

#### CQ-4: `normalize_lang()` in markdown.rs duplicates language alias knowledge with no sync enforcement
- **Difficulty:** medium
- **Location:** src/parser/markdown.rs:957-1009
- **Description:** `normalize_lang` maps 50+ fenced code block tags (e.g., `"py"` -> `"python"`, `"kt"` -> `"kotlin"`) to cqs language name strings. These output strings must match `Language::from_str` inputs (defined by the `define_languages!` macro in `language/mod.rs`), but there's no compile-time or test-time enforcement. Currently `ini` is missing from `normalize_lang` but present as a Language, meaning ````ini` blocks in markdown are silently skipped. When new languages are added, `normalize_lang` requires a manual update with no failing test to catch the omission.
- **Suggested fix:** Add a test that verifies every non-`None` output of `normalize_lang` parses successfully via `Language::from_str`. Also add `"ini"` to `normalize_lang`. Optionally, consider deriving `normalize_lang` from `Language` metadata (each `LanguageDef` could include a `fenced_aliases: &[&str]` field), eliminating the parallel map entirely.

#### CQ-5: Markdown `parse_markdown_chunks` — 3 near-identical Chunk constructions (14 fields each)
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:55-70 (no headings), 85-99 (one heading), 144-159 (section loop)
- **Description:** The three code paths in `parse_markdown_chunks` each construct `Chunk { ... }` with all 14 fields. The structs differ only in `name`, `content`, and line range — `file`, `language`, `chunk_type`, `doc`, `parent_id`, `window_idx`, and `parent_type_name` are always the same values. This pattern is fragile: adding a new field to `Chunk` requires updating all 6 construction sites in `markdown.rs` (3 here + `extract_table_chunks` + `emit_table_window`).
- **Suggested fix:** Extract a helper like `fn make_markdown_chunk(path: &Path, name: String, signature: String, content: String, line_start: u32, line_end: u32, parent_id: Option<String>) -> Chunk` that fills in the constant fields. This would reduce each construction site to a single function call.

## Documentation

#### DOC-1: `lib.rs` doc comment lists 49 languages but `define_languages!` has 50 (Vue missing from list)
- **Difficulty:** easy
- **Location:** src/lib.rs:10
- **Description:** The `lib.rs` module doc comment lists 49 languages with "(49 languages)" label, but `define_languages!` in `src/language/mod.rs` now has 50 variants (Vue was added as uncommitted work). The doc comment enumerates every language but omits Vue. Similarly, `README.md` line 5 says "49 languages", line 477 says "49 languages", `Cargo.toml` line 6 says "49 languages", and `CONTRIBUTING.md` line 70 says "49 languages supported". The README Supported Languages section lists 49 bullet points without Vue.
- **Suggested fix:** When Vue is committed, update the count to 50 in: `src/lib.rs:10`, `Cargo.toml:6`, `README.md:5,477`, `CONTRIBUTING.md:70`. Add Vue bullet to README Supported Languages section and `vue.rs` to CONTRIBUTING.md architecture listing.

#### DOC-2: Feature flag doc comment in `language/mod.rs` missing 10 language flags
- **Difficulty:** easy
- **Location:** src/language/mod.rs:7-52
- **Description:** The module doc comment lists feature flags for language support, but omits 10 that exist in the `define_languages!` macro and `Cargo.toml`: `lang-fsharp`, `lang-powershell`, `lang-html`, `lang-json`, `lang-xml`, `lang-ini`, `lang-svelte`, `lang-razor`, `lang-vbnet`, `lang-markdown`. The list covers 39 flags + `lang-all` but 49 languages (soon 50) exist. This makes the doc comment unreliable for determining available feature flags.
- **Suggested fix:** Add the 10 missing feature flag entries to the doc comment. Keep them in the same order as the `define_languages!` invocation.

#### DOC-3: `lib.rs` comment says "Internal modules — not part of public library API" but 3 modules are `pub`
- **Difficulty:** easy
- **Location:** src/lib.rs:63-83
- **Description:** Line 63 says "Internal modules - not part of public library API" but `diff_parse` (line 67), `drift` (line 68), and `review` (line 78) are declared `pub mod` — making them part of the public API surface. The comment is misleading: a user looking at rustdoc would see these modules. Note: overlaps with AD-4 (API Design finding on visibility).
- **Suggested fix:** Either change the 3 modules to `pub(crate)` (with selective re-exports), or update the comment to note the exceptions. The former is preferred per CLAUDE.md ("nobody else is using cqs but us").

#### DOC-4: CLAUDE.md skills list incomplete — 9 of 14 skills listed
- **Difficulty:** easy
- **Location:** CLAUDE.md:33-41
- **Description:** The Skills section lists 9 skills (`/update-tears`, `/groom-notes`, `/release`, `/audit`, `/pr`, `/cqs`, `/cqs-bootstrap`, `/cqs-plan`, `/reindex`) but `.claude/skills/` has 14 directories. Missing: `/docs-review`, `/migrate`, `/troubleshoot`, `/cqs-batch`, `/red-team`. These skills are listed in `CONTRIBUTING.md` but not in CLAUDE.md, so Claude agents won't know they exist unless they read CONTRIBUTING.md.
- **Suggested fix:** Add the 5 missing skills to the CLAUDE.md Skills section:
  - `/docs-review` -- check project docs for staleness
  - `/migrate` -- schema version upgrades
  - `/troubleshoot` -- diagnose common cqs issues
  - `/cqs-batch` -- batch mode with pipeline syntax
  - `/red-team` -- adversarial security audit

#### DOC-5: CHANGELOG `[Unreleased]` empty despite uncommitted Vue + markdown fenced block features
- **Difficulty:** easy
- **Location:** CHANGELOG.md:8
- **Description:** Significant uncommitted work (Vue language support, markdown fenced code block injection) is not documented in the `[Unreleased]` section. The CHANGELOG jumps from `[Unreleased]` (empty) to `[0.28.1]`. While not technically wrong (features aren't released), it means the features are invisible to anyone reading the CHANGELOG to understand current work.
- **Suggested fix:** Add entries under `[Unreleased]` for the uncommitted features: Vue language support, markdown fenced code block extraction/injection.

#### DOC-6: ROADMAP says "49 languages" but will be 50 when Vue commits
- **Difficulty:** easy
- **Location:** ROADMAP.md:5
- **Description:** Line 5 says "49 languages" — correct for committed code, but the same file at line 74 describes Vue as "Next" while the implementation is already in the working tree. Minor inconsistency — when Vue is committed, the count needs updating here too.
- **Suggested fix:** Update to "50 languages" when Vue is committed.

## Platform Behavior

#### PB-1: `parse_markdown_chunks` / `parse_markdown_references` don't normalize CRLF — rely on caller invariant
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:39, src/parser/markdown.rs:178
- **Description:** Both markdown parsing entry points accept `&str` source and immediately split on `.lines()`. While `.lines()` handles both `\n` and `\r\n`, the line count arithmetic for `line_start`/`line_end` uses `lines.len()` — if a caller passes un-normalized source, the line numbers are correct (Rust's `.lines()` strips both), but the `content` reconstructed via `lines[start..end].join("\n")` discards `\r` characters, causing `content_hash` to differ from a re-read of the same file with different line endings. In practice, all current callers (`parse_file`, `parse_file_all`) normalize CRLF before calling, but this invariant is undocumented and fragile — `parse_markdown_chunks` is `pub` and could be called directly (e.g., from tests or future callers). Compare with `extract_calls` in `calls.rs:29-32` which defensively normalizes CRLF at entry.
- **Suggested fix:** Add a defensive CRLF check at entry: `let source = if source.contains("\r\n") { Cow::Owned(source.replace("\r\n", "\n")) } else { Cow::Borrowed(source) };`. Or add a doc comment: `/// # Precondition: source must have LF line endings (CRLF pre-normalized by caller)`.

#### PB-2: `enumerate_files` extension matching is case-sensitive — `.RS`, `.Py` files skipped on case-preserving FS
- **Difficulty:** easy
- **Location:** src/lib.rs:371-375
- **Description:** The extension filter in `enumerate_files` does `ext.to_str().map(|ext| extensions.contains(&ext))` — the extension is NOT lowercased before comparison. The `extensions` list from `supported_extensions()` contains lowercase entries ("rs", "py", "ts"). On case-preserving filesystems (macOS HFS+, NTFS via WSL), files with uppercase extensions like `main.RS` or `script.PY` are silently skipped during indexing. The v0.28.1 audit fixed the analogous issue in `parse_file` (mod.rs:171), `extract_calls` (calls.rs:236), and `collect_events` (watch.rs:241) by lowercasing extensions, but `enumerate_files` was missed — it's the entry point that decides which files even get considered for indexing.
- **Suggested fix:** Add `.to_ascii_lowercase()` before comparison: `let ext = ext.to_ascii_lowercase(); extensions.contains(&ext.as_str())`. This matches the pattern used in `collect_events` at watch.rs:242.

#### PB-3: `find_project_root` walks to filesystem root on Windows drive letters — stops at `C:\` not CWD
- **Difficulty:** medium
- **Location:** src/cli/config.rs:28-65
- **Description:** `find_project_root` walks up from CWD looking for project markers. The termination condition is `current.parent() == None`. On Unix, this terminates at `/`. On Windows (or WSL with `/mnt/c/`), `Path::new("/mnt/c").parent()` returns `Some("/mnt")`, then `Some("/")`, then `None` — walking through unrelated mount points. While this works correctly (no marker found = returns CWD), it scans `/mnt/c/`, `/mnt/`, and `/` for `Cargo.toml`/`.git` markers. If a user has a `.git` directory at `/mnt/` or `/`, `find_project_root` will return that as the project root instead of CWD. This is a latent issue — no user has `/mnt/.git`, but it's theoretically wrong.
- **Suggested fix:** Add a stop sentinel: break if current is a filesystem root (`current == Path::new("/")` on Unix, or a drive root on Windows). Or limit walk depth (e.g., max 20 levels).

#### PB-4: `ensure_ort_provider_libs` hardcodes `:` as PATH separator — would fail on native Windows
- **Difficulty:** easy
- **Location:** src/embedder.rs:684
- **Description:** `ld_path.split(':')` is hardcoded for Unix PATH separator. The function is gated with `#[cfg(unix)]` so this is not currently broken, but the comment "Find target directory from LD_LIBRARY_PATH" and the `#[cfg(not(unix))]` stub suggest this might be extended to Windows in the future. On Windows, PATH uses `;` as separator, and the env var would be `PATH` not `LD_LIBRARY_PATH`. The `#[cfg(unix)]` guard makes this a non-issue today, but the stub comment "Windows/other platforms: CUDA libraries are typically in PATH already" is misleading — it implies the feature works on Windows when it doesn't.
- **Suggested fix:** No code change needed — the `#[cfg(unix)]` guard is correct. Update the stub comment to clarify: "Windows: GPU provider libraries must be manually placed on PATH. Automatic symlinking is Unix-only."

#### PB-5: `ProjectRegistry::save()` lock file opens registry path, not a separate `.lock` file — NTFS advisory lock only
- **Difficulty:** medium
- **Location:** src/project.rs:56-65
- **Description:** `ProjectRegistry::save()` locks the registry file itself (`projects.toml`) using `File::lock()`, then writes to a temp file and renames over it. On NTFS (WSL `/mnt/c/`), `File::lock()` is advisory-only — another process can still write to the file without acquiring the lock. The rename operation then clobbers the other process's writes. This is the same class of issue documented for HNSW files (persist.rs:19-28 warns about advisory locking on WSL), but `ProjectRegistry::save()` has no such warning. The `hnsw/persist.rs` code calls `warn_wsl_advisory_locking()` — `project.rs` doesn't.
- **Suggested fix:** Add `warn_wsl_advisory_locking`-style warning for registry saves on WSL/NTFS. Low impact because concurrent `cqs ref add` operations are rare, but consistency with HNSW locking warnings would be good.

#### PB-6: `validate_and_read_file` path traversal check may fail with mixed-case on case-insensitive FS
- **Difficulty:** medium
- **Location:** src/cli/commands/read.rs:35-41
- **Description:** `validate_and_read_file` canonicalizes both the file path and the project root, then checks `canonical.starts_with(&project_canonical)`. On case-insensitive filesystems (macOS HFS+, NTFS), `dunce::canonicalize` returns the filesystem's stored case, not the input case. If the project root was opened as `/mnt/c/Projects/CQS` but the filesystem stores it as `/mnt/c/Projects/cqs`, the `starts_with` check works because `dunce::canonicalize` normalizes to the stored case. However, on case-preserving but case-insensitive filesystems, if two different canonical forms exist (e.g., directory rename race), the check could fail spuriously. In practice, `dunce::canonicalize` resolves to the true filesystem path, so this is more of a documentation gap than a real bug.
- **Suggested fix:** Document the case-sensitivity assumption in a comment: `// Note: canonicalize resolves to filesystem-stored case, so this is case-correct on case-insensitive FS`.

#### PB-7: `cmd_watch` `RecommendedWatcher` uses inotify on WSL — `PollWatcher` not offered as fallback
- **Difficulty:** hard
- **Location:** src/cli/watch.rs:90
- **Description:** `RecommendedWatcher::new()` selects inotify on Linux (including WSL). On WSL2, inotify works for the Linux filesystem (`/home/...`) but is unreliable for Windows-mounted paths (`/mnt/c/...`) because the 9P filesystem server doesn't fully implement inotify semantics. The code warns at line 62-63 but doesn't offer a workaround. The `notify` crate provides `PollWatcher` as a cross-platform alternative that works reliably on `/mnt/c/`. Already documented in prior triage (PB-1 in v0.19.4 audit) as "existing behavior" — including here for completeness since it's the primary platform pain point.
- **Suggested fix:** Add a `--poll` flag to `cqs watch` that uses `PollWatcher` instead of `RecommendedWatcher`. On WSL, auto-detect if CWD is under `/mnt/` and suggest `--poll`. This was deferred previously as low priority since `cqs index` works, but it's the #1 WSL usability issue.

#### PB-8: `is_test_chunk` hardcodes both `/` and `\\` separators but `file` is always forward-slash normalized
- **Difficulty:** easy
- **Location:** src/lib.rs:212-216
- **Description:** `is_test_chunk` checks for both `file.contains("/tests/")` and `file.contains("\\tests\\")`, and both `file.starts_with("tests/")` and `file.starts_with("tests\\")`. However, by the time `is_test_chunk` is called, the `file` parameter has already been normalized to forward slashes by `normalize_path()` / `normalize_slashes()` during indexing. The backslash checks are dead code — they'll never match. This is harmless but misleading: a reader might think backslash paths are possible at this layer when they're not.
- **Suggested fix:** Remove the backslash variants and add a comment: `// file paths are forward-slash normalized by this point`. The SQL-level test path patterns in `store/calls.rs` (e.g., `%/tests/%`) correctly use only forward slashes.

## Extensibility

#### EX-1: `Pattern` enum has 4 manually-synced representations with no macro or sync test
- **Difficulty:** easy
- **Location:** src/structural.rs:10-61
- **Description:** The `Pattern` enum has 4 parallel representations that must stay in sync: the enum variants (line 10-17), `FromStr` match arms (line 22-28), `Display` match arms (line 39-46), and `all_names()` (line 53-60). Adding a new pattern (e.g., `Callback`, `Singleton`) requires editing all 4 sites plus the `matches` dispatch (line 82-89) and the error message string (line 30). The `test_all_names_covers_all_variants` test (line 259) checks count equality (`== 6`) but would pass if a variant was added to the enum AND all_names without updating FromStr/Display. Compare with `Language` and `ChunkType` which use `define_languages!` / `define_chunk_types!` macros to generate all 4 from a single declaration.
- **Suggested fix:** Either create a `define_patterns!` macro similar to `define_chunk_types!`, or add a roundtrip test that parses every `all_names()` entry and verifies `Display` produces the same string (the existing `test_pattern_display_roundtrip` does this but doesn't verify the count matches enum variant count via `std::mem::variant_count` or exhaustive matching).

#### EX-2: `chunk_importance` test detection hardcoded — not connected to language system's `test_markers`/`test_path_patterns`
- **Difficulty:** medium
- **Location:** src/search.rs:398-413
- **Description:** `chunk_importance` hardcodes `test_` and `Test` prefix checks for test function detection, and `_test.` / `test_` for test file detection. These patterns overlap with but don't match the language-driven `LanguageDef::test_markers` and `LanguageDef::test_path_patterns`. For example, Java uses `@Test` annotation (not `test_` prefix), Python has `pytest` conventions that differ from `test_` prefix. The v0.19.4 audit fixed the same issue in `find_dead_code` (EX-8, calls.rs), but `chunk_importance` in `search.rs` was missed. The result: dead code detection correctly uses language-aware test heuristics, but search result scoring uses hardcoded heuristics — a Java test class annotated with `@Test` gets full importance score instead of the 0.90 demotion.
- **Suggested fix:** Use the language-driven test detection. Since `chunk_importance` doesn't have a `Language` parameter, either add one (requires changing callers in `search_filtered`) or extract the name-based heuristic into a shared function that `find_dead_code` and `chunk_importance` both call. The file-based check should use `REGISTRY.all_test_path_patterns()` for SQL LIKE-style matching, though for scoring purposes the simple heuristic may be acceptable if documented as intentionally conservative.

#### EX-3: `normalize_lang` has no sync test verifying outputs match `Language::from_str`
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:957-1009
- **Description:** `normalize_lang` maps 50+ fenced code block tags to language name strings. These output strings must parse via `Language::from_str`, but there's no test enforcing this invariant. The CQ-4 finding (already reported) covers the duplication; this finding specifically flags the missing validation test. If someone adds a new language to `define_languages!` but typos the output in `normalize_lang` (e.g., `"objective-c"` instead of `"objc"`), fenced blocks in markdown silently fail to parse with no test catching the mismatch. Also, `"ini"` and `"markdown"` are valid `Language::from_str` inputs but missing from `normalize_lang`.
- **Suggested fix:** Add a test: `for (_, output) in normalize_lang_entries() { assert!(Language::from_str(output).is_ok(), "{} not a valid language", output); }`. This requires making `normalize_lang`'s mapping iterable (e.g., a const array of `(&str, &str)` pairs instead of a match).

#### EX-4: `where_to_add::extract_patterns` catch-all silently falls through for 42 of 50 languages
- **Difficulty:** medium
- **Location:** src/where_to_add.rs:323-449
- **Description:** `extract_patterns` has specific pattern extraction for 8 languages (Rust, Python, TypeScript/JavaScript, Go, C, Java, SQL, Markdown) and a `_ =>` catch-all for the remaining 42. The catch-all returns empty imports and "default" visibility. Languages like Kotlin (`import`), Swift (`import`), C# (`using`), PHP (`use`), Ruby (`require`), C++ (`#include`) all have well-known import patterns that would improve placement suggestions. The catch-all means `cqs where` gives significantly worse results for non-core languages — no import context and no visibility analysis. This is a known gap but not tracked.
- **Suggested fix:** At minimum, add C++ (`#include`), C# (`using`), Ruby (`require`), PHP (`use`), Kotlin (`import`), and Swift (`import`) — these are the most common patterns with minimal logic. A more extensible approach: add an `import_patterns: &[&str]` field to `LanguageDef` so the language system drives import extraction. Low priority since `where` is advisory and the core languages cover most usage.

#### EX-5: HNSW tuning parameters compile-time only — no config file override
- **Difficulty:** medium
- **Location:** src/hnsw/mod.rs:57-62
- **Description:** `MAX_NB_CONNECTION` (M=24), `EF_CONSTRUCTION` (200), and `EF_SEARCH` (100) are compile-time constants with no config file override. The doc comment (lines 52-56) suggests different values for different workloads: M=16 for small codebases, M=32 for large, lower ef_search for batch processing. Users can't tune these without recompiling. This was flagged in the v0.9.1 audit (EX-4: "HNSW tuning parameters compile-time only") and deferred as "by design for now." Since the project now has a config file system (`Config` in config.rs), the infrastructure exists to make at least `ef_search` configurable at runtime. `ef_construction` and `M` affect index build and are reasonably compile-time, but `ef_search` only affects query time and could be overridden per-query.
- **Suggested fix:** Add `ef_search: Option<usize>` to `Config`. Pass it through `SearchFilter` or as a parameter to `HnswIndex::search`. Keep `EF_SEARCH` as the default. Low priority — the current defaults work well for the 10k-100k chunk range that covers most projects.

#### EX-6: `name_match_score` scoring weights are inline magic numbers without named constants
- **Difficulty:** easy
- **Location:** src/search.rs:130-196
- **Description:** `NameMatcher::score` uses inline float literals for scoring tiers: `1.0` (exact match), `0.8` (name contains query), `0.6` (query contains name), `0.5` (max word overlap score). These thresholds control hybrid search ranking but aren't named constants — tuning requires reading the function to find which number means what. Compare with `NOTE_BOOST_FACTOR` (line 265), `RISK_THRESHOLD_HIGH` (impact/hints.rs:11), and `DEFAULT_NAME_BOOST` (cli/config.rs:20) which are all named constants.
- **Suggested fix:** Extract to named constants: `const NAME_SCORE_EXACT: f32 = 1.0`, `const NAME_SCORE_CONTAINS: f32 = 0.8`, `const NAME_SCORE_CONTAINED_BY: f32 = 0.6`, `const NAME_SCORE_MAX_OVERLAP: f32 = 0.5`. Low priority — the function is well-documented with comments and the values are unlikely to change frequently.

#### EX-7: `chunk_importance` demotion factors are inline magic numbers without named constants
- **Difficulty:** easy
- **Location:** src/search.rs:398-413
- **Description:** `chunk_importance` returns `0.90` for test functions/files and `0.95` for underscore-prefixed private helpers. These multipliers affect search ranking but aren't named constants. They're documented in the function's doc comment table (lines 389-391), but someone tuning search scoring has to find and modify inline float literals inside the function body. Related to EX-6 — both are in the same scoring pipeline.
- **Suggested fix:** Extract to named constants: `const IMPORTANCE_TEST: f32 = 0.90`, `const IMPORTANCE_PRIVATE: f32 = 0.95`. Group with the existing `NOTE_BOOST_FACTOR` constant since they're all search scoring parameters.

## Test Coverage

#### TC-1: 9 languages have no parser integration tests in `parser_test.rs` — C#, F#, PowerShell, Scala, Ruby, Vue, Svelte, Razor, VB.NET
- **Difficulty:** medium
- **Location:** tests/parser_test.rs, tests/fixtures/
- **Description:** `parser_test.rs` has integration tests for 41 languages but 9 are missing: C# (has 1 inline unit test only), F# (9 inline tests, no integration), PowerShell (6 inline, no integration), Scala (8 inline, no integration), Ruby (8 inline, no integration), Vue (6 inline, no integration), Svelte (6 inline, no integration), Razor (14 inline, no integration), VB.NET (12 inline, no integration). Integration tests exercise the full `Parser::parse_file()` pipeline including chunk extraction, line numbering, content hashing, and post-process hooks. Inline tests only test the tree-sitter query in isolation. The v0.26.0 audit (TC-1) added integration tests for Bash, HCL, Kotlin, Swift, and Objective-C — these 9 were not included.
- **Suggested fix:** Create fixture files (`sample.cs`, `sample.fs`, `sample.ps1`, `sample.scala`, `sample.rb`, `sample.vue`, `sample.svelte`, `sample.cshtml`, `sample.vb`) and add integration tests following the established pattern (parse fixture, assert chunk count, names, types, line numbers).

#### TC-2: `normalize_lang` has no direct unit tests — missing languages silently skipped
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:957-1009
- **Description:** `normalize_lang()` maps 50+ fenced code block tags to cqs language name strings, but has zero direct unit tests. It is exercised indirectly through `extract_fenced_blocks` tests, but those only test 3 aliases ("js", "py", "ts"). There is no test verifying that every non-None output of `normalize_lang` is a valid `Language::from_str` input. Currently `ini` is missing from the map (reported as CQ-4) — this gap would be caught by a sync test. When new languages are added, `normalize_lang` requires manual updates with no failing test to catch omissions.
- **Suggested fix:** Add two tests: (1) `test_normalize_lang_outputs_are_valid_languages` — iterate all non-None outputs and verify each parses via `Language::from_str`, (2) `test_normalize_lang_all_languages_have_aliases` — verify each enabled language with a grammar has at least one alias in `normalize_lang`. This catches both stale outputs and missing inputs.

#### TC-3: `extract_fenced_blocks` missing edge case tests — unclosed fences, nested fences, mixed fence types
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:1017-1083
- **Description:** The fenced block extractor has 8 tests covering basic cases (single block, aliases, unknown lang, tilde, metadata, empty), but is missing edge cases common in real markdown: (1) unclosed fences (``` with no closing) — silently reads to EOF, (2) nested fences (4-backtick fence containing 3-backtick example), (3) indented fences (spec allows 1-3 spaces), (4) mixed fence types (backtick open + tilde close — should NOT match per spec), (5) case sensitivity in language tag (`Rust` vs `rust`).
- **Suggested fix:** Add 5 edge case tests: `test_extract_fenced_blocks_unclosed`, `test_extract_fenced_blocks_nested`, `test_extract_fenced_blocks_indented`, `test_extract_fenced_blocks_mixed_fence_types`, `test_extract_fenced_blocks_case_insensitive_lang`.

#### TC-4: `build_risk_summary` never directly tested — only exercised through integration
- **Difficulty:** easy
- **Location:** src/review.rs:215-243
- **Description:** `build_risk_summary()` computes the aggregated risk breakdown (high/medium/low counts + overall level) from reviewed functions. It has zero direct tests — all coverage comes through `review_diff()` integration tests in `tests/review_test.rs`. The function's edge cases are not exercised: empty input (returns all-zero with Low), all-same-level inputs, and the high > medium > low priority logic.
- **Suggested fix:** Add 4 unit tests in `review.rs::tests`: `test_build_risk_summary_empty`, `test_build_risk_summary_all_high`, `test_build_risk_summary_mixed_levels`, `test_build_risk_summary_overall_priority`.

#### TC-5: `match_notes` in `review.rs` — note-matching logic only tested through one happy path
- **Difficulty:** easy
- **Location:** src/review.rs:183-212
- **Description:** `match_notes()` uses `path_matches_mention()` to correlate notes with changed files. The integration test `test_review_diff_with_relevant_notes` covers exact filename mention, but doesn't test: (1) notes that don't match any changed file, (2) multiple notes matching the same file, (3) notes with empty mentions list. The filtering and aggregation logic in `match_notes` has only one test path.
- **Suggested fix:** Add 2 test cases in `review_test.rs`: review with non-matching notes (verify empty `relevant_notes`), review with multiple notes matching same file (verify all appear).

#### TC-6: Fenced code block call-graph relationships untested
- **Difficulty:** medium
- **Location:** src/parser/mod.rs:361-363
- **Description:** `parse_file_all()` extracts fenced blocks from markdown and parses them for chunks AND relationships. The existing tests (`test_fenced_blocks_parsed_as_chunks`, `test_fenced_blocks_multiple_languages`) only check chunk extraction via `parse_file()`. No test verifies that `parse_file_all()` extracts function calls from fenced code blocks. If call extraction silently fails for fenced blocks, the call graph for code examples in markdown documentation will have missing edges.
- **Suggested fix:** Add `test_fenced_blocks_call_extraction` using `parse_file_all()` on a markdown file containing a fenced Rust block with explicit function calls. Assert the returned `function_calls` vector contains expected callee names.

#### TC-7: `batch/handlers.rs` — 1306-line file with zero inline tests
- **Difficulty:** hard
- **Location:** src/cli/batch/handlers.rs
- **Description:** `batch/handlers.rs` (1306 lines) contains dispatch logic for all 30+ batch commands. It has zero inline tests. All testing is through `cli_batch_test.rs` (17 integration tests) via `assert_cmd`. Handler-level logic — parameter validation, output format construction, edge case handling — is only tested end-to-end. Integration tests are slow and can't test handler internals at the unit level.
- **Suggested fix:** Extract pure-function helpers from handlers (parameter parsing, output formatting) and add unit tests. Low priority since 17 integration tests provide reasonable common-path coverage.

#### TC-8: `run_ci_analysis` dead code path filtering — path edge cases untested
- **Difficulty:** easy
- **Location:** src/ci.rs:100-104
- **Description:** `run_ci_analysis` filters dead code to diff-touched files using `diff_files.iter().any(|f| d.chunk.file.ends_with(f))`. The `ci_test.rs` tests cover basic scenarios but don't test path edge cases: (1) suffix collision (`bar.rs` matching `foobar.rs` — prevented by `Path::ends_with` component matching, but untested), (2) backslash paths (`src\lib.rs` vs `src/lib.rs`).
- **Suggested fix:** Add 2 tests: `test_ci_dead_code_path_suffix_collision`, `test_ci_dead_code_cross_platform_path`.

## Robustness

#### RB-1: `extract_attribute_from_text` uses byte offset from lowercased text to slice original — panics on non-ASCII before match
- **Difficulty:** easy
- **Location:** src/language/razor.rs:197-209
- **Description:** `extract_attribute_from_text` calls `text.to_lowercase()` to produce `lower`, finds a byte position via `lower.find(&pattern)`, then uses that position to slice the original `text`: `&text[pos + pattern.len()..]`. The `to_lowercase()` transform can change byte lengths for non-ASCII characters (e.g., `\u{0130}` (Latin capital I with dot above) lowercases to 3 bytes from 2). If non-ASCII characters appear in the text *before* the matched attribute, the byte offset from `lower` won't correspond to the correct position in `text`, causing either an incorrect slice or a panic on a non-char boundary. In practice, HTML/Razor attributes are ASCII, so this is latent rather than active.
- **Suggested fix:** Use `to_ascii_lowercase()` instead of `to_lowercase()` — it's guaranteed to preserve byte lengths since it only transforms ASCII characters. This is semantically correct for HTML/Razor attribute matching.

#### RB-2: `detect_listing_language` same `to_lowercase()` byte offset bug as RB-1
- **Difficulty:** easy
- **Location:** src/language/latex.rs:143-145
- **Description:** Same pattern as RB-1: `text_lower = trimmed.to_lowercase()`, `pos = text_lower.find("language=")`, then `trimmed[pos + 9..]`. If `trimmed` contains non-ASCII characters before the `language=` attribute, the byte offset from the lowercased version will be incorrect for slicing the original string. LaTeX documents commonly contain non-ASCII characters (accented characters, math symbols) in listing options, making this more likely to trigger than the Razor variant.
- **Suggested fix:** Use `to_ascii_lowercase()` or find case-insensitively on the original string using byte-level matching.

#### RB-3: `normalize_lang` missing 5+ supported languages — fenced blocks silently skipped in markdown
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:957-1009
- **Description:** `normalize_lang` maps fenced code block language tags to cqs Language names but omits several supported languages: `"ini"`, `"vb"/"vbnet"/"vb.net"`, `"svelte"`, `"razor"/"cshtml"`, `"vue"`. All are supported languages with grammars, but can't be extracted from markdown fenced blocks. Users writing ` ```ini` or ` ```vue` blocks get zero chunks with no warning. This is the robustness aspect of the CQ-4 finding — the primary concern is silent data loss.
- **Suggested fix:** Add the missing mappings. At minimum: `"ini" => Some("ini")`, `"vb" | "vbnet" | "vb.net" => Some("vbnet")`, `"svelte" => Some("svelte")`, `"razor" | "cshtml" => Some("razor")`, `"vue" => Some("vue")`.

#### RB-4: `extract_fenced_blocks` unclosed fence silently eats rest of file
- **Difficulty:** easy
- **Location:** src/parser/markdown.rs:1047-1079
- **Description:** When `extract_fenced_blocks` finds an opening fence (e.g., ` ```rust`) but no matching closing fence, the inner `while i < lines.len()` loop at line 1051 consumes all remaining lines. The outer loop ends because `i == lines.len()`. All subsequent fenced blocks after the unclosed fence are lost because the parser is stuck inside the unclosed block. This is correct behavior (don't emit incomplete blocks), but it does so silently — no `tracing::debug!` or `warn!` indicates that blocks were lost. If a large markdown file has an accidental unclosed fence early, every fenced block after it disappears from the index.
- **Suggested fix:** After the inner while loop, if no close was found, add `tracing::debug!(lang = %lang_tag, open_line, "Unclosed fenced block — remaining blocks in file skipped")`.

#### RB-5: `parse_fenced_blocks` silently skips 3 failure modes with no logging (overlaps EH-1)
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:618-630
- **Description:** Three `continue` statements silently skip fenced code blocks when `set_language` fails (line 618), `parse` returns None (line 622-624), or `get_query` fails (line 627-629). None emit tracing output. Compare with `parse_file` at line 252 which logs `warn!` for extraction failures, and `parse_injected_chunks` at line 294 which logs injection failures. If all fenced blocks in a markdown file fail, the user gets zero chunks with no diagnostic explaining why. Primary concern is silent data loss during parsing.
- **Suggested fix:** Add `tracing::debug!` for each `continue` path. Use `debug!` (not `warn!`) because fenced blocks with invalid content are common in documentation markdown.

#### RB-6: `store/chunks.rs` `CandidateRow`/`ChunkRow` use panicking `row.get()` — 16+ sites
- **Difficulty:** medium
- **Location:** src/store/chunks.rs (~1290-1323, ~1340-1370), src/store/helpers.rs:75-121
- **Description:** Previously flagged as RB-2 in v0.19.4 audit (informational). `sqlx::Row::get()` panics if the column doesn't exist or the type doesn't match. All call sites use `get::<Type, _>("column_name")` where columns are from hardcoded SELECT queries in the same function, so column mismatches require a code change to the SELECT without updating the `from_row`. The risk is low but a schema migration bug or SQLite corruption could trigger a panic deep in the store layer with no recovery. This is the largest class of latent panics in production code (16+ sites across chunks.rs alone).
- **Suggested fix:** Previously marked informational. `try_get()` with `?` propagation would convert panics to errors. Not urgent — the current pattern is standard `sqlx` usage.

#### RB-7: `structural.rs` `is_recursive` joins all lines then searches — O(n) allocation for line-by-line check
- **Difficulty:** easy
- **Location:** src/structural.rs:189-196
- **Description:** `is_recursive` collects all lines into a Vec, then joins lines 1+ into a single String via `lines[1..].join("\n")` before doing `body.contains()`. For a 1000-line function, this allocates a ~40KB string to search. The `cqs structural recursion` command runs this for every function in the index. For 10,000 functions averaging 100 lines each, this is ~100MB of allocations. The search itself is fine (linear scan), but the allocation is unnecessary.
- **Suggested fix:** Search line-by-line instead of joining: `lines[1..].iter().any(|l| l.contains(&pattern1) || l.contains(&pattern2))`. Avoids the allocation entirely and short-circuits on first match.

## Algorithm Correctness

#### AC-1: `search_by_name` results not re-sorted by name-match score — order inconsistent with displayed scores
- **Difficulty:** easy
- **Location:** src/store/mod.rs:622-661
- **Description:** `search_by_name` fetches results from FTS5 (ordered by `bm25()`), then assigns each result a `score_name_match_pre_lower` score (1.0 for exact, 0.9 for prefix, 0.7 for substring, 0.0 for no match). However, the results are returned in BM25 order, not in name-match score order. This means a user searching for `parse_config` might see `test_parse_config` (name-match 0.7) ranked above `parse_config` (name-match 1.0) if BM25 prefers the former (e.g., due to content/signature term frequency). The `.score` field on each result reflects name-match precision, but the ordering reflects BM25 relevance — a mismatch between what the user sees (scores) and how results are ordered. Both CLI `--name-only` and batch `name_only` pass results directly to output without re-sorting.
- **Suggested fix:** Add `results.sort_by(|a, b| b.score.total_cmp(&a.score));` before returning from `search_by_name`. This makes ordering match the score the user sees. Alternatively, rename the score to indicate it's BM25-derived and display BM25 order explicitly.

#### AC-2: `apply_token_budget` can exceed budget due to `.max(1)` guarantee — `used` exceeds `budget` with no cap
- **Difficulty:** easy
- **Location:** src/cli/commands/review.rs:97-98, src/cli/commands/ci.rs:96-97
- **Description:** When `max_callers` computes to 0 (budget exhausted by overhead + changed functions + notes), the `.max(1)` on the truncation forces at least 1 caller and 1 test to be kept, adding `tokens_per_caller + tokens_per_test` (33 tokens) beyond the budget. The returned `used` value can significantly exceed `budget`. The warning message "Output truncated to ~{used} tokens (budget: {budget})" is then misleading — it says output was truncated to a value larger than the budget. While the guarantee of at least 1 item is intentional and useful, the warning text implies the output fits within the budget when it doesn't.
- **Suggested fix:** Adjust the warning message to say "Output limited to ~{used} tokens (budget: {budget}, minimum 1 caller + 1 test guaranteed)" or clamp `used` to `budget` in the warning. Alternatively, skip the `.max(1)` when the budget has already been exceeded (only guarantee 1 item when the budget allows at least 1 item's worth). Both functions (review.rs and ci.rs) have identical code and should be fixed together.

#### AC-3: `EmbeddingBatchIterator::next()` uses recursion for corrupt-embedding skip — unbounded stack depth
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1544-1545
- **Description:** When a batch of rows is fetched but all embeddings fail validation (wrong dimensions or corrupt bytes), `batch.is_empty()` is true but `rows_fetched > 0`. The iterator handles this by calling `self.next()` recursively to fetch the next batch. If a large contiguous range of rows has corrupt embeddings (e.g., a failed migration left thousands of rows with wrong-dimension embeddings), this recursion would create one stack frame per batch. With a batch size of 10,000 and 100,000 corrupt rows, that's 10 recursive calls — fine. But with batch size 1 (allowed by the API) and 10,000 corrupt rows, it's 10,000 frames — potential stack overflow. While the production batch size is 10,000 (safe), the function accepts arbitrary `batch_size` from callers.
- **Suggested fix:** Replace recursion with a loop:
  ```rust
  Ok((batch, _, max_rowid)) => {
      self.last_rowid = max_rowid;
      if batch.is_empty() {
          continue; // loop instead of recurse
      } else {
          return Some(Ok(batch));
      }
  }
  ```
  Wrap the entire `match` in a `loop { ... }` and use `continue`/`return` instead of recursive `self.next()`.

#### AC-4: `index_pack` and `token_pack` always include first item regardless of budget — downstream `used` can exceed budget silently
- **Difficulty:** easy
- **Location:** src/cli/commands/task.rs:62-63, src/cli/commands/mod.rs:135-137
- **Description:** Both greedy knapsack functions guarantee at least one item is included even when it exceeds the budget (`kept.is_empty()` / `!kept_any` check). The returned `used` value can then exceed `budget`. Downstream code like `waterfall_pack` uses `remaining = remaining.saturating_sub(used)` which clamps to 0, so subsequent sections get zero budget. This is correct behavior (one giant code chunk shouldn't starve all other sections), but the surplus calculation `scout_budget.saturating_sub(scout_used)` will produce 0 surplus (correct) while `remaining` could have already been driven to 0 before accounting for subsequent guaranteed-first-items. In the worst case, each of the 5 waterfall sections could exceed its budget by one item, accumulating a multi-hundred-token overrun that's invisible in the final `token_count` field.
- **Suggested fix:** This is by-design behavior (guarantee at least 1 result per section). Document the "first-item guarantee" contract explicitly in both functions. If exact budget adherence is needed, add a `strict: bool` parameter that skips the first-item guarantee when `true`.

## Data Safety

#### DS-8: GC uses wrong HNSW filename — `index.hnsw.id_map.json` instead of `index.hnsw.ids`
- **Difficulty:** easy
- **Location:** src/cli/commands/gc.rs:71
- **Description:** The GC command hardcodes a list of HNSW files to delete before rebuild: `["index.hnsw.graph", "index.hnsw.data", "index.hnsw.checksum", "index.hnsw.id_map.json"]`. The last entry `index.hnsw.id_map.json` is a stale filename from a previous version. The actual HNSW ID map file is `index.hnsw.ids` (as defined in `HNSW_ALL_EXTENSIONS` in `src/hnsw/persist.rs:35`). Consequence: GC's pre-rebuild cleanup never deletes the stale HNSW IDs file, so if GC is interrupted between deletion and rebuild, the old `.ids` file remains and a subsequent `HnswIndex::load()` may load outdated IDs paired with missing graph/data files (load will fail on checksum mismatch, so no data corruption, but it's an unnecessary failure mode). The watch code at `watch.rs:335` correctly uses `HNSW_ALL_EXTENSIONS`, making this a GC-only divergence.
- **Suggested fix:** Replace the hardcoded list at `gc.rs:67-72` with `cqs::hnsw::HNSW_ALL_EXTENSIONS`, matching what `watch.rs` already does. This eliminates the divergence and ensures future filename changes are automatically reflected.

#### DS-9: `config.rs` `add_reference_to_config` reads config via separate `read_to_string` after locking config file itself
- **Difficulty:** medium
- **Location:** src/config.rs:276-288
- **Description:** `add_reference_to_config` opens the config file itself for locking (`OpenOptions::new().read(true).write(true).create(true).truncate(false).open(config_path)`), then acquires an exclusive lock on it. However, it then reads the file content via a separate `std::fs::read_to_string(config_path)` call (line 284) rather than reading from the locked file handle. On platforms where advisory locks don't prevent other processes from reading/writing, this creates a TOCTOU gap — another process could modify the file between the lock acquisition and the read. This is the same anti-pattern that was fixed in `note.rs` (which reads from the locked fd directly). The `remove_reference_from_config` function at line 371 has the identical issue.
- **Suggested fix:** Read from the locked file handle (`lock_file`) using `std::io::Read::read_to_string()` instead of the separate `std::fs::read_to_string()`, mirroring the pattern already used in `note.rs:222-230`. Also apply the same fix to `remove_reference_from_config`.

#### DS-10: `rewrite_notes_file` copy fallback loses exclusive lock protection
- **Difficulty:** medium
- **Location:** src/note.rs:264-281
- **Description:** `rewrite_notes_file` acquires an exclusive lock on the notes file (`lock_file.lock()`) and holds it for the entire read-modify-write cycle. The write path uses `std::fs::rename(&tmp_path, notes_path)` which is atomic. However, when rename fails (EXDEV on cross-device mounts), the fallback uses `std::fs::copy(&tmp_path, notes_path)` which is NOT atomic — it opens the destination, truncates it, and writes. During this non-atomic copy, a concurrent reader acquiring a shared lock would see truncated/partial content. The lock on `lock_file` (the original file descriptor before rename) may no longer protect the same inode after copy overwrites the path. This is a narrow race window but could corrupt a concurrent `parse_notes()` read on cross-device setups (Docker overlayfs, NFS).
- **Suggested fix:** On the copy fallback path, write to a second temp file in the destination directory (guaranteed same device), then rename that. If both renames fail (shouldn't happen for same-device), then fall back to copy. Alternatively, document the limitation for cross-device mounts.

#### DS-11: `extract_relationships` in `index.rs` is not transactional with chunk upserts
- **Difficulty:** medium
- **Location:** src/cli/commands/index.rs:124-137
- **Description:** In `cmd_index`, chunks are upserted via `run_index_pipeline` (line 87), then relationships are extracted in a separate pass via `extract_relationships` (line 131). If the process is interrupted between these two phases, chunks exist in the database without their call graph or type edges. This was documented in previous audit (DS-3 in v0.19.4 triage) as "acceptable (reindex recovers)". The watch path (`watch.rs`) correctly uses `upsert_chunks_and_calls` for atomic chunk+call transactions and upserts type edges immediately after. The `cmd_index` path still has this gap because the pipeline architecture sends chunks through parse->embed->store stages before relationship extraction. This is a design tension, not a bug — documenting for completeness since the previous finding was "acceptable" but the root cause remains.
- **Suggested fix:** Already marked acceptable in prior audit. The pipeline would need restructuring to eliminate this gap. Consider adding a metadata flag (`relationships_complete = false`) that gets set to `true` after `extract_relationships` finishes, so `cqs index` can resume from the relationship extraction phase on restart.

#### DS-12: `notes_need_reindex` uses second-precision mtime comparison — sub-second edits missed
- **Difficulty:** easy
- **Location:** src/store/notes.rs:234-254
- **Description:** `notes_need_reindex` compares file mtime (as seconds since epoch, via `duration_since(UNIX_EPOCH).as_secs() as i64`) with the stored `file_mtime` column. The `as_secs()` call truncates sub-second precision. If a notes file is modified twice within the same second (e.g., by a script or rapid manual edits), the second modification will have the same truncated mtime as the stored value, and `notes_need_reindex` returns `None` (no reindex needed), causing the update to be silently skipped. The same pattern exists in the chunk staleness check (`needs_reindex` in `chunks.rs`). This is a known limitation of second-precision mtime comparison and is unlikely to cause issues in practice (notes are manually edited, not scripted).
- **Suggested fix:** Use `as_millis()` or `as_nanos()` for mtime storage and comparison, or accept this as a known limitation. On filesystems with sub-second mtime support (ext4, APFS), this would catch rapid edits. On FAT/NTFS (2-second precision), no change needed.

#### DS-13: HNSW `count_vectors` reads ID map without any file lock
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:442-485
- **Description:** `count_vectors` opens and reads the HNSW `.ids` file directly without acquiring any file lock (shared or exclusive). In contrast, `load()` acquires a shared lock and `save()` acquires an exclusive lock. If `count_vectors` is called while a concurrent `save()` is in progress (during the rename from temp to final), it could read a partially-written or inconsistent file. The function handles parse failures gracefully (returns `None` with a warning), so this won't crash, but it could return an incorrect vector count. This is used by `cqs stats` which is a read-only informational command.
- **Suggested fix:** Add shared lock acquisition before reading, matching the `load()` pattern. Alternatively, accept this as informational — `cqs stats` showing a stale count during a concurrent index build is harmless.

## Resource Management

#### RM-1: CAGRA `dataset` field retains full embedding matrix for lifetime of index — no release path
- **Difficulty:** hard
- **Location:** src/cagra.rs:64, src/cagra.rs:109-121
- **Description:** `CagraIndex` stores `dataset: Array2<f32>` containing the full embedding matrix (n_vectors x 769 dims x 4 bytes = ~50MB for 17k vectors) for the lifetime of the index. This is necessary because cuVS `search()` consumes the index and `rebuild_index_with_resources()` needs the dataset to rebuild. However, once the index is built on the GPU, this CPU-side copy is only needed for rebuild after search. In batch mode (`BatchContext`), the CAGRA index is held for the entire session via `OnceLock`, meaning this ~50MB+ stays allocated even during idle periods or non-search commands. Related to existing issue #389 (GPU memory), but this is specifically the CPU-side duplication.
- **Suggested fix:** Possible approaches: (1) After a configurable number of idle seconds, drop the CAGRA index entirely and rebuild from store on next search — this would free both CPU `dataset` and GPU memory. (2) Accept as designed — the CAGRA threshold is 5000+ chunks, meaning any project using CAGRA already has significant memory overhead from SQLite and HNSW. Document the tradeoff in the batch mode `VRAM cost` comment.

#### RM-2: `cqs watch` rebuilds full HNSW index on every file change — O(total_chunks) rebuild for O(1) changes
- **Difficulty:** hard
- **Location:** src/cli/watch.rs:324, src/cli/commands/index.rs:286-302
- **Description:** When `cqs watch` detects file changes, it re-parses and re-embeds the changed files (efficient, O(changed)), then calls `build_hnsw_index()` which rebuilds the entire HNSW index from scratch by streaming all embeddings from SQLite. For a 17k-chunk project, this reads ~50MB of embeddings from disk and builds a new HNSW graph on every single file save. The previous HNSW index files on disk are overwritten, so peak memory briefly holds both the old (in-memory if loaded by a concurrent search) and new HNSW data. For a 50k-chunk project, this is ~300MB of embedding I/O per file save.
- **Suggested fix:** Implement incremental HNSW updates. `hnsw_rs` supports `parallel_insert_data` on existing indexes — new/changed chunks can be inserted without rebuilding. Deleted chunks are harder (HNSW doesn't support removal), but a periodic full rebuild (e.g., every 100 incremental updates) would amortize the cost. The `content_hashes` return value from `reindex_files` is already collected for this purpose but unused.

#### RM-3: `BatchContext` caches call graph, test chunks, file set, and notes in `OnceLock` — never released during long sessions
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:55-75
- **Description:** `BatchContext` caches several data structures in `OnceLock` fields that are never invalidated or released during a session: `call_graph` (all call edges — up to 500K string pairs), `test_chunks` (all test chunk summaries), `file_set` (all file paths as `PathBuf`), `notes_cache` (all parsed notes). For `cqs chat` sessions that run for hours, these caches can become stale AND hold memory that's no longer needed. The embedder and reranker have idle timeout clearing (`check_idle_timeout`), but these data caches do not. For a 50k-chunk project with a large call graph (~10K edges), this is ~5-10MB of string data held indefinitely.
- **Suggested fix:** Add a `refresh_caches()` method that resets the `OnceLock` fields (or switch to `RwLock<Option<T>>` like `notes_summaries_cache`). Call it after a configurable number of commands or idle timeout. Alternatively, accept as designed — batch/chat sessions are typically short-lived, and the data stays valid unless the index changes (which would require a separate `cqs index` run anyway).

#### RM-4: `index_notes_from_file` creates a separate `Embedder` during `cqs index` — redundant model path resolution
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:269
- **Description:** After the main indexing pipeline completes (which created and dropped an `Embedder` in the GPU/CPU embed threads), `index_notes_from_file` creates a brand new `Embedder::new()`. While the ONNX session is lazy (so ~500MB is only allocated if notes need embedding), the constructor still: (1) calls `select_provider()` (cached via static — cheap), (2) allocates an LRU cache, (3) creates `OnceCell` wrappers. More importantly, if notes need embedding, this creates a second ONNX session after the pipeline's session was already dropped. The pipeline's Embedder could have been passed through to avoid this.
- **Suggested fix:** Pass the `Embedder` from the pipeline thread (or create one in `cmd_index` and share it) to `index_notes_from_file`. The pipeline threads own their Embedder by value and drop it when they exit, so either (1) create the Embedder in `cmd_index` and share via `Arc`, or (2) accept the current approach since the pipeline's session is dropped before notes indexing starts, so there's no double-allocation of the heavy ONNX session.

#### RM-5: `extract_relationships` re-reads and re-parses every source file during `cqs index` — double I/O for call graph
- **Difficulty:** hard
- **Location:** src/cli/commands/index.rs:193-241
- **Description:** The doc comment on `extract_relationships` acknowledges this: "Note: This re-reads and re-parses files that were already parsed in `parser_stage`." During `cqs index`, every source file is read and parsed twice: once in the pipeline (parse -> embed -> store) and again for call graph extraction. For a large project with 1000+ files, this doubles the file I/O and tree-sitter parsing work. The watch path (`watch.rs`) uses `parse_file_all()` which combines parsing and relationship extraction in a single pass — but the pipeline can't do this because chunks must be stored before relationships reference them.
- **Suggested fix:** Already documented in the code comment. The proper fix is to restructure the pipeline to collect relationships during the parse stage (stage 1) and defer relationship storage to after the write stage (stage 3). The data is available from `parse_file_all()` — the relationships just need to be buffered. This was fixed for watch mode but remains in the bulk indexer. Medium-term, consider adding a "relationship buffer" to `ParsedBatch` that flows through the pipeline alongside chunks.

#### RM-6: `HnswIndex::build` (test-only) creates intermediate `Vec<Vec<f32>>` — 2x peak embedding memory
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs:67-73
- **Description:** The `build` method (soft-deprecated, test-only per its doc comment) calls `prepare_index_data` which flattens embeddings into `Vec<f32>`, then re-chunks this flat buffer into `chunks: Vec<Vec<f32>>` for the `hnsw_rs` API. This doubles the peak memory for embeddings: both `data: Vec<f32>` (N x 769 x 4 bytes) and `chunks: Vec<Vec<f32>>` (same size) exist simultaneously. For 50k vectors, this is ~300MB instead of ~150MB. Production code uses `build_batched` which doesn't have this issue.
- **Suggested fix:** Accept as-is — `build` is test-only (the doc comment says so, and `build_hnsw_index` unconditionally uses `build_batched`). The test datasets are small (<100 vectors). If anyone removes the "test-only" constraint, add a note about the 2x memory peak. Alternatively, refactor `build` to use slices of the flat buffer instead of copying into Vec<Vec<f32>>.

#### RM-7: `last_indexed_mtime` HashMap in watch mode grows unbounded for projects with file churn
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:119, src/cli/watch.rs:315-319
- **Description:** The `last_indexed_mtime: HashMap<PathBuf, SystemTime>` tracks per-file mtimes to deduplicate WSL/NTFS events. It grows every time a new file is indexed and only prunes when size exceeds 10,000 entries OR when size exceeds 1,000 and only 1 file changed. However, for projects where files are frequently created and deleted (e.g., build artifacts, generated code), entries for deleted files accumulate. The pruning condition (`root.join(f).exists()`) does file I/O for every entry, which is expensive when the map is large. The cap of 10,000 entries means up to 10,000 stale `PathBuf`s (~500KB) are held before cleanup triggers.
- **Suggested fix:** Change the pruning heuristic to trigger based on a counter (e.g., every 100 reindex cycles) rather than size-based. Or use a bounded data structure like an LRU cache with a fixed capacity. Current behavior is acceptable for typical projects but could surprise users with heavy file churn.

#### RM-8: SQLite `PRAGMA cache_size = -16384` (16MB) per connection x 4 connections = 64MB page cache
- **Difficulty:** easy
- **Location:** src/store/mod.rs:217
- **Description:** Each SQLite connection in the pool gets `PRAGMA cache_size = -16384` (16MB page cache) via `after_connect`. With `max_connections(4)`, the total SQLite page cache can reach 64MB. Combined with `mmap_size = 268435456` (256MB), a single Store can use up to 320MB of memory for SQLite caching alone. For `cqs batch` or `cqs chat` where both the project Store AND reference Stores may be open simultaneously, this multiplies. Two references = 3 Stores = 192MB page cache + 768MB mmap potential. The mmap pages are demand-paged by the OS and share physical pages, so the actual RSS impact depends on access patterns. The page cache, however, is private per-connection.
- **Suggested fix:** Consider reducing `cache_size` for read-only stores (`open_readonly` uses `max_connections(1)`, so it's already 16MB) and for the pool connections that handle read-only queries. The write path (connections 1-2) benefits from large caches; the search path (connections 3-4) may not need 16MB each. Alternatively, document the expected memory footprint. For most cqs users (single project, ~17k chunks, ~50MB DB), the actual cache utilization is well below 16MB per connection.

## Security

#### SEC-1: `BufRead::lines()` allocates full line before `MAX_BATCH_LINE_LEN` check — OOM on single huge line
- **Difficulty:** medium
- **Location:** src/cli/batch/mod.rs:394-404
- **Description:** The SEC-12 fix from v0.13.1 added a `MAX_BATCH_LINE_LEN` (1MB) check at line 404, but the check runs *after* `BufRead::lines()` has already read the entire line into a `String`. Rust's `BufReader::lines()` internally calls `read_line()` which grows the `String` buffer without bound until it hits `\n` or EOF. A single line without newlines (e.g., `yes | tr -d '\n' | cqs batch`) will allocate memory until OOM before the 1MB check is reached. The check only protects against processing the line — it doesn't prevent the allocation. This is the residual risk that SEC-12 didn't address. Threat model: local CLI tool, so exploitation requires controlling stdin to `cqs batch` or `cqs chat`, which in practice means either a malicious pipe or a compromised agent.
- **Suggested fix:** Replace `stdin.lock().lines()` with a bounded line reader. Use `BufRead::read_line()` in a loop with a running length check: read into a buffer, and if it exceeds `MAX_BATCH_LINE_LEN` before hitting `\n`, discard the rest of the line and emit an error. Alternatively, wrap the reader with `.take(MAX_BATCH_LINE_LEN as u64 + 1)` per line, though this requires more restructuring. The `chat` REPL (cli/chat.rs) uses `rustyline` which has its own bounded input handling and is not affected.

#### SEC-2: `is_webhelp_dir` detection walk follows symlinks through the initial `content/` join
- **Difficulty:** easy
- **Location:** src/convert/webhelp.rs:19-36
- **Description:** `is_webhelp_dir()` checks if a directory looks like a web help site by joining `dir + "content"` and walking it. The `content_dir.is_dir()` check at line 21 follows symlinks — if `content/` is a symlink pointing to an arbitrary directory, `is_dir()` returns true and the `WalkDir` at line 25 enumerates that target. While `WalkDir` defaults to `follow_links(false)` for entries *within* the walk, the initial root path (`content_dir`) is a symlink that was already resolved by `is_dir()`. The actual `webhelp_to_markdown()` at line 58 correctly uses `filter_entry(|e| !e.path_is_symlink())`, but `is_webhelp_dir()` in `convert_directory()` runs before conversion and probes the symlinked target for HTML files. The impact is limited: an attacker who controls a `content/` symlink inside a directory being converted gets information about whether HTML files exist at the symlink target, but no file content is read in the detection phase. The real conversion path is safe.
- **Suggested fix:** Add a symlink check before the `is_dir()` call: `if content_dir.symlink_metadata().map(|m| m.is_symlink()).unwrap_or(false) { return false; }`. This is consistent with the symlink filtering in `webhelp_to_markdown()` and `convert_directory()`.

#### SEC-3: `SECURITY.md` threat model understates "project files" trust level
- **Difficulty:** easy
- **Location:** SECURITY.md:14
- **Description:** The SECURITY.md threat model table says project files are "Trusted" with the note "Your code, indexed by your choice." However, `cqs convert` operates on external documents (PDFs, CHMs, HTML from third parties) and `cqs ref add` indexes arbitrary external codebases. Both paths handle untrusted data — crafted CHM archives, malicious HTML, attacker-controlled source trees. The codebase has extensive mitigations (symlink filtering, zip-slip containment, path traversal checks), but the documented threat model doesn't acknowledge that some project files may be untrusted. The v0.13.1 audit added several fixes for exactly these scenarios (SEC-9 through SEC-15). The `"What We Don't Protect Against"` section says "Malicious code in your project" won't be stopped, but doesn't mention malicious documents or reference sources.
- **Suggested fix:** Update the trust boundary table to distinguish between "project source code" (trusted) and "external documents/references" (semi-trusted — sanitized but not sandboxed). Add a row for "External documents (convert input)" with trust level "Semi-trusted" and note "Symlink/path traversal mitigated, format parsing delegated to libraries." This documents the actual threat model that the code already defends against.

## Performance

#### PF-1: `get_call_graph()` uncached — 15 call sites each do a full `function_calls` table scan
- **Difficulty:** medium
- **Location:** src/store/calls.rs:411 (TODO at line 408); 15 callers across onboard.rs, suggest.rs, task.rs, review.rs, impact/hints.rs, health.rs, scout.rs, gather.rs (2x), cli/commands/trace.rs, impact/diff.rs, impact/analysis.rs (2x), cli/commands/test_map.rs, cli/batch/mod.rs
- **Description:** `get_call_graph()` runs `SELECT DISTINCT caller_name, callee_name FROM function_calls LIMIT 500000` on every call. With 15 call sites across the codebase, any compound operation (e.g., `cqs task` calls scout which calls both `get_call_graph` and `find_test_chunks`) triggers multiple full table scans of the same data. For a project with ~2000 edges this takes ~5ms per call, but it adds up: a single `cqs health` call triggers `get_call_graph` once then `find_test_chunks` once; `cqs task` chains scout + gather + impact + placement, each independently calling `get_call_graph`. The TODO comment at line 408 acknowledges this (`TODO(PF-7): Add OnceLock<CallGraph> cache field to Store`) but was never implemented. The previous audit (v0.19.4) triaged this as PF-7 and marked it "fixed" in PR #502, but only the TODO comment was added — the actual cache was not built.
- **Suggested fix:** Add `call_graph_cache: OnceLock<Result<CallGraph, StoreError>>` to `Store`. Invalidate (reset) in `upsert_function_calls`, `prune_stale_calls`, and `replace_calls_for_file`. The `BatchContext` already has this pattern for its own `OnceLock<CallGraph>` (src/cli/batch/mod.rs:60), but only batch mode benefits — all other callers (scout, task, health, impact, review, suggest, onboard) still hit the database directly.

#### PF-2: `find_test_chunks()` uncached — 14 call sites each do a complex SQL scan with LIKE patterns
- **Difficulty:** medium
- **Location:** src/store/calls.rs:1054 (TODO around line 937); 14 callers across onboard.rs, task.rs, scout.rs, review.rs, suggest.rs, health.rs, impact/diff.rs, impact/analysis.rs (2x), impact/hints.rs, cli/batch/mod.rs, cli/batch/handlers.rs, cli/commands/test_map.rs
- **Description:** `find_test_chunks()` runs a broad SQL query combining name patterns (`test_%`, `Test%`), content LIKE patterns from `LanguageDef::test_markers`, and path patterns from `LanguageDef::test_path_patterns`. The SQL uses OR-chains of LIKE clauses which force full table scans (LIKE with leading wildcards can't use indexes). With 14 call sites, compound operations repeatedly scan the entire chunks table for test detection. Same situation as PF-1: previous audit (v0.19.4 PF-10) marked "fixed" but only a TODO comment exists. `BatchContext` has its own `OnceLock<Vec<ChunkSummary>>` but non-batch callers (scout, health, impact, review, etc.) all hit the database independently.
- **Suggested fix:** Add `test_chunks_cache: OnceLock<Result<Vec<ChunkSummary>, StoreError>>` to `Store`. Invalidate in `replace_file_chunks`, `upsert_chunks_batch`, and `upsert_chunks_and_calls`. This is the same pattern as PF-1 — both caches should be implemented together since they share the invalidation points.

#### PF-3: `analyze_impact` triggers both uncached table scans — double penalty for impact analysis
- **Difficulty:** easy (fixed by PF-1 + PF-2)
- **Location:** src/impact/analysis.rs:29-30
- **Description:** `analyze_impact` calls `store.get_call_graph()` on line 29 and `store.find_test_chunks()` on line 30, back-to-back. Each is a full table scan (PF-1 and PF-2). The `analyze_impact_batch` variant at line 238 does the same. Since `impact` is called from `cqs impact`, `cqs review`, `cqs task`, and `cqs ci`, every impact analysis pays the cost of two full table scans. In compound operations like `cqs task` (which calls scout first, then impact), the call graph is loaded at least twice and test chunks at least twice.
- **Suggested fix:** Automatically resolved by implementing PF-1 and PF-2 caches at the Store level. No changes needed in `analyze_impact` itself.

#### PF-4: `search_across_projects` serializes project searches — no parallelism
- **Difficulty:** medium
- **Location:** src/project.rs:172 (`for entry in &registry.project` loop)
- **Description:** `search_across_projects` iterates over registered projects sequentially in a `for` loop (line 172). Each iteration: (1) checks index existence on disk, (2) opens a SQLite Store (`Store::open_readonly`), (3) loads HNSW index from disk (`HnswIndex::try_load`), and (4) runs a full `search_filtered_with_index`. For N projects, total latency is the sum of all searches rather than the maximum. Store opening involves SQLite connection setup (~10-50ms), HNSW loading involves mmap setup (~5-20ms), and search itself depends on corpus size. With 3+ registered projects, the serial overhead becomes noticeable.
- **Suggested fix:** Use rayon `par_iter` over `registry.project` to search projects in parallel. Each project search is independent (separate Store, separate HNSW index). The results are already merged and sorted after the loop. The gather cross-index code (`gather.rs:540`) already demonstrates the pattern: `rayon::scope` with parallel store operations. Care needed: each parallel search creates its own tokio runtime via `Store::open_readonly` — verify that multiple concurrent runtimes don't conflict.

#### PF-5: `replace_file_chunks` FTS INSERT is per-row despite bulk DELETE
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:290-311
- **Description:** `replace_file_chunks` does a bulk DELETE of all FTS entries for the origin (efficient), then inserts new FTS entries one row at a time in a loop (line 291-311). Each iteration calls `normalize_for_fts` 4 times (name, signature, content, doc) and then issues a separate `INSERT INTO chunks_fts` SQL statement. The chunk data itself is inserted in batches of 55 using `QueryBuilder::push_values` (line 253-287), but the FTS path doesn't batch. For a file with 50 chunks, this is 50 individual INSERT statements plus 200 `normalize_for_fts` calls, vs. potentially 1 batch INSERT. The same per-row pattern exists in `upsert_chunks_batch` (line 119-147) and `upsert_chunks_and_calls` (line 399-427), though those have a hash-based skip optimization for unchanged chunks.
- **Suggested fix:** Use `QueryBuilder::push_values` for FTS INSERT as well, batching similarly to the chunk INSERT (groups of ~200 rows, since FTS has 5 columns = ~200 rows per 999-param batch). The `normalize_for_fts` calls still need to happen per-row, but eliminating 49 of 50 SQL round trips would reduce SQLite transaction overhead. Alternatively, accept the current approach — FTS INSERT is fast within a transaction and `replace_file_chunks` is only called from watch mode (typically 1-5 files at a time).

#### PF-6: `count_stale_files` and `list_stale_files` run identical SQL queries — callers that need both pay double
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:546-586 (`count_stale_files`), src/store/chunks.rs:592-643 (`list_stale_files`)
- **Description:** `count_stale_files` and `list_stale_files` both run `SELECT DISTINCT origin, source_mtime FROM chunks WHERE source_type = 'file'` and iterate over all rows checking file existence and mtime. They differ only in return type: `(u64, u64)` counts vs. `StaleReport` with file lists. Currently no single caller invokes both, but `health.rs` calls `count_stale_files` and `gc.rs` calls `count_stale_files` — if either ever needed the full list, they'd have to call both and double the I/O. The doc comment on `list_stale_files` even says "Like `count_stale_files()` but returns full details."
- **Suggested fix:** Refactor `count_stale_files` to call `list_stale_files` internally and extract counts from the `StaleReport`. The SQL query and file I/O (mtime checks) are the expensive part; computing counts from a `StaleReport` is trivial. This makes `count_stale_files` a thin wrapper and eliminates code duplication. Current impact is low since no caller uses both, but the duplicated logic is a maintenance burden.

