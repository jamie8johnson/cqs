# Audit Findings — v0.19.4+

Audit started 2026-03-02. Covers all code on main after PF-5 (#515).

## Observability

#### OB-1: `resolve_target` — public entry point has no tracing span
- **Difficulty:** easy
- **Location:** src/search.rs:57
- **Description:** `resolve_target()` is the shared entry point used by `blame`, `explain`, `similar`, `callers`, `callees`, `deps`, `trace`, and `test-map`. It calls `store.search_by_name()` and applies file filtering. No span means failures (name not found, ambiguous file filter) are invisible in traces — you only see the outer command span with no insight into why lookup failed or how long it took.
- **Suggested fix:** Add `let _span = tracing::info_span!("resolve_target", target).entered();` at the top of the function.

#### OB-2: `delete_by_origin` and `replace_file_chunks` — no tracing spans on mutation operations
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:194, 222
- **Description:** `delete_by_origin()` deletes all chunks for a file path. `replace_file_chunks()` atomically replaces them. Both are called from the index pipeline and watch mode. Neither has a tracing span, so bulk delete/replace operations are invisible in traces. When a pipeline run is slow, there's no way to distinguish indexing latency from deletion latency.
- **Suggested fix:** Add `info_span!("delete_by_origin", origin = %origin.display())` and `info_span!("replace_file_chunks", origin = %origin.display(), chunks = chunks.len())`.

#### OB-3: `search_filtered` exits without logging result count
- **Difficulty:** easy
- **Location:** src/search.rs:793-795
- **Description:** `search_filtered()` has an entry span but never logs how many results it returns. The span captures `limit` and `rrf` at entry but a caller observing a trace has no way to know whether the search returned 0, 5, or 20 results without reading the span's return value separately. This matters when debugging "why did gather find nothing?" — the seed search returning 0 vs the BFS finding nothing are different problems.
- **Suggested fix:** Add `tracing::debug!(results = results.len(), "search_filtered complete");` before `Ok(results)` at line 795.

#### OB-4: `Store::init` — schema initialization has no span
- **Difficulty:** easy
- **Location:** src/store/mod.rs:355
- **Description:** `Store::init()` creates all tables, inserts metadata, and sets schema version. Called on every `cqs index` run on a fresh database. No span means a slow init (e.g., large schema, pragma tuning) is invisible. Also missing: no log of which schema version was initialized.
- **Suggested fix:** Add `let _span = tracing::info_span!("store_init").entered();` and `tracing::info!(schema_version = CURRENT_SCHEMA_VERSION, "Store initialized");` after the commit.

#### OB-5: `warn_stale_results` — missing entry span
- **Difficulty:** easy
- **Location:** src/cli/staleness.rs:19
- **Description:** `warn_stale_results()` is called after every query command to check if result files changed since last index. It calls `store.check_origins_stale()` which can be non-trivial for large result sets. No span means staleness check latency is hidden in parent command spans. The function already has a `tracing::info!` call inside (line 24) but no enclosing span to correlate it with the originating command.
- **Suggested fix:** Add `let _span = tracing::info_span!("warn_stale_results", origins = origins.len()).entered();` at the top.

#### OB-6: `cmd_watch` — no entry span
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:54
- **Description:** `cmd_watch()` is the main watch loop. It has various `tracing::warn!` calls inside but no entry span. Watch mode can run for hours — without a span, there's no parent context to correlate reindex events or WSL inotify warnings back to a session.
- **Suggested fix:** Add `let _span = tracing::info_span!("cmd_watch", debounce_ms).entered();` at the top of `cmd_watch`.

#### OB-7: `get_caller_counts_batch` / `get_callee_counts_batch` — no spans on batch count queries
- **Difficulty:** easy
- **Location:** src/store/calls.rs:1147, 1163
- **Description:** Both batch count functions delegate through `batch_count_query()` with no span. These are called by `find_dead_code` to identify functions with zero callers. When dead code analysis is slow (common on large indexes with `NOT IN` subqueries), there's no way to attribute latency to the counting phase vs. the filtering phase.
- **Suggested fix:** Add spans: `tracing::info_span!("get_caller_counts_batch", count = names.len())` and `tracing::info_span!("get_callee_counts_batch", count = names.len())`.

## API Design

#### AD-1: BatchCmd::Gather `direction` is stringly-typed — should use GatherDirection enum
- **Difficulty:** easy
- **Location:** src/cli/batch/commands.rs:109, src/cli/batch/handlers.rs:349
- **Description:** The CLI's `Gather` command uses `GatherDirection` as a typed `ValueEnum` (fixed in prior audit AD-1, PR #501), but `BatchCmd::Gather` still declares `direction: String`. The handler (`dispatch_gather`) manually parses it with `.parse().map_err(...)` on line 361. This means batch mode gets no clap validation — invalid directions produce a runtime error instead of a parse-time error.
- **Suggested fix:** Change `BatchCmd::Gather` to use `GatherDirection` directly (it already has `FromStr` and could derive `ValueEnum`). Update `dispatch_gather` signature to accept `GatherDirection` instead of `&str`.

#### AD-2: `get_callers()` is dead public API — zero callers
- **Difficulty:** easy
- **Location:** src/store/calls.rs:252
- **Description:** `Store::get_callers()` returns `Vec<ChunkSummary>` from the chunks table. All production code uses `get_callers_full()` (returns `CallerInfo` from function_calls table) or `get_callers_with_context()` instead. The method has zero call sites outside its own doc comment. Dead public API surface that could confuse callers about which method to use.
- **Suggested fix:** Remove `get_callers()`. If the chunk-level caller lookup is needed in the future, it can be re-added.

#### AD-3: Core store types lack `Serialize` — manual `to_json()` everywhere
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:128 (ChunkSummary), :192 (SearchResult), :243 (CallerInfo), :317 (NoteSummary), :330 (NoteSearchResult)
- **Description:** The five most-used public types in the store API all lack `#[derive(Serialize)]`. Instead, they have manual `to_json()` / `to_json_relative()` methods using `serde_json::json!()` macros. This forces every consumer (CLI commands, batch handlers, `ChunkOutput` batch type) to either call these manual methods or re-serialize fields by hand. The prior audit (AD-6, PR #502) added Serialize to higher-level types (`ScoutResult`, `TaskResult`, `GatherResult`, etc.) but left these foundational types untouched.
- **Suggested fix:** Add `#[derive(serde::Serialize)]` to `ChunkSummary`, `SearchResult`, `CallerInfo`, `NoteSummary`, `NoteSearchResult`. For `ChunkSummary::file` (PathBuf), use `#[serde(serialize_with = "crate::serialize_path_normalized")]` which already exists. The manual `to_json()` methods can remain as convenience wrappers or be deprecated.

#### AD-4: `BlameEntry` and `BlameData` use manual JSON assembly
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:18,26,155
- **Description:** `BlameEntry` and `BlameData` lack `Serialize` derive. `blame_to_json()` manually constructs JSON with `serde_json::json!()` for each field. This is the same pattern fixed in prior audits for other result types (AD-6/CQ-6).
- **Suggested fix:** Add `#[derive(serde::Serialize)]` to `BlameEntry` and `BlameData`. Replace `blame_to_json()` with direct serialization. `BlameData.chunk` is `ChunkSummary` which would need Serialize first (see AD-3).

#### AD-5: `dispatch_search` takes 9 individual parameters instead of a struct
- **Difficulty:** medium
- **Location:** src/cli/batch/handlers.rs:33-42
- **Description:** `dispatch_search(ctx, query, limit, name_only, semantic_only, rerank, lang, path, tokens)` takes 9 parameters. The CLI side avoids this by passing the `Cli` struct which bundles all search options. Batch mode destructures `BatchCmd::Search` into 8 individual args then passes them through. This makes the handler signature fragile — adding a search option requires touching 3 call sites (BatchCmd fields, dispatch match arm, handler signature).
- **Suggested fix:** Extract a `BatchSearchOptions` struct or pass the `BatchCmd::Search` variant directly to the handler, destructuring inside.

#### AD-6: 9 CLI command handlers accept unused `_cli: &Cli` parameter
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:245, where_cmd.rs:8, test_map.rs:9, task.rs:19, scout.rs:9, related.rs:24, impact_diff.rs:18, onboard.rs:9, convert.rs:10
- **Description:** These handlers accept `_cli: &Cli` but never read any field from it. This was reported in the prior audit (AD-5, PR #504, marked fixed) but 9 instances remain — either the fix regressed or new commands were added without removing the parameter. Unused parameters obscure the actual data dependencies and make signatures wider than needed.
- **Suggested fix:** Remove the `_cli` parameter from these 9 handlers and update their call sites in `run_with()`.

## Error Handling

#### EH-1: `build_blame_data` discards StoreError chain via `.map_err(|e| anyhow::anyhow!("{}", e))`
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:46
- **Description:** `resolve_target()` returns `Result<ResolvedTarget, StoreError>`. The blame handler converts the error via `.map_err(|e| anyhow::anyhow!("{}", e))`, which stringifies through `Display` and loses the full error chain. If the underlying cause is `StoreError::Database(sqlx::Error::...)`, the inner sqlx source chain is flattened to a single string. Since `StoreError` implements `std::error::Error` with proper `#[from]` chains, `?` alone would preserve the chain (anyhow auto-converts via `From`).
- **Suggested fix:** Replace `.map_err(|e| anyhow::anyhow!("{}", e)).context("Failed to resolve blame target")` with just `.context("Failed to resolve blame target")?`. The `?` operator on `Result<_, StoreError>` auto-converts to `anyhow::Error` preserving the error source chain.

#### EH-2: 7 bare `Store::open()` calls without path context in CLI commands
- **Difficulty:** easy
- **Location:** src/cli/commands/diff.rs:32, drift.rs:31, index.rs:66, index.rs:73, reference.rs:134, reference.rs:314, cli/watch.rs:113
- **Description:** These `Store::open(&path)?` calls use bare `?` without `.context()` or `.with_context()`. When the store fails to open (e.g., corrupted DB, permission denied, locked), the error says "Database error: ..." without identifying which path failed. The prior audit EH-5 (PR #499) swept 10 CLI command files, and EH-4/EH-5 in v0.19.2 (PR #501) fixed 3 more. These survived because diff.rs/drift.rs were refactored after the fix, and index.rs/watch.rs/reference.rs open stores in contexts where the path was just computed but is still absent from the error.
- **Suggested fix:** Add `.with_context(|| format!("Failed to open store at {}", path.display()))` to each site. The `open_project_store()` helper and `resolve_reference_store()` already do this correctly.

#### EH-3: `SearchFilter::validate()` returns `Result<(), &'static str>` — not a proper error type
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:513
- **Description:** `SearchFilter::validate()` returns `Result<(), &'static str>` where the error is a plain string. Callers must convert with `.map_err(|e| anyhow::anyhow!(e))` (query.rs:96). This bypasses the project's `thiserror`-based error hierarchy. The method checks 4 conditions (name_boost range, note_weight range, contradictory note_only, missing query_text) but returns the same opaque type for all. Callers cannot distinguish or match on specific validation failures.
- **Suggested fix:** Change the return type to `Result<(), StoreError>` using `StoreError::Runtime(msg.to_string())` for each check. Callers in query.rs and batch handlers would then use plain `?`.

#### EH-4: `dispatch_onboard` swallows `get_chunks_by_names_batch` error with fallback to empty HashMap
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers.rs:912-918
- **Description:** In `dispatch_onboard`, when `get_chunks_by_names_batch()` fails, the error is logged via `tracing::warn!` and an empty `HashMap` is returned as fallback. This is the token-packing branch (`Some(budget)`) — returning empty content means the onboard command silently produces an incomplete result with no content for any entry. The `gather` command has a similar degradation pattern but signals it via `"search_degraded": true` in the JSON output. The onboard handler does neither — it silently drops content without any signal to the caller.
- **Suggested fix:** Either propagate the error with `?` (since empty content defeats the purpose of token packing), or add a `"degraded": true` field to the JSON output like `gather` does.

#### EH-5: `GatherDirection::FromStr` uses `String` error type — inconsistent with project conventions
- **Difficulty:** easy
- **Location:** src/gather.rs:97
- **Description:** `GatherDirection`'s `FromStr` impl uses `type Err = String`. This is the only `FromStr` impl in the codebase that returns `String` — all `ChunkType` and `Language` `FromStr` impls return proper typed errors. The batch handler must do `.parse::<GatherDirection>().map_err(|e: String| anyhow::anyhow!("{e}"))` to convert. This is moot if AD-1 (make BatchCmd::Gather accept `GatherDirection` directly) is fixed.
- **Suggested fix:** Fix AD-1 first (use `GatherDirection` as clap `ValueEnum` in `BatchCmd`). If `FromStr` is still needed, change `Err` to a proper error type.

#### EH-6: `schema_version` in `IndexStats` silently defaults to 0 on parse failure
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:873-876
- **Description:** When loading `IndexStats`, the schema_version is parsed from a string metadata value via `.and_then(|s| s.parse().ok()).unwrap_or(0)`. If the stored value is non-numeric (e.g., corrupted), this silently returns schema version 0 with no log. A corrupt schema_version could cause downstream code to behave as if the index is unversioned. Other metadata fields (model_name, created_at) default to empty strings on missing, which is correct, but version 0 is actively misleading.
- **Suggested fix:** Log a `tracing::warn!` when `.parse()` fails (i.e., when the value exists but is non-numeric): `Err(e) => { tracing::warn!(value = %s, error = %e, "Non-numeric schema_version"); 0 }`.

## Documentation

#### DOC-1: `source/` listed in CONTRIBUTING.md architecture but module is entirely dead
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:109-111
- **Description:** CONTRIBUTING.md documents `source/` as "Source abstraction layer (reserved — not yet wired into indexing pipeline)" and lists `filesystem.rs` as an active file. The module is declared in `lib.rs:81` as `pub(crate) mod source` but has zero callers — `use crate::source` and `source::` appear nowhere outside the module itself. The prior audit (v0.19.2 DOC-7, PR #501) noted CONTRIBUTING.md listed it as active but only updated the description to "reserved"; neither the module nor the CONTRIBUTING entry was removed. This mismatch causes confusion about whether `source/` is a development direction or dead scaffolding.
- **Suggested fix:** Remove `src/source/` directory entirely (2 files, ~250 lines), remove `pub(crate) mod source;` from `lib.rs`, and remove the `source/` entry from CONTRIBUTING.md's architecture section. (Duplicate of CQ-1 in this audit — fix code, then update CONTRIBUTING.md.)

#### DOC-2: PRIVACY.md deletion instructions miss `~/.config/cqs/config.toml`
- **Difficulty:** easy
- **Location:** PRIVACY.md:46-51
- **Description:** The "Deleting Your Data" section lists 6 paths to delete. It includes `~/.config/cqs/projects.toml` (the project registry) but omits `~/.config/cqs/config.toml` (the user config file at the same path, confirmed by `src/config.rs:4,120`). A user following these instructions to fully remove cqs data would leave behind their configuration.
- **Suggested fix:** Add `rm -f ~/.config/cqs/config.toml  # User config` to the deletion block. Also change `rm -rf ~/.config/cqs/projects.toml` → `rm -f ~/.config/cqs/projects.toml` (using `-rf` on a file is inconsistent with `-f` used for the other files on lines 49-50).

#### DOC-3: SECURITY.md symlink mitigation description is inaccurate
- **Difficulty:** easy
- **Location:** SECURITY.md:94
- **Description:** SECURITY.md states "Symlinks in extracted archives are skipped" under the convert module mitigations. The actual implementation skips symlinks during **all directory walks** — both CHM-extracted archives (`src/convert/chm.rs:91`) and regular directory traversal (`src/convert/mod.rs:347`) and web help directories (`src/convert/webhelp.rs:60`). The phrase "in extracted archives" implies the protection only applies to CHM/archive extraction, underselling its actual scope.
- **Suggested fix:** Change "Symlinks in extracted archives are skipped" → "Symlinks are skipped in all directory walks (file input, CHM extraction, web help traversal)".

#### DOC-4: lib.rs doc comment omits Web Help format from convert feature description
- **Difficulty:** easy
- **Location:** src/lib.rs:13
- **Description:** The crate-level doc says `- **Document conversion**: PDF, HTML, CHM → cleaned Markdown (optional `convert` feature)`. The convert module also handles Web Help sites (multi-page HTML sites with a structured nav, detected by `src/convert/webhelp.rs`). Web Help is distinct enough from generic HTML to have its own dedicated module and is listed in the README's supported formats table. The lib.rs doc is the first thing library users see and it understates the format support.
- **Suggested fix:** Change to `PDF, HTML, CHM, Web Help → cleaned Markdown`.

#### DOC-5: `cqs dead --min-confidence` undocumented in README and CLAUDE.md
- **Difficulty:** easy
- **Location:** README.md:243-246, CLAUDE.md:71
- **Description:** `cqs dead --help` shows three options: `--json`, `--include-pub`, and `--min-confidence <MIN_CONFIDENCE>` (values: low, medium, high; default: low). README documents `--json` and `--include-pub` but not `--min-confidence`. CLAUDE.md only says `cqs dead — find dead code: functions/methods with no callers in the index` with no options listed. The `--min-confidence` flag is the primary way to reduce false positives in dead code analysis (filtering out low-confidence candidates) and is a meaningful option to document.
- **Suggested fix:** Add `cqs dead --min-confidence medium  # Filter to medium+ confidence` example to README's Maintenance section. Update CLAUDE.md description to include `--min-confidence`.

## Code Quality

#### CQ-1: `source/` module is dead code — never wired into indexing pipeline
- **Difficulty:** easy
- **Location:** src/source/mod.rs, src/source/filesystem.rs (declared at src/lib.rs:81)
- **Difficulty:** easy
- **Description:** The entire `source/` module (~250 lines across 2 files) is dead code. It declares a `Source` trait and `FileSystemSource` implementation but is never used anywhere — `mod source` is declared in `lib.rs` but zero `use crate::source` or `source::` references exist in the codebase. The module header says "Not yet wired into the indexing pipeline — reserved for future use" and has `#![allow(dead_code)]` to suppress warnings. Prior audit DOC-7 (PR #501) noted CONTRIBUTING.md listed it as active, but the module itself was never removed.
- **Suggested fix:** Delete `src/source/mod.rs` and `src/source/filesystem.rs`, remove `pub(crate) mod source;` from `lib.rs`. If the abstraction is needed later, it can be rebuilt from git history.

#### CQ-2: Gather JSON assembly duplicated between CLI command and batch handler
- **Difficulty:** easy
- **Location:** src/cli/commands/gather.rs:114-145, src/cli/batch/handlers.rs:406-438
- **Description:** `GatheredChunk` JSON serialization is written out identically in two places: `cmd_gather` (CLI command) and `dispatch_gather` (batch handler). Both construct the same `serde_json::json!()` with identical field lists (name, file, line_start, line_end, language, chunk_type, signature, score, depth, content) and the same conditional `source` field. Meanwhile, `GatheredChunk` already has both a `#[derive(serde::Serialize)]` and a manual `to_json(&self, root)` method — three different serialization paths for the same struct, none of which are used by these two callers.
- **Suggested fix:** Use `GatheredChunk`'s existing `Serialize` derive or `to_json()` method in both cmd_gather and dispatch_gather. If `normalize_path` is needed (it is), ensure `GatheredChunk.file` is already normalized before serialization, or add a `#[serde(serialize_with = ...)]` attribute.

#### CQ-3: `token_pack_unified` and `token_pack_tagged` are near-identical functions
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:333-398
- **Description:** `token_pack_unified()` and `token_pack_tagged()` are structurally identical 30-line functions that differ only in: (1) the input type (`Vec<UnifiedResult>` vs `Vec<TaggedResult>`), (2) how to extract text content (`.match r` vs `.match &r.result`), and (3) how to extract score (same match depth difference). Both call the same `super::token_pack()` generic function and log the same tracing info. The duplication exists because there's no trait to abstract "something that contains a UnifiedResult".
- **Suggested fix:** Add a trait (e.g., `HasUnifiedResult`) with `fn unified(&self) -> &UnifiedResult` on both `UnifiedResult` (returning `self`) and `TaggedResult` (returning `&self.result`). Then a single generic `token_pack_results<T: HasUnifiedResult>()` replaces both functions.

#### CQ-4: `cmd_query` at 287 lines — multiple concerns in one function body
- **Difficulty:** medium
- **Location:** src/cli/commands/query.rs:39-325
- **Description:** `cmd_query` handles 5 different search modes (name-only, name-only+ref, semantic, semantic+ref, multi-index) with inline branching. It's already been partially decomposed into helper functions (`cmd_query_name_only`, `cmd_query_ref_only`, `cmd_query_ref_name_only`) but the main body still mixes: (1) embedder creation, (2) filter construction, (3) CAGRA/HNSW index loading (30 lines of cfg-gated code), (4) audit mode checks, (5) search execution, (6) reference search + merge, (7) token packing, (8) parent context resolution, (9) staleness warning, and (10) result display. The early returns at lines 50-52 for name-only mode mean the rest of the function is implicitly the "semantic search" path, but this isn't clear from the structure.
- **Suggested fix:** Extract the CAGRA/HNSW index loading block (lines 115-153) into `load_vector_index(store, cqs_dir)`. Extract the staleness check (lines 240-253) into a helper. This would bring cmd_query under 200 lines and make the search mode flow clearer.

#### CQ-5: 11 functions suppress `clippy::too_many_arguments` — parameter struct opportunities
- **Difficulty:** medium
- **Location:** src/cli/commands/gather.rs:11, src/cli/batch/handlers.rs:32,344, src/search.rs:554, src/scout.rs:169,198, src/cli/watch.rs:209,264, src/cli/pipeline.rs:235, src/parser/markdown.rs:707,810
- **Description:** 11 `#[allow(clippy::too_many_arguments)]` annotations across the codebase. The worst offenders are `dispatch_search` (9 params), `dispatch_gather` (7 params), `cmd_gather` (8 params), and `scout_core` (8 params). Many of these pass CLI-extracted fields that were already bundled in a struct (`Cli` or `BatchCmd`) but destructured before the call. This pattern makes signatures fragile — adding a new option requires touching every call site in the chain.
- **Suggested fix:** For the batch handlers, pass `&BatchCmd` variants directly instead of destructuring. For `cmd_gather`, accept a struct (e.g., `GatherArgs { expand, direction, limit, tokens, ref_name, json }`). Not all 11 need fixing — pipeline.rs and markdown.rs take contextual state that doesn't naturally bundle.

## Extensibility

#### EX-1: `pipeable_command_names()` manually duplicates all pipeable `BatchCmd` variants — silently stale on new commands
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs:31-113
- **Description:** `pipeable_command_names()` builds a static array of `(name, BatchCmd::Variant { .. })` pairs to find which commands are pipeable via `is_pipeable()`. Every pipeable `BatchCmd` variant must be listed twice: once in `is_pipeable()` (line 275 of commands.rs) and once in this array. When a new pipeable command is added, forgetting to update `pipeable_command_names()` silently omits it from the pipeline error message — the pipeline still works, the omission is invisible to tests, and it's easy to miss in review. The comment says "kept in sync via is_pipeable()" but there is no compiler enforcement of that sync.
- **Suggested fix:** Remove the static array in `pipeable_command_names()`. Instead, generate the list by using `BatchInput::command().get_subcommands()` to iterate over all subcommand names and check each via `is_pipeable_command(&[name.to_string()])`. This derives the error message list from the actual `is_pipeable()` implementation rather than a separately maintained copy.

#### EX-2: `name_boost: 0.2` hardcoded in `dispatch_search` — no shared constant with CLI default
- **Difficulty:** easy
- **Location:** src/cli/batch/handlers.rs:83
- **Description:** `dispatch_search` hardcodes `name_boost: 0.2` as a bare literal. The CLI default is also `0.2` (via `#[arg(long, default_value = "0.2")]` in `src/cli/mod.rs:146`) and the config file template shows `name_boost = 0.2` (`src/config.rs:53`). These three values are in sync by coincidence — no shared constant exists. If the default changes, one of the three sites is likely to be missed. `SearchFilter::default()` correctly uses `0.0` (no name boosting when unspecified), so the batch handler is intentionally setting a non-default value — but the value should come from a named constant.
- **Suggested fix:** Define `pub const DEFAULT_NAME_BOOST: f32 = 0.2;` in `src/store/helpers.rs` or `src/lib.rs`. Use it in `cli/mod.rs` default, `batch/handlers.rs` dispatch_search, and the config template string.

#### EX-3: `HNSW_EXTENSIONS` and `HNSW_ALL_EXTENSIONS` are two overlapping constants manually kept in sync
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:31,34
- **Description:** `HNSW_EXTENSIONS = &["hnsw.graph", "hnsw.data", "hnsw.ids"]` and `HNSW_ALL_EXTENSIONS = &["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"]` share three entries that are duplicated verbatim. The split is intentional: `HNSW_EXTENSIONS` excludes the checksum file (used in checksum verification), while `HNSW_ALL_EXTENSIONS` includes it (used for cleanup/deletion). Adding a new HNSW file component requires updating both constants and maintaining the correct split, with no compiler help.
- **Suggested fix:** Add a compile-time assertion that `HNSW_ALL_EXTENSIONS` contains all elements of `HNSW_EXTENSIONS`: `const _: () = assert!(/* all HNSW_EXTENSIONS entries are in HNSW_ALL_EXTENSIONS */);`. Or restructure: `const HNSW_DATA_EXTENSIONS: &[&str] = &["hnsw.graph", "hnsw.data", "hnsw.ids"];` and derive both constants from it with a comment making the relationship explicit.

#### EX-4: `extract_from_scout_groups` is a bespoke extractor for scout's nested JSON shape — new non-standard output shapes each need a new extractor
- **Difficulty:** medium
- **Location:** src/cli/batch/pipeline.rs:116-210
- **Description:** The pipeline name extractor uses three strategies: bare array, a generic field walk (`extract_from_standard_fields`), and a bespoke `extract_from_scout_groups` for scout's `file_groups[].chunks[].name` nesting. `extract_from_standard_fields` was made key-agnostic (EX-6 fix, PR #504) to avoid hardcoding field names, but scout's two-level nesting required its own special case. Any new command with a non-standard output shape (e.g., `sections[].functions[].name`) would need another bespoke extractor added to `extract_names()`. The pipeline has no specification of what JSON structure makes a command's output pipeable.
- **Suggested fix:** Define a `"_names"` convention: pipeable command handlers include a top-level `"_names": ["fn1", "fn2"]` field in their JSON output. The pipeline extractor checks `_names` first — zero structural parsing logic. Scout already assembles the output; adding `_names` extraction there is simpler than structural inference at the pipeline level. This also makes pipeability testable without structural knowledge.

#### EX-5: Note/code slot ratio `(limit * 3) / 5` is an inline formula with no named constant — repeated in tests
- **Difficulty:** easy
- **Location:** src/search.rs:1061, :1216, :1223
- **Description:** The 60%/40% note-to-code slot allocation is expressed as `(limit * 3) / 5` (60% minimum code slots). This formula appears three times: once in production (line 1061) and twice re-derived independently in tests (lines 1216, 1223). The policy is documented only in a comment. If the ratio changes, all three sites must be updated manually, and stale tests would not catch a mismatch between production and test formulas.
- **Suggested fix:** Extract a `fn min_code_slots(limit: usize) -> usize { ((limit * 3) / 5).max(1) }` private helper. Both production code and tests call it. The ratio numerator/denominator becomes a single change point. Optionally name the constants: `const MIN_CODE_RATIO_NUM: usize = 3; const MIN_CODE_RATIO_DEN: usize = 5;`.

## Robustness

#### RB-1: `fetch_candidates_by_ids_async` and `fetch_chunks_by_ids_async` unbatched — exceed SQLite 999-param limit with high `--limit`
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1366, 1326
- **Description:** Both functions build a single `IN(?)` placeholder list from all IDs passed in. The HNSW search path computes `candidate_count = (limit * 5).max(100)` (search.rs:811,1030), so `--limit 200` produces 1000 candidate IDs — exceeding SQLite's 999-parameter limit. `check_origins_stale` had the same bug (DS-8, fixed in v0.19.0) and `get_chunks_by_names_batch` already batches in groups of 500. These two functions were added later in PF-5 and missed the batching pattern.
- **Suggested fix:** Batch IDs in groups of 900 (same constant used by `check_origins_stale`). Collect results across batches: `fetch_candidates_by_ids_async` extends a Vec, `fetch_chunks_by_ids_async` merges HashMaps.

#### RB-2: `CandidateRow::from_row` and `ChunkRow::from_row` use panicking `row.get()` — no column-missing resilience
- **Difficulty:** medium
- **Location:** src/store/helpers.rs:75-84, 106-121
- **Description:** Both `from_row` methods use `row.get("column_name")` which panics if the column is absent. The callers control the SQL query, so missing columns indicate a programmer bug rather than corrupt data. However, this means a schema migration that drops or renames a column would cause a panic rather than a descriptive error. Other store code (e.g., `IndexStats`, `metadata`) uses `row.get::<Option<_>, _>()` or `.fetch_optional()` which handle missing data gracefully. The `from_row` pattern is consistent across both structs — 16 panicking `.get()` calls total.
- **Suggested fix:** This is acceptable as-is since the SQL queries are co-located with the from_row calls (programmer invariant). If defensive coding is desired, use `row.try_get()` which returns `Result` instead of panicking. Low priority — leave as informational unless schema migrations are planned.

#### RB-3: `where_to_add.rs` — `suggest_placement_with_options_core` panics via `.expect()` on `query_embedding`
- **Difficulty:** easy
- **Location:** src/where_to_add.rs:170
- **Description:** `suggest_placement_with_options_core` calls `.expect()` on `opts.query_embedding`, relying on the caller (`suggest_placement_with_options`) to always set it. The invariant is enforced by the public entry point: it either passes through when `is_some()` or sets it before calling `_core`. But `_core` is `fn` (not `pub`), so only the one caller can reach it. If a future refactor adds another call path that forgets to set the embedding, this panics at runtime with no compile-time guard.
- **Suggested fix:** Change `_core` to take `query_embedding: &Embedding` as a direct parameter instead of extracting it from `opts`. This makes the requirement compile-time enforced rather than runtime-asserted.

#### RB-4: `blame` — `run_git_log_line_range` passes `u32` line numbers to format string without overflow check on `start > end`
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:86
- **Description:** The function formats `"{},{}:{}"` from `start` and `end` line numbers. If a chunk's `line_start > line_end` (corrupt index data, or a parser bug where an empty node produces inverted ranges), git receives a reversed line range like `100,50:file.rs`. Git handles this gracefully (returns empty output), so this doesn't crash. However, the user gets silent empty results with no explanation. A `line_end >= line_start` assertion or swap would make the failure mode visible.
- **Suggested fix:** Add a check at the top: `if end < start { tracing::warn!(start, end, file = rel_file, "Inverted line range — possible corrupt index"); std::mem::swap(&mut start, &mut end); }`. This self-heals and logs the anomaly.

#### RB-5: `reranker.rs` — `outputs[0]` index access panics if ONNX model returns empty output
- **Difficulty:** easy
- **Location:** src/reranker.rs:140
- **Description:** After `session.run()`, the code accesses `outputs[0]` with direct indexing. If the ONNX model returns zero outputs (malformed model, version mismatch), this panics. The embedder has the same pattern at embedder.rs:528 (`outputs[0]`). Both are ONNX runtime calls on bundled models, so the risk is low — but a model update or file corruption would produce a panic instead of a descriptive error.
- **Suggested fix:** Replace `outputs[0]` with `outputs.first().ok_or(RerankerError::Inference("Model returned no outputs".into()))?` (and similarly for embedder).

#### RB-6: `Language::def()` and `Language::grammar()` panic on disabled feature flags — no graceful path
- **Difficulty:** easy
- **Location:** src/language/mod.rs:549, 570
- **Description:** `Language::def()` panics if the language's feature flag is disabled, and `Language::grammar()` panics if the language has no tree-sitter grammar. Both are documented with `# Panics` doc sections. The `try_def()` alternative exists but callers (parser, search) almost universally use `def()`. In the current codebase all 20 languages are always enabled, so this is theoretical. But it means feature-flag subsetting (e.g., a minimal build with only Rust+Python) would hit panics instead of skip-and-warn behavior.
- **Suggested fix:** Low priority since all languages are always enabled. If feature-flag subsetting is planned, replace panics with `Result` returns. Otherwise, leave as-is with the documented `# Panics` sections.

#### RB-7: `Parser::new()` panics on registry/enum mismatch — fails at construction not usage
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:62-67
- **Description:** `Parser::new()` iterates the language registry and panics if any registered language name can't parse to a `Language` enum variant. This is a compile-time invariant (the registry and enum are defined in the same module), so the panic can only fire if someone adds a registry entry without a matching enum variant. The panic message is descriptive ("Language registry/enum mismatch: 'X' is registered but has no Language variant"). However, it runs at `Parser::new()` which is called during indexing — a panic here kills the entire index pipeline.
- **Suggested fix:** Low priority — this is a developer-facing invariant violation. Could add a `#[cfg(test)]` consistency test instead, which catches the mismatch at test time rather than at runtime.

## Algorithm Correctness

#### AC-1: `emit_empty_results` produces malformed JSON — query string not escaped
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:29, src/cli/commands/similar.rs:99
- **Description:** `emit_empty_results` uses raw `format!`-style interpolation to embed the query string into a JSON string literal: `println!(r#"{{"results":[],"query":"{}","total":0}}"#, query)`. If the query contains `"`, `\`, or control characters, the output is invalid JSON. Example: query `foo"bar` produces `{"results":[],"query":"foo"bar","total":0}` which any JSON parser rejects. The same pattern exists in `cmd_similar` (similar.rs:99) with `chunk_name` — chunk names from the DB are unlikely to contain special chars but C++ operator overloads could trigger it.
- **Suggested fix:** Use `serde_json::json!()` to build the object, which properly escapes all string values: `let obj = serde_json::json!({"results": [], "query": query, "total": 0}); println!("{}", obj);`. Apply the same fix in similar.rs:99.

#### AC-2: `search_by_candidate_ids` FTS runs against full index, not scoped to HNSW candidates
- **Difficulty:** medium
- **Location:** src/search.rs:934-942
- **Description:** In the HNSW-guided path (`search_by_candidate_ids`), semantic scoring only considers the ~500 HNSW candidate IDs. But the FTS keyword search runs against the full `chunks_fts` index with `LIMIT limit*3` (line 938). When `rrf_fuse` merges these two lists, FTS-only results that were NOT in the HNSW candidate set enter the final results. These FTS-only results appear in the output with a pure-FTS RRF score (no semantic component). This is a design tension: the HNSW path was designed to narrow the search space for performance (PF-5), but the unscoped FTS re-expands it. In practice this often improves recall (FTS catches keyword matches HNSW misses), but it means the HNSW candidate count doesn't actually bound the work — it's O(FTS_matches) + O(candidate_count), not O(candidate_count) alone.
- **Suggested fix:** Document the behavior rather than restricting it: "HNSW narrows embedding search; FTS still scans the full index for keyword matches." The FTS scan is fast (SQLite FTS5 is O(matching_docs), not O(total_docs)) and improves recall. If strict bounding is needed, scope FTS with `WHERE id IN (...)` but this reduces recall for keyword-heavy queries.

#### AC-3: `token_pack` has O(n^2) `keep.iter().any()` scan in greedy packing loop
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:134
- **Description:** In the greedy packing loop, `keep.iter().any(|&k| k)` scans the entire `keep` boolean array on every iteration to check if at least one item has been packed. This check implements the "always include at least one item" guarantee. For N items, total scan work is O(N^2). In practice N is small (search results capped at ~20-100), so this isn't a measurable bottleneck. But it's an unnecessary quadratic pattern that could be replaced with a constant-time check.
- **Suggested fix:** Replace with a `bool` flag: add `let mut has_any = false;` before the loop, change the condition to `if used + tokens > budget && has_any { break; }`, and set `has_any = true;` after each `keep[idx] = true;`.

#### AC-4: `cap_scores` in onboard uses `u64::MAX - x` inversion trick — correct but fragile
- **Difficulty:** easy
- **Location:** src/onboard.rs:175-183
- **Description:** The `cap_scores` function sorts ascending by key and truncates to `max`. For caller_scores, the key function inverts scores via `u64::MAX - ((safe * 1e6) as u64)` so highest scores map to lowest keys. This is mathematically correct — ascending sort + truncate keeps highest scores. But the inversion trick is non-obvious: a reader seeing ascending sort + truncate expects "keep lowest values." The callee_scores path uses `|(_s, d)| *d` (keep shallowest depth) which reads naturally. The `f32 * 1e6 as u64` cast is safe since scores are cosine similarities (0.0-1.0), producing values up to 1,000,000 — well within u64 range.
- **Suggested fix:** Replace the inversion trick with a reverse sort: change `entries.sort_by(|a, b| key_fn(&a.1).cmp(&key_fn(&b.1)));` to `entries.sort_by(|a, b| key_fn(&b.1).cmp(&key_fn(&a.1)));` for the caller case, and pass `|(score, _)| { let safe = ...; (safe * 1e6) as u64 }` without inversion. This eliminates the mental gymnastics while preserving the same result.

#### AC-5: `bfs_shortest_path` uses empty-string sentinel instead of `Option` for predecessor tracking
- **Difficulty:** easy
- **Location:** src/cli/commands/trace.rs:203-221
- **Description:** The BFS shortest path stores predecessors in `HashMap<String, String>` where the source node's predecessor is `String::new()`. Path reconstruction walks predecessors until finding an empty string. This relies on no real function name being empty. The parser never produces empty names, so this works in practice. But the sentinel value makes the invariant implicit rather than type-enforced.
- **Suggested fix:** Informational — no practical bug. Could use `HashMap<String, Option<String>>` for cleaner semantics, but the current code works given the parser invariant.

## Test Coverage

#### TC-1: 5 newest languages have no parser integration tests — Bash, HCL, Kotlin, Swift, Objective-C
- **Difficulty:** medium
- **Location:** tests/parser_test.rs (missing), tests/fixtures/ (missing .sh, .tf, .kt, .swift, .m)
- **Description:** The parser integration tests in `tests/parser_test.rs` cover Rust, Python, TypeScript, JavaScript, Go, C, Java, SQL, and Markdown with fixture files. The 5 languages added in v0.19.0 (Bash, HCL, Kotlin, Swift, Objective-C) have no fixture files and no integration tests. Kotlin and Swift have 1 inline unit test each (return type extraction only). Bash, HCL, and Objective-C have zero tests anywhere — neither inline nor integration. The inline language tests only cover `extract_return_type()`, not the full tree-sitter parsing pipeline (`parse_file` -> chunks with correct names, signatures, call extraction, parent_id wiring). A tree-sitter query regression in any of these 5 languages would be invisible to CI.
- **Suggested fix:** Add fixture files (`sample.sh`, `sample.tf`, `sample.kt`, `sample.swift`, `sample.m`) with representative code (functions, classes/structs, nested constructs). Add `test_parse_*_fixture()` integration tests following the existing C/Java/SQL pattern: parse fixture, assert non-empty, verify language tag, check function count.

#### TC-2: `NoteBoostIndex` has zero tests — search scoring hot path
- **Difficulty:** easy
- **Location:** src/search.rs:300-371
- **Description:** `NoteBoostIndex` is the search-time note-based score boosting component used in every search call. It has non-trivial logic: classifying mentions as name-like vs path-like (line 317-319), taking strongest absolute sentiment (line 323), and computing a `1.0 + sentiment * NOTE_BOOST_FACTOR` multiplier (line 368). Zero direct tests. The `test_score_candidate_name_boost` test exercises name boost through `NameMatcher` but not through `NoteBoostIndex`. The path-mention matching logic (suffix/prefix matching via `path_matches_mention`) is tested separately but the `NoteBoostIndex::boost()` composition — multiple notes, mixed name/path mentions, sentiment priority — is never tested in isolation.
- **Suggested fix:** Add tests: (1) empty notes -> boost returns 1.0, (2) name mention with positive sentiment -> boost > 1.0, (3) path mention with negative sentiment -> boost < 1.0, (4) competing name and path mentions -> strongest absolute wins, (5) multiple notes for same mention -> strongest sentiment preserved.

#### TC-3: `search_by_candidate_ids` language and chunk_type filter branches untested
- **Difficulty:** easy
- **Location:** src/search.rs:883-905
- **Description:** The PF-5 `search_by_candidate_ids` function has filter_map branches for `filter.languages` (lines 883-893) and `filter.chunk_types` (lines 895-905) that parse candidate row strings into `Language`/`ChunkType` enums and filter non-matching candidates. These branches are unique to the candidate path — the brute-force `search_filtered` path filters differently (in SQL WHERE clauses). The existing `test_search_by_candidate_ids_with_glob_filter` tests the `path_pattern` filter but not language or chunk_type filters. A regression where string->enum parsing fails (e.g., case mismatch after a ChunkType rename) would silently drop all candidates in the candidate path while the brute-force path continues working.
- **Suggested fix:** Add tests: (1) `filter.languages = Some(vec![Language::Rust])` with mixed-language candidates — only Rust survivors, (2) `filter.chunk_types = Some(vec![ChunkType::Function])` — methods/structs filtered out.

#### TC-4: `ChatHelper::complete` — untested tab-completion logic
- **Difficulty:** easy
- **Location:** src/cli/chat.rs:26-49
- **Description:** `ChatHelper::complete()` implements tab completion with specific behaviors: (1) only completes the first token (returns empty after a space), (2) filters commands by prefix, (3) returns `(0, matches)` — the 0 indicates replacement starts at position 0. The chat.rs test module only tests `handle_meta` and `command_names` — the `Completer` impl is never tested. Tab completion is a user-facing UX feature and the `pos` vs prefix slicing logic (`&line[..pos]`) could regress silently. The test would be trivial since `ChatHelper` is `struct { commands: Vec<String> }` with no external dependencies.
- **Suggested fix:** Add tests: (1) prefix "se" at pos=2 -> returns ["search"], (2) prefix "cal" at pos=3 -> returns ["callers", "callees"], (3) "search fo" at pos=9 -> returns [] (space in prefix, no completion), (4) empty prefix at pos=0 -> returns all commands.

#### TC-5: `build_blame_data` only tested through JSON shape — no end-to-end path with mock git
- **Difficulty:** medium
- **Location:** src/cli/commands/blame.rs:36-68
- **Description:** `build_blame_data` is the core blame logic that wires `resolve_target` -> `run_git_log_line_range` -> `parse_git_log_output` -> `get_callers_full`. The existing tests cover `parse_git_log_output` (5 tests) and `blame_to_json` (2 tests) individually, but `build_blame_data` itself is never tested — its callers (`cmd_blame` and `dispatch_blame`) both need a live git repo and an indexed store, so integration tests would be needed. The function has 3 error paths: resolve_target failure (line 45-47), git log failure (line 52), and callers failure (line 56-59, silently falls back). The callers fallback path in particular — `unwrap_or_else` returning empty Vec on store error — is never exercised by any test.
- **Suggested fix:** Create an integration test that sets up a temp git repo (init + commit), indexes it, then calls `build_blame_data`. This would cover the full pipeline. Lower priority than TC-1 through TC-4 since blame is a thin git wrapper and the individual components are tested.

#### TC-6: Batch pipeline error propagation — malformed mid-pipeline input never tested
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs
- **Description:** The pipeline integration tests cover: valid 2-stage, valid 3-stage, empty upstream, ineligible downstream, quoted pipe, and mixed pipeline+single commands. However, no test exercises error propagation when a **middle** stage fails — e.g., `callers process | explain | callers` where the middle `explain` returns a result that the downstream `callers` can't extract names from (non-array explain output). The `extract_names` function at pipeline.rs handles various JSON shapes but the interaction between an explain output (object with `callers`/`callees` arrays) being fed into a downstream callers stage is untested. The pipeline should degrade gracefully (0 names extracted -> empty result), but this specific shape interaction isn't verified.
- **Suggested fix:** Add a test: `explain process | callers` — verify the pipeline extracts names from explain's callers/callees arrays and feeds them downstream correctly. Also test a deliberately incompatible pipeline like `stats | callers` (stats returns object with no name fields) to verify graceful degradation.

## Platform Behavior

#### PB-1: `cmd_watch` — `RecommendedWatcher` uses inotify on WSL; `Config::with_poll_interval` has no effect on inotify backend
- **Difficulty:** medium
- **Location:** src/cli/watch.rs:61-63, 87-89
- **Description:** On WSL over `/mnt/c/` NTFS-backed paths, inotify events are unreliable (missed or duplicated). The code emits a warning at startup but still creates a `RecommendedWatcher` which uses inotify on Linux. `Config::default().with_poll_interval(Duration::from_millis(debounce_ms))` configures the polling interval for `notify::PollWatcher` — it has no effect on the inotify backend. The result: users see the warning but the watcher continues using inotify, silently missing changes on `/mnt/` paths. The `last_indexed_mtime` deduplication guard mitigates duplicate events but doesn't address missed ones.
- **Suggested fix:** When `is_wsl() && root.to_str().map_or(false, |p| p.starts_with("/mnt/"))`, use `notify::PollWatcher::new(tx, config)?` instead of `RecommendedWatcher`. Both implement the `Watcher` trait, enabling a conditional dispatch. Alternatively, document that `cqs watch` is unsupported on WSL `/mnt/` paths and upgrade the startup warning to a bail with a suggestion to use the `cqs-watch` systemd service configured with `cqs index` instead.

#### PB-2: `collect_events` — `notes_path` silently falls back to non-canonical path if `docs/notes.toml` doesn't exist at watch start
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:97-105, 232
- **Description:** `dunce::canonicalize(&notes_path)` is called at watch startup (line 102), but fails if `docs/notes.toml` doesn't exist — valid for projects without notes. On failure, the fallback at line 103 preserves the un-canonicalized path. Inotify delivers event paths in canonicalized form. The comparison `path == notes_path` (line 232) then compares a canonical event path against a possibly non-canonical stored path. On WSL with symlinked project directories or when the project root itself is a symlink, the paths diverge and notes changes are silently ignored even after the file is created.
- **Suggested fix:** Re-attempt canonicalization each cycle when `notes_path` remains non-canonical: `if !notes_canonical { if let Ok(c) = dunce::canonicalize(&notes_path) { notes_path = c; notes_canonical = true; } }`. Or compare both forms in the equality check.

#### PB-3: `run_git_log_line_range` — forward-slash path in `-L` spec latently incompatible with native Windows git
- **Difficulty:** easy
- **Location:** src/cli/commands/blame.rs:50, 86
- **Description:** `rel_file` is produced by `rel_display(&chunk.file, root)`, which normalizes to forward slashes. The `-L start,end:path` argument format requires the path to match the working tree path as git sees it. On native Windows git (not WSL), git expects Windows-native path separators; forward-slash paths fail the `-L` spec lookup and git returns "no path found in git history" for otherwise valid files. Currently WSL-only, so this is latent — but any future Windows-native deployment would hit this silently.
- **Suggested fix:** Add a code comment at line 86 noting the platform assumption (`// git on WSL handles forward slashes; native Windows git requires MAIN_SEPARATOR`). If Windows native support is added, conditionally replace slashes: `if cfg!(windows) { rel_file.replace('/', "\\") } else { rel_file }`.

#### PB-4: `acquire_index_lock` and `try_acquire_index_lock` duplicate lock-file open code; NTFS ignores `0o600` silently
- **Difficulty:** easy
- **Location:** src/cli/files.rs:52-71, 95-115, 46-81
- **Description:** Both lock functions contain nearly identical `#[cfg(unix)]` / `#[cfg(not(unix))]` file-creation blocks. `acquire_index_lock` duplicates the block a second time in its retry-after-stale-lock path (lines 96-115). On WSL over `/mnt/c/`, the `#[cfg(unix)]` branch applies (WSL is Linux), so `0o600` mode is requested — but NTFS ignores Unix permission bits. The `set_permissions` call succeeds silently, making the lock file world-readable to any user with filesystem access. No warning is emitted.
- **Suggested fix:** Extract `open_lock_file(path) -> Result<File>` helper to eliminate duplication. Add a `tracing::debug!` when `is_wsl() && path.starts_with("/mnt/")` noting the NTFS permission caveat. The PID leak is low severity since `.cqs/` already requires project directory access.

#### PB-5: `is_wsl()` has no `#[cfg(unix)]` guard — non-Unix builds rely on I/O failure for correct behavior
- **Difficulty:** easy
- **Location:** src/config.rs:15-25
- **Description:** `is_wsl()` reads `/proc/version` and returns `false` on I/O error. On a native Windows binary, `/proc/version` doesn't exist, so the function returns `false` — correct, but the correctness depends on `read_to_string` failing rather than on explicit platform conditioning. The callers (`warn_wsl_advisory_locking` at hnsw/persist.rs:21, config permission check at config.rs:206) pair `is_wsl()` with `path.starts_with("/mnt/")` — a Unix-only path prefix that is meaningless on Windows. The code is functionally correct for the current WSL-only deployment but is structurally confusing for any future Windows-native port.
- **Suggested fix:** Add `#[cfg(unix)]` to the real implementation and `#[cfg(not(unix))] pub fn is_wsl() -> bool { false }` as the stub. This makes the platform conditionality explicit at the declaration rather than relying on I/O failure semantics.

## Security

#### SEC-1: `CQS_PDF_SCRIPT` env var allows arbitrary script execution with no path validation
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:56-68
- **Description:** `find_pdf_script()` checks the `CQS_PDF_SCRIPT` env var first. If set, the value is used as a script path passed directly to `Command::new(&python).args([&script, ...])`. The only validation is a non-blocking warning if the extension isn't `.py`. An attacker who can set environment variables (e.g., via `.env` file, shell rc injection, or CI env manipulation) can point this to any script, which is then executed with the Python interpreter. The prior audit SEC-6 finding (v0.19.2, PR #504) addressed the log level but not the fundamental issue: user-controlled env var -> script execution with no guardrails beyond extension sniffing.
- **Suggested fix:** (1) Require `.py` extension and abort (not just warn) on mismatch. (2) Validate the script resolves inside the project root or a known cqs directory. (3) Document in SECURITY.md. Low practical risk given the threat model (local tool, no external users), but violates defense-in-depth.

#### SEC-2: `search_by_name` FTS query safety depends on `debug_assert` — compiled out in release builds
- **Difficulty:** easy
- **Location:** src/store/mod.rs:609-613, src/store/chunks.rs:1199-1203
- **Description:** `search_by_name` and `search_by_names_batch` build FTS5 queries via `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. Safety depends on `sanitize_fts_query` stripping all double quotes. A `debug_assert!(!normalized.contains('"'))` verifies this — but is compiled out in release builds. If `sanitize_fts_query` is ever refactored to miss the `"` character, release builds would have no runtime check and the format string would produce malformed FTS5 queries that could alter search semantics. The `sanitize_fts_query` function does currently strip `"` (mod.rs:144) and has fuzz coverage (mod.rs:916), so this is safe today. But the invariant is enforced by convention rather than by runtime check.
- **Suggested fix:** Change `debug_assert!` to `assert!` at both sites. The assertion runs once per search call on an already-constructed short string — negligible cost. This makes the safety invariant enforceable even if `sanitize_fts_query` is accidentally weakened.

#### SEC-3: `convert_directory` walk has no depth limit — deeply nested directories can exhaust resources
- **Difficulty:** easy
- **Location:** src/convert/mod.rs:345
- **Description:** `convert_directory` uses `walkdir::WalkDir::new(dir)` with no `.max_depth()` limit. CHM extraction (chm.rs:61) and webhelp (webhelp.rs:58) also use unbounded walks but are bounded by temp directory (CHM) or `content/` subdirectory (webhelp), and both have `MAX_PAGES = 1000` as a backstop. `convert_directory` has no equivalent file count limit — it processes all matching files in an arbitrary directory tree. A directory with extreme nesting or a large number of supported files (e.g., 100K HTML files) would produce unbounded memory use and processing time.
- **Suggested fix:** Add `.max_depth(50)` to the `WalkDir` in `convert_directory`. Add a `MAX_FILES` constant (e.g., 10000) and truncate with a warning, matching the `MAX_PAGES` pattern in CHM and webhelp.

#### SEC-4: HNSW index files written with no permission restriction — inconsistent with SQLite database protection
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs (save functions)
- **Description:** `Store::open()` (mod.rs:237-254) explicitly sets `0o600` permissions on SQLite database and WAL/SHM files as defense-in-depth. However, HNSW index files (`*.hnsw.graph`, `*.hnsw.data`, `*.hnsw.ids`, `*.hnsw.checksum`) are written by the HNSW persist module without any permission setting. These files contain the same embedding vectors that are in the database. On a shared system, HNSW files would be world-readable (subject to umask) while the database is owner-only. The `.cqs/` directory provides parent-level protection, but the file-level inconsistency is a gap.
- **Suggested fix:** After writing HNSW files in `save_to_directory()` and `atomic_save()`, set `0o600` permissions on each file with `#[cfg(unix)]` guard, same pattern as `Store::open`. Apply after the final rename in `atomic_save()`.

#### SEC-5: `find_7z` and `find_python` search `PATH` for executables without validation
- **Difficulty:** medium
- **Location:** src/convert/chm.rs:168-183, src/convert/pdf.rs:95-110
- **Description:** Both functions iterate hardcoded names (`["7z", "7za", "p7zip"]` and `["python3", "python"]`) and probe via `Command::new(name)`, which resolves via the system `PATH`. If an attacker can prepend a directory to `PATH`, a malicious binary would be executed instead. Standard PATH poisoning — mitigated in most environments by OS protections, but relevant in CI pipelines or shared systems where `PATH` is not fully trusted.
- **Suggested fix:** Low priority given the threat model. Document in SECURITY.md that `cqs convert` trusts the system PATH. For hardening: prefer absolute paths when available, or validate the found executable is in a trusted directory.

#### SEC-6: No size limit on git diff output — `run_git_diff` can allocate unbounded memory
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:166-188
- **Description:** `run_git_diff` calls `cmd.output()` which reads the entire stdout into memory. `git diff` output is unbounded — a large binary change, mass reformatting, or diff against an unrelated branch can produce hundreds of megabytes. The `read_stdin` function (mod.rs:153-162) has a 50MB limit, but `run_git_diff` has none. The output is then passed to `parse_diff_output` and `impact_diff` which process it in-memory. A pathological diff could cause OOM. The `run_git_log_line_range` (blame.rs) is naturally bounded by the `-n depth` flag, so it doesn't have this issue.
- **Suggested fix:** Add a size check after `cmd.output()`: if `output.stdout.len() > MAX_DIFF_SIZE` (e.g., 50MB, matching the stdin limit), bail with an error message suggesting `--base` to narrow the diff scope. Alternatively, stream the output through a length-limited reader.

## Data Safety

#### DS-1: HNSW save — partial file rename leaves inconsistent index on mid-loop failure
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:241-272
- **Description:** `HnswIndex::save()` renames files from the temp directory to the final location in a sequential loop over `["hnsw.graph", "hnsw.data", "hnsw.ids", "hnsw.checksum"]`. If the rename succeeds for `hnsw.graph` and `hnsw.data` but fails for `hnsw.ids` (e.g., disk full, permissions), the method returns `Err` but the final directory now contains new graph+data files with the old (or missing) id map. The checksum file is renamed last, so a crash before that step means `load()` will fail checksum verification — which is the intended safety net. However, the specific failure mode where rename #3 (`hnsw.ids`) fails but #1 and #2 succeeded leaves new graph+data alongside the old id map from a previous save. On next `load()`, checksums from the old checksum file won't match the new graph/data, so load fails. But the old id map is now permanently desynchronized — a subsequent successful save would need to happen to recover. The recovery path works (next `cqs index` rebuilds everything), but the intermediate state is confusing and could mislead debugging.
- **Suggested fix:** Rename files in reverse dependency order: checksum last (already done), but also track which renames succeeded. On failure mid-loop, attempt to roll back already-renamed files by renaming them back from final to temp. If rollback also fails, log a warning with explicit recovery instructions ("run `cqs index --force`").

#### DS-2: `acquire_index_lock` — truncate(true) races with concurrent read of PID
- **Difficulty:** easy
- **Location:** src/cli/files.rs:57, 99, 104
- **Description:** Both `acquire_index_lock` and `try_acquire_index_lock` open the lock file with `.truncate(true)` before calling `try_lock()`. If process A holds the lock and process B opens with `truncate(true)`, B truncates the lock file's content (clearing A's PID) before attempting the lock. The `try_lock()` call will fail (A still holds the OS lock), but A's PID is now gone from the file. If A crashes while the file is truncated, the stale lock detection in `acquire_index_lock` reads an empty file, fails to parse a PID (`content.trim().parse::<u32>()` fails on empty string), and falls through to the "lock held" error without cleaning up the stale lock. The user gets "Another cqs process is indexing" with no way to detect the stale state. Next attempt also fails because `retried` is only set when a PID is found and the process is dead.
- **Suggested fix:** Open without `.truncate(true)`. Only write the PID after successfully acquiring the lock. This prevents any process from clearing another's PID metadata.

#### DS-3: `extract_relationships` not atomic with chunk upserts — crash between pipeline and relationships leaves index with chunks but no call graph
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:120-133
- **Description:** `cmd_index` runs the pipeline (which upserts chunks+calls per file via `upsert_chunks_and_calls`), then separately calls `extract_relationships()` which overwrites the call graph with `upsert_function_calls` (per file, each in its own transaction). The pipeline's chunk-level `calls` table and the relationship-level `function_calls` table are updated at different times. If `cqs index` is interrupted (Ctrl+C) between the pipeline completing and `extract_relationships` finishing, the `function_calls` table contains stale data from the previous index run. Queries using `function_calls` (blame, callers with line-level precision) return stale call sites that don't match current chunk content. The `calls` table (chunk-level) is correct since the pipeline updates it atomically with chunks, but `function_calls` (line-level) is stale. No staleness indicator exists for this case.
- **Suggested fix:** Add a metadata flag (`"relationships_stale" = "true"`) set before `extract_relationships` starts and cleared after it finishes. Commands using `function_calls` (blame, callers with context) can check this flag and warn. Alternatively, clear `function_calls` at the start of pipeline (before chunks are written) so stale data is never returned — only "no data" during the window.

#### DS-4: `prune_stale_calls` and `prune_stale_type_edges` execute outside GC's index lock scope after chunk pruning
- **Difficulty:** easy
- **Location:** src/cli/commands/gc.rs:44-59
- **Description:** `cmd_gc` acquires the index lock at line 23, then runs `prune_missing` (chunks), `prune_stale_calls`, and `prune_stale_type_edges` sequentially. Each operates in its own transaction. Between `prune_missing` (which deletes chunks) and `prune_stale_calls` (which deletes orphan call entries), a concurrent `cqs watch` process could insert new function_calls referencing chunks that were just pruned. The watch process checks `try_acquire_index_lock` and skips if locked — so normally this doesn't happen. But on WSL where advisory locking is unreliable (documented in PB-4), watch mode could proceed with a reindex cycle that inserts new rows into `function_calls` for a file that GC just pruned, creating orphan call entries. Self-heals on next GC run, but the window exists.
- **Suggested fix:** Wrap all three prune operations in a single transaction. `prune_missing` already uses a transaction internally, so either: (a) expose a `prune_all()` that combines all three in one tx, or (b) accept the self-healing property and document the WSL advisory locking limitation as the root cause (not this code).

#### DS-5: `notes_summaries_cache` invalidation is caller-responsibility — easy to miss on new mutation paths
- **Difficulty:** easy
- **Location:** src/store/mod.rs:746-754, src/store/notes.rs:122,224
- **Description:** The `notes_summaries_cache` (RwLock<Option<Vec<NoteSummary>>>) is manually invalidated via `self.invalidate_notes_cache()` after note mutations. Currently called at two sites: `upsert_notes_batch` (line 122) and `replace_notes_for_file` (line 224). If a future mutation path is added to notes (e.g., `delete_note_by_id`, `update_note_sentiment`) without calling `invalidate_notes_cache()`, the cache returns stale data. The pattern is fragile because: (1) the cache lives on `Store` not `notes.rs`, so the relationship is non-local, (2) there's no compile-time enforcement. Prior audit DS-3 (v0.19.2) fixed `OnceLock` → `RwLock` for cache invalidation, but the caller-responsibility pattern remains.
- **Suggested fix:** Add a `/// IMPORTANT: Call `invalidate_notes_cache()` after any note mutation` doc comment on the `notes_summaries_cache` field. Or, make all note mutations go through a single `fn mutate_notes(&self, f: impl FnOnce(&mut Transaction)) -> Result<()>` that automatically invalidates the cache post-commit.

#### DS-6: `bytes_to_embedding` and `embedding_slice` silently skip corrupted embeddings — no aggregate corruption signal
- **Difficulty:** easy
- **Location:** src/store/helpers.rs:696-706, 714-725
- **Description:** `bytes_to_embedding` and `embedding_slice` return `None` when the byte length doesn't match expected dimensions, with only `tracing::trace!` logging. Callers use `.filter_map()` to silently skip these entries. In `search_notes` (notes.rs:163), `note_embeddings` (notes.rs:338), and multiple search paths, corrupted embeddings are individually skipped with no aggregation. If a schema migration or embedder bug produces widespread dimension mismatches, the user sees degraded search quality (fewer results, missing notes) with no visible error. The per-entry trace-level logging requires `RUST_LOG=trace` to see, and even then doesn't aggregate (100 skips = 100 individual trace lines, no summary).
- **Suggested fix:** Add a counter to search paths that tracks skipped-due-to-dimension-mismatch entries. If the count exceeds a threshold (e.g., >5% of scanned entries), emit a `tracing::warn!` with the count and a "run `cqs index --force` to re-embed" suggestion. This surfaces systematic corruption without impacting the common case (zero skips).

## Resource Management

#### RM-1: `chm_to_markdown` and `webhelp_to_markdown` accumulate all page content in memory then `join()` — peak is 2× output size
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:119-157, src/convert/webhelp.rs:93-128
- **Description:** Both converters collect all converted Markdown pages into a `Vec<String>` (`all_markdown`) and then call `.join("\n\n---\n\n")`. The `join` call allocates a new contiguous string equal to the total of all pages, while the `Vec<String>` holding the original pages is still alive. Peak memory = `all_markdown` Vec + `merged` String ≈ 2× output size. With `MAX_PAGES = 1000` pages and typical API reference pages converting to 50–150 KB each, average peak is ~100–300 MB; worst case with dense HTML is ~500 MB + 500 MB = ~1 GB before `all_markdown` is dropped. There is no guard on cumulative output size — only a page count guard (`MAX_PAGES = 1000`). A single 100 MB HTML page would produce a 100 MB string and pass the page-count check.
- **Suggested fix:** Two independent fixes: (1) Add a cumulative byte guard: after `all_markdown.push(md)`, check `all_markdown.iter().map(|s| s.len()).sum::<usize>() > MAX_TOTAL_BYTES` (e.g., 200 MB) and `break` with a warning. (2) Write to a `String` with `push_str` instead of collecting into a Vec then joining, so only one copy exists at a time. Example: `let mut merged = String::new(); for md in all_markdown { if !merged.is_empty() { merged.push_str("\n\n---\n\n"); } merged.push_str(&md); }`.

#### RM-2: `html_file_to_markdown` and `markdown_passthrough` load entire file into memory with no size guard
- **Difficulty:** easy
- **Location:** src/convert/html.rs:32, src/convert/mod.rs:113
- **Description:** `html_file_to_markdown` reads the entire HTML file into a `String` with `std::fs::read_to_string(path)`, then passes it to `html_to_markdown`. `markdown_passthrough` does the same. Neither checks file size before reading. A 500 MB HTML file would allocate a 500 MB `String`, and after conversion the original string and the Markdown output both exist simultaneously in memory. The CHM and Web Help converters guard against page count but not per-page size — individual pages can be arbitrarily large. The `run_git_diff` finding (SEC-6) already identifies the same pattern for git output.
- **Suggested fix:** Check `path.metadata()?.len()` before reading. If greater than a configurable limit (e.g., 100 MB), bail with a descriptive error: `"File too large to convert ({}MB > 100MB limit): {}"`. Match the limit used for the git diff guard in SEC-6.

#### RM-3: `BatchContext::refs` — loaded reference indexes accumulate for the session lifetime with no eviction
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:60, 139-168
- **Description:** `BatchContext::refs` is a `RefCell<HashMap<String, ReferenceIndex>>` that is populated on first access via `get_ref()` and never evicted. Each `ReferenceIndex` holds a full `Store` (SQLite pool with up to 1 read-only connection at 64 MB mmap + 4 MB cache) and an optional `HnswIndex` (loaded HNSW graph for the reference). A batch session referencing all configured references accumulates all of them simultaneously. With 5 references of 100k chunks each, each HNSW graph is ~300 MB; 5 loaded simultaneously = ~1.5 GB HNSW + 5 × 68 MB SQLite = ~1.9 GB. The RM-16 fix (PR #502) correctly limits to loading only the target reference per `get_ref` call, but all previously loaded references remain in memory for the full session.
- **Suggested fix:** This is largely acceptable as-is — references are loaded on demand and the common case is 1-2 references. Add a tracing note at load time: `tracing::info!(name, total_loaded = refs.len(), "Reference loaded; {} references in memory", refs.len())` so users can see accumulation in traces. If eviction is needed, switch to an LRU map bounded at N references.

#### RM-4: Pipeline `PIPELINE_CHANNEL_DEPTH = 256` uses the same depth for both `ParsedBatch` (chunks only) and `EmbeddedBatch` (chunks + embeddings) — embed channel can hold ~40 MB
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:37, 652-657
- **Description:** All three pipeline channels (`parse_tx/rx`, `embed_tx/rx`, `fail_tx/rx`) use `bounded(PIPELINE_CHANNEL_DEPTH)` with depth 256. `ParsedBatch` holds chunks with content strings — roughly 15 MB at full depth. `EmbeddedBatch` also holds chunk content *plus* 769-dim f32 embeddings (~3 KB each) — roughly 40 MB at full depth. If the writer (SQLite insert) stalls (e.g., fsync, WAL checkpoint), backpressure fills the embed channel before the parse channel, holding ~40 MB of embedded but unwritten chunks. This is bounded and not catastrophic, but using a single depth constant for payloads that differ 2.6× in size is suboptimal. The constant's comment says "Files to parse per batch (bounded memory)" which applies to `FILE_BATCH_SIZE`, not `PIPELINE_CHANNEL_DEPTH`.
- **Suggested fix:** Use separate depth constants: `const PARSE_CHANNEL_DEPTH: usize = 256` (lightweight) and `const EMBED_CHANNEL_DEPTH: usize = 64` (heavy payloads). The embed channel at depth 64 would hold ~10 MB — more proportionate to the payload difference. Also fix the misleading comment on line 36 which describes `FILE_BATCH_SIZE`, not `PIPELINE_CHANNEL_DEPTH`.

#### RM-5: Watch mode `last_indexed_mtime` grows to cover all indexed files and is only pruned on successful reindex — deleted-file cleanup depends on expensive `root.join(f).exists()` per entry
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:117, 307-311
- **Description:** `last_indexed_mtime: HashMap<PathBuf, SystemTime>` accumulates an entry for every file ever indexed in the watch session. The `retain(|f, _| root.join(f).exists())` call (line 311) prunes deleted files, but only runs in the success branch of `reindex_files`. If reindexing fails (e.g., embedder error), the prune is skipped — deleted files stay in the map indefinitely. On a large project with 50k source files, the map holds 50k `PathBuf`+`SystemTime` pairs (~50k × ~100 bytes ≈ 5 MB), which is tolerable. The `exists()` check on every entry on every reindex cycle is O(file_count) filesystem calls. For 50k files that's 50k `stat()` calls per reindex event, which adds latency to otherwise fast incremental reindexes. The `retain` runs every cycle regardless of how many files changed.
- **Suggested fix:** Move the `retain` outside the `Ok` branch — run it after every reindex attempt (success or failure). For the `exists()` cost, consider only pruning when a delete event is detected: add `pending_deletes: HashSet<PathBuf>` alongside `pending_files`, and in `collect_events` when a `Remove` event fires, add to `pending_deletes`. Then in `process_file_changes`, only call `last_indexed_mtime.remove()` for the deleted entries rather than scanning all.

#### DS-7: `cmd_index --force` removes old database before creating the new one — interruption loses entire index
- **Difficulty:** easy
- **Location:** src/cli/commands/index.rs:69-76
- **Description:** When `--force` is passed, `cmd_index` removes the existing `index.db` at line 71, then opens a new empty store at line 73 and calls `init()`. If the process is killed between the `remove_file` and the completion of `run_index_pipeline`, the user has no index at all — neither old nor new. For a large codebase, the pipeline can take minutes, so this window is significant. The HNSW save uses temp-dir-then-rename for crash safety, but the SQLite database itself gets no such protection.
- **Suggested fix:** Instead of removing the old index first, build the new index to a temp path (e.g., `index.db.new`), then atomically rename over the old one after the pipeline completes successfully. If interrupted, the old index remains intact. This is the same pattern used for HNSW save and note file rewrites.


## Performance

#### PF-1: `search_by_candidate_ids` parses language/chunk_type strings to enums per candidate when filters are active
- **Difficulty:** easy
- **Location:** src/search.rs:884-905
- **Description:** In the HNSW-guided search path (`search_by_candidate_ids`), when `filter.languages` or `filter.chunk_types` is set, the scoring loop calls `candidate.language.parse::<Language>()` and `candidate.chunk_type.parse::<ChunkType>()` for every candidate (up to 500). Parsing enum from a string on every iteration is O(candidates) string comparisons plus allocation for the parse error path. Since the filter sets are fixed before the loop, a pre-built `HashSet<&str>` (or even `HashSet<String>`) of the allowed language/type string representations would reduce this to a single hash lookup per candidate, and avoids the parse entirely.
- **Suggested fix:** Before the loop, compute `let lang_strs: Option<HashSet<String>> = filter.languages.as_ref().map(|ls| ls.iter().map(|l| l.to_string()).collect())` and similarly for chunk_types. Inside the loop, replace `candidate.language.parse()` + `langs.contains(&lang)` with `!lang_strs.contains(&candidate.language)`.

#### PF-2: `search_filtered` clones all semantic IDs to `Vec<String>` solely to pass to `rrf_fuse`
- **Difficulty:** easy
- **Location:** src/search.rs:757, src/store/mod.rs:661
- **Description:** After the brute-force scoring loop, `scored: Vec<(String, f32)>` contains the top embedding-scored candidates. When RRF is enabled, line 757 creates `semantic_ids: Vec<String>` by cloning every ID string from `scored`, then passes it to `rrf_fuse(&semantic_ids, ...)`. Inside `rrf_fuse`, these Strings are immediately borrowed as `&str` for the HashMap (line 670). After the call, the cloned `semantic_ids` Vec is dropped. For a typical `semantic_limit` of 30-60 results, this is 30-60 unnecessary String heap allocations per search. `rrf_fuse` signature could accept `&[(String, f32)]` (the scored Vec directly) and extract IDs as `id.as_str()` internally, or take `impl IntoIterator<Item = &str>`.
- **Suggested fix:** Change `rrf_fuse` to accept `semantic_ids: &[(String, f32)]` and let it iterate `semantic_ids.iter().map(|(id, _)| id.as_str())` internally. Remove the intermediate `Vec<String>` at line 757. Same pattern applies to `search_by_candidate_ids` line 943.

#### PF-3: `search_by_name` re-lowercases the query string for every result in the post-fetch scoring loop
- **Difficulty:** easy
- **Location:** src/store/mod.rs:646
- **Description:** `search_by_name` fetches FTS-matched chunks then rescores each with `score_name_match(&chunk.name, name)`. Inside `score_name_match`, `name.to_lowercase()` is called on every invocation — it allocates a new `String` each time. For a limit-20 name lookup (the default from `resolve_target`), this means 20 identical `to_lowercase()` allocations for the same query string. `score_name_match_pre_lower` already exists for exactly this pattern (pre-lowercased strings, inline doc says "use when calling in a loop").
- **Suggested fix:** Before the result iterator, compute `let name_lower = name.to_lowercase();` and call `score_name_match_pre_lower(&chunk.name.to_lowercase(), &name_lower)` (or pre-lower `chunk.name` too). The `score_name_match_pre_lower` function is at `src/store/helpers.rs:648`.

#### PF-4: `score_confidence` clones all candidate IDs to a separate `Vec<String>` before chunking
- **Difficulty:** easy
- **Location:** src/store/calls.rs:881
- **Description:** `score_confidence` takes `candidates: Vec<LightChunk>` and immediately creates `candidate_ids: Vec<String>` by cloning `c.id` for every element. This Vec is used only for `candidate_ids.chunks(BATCH_SIZE)` in the SQL batch loop. For large codebases with many dead-code candidates (hundreds to thousands), this is an O(N) string clone with the only purpose being that the `.chunks()` slice iterator works. The original `candidates` Vec has the same length and can be chunked directly.
- **Suggested fix:** Replace `candidate_ids.chunks(BATCH_SIZE)` with `candidates.chunks(BATCH_SIZE)`, extracting IDs inline: `for id in batch { q = q.bind(&id.id); }`. This eliminates the ID clone Vec entirely.

#### PF-5: `fetch_active_files` uses `IN (subquery)` where a JOIN is more efficient
- **Difficulty:** easy
- **Location:** src/store/calls.rs:841-843
- **Description:** `fetch_active_files` runs `WHERE c.name IN (SELECT DISTINCT callee_name FROM function_calls)` to find files containing called functions. SQLite must first materialize the inner SELECT (all distinct callee_names) then check each chunk name against that set. With an index on `function_calls.callee_name` and `chunks.name`, a JOIN is equivalent and SQLite's query planner typically handles it more efficiently: `JOIN function_calls fc ON c.name = fc.callee_name`. The `DISTINCT` on callee_name in the subquery also forces a sort/dedup step that a `SELECT DISTINCT c.origin` with JOIN avoids.
- **Suggested fix:** Replace the correlated subquery with a direct JOIN: `SELECT DISTINCT c.origin FROM chunks c JOIN function_calls fc ON c.name = fc.callee_name`. SQLite's query planner can use `idx_fcalls_callee` (on `callee_name`) and `idx_chunks_name` (on `name`) for a nested-loop join.

#### PF-6: `build_batched` makes two passes over each batch — separate validation loop then data collection
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs:140-157
- **Description:** For each batch, the code first iterates all `(id, emb)` pairs to validate dimensions and emit `tracing::trace!` per item (lines 140-148), then immediately iterates the same batch again to build `data_for_insert` (lines 155-157). With 10K-row batches (the production default), this is 20K iterations per batch when a single combined loop would do 10K. The trace log is at `trace` level so it's typically inactive in production, making the first loop almost entirely wasted work.
- **Suggested fix:** Combine the validation and collection into one loop. Move dimension checking inside the collection loop: validate `emb.len()` before pushing to `data_for_insert`. Move `tracing::trace!("Adding {} to HNSW index", id)` inside the combined loop too. This halves the iteration count per batch.
