# Audit Findings — v0.12.12

Generated: 2026-02-21

## Documentation

#### DOC-10: README `--expand 2` example is wrong — flag is boolean, not numeric
- **Difficulty:** easy
- **Location:** README.md:84
- **Description:** The example `cqs "query" --expand 2` passes a numeric argument to `--expand`, but the CLI defines `--expand` on query as a boolean flag (`expand: bool` in `src/cli/mod.rs:179`). This command would fail with "unexpected argument '2'". The comment "Expand results via call graph" is also misleading — `--expand` triggers small-to-big parent context retrieval, not call graph expansion. Call graph expansion is `--expand` on the `gather` subcommand (which does take a numeric value).
- **Suggested fix:** Change to `cqs "query" --expand  # Include parent context (small-to-big retrieval)` or remove the example entirely since it duplicates `gather --expand`.

#### DOC-11: README missing `cqs health` command
- **Difficulty:** easy
- **Location:** README.md (Maintenance section, ~line 236)
- **Description:** `cqs health` was added in v0.12.8 but is not documented in any user-facing README section. It only appears in the Claude Code Integration section indirectly via `cqs suggest`. The command provides a codebase quality snapshot (dead code, staleness, hotspots, coverage) and supports `--json`.
- **Suggested fix:** Add `cqs health` to the Maintenance section with a brief example: `cqs health` / `cqs health --json`.

#### DOC-12: README missing `cqs suggest` command
- **Difficulty:** easy
- **Location:** README.md (Maintenance section, ~line 236)
- **Description:** `cqs suggest` was added in v0.12.8 but is not documented in the README at all — not in the main sections or the Claude Code Integration section. The command auto-detects note-worthy patterns (dead code clusters, untested hotspots, stale mentions) and suggests notes. Supports `--apply`, `--json`.
- **Suggested fix:** Add to Maintenance section: `cqs suggest` / `cqs suggest --apply` / `cqs suggest --json`. Also add to the Claude Code Integration command list.

#### DOC-13: CONTRIBUTING.md missing `health.rs`, `suggest.rs`, `deps.rs` from commands list
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:89
- **Description:** The `commands/` directory listing on line 89 is missing three command implementation files: `health.rs` (added v0.12.8), `suggest.rs` (added v0.12.8), and `deps.rs` (added v0.12.11). All three exist on disk and are wired into the CLI dispatch.
- **Suggested fix:** Add `health.rs, suggest.rs, deps.rs` to the comma-separated list on line 89.

#### DOC-14: CONTRIBUTING.md missing `health.rs`, `suggest.rs` from library-level files
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:158-161 (between `ci.rs` and `where_to_add.rs`)
- **Description:** The library-level architecture listing is missing `health.rs` (codebase quality snapshot) and `suggest.rs` (auto-suggest notes from patterns). Both exist at `src/health.rs` and `src/suggest.rs`.
- **Suggested fix:** Add `health.rs - Codebase quality snapshot (dead code, staleness, hotspots)` and `suggest.rs - Auto-suggest notes from patterns (dead clusters, untested hotspots, stale mentions)` to the listing.

#### DOC-15: CHANGELOG missing comparison URLs for v0.12.11 and v0.12.12
- **Difficulty:** easy
- **Location:** CHANGELOG.md:983-984
- **Description:** The `[Unreleased]` comparison URL points to `v0.12.9...HEAD` instead of `v0.12.12...HEAD`. Versions v0.12.11 and v0.12.12 have changelog entries but no comparison URL footer entries. This means the version headers aren't clickable links.
- **Suggested fix:** Update `[Unreleased]` to `v0.12.12...HEAD` and add `[0.12.12]: https://github.com/jamie8johnson/cqs/compare/v0.12.11...v0.12.12` and `[0.12.11]: https://github.com/jamie8johnson/cqs/compare/v0.12.10...v0.12.11`.

#### DOC-16: ROADMAP shows `cqs onboard` and `cqs drift` as unchecked
- **Difficulty:** easy
- **Location:** ROADMAP.md:39,41
- **Description:** Both `cqs onboard` (line 39) and `cqs drift` (line 41) are marked `[ ]` (unchecked) in the "Next — New Commands" section, but both are fully implemented: `onboard` is in the Unreleased changelog, and `drift` is in the Unreleased changelog. Both have CLI commands, library modules, tests, and batch mode support.
- **Suggested fix:** Change `[ ]` to `[x]` for both entries on lines 39 and 41.

#### DOC-17: SECURITY.md missing reranker model download as network request
- **Difficulty:** easy
- **Location:** SECURITY.md:36-39
- **Description:** The Network Requests section only documents the E5-base-v2 model download (~440MB). The `--rerank` flag (added v0.12.7) triggers a separate download of `cross-encoder/ms-marco-MiniLM-L-6-v2` (~91MB) from HuggingFace on first use (`src/reranker.rs:19`). This is an undocumented network request.
- **Suggested fix:** Add a second bullet under Network Requests: "**Reranker model download** (`cqs "query" --rerank` on first use): Downloads ~91MB model from HuggingFace Hub. Source: `huggingface.co/cross-encoder/ms-marco-MiniLM-L-6-v2`. One-time download, cached in `~/.cache/huggingface/`."

#### DOC-18: README missing `--include-types` flag on `cqs impact`
- **Difficulty:** easy
- **Location:** README.md:211-213 (Code Intelligence section)
- **Description:** The `cqs impact` examples don't show the `--include-types` flag (added in v0.12.11 with type edges/deps). This flag includes type-impacted functions via shared type dependencies, extending impact analysis beyond just call graph. Defined at `src/cli/mod.rs:373`.
- **Suggested fix:** Add example: `cqs impact search_filtered --include-types  # include type-dependency impact`.

## Error Handling

#### EH-23: `onboard_to_json` silently returns null on serialization failure
- **Difficulty:** easy
- **Location:** src/onboard.rs:239
- **Description:** `serde_json::to_value(result).unwrap_or_default()` swallows serialization errors, returning `serde_json::Value::Null`. If any field fails to serialize (unlikely but possible with custom Serialize impls or future changes), the caller gets a silent null instead of an error. Since `OnboardResult` derives `Serialize`, this is low risk today, but the pattern masks future breakage.
- **Suggested fix:** Return `Result<serde_json::Value>` or use `.expect("OnboardResult is Serialize")` to fail fast on invariant violation rather than returning garbage.

#### EH-24: `borrow_ref` panics with `.expect()` in non-test code
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:128
- **Description:** `map.get(name).expect("ref must be loaded via get_ref first")` will panic if called with an unloaded reference name. The only caller (line 909) always calls `get_ref` first, but the method is `pub(crate)` and the invariant is enforced only by convention, not the type system. A future caller omitting `get_ref` causes a panic in production code.
- **Suggested fix:** Return `Option<Ref<'_, ReferenceIndex>>` or `Result` instead of panicking. Alternatively, combine `get_ref` + `borrow_ref` into a single method that loads-or-returns.

#### EH-25: `serde_json::to_string().unwrap()` in batch JSONL output loop
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2194, 2207, 2213, 2217, 2222
- **Description:** Five `serde_json::to_string(&json_value).unwrap()` calls in the batch REPL loop. While `serde_json::Value` serialization cannot fail (it's already valid JSON), the pattern is fragile — if any of these are refactored to serialize a struct directly, the unwrap becomes a panic risk. Inconsistent with the rest of the codebase which avoids `.unwrap()` in non-test code.
- **Suggested fix:** Use `serde_json::to_string(&v).unwrap_or_else(|e| format!("{{\"error\":\"serialization failed: {e}\"}}"))` or extract a helper that handles the error.

#### EH-26: `AnalysisError::Embedder` used for non-embedding errors in onboard
- **Difficulty:** easy
- **Location:** src/onboard.rs:106, src/onboard.rs:384
- **Description:** Two error sites misuse `AnalysisError::Embedder`:
  - Line 106: "No relevant code found for concept" — this is a search/empty-result error, not an embedding failure.
  - Line 384: "Entry point not found in index" — this is a store lookup miss, not an embedding failure.
  Error consumers that match on `AnalysisError::Embedder` (for retry/fallback logic) would incorrectly treat these as embedding infrastructure failures.
- **Suggested fix:** Add a new `AnalysisError::NotFound(String)` variant for "no results" errors, keeping `Embedder` for actual ONNX/embedding failures.

#### EH-27: `Store::open` calls without `.context()` in drift command
- **Difficulty:** easy
- **Location:** src/cli/commands/drift.rs:46, src/cli/commands/drift.rs:52
- **Description:** Two `Store::open(&path)?` calls propagate bare SQLite errors without any context about which store failed to open. If the reference store's DB is corrupted, the user sees a raw sqlx error with no indication it was the reference (not project) store. The `bail!` checks above verify file existence, but `Store::open` can fail for other reasons (corruption, permissions, schema mismatch).
- **Suggested fix:** Add `.context(format!("Failed to open reference store at {}", ref_db.display()))` and `.context("Failed to open project store")`.

#### EH-28: `Store::open` without `.context()` in batch drift dispatch
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1770
- **Description:** Same pattern as EH-27 but in the batch drift handler. `cqs::Store::open(&ref_db)?` propagates a bare error with no indication which store failed.
- **Suggested fix:** Add `.context(format!("Failed to open reference store '{}'", reference))`.

#### EH-29: `pick_entry_point` returns sentinel value instead of error
- **Difficulty:** easy
- **Location:** src/onboard.rs:305
- **Description:** `unwrap_or_else(|| ("unknown".to_string(), PathBuf::new()))` returns a sentinel ("unknown", empty path) when no entry point can be found. The caller at line 112 does not check for this sentinel — it passes "unknown" to `fetch_entry_point` which will fail with a confusing "Entry point 'unknown' not found in index" error. The empty scout check on line 105 should prevent reaching this, but defensive code shouldn't produce misleading error messages.
- **Suggested fix:** This code path is theoretically unreachable (line 105 returns early on empty). Either add a `debug_assert!(!scout_result.file_groups.is_empty())` documenting the invariant, or return `Option<(String, PathBuf)>` and handle `None` explicitly with a clear error.

#### EH-30: `get_embeddings_by_hashes` swallows SQL errors returning partial results
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:658-661
- **Description:** The embedding cache lookup logs a warning and `continue`s on SQL errors, returning partial results. This is called from the indexing pipeline (`prepare_for_embedding` in pipeline.rs:139), where a partial cache hit means chunks that *do* have cached embeddings will be re-embedded unnecessarily. The caller cannot distinguish "no cached embedding" from "cache lookup failed". This wastes GPU/CPU time on re-embedding and could mask database corruption during indexing.
- **Suggested fix:** Return `Result<HashMap<...>, StoreError>` so the pipeline can decide whether to warn-and-continue or abort. The current warn-and-continue behavior is fine as degraded operation, but the function signature should indicate fallibility.

#### EH-31: Missing `.context()` on `Embedder::new()` and `embed_query()` in batch handlers
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:561-562 (dispatch_search), src/cli/batch.rs:894 (dispatch_gather), src/cli/batch.rs:1380-1382 (dispatch_onboard), src/cli/batch.rs:1393-1395 (dispatch_scout), src/cli/batch.rs:1405-1407 (dispatch_where)
- **Description:** Multiple batch dispatch functions call `ctx.embedder()?` and `embedder.embed_query(query)?` without `.context()`. The embedder errors reference ONNX runtime internals (model paths, dimension mismatches) — adding context like "Failed to embed query for search" tells the user which batch command triggered the failure, especially useful in JSONL output where the command/query association can be lost.
- **Suggested fix:** Add `.context("embedding query for batch search")` or similar to each `embed_query` call. The `ctx.embedder()` call already has decent error context from `Embedder::new()`.

#### EH-32: `staleness.rs` `warn_stale_results` downgrades error to `tracing::debug`
- **Difficulty:** easy
- **Location:** src/cli/staleness.rs:37
- **Description:** When `store.check_origins_stale()` fails, the error is logged at `debug` level — invisible unless `RUST_LOG=debug` is set. While staleness checks are intentionally non-fatal (comment says "should never break a query"), database errors during staleness checks could indicate corruption or concurrency issues that deserve `warn` level visibility. The comment on line 17 says "Errors are logged and swallowed" but debug != logged for most users.
- **Suggested fix:** Change `tracing::debug!` to `tracing::warn!` to match the stated intent and the pattern used elsewhere in the codebase (e.g., health.rs:53, onboard.rs:121).

## API Design

#### AD-20: chunk_type serialized inconsistently — Display vs Debug across new types
- **Difficulty:** easy
- **Location:** src/drift.rs:77, src/onboard.rs:321
- **Description:** `DriftEntry.chunk_type` is set via `.to_string()` (Display impl, produces lowercase `"function"`), while `OnboardEntry.chunk_type` is set via `format!("{:?}", ...)` (Debug impl, produces PascalCase `"Function"`). Both are `String` fields on serializable structs that appear in JSON output. The same conceptual field produces different string representations depending on which command you run. `GatheredChunk` and `DiffEntry` avoid this by using the `ChunkType` enum directly.
- **Suggested fix:** Add `Serialize` derive to `ChunkType` (with `#[serde(rename_all = "lowercase")]` or custom impl) so `DriftEntry` and `OnboardEntry` can use the enum directly. If that's too broad, at least standardize both to use `.to_string()` (Display) for consistency.

#### AD-21: get_types_used_by returns Vec<(String, String)> instead of typed struct
- **Difficulty:** easy
- **Location:** src/store/types.rs:242, src/store/types.rs:328
- **Description:** `get_types_used_by()` and `get_types_used_by_batch()` return `Vec<(String, String)>` where the tuple is `(type_name, edge_kind)`. This is opaque at call sites — callers must remember tuple field order. The same data is represented by `TypeInfo { type_name, edge_kind }` in `onboard.rs:63`, but the store API doesn't use it. This forces `onboard.rs:395` to manually destructure and reconstruct: `|(type_name, edge_kind)| TypeInfo { type_name, edge_kind }`.
- **Suggested fix:** Either reuse `TypeInfo` from onboard (move to a shared location) or create a `TypeEdgeRef { type_name: String, edge_kind: String }` in `store/types.rs` and return `Vec<TypeEdgeRef>`. The batch variant would return `HashMap<String, Vec<TypeEdgeRef>>`.

#### AD-22: TypeGraph missing standard derives (Debug, Clone)
- **Difficulty:** easy
- **Location:** src/store/types.rs:32
- **Description:** `TypeGraph` is a `pub` type re-exported from `store::mod.rs` but has zero derives — no `Debug`, no `Clone`. `TypeEdgeStats` (same file, line 19) has `Debug, Clone, Default`. All other new public types in the codebase (DriftEntry, DriftResult, OnboardResult, etc.) have at least `Debug, Clone`. Missing `Debug` makes it hard to log or inspect in tests.
- **Suggested fix:** Add `#[derive(Debug, Clone)]` to `TypeGraph`.

#### AD-23: ResolvedTarget missing standard derives (Debug, Clone)
- **Difficulty:** easy
- **Location:** src/search.rs:27
- **Description:** `ResolvedTarget` is a public type re-exported from `lib.rs:122` but has zero derives. No `Debug`, no `Clone`. Its fields (`ChunkSummary` and `Vec<SearchResult>`) both support Clone and Debug. Every other public result type in the search/analysis layer has at minimum `Debug, Clone`.
- **Suggested fix:** Add `#[derive(Debug, Clone)]` to `ResolvedTarget`.

#### AD-24: Note missing Serialize derive — hand-rolled JSON in display.rs
- **Difficulty:** easy
- **Location:** src/note.rs:61, src/cli/display.rs:246-249
- **Description:** `Note` has `#[derive(Debug, Clone)]` but no `Serialize`. The CLI display code at `display.rs:246-249` manually constructs JSON: `json!({"id": r.note.id, "text": r.note.text, "sentiment": r.note.sentiment, "mentions": r.note.mentions})`. This pattern is repeated at `display.rs:477-480`. Meanwhile, `NoteEntry` (the TOML-facing sibling) derives both `Serialize` and `Deserialize`. Adding `Serialize` to `Note` would allow `serde_json::to_value(note)` instead of manual field listing, and prevent drift if fields are added.
- **Suggested fix:** Add `serde::Serialize` to `Note`'s derive list. Replace manual JSON construction with `serde_json::to_value(&r.note)`.

#### AD-25: Drift types not re-exported from lib.rs — inconsistent access pattern
- **Difficulty:** easy
- **Location:** src/lib.rs:64, src/drift.rs
- **Description:** `drift` is `pub mod` in lib.rs (line 64) but its types (`DriftResult`, `DriftEntry`, `detect_drift`) are not re-exported with `pub use`. CLI code accesses them via `cqs::drift::detect_drift()`. By contrast, the `onboard` module's types are all re-exported (`pub use onboard::{onboard, OnboardResult, ...}`) and accessed as `cqs::onboard()`. Same inconsistency with `diff` which has `pub use diff::semantic_diff`. This creates two access patterns for the same library.
- **Suggested fix:** Add `pub use drift::{detect_drift, DriftEntry, DriftResult};` to lib.rs, matching the onboard pattern. Or document that drift is intentionally module-scoped (but then onboard should be too).

#### AD-26: OnboardEntry.edge_kind is stringly-typed — TypeEdgeKind enum exists
- **Difficulty:** easy
- **Location:** src/onboard.rs:65
- **Description:** `TypeInfo.edge_kind` is `String`, but the codebase has a well-defined `TypeEdgeKind` enum (`src/parser/types.rs:87`) with `FromStr`, `Display`, and `as_str()` implementations. The string value comes from `get_types_used_by()` which returns `(String, String)` from the database. Downstream consumers of `TypeInfo` get no compile-time guarantees about valid edge kinds.
- **Suggested fix:** This is a consequence of AD-21 (tuple returns). If that's fixed to return a proper type with `TypeEdgeKind`, `TypeInfo.edge_kind` should also become `TypeEdgeKind`. In the interim, at least document the valid values in the field doc comment.

## Observability

#### OB-15: `run_index_pipeline` missing entry tracing span
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:222
- **Description:** `run_index_pipeline` is the main entry point for the 3-stage indexing pipeline (parser, embedder, writer). It has no `tracing::info_span!` at entry, despite being a long-running operation (seconds to minutes). Child operations (parser thread, GPU/CPU embedder threads) do log, and the function logs a summary at completion (line 667), but there is no enclosing span to correlate all pipeline activity under a single trace. This makes it harder to filter pipeline logs from other concurrent operations (e.g., `cqs watch`).
- **Suggested fix:** Add `let _span = tracing::info_span!("run_index_pipeline", files = files.len(), force, quiet).entered();` at the top of the function.

#### OB-16: `apply_windowing` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:32
- **Description:** `apply_windowing` is a `pub(crate)` function that transforms chunks before embedding. When windowing splits a chunk, there is a per-failure `tracing::warn!` (line 74), but no entry-level span or debug log showing how many chunks entered vs exited (e.g., 32 chunks in, 40 windows out). This makes it difficult to diagnose embedding dimension mismatches or performance issues caused by excessive windowing.
- **Suggested fix:** Add `tracing::debug!(input_count = chunks.len(), "applying windowing")` at entry and `tracing::debug!(input = input_count, output = result.len(), "windowing complete")` before return.

#### OB-17: `onboard_to_json` silently returns null with no logging
- **Difficulty:** easy
- **Location:** src/onboard.rs:239
- **Description:** `serde_json::to_value(result).unwrap_or_default()` returns `serde_json::Value::Null` on serialization failure with zero logging. This is also reported as EH-23, but from an observability perspective: if serialization fails, the caller receives null and the user sees empty JSON output with no indication of what went wrong. No `tracing::warn!` on the error path.
- **Suggested fix:** At minimum log the error: `serde_json::to_value(result).unwrap_or_else(|e| { tracing::warn!(error = %e, "OnboardResult serialization failed"); serde_json::Value::Null })`. Better: return `Result`.

#### OB-18: Pipeline GPU/CPU embedder threads lack thread-level spans
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:397 (GPU thread), src/cli/pipeline.rs:490 (CPU thread)
- **Description:** Both embedder threads (GPU and CPU) run in `thread::spawn` closures with no enclosing `tracing::info_span!`. Individual operations within the threads do log (e.g., GPU failure at line 470, CPU debug at line 553), but all log lines are attributed to the calling thread's span context. When both GPU and CPU threads are active simultaneously (fail_rx routing), interleaved log output has no thread-level context to distinguish GPU vs CPU processing.
- **Suggested fix:** Add `let _span = tracing::info_span!("gpu_embedder").entered();` at the top of the GPU thread closure and `let _span = tracing::info_span!("cpu_embedder").entered();` at the top of the CPU thread closure.

#### OB-19: Batch pipeline per-stage errors logged but not counted in summary
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2065, 2070
- **Description:** When a per-name dispatch fails during pipeline fan-out, the error is logged via `tracing::warn!` and added to the `errors` array. However, there is no summary tracing event at pipeline completion showing how many inputs succeeded vs failed. The final JSON envelope contains `errors` and `results` arrays, but the tracing subsystem only sees individual per-name warnings with no rollup. With PIPELINE_FAN_OUT_LIMIT at 50, a batch of failures generates 50 individual warn events with no "pipeline stage 2 completed: 48/50 succeeded" summary.
- **Suggested fix:** Add `tracing::info!(stage = stage_num + 1, succeeded = results.len(), failed = errors.len(), total = total_inputs, "Pipeline stage complete")` after the per-name dispatch loop (before the "last stage" check at line 2077).

## Code Quality

#### CQ-8: `dispatch_read_focused` duplicates `cmd_read_focused` (~140 lines)
- **Difficulty:** medium
- **Location:** src/cli/batch.rs:1528-1669, src/cli/commands/read.rs:123-263
- **Description:** `dispatch_read_focused` in batch.rs is a near-exact copy of `cmd_read_focused` in read.rs. Both share the same structure: resolve target, compute hints, format caller/test labels, inject audit state, inject notes, build target section, iterate type dependencies with N+1 `search_by_name` calls, format type sections with edge_kind labels. The only differences are: (1) batch uses `ctx.audit_state()` vs direct `load_audit_state`, (2) batch uses `ctx.notes()` vs parsing notes from file, (3) batch returns `serde_json::Value` vs printing. Same ~140 lines of logic duplicated, same N+1 type resolution pattern in both.
- **Suggested fix:** Extract a shared `build_focused_read_output(store, chunk, root, audit_mode, notes) -> (String, Option<Hints>)` function in a library module (e.g., `focused_read.rs`). Both `cmd_read_focused` and `dispatch_read_focused` call it, then format output differently (print vs JSON envelope).

#### CQ-9: `dispatch_read` duplicates `cmd_read` (~90 lines)
- **Difficulty:** medium
- **Location:** src/cli/batch.rs:1437-1526, src/cli/commands/read.rs:19-121
- **Description:** Same pattern as CQ-8 but for non-focused reads. Both have: path traversal protection, 10MB file size limit, audit mode check, notes injection with box-drawing header, content enrichment. The only differences are: batch uses `ctx.audit_state()`/`ctx.notes()` vs direct loading, batch returns JSON vs print. ~90 lines duplicated.
- **Suggested fix:** Extract shared logic into a library function, same as CQ-8.

#### CQ-10: Duplicate `parse_nonzero_usize` function
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:98, src/cli/batch.rs:169
- **Description:** Identical 4-line function defined in two files. The batch.rs copy even has a comment `// reuse logic from CLI` — but then redefines it locally instead of reusing.
- **Suggested fix:** Make the function in `cli/mod.rs` `pub(crate)` and import it in `batch.rs`. Remove the duplicate.

#### CQ-11: Duplicate CAGRA/HNSW vector index construction logic
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:134-163, src/cli/commands/query.rs:88-122
- **Description:** Both files implement the same GPU index selection logic: check chunk_count >= CAGRA_THRESHOLD (5000), attempt `CagraIndex::build_from_store`, fall back to HNSW. Same threshold constant, same fallback chain, same tracing messages. The batch version is extracted into `build_vector_index()`, but the query version has it inline.
- **Suggested fix:** Move `build_vector_index(store, cqs_dir)` to a shared location (e.g., `index.rs` or `cli/mod.rs`) and call from both batch and query.

#### CQ-12: `COMMON_TYPES` defined twice with different contents
- **Difficulty:** easy
- **Location:** src/onboard.rs:29, src/focused_read.rs:14
- **Description:** `onboard.rs` defines a local `const COMMON_TYPES: &[&str]` with 16 entries, while the canonical version in `focused_read.rs` is a `LazyLock<HashSet<&str>>` with ~40+ entries (including `Error`, `Mutex`, `RwLock`, `Cow`, `Pin`, `Future`, `Iterator`, `Display`, `Debug`, `Clone`, `Default`, etc.). The onboard version is missing roughly half the entries, so `filter_common_types` in onboard will let through types that `focused_read.rs` would filter. This is already re-exported as `cqs::COMMON_TYPES`.
- **Suggested fix:** Remove the local `COMMON_TYPES` in `onboard.rs`, import `crate::COMMON_TYPES`, change the filter to `.filter(|(name, _)| !COMMON_TYPES.contains(name.as_str()))` (same API since `HashSet` also has `.contains()`).

#### CQ-13: `dispatch_search` bypasses `BatchContext::audit_state()` cache
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:586
- **Description:** `dispatch_search` calls `cqs::audit::load_audit_state(&ctx.cqs_dir)` directly — a disk read — instead of using the cached `ctx.audit_state()` method (line 102). All other batch dispatch functions that need audit state (`dispatch_read`, `dispatch_read_focused`, `dispatch_scout`, `dispatch_health`) use the cached method. This means every batch search command re-reads the audit state file from disk.
- **Suggested fix:** Replace `let audit_mode = cqs::audit::load_audit_state(&ctx.cqs_dir);` with `let audit_mode = ctx.audit_state();` and adjust the borrow accordingly.

#### CQ-14: Batch `--tokens` parameter accepted but silently ignored in 4 commands
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:533 (dispatch_search), 729 (dispatch_explain), 888 (dispatch_gather), 491 (dispatch_onboard)
- **Description:** Four batch dispatch functions accept `--tokens` from the user but bind it to `_tokens` and drop it. Users can pass `--tokens 500` to batch search/explain/gather/onboard and the parameter is silently ignored — no warning, no token budgeting applied. The regular CLI versions of these commands do support `--tokens`. This is misleading: users think they're getting token-budgeted output in batch mode.
- **Suggested fix:** Either implement token budgeting in batch dispatch (preferred — the packing infra already exists in `cqs::token_packing`), or remove the `--tokens` parameter from these batch commands and document that batch mode doesn't support token budgeting (to avoid user confusion).

#### CQ-15: N+1 `search_by_name` in `dispatch_read_focused` type dependency loop
- **Difficulty:** medium
- **Location:** src/cli/batch.rs:1626, src/cli/commands/read.rs:228
- **Description:** Both focused read implementations iterate `filtered_types` and call `ctx.store.search_by_name(type_name, 5)` per type in a loop. With 10+ type dependencies this issues 10+ separate SQLite queries. The store already has `search_by_names_batch` which could resolve all type names in one query.
- **Suggested fix:** Collect all type names from `filtered_types`, call `store.search_by_names_batch(&type_names, 5)`, then iterate the results. This collapses N queries to 1.

## Extensibility

#### EXT-20: `suggest_notes` detector registry is hardcoded — adding a detector requires editing the function body
- **Difficulty:** easy
- **Location:** src/suggest.rs:27-59
- **Description:** `suggest_notes()` has 4 detectors inlined as numbered blocks (dead clusters, untested hotspots, high-risk, stale mentions). Adding a fifth detector (e.g., "high-churn files" or "deep call chains") requires editing the function body, adding another numbered block with identical match/append/warn boilerplate. The pattern is clear — each detector is `{ match detect_X(store) { Ok(mut s) => ..., Err(e) => ... } }` — but there is no trait, function pointer vec, or other extension point.
- **Suggested fix:** Define `type Detector = fn(&Store, &Path) -> Result<Vec<SuggestedNote>>` and store detectors in a `&[(&str, Detector)]` array. The loop becomes `for (name, detect) in DETECTORS { ... }`. New detectors are one-line additions to the array.

#### EXT-21: `is_callable_type` hardcodes Function/Method — new callable ChunkTypes require surgery
- **Difficulty:** easy
- **Location:** src/onboard.rs:309-311
- **Description:** `is_callable_type(ct: ChunkType) -> bool` matches only `Function | Method`. If a new callable chunk type is added to `ChunkType` (e.g., `Closure`, `Lambda`, `Constructor`), `onboard` will silently skip them as entry points, preferring lower-scored structs/enums. The function is private to `onboard.rs` and only called there. The canonical `ChunkType` enum lives in `src/language/mod.rs` — adding a variant there won't trigger a compile error in `onboard.rs` because the match is `matches!` (not exhaustive).
- **Suggested fix:** Add a `fn is_callable(&self) -> bool` method to `ChunkType` itself (next to the existing `Display` impl in `language/mod.rs`). This keeps the "callable" classification co-located with the type definition. Any new callable variant gets classified once, not per-consumer.

#### EXT-22: Pipeline tuning constants are local variables, not named constants
- **Difficulty:** easy
- **Location:** src/cli/pipeline.rs:229-231
- **Description:** Three pipeline tuning values (`batch_size = 32`, `file_batch_size = 5_000`, `channel_depth = 256`) are local `let` bindings inside `run_index_pipeline`. The batch_size has a history comment ("backed off from 64 - crashed at 2%") indicating it was tuned empirically. These values are not discoverable via grep for constants, and changing them requires finding the right line inside a 450-line function. The GPU CUDNN limit (`8000` at line 430) is similarly inline.
- **Suggested fix:** Extract to module-level named constants with doc comments: `const EMBEDDING_BATCH_SIZE: usize = 32;`, `const FILE_BATCH_SIZE: usize = 5_000;`, `const CHANNEL_DEPTH: usize = 256;`, `const GPU_CUDNN_MAX_CHARS: usize = 8000;`.

#### EXT-23: `health_check` hardcodes `find_hotspots(&graph, 5)` — top-N not configurable
- **Difficulty:** easy
- **Location:** src/health.rs:73
- **Description:** `health_check` always fetches the top 5 hotspots via `find_hotspots(&graph, 5)`. The `suggest_notes` function uses `find_hotspots(&graph, 20)` (line 106 of suggest.rs). Users cannot control hotspot count from the CLI. The `cqs health` command has no `--top` flag. If a user wants more or fewer hotspots in the health report, they must edit code.
- **Suggested fix:** Add a `top_hotspots: usize` parameter to `health_check()` (defaulting to 5), propagate it from a new `--top N` CLI flag on `cqs health`. Low-effort change — only 3 sites: function signature, call site, and CLI arg.

#### EXT-24: `detect_dead_clusters` threshold hardcoded to 5 dead functions per file
- **Difficulty:** easy
- **Location:** src/suggest.rs:92
- **Description:** `.filter(|(_, count)| *count >= 5)` is the threshold for flagging a file as having a "dead code cluster." This magic number controls whether a suggestion is generated. It's not configurable via CLI, config, or function parameter. A file with 4 dead functions gets no suggestion; one with 5 does. The cutoff is reasonable but arbitrary, and there is no way to adjust it for codebases where 3 dead functions per file is already concerning.
- **Suggested fix:** Accept a `min_cluster_size: usize` parameter on `suggest_notes()` or `detect_dead_clusters()`, defaulting to 5. Propagate from `cqs suggest --min-cluster N`.

#### EXT-25: `detect_risk_patterns` "untested hotspot" threshold hardcoded to caller_count >= 5
- **Difficulty:** easy
- **Location:** src/suggest.rs:121, src/health.rs:84
- **Description:** Both `suggest.rs` and `health.rs` use `caller_count >= 5 && test_count == 0` as the definition of "untested hotspot." The threshold 5 appears as a magic number in two separate files (neither references a shared constant). If the threshold definition changes, both files need updating. The health.rs copy additionally requires `risk_level == RiskLevel::High`, making the definitions inconsistent.
- **Suggested fix:** Extract `const MIN_HOTSPOT_CALLER_COUNT: usize = 5;` into a shared location (e.g., `impact/hints.rs` next to `find_hotspots`). Both `suggest.rs` and `health.rs` import and use the constant. Consider also aligning the definitions (health.rs additionally checks `RiskLevel::High`).

#### EXT-26: `PIPEABLE_COMMANDS` list requires manual update for each new batch command
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1849-1851
- **Description:** `PIPEABLE_COMMANDS` is a static list of command name strings: `["callers", "callees", "deps", "explain", "similar", "impact", "test-map", "related", "scout"]`. When a new batch command is added that accepts a function name as its first positional arg, the developer must remember to add it to this list. Nothing enforces this — the command works in batch mode but silently fails as a pipeline downstream target with a confusing error ("Cannot pipe into 'X'"). The `Drift` command, for example, is arguably pipeable (takes a reference name) but isn't listed.
- **Suggested fix:** Add a `const PIPEABLE: bool` to each `BatchCmd` variant via an attribute or method, so the "is this pipeable?" question is answered next to each command's definition rather than in a separate static list.

#### EXT-27: `extract_names` field list requires manual update for each new JSON shape
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1892-1906
- **Description:** `NAME_ARRAY_FIELDS` is a hardcoded list of 12 JSON field names that `extract_names` walks to find function names in pipeline results. When a new batch command produces a result with a differently-named array field (e.g., `"drifted"` from drift, `"types"` from deps), names won't be extracted and the pipeline will produce empty fan-out. The drift command returns `"drifted"` array with `"name"` fields, but `"drifted"` is not in `NAME_ARRAY_FIELDS`, so `search "X" | drift ref` pipelines would silently produce no names.
- **Suggested fix:** Either add `"drifted"` to the list now (it is already missing and drift is pipeable in principle), or switch to a recursive name extraction strategy that walks all arrays looking for `"name"` fields, eliminating the need for a field list entirely.

#### EXT-28: `classify_mention` heuristic tightly couples syntax patterns to classification
- **Difficulty:** easy
- **Location:** src/suggest.rs:160-168
- **Description:** `classify_mention` determines if a note mention is a file, symbol, or concept by checking for `.`/`/`/`\\` (file), `_`/`::`/PascalCase (symbol), else concept. This heuristic breaks for: (1) mentions with dots that aren't files (e.g., `crate.feature`), (2) kebab-case symbols (e.g., `my-feature`), (3) single uppercase words that are concepts not types (e.g., `CUDA`). Adding a new classification rule requires editing the function body.
- **Suggested fix:** This is acceptable as-is since it only drives staleness suggestions (not user-facing correctness), but document the known limitations. If false positives become noisy, consider allowing `MentionKind` annotations in notes.toml (e.g., `mentions = ["src/foo.rs:file", "CUDA:concept"]`).

## Robustness

#### RB-24: Drift `threshold` and `min_drift` accept NaN/Infinity without validation
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:316,319, src/cli/batch.rs:387,390
- **Description:** Both CLI and batch `drift` commands accept `f32` values for `--threshold` and `--min-drift` with no range validation. Passing `NaN` causes all `drift >= min_drift` comparisons to return `false` (NaN is not >= anything), silently dropping all results. Passing `Infinity` for `min_drift` has the same effect. A `threshold` of `NaN` passes into `semantic_diff()` which uses it for `>= threshold` similarity comparison — NaN makes all entries appear "modified" (similarity is never >= NaN). The `Similar` command's `--threshold` (mod.rs:123) and query `--threshold` (mod.rs:302) have the same issue.
- **Suggested fix:** Add `value_parser = clap::value_parser!(f32).range(0.0..=1.0)` to threshold/min_drift clap args. This rejects NaN, Infinity, and out-of-range values at parse time.

#### RB-25: `dispatch_test_map` BFS chain reconstruction lacks defensive iteration bound
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1042-1051
- **Description:** The chain reconstruction `while !current.is_empty()` relies on the `ancestors` map eventually producing an empty string predecessor (the root node at line 1011). The BFS guarantees acyclic predecessors by construction (`if !ancestors.contains_key(caller)` at line 1020), so the loop always terminates when it reaches `target_name` (line 1044 breaks) or the root. While correct today, the termination condition is indirect — a subtle refactoring error (e.g., initializing root with a non-empty predecessor) would create an infinite loop. No explicit cycle detection or iteration bound exists.
- **Suggested fix:** Add an iteration bound: `for _ in 0..max_depth + 2` instead of `while !current.is_empty()`. This defensively caps the loop even if the predecessor map is malformed.

#### RB-26: `onboard` depth unbounded in library function — CLI clamps but library accepts any usize
- **Difficulty:** easy
- **Location:** src/onboard.rs:94
- **Description:** Both CLI entry points clamp `depth.clamp(1, 5)` before calling `onboard()`, but the library function `onboard(store, embedder, concept, root, depth)` accepts any `usize`. A direct library caller could pass `depth = 1000`. The `max_expanded_nodes(100)` cap on `GatherOptions` (line 133) prevents truly unbounded growth, but BFS still explores all reachable nodes per level up to that cap. The resulting `callee_scores` HashMap is then passed to `fetch_and_assemble` which does store queries per entry.
- **Suggested fix:** Add `let depth = depth.min(10);` at the top of the `onboard()` library function, independent of CLI clamping.

#### RB-27: `get_type_graph` cap comparison uses unnecessary `as i64` cast direction
- **Difficulty:** easy
- **Location:** src/store/types.rs:411
- **Description:** `rows.len() as i64 >= MAX_TYPE_GRAPH_EDGES` converts `usize` to `i64` for comparison with an `i64` constant. While safe in practice (`MAX_TYPE_GRAPH_EDGES = 500_000`), the cast direction is unusual — `usize as i64` can overflow on 64-bit systems for values > `i64::MAX`. The comparison could be simplified.
- **Suggested fix:** Change `MAX_TYPE_GRAPH_EDGES` to `usize` and compare directly: `if rows.len() >= MAX_TYPE_GRAPH_EDGES`. Or cast the constant: `rows.len() >= MAX_TYPE_GRAPH_EDGES as usize`.

#### RB-28: `name_boost` and `note_weight` accept `f32` without validation — negative values invert scoring
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:126-127 (name_boost), src/cli/mod.rs:130 (note_weight)
- **Description:** `--name-boost` and `--note-weight` are documented as `0.0-1.0` in the help text but accept any `f32`. A negative `--name-boost` would subtract from matching scores. A `--note-weight` of `-1.0` would invert note sentiment (warnings boost, patterns penalize). NaN would cause undefined sorting behavior.
- **Suggested fix:** Add `value_parser = clap::value_parser!(f32).range(0.0..=1.0)` to both args, matching the documented range.

#### RB-29: `search_by_name` limit parameter not clamped inside the function
- **Difficulty:** easy
- **Location:** src/store/chunks.rs (search_by_name function)
- **Description:** `search_by_name(name, limit)` passes `limit` directly as a SQL `LIMIT` clause. In batch mode, `dispatch_search` clamps to 1..100 (batch.rs:538), but the function itself doesn't enforce an upper bound. All current callers use small constants (1-10), but a future caller passing a user-controlled value without clamping could request millions of rows.
- **Suggested fix:** Add `let limit = limit.min(1000);` inside `search_by_name` as defense-in-depth.

#### RB-30: `dispatch_trace` BFS doesn't early-exit when target is enqueued
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1141-1146
- **Description:** The BFS trace only checks for target match when dequeuing (line 1124). When the target is enqueued as a callee (line 1144), BFS continues exploring all other callees at the current depth before dequeuing and finding the target. For a function with 100 callees where target is the first one, 99 unnecessary callees are enqueued and explored. With `max_depth = 50`, this adds up.
- **Suggested fix:** Add an early exit when target is found during callee enumeration: `if *callee == target_name { visited.insert(callee.clone(), current.clone()); break 'outer; }`. This can dramatically reduce BFS expansion for shallow paths.

## Algorithm Correctness

#### AC-22: `extract_file_from_chunk_id` window detection never fires — windowed chunks misparse
- **Difficulty:** medium
- **Location:** src/search.rs:199-228
- **Description:** The window detection heuristic checks if the last segment of a chunk ID is a short all-digit string (line 210-213: `last_seg.len() <= 2 && last_seg.bytes().all(|b| b.is_ascii_digit())`). However, windowed chunk IDs use the format `"path:line:hash:wN"` (pipeline.rs:51: `format!("{}:w{}", parent_id, window_idx)`). The `w` prefix means the last segment is `"w0"`, `"w1"`, etc. — never pure digits. So `segments_to_strip` is always 2 (standard), never 3 (windowed). For a windowed ID like `"src/foo.rs:10:abc12345:w0"`, the function strips only `:abc12345` and `:w0`, returning `"src/foo.rs:10"` instead of `"src/foo.rs"`. This causes: (1) glob filter `src/**/*.rs` to reject the chunk (path doesn't end with `.rs`), (2) note boosting via `path_matches_mention` to miss matches. Windowed chunks are effectively invisible to glob filtering and note boosting in `search_filtered`.
- **Suggested fix:** Either change the window ID format from `:wN` to `:N` (breaking change) or update the detection to match the actual format: `!last_seg.is_empty() && last_seg.starts_with('w') && last_seg[1..].bytes().all(|b| b.is_ascii_digit())`. The latter preserves compatibility. Alternatively, strip any segment starting with `w` followed by digits.

#### AC-23: `onboard` summary `total_items` excludes `key_types` count
- **Difficulty:** easy
- **Location:** src/onboard.rs:212
- **Description:** `total_items: 1 + call_chain.len() + callers.len() + tests.len()` counts entry_point + callees + callers + tests but excludes `key_types.len()`. The `OnboardSummary` is serialized to JSON and consumed by the CLI display. A user reading `total_items: 7` while seeing 7 code entries + 3 type dependencies gets a count that doesn't match the displayed list. The `files_covered` field similarly only counts code entry files, not type definition files. This is a display inconsistency rather than a logic error, but `total_items` claiming to count items while missing an entire section is misleading.
- **Suggested fix:** Either add `key_types.len()` to the sum: `total_items: 1 + call_chain.len() + callers.len() + key_types.len() + tests.len()`, or rename to `total_code_items` to clarify what is counted.

#### AC-24: `onboard` COMMON_TYPES diverges from canonical `focused_read::COMMON_TYPES`
- **Difficulty:** easy
- **Location:** src/onboard.rs:29-33
- **Description:** Already noted in CQ-12, but adding the correctness perspective: `onboard.rs` defines a local `const COMMON_TYPES: &[&str]` with 16 entries and uses it in `filter_common_types`. The canonical version in `focused_read.rs` is a `LazyLock<HashSet<&str>>` with 44 entries, including `Error`, `Mutex`, `RwLock`, `Cow`, `Pin`, `Future`, `Iterator`, `Display`, `Debug`, `Clone`, `Default`, `Send`, `Sync`, `Serialize`, `Deserialize`, etc. The `onboard` filter passes through types that `focused_read` would filter. For example, `Error` (among the most commonly referenced types in Rust) appears in onboard's `key_types` output but is filtered in `read --focus` output. This inconsistency means the same function's type dependencies differ depending on which command you use.
- **Suggested fix:** Remove the local `COMMON_TYPES` in `onboard.rs` and import `crate::COMMON_TYPES`. Change the filter to `.filter(|(name, _)| !COMMON_TYPES.contains(name.as_str()))`.

#### AC-25: `note_boost` takes first match per note instead of best match — mentions order affects result
- **Difficulty:** easy
- **Location:** src/search.rs:266-280
- **Description:** The inner loop iterates `note.mentions` and `break`s on the first match (line 278). This means if a note has `mentions = ["lib.rs", "my_fn"]` and both the file path and chunk name match, only the first mention (`lib.rs`) triggers the match. This is correct for determining *whether* the note matches (it does), but the `break` skips checking if later mentions also match. In practice, this doesn't affect the sentiment value chosen (it's per-note, not per-mention), so the result is correct. However, the `break` comment says "This note already matched, check next note" which is exactly right. **Verdict: Not a bug.** The `break` is an intentional optimization. Removing this finding upon verification.

#### AC-26: `extract_file_from_chunk_id` fails for paths containing only digits in segment
- **Difficulty:** easy
- **Location:** src/search.rs:210-217
- **Description:** A standard chunk ID for a file at path `"42:1:abc12345"` (e.g., a file literally named `42`) would have `last_seg = "abc12345"`, which is 8 hex chars — not all digits if it contains a-f. But a file named `"99"` with chunk ID `"99:1:00000000"` would have: last_seg = `"00000000"` (8 chars, len > 2, so not matched as window). This is fine. But consider a chunk at line 1 in file `"1"`: ID = `"1:1:abc12345"`. Here last_seg = `"abc12345"`, not digits, strips 2 → `"1"`. Correct. Now consider a numeric-only hash prefix: `"src/foo.rs:1:12345678"`. Last segment `"12345678"` has len 8, > 2, not matched as window. Correct. The heuristic is safe for standard IDs because hash prefixes are always 8 chars. **Verdict: Not a bug for standard IDs.** The only issue is with windowed IDs (AC-22).

#### AC-27: Pipeline `stage_num` offset causes confusing stage numbering in logs
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2042
- **Description:** In the pipeline loop, `stage_num = stage_idx + 1` (line 2022, 1-indexed for display), then the tracing span uses `stage = stage_num + 1` (line 2042), making it 2-indexed. A 3-stage pipeline (A | B | C) logs: stage 0 = "Pipeline stage 0" (from line 1993), then B = "Pipeline stage 3" (stage_idx=0, stage_num=1, log=1+1=2... wait, let me re-read). Actually: `segments[1..]` iterates with `enumerate()`, so `stage_idx` = 0 for the first downstream stage (B). `stage_num = stage_idx + 1 = 1`. Tracing span: `stage = stage_num + 1 = 2`. So stages log as: A=stage 0 (line 1993 uses `stage = 0`), B=stage 2, C=stage 3. Stage 1 is skipped in log output. This is a display bug, not a logic bug, but confusing when debugging pipelines.
- **Suggested fix:** Use consistent 1-based numbering: change line 2042 to `stage = stage_num` (B=stage 1, C=stage 2) and line 1993 `stage = 0` to `stage = 1`. Or keep 0-based throughout.

#### AC-28: `diff` modified sort uses `unwrap_or(0.0)` for missing similarity — conflates "unknown" with "maximally changed"
- **Difficulty:** easy
- **Location:** src/diff.rs:173-178
- **Description:** The sort comparator for `modified` entries uses `.unwrap_or(0.0)` when similarity is `None`. A `similarity: None` means embeddings couldn't be compared (one or both missing). `0.0` similarity means "completely different." By treating unknown similarity as 0.0, these entries sort to the top of the "most changed" list alongside genuinely maximally-drifted functions. In `detect_drift` (drift.rs:72), `similarity: None` becomes `drift = 1.0 - 0.0 = 1.0` (maximum drift). Functions with missing embeddings appear as the most drifted, displacing genuinely changed functions from the top of the output.
- **Suggested fix:** Sort entries with `None` similarity separately (after all entries with known similarity), or use a sentinel like `-1.0` so they sort to the end. In `detect_drift`, filter out entries with `similarity: None` or flag them distinctly (e.g., `drift: None` or `drift_unknown: true`).

#### AC-29: `BoundedScoreHeap` capacity 0 accepted — infinite insertion without eviction
- **Difficulty:** easy
- **Location:** src/search.rs:318-319
- **Description:** `BoundedScoreHeap::new(0)` creates a heap with capacity 0. The `push` method checks `self.heap.len() < self.capacity` — which is `0 < 0 = false`, so it goes to the "at capacity" branch. `self.heap.peek()` returns `None` (empty heap), so nothing is inserted. This means a capacity-0 heap correctly rejects all inserts. Not a bug per se, but `BoundedScoreHeap::new(0)` silently becomes a /dev/null. The only caller is `search_filtered` which passes `semantic_limit = limit * 3` (or `limit` if not RRF). If `limit = 0`, `search_unified_with_index` already returns early (line 759), so capacity 0 shouldn't be reachable in practice. **Verdict: Not a bug** due to the early return guard.

#### AC-30: `pick_entry_point` ModifyTarget search doesn't consider across-group score comparisons
- **Difficulty:** easy
- **Location:** src/onboard.rs:252-271
- **Description:** The ModifyTarget search iterates all file groups and within each group checks chunks with `ChunkRole::ModifyTarget`. When a callable ModifyTarget is found, it updates `best_modify_callable` only if the new score is strictly greater. But file groups are sorted by relevance (highest first), so the first group's chunks are generally more relevant. If two different groups each have a callable ModifyTarget with the same score, the later one wins (last-write). This is unlikely to cause practical issues since score ties are rare, but the search doesn't leverage the group ordering (which would naturally prefer earlier groups). **Verdict: Low impact** — strict `>` comparison means earlier entries are kept on ties, which actually favors earlier (more relevant) groups.

#### AC-31: `pipeline` intermediate stage always merges ALL extracted names, ignoring fan-out cap
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2090-2098
- **Description:** In intermediate pipeline stages, the code merges names from ALL per-name dispatch results into `merged_names`. But each dispatch result could itself return many names (up to 50 from a search), and there could be up to 50 dispatches (from the fan-out cap). So an intermediate merge could produce up to 50 * 50 = 2500 names. These merged names are then passed as `current_value` to the next iteration, where `extract_names` extracts them and the fan-out cap of 50 is applied. So the cap IS applied in the next iteration — but the intermediate `merged_names` Vec can be large (up to 2500 strings) without any cap. This is bounded memory (2500 short strings) and the downstream fan-out cap prevents runaway dispatches, so it's not a correctness bug. **Verdict: Design acceptable** — the fan-out cap is applied per-stage on dispatch, not on name collection.

## Platform Behavior

#### PB-14: `onboard_to_json` serializes PathBuf fields without backslash normalization
- **Difficulty:** easy
- **Location:** src/onboard.rs:239, src/onboard.rs:51, src/onboard.rs:72
- **Description:** `onboard_to_json()` uses `serde_json::to_value(result)` to serialize the entire `OnboardResult` struct, which contains `OnboardEntry.file: PathBuf` and `TestEntry.file: PathBuf`. On Windows (or WSL with native Windows paths), `PathBuf` serialization preserves platform path separators — backslashes on Windows. Every other JSON-producing command in the codebase manually constructs JSON with `.to_string_lossy().replace('\\', '/')` to ensure consistent forward-slash paths (e.g., `gather` at `commands/gather.rs:131`, `search` at `batch.rs:544`, `explain` at `commands/explain.rs:160`). The `onboard` command is the only one that derives `Serialize` on structs containing `PathBuf` fields and serializes them directly. In practice this is safe on WSL (Linux paths are always forward-slash) and stored paths from SQLite are already normalized. But if a `PathBuf` is ever constructed from a Windows-native path (e.g., `entry_file` from `pick_entry_point` which comes from `ScoutResult.file_groups[].file`), the JSON output would contain backslashes.
- **Suggested fix:** Either (a) add `#[serde(serialize_with = "serialize_path_forward_slash")]` to the `file` fields in `OnboardEntry` and `TestEntry`, or (b) change `file` fields from `PathBuf` to `String` and normalize at construction time (matching `DriftEntry.file: String` which already does this).

#### PB-15: `dispatch_context` constructs absolute path for origin lookup — always fails first query
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1268-1273, src/cli/commands/context.rs:24-29
- **Description:** Both `dispatch_context` (batch) and `cmd_context` (CLI) do `let origin = abs_path.to_string_lossy().to_string()` (an absolute path like `/mnt/c/Projects/cq/src/foo.rs`) and then call `store.get_chunks_by_origin(&origin)`. But origins in SQLite are stored as relative forward-slash paths (e.g., `src/foo.rs`) — the pipeline normalizes them at `pipeline.rs:288`. The absolute-path lookup always returns 0 results, falling through to the relative-path fallback on the next line. This is harmless (the fallback works) but it means every `cqs context` and batch `context` command issues a wasted SQLite query. On WSL with `/mnt/c/` paths, `to_string_lossy()` produces a Linux-style absolute path, which never matches a relative stored origin. This pattern was pre-existing (not new since v0.12.3) but is duplicated in the new batch.rs code.
- **Suggested fix:** Remove the absolute path attempt. Query directly with the relative `path` argument, which matches stored origins. Or normalize: `let origin = path.replace('\\', "/")` and query with that.

## Test Coverage

#### TC-11: `onboard()` has zero integration test — only helper unit tests
- **Difficulty:** medium
- **Location:** src/onboard.rs:89
- **Description:** The main `onboard()` function has zero test coverage (`cqs test-map onboard` returns 0 tests). The 11 unit tests in `onboard.rs` only test internal helpers (`pick_entry_point`, `filter_common_types`, `test_info_to_entry`, `callee_ordering_by_depth`). None exercise the full pipeline: scout -> pick entry -> BFS callees -> BFS callers -> fetch types -> find tests -> assemble. This means the integration between these stages is untested — wrong wiring, incorrect BFS options, or mismatched data flow between stages would not be caught.
- **Suggested fix:** Add an integration test in `tests/` that creates a small project with call relationships, indexes it, then calls `onboard()` and verifies: (1) entry_point is set, (2) call_chain is non-empty and ordered by depth, (3) callers are found, (4) tests are detected. Requires embedding, so either use the real embedder or extract the core logic into a testable path that accepts pre-embedded data.

#### TC-12: `health_check()` only tested with empty store
- **Difficulty:** easy
- **Location:** src/health.rs:140
- **Description:** The single test `test_health_check_empty_store` verifies the empty case only — all counters zero, no warnings. This doesn't exercise any of the 5 sub-queries (staleness, dead code, hotspots, untested hotspots, notes). Since `health_check` degrades gracefully per sub-query (each has its own `match` with a warning fallback), the test doesn't verify that the degradation paths produce correct warnings or that successful sub-queries produce correct non-zero values.
- **Suggested fix:** Add a test with a populated store: insert chunks with call relationships, verify `hotspots` is non-empty and `dead_confident > 0`. Add a test that verifies the degradation warning path (e.g., force a sub-query failure and check `warnings` vec).

#### TC-13: `suggest_notes()` only tested with empty store — detectors never produce output
- **Difficulty:** medium
- **Location:** src/suggest.rs:285
- **Description:** `test_suggest_empty_store` only verifies `suggest_notes` returns empty on an empty store. None of the three detectors (`detect_dead_clusters`, `detect_risk_patterns`, `detect_stale_mentions`) are tested with data that would trigger a suggestion. `detect_dead_clusters` requires 5+ dead functions in one file. `detect_risk_patterns` requires hotspots with 5+ callers and 0 tests. The stale mention test (`test_detect_stale_file_mention`) tests the helper directly but not through `suggest_notes`. The deduplication logic (lines 62-73) is also untested.
- **Suggested fix:** Add tests with populated stores: (1) insert 5+ uncalled functions in one file, verify dead cluster suggestion appears; (2) insert a note with a mention matching an existing function, add the function, verify no stale suggestion; (3) add a suggestion that matches an existing note text, verify deduplication filters it.

#### TC-14: `detect_drift()` tested only with empty stores — no actual drift detection
- **Difficulty:** medium
- **Location:** src/drift.rs:123
- **Description:** `test_drift_empty_stores` only verifies the empty case. The three other tests (`test_drift_entry_fields`, `test_drift_sort_order`, `test_drift_min_filter`) construct `DriftEntry` structs manually and test properties on them, never calling `detect_drift()`. The actual function — which delegates to `semantic_diff()` and filters/sorts the results — is only tested via the empty path. No test verifies that `detect_drift` with two stores containing the same function at different embeddings produces a drift entry with correct similarity/drift values.
- **Suggested fix:** Add a test that inserts the same function name into two stores with different embeddings, calls `detect_drift`, and verifies: (1) `drifted` is non-empty, (2) `similarity` is between 0 and 1, (3) `drift = 1.0 - similarity`, (4) `min_drift` filtering works correctly on actual data.

#### TC-15: `apply_windowing()` has zero test coverage
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:32
- **Description:** `apply_windowing` transforms chunks before embedding — splitting long chunks into overlapping windows. It has zero tests (`cqs test-map apply_windowing` returns 0). The function handles three code paths: (1) chunk fits in one window (pass-through), (2) chunk splits into multiple windows, (3) tokenization failure (pass-through with warning). None are tested. The windowing constants test (`test_windowing_constants`) only checks compile-time bounds on the constants, not the function behavior.
- **Suggested fix:** Create a chunk with content longer than `MAX_TOKENS_PER_WINDOW` tokens, call `apply_windowing`, verify: (1) output has more chunks than input, (2) window chunks have `parent_id` set, (3) window chunks have sequential `window_idx`, (4) first window preserves doc, subsequent windows have `None`. Also test the pass-through case with a short chunk.

#### TC-16: `TypeEdgeKind::from_str()` round-trip untested
- **Difficulty:** easy
- **Location:** src/parser/types.rs:122
- **Description:** `TypeEdgeKind` has `Display` and `FromStr` impls (6 variants each) with zero unit tests. The `FromStr` error case (unknown string) is also untested. While the store tests exercise the types indirectly through `upsert_type_edges`/`get_types_used_by`, the serialization round-trip (`as_str()` -> `from_str()`) is not directly verified. A typo in either the `Display` or `FromStr` match arms would silently break edge kind round-tripping through the database.
- **Suggested fix:** Add a `#[cfg(test)] mod tests` to `parser/types.rs` with: (1) round-trip test for each variant (`TypeEdgeKind::Param.to_string().parse::<TypeEdgeKind>() == Ok(TypeEdgeKind::Param)`), (2) error case test (`"invalid".parse::<TypeEdgeKind>().is_err()`).

#### TC-17: `warn_stale_results()` test discards return value — no assertion
- **Difficulty:** easy
- **Location:** src/cli/staleness.rs:74
- **Description:** `test_warn_stale_results_nonexistent_origins` calls `warn_stale_results` with origins not in the index, but assigns the result to `let _ = result;` with the comment "Key: it must not panic." This is a non-assertion — it verifies the function doesn't crash, but doesn't verify the return value. The test should at least assert something about the returned `HashSet<String>`.
- **Suggested fix:** Replace `let _ = result;` with `assert!(result.is_empty(), "Origins not in index should produce empty stale set");` (since the store has no chunks, nothing can be stale).

#### TC-18: No CLI integration tests for `cqs drift`, `cqs onboard`, `cqs health`, `cqs suggest`, `cqs deps`
- **Difficulty:** medium
- **Location:** tests/ (missing files)
- **Description:** Five commands added since v0.12.3 have zero CLI integration tests. All other major commands (`query`, `callers`, `callees`, `explain`, `gather`, `impact`, `stale`, `batch`) have integration tests in `tests/cli_commands_test.rs` or dedicated test files. The missing commands are: `drift` (requires reference setup), `onboard` (requires embedder), `health`, `suggest`, and `deps` (requires type edges). Batch mode also lacks integration tests for the `health`, `stale`, `drift`, `onboard`, and `deps` dispatch paths.
- **Suggested fix:** Add integration tests for at least `health --json`, `suggest --json`, and `deps --json` (these three don't require embedding and can work with a basic indexed project). `health` and `suggest` should return valid JSON with expected fields. `deps` in both forward and reverse mode should return correct results when type edges are present. `onboard` and `drift` are harder (require embedder/references) but at minimum a smoke test that verifies exit code 0 and valid JSON output would catch wiring regressions.

#### TC-19: Batch mode `--tokens` silently ignored — no test catches the gap
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:533, 729, 888, 491
- **Description:** Related to CQ-14 (already reported in Code Quality), but from a test coverage perspective: there are zero tests that verify `--tokens` behavior in batch mode for `search`, `explain`, `gather`, or `onboard`. If/when token budgeting is implemented in batch, there will be no regression tests. Currently the parameter is accepted and silently dropped (`_tokens`), and no test verifies either that it works or that it is intentionally unsupported.
- **Suggested fix:** If `--tokens` should work in batch: implement it and add tests. If intentionally unsupported: add a test that verifies a warning or error is emitted when `--tokens` is passed in batch mode.

#### TC-20: `onboard.rs` tests use isolated helpers but never test full assembly
- **Difficulty:** easy
- **Location:** src/onboard.rs:622-645
- **Description:** `test_entry_point_excluded_from_call_chain` tests `HashMap::remove()` behavior, not the actual onboard logic. It creates a scores map, removes "entry", and checks the map has 2 items. This is testing standard library behavior, not project code. Similarly, `test_callee_ordering_by_depth` tests `Vec::sort_by` with custom comparator — the sort is inlined in `onboard()` (line 161-166), so the test only validates the comparator in isolation, not that the sort is applied correctly to `callee_chunks`.
- **Suggested fix:** Remove the HashMap test (tests std library). The sort test is marginally useful but could be strengthened by testing through a higher-level function that applies the sort, rather than reimplementing it in the test.

## Performance

#### PERF-20: `dispatch_trace` N+1 `search_by_name` for path enrichment
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1154-1167
- **Description:** After BFS finds the shortest call path, `dispatch_trace` iterates each node in the path and calls `ctx.store.search_by_name(name, 1)` per node to fetch file/line/signature metadata. For a depth-10 path, this issues 10 separate SQLite FTS queries. The store already provides `search_by_names_batch` which resolves all names in a single query. The CLI `cmd_trace` at `src/cli/commands/trace.rs` has the same N+1 pattern.
- **Suggested fix:** Collect all path names into a `Vec<&str>`, call `store.search_by_names_batch(&names, 1)`, then look up each name from the returned `HashMap` when building `path_json`.

#### PERF-21: `dispatch_search` bypasses `BatchContext::audit_state()` cache — re-reads disk
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:586
- **Description:** `dispatch_search` calls `cqs::audit::load_audit_state(&ctx.cqs_dir)` directly instead of using the cached `ctx.audit_state()` method. Every batch search command re-reads the audit state file from disk. All other dispatch functions (`dispatch_read`, `dispatch_read_focused`, `dispatch_health`) use the cached method. This was already noted in CQ-13 (Code Quality) but belongs here as a performance finding: in a batch session with 100 search commands, this issues 100 unnecessary file reads.
- **Suggested fix:** Replace `let audit_mode = cqs::audit::load_audit_state(&ctx.cqs_dir);` with `let audit_mode = ctx.audit_state();`.

#### PERF-22: `get_call_graph()` not cached in BatchContext — reloaded per command
- **Difficulty:** medium
- **Location:** src/cli/batch.rs:1005, 1114
- **Description:** `dispatch_test_map` and `dispatch_trace` both call `ctx.store.get_call_graph()?` which scans the entire `function_calls` table and builds in-memory `HashMap<String, Vec<String>>` adjacency lists. This is a full table scan (O(calls)) per invocation. In a batch pipeline like `search "error" | test-map`, the fan-out dispatches `test-map` up to 50 times — each loading the full call graph from SQLite. The call graph is immutable during a batch session (no indexing occurs). The `onboard` library function also loads both the call graph and test chunks (lines 116-124), issuing 2 queries per invocation.
- **Suggested fix:** Add a `call_graph: OnceLock<CallGraph>` field to `BatchContext`, similar to the existing `hnsw` and `file_set` caches. Load on first use, return `&CallGraph` thereafter. Same pattern for `test_chunks: OnceLock<Vec<TestChunk>>`.

#### PERF-23: `list_notes_summaries()` called redundantly in `search_filtered` and `search_by_candidate_ids`
- **Difficulty:** easy
- **Location:** src/search.rs:390, 644
- **Description:** Both `search_filtered` and `search_by_candidate_ids` call `self.list_notes_summaries()` at the top to load notes for boosting. When `search_filtered_with_index` falls back to brute-force (line 609), it calls `search_filtered` which loads notes again. More critically, `search_unified_with_index` (line 806) calls `search_by_candidate_ids` or `search_filtered`, each of which loads notes independently. Notes are loaded from SQLite, parsed, and allocated fresh each time. In a batch session, notes don't change — they're loaded once per search command but discarded between calls.
- **Suggested fix:** Accept notes as an optional parameter: `search_filtered(..., notes: Option<&[NoteSummary]>)`. When `None`, load from store (backward-compatible). The caller `search_unified_with_index` loads notes once and passes them down. In batch mode, `BatchContext` already caches notes via `notes_cache: OnceLock` — pass those through.

#### PERF-24: `strip_markdown_noise` chains 8 regex/string replacements creating 8 intermediate Strings
- **Difficulty:** medium
- **Location:** src/nl.rs:472-503
- **Description:** `strip_markdown_noise` performs 8 sequential string transformations, each creating a new `String` via `.to_string()` or `.replace()`. For a 3000-char markdown chunk (not unusual), this allocates and copies ~24KB of intermediate strings. The function is called once per markdown chunk during indexing via `generate_nl_description`. With hundreds of markdown sections in a documentation index, the allocation overhead adds up. The regex replacements use `replace_all(...).to_string()` which always allocates, even when no match is found (`Cow::Borrowed` is immediately converted to owned).
- **Suggested fix:** Use `Cow`-aware chaining: keep the result as `Cow<str>` and only convert to `String` at the end. Each `replace_all` already returns `Cow<str>` — chain them without `.to_string()` until the final result. The `replace('*', "")` calls can be batched into a single char-filtering pass. This halves allocations for content with no matches (the common case for most transformations).

#### PERF-25: `dispatch_context` abs_path lookup always fails — wasted SQLite query
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1268-1273
- **Description:** Already noted in PB-15 (Platform Behavior), but quantifying the performance impact: `dispatch_context` constructs an absolute path (`/mnt/c/Projects/cq/src/foo.rs`) and calls `store.get_chunks_by_origin(&origin)`, which always returns empty because origins are stored as relative paths (`src/foo.rs`). The function then falls through to a second query with the relative path. Every `cqs context` and batch `context` command issues a wasted SQLite query that scans the origin index and returns 0 rows. In a pipeline like `search "query" | context`, 50 wasted queries are issued.
- **Suggested fix:** Remove the absolute path attempt. Query directly with the relative `path` argument: `let chunks = ctx.store.get_chunks_by_origin(path)?;`.

#### PERF-26: `suggest_notes` deduplication uses O(n*m) substring matching
- **Difficulty:** easy
- **Location:** src/suggest.rs:68-73
- **Description:** The deduplication loop checks each suggestion against every existing note text with bidirectional `contains()`. With S suggestions and N existing notes, this is O(S*N) string comparisons. For each comparison, `contains()` is O(len_a * len_b) worst case. In practice, S is typically <20 and N <100, so this is bounded — but the pattern scales poorly if note counts grow. More importantly, the substring check `s.text.contains(existing_text)` will false-positive on short existing note texts (e.g., a note "CUDA" would match any suggestion containing "CUDA" as a substring, even if unrelated).
- **Suggested fix:** For the current scale, the O(n*m) is acceptable. Consider using exact text match or normalized prefix match instead of bidirectional `contains()` to reduce false positives. If note counts grow beyond ~500, switch to a `HashSet<&str>` for exact-text dedup and skip substring matching.

#### PERF-27: `dispatch_drift` opens reference store without caching — not reused across pipeline stages
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1750-1770
- **Description:** `dispatch_drift` loads the config, finds the reference config, and opens the reference store (`cqs::Store::open(&ref_db)`) on every call. Unlike `get_ref()` (which caches `ReferenceIndex` in `BatchContext::refs`), the drift path opens a raw `Store` each time. In a pipeline like `search "query" | drift ref-name`, each of the 50 fan-out dispatches opens a separate SQLite connection to the reference database. `Store::open` creates a new tokio runtime + sqlx connection pool (~5-10ms per open).
- **Suggested fix:** Either cache the reference `Store` in `BatchContext` (alongside the existing `ReferenceIndex` cache), or use the `ReferenceIndex` path which already has `store` accessible. Since `detect_drift` takes `&Store` directly, add a `ref_stores: RefCell<HashMap<String, Store>>` field to `BatchContext`.

#### PERF-28: `onboard` calls `scout()` with `limit=10` which internally runs full search + hints computation
- **Difficulty:** easy
- **Location:** src/onboard.rs:99
- **Description:** `onboard` calls `scout(store, embedder, concept, root, 10)` which runs a full search, then computes hints (callers + tests) for each result, and groups by file. The `onboard` function only uses the result to pick an entry point name+file (via `pick_entry_point`). It doesn't use the hints, caller counts, test counts, or staleness information computed by scout. Scout internally calls `compute_hints` which issues 2 SQLite queries per chunk (caller count + test count), plus `check_origins_stale` for staleness. For 10 results, this is ~20+ unnecessary SQLite queries.
- **Suggested fix:** Either (a) use a lightweight search instead of `scout` — `store.search_filtered()` returns `SearchResult` with file/name/score, enough for `pick_entry_point`, or (b) add a `scout_lite` variant that skips hints/staleness computation. The entry point picker only needs: chunk name, file, chunk_type, role classification, and score.

## Data Safety

#### DS-13: Type edges upserted outside chunk transaction — crash leaves inconsistent state
- **Difficulty:** medium
- **Location:** src/cli/commands/index.rs:196, src/cli/watch.rs:403
- **Description:** The indexing pipeline (`run_index_pipeline`) writes chunks + calls in a single transaction via `upsert_chunks_and_calls`. Type edges are extracted and upserted in a completely separate pass (`extract_relationships` in index.rs, or the loop in watch.rs) *after* the pipeline completes. If the process crashes between pipeline completion and relationship extraction, chunks exist in the DB with stale or missing type edges. The next `cqs index` run sees mtime is current and skips reindexing, leaving type edges permanently stale until `--force`. The `INSERT OR REPLACE` in `upsert_chunks_and_calls` triggers `ON DELETE CASCADE`, removing old type edges for replaced chunks — so after the pipeline commits but before `extract_relationships` runs, the DB has chunks with *no* type edges. Queries like `get_types_used_by` return empty during this window and permanently if the process crashes before the second pass.
- **Suggested fix:** Either (1) incorporate type edge extraction into the pipeline's writer stage so chunks + calls + type edges are a single transaction, or (2) track type-edge staleness separately (e.g., a per-origin metadata flag) so stale type edges are re-extracted on the next run without `--force`. Option (2) is simpler since it doesn't require the pipeline to carry type refs through the embedding stages.

#### DS-14: `upsert_type_edges_for_file` reads chunk IDs outside the transaction — TOCTOU race
- **Difficulty:** medium
- **Location:** src/store/types.rs:117-123, 158
- **Description:** `upsert_type_edges_for_file` queries chunk IDs from the `chunks` table (line 118-123) using `&self.pool` (auto-commit read), then later opens a transaction (line 158) for the DELETE + INSERT. Between the read and the transaction, a concurrent process (`cqs watch` or another `cqs index`) could delete or replace the chunks, causing the resolved IDs to be stale. The INSERT would then reference a `source_chunk_id` that no longer exists, causing an FK violation error. Even under WAL mode, the read and the later transaction are separate snapshots — WAL provides snapshot isolation per-statement or per-transaction, not across separate operations.
- **Suggested fix:** Move the chunk ID resolution query inside the transaction: begin `tx` before the `SELECT id, name, line_start, window_idx FROM chunks ...` query, and use `&mut *tx` instead of `&self.pool`. This provides consistent read + write within a single snapshot.

#### DS-15: Batch `notes_cache` and `audit_state` never invalidated during session
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:108-123, 102-105
- **Description:** `BatchContext::notes()` and `audit_state()` cache via `OnceLock` — parsed once on first access, never refreshed. If a user modifies `docs/notes.toml` or toggles audit mode in another terminal while a batch session is active, the batch session uses stale data for its entire lifetime. This affects: (1) `dispatch_notes` returning stale note lists, (2) note injection in `dispatch_read`/`dispatch_read_focused`, (3) audit mode detection controlling whether notes are shown in search results. The batch session can run indefinitely (it reads from stdin until EOF/quit), so staleness could accumulate significantly.
- **Suggested fix:** Document that batch sessions use snapshot-at-start for notes/audit state and changes require restarting the batch session. This is acceptable since batch is designed as a short-lived stdin pipe, not a long-running daemon. Alternatively, add an mtime check on each access and re-parse if the file changed.

#### DS-16: `upsert_type_edges_for_file` deletes type edges for ALL file chunks, not just those being updated
- **Difficulty:** easy
- **Location:** src/store/types.rs:160-176
- **Description:** The DELETE at line 160-176 removes type edges for ALL resolved chunk IDs in the file (`name_to_id.values()`), not just those in `chunk_type_refs`. If `chunk_type_refs` contains only a subset of the file's chunks (e.g., a partial re-extraction), type edges for omitted chunks are deleted and never re-inserted. Both current callers (`index.rs` and `watch.rs`) use `parse_file_relationships` which returns all chunks, so this is safe today. But the function's contract is implicit — a future caller passing partial type refs would silently lose type edges for omitted chunks with no error or warning.
- **Suggested fix:** Either (1) change the DELETE to only target chunk IDs that appear in the `edges` vec rather than all resolved IDs, or (2) add a doc comment explicitly stating the contract: "`chunk_type_refs` must contain ALL chunks in the file — partial updates will delete type edges for omitted chunks."

#### DS-17: `BatchContext::refs` uses `RefCell` — not `Sync`, blocks future parallelization of fan-out
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:36
- **Description:** `refs: RefCell<HashMap<String, ReferenceIndex>>` is `!Sync`. `BatchContext` is currently single-threaded (batch stdin loop), so this is safe today. However, all other cached fields use `OnceLock` (which is `Sync`), making `RefCell` the odd one out. If pipeline fan-out (line 2056-2073) were ever parallelized with rayon, concurrent borrows of `RefCell` would panic at runtime. The struct also can't be wrapped in `Arc` for sharing — the compiler would reject it due to `RefCell: !Sync`.
- **Suggested fix:** Replace `RefCell<HashMap<String, ReferenceIndex>>` with `std::sync::RwLock<HashMap<String, ReferenceIndex>>`. The performance cost of RwLock vs RefCell is negligible for a reference cache accessed at most once per `--ref` command.

#### DS-18: Window priority in `upsert_type_edges_for_file` depends on undefined row order
- **Difficulty:** easy
- **Location:** src/store/types.rs:118-134
- **Description:** The chunk ID resolution query (line 118) has no `ORDER BY`. The HashMap insertion loop (line 128-134) sets `is_primary = window_idx.is_none() || *window_idx == Some(0)` and inserts when `is_primary || !name_to_id.contains_key(&key)`. For a chunk that has both windowed (window_idx=0) and non-windowed (window_idx=NULL) rows, the correct ID to use is the non-windowed row (it's the logical parent). But without `ORDER BY`, SQLite may return rows in any order. If the non-windowed row appears first, the windowed row (also `is_primary`) overwrites it with a different chunk ID. In practice, the non-windowed row usually doesn't coexist with windowed rows (windowing replaces the parent), but if index repair or partial re-indexing creates both, the wrong ID gets selected nondeterministically.
- **Suggested fix:** Add `ORDER BY window_idx ASC NULLS LAST` to the query so non-windowed chunks (NULL) are processed last and win the HashMap insertion. Alternatively, in the loop, prefer NULL over `Some(0)`: only overwrite if the incoming row has `window_idx.is_none()` and the existing entry was `Some(0)`.

#### DS-19: `get_embeddings_by_hashes` returns partial results silently — callers can't detect store errors
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:656-661
- **Description:** When a batch SQL query fails inside `get_embeddings_by_hashes`, the error is logged at `warn` and the function `continue`s to the next batch, returning whatever was fetched so far. The return type `HashMap<String, Embedding>` (no `Result`) makes it impossible for the caller to distinguish "embedding not cached" from "cache lookup failed due to DB error." The sole caller (`prepare_for_embedding` in pipeline.rs:139) treats missing entries as "needs new embedding," which wastes GPU/CPU time re-embedding chunks that may already be cached. More importantly, a persistent DB error (corruption, locking) causes silent re-embedding of every batch without any escalation.
- **Suggested fix:** Change return type to `Result<HashMap<String, Embedding>, StoreError>`. The pipeline caller can then decide to warn-and-continue (preserving current degraded behavior) while having the option to detect and escalate persistent failures.

## Security

#### SEC-12: Batch stdin has no line length limit — unbounded memory allocation via single line
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2168
- **Description:** `stdin.lock().lines()` reads each line fully into memory with no upper bound on line length. Rust's `BufReader::lines()` will attempt to allocate memory for the entire line. A malicious or accidental input (e.g., a 4GB line piped into `cqs batch`) will cause unbounded memory allocation, potentially OOM-killing the process. While `cqs batch` is primarily consumed by Claude Code agents (not untrusted users), the tool does accept arbitrary stdin. The pipeline fan-out has a `PIPELINE_FAN_OUT_LIMIT` of 50 to prevent dispatch explosion, but there is no equivalent guard on raw input line size. Other commands have size guards: `dispatch_read` has a 10MB file limit, `rewrite_notes_file` has a 10MB guard, `Config::load_file` has a 1MB guard.
- **Suggested fix:** Add a line length check after reading: `if line.len() > MAX_BATCH_LINE_LEN { emit error JSON and continue; }` after the `Ok(l)` match arm, with `const MAX_BATCH_LINE_LEN: usize = 1_048_576;` (1MB).

#### SEC-13: Drift command opens reference store read-write instead of read-only
- **Difficulty:** easy
- **Location:** src/cli/commands/drift.rs:46, src/cli/batch.rs:1770
- **Description:** Both drift implementations use `Store::open(&ref_db)` which creates a connection with `?mode=rwc` (read-write-create). The drift command only reads from both stores — it never writes. Using `mode=rwc` means: (1) if the reference path points to a non-existent location, an empty SQLite database is silently created there, and (2) the reference store is opened with write access unnecessarily. The `ReferenceConfig.path` comes from `.cqs.toml` which is user-editable. While `ref add` safely constructs paths via `ref_path()`, a hand-edited config could contain any path. Combined with `mode=rwc`, this could create empty databases at unexpected locations. `Store::open_readonly()` exists and is already used for reference stores in `load_references()`.
- **Suggested fix:** Change `Store::open(&ref_db)` to `Store::open_readonly(&ref_db)` in both `src/cli/commands/drift.rs:46` and `src/cli/batch.rs:1770`. This follows the principle of least privilege and matches the `load_references()` pattern.

#### SEC-14: Batch `--limit` not clamped on Similar, Gather, Scout, Related — allows resource amplification
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:248, 264, 320, 355
- **Description:** The `--limit` parameter on batch `Search` is clamped to `1..100` at dispatch time (line 571), but other commands accept unclamped limits: `Similar` (default 5, no max), `Gather` (default 10, no max), `Related` (default 5, no max), `Scout` (default 10, no max). A user could pass `similar func --limit 999999` in batch mode, causing large allocations and many SQLite queries. In a pipeline, fan-out of 50 dispatches each requesting huge results amplifies memory usage. The `Gather` command's `expand` is also unclamped in batch (default 1, no max enforced at parse time), though the library caps `max_expanded_nodes` at 100.
- **Suggested fix:** Add `.clamp(1, 100)` at dispatch time for `dispatch_similar`, `dispatch_gather`, `dispatch_related`, and `dispatch_scout`, matching the pattern in `dispatch_search` (line 571). For `expand`, add `.clamp(0, 5)` at dispatch time.

#### SEC-15: `dispatch_read` TOCTOU between canonicalize check and file read
- **Difficulty:** hard
- **Location:** src/cli/batch.rs:1447-1471, src/cli/commands/read.rs:27-52
- **Description:** Both `dispatch_read` and `cmd_read` have a TOCTOU (time-of-check/time-of-use) window: (1) `file_path.exists()`, (2) `dunce::canonicalize` resolves the real path, (3) `canonical.starts_with(&project_canonical)` validates containment, (4) `read_to_string(&file_path)` reads the file. Between steps 3 and 4, any directory component in `file_path` could be replaced with a symlink pointing outside the project. The `canonicalize` check resolves at check time, but `read_to_string` resolves again at read time — if the target changes between these calls, the read could access files outside the project root. Practical exploitation requires local filesystem access and precise timing (microsecond window). Risk is low for a single-user dev tool.
- **Suggested fix:** Read via the canonical path instead of the original: change `read_to_string(&file_path)` to `read_to_string(&canonical)`. This eliminates the TOCTOU by using the already-resolved path for the actual read.

## Resource Management

#### RM-16: `get_ref` loads ALL reference stores to find one — drops the rest
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:76-78
- **Description:** `BatchContext::get_ref(name)` calls `load_references(&config.references)` which opens a Store (SQLite pool + tokio runtime) and loads an HNSW index for **every** configured reference, then iterates to find the one matching `name` and discards the rest. Each unwanted `ReferenceIndex` holds a `Store` with a SQLite connection pool plus an optional HNSW index loaded from disk. With 3 configured references, requesting "ref-A" opens all 3 databases (~100ms each), loads 3 HNSW files, then drops 2 stores and 2 indexes immediately. The cache in `self.refs` only prevents re-loading already-seen names — each new reference name triggers another load-all-and-filter.
- **Suggested fix:** Add a `load_reference_by_name(configs, name)` function that filters `configs` to the single matching entry before calling `Store::open_readonly` + `HnswIndex::try_load`. Alternatively, change `get_ref` to iterate configs, find the matching one, and open only that reference.

#### RM-17: `dispatch_drift` opens fresh Store per call — bypasses BatchContext reference cache
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:1770
- **Description:** `dispatch_drift` calls `cqs::Store::open(&ref_db)?` directly for each drift command, creating a new Store (SQLite pool + tokio runtime) per invocation. In batch mode, running `drift ref1` 10 times opens 10 separate database connections. `BatchContext` already caches reference stores via `get_ref()`/`borrow_ref()` — but `dispatch_drift` bypasses this entirely. Each `Store::open` creates a new tokio runtime + sqlx connection pool (~5-10ms overhead). Also loads `Config` from disk per call (line 1750). (Overlaps with PERF-27 — same root cause.)
- **Suggested fix:** Use `ctx.get_ref(reference)?` and `ctx.borrow_ref(reference)` to get the cached reference store, then pass `&ref_idx.store` to `detect_drift()`. This is exactly the pattern `dispatch_gather` uses (lines 908-909).

#### RM-18: Reranker model not cached in BatchContext — ~91MB ONNX session re-created per reranked search
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:623
- **Description:** Each `dispatch_search` with `--rerank` calls `cqs::Reranker::new()` which creates a new Reranker struct. On first `rerank()` call, this loads the cross-encoder ONNX session (~91MB) and tokenizer. Since a new Reranker is created per search, the lazy initialization provides no benefit — the model is loaded, used once, and dropped. 10 reranked searches in batch = 10 ONNX session loads (~200ms each) and ~91MB peak memory allocated and freed repeatedly, fragmenting the heap.
- **Suggested fix:** Add a `reranker: OnceLock<cqs::Reranker>` to `BatchContext`, initialized on first `--rerank` use. All subsequent reranked searches reuse the same instance.

#### RM-19: `semantic_diff` loads all chunk identities + all matched embeddings with no size cap
- **Difficulty:** medium
- **Location:** src/diff.rs:76-77, 138-139
- **Description:** `semantic_diff()` calls `all_chunk_identities_filtered()` on both stores (loading all rows), then batch-fetches embeddings for all matched pairs. For cqs (~1800 chunks), peak is ~6MB. But `all_chunk_identities_filtered` has no row limit, unlike `get_call_graph` and `get_type_graph` which cap at 500K. A reference store indexing a large monorepo (50K+ chunks) would load 50K identity structs plus 50K embeddings (769 x 4 bytes = ~3KB each = ~150MB) simultaneously. Two such stores = ~300MB peak. No streaming or chunked comparison.
- **Suggested fix:** Add a `MAX_DIFF_CHUNKS` cap (e.g., 100K) to `all_chunk_identities_filtered` with a warning when the cap is hit. Matches the precedent from `get_call_graph` (500K) and `get_type_graph` (500K).

#### RM-20: Pipeline intermediate merge collects unbounded names before downstream fan-out cap
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:2090-2098
- **Description:** In intermediate pipeline stages, `merged_names` collects names from all per-name dispatch results without a cap. Each of 50 dispatches can return results with many names. The downstream `PIPELINE_FAN_OUT_LIMIT` (50) is applied at the next stage's dispatch, not during collection. For `search | callers | test-map`, the callers stage produces up to 50 x N callers. All are inserted into `merged_names` Vec and `merged_seen` HashSet, then only 50 survive to the next stage. Memory is small (short strings) but the work is wasted.
- **Suggested fix:** Cap `merged_names` at `PIPELINE_FAN_OUT_LIMIT` during collection. Add early break: `if merged_names.len() >= PIPELINE_FAN_OUT_LIMIT { break; }` in the merge loop.

#### RM-21: `Config::load` called per batch `get_ref` and `dispatch_drift` — redundant disk I/O per command
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:76 (get_ref), src/cli/batch.rs:1750 (dispatch_drift)
- **Description:** Both `get_ref` and `dispatch_drift` call `cqs::config::Config::load(&self.root)` per invocation, reading and parsing the config file from disk each time. In batch mode with multiple reference commands, the config is re-read per command.
- **Suggested fix:** Add `config: OnceLock<cqs::config::Config>` to `BatchContext`. Load once on first use.

#### RM-22: Batch REPL holds CAGRA GPU index for entire session — GPU memory never released
- **Difficulty:** medium
- **Location:** src/cli/batch.rs:35, 134-163
- **Description:** `BatchContext::hnsw` is `OnceLock<Option<Box<dyn VectorIndex>>>`. When a CAGRA GPU index is built (5000+ chunks), it allocates GPU memory proportional to vector count (5K x 769 x 4 = ~15MB on GPU). For 50K+ chunks, GPU memory reaches 100MB+. The HNSW fallback holds an in-memory graph. Neither is released until the batch process exits. `OnceLock` cannot be reset. No mechanism exists for users to release GPU memory during long sessions.
- **Suggested fix:** Low priority — documented design. If needed, replace `OnceLock` with `Mutex<Option<...>>` and add a `drop-cache` batch command. Document memory expectations in `cqs batch --help`.

#### RM-23: `dispatch_search` audit mode loaded from disk — bypasses `ctx.audit_state()` cache
- **Difficulty:** easy
- **Location:** src/cli/batch.rs:586
- **Description:** Already reported as CQ-13 (Code Quality). `dispatch_search` reads audit state from disk per call instead of using the cached `ctx.audit_state()`. Duplicate — included for resource management completeness.
- **Suggested fix:** Replace with `ctx.audit_state()`.

#### RM-24: `onboard` allocates full content for all callees + callers — high memory for deep graphs
- **Difficulty:** medium
- **Location:** src/onboard.rs:159, 167-177
- **Description:** `fetch_and_assemble` fetches full `GatheredChunk` structs with `content` for every callee (up to 100 via `max_expanded_nodes`) and every caller (up to 50). Each function body is then cloned into `OnboardEntry.content` via `gathered_to_onboard`. Peak: ~200 function content strings in memory, ~200KB-1MB for a single `onboard` call. In batch fan-out, 50 concurrent onboard calls could hold up to 50MB of content. Currently bounded since batch is single-threaded, but worth noting.
- **Suggested fix:** No immediate fix needed — bounded and manageable. If parallelized, consider a content-free intermediate representation or streaming assembly.
