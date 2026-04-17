# Audit Findings — v1.0.0

Generated 2026-03-12. 14-category audit across 3 batches.

## Documentation

#### DOC-1: Model download size stale in SECURITY.md and PRIVACY.md
- **Difficulty:** easy
- **Location:** SECURITY.md:39, PRIVACY.md:27
- **Description:** Both files state the embedding model download is "~440MB". The actual code (`src/cli/commands/init.rs:45`) prints "Downloading model (~547MB)..." and prior audit notes (docs/AUDIT_2026-01-31.md) confirmed the actual file is 547MB. Two user-facing docs are wrong by ~23%, which matters for users on metered connections or small disks.
- **Suggested fix:** Update both files: `~440MB` → `~547MB`.

#### DOC-2: `store/mod.rs` module structure comment missing `types` and `migrations` submodules
- **Difficulty:** easy
- **Location:** src/store/mod.rs:6-11
- **Description:** The module-level doc comment lists four submodules (`helpers`, `chunks`, `notes`, `calls`) but omits `types` (type edge storage, added in schema v11) and `migrations` (schema migration framework). Both are declared `mod types;` and `mod migrations;` immediately below. A contributor looking at the module overview gets an incomplete picture.
- **Suggested fix:** Add two lines: `- \`types\` - Type edge storage and queries` and `- \`migrations\` - Schema migration framework`.

#### DOC-3: README config example missing `ef_search`, `stale_check`, and `note_only` options
- **Difficulty:** easy
- **Location:** README.md:112-128
- **Description:** The example `.cqs.toml` block shows six config keys (`limit`, `threshold`, `name_boost`, `note_weight`, `quiet`, `verbose`). Three additional config keys supported by `src/config.rs` are absent: `ef_search` (added v1.0.0, controls HNSW search accuracy — the only HNSW parameter that's user-tunable), `stale_check` (disable per-file staleness checks on NFS), and `note_only` (default to note-only search). `ef_search` is especially relevant because the HNSW Tuning section (README:487-503) advises users to tune it but doesn't show how.
- **Suggested fix:** Add the three missing config keys to the example block with comments matching existing style.

#### DOC-4: `--no-demote` search flag undocumented in README
- **Difficulty:** easy
- **Location:** README.md (Filters section), src/cli/mod.rs:263-265
- **Description:** The CLI accepts `--no-demote` (disables search-time score demotion of test functions and underscore-prefixed names). It is defined in `Cli` struct with a clear help string but appears nowhere in README. Users debugging unexpected ranking or auditing the index would benefit from knowing this flag exists.
- **Suggested fix:** Add `cqs --no-demote "query"  # Disable demotion of test/private functions` to the Filters section.

#### DOC-5: SECURITY.md Write Access table missing `.cqs/audit-mode.json`
- **Difficulty:** easy
- **Location:** SECURITY.md:63-72
- **Description:** The Write Access table lists seven paths written by cqs. `cqs audit-mode on/off` writes `.cqs/audit-mode.json` (`src/audit.rs:106`), which is entirely absent from the table. This file persists audit mode state across sessions and is written on every `cqs audit-mode` invocation.
- **Suggested fix:** Add a row: `| \`.cqs/audit-mode.json\` | Audit mode state | \`cqs audit-mode on/off\` |`.

#### DOC-6: `cqs completions` command not documented in README
- **Difficulty:** easy
- **Location:** README.md (no mention), src/cli/mod.rs:326-331
- **Description:** `cqs completions <shell>` generates shell completions (bash, zsh, fish, etc.) via `clap_complete`. It is a real, user-facing command defined in `Commands::Completions` but appears nowhere in README, CONTRIBUTING.md, or any user-facing doc. Users setting up shell integrations have no documented way to discover this.
- **Suggested fix:** Add a brief mention (e.g., `cqs completions bash >> ~/.bashrc`) to the Install or Quick Start section.

#### DOC-7: `store/mod.rs` module-level comment says "sqlx async with sync wrappers" — misleading since v0.x refactor
- **Difficulty:** easy
- **Location:** src/store/mod.rs:1-4
- **Description:** The module doc says "Provides sync methods that internally use tokio runtime to execute async sqlx operations." This accurately describes the implementation but is potentially confusing for library consumers who see `Store` as a synchronous API: it implies internal async machinery that callers shouldn't need to know about. More importantly, the tokio runtime is created and owned inside `Store::open` — this is a non-obvious design choice that affects users trying to call `Store` from inside an existing async runtime (they'd get a "cannot start a runtime from within a runtime" panic). The doc comment should warn about this constraint.
- **Suggested fix:** Add a note: "⚠ `Store` creates an internal `tokio::Runtime`. Do not call from within an existing async runtime context — use `block_on` or spawn a blocking task instead."

## Code Quality

#### CQ-1: `resolve_reference_store` and `resolve_reference_store_readonly` are near-identical
- **Difficulty:** easy
- **Location:** src/cli/commands/resolve.rs:44-99
- **Description:** Both functions are 25 lines of identical logic: load config, find ref by name, check `index.db` exists, open store. The only difference is the final call (`Store::open` vs `Store::open_readonly`). The shared body is duplicated 1:1, including identical error messages and the same bail! strings. Two callers exist: `cmd_diff` uses `resolve_reference_store` and `cmd_drift` uses `resolve_reference_store_readonly`.
- **Suggested fix:** Extract a private `resolve_reference_db_path(root, ref_name) -> Result<PathBuf>` that returns the validated path. Both public functions call it, then diverge only on which `Store::open*` variant to use.

#### CQ-2: Random temp-file suffix pattern duplicated 5 times
- **Difficulty:** easy
- **Location:** src/audit.rs:115, src/config.rs:333, src/config.rs:406, src/note.rs:238, src/project.rs:84
- **Description:** Every atomic-write site computes a random suffix with the same 3-line idiom: `std::collections::hash_map::RandomState::new().build_hasher().finish()` followed by `format!("{:016x}.tmp", suffix)`. This pattern is copy-pasted across 5 files. It also imports `std::hash::{BuildHasher, Hasher}` in each file just for this one use.
- **Suggested fix:** Add a `fn random_hex_suffix() -> String` helper in `src/lib.rs` or a new `src/util.rs`. Each write site calls it and drops the redundant imports.

#### CQ-3: `DeadConfidence` → `&str` mapping repeated in 3 locations
- **Difficulty:** easy
- **Location:** src/cli/commands/dead.rs:43, src/ci.rs:109, src/cli/batch/handlers.rs:698
- **Description:** Three independent `match` arms convert `DeadConfidence::{High,Medium,Low}` to the strings `"high"`, `"medium"`, `"low"`. `dead.rs` defines a local `fn confidence_label(c: DeadConfidence) -> &'static str`; `ci.rs` and `batch/handlers.rs` repeat the same match inline. All three must be kept in sync if a new variant is added.
- **Suggested fix:** Add `fn as_str(&self) -> &'static str` (or `impl Display`) on `DeadConfidence` in `src/store/calls.rs`. Remove the three local impls.

#### CQ-4: SQLite IN-clause placeholder builder duplicated ~20 times across store/
- **Difficulty:** medium
- **Location:** src/store/chunks.rs (10+ sites), src/store/calls.rs (5+ sites), src/store/types.rs (3 sites)
- **Description:** Every batch SQL query builds a placeholder string with the same 3-line pattern:
  ```rust
  let placeholders: String = (1..=batch.len())
      .map(|i| format!("?{}", i))
      .collect::<Vec<_>>()
      .join(",");
  ```
  Across the store modules this pattern appears ~20 times. A future developer unfamiliar with SQLx numbered parameters could silently introduce a bug (e.g., using `?` instead of `?1`, or miscounting). The pattern is also the main source of `Vec` allocations in hot store paths.
- **Suggested fix:** Add `pub(super) fn make_placeholders(n: usize) -> String` in `src/store/mod.rs` or `helpers.rs`. All batch query sites call it. Optionally intern common sizes (1–500) with `LazyLock`.

#### CQ-5: `diff.rs` test module duplicates `full_cosine_similarity` tests already in `math.rs`
- **Difficulty:** easy
- **Location:** src/diff.rs:206-237, src/math.rs:81-130
- **Description:** `src/diff.rs` has 5 unit tests (`test_cosine_similarity_identical`, `_orthogonal`, `_opposite`, `_empty`, `_zero_vector`) that test `full_cosine_similarity` from `math.rs`. These are entirely redundant with tests in `math.rs`. The `diff.rs` comment at line 206 is named `test_cosine_similarity_identical` — identical to the `math.rs` test name — confirming it was copy-pasted and not adapted. Two bugs in `full_cosine_similarity` would need to be caught in both places; one fix might update only one test file.
- **Suggested fix:** Remove the 5 duplicate tests from `diff.rs`. Add a single comment `// full_cosine_similarity tests are in src/math.rs` if the rationale needs to be explicit (following the pattern already used in `src/search.rs:1136`).

#### CQ-6: `cmd_query` function is 270 lines handling 4 dispatch paths
- **Difficulty:** medium
- **Location:** src/cli/commands/query.rs:40-303
- **Description:** `cmd_query` dispatches four execution paths (name-only, ref-name-only, ref-only, main) plus cross-cutting concerns (token budgeting, reranking, multi-index merge, parent context resolution, staleness warning). Each path shares the same `filter` construction, `emit_empty_results` call, and display logic. The function is readable but at 270 lines it's the longest in the commands directory and tests cannot unit-test individual dispatch paths without going through the full CLI.
- **Suggested fix:** Extract the main (no-ref, no-name-only) path into `cmd_query_main(cli, store, root, cqs_dir, query, query_embedding, filter) -> Result<()>` to match the existing helper pattern (`cmd_query_ref_only`, `cmd_query_name_only`). This is a structural refactor with no behavior change.

## Error Handling

#### EH-1: `impact/`, `review.rs`, `gather.rs` use `anyhow::Result` in library code
- **Difficulty:** medium
- **Location:** src/impact/analysis.rs:25,75,192,394; src/impact/diff.rs:76,91; src/impact/hints.rs:59; src/review.rs:74,183; src/gather.rs:13
- **Description:** The project convention is `thiserror` for library errors and `anyhow` only in the CLI. These library modules use bare `anyhow::Result`. Notably, `impact/analysis.rs` uses `anyhow::Result` without a `use anyhow` import — it compiles via transitive import only. `AnalysisError` already exists in `lib.rs` for exactly this purpose and is used correctly in `scout.rs`, `task.rs`, `onboard.rs`. These modules predate `AnalysisError` and were never migrated. Impact: library consumers get untyped errors with no variant to match on — can't distinguish "not found" from "store I/O failure".
- **Suggested fix:** Migrate functions to return `Result<T, AnalysisError>` or `Result<T, StoreError>`. `AnalysisError::Store` covers all `StoreError` cases via `#[from]`. `gather.rs` can return `Result<GatherResult, StoreError>` since its only error path is store I/O.

#### EH-2: `health.rs`, `suggest.rs`, `ci.rs` use `anyhow::Result` in library code
- **Difficulty:** medium
- **Location:** src/health.rs:9,53; src/suggest.rs:9,57; src/ci.rs:9
- **Description:** Same violation as EH-1. `pub fn health_check`, `pub fn suggest_notes`, `pub fn run_ci_analysis` all return `anyhow::Result`. All their failure modes are `StoreError` propagations (via `?`). No `anyhow::bail!` or `anyhow::anyhow!` macros are used in these files — the only reason for `anyhow` is to avoid writing the error type. Library consumers (and future callers) get no typed recovery path.
- **Suggested fix:** Change `use anyhow::Result` to use `StoreError` or `AnalysisError` as the error type. For `health.rs` the only non-store failure is `HnswIndex::count_vectors` — which returns `HnswError`, already part of `StoreError::Hnsw` if the store wraps it.

#### EH-3: `cmd_doctor` prints "All checks passed." unconditionally, even on failure
- **Difficulty:** easy
- **Location:** src/cli/commands/doctor.rs:129
- **Description:** `cmd_doctor` runs checks for model, parser, index, and references. Each failure branch prints `[✗]` and continues (soft errors). But the final line always prints "All checks passed." regardless of whether any check failed. A user running `cqs doctor` after a broken install sees success messaging alongside failure markers.
- **Suggested fix:** Track a `let mut any_failed = false;` flag, set it in each `Err` branch, and print either "All checks passed." or "Some checks failed — see [✗] items above." conditionally.

#### EH-4: `reference list` silently swallows `Store::open` errors, shows `0` chunks
- **Difficulty:** easy
- **Location:** src/cli/commands/reference.rs:172-175, 193-196, 274-277
- **Description:** Three `cmd_ref_list` paths (JSON output, text output, diff-add preview) display chunk counts via `Store::open(&path).ok().and_then(|s| s.chunk_count().ok()).unwrap_or(0)`. If the reference store fails to open (corrupt DB, permission error, wrong schema), the user sees `0` chunks with no error indication. This is confusing: a user may think the reference is empty when it's actually inaccessible. Compare with `cmd_doctor` which correctly shows `[✗] name: error`.
- **Suggested fix:** Replace the `.ok()` chain with a `match Store::open(...)` that prints the count on success or annotates with `(error: {e})` on failure, consistent with the doctor command pattern.

#### EH-5: `convert` module silently skips `walkdir` entry errors in 6 locations
- **Difficulty:** easy
- **Location:** src/convert/mod.rs:334,360; src/convert/chm.rs:63,92; src/convert/webhelp.rs:31,65
- **Description:** All walkdir iterations use `.filter_map(|e| e.ok())` — silently discarding directory traversal errors (permission denied, too-deep symlinks, filesystem errors). The outer `convert_file` match correctly logs warnings when individual conversions fail. But traversal errors — which prevent files from being found at all — are silently dropped. `cqs convert /some/dir` can return 0 results with no explanation when the directory is unreadable.
- **Suggested fix:** Replace `.filter_map(|e| e.ok())` with `.filter_map(|e| e.map_err(|err| tracing::warn!(error = %err, "Directory traversal error")).ok())`. No behavior change — just visibility for errors that currently disappear.

#### EH-6: `audit.rs` swallows JSON parse error with bare `Err(_)`, no log message
- **Difficulty:** easy
- **Location:** src/audit.rs:75
- **Description:** `load_audit_state` reads `audit-mode.json` and on `serde_json::from_str` failure executes `Err(_) => return AuditMode::default()` — dropping the error value entirely. If the file is corrupt or incompatible, audit mode silently reverts to disabled with no diagnostic. The `expires_at` parse at line 88 correctly uses `tracing::debug!`, making line 75 inconsistent within the same function.
- **Suggested fix:** `Err(e) => { tracing::debug!(error = %e, path = %path.display(), "Failed to parse audit-mode.json, resetting to default"); return AuditMode::default(); }`

#### EH-7: `audit.rs` `parse_duration` drops `ParseIntError` from `map_err(|_| ...)`
- **Difficulty:** easy
- **Location:** src/audit.rs:161, 177, 198
- **Description:** Three `current_num.parse::<i64>()` calls use `.map_err(|_| anyhow!("Invalid number '{}' ..."))` — discarding the underlying `ParseIntError`. The error message includes the input string but not the parse failure reason (e.g., "number too large to fit in target type" vs "invalid digit found in string"). The `|_|` pattern is a red flag: it almost always hides useful diagnostic information.
- **Suggested fix:** Change `map_err(|_| anyhow!("..."))` to `map_err(|e| anyhow!("...: {e}"))` to include the original error cause in the message.

## Observability

#### OB-1: `process_file_changes` has no tracing span
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:295
- **Description:** `process_file_changes` is the central watch-mode orchestrator — it drains the pending file set, calls `reindex_files`, runs incremental or full HNSW rebuild, and records mtimes. It is the most frequently-executed non-trivial function in watch mode, yet has no `info_span!`. When debugging slow watch cycles or HNSW rebuild failures, there is no span to scope the logged events (the `info!`/`warn!` calls inside it appear detached from the enclosing watch loop). All sibling functions (`reindex_files`, `reindex_notes`, `process_note_changes`) have spans.
- **Suggested fix:** Add `let _span = tracing::info_span!("process_file_changes", file_count = files.len()).entered();` immediately after draining `pending_files` on line 307.

#### OB-2: `find_pdf_script` emits the same event to both `eprintln!` and `tracing::warn!`
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:57-58
- **Description:** When `CQS_PDF_SCRIPT` is set, `find_pdf_script` calls `eprintln!("cqs: Using custom PDF script: {}", script)` on line 57, then immediately calls `tracing::warn!(script = %script, "Using custom PDF script from CQS_PDF_SCRIPT env var")` on line 58. This is the only place in the codebase where the same event is emitted twice — once to raw stderr (bypassing the tracing subscriber), once through structured tracing. In machine-readable or piped contexts the `eprintln!` will appear as unstructured output mixed into what could be clean JSON.
- **Suggested fix:** Remove the `eprintln!` on line 57. The `tracing::warn!` already covers the event with a structured field. If terminal visibility is needed, the tracing subscriber at debug/info level already surfaces it.

## API Design

#### AD-1: `file` field type — `String` vs `PathBuf` inconsistent across public types
- **Difficulty:** medium
- **Location:** `src/impact/types.rs:83` (`ChangedFunction`), `src/review.rs:43` (`ReviewedFunction`), `src/ci.rs:44` (`DeadInDiff`), `src/drift.rs:15` (`DriftEntry`), `src/diff.rs:18` (`DiffEntry`), `src/diff_parse.rs:17` (`DiffHunk`)
- **Description:** Most public types use `file: PathBuf` (`CallerDetail`, `TestInfo`, `TransitiveCaller`, `TypeImpacted`, `DiffTestInfo`, `FileSuggestion`, `GatheredChunk`, `ChunkSummary`, `CallerInfo`). Several others use `file: String` (`ChangedFunction`, `ReviewedFunction`, `DeadInDiff`, `DriftEntry`, `DiffEntry`, `DiffHunk`). No consistent rule governs the split — `DriftEntry.file` is a raw DB string while `DiffTestInfo.file` is a `PathBuf` in the same analysis context. Callers must know per-type which representation is used, and path operations (stripping prefixes, joining) work differently on each.
- **Suggested fix:** Standardize on `PathBuf` for all public `file` fields. Types that serialize as relative paths can use `serialize_with = "crate::serialize_path_normalized"` at the boundary. If a type must use `String` for a documented reason, add a doc comment explaining why.

#### AD-2: `chunk_type` field — `String` in some public types, `ChunkType` enum in others
- **Difficulty:** easy
- **Location:** `src/onboard.rs:58` (`OnboardEntry`), `src/drift.rs:17` (`DriftEntry`)
- **Description:** `OnboardEntry.chunk_type` and `DriftEntry.chunk_type` are `String`. `ChunkSummary.chunk_type`, `GatheredChunk.chunk_type`, and `ScoutChunk.chunk_type` use the `ChunkType` enum. For `DriftEntry`, the string is produced by `.to_string()` on a `ChunkType` at `drift.rs:77` — type information is immediately discarded. `OnboardEntry` is assembled from `GatheredChunk` which already holds `ChunkType`.
- **Suggested fix:** Use `ChunkType` in `OnboardEntry.chunk_type` and `DriftEntry.chunk_type`. Add `#[serde(rename_all = "lowercase")]` to `ChunkType` if the lowercase wire format is required.

#### AD-3: `ChunkRole` Serialize inconsistency — PascalCase from derive, snake_case from `as_str()`
- **Difficulty:** easy
- **Location:** `src/scout.rs:15-34`
- **Description:** `ChunkRole` derives `serde::Serialize` (produces `"ModifyTarget"`, `"TestToUpdate"`, `"Dependency"`) but `ChunkRole::as_str()` returns `"modify_target"`, `"test_to_update"`, `"dependency"`. `scout_to_json()` uses `as_str()` (snake_case), but `ScoutChunk` also derives `Serialize`, so `serde_json::to_value(&scout_chunk)` produces PascalCase for `role`. Two public serialization paths for the same field produce different JSON shapes.
- **Suggested fix:** Add `#[serde(rename_all = "snake_case")]` to the `ChunkRole` derive to align with `as_str()`.

#### AD-4: `DeadInDiff.confidence` is `String` when `DeadConfidence` enum already has `Serialize`
- **Difficulty:** easy
- **Location:** `src/ci.rs:46,109-115`, `src/store/calls.rs:30`
- **Description:** `DeadInDiff` stores `confidence: String` via a manual match-to-string mapping in `run_ci_analysis`. The source type `DeadFunction.confidence` is `DeadConfidence` which already derives `serde::Serialize`. The manual mapping produces lowercase strings; `DeadConfidence`'s derived form is PascalCase — a hidden inconsistency. The manual match also requires updating if `DeadConfidence` gains a new variant.
- **Suggested fix:** Change `DeadInDiff.confidence` to `DeadConfidence`. Add `#[serde(rename_all = "lowercase")]` to `DeadConfidence`. Remove the manual string mapping in `run_ci_analysis`.

#### AD-5: `DiffEntry` not re-exported despite being the element type of the public `DiffResult`
- **Difficulty:** easy
- **Location:** `src/lib.rs:104`, `src/diff.rs:14`
- **Description:** `DiffResult` is re-exported from the crate root. Its public fields `added`, `removed`, `modified` are all `Vec<DiffEntry>`. `DiffEntry` is not re-exported. External consumers who receive a `DiffResult` can iterate its fields via type inference but cannot name the type in a function signature, match arm, or explicit `Vec<DiffEntry>` annotation.
- **Suggested fix:** Add `DiffEntry` to the `pub use diff::` re-export in `lib.rs`.

#### AD-6: `review::NoteEntry` not re-exported and name-collides with `note::NoteEntry`
- **Difficulty:** easy
- **Location:** `src/lib.rs:80,94-96`, `src/review.rs:50`, `src/note.rs:45`
- **Description:** `ReviewResult` (re-exported) contains `pub relevant_notes: Vec<review::NoteEntry>`. The crate also re-exports `note::NoteEntry` under the name `NoteEntry`. These are two distinct types: `note::NoteEntry` has `{sentiment, text, mentions}`, `review::NoteEntry` has `{text, sentiment, matching_files}`. A consumer of `ReviewResult.relevant_notes` who tries `cqs::NoteEntry` gets the wrong type. The actual field type cannot be named from outside the crate.
- **Suggested fix:** Rename `review::NoteEntry` to `ReviewNoteEntry` (or `MatchedNote`) and add it to the `pub use review::` re-export in `lib.rs`.

#### AD-7: `FileSuggestion::to_json()` silently omits the `patterns: LocalPatterns` field
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:54-62`
- **Description:** `FileSuggestion` derives `serde::Serialize` (includes all six fields including `patterns`). `to_json(root)` manually constructs JSON with only five fields, omitting `patterns`. `task_to_json()` uses `to_json(root)`, so the task command JSON never includes placement patterns. A caller using `serde_json::to_value(&suggestion)` directly does get `patterns`. No `#[serde(skip)]` annotation or documentation explains the omission.
- **Suggested fix:** Either include `patterns` in `to_json()`, or add `#[serde(skip)]` to `FileSuggestion.patterns` with a comment. Do not leave the two paths silently divergent.

#### AD-8: `suggest_placement_with_embedding` is redundant — superseded by `PlacementOptions.query_embedding`
- **Difficulty:** easy
- **Location:** `src/where_to_add.rs:125-136`, `src/lib.rs:135`
- **Description:** Three public placement functions exist: `suggest_placement`, `suggest_placement_with_embedding`, `suggest_placement_with_options`. The middle function just sets `PlacementOptions.query_embedding` and delegates to `suggest_placement_with_options_core`. Since `PlacementOptions.query_embedding` is public, callers can do this themselves. The function adds API surface without behavior and forces callers to choose among three options when two would suffice.
- **Suggested fix:** Remove `suggest_placement_with_embedding` from the public API. Document that callers wanting a pre-computed embedding should use `PlacementOptions { query_embedding: Some(emb), ..Default::default() }` with `suggest_placement_with_options`.

#### AD-9: `TaskResult.risk` uses anonymous tuple `Vec<(String, RiskScore)>` — inconsistent serialization
- **Difficulty:** easy
- **Location:** `src/task.rs:37`
- **Description:** `TaskResult.risk: Vec<(String, RiskScore)>` where the `String` is a function name. Derived `Serialize` produces `[[name, risk_obj], ...]` — an array of arrays. `task_to_json()` (line 257) calls `r.to_json(n)` which produces `{"name": n, ...}` — an array of objects. The two serialization paths produce structurally different JSON for the same field. The unnamed tuple also gives no self-documentation to the `String` element.
- **Suggested fix:** Define `pub struct NamedRiskScore { pub name: String, pub risk: RiskScore }` and use `pub risk: Vec<NamedRiskScore>` in `TaskResult`. Both serialization paths then produce consistent object arrays.

#### AD-10: `ScoutResult.relevant_notes` has `#[serde(skip)]` but is included in `scout_to_json()`
- **Difficulty:** easy
- **Location:** `src/scout.rs:83-85`, `src/scout.rs:459-469`
- **Description:** `ScoutResult.relevant_notes` is annotated `#[serde(skip)]` — excluded from derived serialization. But `scout_to_json()` explicitly includes `relevant_notes`. `serde_json::to_value(&result)` omits notes; `scout_to_json(&result, root)` includes them. Since `ScoutResult` derives `Serialize` and is public, callers may use either path and get structurally different JSON with no indication which is correct.
- **Suggested fix:** Remove `#[serde(skip)]` and align the derived serializer with `scout_to_json()` via a custom serializer, or drop `#[derive(Serialize)]` from `ScoutResult` and document that `scout_to_json` is the only supported path.

#### AD-11: `ModelInfo` missing `Debug`, `Clone`, and `Serialize`
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:591-606`
- **Description:** `ModelInfo` is a public struct re-exported from the crate root and used in `Store::init(&ModelInfo)`. It derives none of `Debug`, `Clone`, or `Serialize`. Every other public data struct in the crate derives at least `Debug`. Without `Clone`, callers must move or reconstruct the value; without `Debug` it cannot appear in error messages or test assertions; without `Serialize` it cannot be included in JSON diagnostics.
- **Suggested fix:** Add `#[derive(Debug, Clone, serde::Serialize)]` to `ModelInfo`.

#### AD-12: `score_name_match_pre_lower` not exported despite docs recommending it for batch use
- **Difficulty:** easy
- **Location:** `src/store/helpers.rs:668`, `src/store/mod.rs:100`
- **Description:** `score_name_match` is re-exported from `cqs::store`. Its doc comment explicitly says "For batch/loop usage where the same query is reused, prefer `score_name_match_pre_lower` with pre-lowercased strings to avoid redundant heap allocations." But `score_name_match_pre_lower` is not re-exported — only accessible internally. An external caller following the doc recommendation cannot access the recommended function.
- **Suggested fix:** Add `pub use helpers::score_name_match_pre_lower;` alongside `score_name_match` in `store/mod.rs`.

#### OB-3: `cmd_query` span captures only `query_len`, not `query` text
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:41
- **Description:** `tracing::info_span!("cmd_query", query_len = query.len())` captures only the length of the query string. For debugging failed or slow searches, knowing the query length is nearly useless — the actual query text is needed to reproduce the issue. All inner spans (`search_filtered`, `search_index_guided`) also omit the query string. When tracing output shows a slow `cmd_query` span with `query_len=47`, there is no way to know what query caused it without correlating against stdout, which may not be captured.
- **Suggested fix:** Add `query` to the span: `tracing::info_span!("cmd_query", query_len = query.len(), query = query)`. If query privacy is a concern for some deployments, add it at `debug` level: the span itself at `info` level, with a `tracing::debug!(query, "Query text")` after.

#### OB-4: `semantic_diff` span missing source/target labels and threshold
- **Difficulty:** easy
- **Location:** src/diff.rs:79
- **Description:** `tracing::info_span!("semantic_diff")` captures no fields despite the function receiving `source_label`, `target_label`, `threshold`, and `language_filter` — all of which are directly relevant for debugging diff results. The `detect_drift` caller (which wraps `semantic_diff`) does include `reference` and `threshold` in its own span, but `semantic_diff` is also called directly from `cmd_diff` where the outer span only has `source`. When `semantic_diff` is slow or produces unexpected output, the span gives zero context.
- **Suggested fix:** `tracing::info_span!("semantic_diff", source = source_label, target = target_label, threshold, language = ?language_filter)`.

#### OB-5: `--rerank` multi-index warning uses `eprintln!` instead of `tracing::warn!`
- **Difficulty:** easy
- **Location:** src/cli/commands/query.rs:248-251
- **Description:** When `--rerank` is combined with multi-index search (project + references), the code emits: `eprintln!("Warning: --rerank is not supported with multi-index search. Skipping re-ranking.")`. This bypasses the tracing infrastructure entirely. In a pipeline (`cqs "query" --json`) the warning appears on stderr as raw text alongside JSON output, which is acceptable UX but inconsistent with every other warning in the codebase (which all use `tracing::warn!`). It also means the warning is invisible in RUST_LOG filtered log captures.
- **Suggested fix:** Replace with `tracing::warn!("--rerank is not supported with multi-index search, skipping re-ranking")`. The warning will still appear on stderr via the tracing subscriber at default log levels.

#### OB-6: `convert` module missing bytes/duration metrics on successful conversion
- **Difficulty:** easy
- **Location:** src/convert/mod.rs:298-304, src/convert/pdf.rs:44, src/convert/html.rs:21
- **Description:** `finalize_output` logs `tracing::info!(source, output, title, sections, "Converted document")` but omits the input and output byte counts and elapsed conversion time. For performance investigations (e.g., "why did converting this PDF take 30s?"), there is no way to correlate the log with any timing data. `pdf_to_markdown` logs `bytes = markdown.len()` for the raw markdown but drops it before `finalize_output`. `html_to_markdown` logs `bytes` of the HTML conversion result but not the file size read. The section count alone (logged in `finalize_output`) is not a reliable proxy for document size.
- **Suggested fix:** Add `input_bytes` (file size) and `output_bytes` (cleaned markdown length) to the `finalize_output` info log. Optionally wrap the conversion in a `std::time::Instant` and log `elapsed_ms`. These fields are already available at the call sites.

#### OB-7: Watch mode `reindex_files` warns on parse failure without file count context
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:500
- **Description:** `tracing::warn!("Failed to parse {}: {}", abs_path.display(), e)` uses positional formatting instead of structured fields. This is the only `tracing::warn!` in `reindex_files` that doesn't use the `field = value` pattern — inconsistent with the structured fields used on lines 612 (`file = %rel_path.display(), error = %e`) and 617 (`error = %e`). Positional formatting prevents log processors from extracting the path and error as filterable fields.
- **Suggested fix:** `tracing::warn!(file = %abs_path.display(), error = %e, "Failed to parse file");` — consistent with surrounding structured logging style.

## Robustness

#### RB-1: `cached_notes_summaries()` panics on poisoned `RwLock` instead of returning `StoreError`
- **Difficulty:** easy
- **Location:** src/store/mod.rs:748,759
- **Description:** `cached_notes_summaries()` calls `.expect("notes cache lock poisoned")` on both the read and write guard acquisitions. If a previous thread panicked while holding the lock, Rust marks the `RwLock` as poisoned, and subsequent `.read()`/`.write()` calls return `Err(PoisonError)`. The `.expect()` turns that into a panic, crashing the process rather than surfacing a `StoreError` to the caller. Every call to `cached_notes_summaries()` — which is called by search, context, scout, and every command that reads notes — becomes a panic risk after any prior panic in the store. The convention in this codebase is to recover from poisoned mutexes using `unwrap_or_else(|p| p.into_inner())` (see `embedder.rs:418`, `cagra.rs:129`, `reranker.rs:198`).
- **Suggested fix:** Replace both `.expect("notes cache lock poisoned")` calls with `.unwrap_or_else(|p| { tracing::warn!("notes cache lock poisoned, recovering"); p.into_inner() })` to match the established recovery pattern.

#### RB-2: `cached_notes_summaries()` panics on poison; `invalidate_notes_cache()` silently ignores it
- **Difficulty:** easy
- **Location:** src/store/mod.rs:748-760 (panics), src/store/mod.rs:770 (silently ignores)
- **Description:** The read path (`cached_notes_summaries()`) uses `.expect()` and panics on a poisoned lock. The write path (`invalidate_notes_cache()`) uses `if let Ok(mut guard) = self.notes_summaries_cache.write() { ... }` — silently skipping the invalidation on poison. If the lock is poisoned: reads panic (process crash), invalidations silently no-op (stale cache stays populated). These two behaviors are contradictory: if poison is catastrophic enough to panic on read, it should also be reported on invalidate; if it's safe to recover (standard practice), both paths should recover. The asymmetry means there is no consistent policy.
- **Suggested fix:** Align both paths to the recovery pattern: add a `tracing::warn!` in `invalidate_notes_cache` when `write()` returns `Err`, or apply the same `unwrap_or_else` recovery from RB-1 to both paths.

#### RB-3: `reranker.rs` stride=0 edge case bypasses bounds check and panics at `data[i * stride]`
- **Difficulty:** easy
- **Location:** src/reranker.rs:147-163
- **Description:** When the cross-encoder ONNX model returns a tensor with shape `[batch, 0]` (an edge case possible with a misconfigured or custom model), `stride` is set to `shape[1] as usize = 0`. The bounds check at line 154 computes `expected_len = batch_size * 0 = 0`, so `data.len() < 0` is never true — the check passes even for empty `data`. Then `data[i * stride]` = `data[0]` panics with "index out of bounds: the len is 0 but the index is 0" for every element. No error is returned; the process panics. The same `stride` value of 0 would also cause all results to receive the same score (from `data[0]`) rather than a unique score per result — silent wrong output before the panic is even reached.
- **Suggested fix:** After computing `stride`, add: `if stride == 0 { return Err(RerankerError::Inference("Model returned zero-width output tensor".to_string())); }`.

#### RB-4: `embedder.rs` panics via `outputs["last_hidden_state"]` if custom ONNX model lacks that output
- **Difficulty:** easy
- **Location:** src/embedder.rs:538
- **Description:** `outputs["last_hidden_state"]` uses ORT's `Index<&str>` implementation on `SessionOutputs`, which panics if the key is absent (standard Rust `Index` contract). With the default bundled model this never triggers. However, users who set `CQS_MODEL_PATH` to a custom ONNX model with different output names (e.g., `"hidden_states"`, `"pooler_output"`, `"embeddings"`) will get a panic at inference time rather than an actionable error message. The panic message will be ORT-internal and not mention `CQS_MODEL_PATH` or output name expectations. The shape validation immediately below (lines 545-563) is correct but never reached.
- **Suggested fix:** Replace `outputs["last_hidden_state"]` with `outputs.get("last_hidden_state").ok_or_else(|| EmbedderError::InferenceFailed(format!("ONNX model has no 'last_hidden_state' output. Available outputs: {:?}", outputs.keys().collect::<Vec<_>>())))?.try_extract_tensor::<f32>().map_err(ort_err)?`.

#### RB-5: `search_by_name` uses `assert!` (not `debug_assert!`) in hot production path
- **Difficulty:** easy
- **Location:** src/store/mod.rs:623-626
- **Description:** `search_by_name` contains `assert!(!normalized.contains('"'), "sanitized query must not contain double quotes")`. This assertion runs on every name-only search in release builds. The invariant is correct — `sanitize_fts_query` strips `"` in its first pass — but `assert!` in a hot search path adds unnecessary overhead and couples the runtime behavior to the sanitizer's implementation. If `sanitize_fts_query` were ever refactored to use a different stripping strategy, this `assert!` would panic in production rather than being caught in tests. The project convention (per `CLAUDE.md`) is to use `assert!` in tests and `debug_assert!` for internal invariants in library code.
- **Suggested fix:** Change `assert!` to `debug_assert!`. The invariant is enforced at compile-time by `sanitize_fts_query`'s logic; a debug assertion is sufficient to catch regressions during testing.

#### RB-6: `hnsw/build.rs` `prepare_index_data` validates dimensions but not zero-length embedding vecs
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs (via `super::prepare_index_data`), src/hnsw/mod.rs
- **Description:** `build_batched` validates that each embedding's length equals `EMBEDDING_DIM` (line 150), correctly catching dimension mismatches. However, if an embedding `Vec<f32>` has the correct length but all values are `0.0` (a zero vector), `DistCosine` will compute a division by zero during HNSW construction, producing `NaN` distances. NaN distances propagate silently through the graph construction — `hnsw_rs` does not validate for NaN. Subsequent searches that encounter NaN scores will have undefined ranking. Zero-vector embeddings can occur when the ONNX model produces degenerate output for unusual inputs (e.g., a file containing only null bytes, or extremely short content after tokenization produces an empty sequence).
- **Suggested fix:** In `prepare_index_data` (or `build_batched`), after dimension validation, add: `let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt(); if norm == 0.0 { tracing::warn!(chunk_id, "Skipping zero-vector embedding — would produce NaN cosine distances"); continue; }`.

## Algorithm Correctness

#### AC-1: HNSW `ef_search` cap formula doesn't enforce index-size bound
- **Difficulty:** easy
- **Location:** src/hnsw/search.rs:41-44
- **Description:** The adaptive `ef_search` formula is `.max(k * 2).min(index_size.max(self.ef_search))`. The comment says "capped at index size (searching more than the index is pointless for small indexes)". However, the cap expression `index_size.max(self.ef_search)` means the actual cap is `max(index_size, self.ef_search)`, not `index_size`. When `self.ef_search > index_size` (e.g., default ef_search=100 on a 30-chunk index), the formula never clips below `self.ef_search`, so `ef_search` can exceed `index_size`. The stated invariant ("capped at index size") is not enforced. The correct formula to satisfy the stated intent is `.min(index_size)`. The current formula produces a value that is correct for correctness purposes (hnsw_rs handles oversized ef_search gracefully) but wastes search effort and the misleading comment may cause future developers to trust the wrong bound.
- **Suggested fix:** Change `.min(index_size.max(self.ef_search))` to `.min(index_size)`. The `self.ef_search` baseline is already preserved by the initial value before `.max(k * 2)`, so the `.max(self.ef_search)` in the cap is redundant.

#### AC-2: `waterfall_pack` surplus propagation uses wrong base for code budget when scout overshoots
- **Difficulty:** medium
- **Location:** src/cli/commands/task.rs:144-146
- **Description:** The code section budget is computed as `((budget * WATERFALL_CODE) as usize + scout_budget.saturating_sub(scout_used)).min(remaining)`. The `scout_budget.saturating_sub(scout_used)` term gives 0 if `scout_used >= scout_budget`, but `index_pack` always includes at least one item even when it exceeds the budget (the "first-item guarantee"). So `scout_used` can be larger than `scout_budget`, leaving `remaining` decremented by more than `scout_budget`, while the surplus passed downstream is 0. This means the code section's effective budget is `budget * WATERFALL_CODE`, not `budget * WATERFALL_CODE + surplus`, but `remaining` has already been reduced by the overshoot. The net effect is the impact/placement/notes sections get less total budget than they should when scout overshoots. Example: `budget=1000`, scout_budget=150, but single file group is 200 tokens. `scout_used=200`, `remaining` becomes 800, code_budget = `(500 + 0).min(800) = 500`, impact_budget = `(150 + 0).min(300) = 150`. The 50-token overshoot in scout silently reduces the notes/placement budget by 50. A consistent treatment would charge the overshoot only to the section that caused it, not cascade silently.
- **Suggested fix:** After each section, charge only `min(section_used, section_budget)` to remaining and propagate the deficit separately. Simpler fix: remove the `.min(remaining)` cap and let `index_pack`'s "first-item guarantee" naturally enforce a global budget via the section sum at the end — the current `.min(remaining)` cap doesn't actually prevent total overshoot, it just shifts it.

#### AC-3: `bfs_expand` depth check uses `>=` on seeds' initial depth — silently skips expansion when seeds start at depth > 0
- **Difficulty:** easy
- **Location:** src/gather.rs:197
- **Description:** In `bfs_expand`, seeds are loaded into the queue with their initial depth from `name_scores` (line 192-194). In `gather_with_graph`, seeds always have depth 0. But in `gather_cross_index`, bridge results are inserted with depth 1 (see the bridge `name_scores.insert(...)` path). The loop's `if depth >= opts.expand_depth` check uses the seed's depth directly. With default `expand_depth=1` and bridge seeds at depth 1, the check `1 >= 1` immediately fires and the bridge seeds are never expanded. The bridge was supposed to find project-side code and expand it one hop, but with `expand_depth=1`, zero expansion happens. The cross-index gather's BFS expansion is effectively disabled at the default depth. This is a boundary condition where seed depth and expansion depth interact non-obviously; there is no test exercising cross-index gather with depth verification.
- **Suggested fix:** Either (a) always seed the queue at depth 0 regardless of input depth (treating the seed itself as the BFS root for expansion purposes), or (b) check `depth - seed_depth >= opts.expand_depth` using a per-node origin depth. Option (a) is simpler: `queue.push_back((name.clone(), 0))` regardless of stored depth.

#### AC-4: `extract_call_snippet_from_cache` window is `[offset-1 .. offset+2)` — shows 3 lines but skips context when `call_line == line_start`
- **Difficulty:** easy
- **Location:** src/impact/analysis.rs:143-145
- **Description:** The snippet window is `start = offset.saturating_sub(1)`, `end = (offset + 2).min(len)`. For `offset=0` (call_line is the first line of the chunk, i.e., `call_line == line_start`), `start = 0` and `end = 2`, showing lines 0 and 1. This gives 1 line of context after but 0 before, which is asymmetric compared to offset > 0 (which gets 1 before, the target, 1 after). More importantly, the window `[offset-1..offset+2)` has size 3 but the doc comment says "snippet around the call site", implying the call site is in the middle. When `offset == 0`, the call site is at index 0 which is the first line of the 2-line window — not centered. The asymmetry is intentional per the `saturating_sub(1)` clamp, but there's no test for `call_line == line_start` to document this as deliberate.
- **Suggested fix:** Add a test case for `call_line == line_start`. If the asymmetry is intentional, add a doc comment. If not, use `offset.saturating_sub(2)` and `offset + 3` to give more context lines.

#### AC-5: `reverse_bfs` includes the target itself at depth 0 — callers using `d > 0` to filter it must know this invariant
- **Difficulty:** easy
- **Location:** src/impact/bfs.rs:15, src/impact/analysis.rs:166
- **Description:** `reverse_bfs` always inserts `target` at depth 0 into `ancestors`. Two callers correctly filter with `if d > 0`: `find_affected_tests_with_chunks` (analysis.rs:166) and `find_transitive_callers` (analysis.rs:199). A third caller, `suggest_tests` (analysis.rs:285), calls `reverse_bfs` for each caller individually and checks `ancestors.get(&t.name).is_some_and(|&d| d > 0)`. If `caller.name == test.name` (a caller that is also a test), this correctly excludes it. The invariant is load-bearing and non-obvious: if a future caller of `reverse_bfs` forgets to filter `d == 0`, the target itself appears as its own "caller" in results. The function has no doc comment warning about this.
- **Suggested fix:** Add a doc comment: "Note: the target itself is always present at depth 0. Callers wishing to exclude the target should filter `d > 0`." Consider also providing a `reverse_bfs_callers_only` wrapper that filters it automatically.

#### AC-6: `token_pack` and `index_pack` "always include first item" guarantee can exceed budget by an unbounded amount
- **Difficulty:** easy
- **Location:** src/cli/commands/mod.rs:135, src/cli/commands/task.rs:62
- **Description:** Both `token_pack` and `index_pack` enforce "always include at least one item even if over budget" (the `kept_any` flag in `token_pack`, and `!kept.is_empty()` in `index_pack`). When `budget = 100` and the highest-scored item has 50,000 tokens, the function returns 1 item at 50,000 tokens. The caller receives `used = 50000` when it asked for `budget = 100`. In `waterfall_pack`, the upstream section's `remaining` is decremented by `scout_used` (which could be 50,000) via `remaining.saturating_sub(scout_used)`, making `remaining = 0` for all subsequent sections. The downstream sections then get `budget = 0`, and `index_pack` returns nothing (because `budget == 0` is an explicit early return at `task.rs:52`). The behavior is: first section gets an oversized item, all other sections get nothing. No warning is emitted. This is documented as "by design" in v0.28.3 triage (AC-4), but the interaction with waterfall propagation is not documented and the silent zeroing of downstream budgets is surprising.
- **Suggested fix:** Add a `tracing::warn!` when `used > budget` after packing, logging the overrun. This makes the "first item guarantee" visible in traces. No behavior change needed if this is intentional, but the silence is a diagnostics gap.

## Platform Behavior

#### PB-1: `.cqs/.gitignore` created by `cqs init` omits HNSW files and lock file
- **Difficulty:** easy
- **Location:** src/cli/commands/init.rs:37-41
- **Description:** `cmd_init` writes `.cqs/.gitignore` with four entries: `index.db`, `index.db-wal`, `index.db-shm`, `index.lock`. Missing from this list are the HNSW index files (`index.hnsw.graph`, `index.hnsw.data`, `index.hnsw.ids`, `index.hnsw.checksum`) and the HNSW lock file (`index.hnsw.lock`). A developer who runs `cqs init` and then `git add .` in a project will commit the HNSW files — typically 50–500MB of binary vector data. The HNSW files are runtime artifacts identical in status to `index.db`. The real `.cqs/.gitignore` in this repo also only has the 4 DB entries, confirming the gap is live.
- **Suggested fix:** Add to the written gitignore: `index.hnsw.graph\nindex.hnsw.data\nindex.hnsw.ids\nindex.hnsw.checksum\nindex.hnsw.lock\n`. Also add `*.tmp` to catch temp files from interrupted atomic writes.

#### PB-2: `7z -o` output path uses `Path::display()` — breaks on non-UTF-8 paths
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:30
- **Description:** `format!("-o{}", temp_dir.path().display())` uses `Display` to format the temp dir path into the 7z `-o` argument. On Unix, `Path::display()` uses lossy UTF-8 substitution (`U+FFFD`) for non-UTF-8 path components. If the system temp directory (e.g., `$TMPDIR`) contains non-UTF-8 bytes, the `-o` argument sent to 7z is corrupted and 7z extracts to a different or nonexistent path. The result is a silent empty extraction (7z succeeds, but content goes elsewhere) and the subsequent walkdir finds no HTML files. `tempfile::tempdir()` typically places files in `/tmp` which is ASCII-safe, but `$TMPDIR` is user-controlled. The fix avoids the lossy conversion entirely by passing the path as an `OsStr` argument.
- **Suggested fix:** Use `std::os::unix::ffi::OsStrExt` to build the argument directly: `let mut output_arg = std::ffi::OsString::from("-o"); output_arg.push(temp_dir.path()); cmd.arg(output_arg);`. On Windows, 7z accepts Unicode paths natively so the analogous Windows API applies.

#### PB-3: `is_wsl()` reads `/proc/version` on every call site — WSL detection is first-call cached, but called before the cache is populated in non-standard order
- **Difficulty:** easy
- **Location:** src/config.rs:17-27
- **Description:** `is_wsl()` correctly uses `OnceLock` to cache the result after the first call. However, the WSL check string at line 23 (`lower.contains("microsoft") || lower.contains("wsl")`) would match a non-WSL Linux host whose `/proc/version` mentions "Microsoft" (e.g., a VM running on Azure or a VM in a dev environment that includes "Microsoft" in a compiler description string). In practice this is rare but the detection is not definitive. More concretely: the check fires on `/proc/version` strings like `Linux version 5.15... (Microsoft@Microsoft.com) ...` which appear in some Azure Linux VMs even without WSL, causing all WSL advisory locking warnings to trigger incorrectly. The canonical WSL detection should check `WSL_DISTRO_NAME` or `WSL_INTEROP` environment variables (set only in real WSL sessions) before falling back to `/proc/version`.
- **Suggested fix:** Add a primary check: `if std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some() { return true; }` before reading `/proc/version`. This is faster (no file I/O) and more accurate for modern WSL2 deployments.

#### PB-4: `find_pdf_script` uses `scripts/pdf_to_md.py` relative to CWD — breaks when cqs is run from a subdirectory
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:72-83
- **Description:** `find_pdf_script` checks `scripts/pdf_to_md.py` relative to `std::env::current_dir()`. When a user runs `cqs convert foo.pdf` from a subdirectory of their project (e.g., `src/`), the script is looked up at `src/scripts/pdf_to_md.py` instead of the project root's `scripts/pdf_to_md.py`. The function does try `current_exe()/../scripts/pdf_to_md.py` as a second candidate, but not `project_root/scripts/pdf_to_md.py`. Every other command that needs project context uses `find_project_root()` from `src/cli/config.rs`. The PDF converter bypasses this entirely.
- **Suggested fix:** Add a third candidate: `find_project_root().join("scripts/pdf_to_md.py")` between the CWD candidate and the binary-relative candidate. Alternatively, document that `cqs convert` must be run from the project root.

#### PB-5: WSL poll auto-detection only triggers on `/mnt/` prefix — misses WSL native filesystem projects on `/mnt/` subpaths
- **Difficulty:** easy
- **Location:** src/cli/watch.rs:67-72
- **Description:** Watch mode auto-selects poll when `is_wsl() && root.starts_with("/mnt/")`. The WSL advisory locking warning in `hnsw/persist.rs:20-27` and `project.rs:72-79` use the same `/mnt/` prefix check. However, WSL projects can live on the Linux native filesystem (e.g., `/home/user/project`) and still encounter inotify unreliability for files on DrvFS-mounted paths that are accessed indirectly. More importantly, `/mnt/` is checked as a string prefix rather than by checking the actual filesystem type, so projects on `/mnt/data` (a non-Windows NTFS mount, e.g., an external USB drive formatted NTFS) also trigger the advisory warning, while a Windows path mounted at `/win` (custom `wsl.conf` entry) would not. The check is a reasonable heuristic but it's undocumented that the detection is prefix-based, not filesystem-type-based.
- **Suggested fix:** Add a doc comment to `is_wsl()` and to the `/mnt/` check sites explaining the heuristic: "Checked via /mnt/ path prefix — this catches Windows filesystem mounts (C:, D:) but may false-positive on other NTFS mounts or miss custom WSL mount points." No behavior change needed, but the undocumented assumption has tripped developers before (see watch.rs:70 warning text which only mentions 'Windows filesystem mounts').

#### PB-6: `chm_to_markdown` `path_str` uses `to_string_lossy()` — non-UTF-8 CHM paths silently fail
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:29
- **Description:** `let path_str = path.to_string_lossy();` is passed to 7z as a positional argument. On Unix, paths can contain arbitrary bytes. If the CHM file path contains non-UTF-8 bytes, `to_string_lossy()` replaces them with `U+FFFD`, and 7z receives a path with replacement characters — a path that does not exist. 7z then fails with "No such file or directory", which `chm_to_markdown` surfaces as a 7z extraction failure. The user sees "7z extraction failed" with no indication their path has non-UTF-8 characters. `pdf_to_markdown` has the same issue at line 22 (`path.to_string_lossy().to_string()`). Both should use platform-native argument passing.
- **Suggested fix:** Pass the path directly as an `OsStr` argument to `Command::args`. Replace `path_str.as_ref()` with `path.as_os_str()` so the kernel receives the exact bytes without UTF-8 conversion. Same fix applies to `pdf.rs:22`.

#### PB-7: `ensure_ort_provider_libs` silently skips GPU setup when `LD_LIBRARY_PATH` is unset
- **Difficulty:** medium
- **Location:** src/embedder.rs:685-700
- **Description:** On systems where GPU acceleration is desired but `LD_LIBRARY_PATH` is not set (or set to an empty string), `ensure_ort_provider_libs` silently returns with no action and no log message. The function finds the ORT cache dir and the provider libs correctly, but then fails to find any target directory because `ld_path.split(':')` over an empty string yields only `""` which fails the `!p.is_empty()` filter. The user gets CPU-only embedding with no explanation, even if their CUDA/TensorRT libs are present in the ORT cache. The function has a `tracing::debug!` for "ORT cache directory not found" but nothing for "LD_LIBRARY_PATH unset/empty — skipping symlink setup".
- **Suggested fix:** Add a `tracing::debug!` at the point where `target_dir` is `None`: `tracing::debug!("No suitable target directory in LD_LIBRARY_PATH — GPU provider symlinks not created. Set LD_LIBRARY_PATH to a writable lib directory to enable GPU acceleration");`. This surfaces the issue without changing behavior.

## Test Coverage

#### TC-1: `convert/html.rs`, `convert/chm.rs`, `convert/webhelp.rs` — zero tests for conversion logic
- **Difficulty:** medium
- **Location:** src/convert/html.rs (47 lines), src/convert/chm.rs (189 lines), src/convert/webhelp.rs (144 lines)
- **Description:** `html_to_markdown`, `html_file_to_markdown`, `is_webhelp_dir`, `webhelp_to_markdown`, and `chm_to_markdown` have zero unit or integration tests. The only convert tests are `convert/mod.rs` (detect_format round-trips, feature-gated), `convert/cleaning.rs` (cleaning rules), and `convert/naming.rs` (title/filename helpers). The HTML conversion logic, the 100 MB size-limit enforcement in `html_file_to_markdown`, the WebHelp detection heuristic (`is_webhelp_dir`), and the CHM zip-slip containment check (`chm.rs:61-82` — verifies extracted paths stay inside the temp dir) are all untested. The zip-slip check is security-relevant and has no test confirming the bail path fires.
- **Suggested fix:** Add unit tests for (1) `html_to_markdown` with a minimal HTML string; (2) `html_file_to_markdown` with a temp file over 100 MB (should fail with the size-limit error); (3) `is_webhelp_dir` with a temp dir that has/doesn't have a `content/*.html`; (4) `webhelp_to_markdown` with a temp dir containing one HTML page. CHM tests require `7z` so mark `#[ignore]` or use a pre-extracted fixture.

#### TC-2: `suggest.rs` `detect_risk_patterns` — `high_risk` branch never exercised
- **Difficulty:** easy
- **Location:** src/suggest.rs:141-151
- **Description:** `detect_risk_patterns` branches on two mutually exclusive paths: `untested_hotspot` (caller_count ≥ 5 AND test_count == 0) and `high_risk` (else if `risk_level == High`, meaning callers ≥ 5, some tests, but score still ≥ `RISK_THRESHOLD_HIGH`). `test_suggest_untested_hotspot` covers the first branch. The `high_risk` branch — a function with 6+ callers and at least one test, but `caller_count * (1.0 - coverage) >= 5.0` — is untested. This branch emits `reason: "high_risk"` and sentiment `-1.0` (vs `-0.5` for untested hotspot). A regression swapping the branch conditions or sentiments would go undetected.
- **Suggested fix:** Add a test inserting a hotspot with 8 callers and 1 test chunk. Score = `8 * (1 - 1/8) = 7.0 > RISK_THRESHOLD_HIGH`. Call `suggest_notes` and assert one suggestion has `reason == "high_risk"` and `sentiment == -1.0`.

#### TC-3: `health.rs` — `untested_hotspots` field computed but never asserted in tests
- **Difficulty:** easy
- **Location:** src/health.rs:86-107, src/health.rs:284-346 (test module)
- **Description:** `health_check` computes `untested_hotspots` (hotspots with ≥ `HOTSPOT_MIN_CALLERS` callers and `test_count == 0`). The test `test_health_hotspots` inserts `hot_target` with 6 callers and no test chunks — the function should appear in `untested_hotspots`. The test asserts `!report.hotspots.is_empty()` and checks `hotspots[0].name` but never reads `report.untested_hotspots`. A regression in the filter predicate (`r.caller_count >= HOTSPOT_MIN_CALLERS && r.test_count == 0 && r.risk_level == RiskLevel::High`) or `compute_risk_batch` integration would not be caught. The negative case (tested hotspot excluded from `untested_hotspots`) is also unverified.
- **Suggested fix:** Add `assert!(!report.untested_hotspots.is_empty(), ...)` and `assert_eq!(report.untested_hotspots[0].name, "hot_target")` to `test_health_hotspots`. Add a separate test that inserts test chunks pointing to the hotspot and asserts `report.untested_hotspots.is_empty()`.

#### TC-4: `review.rs` `match_notes` — partial-mention-match edge cases untested
- **Difficulty:** easy
- **Location:** src/review.rs:183-211
- **Description:** `match_notes` builds `matching_files` by checking each changed file against each note's mentions via `path_matches_mention`. The integration test `test_review_diff_with_relevant_notes` covers the basic case: a note with one mention that matches a changed file. Neither unit nor integration tests cover: (1) a note with 2 mentions where only 1 matches — note should be included and `matching_files` should list only the matching file; (2) a note where all mentions fail to match — should be absent from results. The function is `fn` (not `pub`) and has no inline unit tests. `match_notes` is only reachable by going through the full `review_diff` call.
- **Suggested fix:** Add tests in the `review.rs` test module using a Store with pre-inserted notes via `store.replace_notes_for_file`. Test the partial-match case (2 mentions, 1 matches → `matching_files.len() == 1`) and the no-match case (note absent).

#### TC-5: `impact/diff.rs` `analyze_diff_impact_with_graph` — depth-0 exclusion and BFS anomaly path untested
- **Difficulty:** easy
- **Location:** src/impact/diff.rs:168, src/impact/diff.rs:181-183
- **Description:** Two defensive paths have no tests: (1) `if depth > 0` guard at line 168 — when a changed function is itself a test, it should not appear in `all_tests` (depth == 0 means the function is the BFS root, not an ancestor). Integration tests in `impact_diff_test.rs` do not include a scenario where a changed function is a test. (2) The BFS anomaly fallback at line 181-183: `reverse_bfs_multi` finds a test reachable from the combined start set, but no individual per-function BFS includes it — yielding `via = "(unknown)"`. This sentinel string is emitted via `tracing::debug!` but is never verified to be produced under the right conditions. No test constructs a scenario where this path fires.
- **Suggested fix:** Add a unit test where `changed = [test_fn]` with `test_fn` also in `test_chunks` and assert `result.all_tests.is_empty()` (depth-0 exclusion). Document the `"(unknown)"` path as a known untested defensive case in a comment at line 181.

#### TC-6: `related.rs` unit test module tests only struct construction, not module logic
- **Difficulty:** easy
- **Location:** src/related.rs:170-234
- **Description:** The 3 unit tests in `related.rs` construct `RelatedFunction` and `RelatedResult` structs and assert field values. They do not call `find_related`, `resolve_to_related`, or `find_type_overlap`. Integration tests in `tests/related_test.rs` cover `find_related` end-to-end. However, the silent-empty-on-error path in `resolve_to_related` (lines 87-90: `get_chunks_by_names_batch` failure returns `Vec::new()` silently) and the early-return in `find_type_overlap` (lines 117-119: empty `type_names` returns `Ok([])`) have no test at any level. The unit test module's existence gives the false impression of local coverage.
- **Suggested fix:** Replace the 3 tautological struct tests with: (1) `find_type_overlap` with empty `type_names` returns `Ok([])` (use a Store with minimal setup); (2) `resolve_to_related` with an empty `pairs` slice returns `[]`. These are fast unit tests that actually verify behavior.

#### TC-7: `convert/pdf.rs` `find_pdf_script` search logic untested
- **Difficulty:** easy
- **Location:** src/convert/pdf.rs:54-90
- **Description:** `find_pdf_script` has three code paths: (1) `CQS_PDF_SCRIPT` env var pointing to an existing file — return it; (2) `CQS_PDF_SCRIPT` pointing to a nonexistent file — warn and fall through; (3) check `scripts/pdf_to_md.py` relative to CWD and binary dir. Additionally, a `.py` extension check at line 60 warns but still returns the script (non-fatal). None of these paths are tested. The function controls whether `pdf_to_markdown` can run at all, yet its logic has no regression safety. A change that inverts the nonexistent-file fallback (continuing instead of trying candidates) would break PDF conversion silently.
- **Suggested fix:** Add unit tests using `std::env::set_var` and `std::env::remove_var` in a scoped context (or `temp_env` crate): (1) `CQS_PDF_SCRIPT` set to a nonexistent path → `find_pdf_script` falls through to candidates (returns `Err` when candidates also absent); (2) `CQS_PDF_SCRIPT` set to a temp file with `.txt` extension → succeeds but only after the extension warning is emitted. Mark `#[serial]` to prevent env var races.

## Extensibility

#### EX-1: `CHUNK_CAPTURE_NAMES` and `capture_name_to_chunk_type` are a third sync point for `ChunkType`
- **Difficulty:** easy
- **Location:** src/parser/types.rs:14-54
- **Description:** Adding a new `ChunkType` variant requires updates in three independent locations: (1) the `define_chunk_types!` invocation in `src/language/mod.rs`, (2) the `capture_name_to_chunk_type` match in `src/parser/types.rs:14-32`, and (3) the `CHUNK_CAPTURE_NAMES` const array in `src/parser/types.rs:38-54`. There is no compile-time guard connecting them — a developer can add a variant to the enum and the DB will store the right name, but `capture_name_to_chunk_type` will silently fall through to `None`, and `CHUNK_CAPTURE_NAMES.contains()` will return false, causing the parser to discard all chunks of the new type. Additionally, `ChunkType::Constant` has a split identity: its display/DB name is `"constant"` but its capture name is `"const"` (because `const` is a keyword in tree-sitter queries). This divergence is necessary but undocumented in context — a future language author using the wrong name for a new type would silently drop chunks.
- **Suggested fix:** (1) Add a `// SYNC: also update capture_name_to_chunk_type and CHUNK_CAPTURE_NAMES` comment to the `define_chunk_types!` invocation. (2) Add a compile-time or startup assertion that `capture_name_to_chunk_type` covers every string in `CHUNK_CAPTURE_NAMES`. (3) Document the `"const"` / `"constant"` split in `capture_name_to_chunk_type`'s doc comment: "Note: `ChunkType::Constant` uses capture name `const` (a keyword in many grammars) but displays as `constant` in DB and JSON."

#### EX-2: `Pattern::FromStr` error message hardcodes valid names; coverage test hardcodes variant count
- **Difficulty:** easy
- **Location:** src/structural.rs:29-32, src/structural.rs:265
- **Description:** Adding a new `Pattern` variant requires updates in four places: (1) the `match` arm in `FromStr`, (2) the `Display` impl, (3) the `all_names()` array, and (4) the error message string at line 30 which hardcodes `"Valid: builder, error_swallow, async, mutex, unsafe, recursion"`. `all_names()` exists precisely to avoid this duplication — but `FromStr` doesn't use it. The coverage test at line 265 asserts `Pattern::all_names().len() == 6`, which is a brittle count that must be manually updated rather than derived. A developer adding a new variant who updates `all_names()` and the match arms but forgets the error message string ships a stale user-facing error message.
- **Suggested fix:** Replace the hardcoded error string with `format!("Unknown pattern '{}'. Valid: {}", s, Pattern::all_names().join(", "))`. Change the test assertion from `assert_eq!(Pattern::all_names().len(), 6)` to ensure every entry round-trips through `FromStr` and `Display` (the existing roundtrip loop at lines 255-258 already does this — the count check is redundant and fragile).

#### EX-3: `--chunk-type` CLI help text lists 11 of 16 `ChunkType` variants — 5 silently absent
- **Difficulty:** easy
- **Location:** src/cli/mod.rs:207
- **Description:** The `--chunk-type` argument doc comment reads: `"Filter by chunk type (function, method, class, struct, enum, trait, interface, constant, property, delegate, event)"`. Five valid variants are missing: `section`, `module`, `macro`, `object`, `typealias`. A user searching for Rust `macro_rules!` definitions with `--chunk-type macro` would not know this is valid from the help text. The parse itself succeeds (via `ChunkType::FromStr`), but discoverability is broken. This list also must be manually updated whenever a new `ChunkType` is added.
- **Suggested fix:** Replace the hardcoded list with a note pointing to runtime validation: `"Filter by chunk type. Valid types: function, method, struct, class, enum, trait, interface, constant, section, property, delegate, event, module, macro, object, typealias"`. Long-term, derive this from `ChunkType::valid_names()` via a `#[arg(help = ...)]` computed at build time or a lazy-static help string.

#### EX-4: `nl.rs` hardcodes `"typealias"` string to fix multi-word NL display — future `ChunkType` additions require the same workaround
- **Difficulty:** easy
- **Location:** src/nl.rs:339
- **Description:** `generate_nl_description` computes `type_word` with: `if type_display == "typealias" { "type alias" } else { &type_display }`. This converts `ChunkType::TypeAlias`'s single-word display form to a readable two-word NL phrase. The pattern is a one-off workaround with no general mechanism. If a new `ChunkType` is added whose display string is a single concatenated word (e.g., `"abstractclass"`, `"extmethod"`), the same workaround must be discovered and applied manually. The `ChunkType` enum has no field for "human readable NL label", so the `nl.rs` author has no signal that this file needs updating.
- **Suggested fix:** Add an `fn nl_label(self) -> &'static str` method to `ChunkType` that returns the human-readable label (e.g., `ChunkType::TypeAlias => "type alias"`, all others return their display string). `generate_nl_description` calls `chunk.chunk_type.nl_label()` instead of the string comparison. Adding a new `ChunkType` variant without implementing `nl_label` is then a compile error (non-exhaustive match).

#### EX-5: `find_project_root` hardcodes build-system markers for 5 of 50 supported languages
- **Difficulty:** medium
- **Location:** src/cli/config.rs:45-52
- **Description:** `find_project_root` recognizes six project markers: `Cargo.toml` (Rust), `package.json` (Node.js), `pyproject.toml`/`setup.py` (Python), `go.mod` (Go), and `.git` (fallback). The codebase supports 50 languages, and many have standard project root conventions not covered: Java/Maven (`pom.xml`), Java/Gradle (`build.gradle`), Scala (`build.sbt`), Elixir (`mix.exs`), Ruby (`Gemfile`), PHP (`composer.json`), Swift/Xcode (`Package.swift`), Dart/Flutter (`pubspec.yaml`), and C/C++ (`CMakeLists.txt`). When cqs is run from inside a Java or Ruby project without a `.git` directory, `find_project_root` walks to the filesystem root and falls back to CWD with a warning — the index is then placed in the wrong directory and subsequent commands produce confusing results. Each missing language requires a doc issue, user report, and targeted PR to fix.
- **Suggested fix:** Expand the markers array to include `pom.xml`, `build.gradle`, `build.sbt`, `Gemfile`, `composer.json`, `Package.swift`, `pubspec.yaml`, `mix.exs`, and `CMakeLists.txt`. Keep the existing priority ordering (language-specific ahead of `.git`). Alternatively, source the marker list from `LanguageDef` by adding an optional `project_markers: &'static [&'static str]` field to `LanguageDef`, making project detection data-driven alongside language parsing.

## Performance

#### PERF-1: SQL placeholder strings rebuilt on every batch iteration in 22 call sites
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:64-67, src/store/calls.rs:519-521, src/store/types.rs (3 sites), and 18 additional sites across store/
- **Description:** Every batched SQL function generates its placeholder string (`"?1,?2,...?N"`) with `(1..=batch.len()).map(|i| format!("?{}", i)).collect::<Vec<_>>().join(",")`. This pattern appears 22 times across `chunks.rs`, `calls.rs`, `types.rs`, and `search.rs`. Each call allocates a `Vec<String>` of N formatted strings plus a joined `String`. For the common batch sizes (20–500), this costs 20–500 small allocations per batch iteration, and most functions call it in a loop. For a 10k-chunk index with 500-item batches, this is 20 iterations × 2 allocations each = 40 allocation sequences per `get_embeddings_by_ids` call. The pattern also prevents query plan reuse since each invocation may produce a different-length placeholder list (SQLite caches query plans by SQL text).
- **Suggested fix:** Extract a helper `fn make_placeholders(n: usize) -> String` that pre-allocates with `String::with_capacity(n * 4)` and writes `?1,?2,...` in one pass without intermediate `Vec<String>`. For fixed batch sizes (e.g., `BATCH_SIZE = 500`), consider pre-building and reusing the placeholder string. This eliminates N intermediate `String` allocations per call.

#### PERF-2: `search_by_names_batch` post-filter is O(results × batch_names) per result row
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:1213-1229
- **Description:** After executing the combined FTS query for a batch of 20 names, the code assigns each returned row to matching query names by iterating the entire batch: `for (original_name, _normalized) in batch { let score = super::score_name_match(&chunk.name, original_name); ... }`. With batch size 20 and `total_limit = limit_per_name * 20` rows returned, every row requires up to 20 `score_name_match` calls. `score_name_match` itself calls `tokenize_identifier` (which allocates a `Vec<String>`) and `to_lowercase()` on both inputs. For 200 search names expanded by BFS gather, this is 10 batches × 20 rows × 20 score calls × 2 allocations = ~8,000 tokenization operations. This is the primary cost path for `cqs gather`.
- **Suggested fix:** Build a `HashMap<String, &str>` from `(normalized_name → original_name)` before the loop, then look up the returned chunk's normalized name in O(1) instead of scanning. Alternatively, pre-normalize chunk names returned by the query (SQLite can return `lower(name)`) and use a trie or prefix map for O(log n) assignment.

#### PERF-3: `upsert_chunks_and_calls` duplicates the entire chunk-upsert logic from `upsert_chunks_batch`
- **Difficulty:** medium
- **Location:** src/store/chunks.rs:333-475
- **Description:** `upsert_chunks_and_calls` contains a verbatim copy of the content-hash snapshot, batch INSERT, and FTS per-row upsert logic from `upsert_chunks_batch` (lines 39–155). The ~120-line duplicated block is not a thin wrapper — it re-implements the same 3-phase transaction (hash snapshot → INSERT chunks → FTS upsert). Bug fixes to one (e.g., FTS skip-on-unchanged optimization added to `upsert_chunks_batch`) must be manually mirrored. Beyond maintenance cost, the duplication means every optimization applied to one path must be verified for the other.
- **Suggested fix:** Extract an `async fn upsert_chunks_in_tx(tx, chunks, embedding_bytes, old_hashes) -> Result<()>` helper that both functions call. This unifies the logic and ensures both paths benefit from future optimizations (e.g., batched FTS deletes, hash-based FTS skip).

#### PERF-4: `gather_cross_index` fires N parallel `search_filtered` calls (one per ref seed) with full brute-force scans each
- **Difficulty:** medium
- **Location:** src/gather.rs:480-504
- **Description:** The bridge phase calls `project_store.search_filtered()` once per reference seed via `par_iter()`. With `seed_limit = 5` (default) and each `search_filtered` scanning all chunks in 5,000-row SQLite batches, a 50k-chunk project index requires 5 × 10 batches = 50 full sequential scans within a parallel rayon context. Since `Store` uses a single `tokio::runtime::Runtime` and `SqlitePool`, these parallel `block_on` calls all contend on the connection pool (4 connections, WAL mode). The rayon parallelism doesn't actually help because the async SQLite reads serialize through the pool. Effective throughput is sequential despite the `par_iter`.
- **Suggested fix:** If an HNSW index is available, route the bridge searches through `search_filtered_with_index` (which has O(log n) candidate retrieval). Alternatively, batch all bridge embeddings into a single search call by aggregating candidate sets: collect all bridge embedding vectors, search HNSW once with `k = seed_count * bridge_limit`, then partition results by seed similarity. This reduces 5 sequential scans to 1.

#### PERF-5: Placeholder string generation for `prune_missing` builds two identical placeholder strings per batch
- **Difficulty:** easy
- **Location:** src/store/chunks.rs:514-536
- **Description:** In `prune_missing`, each batch iteration generates the same placeholder string twice: once for the FTS delete query and once for the chunks delete query (lines 515-516 and 531). Both strings are identical (`"?1,?2,...?N"` for the same batch). Two `(1..=batch.len()).map(...).collect().join()` sequences are computed per batch when one would suffice.
- **Suggested fix:** Compute `placeholder_str` once per batch and reuse it for both queries. This is a trivial change that eliminates 50% of placeholder allocations in the prune path.

#### PERF-6: `find_test_chunks_async` and `find_test_chunk_names_async` build identical SQL dynamically on every call
- **Difficulty:** easy
- **Location:** src/store/calls.rs:957-1043
- **Description:** Both `find_test_chunks_async` and `find_test_chunk_names_async` independently call `build_test_content_markers()`, `build_test_path_patterns()`, and format the `filter` clause string on every invocation. The SQL is deterministic for a given binary (language definitions are static). With 14 test calls (`find_test_chunks` is called from ~14 sites, cached via `OnceLock`), only the first call hits the async path — but it still rebuilds the SQL from scratch each time the cache is cold (e.g., across multiple test runs or if the cache is ever invalidated). The comment at line 954 notes `TODO(PF-10): Add OnceLock<Vec<ChunkSummary>> cache`, which is already done for the result. The SQL itself could be pre-built as a `LazyLock<String>`.
- **Suggested fix:** Move the SQL construction into a `static TEST_CHUNK_SQL: LazyLock<String>` and a `static TEST_CHUNK_NAMES_SQL: LazyLock<String>`, built once at startup. This eliminates repeated string formatting and `Vec<String>` allocation for the filter clause on every cold cache access.

#### PERF-7: `embed_batch` mean-pooling loop uses element-wise indexing instead of SIMD-friendly slice operations
- **Difficulty:** medium
- **Location:** src/embedder.rs:566-589
- **Description:** The mean pooling after ONNX inference (lines 566–589) accumulates the hidden state with a nested loop: outer over `seq_len`, inner over `embedding_dim` (768), with an index calculation `offset = i * seq_len * embedding_dim + j * embedding_dim`. The inner loop mutates a `sum` vec element-by-element with scalar multiply-add. For a batch of 32 documents with `seq_len=128` and `dim=768`, this is 32 × 128 × 768 = ~3.1M scalar operations. The compiler may auto-vectorize this, but the non-contiguous memory access pattern (strided by `embedding_dim` within the flat `data` slice) may inhibit SIMD. The `data` slice is a flat contiguous array returned by ort, laid out as `[batch][seq][dim]`, so the inner loop `data[offset..offset+dim]` is contiguous — but it's accessed via index arithmetic rather than slice operations, missing the opportunity to use `ndarray` slice addition or the existing `simsimd` dependency.
- **Suggested fix:** Refactor to use `ndarray` array views: reshape `data` into `Array3<f32>` of shape `[batch, seq, dim]`, then compute mean pooling as `(hidden_state * mask).sum_axis(Axis(1)) / mask_sum` using ndarray operations. This allows ndarray's BLAS backend or SIMD to accelerate the sum, and the code becomes significantly clearer.

#### PERF-8: `sanitize_fts_query` allocates two intermediate strings for every search query
- **Difficulty:** easy
- **Location:** src/store/mod.rs:143-157
- **Description:** `sanitize_fts_query` first collects a filtered char iterator into `cleaned: String`, then splits it on whitespace, filters boolean operators, collects into `Vec<&str>`, and joins with `" "`. This is two heap allocations (the intermediate `cleaned` string and the `Vec<&str>`) plus a final `join` allocation for every search query. Called from `search_fts`, `search_by_name`, `search_filtered`, `search_by_candidate_ids`, and `search_by_names_batch` — i.e., on every search path. The input is typically already clean (most queries contain no FTS operators), making both allocations common-case waste.
- **Suggested fix:** Check first if any special characters are present before allocating. If the input is clean (no `"*()+-^:` and no standalone `OR`/`AND`/`NOT`/`NEAR` words), return `Cow::Borrowed(s)` directly. Only allocate for the uncommon case. Alternatively, combine the two passes into one: stream characters into the output, detect word boundaries inline, and skip operator words without the intermediate string.

#### PERF-9: `strip_markdown_noise` applies 6 regex replacements with repeated string allocations
- **Difficulty:** easy
- **Location:** src/nl.rs:503-517
- **Description:** `strip_markdown_noise` chains 4 regex `replace_all` calls followed by 4 `.replace()` calls for bold/italic markers (`***`, `**`, `*`, backticks). Each `replace_all` may allocate a new `String` (or return `Cow::Borrowed` if no match), but the 4 `.replace()` calls each unconditionally allocate a new `String` even when the pattern is absent. For markdown docs without any bold/italic/code markers (common for plain-text READMEs), all 4 allocations are wasted. This is called once per markdown section chunk during embedding — with a 5k-section documentation index, that's 20k unnecessary `String` allocations.
- **Suggested fix:** Check `.contains("**")` / `.contains('*')` / `.contains('`')` before calling `.replace()`. For markdown-heavy codebases, consider combining the `***`/`**`/`*` replacements into a single regex `replace_all` pass. The `Cow<str>` pattern already used by `replace_all` could be extended to the manual replacements via `std::borrow::Cow::Borrowed`/`Cow::Owned` branching.

#### PERF-10: `find_dead_code` `fetch_active_files` runs two separate full-table scans of `chunks` and `type_edges`
- **Difficulty:** easy
- **Location:** src/store/calls.rs:807-838
- **Description:** `fetch_active_files` executes two independent queries: one joining `chunks` with `function_calls` to find files with callers, and one joining `chunks` with `type_edges` to find files with type activity. Both queries do a full scan of `chunks` (`SELECT DISTINCT c.origin FROM chunks c INNER JOIN ...`). For a 50k-chunk index these are two full table scans that could be collapsed into a single `UNION` query: `SELECT DISTINCT origin FROM chunks WHERE id IN (SELECT source_chunk_id FROM type_edges) UNION SELECT DISTINCT c.origin FROM chunks c INNER JOIN function_calls fc ON c.name = fc.callee_name`. This halves the I/O for dead code analysis.
- **Suggested fix:** Combine into one query using `UNION`: `SELECT DISTINCT c.origin FROM chunks c INNER JOIN function_calls fc ON c.name = fc.callee_name UNION SELECT DISTINCT c.origin FROM chunks c JOIN type_edges te ON c.id = te.source_chunk_id`. Single `fetch_all` call, one round trip.

## Data Safety

#### DS-1: `add_reference_to_config` and `remove_reference_from_config` cross-device copy fallback is non-atomic
- **Difficulty:** easy
- **Location:** src/config.rs:347-357, src/config.rs:420-430
- **Description:** Both functions use the pattern `if rename fails → std::fs::copy(&tmp_path, config_path)`. `std::fs::copy` writes directly to the final destination path — not atomic. If the process crashes mid-copy (e.g., disk full, SIGKILL), `config_path` is left as a truncated partial file, permanently destroying the user's `.cqs.toml`. Compare to `note.rs:266-292` which correctly copies to a same-directory temp file first, then performs an atomic same-device rename: `let dest_tmp = dest_dir.join(...)`, `copy(&tmp, &dest_tmp)`, `rename(&dest_tmp, notes_path)`. The config functions skip this intermediate step. This path fires on Docker overlayfs and some NFS configurations where `rename(2)` returns `EXDEV`.
- **Suggested fix:** Apply the same three-step pattern used in `note.rs`: (1) copy `tmp_path` to `dest_dir/.config.{suffix}.tmp` (guaranteed same device as `config_path`), (2) `rename(dest_tmp, config_path)` (atomic), (3) clean up `tmp_path`. Both functions share the same fix pattern.

#### DS-2: `schema_version` parse failure silently treats database as version 0, bypassing migration guard
- **Difficulty:** easy
- **Location:** src/store/mod.rs:442-459
- **Description:** `check_schema_version` reads `schema_version` from the metadata table as a TEXT row and parses it as `i32`. If the parse fails (e.g., the stored value is `"eleven"`, `""`, or any non-integer due to manual editing or bit-flip), the code warns and defaults `version` to `0`. The subsequent migration guard is `if version < CURRENT_SCHEMA_VERSION && version > 0` — which is false when `version == 0`. The database is then opened and used without migration or error. While in practice the on-disk schema still matches v11 (since v11 code wrote it), the guard's intent is to catch version mismatches. A corrupted `schema_version` bypasses this check entirely, and any future caller relying on the version value (e.g., conditional logic, stats reporting) gets `0` instead of the real version.
- **Suggested fix:** Treat a non-parseable `schema_version` row as an error rather than defaulting to 0: `s.parse::<i32>().map_err(|_| StoreError::Corruption(format!("schema_version is not an integer: {:?}", s)))`. The `no such table` path (fresh uninitialized DB) and the `row is None` path (key absent) both correctly return `Ok(())` — only the "row exists but value is garbage" path needs fixing.

#### DS-3: `ProjectRegistry::load()` reads without a file lock — size check and read are TOCTOU
- **Difficulty:** easy
- **Location:** src/project.rs:32-51
- **Description:** `load()` performs a metadata size check (`std::fs::metadata(&path)`) and then a separate `std::fs::read_to_string(&path)`. Between the size check and the read, another process could replace the file with a large one, bypassing the 1MB guard and causing a large allocation. More practically: a concurrent `save()` could rename a new file into place between the two calls, causing `load()` to read a partially-visible intermediate state. Contrast with `rewrite_notes_file` (note.rs:186-206) and `add_reference_to_config` (config.rs:282-293), both of which open the file once and hold an exclusive or shared lock through the full read. `load()` uses no lock.
- **Suggested fix:** Open the file once and read through the handle: `let mut f = std::fs::File::open(&path)?; f.lock_shared()?; let meta = f.metadata()?; if meta.len() > MAX { bail!(...); } let mut content = String::new(); f.read_to_string(&mut content)?;`. This is the exact pattern used in `parse_notes` (note.rs:135-175).

#### DS-4: `call_graph_cache` and `test_chunks_cache` use `OnceLock` — invalidation is impossible after mutation, and the comment does not document the actual lifetime invariant
- **Difficulty:** easy
- **Location:** src/store/mod.rs:191-193, src/store/calls.rs:419-460, src/store/calls.rs:1074-1081
- **Description:** `Store` holds `call_graph_cache: OnceLock<CallGraph>` and `test_chunks_cache: OnceLock<Vec<ChunkSummary>>`. `OnceLock` cannot be reset — once populated, it returns the cached value forever. The only guarantee of correctness is that no mutation of `calls` or `chunks` occurs on a Store instance after these caches are populated. This invariant is upheld today because: batch mode never mutates (read-only), CLI commands are single-shot (fresh Store per invocation), and watch mode creates a fresh Store. However: (1) there is no enforcement — any future code path that writes to `calls` or `chunks` after calling `get_call_graph()` on the same Store would silently use stale data; (2) the comment on `get_call_graph` says "Each CLI invocation creates a fresh Store, so the cache is valid for the invocation's lifetime" but watch mode reindexes via the same Store instance and calls `upsert_chunks_and_calls` repeatedly; watch mode does not call `get_call_graph()` itself, but it is one misplaced call away from a stale-cache bug. (3) `upsert_calls_batch` at calls.rs:956 has a TODO comment: "Cache should invalidate on chunk upsert" — confirming the cache was intended to be invalidatable.
- **Suggested fix:** Replace `OnceLock<CallGraph>` with `RwLock<Option<CallGraph>>` (matching the existing `notes_summaries_cache` pattern). Add `invalidate_call_graph_cache()` and call it from `upsert_calls`, `upsert_calls_batch`, and `upsert_chunks_and_calls`. This closes the latent bug and satisfies the TODO at calls.rs:956. The read path stays fast (most callers hold the lock only for a clone).

#### DS-5: `run_index_pipeline` opens a second `Store` on the same database file simultaneously with `cmd_index`'s store
- **Difficulty:** medium
- **Location:** src/cli/pipeline.rs:781, src/cli/commands/index.rs:68-80
- **Description:** `cmd_index` opens `Store::open(&index_path)` (up to 4 connections via the pool), then calls `run_index_pipeline(&root, files, &index_path, ...)` which opens another `Store::open(store_path)` (another 4-connection pool) on the same file. Two independent connection pools with separate WAL visibility and independent `quick_check` runs are live simultaneously. SQLite WAL mode correctly serializes writers, so no data corruption occurs. However: (1) the two Stores hold independent `call_graph_cache` / `test_chunks_cache` / `notes_summaries_cache` states — any read through `cmd_index`'s store sees different cache state than the pipeline's store; (2) at peak: 8 open connections (4+4) per database, hitting SQLite's default `max_connections` warnings; (3) `cmd_index`'s store then calls `prune_missing()` after the pipeline closes its store — this is the intended ownership but is fragile if the pipeline is ever refactored to return its store.
- **Suggested fix:** Pass `store: Arc<Store>` into `run_index_pipeline` instead of `store_path: &Path`. This eliminates the second open, unifies cache state, and halves connection pool pressure. `cmd_index` constructs the single Store and shares it via `Arc`. The pipeline threads take `Arc::clone(&store)` (already the pattern for `parser`).

#### DS-6: Migration runs inside `check_schema_version` which is called inside `Store::open`, but the migrated schema is committed before the Store is fully initialized
- **Difficulty:** medium
- **Location:** src/store/mod.rs:459-476, src/store/mod.rs:279-285
- **Description:** When `Store::open` finds an outdated schema, it calls `migrations::migrate()` inside `check_schema_version()`, which commits the schema upgrade. This migration runs before `check_model_version()` and `check_cq_version()` (lines 281-283). If `check_model_version()` then returns an error (e.g., a model mismatch because the user tried to open an old index with a new model), the migration is already committed — the schema has been upgraded but the Store is never returned to the caller. On the next `Store::open` call, `check_schema_version()` now finds the new schema version and succeeds, but the model still mismatches. The schema upgrade is irreversible (there is no downgrade migration path). While the schema changes are additive (new tables/columns with IF NOT EXISTS) and idempotent, the committed schema version number makes it impossible to detect the original version after the fact. More critically: if the migration fails partway through (e.g., mid-transaction crash), the metadata UPDATE at `migrations.rs:48-50` is in the same transaction as the DDL — SQLite rolls back the entire transaction on crash, restoring the original schema. However, if the transaction commits but the caller then fails, the schema is upgraded with no record that initialization was incomplete.
- **Suggested fix:** Reorder the checks: (1) `check_model_version()` first (fails fast if model mismatch, no migration needed), (2) `check_schema_version()` / migrate second, (3) `check_cq_version()` last (advisory only). This ensures the expensive and irreversible migration only runs when the store is actually usable. Add a comment documenting that migration ordering is intentional.

## Security

#### SEC-1: `assert!` in FTS query construction crashes process on invariant regression
- **Difficulty:** easy
- **Location:** src/store/mod.rs:623-626, src/store/chunks.rs:1189-1192
- **Description:** Two production code sites use `assert!` (not `debug_assert!`) to check that `sanitize_fts_query` did not produce a string containing `"` before interpolating it into a `format!`-built FTS5 MATCH argument (`name:"{}"`). If this invariant is violated — due to a future change to `sanitize_fts_query`, a Unicode normalization edge case in `normalize_for_fts`, or any caller bypassing sanitization — the process panics with a user-visible backtrace. This crashes `cqs search --name-only`, `cqs explain`, `cqs callers`, `cqs gather`, and all batch/chat commands that invoke name lookup. The CLAUDE.md convention "No `unwrap()` except in tests" applies equally to `assert!` in production library code. The assertion fires on every `--name-only` call and every BFS expansion in `gather`. The actual FTS5 injection risk from a `"` surviving is real (it would close the quoted phrase token and allow arbitrary FTS5 operator injection in the MATCH argument), making the guard necessary — but the guard should return a `StoreError`, not panic.
- **Suggested fix:** At `store/mod.rs:623`, replace the `assert!` with: `if normalized.contains('"') { return Err(StoreError::Runtime("FTS sanitization invariant violated: double quote in normalized query".into())); }`. In `chunks.rs:1189`, the check is inside a closure; hoist it out: `let fts_terms: Result<Vec<_>, StoreError> = batch.iter().map(|(_, norm)| { if norm.contains('"') { return Err(StoreError::Runtime(...)); } Ok(format!("name:\"{}\" OR name:\"{}\"*", norm, norm)) }).collect(); let fts_terms = fts_terms?;`.

#### SEC-2: `validate_ref_name` permits dot-prefixed reference names, creating hidden directories
- **Difficulty:** easy
- **Location:** src/reference.rs:214-228
- **Description:** `validate_ref_name` rejects empty, null-byte, slash-containing, and `.` names, but accepts names starting with `.` (e.g., `.hidden`, `.git`, `.cqs`). On Unix, `refs_dir().join(".git")` creates `~/.local/share/cqs/refs/.git`, which may be confused for a git repository by external tooling or ignored by backup software. A reference named `.cqs` would place a second index tree inside the refs directory, potentially confusing other `cqs` commands that look for `.cqs` directories. The containment check in `cmd_ref_remove` (canonicalize + `starts_with`) is correct and prevents directory escape — this is a naming-surprise issue, not a traversal vulnerability.
- **Suggested fix:** Add one line to `validate_ref_name`: `if name.starts_with('.') { return Err("Reference name cannot start with '.'"); }`. This is consistent with the existing rejection of `.` and `..` (already blocked via the `contains("..")` check for `..`, and the `name == "."` exact check for `.`).

#### SEC-3: CHM and PDF file paths are passed to external processes without leading-dash protection
- **Difficulty:** easy
- **Location:** src/convert/chm.rs:31-32, src/convert/pdf.rs:22
- **Description:** `chm_to_markdown` calls `.args(["x", path_str.as_ref(), output_arg.as_str(), "-y"])`, passing the user-supplied CHM file path as the second positional argument to 7z. If the path starts with `-` (e.g., a file named `-v.chm`, or a relative path like `-output/file.chm`), 7z interprets it as an option flag (`-v` triggers volume splitting, `-r` enables recursion, `-p` expects a password argument next, etc.). The same issue exists in `pdf_to_markdown` at pdf.rs:22: `path.to_string_lossy().to_string()` is passed as a Python positional argument and Python passes it verbatim to the script's `sys.argv`. The `blame.rs` command guards against this exact pattern with an explicit `starts_with('-')` check (line 79) and the git `--no-pager log` prefix ensures git does not treat the file argument as an option. The convert module has no equivalent protection.
- **Suggested fix:** For CHM: insert `"--"` before the path argument to signal end-of-options to 7z: `.args(["x", "--", path_str.as_ref(), output_arg.as_str(), "-y"])`. Note that older 7z builds may not support `--`; an alternative is to prepend `./` for relative paths: `let safe_path = if path_str.starts_with('-') { format!("./{}", path_str) } else { path_str.into_owned() }`. For PDF: the Python script receives `path` via `sys.argv[1]`; add `"--"` before it in the args list, or prepend `./` for relative paths.

#### SEC-4: FTS5 MATCH argument embeds sanitized user input via `format!` — sanitization is the sole injection barrier
- **Difficulty:** medium
- **Location:** src/store/mod.rs:627, src/store/chunks.rs:1193
- **Description:** Both `search_by_name` and `search_by_names_batch` construct FTS5 MATCH strings by interpolating sanitized user input: `format!("name:\"{}\" OR name:\"{}\"*", normalized, normalized)`. This string is then bound via `?1` and evaluated by SQLite's FTS5 parser at query time. `sanitize_fts_query` correctly strips `"`, `*`, `(`, `)`, `+`, `-`, `^`, `:`, and filters FTS5 boolean operators. However, FTS5 MATCH argument parsing is done by SQLite internally, and the sanitized string content is not further parameterized — `sanitize_fts_query` is the only layer preventing injection into FTS5 query syntax. This is unlike the rest of the SQL queries which are fully parameterized. There are no tests verifying that arbitrary Unicode input cannot produce a `"` or `:` in the sanitized output (e.g., via Unicode folding of confusable characters). The `assert!` at SEC-1 serves as the runtime guard, but with no property-based tests there is no fuzz coverage of edge cases.
- **Suggested fix:** Add a property-based fuzz test (via `proptest` or `quickcheck`) in the `sanitize_fts_query` module that feeds arbitrary `String` values and asserts the result contains none of `'"', '*', '(', ')', '+', '-', '^', ':'`. Run it in CI. Add a comment to `sanitize_fts_query`'s doc: "This function is the sole injection barrier for all FTS5 MATCH queries. FTS5 MATCH arguments cannot be further parameterized at the token level — do not remove or weaken any of the filtered characters without a full FTS5 security review."

## Resource Management

#### RM-1: `HnswIndex::build` doubles peak memory via simultaneous flat buffer and chunked Vec
- **Difficulty:** easy
- **Location:** src/hnsw/build.rs:57-79
- **Description:** `HnswIndex::build` calls `prepare_index_data` which returns a flat `Vec<f32>` buffer containing all embeddings (e.g. 50k × 769 × 4 bytes ≈ 154MB). Immediately after, the code re-slices this buffer into `Vec<Vec<f32>> chunks` (another ≈154MB), holding both simultaneously before passing `chunks` to `parallel_insert_data`. Peak memory is 2× the embedding data size. This path is documented as test-only, and production uses `build_batched`, but any test exercising `build` with a large dataset hits the spike, and the pattern is misleading for anyone reading the code.
- **Suggested fix:** Either `drop(data);` immediately after building `chunks` (while `data` is still in scope but no longer needed), or eliminate the flat buffer entirely by building `chunks` directly during `prepare_index_data`. Annotate the method with a doc note: "Peak memory is O(2×n×dim×4 bytes) due to simultaneous flat buffer + chunked Vec."

#### RM-2: `count_vectors` deserializes full id map to count entries
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs (the `count_vectors` function)
- **Description:** `count_vectors` reads and deserializes the entire `Vec<String>` id map from the `.hnsw.graph` file (a bincode-serialized structure) just to call `.len()` on the result. For a 100k-vector index, the id map alone is ~100k × ~15 bytes average chunk-id string ≈ 1.5MB on disk, fully materialized into heap strings. The only caller uses this count as the `estimated_total` hint for `build_batched`, so approximate accuracy is sufficient.
- **Suggested fix:** Store the vector count as a u64 at a known offset at the head of the `.hnsw.graph` file (or a sidecar `.hnsw.count` file). For backward compatibility, fall back to the full-deserialize path when the count header is absent, with a migration note.

#### RM-3: CAGRA GPU index retains full CPU-side dataset copy indefinitely
- **Difficulty:** hard
- **Location:** src/cagra.rs:64 (`dataset: Array2<f32>` field)
- **Description:** `CagraIndex` stores `dataset: Array2<f32>`, a full CPU copy of the embedding matrix, alongside the GPU index. For a 50k-vector index this is 50k × 769 × 4 bytes ≈ 154MB of CPU RAM held for the lifetime of the index — in addition to the GPU VRAM consumed by the cuVS CAGRA index. This doubles the RAM footprint of the GPU path compared to the HNSW path. The CPU copy is retained because `cuvs::cagra::Index` consumes its input on construction and rebuilding the GPU index after every search would be prohibitively expensive. This is tracked as issue #389.
- **Suggested fix:** Investigate whether the cuVS CAGRA API exposes a non-consuming search path (i.e., can the index be searched without consuming the dataset). If not, document the 2× memory multiplier prominently in the GPU configuration docs and `CagraIndex` doc comment. Consider a flag to release the dataset if the user opts into "no-rebuild" mode (search-only after first build).

#### RM-4: Watch mode full HNSW rebuild holds old and new index in memory simultaneously
- **Difficulty:** easy
- **Location:** src/cli/watch.rs (the full-rebuild branch, around the `build_hnsw_index_owned` call)
- **Description:** When the incremental-change threshold is exceeded and a full HNSW rebuild is triggered, the code calls `build_hnsw_index_owned(...)` which allocates a complete new index while the old `hnsw_index: Option<HnswIndex>` still occupies memory. The assignment `*hnsw_index = Some(new_index)` then drops the old index, but only after the new one is fully built. For a 50k-vector index this is a transient ~300MB spike (old ≈ 150MB + new build peak ≈ 150MB during `parallel_insert_data`).
- **Suggested fix:** Add `*hnsw_index = None;` immediately before calling `build_hnsw_index_owned`. This releases the old index before the new one is allocated, capping peak memory at O(1×) rather than O(2×) during rebuild. Confirm via `MALLOC_STATS` or `valgrind --tool=massif` that the drop is eager before the build.

#### RM-5: `Store::open` creates a multi-threaded tokio runtime when `current_thread` suffices
- **Difficulty:** medium
- **Location:** src/store/mod.rs (the `Runtime::new()` call in `Store::open`)
- **Description:** `Store::open` calls `Runtime::new()` which creates a multi-threaded scheduler with one thread per CPU core (e.g. 16 threads on a 16-core machine). All of these threads sit idle waiting for tokio tasks; all actual work is synchronous SQLite I/O. `Store::open_readonly` correctly uses `Builder::new_current_thread()`. Every `cqs` subcommand that writes to the store (index, watch, notes add/update/remove, gc) spawns a full thread pool that is never utilized beyond the single calling thread.
- **Suggested fix:** Change `Runtime::new()` in `Store::open` to `Builder::new_current_thread().enable_all().build()`. This matches `open_readonly` and eliminates O(CPU) idle threads. If any sqlx operation specifically requires a multi-thread scheduler, add a targeted comment explaining why.

#### RM-6: `verify_checksum` reads the full 547MB ONNX model on every startup
- **Difficulty:** medium
- **Location:** src/embedder.rs (the `verify_checksum` function, called from `load_session`)
- **Description:** `verify_checksum` opens the `.onnx` model file and streams all 547MB through blake3 on every startup that generates embeddings (i.e., every `cqs index`, `cqs search`, `cqs gather`, watch mode startup, and batch mode startup). On a typical NVMe SSD, streaming 547MB takes 500ms–1s; on a spinning disk or network filesystem it can take several seconds. This cost is paid even when the model file has not changed since the last run.
- **Suggested fix:** Cache the verification result in a sentinel file (e.g., `.cqs/model.verified`) storing the model file's mtime and size. On startup, if the sentinel's mtime+size matches the current model file, skip the full hash. If mismatched, re-verify and update the sentinel. This reduces verification to a two-stat call on the hot path, with a fallback full read on model updates. Include the blake3 hash in the sentinel for belt-and-suspenders integrity.

#### RM-7: `BatchContext` structural caches are `OnceLock` — never released during idle timeout
- **Difficulty:** easy
- **Location:** src/cli/batch/mod.rs (the `call_graph_cache`, `test_chunks_cache`, `notes_cache` fields)
- **Description:** `BatchContext` uses `OnceLock<CallGraph>`, `OnceLock<Vec<ChunkSummary>>`, and `OnceLock<Vec<Note>>` for the three heaviest caches. `OnceLock` values cannot be cleared after being set. The idle timeout logic (triggered after `IDLE_TIMEOUT_MINUTES = 5` of inactivity) correctly clears the ONNX embedding and reranker sessions, but cannot clear these structural caches — they persist for the entire lifetime of the batch session. A `CallGraph` for a 100k-chunk codebase can occupy tens of MB; `Vec<ChunkSummary>` is proportional to chunk count. In a long-running `cqs chat` session querying a large codebase, these caches grow and stay.
- **Suggested fix:** Convert `notes_cache` (most volatile — notes are updated by other commands) from `OnceLock<Vec<Note>>` to `Mutex<Option<Vec<Note>>>` so it can be cleared during idle timeout and on note mutation. For `call_graph_cache` and `test_chunks_cache`, add size logging when they are first populated (e.g., `tracing::debug!(call_graph_edges = ..., "call graph cache populated")`). Document the intentional trade-off in the struct's doc comment.

#### RM-8: HNSW lock file not included in `HNSW_ALL_EXTENSIONS` — survives `cqs gc --delete`
- **Difficulty:** easy
- **Location:** src/hnsw/persist.rs (the `HNSW_ALL_EXTENSIONS` constant)
- **Description:** `save` and `load` both create a `{basename}.hnsw.lock` advisory lock file. `HNSW_ALL_EXTENSIONS` lists all known HNSW file extensions for use by `cqs gc` when identifying and removing stale index files. `"hnsw.lock"` is not in this list. When `cqs gc --delete` removes a stale HNSW index, it deletes `.hnsw.graph`, `.hnsw.data`, and `.hnsw.ids`, but leaves behind the orphaned `.hnsw.lock` file. Over repeated cycles (e.g., watch mode with full rebuilds, or repeated `cqs index` calls), these files accumulate and are never cleaned up.
- **Suggested fix:** Add `"hnsw.lock"` to `HNSW_ALL_EXTENSIONS`. Verify that `gc` iterates this constant when collecting files to delete (it should, since `HNSW_ALL_EXTENSIONS` is the single source of truth for HNSW file extensions).

#### RM-9: `embed_documents` has no internal batch-size cap — callers can submit unbounded GPU batches
- **Difficulty:** medium
- **Location:** src/embedder.rs (the `embed_documents` / `embed_batch` call path)
- **Description:** `embed_documents` accepts a `Vec<String>` of arbitrary length and passes it in a single call to the ONNX session runner without internal chunking. The GPU batch size is bounded externally by `pipeline.rs` (`EMBED_BATCH_SIZE = 32`) and `cli/commands/index.rs` (`HNSW_BATCH_SIZE = 10_000`), but no guard exists inside `embed_batch` itself. Any caller that bypasses the pipeline and calls `embed_documents` directly (e.g., `embed_query` for a batch of queries, `index_notes_from_file`, custom CLI commands) can accidentally submit a batch of thousands of documents, causing GPU OOM or host OOM on large inputs. The ONNX Runtime does not internally chunk large batches.
- **Suggested fix:** Add a compile-time constant `MAX_EMBED_BATCH: usize = 64` in `embedder.rs`. Inside `embed_batch`, chunk the input into windows of `MAX_EMBED_BATCH` and concatenate the results. This makes the batch-size constraint enforcement the responsibility of `embed_batch` rather than every caller. Keep the external `EMBED_BATCH_SIZE = 32` in the pipeline as a tuning parameter (it can remain smaller than `MAX_EMBED_BATCH` for throughput control).

#### RM-10: `reindex_files` in watch mode scans all call edges per file — O(files × total_calls)
- **Difficulty:** easy
- **Location:** src/cli/watch.rs (the `reindex_files` function, flat `all_calls` Vec scan)
- **Description:** During watch mode, `reindex_files` collects all call edges from the store into a flat `Vec<(String, CallSite)>` (`all_calls`), then for each changed file linearly scans `all_calls` to find edges belonging to that file. This is O(files_changed × total_call_edges). For a codebase with 10k files and 100k call edges, each watch cycle with 10 changed files does 1M comparisons. The `CallSite` struct contains a file path that is compared on every iteration. This is the dominant CPU cost of incremental watch cycles on large codebases.
- **Suggested fix:** Replace the flat scan with a `HashMap<PathBuf, Vec<(String, CallSite)>>` built once from `all_calls` before the per-file loop: `let calls_by_file: HashMap<_, _> = all_calls.iter().group_by(|(_, cs)| &cs.file).into_iter().map(...).collect();`. Each per-file lookup is then O(1). Alternatively, add a store query to fetch call edges filtered by file path rather than loading all edges into memory.
