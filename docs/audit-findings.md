# Audit Findings — v1.15.1 (post-schema-migration)

Audit date: 2026-04-02
Scope: Multiple categories — edge-case/sad-path test coverage gaps, robustness (unwrap/expect/panic paths).

## Adversarial Test Coverage

#### TC-6: `token_pack` silent empty return on zero budget contradicts documented guarantee
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:361
- **Description:** `token_pack` documents "always includes at least one item" but silently returns empty when `budget = 0` with non-empty input. The 10x-cap logic at line 386 (`if tokens > budget * 10`) evaluates to `tokens > 0` for any positive token count, so every item is skipped and `kept_any` never becomes true. No test exercises `token_pack` with `budget = 0` and non-empty items. Compare with `index_pack` at line 428 which explicitly documents and guards `budget == 0` as a valid "zero allocation" case.
- **Suggested fix:** Add test: `token_pack(vec!["a"], &[10], 0, 0, |_| 1.0)` and decide the intended behavior — either return empty (matching `index_pack`) or include one item (matching the doc comment). Update the doc comment to match whichever is chosen.

#### TC-7: `sanitize_json_floats` and `write_json_line` have zero tests
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs:433, 459
- **Description:** `sanitize_json_floats` recursively replaces NaN/Infinity with null in `serde_json::Value` trees. `write_json_line` uses it as a fallback when direct serialization fails. Neither function has a single test. The comment on `write_json_line` says "NaN/Infinity in the value — sanitize and retry" but there is no test verifying the retry path, verifying that nested NaN values in arrays/objects are sanitized, or verifying that the error branch writes `{"error":"JSON serialization failed"}`. The batch mode loop calls `write_json_line` on every result — this is a hot path with no regression coverage.
- **Suggested fix:** Add unit tests for: (1) `sanitize_json_floats` with NaN in nested object, (2) NaN in nested array, (3) `write_json_line` with a value containing NaN triggers the retry path, (4) verify the output is valid JSON after retry.

#### TC-8: L5X `<STContent>` block with no CDATA sections — no test
- **Difficulty:** easy
- **Location:** src/parser/l5x.rs:251
- **Description:** `extract_l5x_regions` skips `<STContent>` blocks where no `<![CDATA[...]]>` sections are found (lines 251-253). The existing test `test_l5x_no_st_content` tests a file with no `<STContent>` tag at all. There is no test for a file that has `<STContent></STContent>` (tag present, content empty) or `<STContent><Comment>foo</Comment></STContent>` (tag present but only non-CDATA children). Real Logix exports occasionally emit empty STContent blocks for placeholder routines.
- **Suggested fix:** Add test with `<Routine Type="ST"><STContent></STContent></Routine>` — verify `extract_l5x_regions` returns empty (no panic, no corrupt region).

#### TC-9: L5K ST routine with ST type check ambiguity — no malformed block test
- **Difficulty:** easy
- **Location:** src/parser/l5x.rs:340-350
- **Description:** `extract_l5k_regions` determines ST type by scanning the first 5 lines for `TYPE`, `:=`, and `ST` all present on the same line. No test exercises an L5K routine whose type declaration is on line 6 or later (would be silently skipped), a routine where the word "ST" appears in a comment on line 3 but the actual type is RLL (false positive), or a ROUTINE block with no `END_ROUTINE` terminator (regex `(?msi)^\s*ROUTINE ... END_ROUTINE` would just fail to match — no test verifies the behavior with truncated input).
- **Suggested fix:** Add tests for: (1) ST type declaration appearing after line 5 — verify the routine is skipped, (2) "ST" appearing in a comment with RLL type — verify false-positive rate, (3) truncated input without `END_ROUTINE` — verify empty result, no panic.

#### TC-10: L5X with malformed CDATA (unclosed `<![CDATA[`) — no test
- **Difficulty:** easy
- **Location:** src/parser/l5x.rs:231
- **Description:** The `CDATA_RE` regex is `<!\[CDATA\[(.*?)]]>` using non-greedy matching. If source contains `<![CDATA[` with no closing `]]>` the regex simply produces no match (no panic). But if two CDATA blocks appear with the first unclosed, the regex may greedily consume content from both blocks into a single match. No test covers this malformed-CDATA edge case. Large L5X files from older RSLogix exports sometimes have encoding issues that produce partial CDATA blocks.
- **Suggested fix:** Add test: `<STContent><![CDATA[good_code;]]> garbage <![CDATA[other_code;]]></STContent>` — verify only the well-formed blocks are extracted without corrupting content.

#### TC-11: `CommandContext::embedder` init failure path — untested
- **Difficulty:** medium
- **Location:** src/cli/store.rs:96
- **Description:** `CommandContext::embedder()` uses a `OnceLock<cqs::Embedder>`. If `Embedder::new()` fails (e.g., ONNX model file missing or corrupt), the method returns `Err(...)` and the `OnceLock` stays empty. A subsequent call will attempt `Embedder::new()` again — correct behavior but untested. More critically, there is no test for what happens when a command that calls `ctx.embedder()?` fails at the init step — the `?` propagates the error up, but the caller (e.g., `cmd_query`) may have already done partial work (printed headers, opened the store). The batch context has the same pattern in `src/cli/batch/mod.rs:223`. Zero tests simulate an embedder init failure.
- **Suggested fix:** Add a test using a `ModelConfig` pointing to a nonexistent ONNX path that verifies `embedder()` returns `Err` (not panic) and that calling it again returns `Err` again (not a cached bad state).

#### TC-12: `token_pack` with NaN scores — sort order undefined
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:372
- **Description:** `token_pack` sorts items by `score_fn` using `total_cmp`, which places NaN after all other values. If a caller passes a score function that returns NaN (e.g., a corrupt search result), items with NaN scores are sorted last and then skipped once `kept_any` is true. The behavior is deterministic (NaN items are deprioritized) but undocumented and untested. Unlike `BoundedScoreHeap` (which was hardened in PR #744 per TC-1), `token_pack` has no explicit NaN guard and no NaN test.
- **Suggested fix:** Add test: items with NaN scores mixed with valid-scored items — verify NaN-scored items are excluded from output when budget fits only the valid items. Add a doc comment noting NaN behavior.

#### TC-13: `NeighborEntry::similarity` can serialize NaN — not guarded
- **Difficulty:** easy
- **Location:** src/cli/commands/search/neighbors.rs:14, 101
- **Description:** `NeighborEntry` has `similarity: f32` which serializes directly. The `dot()` function at line 68 returns `f32` via `sum()`. If any embedding vector contains NaN or Inf values (which `Embedding::try_new` rejects but raw store data might contain due to index corruption), `dot()` returns NaN, and `serde_json::to_string_pretty` will panic on NaN f32. The existing test `neighbor_entry_serializes` only tests `similarity: 0.95`. Unlike the batch handler which uses `write_json_line` + `sanitize_json_floats`, `cmd_neighbors` calls `serde_json::to_string_pretty` directly with no NaN guard.
- **Suggested fix:** Add test asserting `serde_json::to_value` of `NeighborEntry { similarity: f32::NAN }` either panics (documenting the gap) or sanitizes. Then add a guard (e.g., replace NaN with null or 0.0) in `build_neighbors_output`.

#### TC-14: `build_explain_output` with special characters in `name`/`signature` fields — no test
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/explain.rs:277
- **Description:** The existing `ExplainOutput` serialization tests use only ASCII names like `"foo"` and `"bar"`. No test covers: (1) function names with Unicode (valid in Rust with `#[allow(non_ascii_idents)]`), (2) signatures containing `<` and `>` (generic types like `Vec<T>`), (3) doc comments containing literal quotes or backslashes. While `serde_json` handles these correctly by design, the test gap means a regression (e.g., double-escaping, truncation) would go undetected. The `SimilarEntry.score: f32` field also has no test for boundary values like 0.0 or 1.0.
- **Suggested fix:** Add tests with `name: "parse<T>"`, `signature: "fn foo<T: Debug>(x: &T) -> Vec<T>"`, and `doc: Some("returns \"best\" result")`. Verify round-trip through `serde_json::to_value` / `from_value`.

#### TC-15: `scout`/`onboard` commands have zero unit tests
- **Difficulty:** medium
- **Location:** src/cli/commands/search/scout.rs, src/cli/commands/search/onboard.rs
- **Description:** `cmd_scout` (151 lines) and `cmd_onboard` (229 lines) have no `#[cfg(test)]` blocks at all. Both use `inject_content_into_scout_json` and `inject_token_info` as shared helpers, and both call `scout_to_json` / `onboard_to_json` from the lib crate. The JSON output shapes for scout and onboard are completely untested at the CLI layer — field names, token info injection, content map injection, and the empty-result branch. These are high-traffic commands (used by agents constantly per MEMORY.md).
- **Suggested fix:** Add at minimum: (1) a test for `inject_content_into_scout_json` with a known JSON shape, (2) a test for empty `content_map` leaving JSON unchanged, (3) a test for `inject_token_info` on a mutable JSON value.

#### TC-16: `DiffEntryOutput::similarity: Option<f32>` not tested for NaN
- **Difficulty:** easy
- **Location:** src/cli/commands/io/diff.rs:22
- **Description:** `DiffEntryOutput` has `similarity: Option<f32>` with `#[serde(skip_serializing_if = "Option::is_none")]`. The existing test covers `None` (omitted) and `Some(0.85)` (present). No test covers `Some(f32::NAN)` — if semantic_diff ever produces a NaN similarity (e.g., identical-hash chunks with zero-norm embeddings), `serde_json::to_string_pretty` will panic at the `cmd_diff` call site. Unlike other commands that use `write_json_line`, `cmd_diff` uses direct `serde_json::to_string_pretty` with no sanitization.
- **Suggested fix:** Add test asserting `serde_json::to_value(DiffEntryOutput { similarity: Some(f32::NAN), ... })` panics (documenting the gap), then fix by filtering NaN to None before constructing the output entry.

## Robustness

#### RB-7: `print_telemetry_text` panics on multi-byte UTF-8 query strings
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:421
- **Description:** `print_telemetry_text` truncates long queries for display with `&tq.query[..47]`. This is a raw byte-slice on a `String`, which panics with `byte index N is not a char boundary` if a multi-byte UTF-8 character straddles the 47-byte cutoff. The check `tq.query.len() > 50` compares byte length (not char count), so a query of ~17 Chinese characters (~51 bytes) would pass the check and attempt byte index 47, which may land mid-character. The rest of the codebase uniformly uses `floor_char_boundary` for this purpose (e.g., `src/llm/prompts.rs:9`, `src/suggest.rs:251`, `src/cli/commands/search/scout.rs:119`). The telemetry display function is the only production site that byte-slices a user-provided string without this guard.
- **Suggested fix:** Replace `&tq.query[..47]` with `&tq.query[..tq.query.floor_char_boundary(47)]`.

#### RB-8: `Cli::model_config()` panics if called before `resolve_model()`
- **Difficulty:** medium
- **Location:** src/cli/definitions.rs:254
- **Description:** `Cli::model_config()` calls `.expect("ModelConfig not resolved — call resolve_model() first")` on `self.resolved_model`. The dispatch code (via `CommandContext::open_readonly`) calls this method after store open. If a future refactor or new code path opens `CommandContext` without going through the main dispatch (e.g., a test helper or a new CLI entry point), the panic fires. The comment says "All 20+ callers are in the dispatch path" — this is a temporal coupling invariant enforced only by convention, not the type system. The `Option<ModelConfig>` returning `None` would be a better API, but at minimum the panic message could include a stack trace hint. This is the same issue as the old RB-4 finding.
- **Suggested fix:** Convert `model_config()` to return `Option<&ModelConfig>` and let callers handle `None`, or enforce via a newtype wrapper that can only be constructed in dispatch.

#### RB-9: `count_sessions` returns 1 for entries containing only Reset events (no Commands)
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:124
- **Description:** `count_sessions` initializes `sessions = 1` and increments for each Reset event and each 4-hour gap. If `entries` contains only Reset events and no Command events (e.g., the user ran `cqs telemetry reset` multiple times but issued no commands), the function returns `1 + number_of_resets` even though there are zero actual sessions with activity. `build_telemetry` calls `count_sessions(entries)` on the full `entries` slice (including Resets) after confirming `commands` is non-empty — but if there are Reset events mixed in before any commands, those Resets inflate the session count. Example: two resets followed by one search would report 3 sessions instead of 1.
- **Suggested fix:** Only count sessions from Command entries (or treat a Reset as starting a new session only when followed by at least one Command). The current behavior over-counts sessions when reset events appear before the first command.

## Error Handling

#### EH-7: `is_hnsw_dirty()` silently treats DB errors as "not dirty"
- **Difficulty:** easy
- **Location:** src/cli/store.rs:170
- **Description:** `store.is_hnsw_dirty().unwrap_or(false)` converts a DB query failure into `false`, causing the code to proceed with loading the HNSW index even when it cannot confirm whether the dirty flag is set. The dirty flag is the crash-safety invariant from RT-DATA-6 — if the query itself fails, silently defaulting to "not dirty" is the wrong bias since it may load a stale index written by an interrupted indexing run.
- **Suggested fix:** Use `unwrap_or_else(|e| { tracing::warn!(error = %e, "Failed to read HNSW dirty flag, assuming dirty to be safe"); true })`. Defaulting to `true` (treat as dirty) falls back to brute-force search rather than risking a corrupt index. Alternatively, propagate via `?` since `build_vector_index_with_config` already returns `Result`.

#### EH-8: Six `*_to_json` functions fall back to `json!({})` on serialization error — silent output corruption
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:87,278,449; src/cli/commands/io/blame.rs:209; src/cli/commands/search/related.rs:67; src/cli/commands/search/where_cmd.rs:88
- **Description:** `compact_to_json`, `full_to_json`, `summary_to_json`, `blame_to_json`, `related_result_to_json`, and `where_to_json` each return `serde_json::Value`. On serialization failure they fall back to `json!({})` with a tracing warn. Callers (CLI and batch handlers) receive an empty object and print/return it as valid output. In the CLI path, `serde_json::to_string_pretty(&output).context(...)` in `cmd_blame` gives false confidence — it only covers the `to_string_pretty` step (which succeeds for `{}`), not the upstream serialization failure. In the batch path, agents receive `{}` with no `"error"` field and no way to distinguish it from a valid empty result.
- **Suggested fix:** Change these functions to return `Result<serde_json::Value, serde_json::Error>`. At batch handler call sites that must return `serde_json::Value`, use `json!({"error": "serialization failed"})` instead of `{}` so agents can detect the failure. These types use `#[derive(Serialize)]` on structs with only `String` and primitive fields — `serde_json::to_value` will only fail if a float is NaN/Inf, which should be caught upstream.

#### EH-9: `store.chunk_count()` error silently bypasses GPU index path
- **Difficulty:** easy
- **Location:** src/cli/store.rs:145-148
- **Description:** In the `#[cfg(feature = "gpu-index")]` block, `store.chunk_count().unwrap_or_else(|e| { tracing::warn!(...); 0 })` returns 0 on DB error. This causes the CAGRA threshold check (`0 < 5000`) to fail, silently skipping GPU indexing. The user gets HNSW/brute-force search without any explanation beyond a tracing warn. Since `build_vector_index_with_config` returns `Result<...>`, the error can be propagated.
- **Suggested fix:** Replace with `store.chunk_count().context("Failed to read chunk count for vector index selection")?`. The CAGRA path is not critical (HNSW is the fallback), but the error should surface rather than silently downgrading performance.

#### EH-10: `build_brief_data` silently returns partial data when store queries fail
- **Difficulty:** easy
- **Location:** src/cli/commands/io/brief.rs:59-75
- **Description:** `get_caller_counts_batch`, `get_call_graph`, and `find_test_chunks` each fall back to empty data on error. `build_brief_data` returns `BriefData` with caller counts and test counts zeroed, with no flag indicating degraded data. The `cqs brief` terminal output shows `callers: 0` and `tests: 0` for every function, indistinguishable from a codebase with genuinely no callers or tests. The tracing warn is invisible to the user.
- **Suggested fix:** Acceptable soft-fallback pattern for a display command. Add a visible warning in non-JSON mode when any query fails: `eprintln!("Warning: partial data (store query failed: {})", e)`. For JSON mode, add a `"warnings"` field so agents can detect degraded state.

#### EH-11: `build_full_data` partial external caller/callee data not surfaced to caller
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:120-131
- **Description:** `get_callers_full_batch` and `get_callees_full_batch` fall back to empty `HashMap` on error. `build_full_data` returns `FullData` with empty external caller/callee maps, and callers (`cmd_context`, batch `dispatch_context`) present this as complete data. A user analyzing a file's external dependencies sees 0 callers / 0 callees with no warning.
- **Suggested fix:** Add a `data_degraded: bool` field to `FullData`, or add a `"warning"` key in JSON output when fallbacks fire. Batch handler at `src/cli/batch/handlers/info.rs:165` should propagate the degradation signal.

#### EH-12: `parse_notes` failures in `cmd_read`/`cmd_read_focused` are invisible to users
- **Difficulty:** easy
- **Location:** src/cli/commands/io/read.rs:313, 352
- **Description:** Both `cmd_read` and `cmd_read_focused` call `parse_notes(...).unwrap_or_else(|e| { tracing::warn!(...); vec![] })`. If notes.toml cannot be parsed, note annotations are silently omitted from `cqs read` output. The output appears complete but lacks the `[note]` injections that guide agents. The tracing warn is only visible in debug-level logs. Agents relying on note annotations receive incomplete context without any signal.
- **Suggested fix:** In non-JSON mode, emit `eprintln!("Warning: could not parse notes.toml: {}", e)`. In JSON mode, add `"warnings": ["notes unavailable"]` to the output struct. This matches the pattern already used by `cmd_index` at line 403.

## Observability

#### OB-1: `parser_stage` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/pipeline/parsing.rs:28
- **Description:** `parser_stage` is Stage 1 of the 3-stage index pipeline. It processes file batches in parallel (rayon), filters by staleness, and sends parsed chunks to the embedder channel. It has no `info_span!` entry point, so there is no trace event to mark when this stage starts or ends. The function has per-batch `tracing::info!` and per-file `tracing::warn!` but no outer span to group them or record total file count.
- **Suggested fix:** Add `let _span = tracing::info_span!("parser_stage", file_count = files.len()).entered();` at the top of `parser_stage`.

#### OB-2: `store_stage` missing tracing span and unlogged `upsert_chunks_and_calls` failures
- **Difficulty:** easy
- **Location:** src/cli/pipeline/upsert.rs:19 (span), :51 and :64 (error paths)
- **Description:** `store_stage` is Stage 3 of the index pipeline — it writes all embedded chunks to SQLite, stores function calls, and inserts type edges. It runs for the entire duration of indexing and has no `info_span!`. The two calls to `upsert_chunks_and_calls` at lines 51 and 64 propagate failures via `?` with no `tracing::warn!` before propagation. When this fails the calling thread exits with an anyhow error with no trace context about which batch or file was being processed. Peer error paths (function calls, chunk calls, type edges) all have `warn!` before recovery, making the chunk upsert path the odd one out.
- **Suggested fix:** Add `let _span = tracing::info_span!("store_stage").entered();` at the top. Add `tracing::warn!(batch_size = batch.chunk_embeddings.len(), error = %e, "Failed to upsert chunks batch")` before the `?` at lines 51 and 64.

#### OB-3: `prepare_for_embedding` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/pipeline/embedding.rs:24
- **Description:** `prepare_for_embedding` is the shared preparation logic called from both `gpu_embed_stage` and `cpu_embed_stage`. It performs windowing (step 1, has its own span), content-hash cache lookup (step 2), batch split into cached/to_embed (step 3), and NL description generation (step 4). No outer span covers steps 2-4. Cache lookup failures are recovered with a `warn!` but there is no span attribute to see batch size or cache-hit rate per call.
- **Suggested fix:** Add `let _span = tracing::info_span!("prepare_for_embedding", batch_size = batch.chunks.len()).entered();` at the top.

#### OB-4: `build_explain_data` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/explain.rs:42
- **Description:** `build_explain_data` resolves a target, fetches callers and callees, performs a similarity search, and computes impact hints. Called from both `cmd_explain` (which has a span) and batch handlers. Individual error paths within it have `tracing::warn!` but there is no outer span, making it impossible to distinguish explain data-gathering time from output rendering time in traces.
- **Suggested fix:** Add `let _span = tracing::info_span!("build_explain_data", target).entered();` at the top.

#### OB-5: `build_vector_index_with_config` missing tracing span
- **Difficulty:** easy
- **Location:** src/cli/store.rs:133
- **Description:** `build_vector_index_with_config` selects among CAGRA (GPU), HNSW (CPU), and brute-force — a routing decision with significant performance impact. It has `tracing::warn!` and `tracing::info!` for individual branches but no outer span to record which path was taken and how long the overall index-selection took. Called from both CLI commands and batch mode.
- **Suggested fix:** Add `let _span = tracing::info_span!("build_vector_index", ?ef_search).entered();` at the top.

#### OB-6: `check_schema_version` missing tracing span
- **Difficulty:** easy
- **Location:** src/store/metadata.rs:23
- **Description:** `check_schema_version` reads schema version from the DB and may trigger a migration (calling `migrations::migrate`). Migration is a significant operation with schema-altering SQL. The function has no outer span — a slow or failed migration is invisible in traces. The only evidence is a `tracing::info!` on success or a propagated error on failure.
- **Suggested fix:** Add `let _span = tracing::info_span!("check_schema_version", path = %path.display()).entered();` at the top.

#### OB-7: `cpu_embed_stage` embedding failure propagates without `warn!`
- **Difficulty:** easy
- **Location:** src/cli/pipeline/embedding.rs:315
- **Description:** In `cpu_embed_stage`, `emb.embed_documents(&text_refs)?` propagates errors directly with no `tracing::warn!`. In contrast, `gpu_embed_stage` explicitly logs GPU failures with the failing chunk list before falling back to CPU. CPU failures have no fallback — the error bubbles up and terminates the CPU thread. There is no context in the trace about which files or batch was being processed.
- **Suggested fix:** Replace `emb.embed_documents(&text_refs)?` with a `match` that calls `tracing::warn!(error = %e, chunks = prepared.to_embed.len(), "CPU embedding failed")` before returning the error.

#### OB-8: `submit_fresh` in `BatchRunner` missing tracing span
- **Difficulty:** easy
- **Location:** src/llm/batch.rs:653
- **Description:** `submit_fresh` submits a new Claude Batches API request and stores the pending batch ID. It has `tracing::info!` and `tracing::error!` calls but no outer span, so the total time for batch submission is not captured. The peer functions `resume` (line 576) and `submit_or_resume` (line 456) both have `info_span!`. `submit_fresh` is the asymmetric gap.
- **Suggested fix:** Add `let _span = tracing::info_span!("submit_fresh", count = batch_items.len(), purpose = self.purpose).entered();` at the top.

#### OB-9: `build_compact_data` and `build_full_data` missing tracing spans
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:25 and :105
- **Description:** Both functions perform multiple DB queries (chunks, caller counts, callee counts, external callers/callees) to assemble data for `cmd context`. They are `pub(crate)` and called from both the CLI command handler and batch mode handlers. Neither function has its own span — their DB work is invisible in traces. Errors propagate via `.context(...)` without any `tracing::warn!`.
- **Suggested fix:** Add `let _span = tracing::info_span!("build_compact_data", path).entered();` to `build_compact_data` and `let _span = tracing::info_span!("build_full_data", path).entered();` to `build_full_data`.

## Documentation

#### DOC-11: README "Supported Languages" section heading still says (51)
- **Difficulty:** easy
- **Location:** README.md:503
- **Description:** The `<summary>` heading reads `Supported Languages (51)` but there are 52 language files (excluding mod.rs) in `src/language/`. DOC-1 was marked fixed in PR #737 and further addressed in PR #759, but both PRs only updated the TL;DR line and the "How It Works" section. The HTML details toggle heading was missed in both passes. The body of the section already lists 52 languages correctly, making the count mismatch obvious.
- **Suggested fix:** Change `<summary><h2>Supported Languages (51)</h2></summary>` to `<summary><h2>Supported Languages (52)</h2></summary>`.

#### DOC-12: CONTRIBUTING.md Architecture Overview: `commands/` shown as flat file list
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:108-109
- **Description:** The architecture tree shows `commands/` with a flat comma-separated list of 50+ `.rs` files. The actual structure is seven subdirectories: `graph/`, `index/`, `infra/`, `io/`, `review/`, `search/`, `train/`, each with a `mod.rs` and grouped command files. A developer following the "Adding a New CLI Command" guide (line 266) would place their file at the wrong path.
- **Suggested fix:** Replace the flat listing with the subdirectory structure showing each group and its contents, matching the actual layout at `src/cli/commands/`.

#### DOC-13: CONTRIBUTING.md: `cli/pipeline.rs` listed as a flat file; it is now a directory
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:121, 123
- **Description:** Line 123 lists `pipeline.rs - Multi-threaded indexing pipeline` as a flat file. It has been refactored into `src/cli/pipeline/` with submodules `embedding.rs`, `mod.rs`, `parsing.rs`, `types.rs`, `upsert.rs`, `windowing.rs`. Line 121 also says `enrichment.rs - Enrichment pass (extracted from pipeline.rs)` — the parenthetical is stale since `pipeline.rs` no longer exists as a file.
- **Suggested fix:** Replace `pipeline.rs - Multi-threaded indexing pipeline` with a `pipeline/` directory block listing its submodules. Remove the `(extracted from pipeline.rs)` parenthetical from the `enrichment.rs` entry.

#### DOC-14: CONTRIBUTING.md: `store/helpers.rs` shown as flat file; it is now a `store/helpers/` directory
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:142
- **Description:** The architecture tree shows `helpers.rs - Types, embedding conversion functions`. The actual structure is `src/store/helpers/` with 8 submodules: `embeddings.rs`, `error.rs`, `mod.rs`, `rows.rs`, `scoring.rs`, `search_filter.rs`, `sql.rs`, `types.rs`.
- **Suggested fix:** Replace the `helpers.rs` single-line entry with a `helpers/` directory block. See also DOC-18 and DOC-20 for secondary references to this stale path.

#### DOC-15: CONTRIBUTING.md: duplicate `search/` block in architecture tree
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:152-166
- **Description:** `src/search/` appears twice. Lines 152-156 give a brief description. Lines 162-166 give a more detailed description including the scoring submodule contents (`candidate.rs`, `config.rs`, `filter.rs`, `name_match.rs`, `note_boost.rs`). The second block is the accurate one; the first is a stale residue. The `reranker.rs` line (161) is sandwiched between them, disrupting the tree layout.
- **Suggested fix:** Remove lines 152-156 (first `search/` entry). Merge `synonyms.rs` into the retained block at lines 162-166. Move `reranker.rs` to its correct position in the flat-file section.

#### DOC-16: CONTRIBUTING.md: "Adding a New CLI Command" guide references wrong file paths
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:266, 277
- **Description:** Line 266 says `src/cli/commands/<name>.rs` — commands now live in a category subdirectory (e.g., `src/cli/commands/io/<name>.rs`). Line 277 says `look at src/cli/commands/blame.rs or src/cli/commands/dead.rs` — these are now at `src/cli/commands/io/blame.rs` and `src/cli/commands/review/dead.rs`.
- **Suggested fix:** Update line 266 to `src/cli/commands/<group>/<name>.rs` with a note about the 7 group directories. Update line 277 to use the correct paths.

#### DOC-17: README command reference missing `reconstruct`, `brief`, `neighbors`, `affected`, `train-pairs`
- **Difficulty:** easy
- **Location:** README.md:440-498 (Claude Code Integration command list)
- **Description:** Five commands present in `src/cli/definitions.rs` are absent from the README command reference block: `cqs reconstruct` (reconstruct source from index), `cqs brief` (one-line-per-function summary), `cqs neighbors` (brute-force cosine neighbors), `cqs affected` (functions/callers/tests affected by diff), and `cqs train-pairs` (extract NL/code training pairs). Agents reading the README to discover available commands will not know these exist.
- **Suggested fix:** Add entries for all five commands to the command list in the Claude Code Integration section. Example: `- \`cqs reconstruct <path>\` - reconstruct source file from index (works without source on disk)`.

#### DOC-18: `src/store/migrations.rs` doc comment references stale path `helpers.rs`
- **Difficulty:** easy
- **Location:** src/store/migrations.rs:8
- **Description:** The module doc says `1. Increment \`CURRENT_SCHEMA_VERSION\` in \`helpers.rs\``. The constant is now in `src/store/helpers/mod.rs`. A developer following this instruction for a schema bump would look for the wrong file.
- **Suggested fix:** Change to `1. Increment \`CURRENT_SCHEMA_VERSION\` in \`helpers/mod.rs\``.

#### DOC-19: `src/plan.rs` New Command template references stale paths
- **Difficulty:** easy
- **Location:** src/plan.rs:72-75
- **Description:** The `TaskTemplate` for "New CLI Command" tells developers to edit `src/cli/mod.rs` for both the Commands enum (now in `src/cli/definitions.rs`) and the dispatch match arm (now in `src/cli/dispatch.rs`). Line 74 says `src/cli/commands/<name>.rs` without noting that commands are now in category subdirectories. This template is shown to agents running `cqs plan "add new command"`.
- **Suggested fix:** Update to: `src/cli/definitions.rs — Add variant to Commands enum with args`, `src/cli/dispatch.rs — Add match arm in run_with()`, `src/cli/commands/<group>/<name>.rs — New file in appropriate subdirectory (graph/, index/, infra/, io/, review/, search/, train/)`.

#### DOC-20: `src/plan.rs` Schema Migration template references stale path `helpers.rs`
- **Difficulty:** easy
- **Location:** src/plan.rs:269
- **Description:** The `TaskTemplate` for "Schema Migration" shows `"src/store/helpers.rs — Bump CURRENT_SCHEMA_VERSION"`. The file is now `src/store/helpers/mod.rs`. A developer following the schema migration checklist from `cqs plan` would look for the wrong file.
- **Suggested fix:** Update to `"src/store/helpers/mod.rs — Bump CURRENT_SCHEMA_VERSION"`.

## Code Quality

#### CQ-NEW-1: `scout_to_json`, `task_to_json`, `plan_to_json`, `onboard_to_json` are trivial wrappers surviving as public API noise
- **Difficulty:** easy
- **Location:** `src/scout.rs:461`, `src/task.rs:274`, `src/plan.rs:408`, `src/onboard.rs:265`
- **Description:** After PR #783 migrated result types to typed `Serialize`, these four functions are pure pass-through wrappers. `scout_to_json` / `task_to_json` / `plan_to_json` each call `serde_json::to_value(result)` with a `tracing::warn!` fallback; `onboard_to_json` calls `serde_json::to_value(result)` with no fallback at all. Every caller could call `serde_json::to_value` directly and get identical behavior. Because `scout::*`, `task::*`, and `onboard::*` are glob-re-exported from `src/lib.rs` (lines 133, 136, 130), these three function names inflate the public crate API surface. The batch handlers in `batch/handlers/misc.rs` and `batch/handlers/info.rs` are the only production callers beyond CLI commands that hold a `mut` JSON reference to inject content after the call. The comment at `related.rs:59-60` ("backward compatibility with batch handlers") reveals the origin pattern for these wrappers.
- **Suggested fix:** Delete all four wrappers. Update each call site to call `serde_json::to_value(&result).unwrap_or_else(|e| { tracing::warn!(...); serde_json::json!({}) })` inline. Remove the four names from any glob re-exports.

#### CQ-NEW-2: `related_result_to_json` and `where_to_json` wrap typed builders that callers could use directly
- **Difficulty:** easy
- **Location:** `src/cli/commands/search/related.rs:62`, `src/cli/commands/search/where_cmd.rs:82`
- **Description:** Both functions build a typed struct (`RelatedOutput`, `WhereOutput`) and immediately call `serde_json::to_value` on it. The typed builders (`build_related_output`, `build_where_output`) are already public. Each wrapper has exactly one batch caller: `dispatch_related` at `batch/handlers/graph.rs:275` and `dispatch_where` at `batch/handlers/misc.rs:231`. The CLI paths already call `build_*_output` directly. Both wrapper functions are re-exported from `commands/mod.rs:41-42` and `commands/search/mod.rs:16,19`, surviving from the incremental refactor sequence.
- **Suggested fix:** Delete both wrappers. Update the two batch callers to call `build_related_output`/`build_where_output` + `serde_json::to_value`. Remove the re-exports. Replace the `where_to_json_compat` test at `where_cmd.rs:223` with a direct test of `build_where_output` serialization.

#### CQ-NEW-3: `display.rs` inline JSON construction duplicated across three functions and diverges from `ChunkOutput`
- **Difficulty:** medium
- **Location:** `src/cli/display.rs:230-251`, `src/cli/display.rs:446-465`, `src/cli/display.rs:410-421`
- **Description:** `display_unified_results_json` and `display_tagged_results_json` each contain nearly identical inline `serde_json::json!({...})` blocks for `UnifiedResult::Code(r)` (~15 lines each). A third near-copy exists in `display_similar_results_json` (3-field version). Meanwhile, `batch/types.rs` defines `ChunkOutput` — a typed `#[derive(Serialize)]` struct for search results. The schemas diverge: `display.rs` adds `"type": "code"` and `"has_parent"` fields absent from `ChunkOutput`; `ChunkOutput` makes `signature`/`content` optional while `display.rs` always includes them; `display_similar_results_json` omits `line_end`, `signature`, `content`, and `has_parent` entirely. Three divergent schemas for "a search result" with no authoritative definition.
- **Suggested fix:** Extend `ChunkOutput` (or create `SearchResultOutput`) with `type_label: Option<String>`, `has_parent: bool`, and optional `parent_*` fields. Migrate `display_unified_results_json` and `display_tagged_results_json` to it. Promote `ChunkOutput` from `pub(super)` to `pub(crate)`. This is the same pattern PRs #776-#783 applied to other batch handlers.

#### CQ-NEW-4: `cmd_onboard` token-packing logic diverges from the shared helpers used by `dispatch_onboard`
- **Difficulty:** medium
- **Location:** `src/cli/commands/search/onboard.rs:27-98`
- **Description:** Two different token-packing strategies exist for the same onboard output. `dispatch_onboard` (batch, `batch/handlers/info.rs:272`) uses `onboard_scored_names` + `fetch_and_pack_content` + `inject_content_into_onboard_json` — fetches content from store, injects it for budget-included items, leaves unincluded items at their serialized state. `cmd_onboard` (CLI) uses an inline 70-line strategy working from already-fetched `result` content: computes total tokens, and if over budget zeroes excluded items by setting `content` to `""`. The two produce different output shapes — CLI emits all items with empty-string content for excluded entries; batch only annotates items that fit the budget. The shared helpers in `commands/mod.rs` exist precisely to prevent this pattern.
- **Suggested fix:** Refactor `cmd_onboard` to use `onboard_scored_names` + `fetch_and_pack_content` + `inject_content_into_onboard_json`. Decide once on whether excluded items get `""` or absent content, encode that in the shared helper.

#### CQ-NEW-5: `dispatch_similar` and `cmd_similar` produce incompatible JSON schemas
- **Difficulty:** medium
- **Location:** `src/cli/batch/handlers/info.rs:129-144`, `src/cli/display.rs:407-421`
- **Description:** The batch `dispatch_similar` returns `{name, file, score}` (3 fields). The CLI `cmd_similar` goes through `display_similar_results_json` which returns `{file, line_start, line_end, name, signature, language, chunk_type, score, content}` (9 fields). The `TODO(json-schema)` comment in `similar.rs:4-5` acknowledges this divergence but marks it as "blocked until display module has typed output structs." That blocker no longer applies — `ChunkOutput` in `batch/types.rs` already covers the richer shape. The batch handler is the stale path, intentionally stripped-down at an earlier iteration.
- **Suggested fix:** Update `dispatch_similar` to use `ChunkOutput::from_search_result(r, true)` matching the CLI output shape. Migrate `cmd_similar` off `display_similar_results_json` onto the typed path. Delete `display_similar_results_json`. Remove the `TODO` comment in `similar.rs`.

#### CQ-NEW-6: `impact_to_json` injects computed count fields post-serialization, making itself a mandatory wrapper
- **Difficulty:** medium
- **Location:** `src/impact/format.rs:10-27`, `src/impact/types.rs:54-67`
- **Description:** `ImpactResult` derives `Serialize` but `impact_to_json` mutates the serialized JSON to inject `caller_count`, `test_count`, and `type_impacted_count` as computed fields (`callers.len()`, `tests.len()`, `type_impacted.len()`). These are derivable from the struct's own vecs. This forces `impact_to_json` to remain as a mandatory wrapper — callers cannot use `serde_json::to_value` directly, unlike every other post-#783 result type. The function also uses `as_object_mut()` + `insert()` which silently no-ops if the root value is not an object. `ImpactResult` was listed in PR #783's scope but the count injection was not removed.
- **Suggested fix:** Add `caller_count`, `test_count`, `type_impacted_count` as computed fields using a custom `Serialize` impl or `#[serde(serialize_with)]` that derives them from vec lengths at serialization time. Once the struct serializes cleanly, `impact_to_json` reduces to a trivial wrapper and can be deleted alongside CQ-NEW-1.

#### CQ-NEW-7: `SearchResult::to_json` and `UnifiedResult::to_json` are manual JSON builders inconsistent with the typed-Serialize direction
- **Difficulty:** medium
- **Location:** `src/store/helpers/types.rs:124-161`, `src/store/helpers/types.rs:349-370`
- **Description:** `SearchResult` and `UnifiedResult` have no `#[derive(Serialize)]` and instead provide manual `to_json()` / `to_json_relative()` methods building `serde_json::json!({...})` blobs. The comment at `types.rs:111-115` says this was intentional (AD-27) to avoid divergent serialization paths. However, PRs #776-#783 moved every other result type to typed `Serialize`, making `SearchResult` the sole holdout. No production code calls `SearchResult::to_json()` directly — the only non-test caller is `UnifiedResult::to_json()` which delegates to it. `display.rs` must duplicate the same 10-field JSON blob in two functions instead of sharing a typed struct (CQ-NEW-3). The original AD-27 rationale now argues for uniformity via typed `Serialize`, not for preserving the manual builder.
- **Suggested fix:** Add a typed `SearchResultOutput` struct with `#[derive(Serialize)]`, `has_parent: bool`, and `#[serde(serialize_with)]` for path normalization. Replace `display.rs` inline blobs and `UnifiedResult::to_json()` with this struct. Delete `SearchResult::to_json()`, `to_json_relative()`, `UnifiedResult::to_json()`, and `to_json_relative()`. This resolves CQ-NEW-3 as a consequence.

## Scaling & Hardcoded Limits

#### SHL-16: `bfs_shortest_path` MAX_NODES=10_000 has no env-var override
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/trace.rs:272
- **Description:** `bfs_shortest_path` (used by `cmd_trace`) caps the BFS at 10,000 visited nodes as a local `const` with no override. The sibling commands impact and gather both have env-var overrides (`CQS_IMPACT_MAX_NODES`, `CQS_GATHER_MAX_NODES`) added in the v1.13.0 audit. The trace BFS was not updated at the same time. Dense call graphs in large monorepos hit this cap silently — the warning logs but the returned path is truncated with no user-visible error in the CLI output.
- **Suggested fix:** Extract to a `bfs_trace_max_nodes()` function reading `CQS_TRACE_MAX_NODES` with `OnceLock` and default 10_000, matching the pattern in `src/impact/bfs.rs:12-27`.

#### SHL-17: HNSW persist file-size limits hardcoded — rejects legitimate large indexes
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs:436-437
- **Description:** `HnswIndex::load()` hard-rejects any graph file >500MB and any data file >1GB before deserializing. These are OOM guards (correct intent) but are compile-time constants with no env-var override. For a large monorepo at 500k chunks with 1024-dim BGE-large: data file size ≈ 500k × 1024 × 4 bytes × HNSW graph overhead ≈ 2–4GB. The 1GB limit fires before deserialization, causing `cqs search` to fall back to O(n) brute-force with no explanation to the user. The existing `cagra_max_bytes()` in `src/cagra.rs:49` shows the pattern to follow.
- **Suggested fix:** Read `CQS_HNSW_MAX_GRAPH_BYTES` and `CQS_HNSW_MAX_DATA_BYTES` env vars via `OnceLock`, defaulting to current constants. Emit a `tracing::warn!` when the limit is raised by env var so users know they are overriding a safety guard.

#### SHL-18: `FILE_BATCH_SIZE=5_000` compile-time constant — not configurable
- **Difficulty:** easy
- **Location:** src/cli/pipeline/types.rs:63
- **Description:** The indexing pipeline processes files in batches of 5,000. `EMBED_BATCH_SIZE` is configurable via `CQS_EMBED_BATCH_SIZE`, but `FILE_BATCH_SIZE` is a plain `const` with no override. Operators on memory-constrained systems have no way to reduce memory pressure at the file-batch level without recompiling. The inconsistency with `EMBED_BATCH_SIZE` is confusing for operators tuning pipeline memory.
- **Suggested fix:** Convert to a function `file_batch_size()` reading `CQS_FILE_BATCH_SIZE` with default 5_000, matching the style of `embed_batch_size()` in the same file.

#### SHL-19: `MAX_CONTENT_CHARS=8000` for LLM summarization is not configurable
- **Difficulty:** easy
- **Location:** src/llm/mod.rs:156
- **Description:** LLM summarization (SQ-6), doc-comment generation (SQ-8), and HyDE query generation each truncate chunk content to 8,000 characters before sending to the API. The constant is shared by `prompts.rs`, `doc_comments.rs`, and `hyde.rs`. Users running a custom LLM via `CQS_LLM_MODEL`/`CQS_LLM_API_BASE` (e.g., a 4k-token local model) have no way to reduce this without recompiling. All other LLM parameters (`max_tokens`, `hyde_max_tokens`, `model`, `api_base`) are already env-var or config-file overridable.
- **Suggested fix:** Add `MAX_CONTENT_CHARS` to `LlmConfig` with env-var `CQS_LLM_MAX_CONTENT_CHARS` and config file field `llm_max_content_chars`, defaulting to 8000. Thread `LlmConfig` to the three use sites (already done for `max_tokens`).

#### SHL-20: Telemetry file grows unbounded — no size cap or rotation
- **Difficulty:** easy
- **Location:** src/cli/telemetry.rs:40-52
- **Description:** `log_command` appends a JSON line on every invocation with no size check. A batch processing session with 10,000 commands can generate ~5MB in one run with no automatic rotation. `cmd_telemetry_reset` archives the file but requires manual invocation. Other file-backed stores have explicit guards: notes.toml (10MB guard at `src/note.rs:171`), watch mtime map (5,000 entry prune at `src/cli/watch.rs:484`). Telemetry is the only write-only append file without any limit.
- **Suggested fix:** In `log_command`, check file size before appending. If size exceeds `CQS_TELEMETRY_MAX_BYTES` (default 10MB), rename to `telemetry_<ts>.jsonl` and start fresh, emitting a single `tracing::info!`.

#### SHL-21: Stale doc comments hardcode "768" where code uses `EMBEDDING_DIM`
- **Difficulty:** trivial
- **Location:** src/math.rs:73,77,82,86,88; src/drift.rs:248; src/search/scoring/candidate.rs:510
- **Description:** Seven doc comment lines in test helpers say "768 times", "768 f32 values", "idx >= 768", or "normalized 768-dim". The actual code uses `crate::EMBEDDING_DIM` (currently 1024 for the default BGE-large model since v1.9.0). The comments are wrong for any user running the default model and will mislead developers reading the test code.
- **Suggested fix:** Replace literal "768" in doc comments of these test helpers with `EMBEDDING_DIM` or the phrase "model-configured dimension".

## API Design

#### AD-11: `ProjectSearchResult` uses `"line"` instead of `"line_start"`
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/project.rs:24
- **Description:** `ProjectSearchResult` declares `pub line: u32` which serializes as `"line"` in JSON output. Every other output struct uses `"line_start"` (normalized during the schema migration). The test at line 180 asserts `json["line"] == 42`, confirming the field is live. `ProjectSearchResult` was not updated during the migration.
- **Suggested fix:** Rename the field to `line_start` (or add `#[serde(rename = "line_start")]`). Update the single test. The population site at line 130 already reads from `r.line_start`.

#### AD-12: `lines: [u32; 2]` array used instead of `line_start`/`line_end` in context and blame output
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:59,201,427 and src/cli/commands/io/blame.rs:173
- **Description:** Four output structs serialize line ranges as a 2-element JSON array `"lines": [start, end]`: `CompactChunkEntry`, `FullChunkEntry`, `SummaryChunkEntry`, and `BlameOutput`. All other output structs use two separate scalar fields `"line_start"` and `"line_end"`. Agents consuming JSON output must use different access patterns depending on which command they called: `result["line_start"]` for most commands vs `result["lines"][0]` for context/blame.
- **Suggested fix:** Replace `lines: [u32; 2]` with separate `line_start: u32` and `line_end: u32` fields in all four structs. Update the population sites (already have `c.line_start` / `c.line_end` available). Update tests in `blame.rs` lines 381-382.

#### AD-13: `NeighborEntry` uses `"similarity"` instead of `"score"` for the relevance field
- **Difficulty:** easy
- **Location:** src/cli/commands/search/neighbors.rs:20
- **Description:** `NeighborEntry` has `similarity: f32` which serializes as `"similarity"`. All other search-adjacent output structs use `"score"`: `SimilarEntry` (explain), `ProjectSearchResult`, `WhereSuggestionEntry`, and `ChunkOutput` (batch search). An agent expecting `result["score"]` to work across search commands will fail on `neighbors` output.
- **Suggested fix:** Rename `similarity` to `score` in `NeighborEntry`. Update the test at `neighbors.rs:218` which asserts on `json["similarity"]`.

#### AD-14: Batch `dispatch_context` summary mode diverges completely from CLI `summary_to_json`
- **Difficulty:** medium
- **Location:** src/cli/batch/handlers/info.rs:187-193
- **Description:** When `summary=true`, `dispatch_context` (batch) returns `{"file", "chunk_count", "total_callers", "total_callees"}` built inline. The CLI's `summary_to_json` returns a typed `SummaryOutput` with `{"file", "chunk_count", "chunks": [{name, chunk_type, lines}], "external_caller_count", "external_callee_count", "dependent_files"}`. These are completely different schemas for the same command mode. The batch context full-mode at `info.rs:226-234` also uses an independent inline `serde_json::json!{}` with `"language"` and `"total"` fields absent from the typed `FullOutput`.
- **Suggested fix:** Expose `summary_to_json` and `full_to_json` for batch use (already `pub(crate)`). Replace the inline JSON construction in `dispatch_context` with calls to the shared typed builders.

#### AD-15: `ExternalCallerEntry` uses `"caller"`/`"callee"` instead of `"name"` for function names
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs:210,219
- **Description:** `ExternalCallerEntry` serializes as `{"caller": ..., "caller_file": ..., "calls": ..., "line_start": ...}` and `ExternalCalleeEntry` as `{"callee": ..., "called_from": ...}`. All other output structs use `"name"` as the primary function identifier key. An agent expecting `result["name"]` to work across command outputs will find `context` entries use `"caller"` or `"callee"` instead.
- **Suggested fix:** Rename `caller` to `name` in `ExternalCallerEntry` and `callee` to `name` in `ExternalCalleeEntry`. The surrounding array field names (`external_callers`, `external_callees`) provide disambiguation. Align the batch full-context handler to use the typed structs.

#### AD-16: Pipeline test fixtures use pre-migration schema field names (`"function"`, `"line"`)
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs:380-395
- **Description:** `test_extract_names_callees` (line 378) uses `"function": "f"` and `"calls": [{"name": "a", "line": 1}]`. `test_extract_names_impact` (line 387) uses `"function": "f"`. The current `CalleesOutput` serializes as `"name"` (not `"function"`) and `CalleeEntry` as `"line_start"` (not `"line"`). Tests still pass because `extract_names` is key-agnostic, but the fixtures no longer reflect actual command output. Agents reading these tests as documentation learn the wrong schema.
- **Suggested fix:** Update both test fixtures to current output: `"name": "f"` and `"line_start": 1`. Add an assertion that `"function"` is absent from the top-level callees object, mirroring the pattern in `callers.rs:test_callees_output_field_names`.

#### AD-17: `CommandContext` has no writable constructor; write commands bypass shared context
- **Difficulty:** medium
- **Location:** src/cli/store.rs:47-109
- **Description:** `CommandContext` only exposes `open_readonly()`. Write commands (`cmd_gc` at `gc.rs:43`, the batch session initializer at `batch/mod.rs:486`) call `open_project_store()` directly and build their own `(store, root, cqs_dir)` tuple, bypassing the lazy-init pattern for `embedder` and `reranker`. Any logic added to `CommandContext` must be duplicated for write paths. The struct is named "shared context for CLI commands" but only covers read paths.
- **Suggested fix:** Add `CommandContext::open_writable(cli)` using `open_project_store()` internally. Update `cmd_gc` and the batch session to use it. Non-breaking: existing `open_readonly()` callers are unchanged.

#### AD-18: `SuggestOutput` and `build_suggest_output` are dead code; CLI and batch output divergent schemas
- **Difficulty:** easy
- **Location:** src/cli/commands/review/suggest.rs:21-27 and src/cli/batch/handlers/analysis.rs:112-128
- **Description:** `SuggestOutput` and `build_suggest_output` are both `#[allow(dead_code)]` with a "will be wired" comment. CLI `cmd_suggest` emits a bare JSON array (`[{text, sentiment, mentions, reason}]`). Batch `dispatch_suggest` emits `{"suggestions": [{...}], "total": N, "applied": bool}` built with `serde_json::json!{}`. Neither path uses the typed structs, and the two schemas differ: CLI is a bare array, batch wraps it in an object.
- **Suggested fix:** Wire `build_suggest_output` into both paths. CLI should emit the `SuggestOutput` envelope (breaking schema change, but consistent with all other commands). Batch should call `build_suggest_output` instead of inline `json!{}`. Remove both `#[allow(dead_code)]` annotations once wired.

## Algorithm Correctness

#### AC-7: `bfs_shortest_path` checks node cap before target check — returns `None` for reachable paths in large graphs
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/trace.rs:280-284
- **Description:** In the main BFS loop, line 280 checks `if visited.len() >= MAX_NODES { break }` before line 284 checks `if current == target`. When the target node was already discovered and inserted into `visited` (as a neighbor of some predecessor), it sits in the queue. If enough other nodes are discovered first, `visited.len()` reaches `MAX_NODES` and the loop breaks before the target's turn in the queue. The function returns `None` despite a valid path existing and the target being present in `visited`. The bug was introduced when AC-4 (no node cap) was fixed in PR #737 — the cap was inserted at the top of the loop body rather than after the target check. Trace: with MAX_NODES=10_000 and a graph with 10,001+ reachable functions, the BFS can correctly discover the target at position 9,999, enqueue it, then break before processing it.
- **Suggested fix:** Move the target check before the node cap check: swap lines 280-283 (cap check) with lines 284-296 (target check). The cap check should only guard expansion of new neighbors, not prevent returning a found path.

#### AC-8: `window_overlap_tokens` produces overlap that exactly equals `max_tokens / 2` at the minimum window size, causing `split_into_windows` to return an error and silently skip windowing
- **Difficulty:** easy
- **Location:** src/cli/pipeline/windowing.rs:22-24 and src/embedder/mod.rs:391-396
- **Description:** `max_tokens_per_window()` clamps the minimum window size to 128 via `.max(128)`. `window_overlap_tokens(128)` returns `max(64, 128/8) = 64`. `split_into_windows` validates `overlap >= max_tokens / 2`, which evaluates as `64 >= 128/2 = 64` — true — and returns `Err(...)`. This affects any model with `max_seq_length ∈ [32, 161]`, where `model_max_seq - WINDOW_OVERHEAD` is 128 or 129 (both produce integer `max_tokens/2 = 64`). The error is caught at `windowing.rs:67-71` where `Err(e)` emits a tracing warn and passes the chunk through unchanged. Long chunks exceeding the model's actual token limit are sent to the embedder unwindowed, producing truncated embeddings silently.
- **Suggested fix:** Either (1) change the guard in `split_into_windows` to `overlap > max_tokens / 2` (strict inequality, matching the comment which says "less than"), or (2) change `window_overlap_tokens` to return `(max_tokens / 2).saturating_sub(1).max(1)` when overlap would equal or exceed `max_tokens / 2`. Option (1) is simpler and matches the comment's stated constraint.

#### AC-9: `rrf_fuse` does not deduplicate `fts_ids` — asymmetric with `semantic_ids` deduplication
- **Difficulty:** easy
- **Location:** src/store/search.rs:151-155
- **Description:** `rrf_fuse` deduplicates `semantic_ids` (lines 140-149, with a comment explaining the rationale) but processes `fts_ids` without any deduplication (lines 151-155). If the FTS list contains duplicate chunk IDs, each occurrence accumulates an independent rank contribution, inflating the score. The comment at line 138 says "Deduplicate semantic_ids — keep first occurrence (best rank) only. Duplicates would get RRF contributions at multiple ranks, inflating score." The same rationale applies to `fts_ids`. In practice, SQLite FTS5 `SELECT id FROM chunks_fts WHERE MATCH` on a primary-key column should not produce duplicate rows, so this is currently safe. However, `rrf_fuse_test` accepts arbitrary string slices, making it easy for callers to pass duplicate FTS IDs, and the asymmetry makes the function fragile for future callers that bypass the SQL layer (e.g., in-memory search, test injection).
- **Suggested fix:** Add an analogous deduplication loop for `fts_ids` mirroring lines 140-149: track `seen_fts: HashSet<&str>` and skip subsequent occurrences of any FTS ID. Update the property test comment to note that deduplication is applied to both lists.

#### AC-10: `build_test_map` reverse BFS has no node cap — unbounded memory on large call graphs
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/test_map.rs:64-76
- **Description:** `build_test_map` performs a reverse BFS from the target function through `graph.reverse` to find ancestor test functions. Unlike `reverse_bfs` in `src/impact/bfs.rs` (which enforces `bfs_max_nodes()`), `build_test_map` has no node cap. On a hub function called by thousands of functions (e.g., `unwrap`, `clone`, `into`), the BFS expands to the entire call graph. The triage item CQ-2 ("dispatch_test_map duplicates 80-line reverse-BFS with divergent guard") identifies the duplication but the unfixed status means neither path has a cap. The batch handler's inline BFS in `src/cli/batch/handlers/graph.rs` also lacks a cap. Neither is tested with large inputs.
- **Suggested fix:** Add a `const TEST_MAP_MAX_NODES: usize = 10_000;` check inside the BFS loop of `build_test_map`, mirroring the pattern in `reverse_bfs`. Emit a `tracing::warn!` when the cap fires. Resolving CQ-2 (shared function) would make this a one-time fix.

#### AC-11: `index_pack` includes the first item unconditionally when `budget > 0`, even if cost far exceeds budget — no 10x guard
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:436-443
- **Description:** `index_pack` (used by waterfall budgeting in `task`) includes the first item (by score) unconditionally when `kept.is_empty()`, even if its cost exceeds the budget by orders of magnitude. This diverges from `token_pack` which caps the override at 10x the budget (`tokens > budget * 10`). `index_pack` is documented as returning empty for `budget == 0`, but for `budget > 0` any single item cost is accepted. In `build_budgeted_task`, sections share a `remaining_budget` computed from `budget - used_so_far`. A section with a 50,000-token item and a 100-token budget allocation will include it, setting `used = 50,000`. Subsequent sections see `remaining_budget = budget - 50,000` which underflows to 0 via `saturating_sub`, silently zeroing all downstream allocations. The triage for AC-6 says `token_pack` first-item override was fixed in PR #737, but `index_pack` was not given the same guard.
- **Suggested fix:** Add the same 10x cap as `token_pack`: in the `if used + cost > budget && !kept.is_empty()` branch, change to `if used + cost > budget && (!kept.is_empty() || cost > budget * 10) { break; }`. This prevents pathological overshoots while preserving the "always show at least one result" guarantee for reasonable items.

## Extensibility

#### EXT-39: `"block_comment"` doc_format tag has no match arm — Structured Text silently gets wrong comment syntax
- **Difficulty:** easy
- **Location:** src/doc_writer/formats.rs:50-126 and src/language/structured_text.rs:141
- **Description:** `doc_format_from_tag` is a `match` on string tags delegated from `LanguageDef.doc_format`. `structured_text.rs` sets `doc_format: "block_comment"` (line 141) with a convention comment saying "Use `(* ... *)` block comments before declarations." However, `doc_format_from_tag` has no `"block_comment"` arm — it falls through to the `_ =>` default which produces `// ` prefix (`go_comment` style). When `cqs improve-docs` runs on Structured Text, generated doc comments use `// ` instead of `(* *)`. The test `all_language_doc_formats_are_valid` at line 449 lists `"block_comment"` in its `valid` array, making it appear validated, but that test only checks that the tag is in the allowlist — it does not verify that the tag has a non-default match arm. The bug was introduced when Structured Text support was added without adding the corresponding format arm.
- **Suggested fix:** Add a `"block_comment"` arm to `doc_format_from_tag` producing `DocFormat { prefix: "(*", line_prefix: "   ", suffix: " *)", position: InsertionPosition::BeforeFunction }` (matching IEC 61131-3 block comment convention). Add a test in the `#[cfg(test)]` block asserting `doc_format_for(Language::StructuredText).prefix == "(*"`.

#### EXT-40: `chat.rs::command_names()` is a stale hardcoded list — 8 batch commands missing from autocomplete
- **Difficulty:** easy
- **Location:** src/cli/chat.rs:100-109
- **Description:** `cmd_chat` (the interactive REPL) provides tab-completion via `command_names()`, a manually maintained `vec!` of 25 strings. Eight commands present in `BatchCmd` are absent: `review`, `ci`, `diff`, `impact-diff`, `plan`, `suggest`, `gc`, `refresh`. Users typing these commands in `cqs chat` get no autocomplete hint even though the commands work. The peer function `telemetry::describe_command` (line 59-83 of `telemetry.rs`) demonstrates the correct pattern: it uses `Cli::command().get_subcommands()` to derive the list at runtime from clap's registration, so new commands are recognized automatically. `BatchInput` derives `Parser` and is available in scope (`use super::batch;`), making the same pattern applicable.
- **Suggested fix:** Replace the static `vec!` with a function that calls `BatchInput::command().get_subcommands().map(|sc| sc.get_name().to_string())`, then extend with `["exit", "quit", "clear"]`. This eliminates the manual sync requirement and ensures all future batch commands appear in autocomplete automatically.

## Platform Behavior

#### PB-8: `DiffEntryOutput.file` and `DriftEntryOutput.file` use `display()` — backslashes on Windows
- **Difficulty:** easy
- **Location:** src/cli/commands/io/diff.rs:62, src/cli/commands/io/drift.rs:53
- **Description:** Both `build_diff_output` and `build_drift_output` set the `file` field of their typed JSON output struct using `e.file.display().to_string()`. `PathBuf::display()` emits OS-native separators, so on Windows (a declared release target in `.github/workflows/release.yml`) this produces backslashes in JSON: `"file": "src\\lib.rs"`. All other output structs that produce a `file: String` field use `cqs::normalize_path(&path)` to guarantee forward slashes. The `DiffEntry.file` and `DriftEntry.file` fields are `PathBuf` without `#[serde(serialize_with = "crate::serialize_path_normalized")]`, making this the only JSON path in the output layer that bypasses normalization. Agents consuming `cqs diff --json` or `cqs drift --json` on Windows receive backslash-separated paths that fail any path comparison with strings from other commands.
- **Suggested fix:** Replace `e.file.display().to_string()` with `cqs::normalize_path(&e.file)` in both `build_diff_output` (diff.rs:62) and `build_drift_output` (drift.rs:53). The fix is two characters each: `normalize_path(&e.file)` instead of `e.file.display().to_string()`.

#### PB-9: `ExplainOutput.file` is relative but `CallerEntry.file` and `SimilarEntry.file` within it are absolute — mixed convention in a single JSON object
- **Difficulty:** easy
- **Location:** src/cli/commands/graph/explain.rs:231, 258, 279
- **Description:** `build_explain_output` populates the top-level `ExplainOutput.file` with `cqs::rel_display(&chunk.file, root)` (relative path, e.g., `src/lib.rs`). Within the same output object, `CallerEntry.file` uses `normalize_path(&c.file)` (absolute path, e.g., `/home/user/project/src/lib.rs`) and `SimilarEntry.file` uses `normalize_path(&r.chunk.file)` (also absolute). An agent parsing the explain JSON must use a different path resolution strategy for the target function vs its callers and similar functions. The `build_callers` function in `callers.rs` intentionally uses `normalize_path` (absolute), but `ExplainOutput` uses it for callers in a context where `root` is available and a relative path would be consistent. No other output struct mixes relative and absolute paths in the same JSON object.
- **Suggested fix:** Change the `CallerEntry` construction in `build_explain_output` (line 231) to use `cqs::rel_display(&c.file, root)` instead of `normalize_path(&c.file)`. Change the `SimilarEntry` construction (line 258) likewise. The `build_callers` shared builder in `callers.rs` can remain absolute for standalone callers/callees commands; the inconsistency is specific to how `explain` assembles the envelope.

#### PB-10: `chrono_like_timestamp` spawns POSIX `date` — fails silently on Windows native, degrades archive filenames
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:475-491
- **Description:** `chrono_like_timestamp` runs `Command::new("date").arg("+%Y%m%d_%H%M%S")` to format the current time as `YYYYMMDD_HHMMSS`. On Windows native (a release target: `x86_64-pc-windows-msvc` in `release.yml`), `date` is a `cmd.exe` built-in rather than a standalone executable. `Command::new("date")` will fail with `ERROR_FILE_NOT_FOUND`, `.output().ok()` returns `None`, and the fallback at line 484 emits a raw Unix timestamp integer (e.g., `1743523200`) as the archive filename: `telemetry_1743523200.jsonl`. This is functional but produces an opaque filename. macOS `date` supports the `+FORMAT` argument the same as GNU date, so macOS is unaffected. The comment in the code ("simpler than reimplementing timezone-aware formatting") is now misleading since `std::time::SystemTime` already covers the fallback.
- **Suggested fix:** Replace the `date` subprocess with a pure-Rust implementation using `std::time::SystemTime`. Example: decompose `duration_since(UNIX_EPOCH)` into total seconds, then compute year/month/day/hour/min/sec via integer arithmetic (no `chrono` dependency needed for this). This is about 20 lines, avoids platform branching, and produces correct output everywhere. Alternatively, add `chrono` as a dev-dependency or use the already-present `time` crate if available.

#### PB-11: L5X CDATA line-ending normalization precedes CDATA extraction — verified safe, but no test documents the guarantee
- **Difficulty:** easy
- **Location:** src/parser/mod.rs:200, src/parser/l5x.rs:246-247
- **Description:** The CRLF-to-LF normalization at `mod.rs:200` (`source.replace("\r\n", "\n")`) runs before `parse_l5x_chunks` is called, so CDATA content extracted by `CDATA_RE` at `l5x.rs:246` never contains `\r`. This is correct. However, no test verifies this guarantee: there is no test that passes an L5X file with CRLF line endings inside a `<![CDATA[...]]>` block and asserts that the extracted ST source is free of `\r`. A future refactor that moves L5X parsing before the normalization step (e.g., for streaming parse or lazy read) would silently break the ST code passed to the tree-sitter parser. Tree-sitter's structured-text grammar may or may not handle `\r` within tokens, and an embedding generated from CR-contaminated source would differ from the same source with LF only, causing unnecessary reindexing.
- **Suggested fix:** Add a test in `src/parser/l5x.rs` with a CRLF L5X source string (substitute `\r\n` for `\n` throughout) passed to `extract_l5x_regions` directly (bypassing `parse_file`), asserting that the returned `StRegion.source` contains no `\r`. This documents the invariant and guards against ordering changes.

#### EXT-41: `PIPEABLE_NAMES` sync test is one-directional — adding a pipeable command without updating the const goes undetected
- **Difficulty:** easy
- **Location:** src/cli/batch/pipeline.rs:32-35 and 614-630
- **Description:** `PIPEABLE_NAMES` is a `const &[&str]` used only in the error message produced by `pipeable_command_names()`. The test `test_pipeable_names_sync` (line 617) verifies that every entry in `PIPEABLE_NAMES` parses as a valid pipeable command. However, it does not verify the inverse: that every command returning `true` from `BatchCmd::is_pipeable()` is listed in `PIPEABLE_NAMES`. If a developer adds a new pipeable variant to `BatchCmd::is_pipeable()` without updating `PIPEABLE_NAMES`, the error message emitted by `parse_pipeline` will silently omit the new command name. Users who see the error ("downstream segment must be one of: blame, callers, ...") will not know their new command can be used as a pipeline target. The issue is low-severity (error message only), but the missing reverse-direction assertion makes the guardrail incomplete.
- **Suggested fix:** Add a reverse-direction test: for each command name in `BatchCmd::command().get_subcommands()`, attempt to parse it with a dummy arg and check `is_pipeable()`. If `is_pipeable()` returns `true`, assert `PIPEABLE_NAMES.contains(&name)`. This mirrors the existing test's structure but in the opposite direction.

#### EXT-42: JSON output field naming conventions (`line_start`, `score`, `name`) not documented in CONTRIBUTING.md or enforced by any test
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:262-277 (Adding a New CLI Command checklist)
- **Description:** Findings AD-11 through AD-18 in this audit document multiple violations of the normalized JSON field naming conventions (`"line_start"` not `"line"`, `"score"` not `"similarity"`, `"name"` not `"function"` or `"caller"`/`"callee"`, two scalar fields not a `"lines": [u32; 2]` array). These violations occurred because the conventions are not documented anywhere a new command author would see them. The CONTRIBUTING.md "Adding a New CLI Command" checklist (line 264) has 10 items covering implementation, dispatch, tracing, error handling, tests, and changelog — but no mention of JSON field naming conventions or the `serialize_path_normalized` requirement for `PathBuf` fields. There is no linting test that verifies new output structs follow these conventions. Each time a new command is added, the author must infer the convention by reading existing structs.
- **Suggested fix:** Add a "JSON Output Conventions" section to CONTRIBUTING.md between the checklist and the "Pattern to follow" line, listing: (1) use `line_start`/`line_end` scalars, not `"line"` or `"lines": [u32; 2]`, (2) use `"score"` for relevance fields, not `"similarity"` or `"rank"`, (3) use `"name"` as the primary function identifier, not `"function"`, `"caller"`, or `"callee"`, (4) use `#[serde(serialize_with = "crate::serialize_path_normalized")]` on all `PathBuf` fields. Optionally add an integration test that exercises each new output struct's `serde_json::to_value` output and asserts the correct field names are present.

## Security

#### SEC-7: `cmd_telemetry_reset` reads entire telemetry.jsonl into memory without size guard
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:440-443
- **Description:** `cmd_telemetry_reset` uses `fs::read_to_string(&current).unwrap_or_default().lines().count()` to count events before archiving. This loads the entire telemetry file into memory with no size limit. In contrast, `parse_entries` uses `BufReader` and processes line-by-line. A telemetry.jsonl file grown to multiple GB (from a long-running agent session with `CQS_TELEMETRY=1`) would cause a large allocation on `cqs telemetry reset`. The 50 MB file size guard used by the parser is not applied here. `fs::read_to_string` creates a full in-memory copy despite the only use being counting newlines.
- **Suggested fix:** Replace `fs::read_to_string(&current).unwrap_or_default().lines().count()` with a `BufReader`-based line count: `std::fs::File::open(&current).map(|f| std::io::BufReader::new(f).lines().count()).unwrap_or(0)`. This keeps peak allocation to one line at a time.

#### SEC-8: `L5K_ROUTINE_BLOCK_RE` scan is O(N * unterminated_blocks) on malformed L5K input
- **Difficulty:** easy
- **Location:** src/parser/l5x.rs:314-315
- **Description:** `L5K_ROUTINE_BLOCK_RE` is `(?msi)^\s*ROUTINE\s+(\w+)\b([^\x00]*?)^\s*END_ROUTINE\b`. For a large L5K file where a `ROUTINE` block has no matching `END_ROUTINE`, the regex engine must scan from the `ROUTINE` keyword to EOF to confirm no terminator follows. With N such unterminated blocks in a 50 MB file, the total work is O(N * file_size). Rust's `regex` crate guarantees linear time via NFA, so true catastrophic backtracking is impossible, but each unterminated block adds a full file-length scan. A crafted L5K file with 100 `ROUTINE` headers and no `END_ROUTINE` terms triggers 100 * 50 MB of character examination. No test covers truncated/unterminated ROUTINE blocks (TC-9 is still open). The 50 MB file limit caps the per-block worst case, but the multiplicative factor is unbounded.
- **Suggested fix:** Add a file-level fast-path: `if !source.contains("END_ROUTINE") { return vec![]; }` before running `L5K_ROUTINE_BLOCK_RE`. For a more robust fix, replace the whole-file regex with a line-by-line state machine that accumulates ROUTINE blocks explicitly, which would also resolve TC-9.

#### SEC-9: `run_git_log_line_range` relies on git's "file not tracked" rejection instead of validating `rel_file` stays within the repo
- **Difficulty:** easy
- **Location:** src/cli/commands/io/blame.rs:79-110
- **Description:** `run_git_log_line_range` validates that `rel_file` does not start with `-` (flag injection guard) and does not contain `:` (git `-L` syntax conflict guard). It does not check whether the path is relative or stays within the project root. `rel_file` comes from `rel_display(&chunk.file, root)` which calls `path.strip_prefix(root).unwrap_or(path)` — if the chunk's stored file path is absolute or contains `..`, `strip_prefix` fails and returns the raw absolute path unchanged. That path (e.g., `/etc/passwd`) is then passed to `git log -L start,end:/etc/passwd`. Git restricts `-L` to tracked files and fails with "no path in working tree", caught at line 120-123. There is no information disclosure, but the defense relies on git's error message text. A future git version changing its error message text would silently break the guard at line 120 (`stderr.contains("no path")`), causing the error to fall through to the generic `bail!` instead.
- **Suggested fix:** Add an explicit pre-check: `if rel_file.starts_with('/') || rel_file.contains("..") { anyhow::bail!("Invalid file path '{}': must be a relative path within the repository", rel_file); }`. On Windows, also reject drive-letter prefixes. This makes the guard independent of git's error output format.

#### SEC-10: `search_by_name` injection guard uses `debug_assert!` which is compiled out in release builds
- **Difficulty:** easy
- **Location:** src/store/search.rs:75-81
- **Description:** `search_by_name` sanitizes the query with `sanitize_fts_query` and adds a `debug_assert!(!normalized.contains('"'), "sanitized query must not contain double quotes")` to verify the invariant. This assert is compiled out in release builds. The only release-mode protection is the runtime guard at lines 79-81 (`if normalized.contains('"') { return Ok(vec![]); }`). The same pattern appears in `search_by_names_batch` at `src/store/chunks/query.rs:376-382`. The runtime guard checks only for `"` — but `sanitize_fts_query` strips several other FTS5 special characters (`*`, `(`, `)`, `+`, `-`, `^`, `:`, and boolean operators `OR`/`AND`/`NOT`). A future change to `sanitize_fts_query` that allows one of these characters through would not be caught by the runtime guard, and the `debug_assert!` only fires in debug builds. The FTS5 MATCH query at line 83 is `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)` — if `normalized` contained `)`, it could close the implicit grouping early.
- **Suggested fix:** Broaden the runtime guard to assert full sanitization: after calling `sanitize_fts_query`, verify idempotence by calling it again and checking the result is unchanged — `debug_assert_eq!(sanitize_fts_query(&normalized), normalized, "sanitize_fts_query must be idempotent")`. For release builds, add a broader runtime guard: check for any character in `"*()+^:-` or the words OR/AND/NOT before constructing the FTS query.

## Resource Management

#### RM-7: `CommandContext::open_readonly` opens with write-capable `open_light` — full 256MB mmap and 4-connection pool for read-only commands
- **Difficulty:** easy
- **Location:** src/cli/store.rs:40-42, src/store/mod.rs:259-270
- **Description:** `open_project_store_readonly()` is called for every Group B CLI command (callers, callees, deps, stats, brief, dead, notes list, reconstruct, stale, health, impact, etc.) and is documented as "for read-only commands." It calls `Store::open_light`, which opens with `read_only: false`, `max_connections: 4`, `mmap_size: 256MB`, and `cache_size: 16MB`. `Store::open_readonly` already exists with `read_only: true`, `max_connections: 1`, `mmap_size: 64MB`, `cache_size: 4MB` and is used for reference stores. Simple graph commands such as `callers`, `deps`, `stats`, `brief`, `reconstruct`, and `dead` perform only lightweight SQLite key lookups and do not benefit from the 4-connection pool or 256MB mmap hint. The name ("readonly") contradicts the actual open mode (`read_only: false`): the WAL write connection is held open for the entire command even though no writes occur. On systems with memory limits, the 256MB mmap reservation adds pressure even though it is demand-paged.
- **Suggested fix:** Split Group B commands into two sub-groups: search commands that benefit from the larger mmap (search, gather, onboard, scout, query, similar, where, explain, plan, task) keep `open_light`; pure graph/structural commands (callers, callees, deps, dead, stats, brief, reconstruct, stale, health, impact, trace, test-map, affected, blame) use `Store::open_readonly` via a new `open_project_store_light_readonly()` helper. Alternatively, add a `needs_search: bool` flag to `CommandContext::open_readonly` to select the opener.

#### RM-8: `cmd_notes add/update/remove` opens two store connections simultaneously
- **Difficulty:** easy
- **Location:** src/cli/dispatch.rs:161, src/cli/commands/io/notes.rs:144-156
- **Description:** All `Notes` subcommands go through Group B dispatch (line 201), which calls `CommandContext::open_readonly` at line 161 — opening store #1 via `open_light` (256MB mmap, 16MB cache, 4 connections, single-threaded tokio runtime). For the `add`, `update`, and `remove` subcommands, `cmd_notes` passes only `ctx.cli` to the handler, not `ctx.store`. After writing notes.toml, the handler calls `reindex_notes_cli(&root)` which calls `Store::open` — store #2 with full multi-threaded runtime, 4 connections, 256MB mmap. Both stores coexist for the duration of the reindex. Store #1 is never used by `add/update/remove`; `ctx.store` is only accessed by `cmd_notes_list --check`. Two pools of 4 connections each and two 256MB mmap reservations for a simple `cqs notes add`.
- **Suggested fix:** Move `NotesCommand::Add`, `Update`, `Remove` to Group A in dispatch (before `CommandContext` is created), so they call no-store handlers directly. They already use `find_project_root()` independently and do not touch `ctx.store`. `cmd_notes_list` (which may need `ctx.store` for `--check`) remains in Group B.

#### RM-9: `store_stage` deferred Vecs accumulate all call-sites and type-edges in memory for the full index run before a single flush
- **Difficulty:** medium
- **Location:** src/cli/pipeline/upsert.rs:30-31, 42, 85-89
- **Description:** `store_stage` maintains two `Vec`s that grow for the entire duration of indexing: `deferred_chunk_calls` (one entry per call site across every chunk in every file) and `deferred_type_edges` (one `(PathBuf, Vec<ChunkTypeRefs>)` per file). Both are flushed in a single pass at the end of `store_stage`. The rationale is correct — FK constraints require all chunks to be present before inserting call references — but there is no cap on how large these Vecs grow. For a monorepo with 100,000 chunks averaging 20 call sites each, `deferred_chunk_calls` holds 2,000,000 `(String, CallSite)` entries. Each `CallSite` has a `String` callee_name (~16-64 bytes) and a `u32`, plus the caller-id `String` (~32 bytes): roughly 50-100 bytes per entry = 100-200MB before the final flush. `deferred_type_edges` compounds this with one `Vec<ChunkTypeRefs>` per file, each containing a `Vec<TypeRef>` with per-type `String` names. The channel depth limit of `EMBED_CHANNEL_DEPTH=64` bounds the embedding queue but does not bound these deferred Vecs, which grow proportional to total codebase size.
- **Suggested fix:** The FK constraint only requires all chunks in the same file-batch to be committed before their call edges. Since `parser_stage` processes files in `FILE_BATCH_SIZE=5_000`-file batches, flush `deferred_chunk_calls` and `deferred_type_edges` after each file-batch boundary rather than at end-of-all-batches. This bounds the deferred Vecs to at most one file-batch worth of data (5,000 files) instead of the entire codebase.

#### RM-10: `cmd_telemetry --all` loads all archived telemetry into a single unbounded `Vec<Entry>` before processing
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:296-315
- **Description:** `cmd_telemetry --all` calls `fs::read_dir` to collect all `telemetry*.jsonl` files, then calls `parse_entries` on each, extending a single `Vec<Entry>` with all results before calling `build_telemetry`. `parse_entries` uses `BufReader::lines()` (streaming per-line parse), but all `Entry` structs are materialized into the Vec before any aggregation. A heavy long-term user accumulating many archived files (SHL-20 notes that a 10,000-command batch session generates ~5MB) could have the Vec hold 50,000+ `Entry` values simultaneously. Each `Entry::Command` contains a `String` cmd, `Option<String>` query, and `u64` ts — roughly 64-256 bytes each with heap allocation. There is no count limit, no size warning, and no streaming aggregation. `build_telemetry` only needs aggregation maps (command counts, query counts, timestamps) and does not require the raw `Vec<Entry>` to persist in memory.
- **Suggested fix:** Refactor `build_telemetry` to accept an incremental accumulator so entries can be processed one at a time without materializing the full Vec. Alternatively, add a hard cap and warn after exceeding 1,000,000 entries.

#### RM-11: `cmd_telemetry_reset` loads entire telemetry file into a heap `String` just to count lines, with silent error swallowing
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:440-443
- **Description:** `cmd_telemetry_reset` counts lines with `fs::read_to_string(&current).unwrap_or_default().lines().count()`. This allocates the entire file as a heap `String`, iterates its lines, then discards the String. The file is then separately copied with `fs::copy`. Two problems: (1) the file is in memory as an allocated String while the OS also buffers it for `fs::copy`; (2) `unwrap_or_default()` silently converts any I/O error (permissions failure, file removed between the `exists()` check and `read_to_string`) into a 0 line count with no warning. The `parse_entries` function in the same file already demonstrates the correct pattern: `BufReader::lines()` for streaming line-by-line processing.
- **Suggested fix:** Replace `fs::read_to_string(&current).unwrap_or_default().lines().count()` with `BufReader::new(File::open(&current)?).lines().count()`. This eliminates the heap allocation and propagates I/O errors via `?` instead of silently returning 0.

## Happy-Path Test Coverage

#### HP-1: `context.rs` has zero unit tests despite eight typed output structs and four shared builder functions
- **Difficulty:** easy
- **Location:** src/cli/commands/io/context.rs (whole file)
- **Description:** `context.rs` defines `CompactOutput`, `CompactChunkEntry`, `FullOutput`, `FullChunkEntry`, `ExternalCallerEntry`, `ExternalCalleeEntry`, `SummaryOutput`, and `SummaryChunkEntry` — eight typed serializable structs. The four builder functions `compact_to_json`, `full_to_json`, `summary_to_json`, and `pack_by_relevance` are used by both CLI and batch handlers. None of these structs or functions has a single unit test. The only coverage is the integration test `test_context_json` which checks only two fields (`file`, `chunks.len()`). Notably absent from any test: the `lines: [u32; 2]` array format (flagged separately in AD-12), `caller_count`/`callee_count` in compact mode, `external_callers`/`external_callees`/`dependent_files` in full mode, and the `token_count`/`token_budget` optional fields in `FullOutput`.
- **Suggested fix:** Add a `#[cfg(test)] mod tests` block with: (1) `compact_to_json` serialization covering all fields including the `lines` array, (2) `full_to_json` with `external_callers`, `external_callees`, `dependent_files`, and token info fields, (3) `summary_to_json` with counts, (4) `full_to_json` with `content_set = Some(...)` verifying included vs excluded chunks.

#### HP-2: `inject_token_info`, `inject_content_into_scout_json`, and `inject_content_into_onboard_json` have zero tests
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:174, 196, 288
- **Description:** Three shared JSON mutation helpers used by scout (CLI and batch), onboard (batch), and gather (batch) have no tests in the `mod.rs` test block. `inject_token_info` is called from 4 production sites. `inject_content_into_scout_json` mutates `file_groups[].chunks[].content` by name lookup. `inject_content_into_onboard_json` mutates `entry_point.content`, `call_chain[].content`, and `callers[].content`. Regressions in JSON key names (e.g., from a refactor that renames `file_groups` to `files`) would produce silent no-ops — the mutation quietly finds no matching key and returns without error, and no test would catch the empty output.
- **Suggested fix:** Add tests for: (1) `inject_token_info(json, Some((100, 500)))` verifies `json["token_count"] == 100` and `json["token_budget"] == 500`, (2) `inject_token_info(json, None)` is a no-op, (3) `inject_content_into_scout_json` with a known `file_groups` shape verifies content injection by name, (4) `inject_content_into_scout_json` with unknown names leaves JSON unchanged, (5) `inject_content_into_onboard_json` injects into `entry_point`, `call_chain`, and `callers`.

#### HP-3: `run_git_diff` argument injection guard is untested
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:469-476
- **Description:** `run_git_diff` validates the `base` ref with two guards: `b.starts_with('-')` (prevents flag injection) and `b.contains('\0')` (prevents null-byte injection). Neither guard has a unit test. The integration tests for `impact-diff` and `ci` only exercise the no-`base` path. A future change that accidentally weakens these guards would go undetected until a security audit.
- **Suggested fix:** Extract the validation into a testable `validate_git_ref(base: &str) -> anyhow::Result<()>` function and add tests for: (1) `base = "-f"` returns `Err`, (2) `base = "main\0injected"` returns `Err`, (3) `base = "main"` returns `Ok`, (4) `base = "HEAD~1"` returns `Ok`.

#### HP-4: Batch integration tests cover 5 of ~30 batch commands; no JSON field assertions beyond basic presence checks
- **Difficulty:** medium
- **Location:** tests/cli_batch_test.rs (whole file)
- **Description:** The 10 batch integration tests exercise only `callers`, `callees`, `explain`, `stats`, and `dead`. The following batch commands have no integration test at any level: `context`, `brief`, `onboard`, `similar`, `read`, `blame`, `trace`, `test-map`, `deps`, `affected`, `drift`, `reconstruct`, `impact`, `scout`, `gather`, `where`, `diff`, `gc`, `health`, `stale`, `suggest`, `ci`, `impact-diff`, `related`, `notes`, `plan`, `task`. For the 5 covered commands, field assertions are minimal: `stats` checks only `total_chunks`, `dead` checks only `dead` or `total_dead`, `explain` checks only `callers` and `callees`. No test verifies full field sets, type correctness, or the JSONL framing. The divergent schemas identified in AD-14 (batch context summary vs CLI) and CQ-NEW-5 (batch similar vs CLI) went undetected precisely because there are no batch integration tests for those commands.
- **Suggested fix:** Add batch integration tests for at minimum: `context`, `onboard`, `gather`, `impact`, `test-map`, `trace`, and `scout`. Each test should: (1) verify the output is valid JSON, (2) assert the key top-level fields are present with correct types, (3) compare field names against the equivalent CLI `--json` output to catch schema divergence.

#### HP-5: `test_context_json` integration test checks only 2 of many output fields
- **Difficulty:** easy
- **Location:** tests/cli_graph_test.rs:382-392
- **Description:** `test_context_json` asserts `parsed["file"] == "src/lib.rs"` and `chunks.len() >= 4`. It does not verify: `chunk_count` (a separate field from `chunks` array length), any field within a chunk entry (`name`, `chunk_type`, `signature`, `lines`, `caller_count`, `callee_count`), or the `external_callers`/`external_callees`/`dependent_files` arrays. The `lines: [u32; 2]` array format (AD-12) is completely unverified — agents discovering that `result["lines"][0]` is needed instead of `result["line_start"]` only learn this by trial and error. The test would pass even if `chunks` contained empty objects.
- **Suggested fix:** Add assertions for at least one chunk's field names and types: `chunks[0]["name"].is_string()`, `chunks[0]["lines"].is_array()`, `chunks[0]["caller_count"].is_number()`. Add `assert!(parsed["external_callers"].is_array())` and `assert_eq!(parsed["chunk_count"].as_u64().unwrap(), chunks.len() as u64)`. Add a separate integration test for `context --compact --json` verifying the compact field shape.

#### HP-6: `GcOutput` serialization test asserts only 3 of 8 fields
- **Difficulty:** easy
- **Location:** src/cli/commands/index/gc.rs:192-207
- **Description:** `test_gc_output_serialization` constructs a `GcOutput` with all 8 fields set but only asserts `pruned_chunks`, `hnsw_rebuilt`, and `hnsw_vectors`. It does not assert: `stale_files`, `missing_files`, `pruned_calls`, `pruned_type_edges`, or `pruned_summaries`. A rename of any of these five fields during schema normalization would not be caught. The integration test `test_gc_json_on_clean_index` at `tests/cli_test.rs:408` checks only `json.get("pruned_chunks").is_some()`.
- **Suggested fix:** Assert all 8 fields in `test_gc_output_serialization`. Add a second integration test verifying `pruned_calls`, `pruned_type_edges`, and `pruned_summaries` are present in the JSON output after GC runs on an index that has been modified.

#### HP-7: `cmd_query` and `display_similar_results_json` have no unit tests; query JSON schema is unverified beyond presence of `results` array
- **Difficulty:** medium
- **Location:** src/cli/commands/search/query.rs (whole file), src/cli/display.rs:403-432
- **Description:** `cmd_query` has no `#[cfg(test)]` block. The `TODO(json-schema)` comment at `query.rs:1-7` acknowledges a known gap. The search integration test `test_search_json_output` at `tests/cli_test.rs:213` only checks `parsed["results"].is_array()`. For `similar`, `display_similar_results_json` at `display.rs:403` constructs a 9-field JSON object inline with no tests. The schema mismatch between `cmd_similar` (9 fields via display) and `dispatch_similar` (3 fields in batch) documented in CQ-NEW-5 is undetected by any test. No test verifies `line_start` vs `line` or `chunk_type` in search JSON output.
- **Suggested fix:** Add an integration test for `cqs search "query" --json` asserting at minimum: `results[0]["name"].is_string()`, `results[0]["line_start"].is_number()`, and `results[0].get("line").is_none()`. Add a unit test for `display_similar_results_json` field names. Track progress against the `TODO(json-schema)` with a concrete issue.

#### HP-8: `CommandContext` lazy-init and OnceLock reuse behavior have no tests
- **Difficulty:** medium
- **Location:** src/cli/store.rs:47-109
- **Description:** `CommandContext` has no `#[cfg(test)]` block. The `embedder()` and `reranker()` methods use `OnceLock` for lazy initialization. TC-11 covers the failure path. What is additionally missing: (1) no test that a second call to `embedder()` returns the same cached instance (OnceLock reuse), (2) no test that `build_vector_index_with_config` returns `Ok(None)` when `is_hnsw_dirty()` is true, (3) no test for the `chunk_count` fallback branch in the CAGRA threshold check. All coverage is indirect through full binary integration tests, making it impossible to test error branches without faking filesystem state.
- **Suggested fix:** Add a unit test for `build_vector_index_with_config` using a store with no HNSW file present and `is_hnsw_dirty = false` — verify `Ok(None)` is returned when no HNSW file is present. For the dirty flag case, use a store that returns `Ok(true)` from `is_hnsw_dirty` and verify the function returns `Ok(None)`.

#### HP-9: `onboard_scored_names` and `scout_scored_names` score computation logic has no tests
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:232, 247
- **Description:** `onboard_scored_names` assigns scores: entry point gets 1.0, call chain entries get `1.0 / (depth as f32 + 1.0)`, callers get 0.3. `scout_scored_names` multiplies `group.relevance_score * chunk.search_score`. Neither function has a single test. These score values drive `token_pack` priority ordering — if a formula change (e.g., using `depth` instead of `depth + 1`) were introduced, entry-point content could be deprioritized and excluded from token budgets silently. The divergence between CLI and batch onboard behavior (CQ-NEW-4) exists partly because the shared scoring path has no test anchoring the contract.
- **Suggested fix:** Add tests: (1) `onboard_scored_names` with an `OnboardResult` having a depth=0 entry point, a depth=1 call chain entry, and one caller — verify scores are 1.0, 0.5, and 0.3 respectively, (2) `scout_scored_names` with `relevance_score=0.8` and `search_score=0.5` — verify the combined score is 0.4.

## Performance

#### PF-1: `map_hunks_to_functions` issues N sequential DB queries — one per changed file
- **Difficulty:** easy
- **Location:** src/impact/diff.rs:40-48
- **Description:** `map_hunks_to_functions` groups hunks by file and then loops over unique files, issuing one `store.get_chunks_by_origin(&normalized)` call per file inside the loop. Each call executes `block_on(async { sqlx::query(...).fetch_all(...).await })` — a synchronous barrier per file. For a large diff touching N unique files, this is N sequential round trips to SQLite. `get_chunks_by_origins_batch` already exists at `src/store/chunks/query.rs:158` for exactly this purpose and is already used in `src/impact/analysis.rs:308` and `src/cli/commands/train/train_pairs.rs:122`. The `review`, `ci`, and `impact-diff` commands all go through `map_hunks_to_functions`, so any large-diff review pays the N+1 cost. A diff touching 50 files issues 50 sequential DB queries instead of one.
- **Suggested fix:** Before the file loop, collect all unique normalized origins into a `Vec<String>` and call `store.get_chunks_by_origins_batch(&origin_refs)?` once. Store the result in a `HashMap<String, Vec<ChunkSummary>>` keyed by origin string. Replace the per-file `get_chunks_by_origin` call in the loop body with a lookup into this map. This reduces N sequential synchronous DB calls to one batched query using the existing API.

#### PF-2: `rrf_fuse` reads `CQS_RRF_K` from the environment on every search query
- **Difficulty:** easy
- **Location:** src/store/search.rs:130-133
- **Description:** `rrf_fuse` is called on every search where RRF is enabled (the default for all agent-facing searches). On each call it reads `std::env::var("CQS_RRF_K")`, parses the string, and falls back to `60.0`. In batch mode with hundreds of queries per session, this is hundreds of syscalls and string parses that almost always return the default. The existing codebase pattern for tunable env-var constants is `OnceLock` with an accessor function — see `max_nb_connection()`, `ef_construction()`, `ef_search()` in `src/hnsw/mod.rs:69-102`, and `bfs_max_nodes()` in `src/impact/bfs.rs`. `rrf_fuse` is the only hot-path function that reads an env var unconditionally without caching.
- **Suggested fix:** Extract a `fn rrf_k() -> f32` function using `OnceLock<f32>` that reads `CQS_RRF_K` once on first call and returns the cached value on subsequent calls. Pattern to follow: `src/hnsw/mod.rs:69-78`. Call `rrf_k()` instead of the inline `std::env::var` block in `rrf_fuse`.

#### PF-3: `impact_to_json` serializes `ImpactResult` to `Value` then immediately mutates it to insert computed counts
- **Difficulty:** easy
- **Location:** src/impact/format.rs:10-27
- **Description:** `impact_to_json` calls `serde_json::to_value(result)` which fully serializes `ImpactResult` (including all `callers`, `tests`, and `type_impacted` vecs) into a `serde_json::Value` tree, then immediately calls `as_object_mut()` and inserts `caller_count`, `test_count`, and `type_impacted_count` — which are just `result.callers.len()`, `result.tests.len()`, and `result.type_impacted.len()`. These values were available before serialization began. The `as_object_mut().insert()` at line 15 also silently no-ops if `serde_json::to_value` returns the `json!({})` fallback on error, leaving the count fields absent in the error case with no observable signal. This was deferred when PR #783 migrated `ImpactResult` to typed `Serialize` because the count injection was not yet moved into the struct (see also CQ-NEW-6).
- **Suggested fix:** Add `caller_count`, `test_count`, and `type_impacted_count` as serialization-time computed fields to `ImpactResult`. The cleanest approach is a custom `Serialize` impl or a thin output wrapper struct that captures the counts before serializing. Once the struct serializes the count fields natively, `impact_to_json` reduces to a `serde_json::to_value(result)` wrapper and can be deleted (as noted in CQ-NEW-6). This also eliminates the silent no-op risk on error.

#### PF-4: `merge_results` recomputes `blake3::hash` on content strings — stored `content_hash` is ignored
- **Difficulty:** easy
- **Location:** src/reference.rs:235-239
- **Description:** `merge_results` deduplicates cross-index results by content hash. For every result in the combined list it computes `blake3::hash(r.chunk.content.as_bytes())`. `ChunkSummary` already stores `content_hash: String` (a blake3 hex string) at `src/store/helpers/types.rs:42`, computed at index time and persisted in SQLite. Re-hashing at merge time is redundant. For a search returning 10 results from 3 reference indexes, this is 40 blake3 hash computations over potentially large content strings. The `content_hash` field is directly accessible as `r.chunk.content_hash` at the call site.
- **Suggested fix:** Replace `blake3::hash(r.chunk.content.as_bytes())` with `r.chunk.content_hash.as_str()`. Use `HashSet<String>` or `HashSet<&str>` (borrowing from the `content_hash` fields of the tagged results via their `SearchResult`) instead of `HashSet<blake3::Hash>`. This eliminates all rehash computations and reduces dedup to string equality checks on already-computed hex strings.

## Data Safety

#### DS-NEW-1: HNSW shared lock released at end of `load_with_dim` — concurrent writer can overwrite files while index is in use
- **Difficulty:** medium
- **Location:** src/hnsw/persist.rs:403-409, 534
- **Description:** `HnswIndex::load_with_dim` acquires a shared file lock (`lock_file.lock_shared()`) at line 409, performs checksum verification and deserialization, then returns `Ok(Self { ... })` at line 534. The `lock_file` is a local variable — it is **dropped at the point the `Ok(Self {...})` value is constructed**, releasing the shared lock before the caller receives the `HnswIndex` value. Once `load_with_dim` returns, the caller holds an `HnswIndex` with no accompanying file lock. A concurrent `cqs index` (or `cqs watch`) can then acquire the exclusive write lock, rename the temp dir over the live files, and replace the HNSW files on disk while searches are active. The in-memory loaded HNSW graph is unaffected (it owns its data via `LoadedHnsw`), but the `id_map` (a `Vec<String>`) was deserialized from the file while the file was locked, then owned in memory — this is safe. The real gap is the TOCTOU between `verify_hnsw_checksums` (line 414, under the lock) and the actual deserialization from disk (line 518, also under the lock), which are both within the `load_with_dim` scope and thus correctly protected. However, the lock intent stated in the comment ("prevents concurrent cqs processes from corrupting the index") is misleading: the lock is dropped before the returned index is ever searched. If a concurrent writer starts while a search is executing (i.e., after `load_with_dim` returns), the advisory lock provides no protection. On WSL/NTFS this lock is advisory-only anyway. In practice the risk is limited because: (a) the in-memory data is already fully owned and not re-read from disk during search, and (b) the `hnsw_dirty` flag causes readers to skip the HNSW index entirely after a write. But the lock comment creates false confidence that concurrent access is guarded during the search lifetime.
- **Suggested fix:** Either (1) document clearly that the lock only protects the load operation itself (not subsequent searches), and update the comment at line 398-401 to say "prevents concurrent processes from corrupting the index *during load*" — or (2) store the `lock_file` in `HnswIndex` (as `_lock: Option<std::fs::File>`) so the shared lock is held for the index's entire lifetime. Option 2 would block exclusive writers while the loaded index is alive, which is the semantically correct behavior. Given WSL advisory-only locking and the fact that the in-memory index is self-contained, option 1 (accurate documentation) is lower-risk than option 2 (which could cause deadlocks if the same process tries to save and then load).

#### DS-NEW-2: `cmd_telemetry_reset` uses non-atomic `fs::write` without file lock — concurrent `log_command` loses events
- **Difficulty:** easy
- **Location:** src/cli/commands/infra/telemetry_cmd.rs:448, 463; src/cli/telemetry.rs:41
- **Description:** `cmd_telemetry_reset` operates on `telemetry.jsonl` in three steps: (1) `fs::copy(&current, &archive)` — archives the current file, (2) constructs a reset-event JSON string, (3) `fs::write(&current, ...)` — truncates the file and writes only the reset event. Meanwhile, `log_command` in `telemetry.rs:41` opens the same file with `OpenOptions::new().append(true).open(&path)` and writes to it. These two paths share no file lock. A race window exists between steps 1 and 3 of reset: if `log_command` runs a concurrent `cqs` command (e.g., `cqs watch` logging its periodic search) and appends a line after `fs::copy` completes but before `fs::write` runs, that line is not captured in the archive and is then overwritten by `fs::write`. The event is silently lost. On Linux, POSIX `O_APPEND` writes are atomic at the OS level for small writes (a single `write()` syscall), but `fs::write` is a `truncate(0)` + `write()` pair — not atomic. There is no `flock`, `fcntl`, or atomic rename protecting either path.
- **Suggested fix:** In `cmd_telemetry_reset`, replace the `fs::copy` + `fs::write` sequence with an atomic archive-and-reset using `std::fs::rename`. Procedure: (1) open the current file with `OpenOptions::read(true)` and call `lock_exclusive()`, (2) count lines while holding the lock, (3) copy to archive path, (4) truncate the locked file and write only the reset event (use `file.set_len(0)` + `file.write_all(...)`), (5) release the lock. This keeps the lock held across all three operations, preventing concurrent `log_command` appends from racing the truncate.

#### DS-NEW-3: `test_embed_batch_size` mutates `CQS_EMBED_BATCH_SIZE` without a mutex — other tests that call `embed_batch_size()` can observe corrupt env state
- **Difficulty:** easy
- **Location:** src/cli/pipeline/mod.rs:476-497
- **Description:** `test_embed_batch_size` (marked with a comment "must run sequentially") combines its sub-cases into a single `#[test]` function to avoid racing itself. However, it holds no mutex. Other tests in the same binary that call `embed_batch_size()` (directly or indirectly via `run_index_pipeline` or `embed_batched`) run in parallel via Rust's default test runner. During the `set_var("CQS_EMBED_BATCH_SIZE", "not_a_number")` or `set_var("...", "0")` phases, any other test reading `embed_batch_size()` sees the bogus value and may use it in pipeline size calculations. The pattern used elsewhere — `embedder/models.rs:322` defines `static ENV_MUTEX: Mutex<()>` and every model test holds it — is the correct fix. Notably, `embed_batch_size()` does NOT use `OnceLock` (it re-reads the env var on every call), so there is no "cached before the test" protection. A separate concern: `config.rs:tc37_embedding_config_empty_string_model` (line 1116) calls `env::remove_var("CQS_EMBEDDING_MODEL")` without any lock while `embedder/models.rs` tests use `ENV_MUTEX` for the same variable. Tests from these two modules can race across module boundaries.
- **Suggested fix:** Add `static BATCH_ENV_MUTEX: Mutex<()> = Mutex::new(());` in `pipeline/mod.rs` tests and hold `let _lock = BATCH_ENV_MUTEX.lock().unwrap();` in `test_embed_batch_size` for the entire function body. For `config.rs:tc37`, either (a) add a guard matching the `ENV_MUTEX` in `models.rs` (requires making it `pub(crate)`), or (b) replace the `env::remove_var` with a check that the env var is absent (`assert!(std::env::var("CQS_EMBEDDING_MODEL").is_err(), ...)`) without mutating it, relying on the test environment not having the var set.

#### DS-NEW-4: `cached_notes_summaries` releases read lock before acquiring write lock — double-population race
- **Difficulty:** easy
- **Location:** src/store/metadata.rs:282-300
- **Description:** `cached_notes_summaries` uses a double-checked lock pattern with `RwLock`. It acquires a read lock to check the cache (lines 283-289), drops the read lock, then acquires a write lock to populate the cache on miss (lines 294-298). Between the read-unlock and the write-lock, a concurrent thread can also observe a cache miss and call `list_notes_summaries()`. Both threads then acquire the write lock sequentially and the second writer's result silently overwrites the first. For notes (immutable `Vec<NoteSummary>`), this is a benign double-population: the same data is written twice with no corruption. However, the `Arc::clone` returned to the first caller remains valid even after the second writer replaces the cache entry, because `Arc` keeps the first allocation alive. The issue is wasted work (two `list_notes_summaries()` DB queries) rather than data corruption. The pattern is documented in the literature as "correct but wasteful." A more subtle concern: if `invalidate_notes_cache` runs between the read-lock and write-lock, the second thread's write re-populates the cache with **pre-invalidation data**, masking the invalidation. This can cause stale notes to be served after `cmd_notes add/update/remove` runs `invalidate_notes_cache()`.
- **Suggested fix:** Use `RwLock::upgradable_read` (if the lock type supports it) or restructure to use a `Mutex<Option<Arc<Vec<NoteSummary>>>>` for simple write-once semantics. Alternatively, document the benign double-population case and add a comment explaining why `invalidate_notes_cache` racing with `cached_notes_summaries` is acceptable. If stale reads after invalidation are possible, upgrade to a `Mutex` or use `write()` lock for the entire check-and-populate sequence (accepting reduced read concurrency, which is fine for the infrequently-called `cached_notes_summaries`).
