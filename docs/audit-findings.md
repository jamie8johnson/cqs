# Audit Findings — v0.12.3

Generated: 2026-02-13

---

# Batch 1: Code Quality, Documentation, API Design, Error Handling, Observability

## Code Quality

#### CQ-1: `review_diff()` double-loads call graph and test chunks
- **Difficulty:** easy
- **Location:** `src/review.rs:104-122`
- **Description:** `review_diff()` calls `analyze_diff_impact(store, changed)` which internally loads `store.get_call_graph()` and `store.find_test_chunks()` (see `src/impact/diff.rs:89-90`). Then `review_diff()` loads both again at lines 107-114 for `compute_risk_batch()`. This doubles two moderately expensive queries (full SQL scan of `function_calls` + test chunk heuristic queries). The data is identical both times.
- **Suggested fix:** Either (a) have `analyze_diff_impact` return or accept the pre-loaded graph/test_chunks, or (b) add an `analyze_diff_impact_with_graph()` variant that takes `&CallGraph` and `&[ChunkSummary]` so `review_diff()` can load once and pass to both.

#### CQ-2: `gather()` and `gather_cross_index()` share ~80 lines of duplicated BFS + chunk assembly
- **Difficulty:** medium
- **Location:** `src/gather.rs:182-289` vs `src/gather.rs:441-553`
- **Description:** The BFS expansion loop, batch-fetch + dedup, and sort-truncate-resort are structurally identical between the two functions. They differ only in seed source and final sort order. Changes to BFS logic, decay scoring, or chunk assembly must be applied in both places.
- **Suggested fix:** Extract a `fn expand_and_fetch(store, name_scores, graph, opts, project_root) -> (Vec<GatheredChunk>, bool, bool)` helper.

#### CQ-3: `find_transitive_callers()` reimplements reverse BFS with N+1 store queries
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:141-182`
- **Description:** Hand-rolls a reverse BFS that is structurally identical to `reverse_bfs()` in `src/impact/bfs.rs`, but also calls `store.search_by_name(caller_name, 1)` inside the BFS loop for each discovered caller. N+1 query pattern. The batch version (`store.search_by_names_batch()`) is available and used elsewhere.
- **Suggested fix:** Use `reverse_bfs(graph, target_name, depth)` from `bfs.rs`, then `search_by_names_batch()` once.

#### CQ-4: `review::CallerEntry` / `review::TestEntry` near-duplicate impact types
- **Difficulty:** easy
- **Location:** `src/review.rs:46-61` vs `src/impact/types.rs:10-62`
- **Description:** Four new public types that are thin wrappers over existing ones, differing only in String vs PathBuf for file fields and an optional snippet. `review_diff()` manually converts by cloning each field.
- **Suggested fix:** Derive `serde::Serialize` on impact types and use them directly, or add `file_display: String` to existing types.

#### CQ-5: Four reference search functions differ only in weight application
- **Difficulty:** easy
- **Location:** `src/reference.rs:87-169`
- **Description:** `search_reference`, `search_reference_unweighted`, `search_reference_by_name`, `search_reference_by_name_unweighted` — weighted/unweighted variants differ by ~5 lines. Doubles API surface.
- **Suggested fix:** Collapse to two functions with an `apply_weight: bool` parameter.

#### CQ-6: `convert_file()` and `convert_webhelp()` duplicate post-processing pipeline
- **Difficulty:** easy
- **Location:** `src/convert/mod.rs:136-185` vs `src/convert/mod.rs:267-306`
- **Description:** ~35 lines of identical clean → title → filename → resolve → write → result logic.
- **Suggested fix:** Extract `fn finalize_output(raw_markdown, source, format, opts, check_source_overwrite) -> Result<ConvertResult>`.

#### CQ-7: `get_caller_counts_batch()` and `get_callee_counts_batch()` structurally identical
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:834-910`
- **Description:** Identical batching structure, differ only in SQL query string. ~35 lines duplicated.
- **Suggested fix:** Extract private `fn batch_count_query(&self, sql_template, names) -> Result<HashMap<String, u64>>`.

## Documentation

#### DOC-1: README missing `cqs review` command
- **Difficulty:** easy
- **Location:** README.md (~line 179-208)
- **Description:** New v0.12.3 command with no user-facing documentation.
- **Suggested fix:** Add Diff Review section with usage examples.

#### DOC-2: README missing `--tokens` flag documentation
- **Difficulty:** easy
- **Location:** README.md (~line 56-95)
- **Description:** Token budgeting flag works across 5 commands, not mentioned in README.
- **Suggested fix:** Add to Filters/Output options section with examples.

#### DOC-3: README missing `--ref` scoped search flag
- **Difficulty:** easy
- **Location:** README.md (~line 272-300)
- **Description:** Ref-scoped search not documented in Reference Indexes or Filters section.
- **Suggested fix:** Add examples to Reference Indexes section.

#### DOC-4: CONTRIBUTING.md architecture lists `impact.rs` but it's now `impact/` directory
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:143
- **Description:** PR #402 split impact.rs into 7 files. Architecture overview still lists single file.
- **Suggested fix:** Replace with directory listing.

#### DOC-5: CONTRIBUTING.md commands/ listing missing `review.rs`
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:89
- **Description:** New CLI command file not listed in architecture overview.
- **Suggested fix:** Add to command file list.

#### DOC-6: CONTRIBUTING.md missing `review.rs` library module
- **Difficulty:** easy
- **Location:** CONTRIBUTING.md:143-147
- **Description:** `src/review.rs` not listed alongside other library modules.
- **Suggested fix:** Add entry for review.rs.

#### DOC-7: CHANGELOG missing comparison URLs for v0.12.2 and v0.12.3
- **Difficulty:** easy
- **Location:** CHANGELOG.md:865-866
- **Description:** Footer links missing for two versions. `[Unreleased]` still points to v0.12.1.
- **Suggested fix:** Update footer comparison URLs.

#### DOC-8: SECURITY.md missing `cqs convert` attack surface
- **Difficulty:** medium
- **Location:** SECURITY.md
- **Description:** `cqs convert` shells out to `python3` and `7z`/`7za`/`p7zip`. Subprocess execution not in threat model. `CQS_PDF_SCRIPT` env var allows overriding script path.
- **Suggested fix:** Document subprocess execution trust boundary and env var.

#### DOC-9: ROADMAP shows completed items under "Next" heading
- **Difficulty:** easy
- **Location:** ROADMAP.md:30,38
- **Description:** `cqs review` and "Change risk scoring" have `[x]` but still under "Next" instead of "Recently Completed".
- **Suggested fix:** Move to Recently Completed section.

## API Design

#### AD-12: Impact types missing standard derives (Debug, Clone, Serialize)
- **Difficulty:** easy
- **Location:** `src/impact/types.rs:10-91`
- **Description:** Ten of twelve structs have zero derives. Forces 80+ lines of hand-rolled JSON in format.rs. Review types all derive Serialize and serialize in one line.
- **Suggested fix:** Add `#[derive(Debug, Clone, serde::Serialize)]` to all structs. Replace hand-built JSON in format.rs.

#### AD-13: Leaked opaque types — public fields use unexported types
- **Difficulty:** medium
- **Location:** `src/impact/mod.rs:14-16`, `src/impact/types.rs:72-77`
- **Description:** `DiffImpactResult` and `ImpactResult` are re-exported but their field types (`CallerDetail`, `DiffTestInfo`, etc.) are not. External consumers can access values but cannot name the types.
- **Suggested fix:** Re-export all types used in public struct fields from mod.rs and lib.rs.

#### AD-14: Inconsistent file path types — String vs PathBuf across related types
- **Difficulty:** medium
- **Location:** `src/impact/types.rs:51`, `src/review.rs:39,48,57`
- **Description:** `ChangedFunction.file` is String while sibling types use PathBuf. Review types all use String. No semantic distinction.
- **Suggested fix:** Standardize on one approach.

#### AD-15: Duplicated read_stdin/run_git_diff *(already on roadmap)*
- **Difficulty:** easy
- **Location:** `src/cli/commands/review.rs:60-92` vs `src/cli/commands/impact_diff.rs:72-104`
- **Description:** Identical 33-line functions in two files. Already tracked on ROADMAP.md.

#### AD-16: Review command missing --format option
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:273-284`
- **Description:** `review` only supports `--json`, while related `impact` and `trace` support `--format text|json|mermaid`.
- **Suggested fix:** Add `--format` or document the asymmetry.

#### AD-17: CLI args stringly-typed instead of clap value_enum
- **Difficulty:** easy
- **Location:** `src/cli/mod.rs:252,296,339,350`
- **Description:** `--format`, `--direction`, `--min-confidence` accept String with runtime validation. Could use `#[arg(value_enum)]` for compile-time safety, better errors, shell completion.
- **Suggested fix:** Add `#[derive(clap::ValueEnum)]` to existing enums, change CLI field types.

#### AD-18: RiskScore contains redundant name field
- **Difficulty:** easy
- **Location:** `src/impact/types.rs:115`
- **Description:** `RiskScore.name` duplicates `ReviewedFunction.name`. Every reviewed function stores its name twice.
- **Suggested fix:** Remove `name` from RiskScore, return unnamed scores.

#### AD-19: GatherOptions lacks Debug derive
- **Difficulty:** easy
- **Location:** `src/gather.rs:22`
- **Description:** Cannot be logged or printed. Sibling types `GatherResult` and `GatheredChunk` have Debug.
- **Suggested fix:** Add `#[derive(Debug)]`.

## Error Handling

#### EH-16: Silent file read failure in `resolve_parent_context`
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:600`
- **Description:** `if let Ok(content) = std::fs::read_to_string(...)` silently discards file read errors. Users get degraded `--expand` output with no diagnostic.
- **Suggested fix:** Add `Err` branch with `tracing::warn!`.

#### EH-17: Missing tracing span on `suggest_tests` *(= OB-12)*
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:188`
- **Description:** Public function with store I/O, no `tracing::info_span!` at entry. Invisible in trace output.
- **Suggested fix:** Add entry span.

#### EH-18: `unwrap()` in non-test code for Java test name generation
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:256`
- **Description:** `chars.next().unwrap()` guarded by `!base_name.is_empty()` but violates convention.
- **Suggested fix:** Use byte indexing: `base_name[..1].to_uppercase()`.

#### EH-19: Missing `.context()` on 7z command spawn in CHM conversion
- **Difficulty:** easy
- **Location:** `src/convert/chm.rs:30`
- **Description:** Bare OS error with no indication it came from 7z. Compare with pdf.rs which uses `.context()`.
- **Suggested fix:** Add `.context(format!("Failed to run '{}' for CHM extraction", sevenzip))`.

#### EH-20: Missing `.context()` on fs::write/create_dir_all in convert pipeline
- **Difficulty:** easy
- **Location:** `src/convert/mod.rs:151,177,280,289`
- **Description:** Four bare `?` on filesystem ops. OS errors propagate without path context.
- **Suggested fix:** Add `.with_context()` with path info.

#### EH-21: Regex compiled per-call instead of LazyLock in cleaning rules
- **Difficulty:** easy
- **Location:** `src/convert/cleaning.rs:130,131,160,161,296,319`
- **Description:** Six `Regex::new().unwrap()` per invocation. Convention is `LazyLock<Regex>` with `.expect()`.
- **Suggested fix:** Move to module-level `LazyLock<Regex>` statics.

#### EH-22: `via` field silently defaults to empty string in diff impact
- **Difficulty:** easy
- **Location:** `src/impact/diff.rs:144-148`
- **Description:** `.unwrap_or_default()` on theoretically unreachable path. No diagnostic if triggered.
- **Suggested fix:** Add `tracing::debug!` on fallback, use `"(unknown)"` instead of empty.

## Observability

#### OB-12: `suggest_tests` missing entry span *(= EH-17)*
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:188`
- **Description:** Public function with store I/O, no entry span. Siblings have spans.

#### OB-13: `compute_hints` missing entry span
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:69`
- **Description:** Public wrapper performing three store queries with `?`. No context about which function being analyzed on failure. Siblings `compute_risk_batch` and `find_hotspots` have spans.
- **Suggested fix:** Add entry span with function name.

#### OB-14: `cmd_query_name_only` missing entry span
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:364`
- **Description:** Siblings `cmd_query_ref_only` and `cmd_query_ref_name_only` have entry spans; this one does not.
- **Suggested fix:** Add entry span matching sibling pattern.

---

# Batch 1 Cross-Category Duplicates

1. **EH-17 = OB-12**: `suggest_tests` missing tracing span (fix once)
2. **CQ-4 ≈ AD-14**: Review types vs impact types, String vs PathBuf (related, fix together)
3. **AD-15**: Already tracked on ROADMAP (read_stdin/run_git_diff duplication)

**Batch 1 Total: 34 findings (31 unique after dedup)**

---

# Batch 2: Test Coverage, Robustness, Algorithm Correctness, Extensibility, Platform Behavior

## Test Coverage

#### TC-1: `review_diff()` has zero unit or integration tests
- **Difficulty:** medium
- **Location:** `src/review.rs:88`
- **Description:** Entire `review_diff()` function — the main public API of review.rs — has no tests. Orchestrates 10 steps. Only CLI flag parsing is tested.
- **Suggested fix:** Add integration tests with synthetic diff + seeded store, and CLI test with `--stdin --json`.

#### TC-2: `match_notes()` and `build_risk_summary()` untested
- **Difficulty:** easy
- **Location:** `src/review.rs:194`, `src/review.rs:232`
- **Description:** Private helpers with no tests. `build_risk_summary()` has three-way branch. `match_notes()` has tricky path normalization.
- **Suggested fix:** Unit tests for `build_risk_summary`, integration test for `match_notes`.

#### TC-3: `reverse_bfs_multi()` has zero tests
- **Difficulty:** easy
- **Location:** `src/impact/bfs.rs:40`
- **Description:** New multi-source BFS with shortest-path update logic. Single-source sibling has 3 tests. This one has none.
- **Suggested fix:** Add tests: multi-source non-overlapping, shared ancestor (min depth wins), depth limit, empty targets.

#### TC-4: `gather_cross_index()` has zero tests
- **Difficulty:** hard
- **Location:** `src/gather.rs:300`
- **Description:** 250-line function implementing cross-index gather. Only consumer of `search_reference_unweighted()` and `get_embeddings_by_ids()`. Zero tests.
- **Suggested fix:** Integration tests with two TestStore instances (project + reference).

#### TC-5: Token budgeting (`--tokens`) has no functional tests
- **Difficulty:** medium
- **Location:** `src/cli/commands/query.rs:291-360`
- **Description:** Token budgeting across 6 commands has zero behavioral tests. Only flag parsing verified.
- **Suggested fix:** Integration tests verifying `token_count <= token_budget` in JSON and result truncation.

#### TC-6: `--ref` scoped search has no CLI integration tests
- **Difficulty:** medium
- **Location:** `src/cli/commands/query.rs`, `src/cli/commands/gather.rs`
- **Description:** Library-level reference search tested, but end-to-end CLI `--ref` path untested.
- **Suggested fix:** CLI integration test with `cqs ref add` + `cqs query --ref`.

#### TC-7: `compute_risk_batch()` boundary thresholds not exercised
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:96`
- **Description:** Threshold boundaries (5.0 for High, 2.0 for Medium) not tested precisely. Tests use values well above/below.
- **Suggested fix:** Add exact-boundary tests (score = 5.0, 2.0, 1.99).

#### TC-8: Convert format-specific files have no unit tests
- **Difficulty:** medium
- **Location:** `src/convert/html.rs`, `pdf.rs`, `chm.rs`, `webhelp.rs`
- **Description:** Cleaning (11 tests) and naming (9 tests) are covered. Actual conversion entry points have zero tests.
- **Suggested fix:** Unit tests for HTML with inline snippets; test error paths for PDF/CHM.

#### TC-9: `analyze_diff_impact` test discovery not verified end-to-end
- **Difficulty:** medium
- **Location:** `src/impact/diff.rs:71`
- **Description:** Integration test has right fixture but only asserts on callers, not tests. `all_tests` not verified.
- **Suggested fix:** Assert `result.all_tests` contains expected test with correct `via`.

#### TC-10: `get_embeddings_by_ids()` store method has no tests
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs`
- **Description:** Database query method used by `gather_cross_index()`. Untested — silent degradation path.
- **Suggested fix:** Test in `tests/store_test.rs` with known embeddings.

## Robustness

#### RB-12: `unwrap()` on `chars().next()` for Java test name *(= EH-18)*
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:256`
- **Description:** Safe due to `!is_empty()` guard but violates no-unwrap convention.

#### RB-13: Regex `unwrap()` in cleaning rules *(= EH-21)*
- **Difficulty:** easy
- **Location:** `src/convert/cleaning.rs:130-131,160-161,296,319`
- **Description:** Four `Regex::new().unwrap()` per call instead of `LazyLock` statics.

#### RB-14: Byte-index slicing in naming/cleaning
- **Difficulty:** easy
- **Location:** `src/convert/naming.rs:23,34`, `src/convert/cleaning.rs:117`
- **Description:** `trimmed[2..]` after `starts_with("# ")` — safe for ASCII but `strip_prefix` is more idiomatic.
- **Suggested fix:** Use `strip_prefix("# ")`.

#### RB-15: `upsert_calls_batch` can exceed SQLite variable limit
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:236-243`
- **Description:** `push_values` for unbounded batch inserts. 3 binds per row, limit ~333 rows. Large files with many calls can exceed `SQLITE_MAX_VARIABLE_NUMBER`.
- **Suggested fix:** Chunk at ~300 rows per sub-batch.

#### RB-16: `upsert_function_calls` unbounded batch insert
- **Difficulty:** medium
- **Location:** `src/store/calls.rs:369-380`
- **Description:** Same as RB-15 but 5 binds per row, limit ~199 rows. Generated code can trigger this.
- **Suggested fix:** Chunk at ~190 rows.

#### RB-17: `node_letter` cast safe (non-issue)
- **Difficulty:** easy
- **Location:** `src/impact/format.rs:167-176`
- **Description:** Narrowing cast `as u8` is safe due to `i % 26` range. No action needed.

#### RB-18: `--tokens 0` accepted and produces token_count > token_budget
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:312-314`
- **Description:** Budget of 0 always includes first result. Confusing `token_count > token_budget` in output.
- **Suggested fix:** Reject `--tokens 0` at CLI validation.

#### RB-19: `PathBuf::from("")` fallback in `find_pdf_script`
- **Difficulty:** easy
- **Location:** `src/convert/pdf.rs:64-67`
- **Description:** Harmless but confusing empty path candidate.
- **Suggested fix:** Use `if let Some(p)` instead of `unwrap_or_default()`.

#### RB-20: `search_by_names_batch` total_limit multiplication (non-issue)
- **Difficulty:** easy
- **Location:** `src/store/chunks.rs:1122`
- **Description:** Safe due to batch cap of 20. No action needed.

#### RB-21: Unicode uppercasing edge case (non-issue)
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:256`
- **Description:** `to_uppercase()` on non-ASCII chars. Extreme edge case. No action needed.

#### RB-22: `to_ascii_lowercase()` doesn't handle Unicode in naming
- **Difficulty:** easy
- **Location:** `src/convert/naming.rs:76`
- **Description:** Unicode letters kept but not lowercased. Mixed-case filenames possible.
- **Suggested fix:** Use `c.to_lowercase()` instead.

#### RB-23: `resolve_conflict` random fallback not existence-checked (non-issue)
- **Difficulty:** easy
- **Location:** `src/convert/naming.rs:126-131`
- **Description:** Vanishingly small collision chance. No action needed.

## Algorithm Correctness

#### AC-13: `score_name_match` minimum 0.5 causes batch result misassignment
- **Difficulty:** medium
- **Location:** `src/store/helpers.rs:586`, `src/store/chunks.rs:1152-1164`
- **Description:** `score_name_match` returns 0.5 floor for no match. `search_by_names_batch` checks `score > 0.0`, so every FTS row is assigned to the **first** query name regardless of actual match. Affects gather and diff-impact snippet retrieval.
- **Suggested fix:** Change the else branch in `score_name_match` to return `0.0`.

#### AC-14: Gather BFS decay double-compounds
- **Difficulty:** medium
- **Location:** `src/gather.rs:200-202` (also line 458-460)
- **Description:** Decay `powi(depth+1)` applied to already-decayed parent score. Depth-2 nodes get `decay^3` instead of `decay^2`. Only manifests with `expand_depth >= 2`.
- **Suggested fix:** Use `new_score = base_score * opts.decay_factor` for single multiplicative decay per hop.

#### AC-15: `reverse_bfs_multi` depth accuracy with re-enqueueing
- **Difficulty:** hard
- **Location:** `src/impact/bfs.rs:64-69`
- **Description:** Re-enqueue on shorter path breaks BFS queue ordering. Some depth values may be stale. Practical impact low with max_depth=5.
- **Suggested fix:** Document limitation. Full fix would require priority queue.

#### AC-16: `DiffTestInfo.via` always picks first changed function
- **Difficulty:** easy
- **Location:** `src/impact/diff.rs:143-148`
- **Description:** All changed functions are seeds at depth 0. `.find()` always returns first one. Supposedly fixed in v0.12.1 but logic unchanged after refactoring split.
- **Suggested fix:** Track per-source provenance in multi-source BFS or run per-source BFS.

#### AC-17: Token packing doesn't count JSON structure overhead
- **Difficulty:** easy
- **Location:** `src/cli/commands/query.rs:300-315`
- **Description:** `token_count` underestimates by ~200-500 tokens (metadata fields). Design issue.
- **Suggested fix:** Document that token_count is content-only.

#### AC-18: Gather expansion cap overshoot
- **Difficulty:** easy
- **Location:** `src/gather.rs:194-196`
- **Description:** Cap checked per outer loop iteration, not per neighbor. Hub nodes can overshoot by out-degree (~50 max).
- **Suggested fix:** Add inner-loop cap check.

#### AC-19: `extract_call_snippet_from_cache` wrong for windowed chunks
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:98-106`
- **Description:** Windowed chunk `line_start` differs from function's. Offset may be wrong. `saturating_sub` silently produces 0 on stale data.
- **Suggested fix:** Add bounds check that `call_line` falls within chunk's line range.

#### AC-20: Risk scoring loses blast radius at full coverage
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:116-118`
- **Description:** 100 callers + 100 tests = score 0.0, same as 1 caller + 1 test. Design issue.
- **Suggested fix:** Consider two-factor score retaining blast radius, or document as "untested exposure" metric.

#### AC-21: Context token packing uses file order, not relevance
- **Difficulty:** easy
- **Location:** `src/cli/commands/context.rs:192-198`
- **Description:** With `--tokens`, excludes later-in-file functions regardless of importance.
- **Suggested fix:** Sort by caller count before packing, then re-sort for display.

## Extensibility

#### EXT-13: Risk score thresholds are magic numbers
- **Difficulty:** easy
- **Location:** `src/impact/hints.rs:122-127`
- **Description:** `5.0` (high) and `2.0` (medium) are inline. Not configurable or named.
- **Suggested fix:** Extract to named constants.

#### EXT-14: `DocFormat` requires 5 touch points to add a format
- **Difficulty:** medium
- **Location:** `src/convert/mod.rs:86-95`
- **Description:** Same N-changes-per-variant pattern as #387. Enum + detect + display + convert_file + module.
- **Suggested fix:** Converter trait with registration. Low priority.

#### EXT-15: Token-budget packing logic duplicated across 5 commands
- **Difficulty:** medium
- **Location:** `src/cli/commands/query.rs:292-360` and 4 others
- **Description:** ~30 lines of identical greedy knapsack per command, differing only in item type.
- **Suggested fix:** Generic `token_pack<T>` function with closures.

#### EXT-16: `suggest_test_file` hardcodes per-language test conventions
- **Difficulty:** easy
- **Location:** `src/impact/analysis.rs:288-313`
- **Description:** Disconnected from `define_languages!` macro. No compile-time reminder on new language.
- **Suggested fix:** Move test conventions into `LanguageDef` fields.

#### EXT-17: `is_webhelp_dir` hardcodes `content/` detection
- **Difficulty:** easy
- **Location:** `src/convert/webhelp.rs:16-33`
- **Description:** Only one directory structure recognized. Low priority.
- **Suggested fix:** `--webhelp-content-dir` override.

#### EXT-18: Review command missing `--tokens` support
- **Difficulty:** easy
- **Location:** `src/cli/commands/review.rs`
- **Description:** Unlike 5 other analysis commands, review has no token budget cap.
- **Suggested fix:** Add `--tokens` with greedy packing.

#### EXT-19: Copyright regex hardcodes single vendor's year range
- **Difficulty:** easy
- **Location:** `src/convert/cleaning.rs:130`
- **Description:** `© 2015-2024.*AVEVA Group Limited` will stop matching after year update.
- **Suggested fix:** Broaden year pattern to `\d{4}[-–]\d{4}`.

## Platform Behavior

#### PB-11: `std::canonicalize` in convert overwrite guard fallback
- **Difficulty:** easy
- **Location:** `src/convert/mod.rs:158`
- **Description:** `opts.output_dir.canonicalize()` uses std instead of dunce. UNC prefix mismatch on Windows causes false negative on overwrite guard.
- **Suggested fix:** Use `dunce::canonicalize(&opts.output_dir)`.

#### PB-12: `python3` not available on stock Windows
- **Difficulty:** easy
- **Location:** `src/convert/pdf.rs:19`
- **Description:** Windows uses `python` not `python3`. Low priority (WSL target).
- **Suggested fix:** Try `python3` then fall back to `python`.

#### PB-13: `find_7z` error message assumes Debian/Ubuntu
- **Difficulty:** easy
- **Location:** `src/convert/chm.rs:123`
- **Description:** Install instructions only show `apt`. Should include `brew` alternative.
- **Suggested fix:** Make message generic or platform-aware.

---

# Batch 2 Cross-Category Duplicates

1. **RB-12 = EH-18**: `unwrap()` on chars().next() for Java test name (fix once)
2. **RB-13 = EH-21**: Regex unwrap in cleaning.rs (fix once)
3. **AC-16**: Supposedly fixed in v0.12.1 but logic unchanged after refactoring

**Batch 2 Total: 41 findings (38 unique after dedup, ~5 non-issues)**

---

# Batch 3: Security, Data Safety, Performance, Resource Management

## Security

#### SEC-8: CQS_PDF_SCRIPT arbitrary script execution
- **Difficulty:** easy
- **Location:** `src/convert/pdf.rs:54-58`
- **Description:** `CQS_PDF_SCRIPT` env var allows arbitrary Python script path. Attacker controlling env vars gets code execution on `cqs convert`. Only check is `.exists()` (TOCTOU). Unlike `$EDITOR`, triggered implicitly.
- **Suggested fix:** Log warning when env var active, verify `.py` extension, document risk.

#### SEC-9: CHM symlink escape — arbitrary file read
- **Difficulty:** medium
- **Location:** `src/convert/chm.rs:43-65`
- **Description:** Malicious CHM with symlinks pointing outside temp dir. `walkdir` enumerates symlink files, `std::fs::read()` follows them. Local file disclosure into Markdown output.
- **Suggested fix:** Add `.filter(|e| !e.path_is_symlink())` to walkdir chain.

#### SEC-10: CHM zip-slip — 7z extraction path traversal
- **Difficulty:** medium
- **Location:** `src/convert/chm.rs:26-27`
- **Description:** Crafted CHM with `../` entries. 7z behavior varies by version. Main risk is temp file pollution.
- **Suggested fix:** Verify canonical paths of extracted files are inside `temp_dir.path()`.

#### SEC-11: Webhelp directory traversal via symlinks
- **Difficulty:** medium
- **Location:** `src/convert/webhelp.rs:51-62`
- **Description:** Same symlink pattern as SEC-9 but for webhelp dirs. Easier to exploit (operates on directories, e.g., git repos with symlinks).
- **Suggested fix:** Add `.filter(|e| !e.path_is_symlink())` to walkdir.

## Data Safety

#### DS-7: `upsert_calls_batch` unbounded push_values *(= RB-15/16)*
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:236-243`
- **Description:** Can exceed SQLite variable limit. Same finding as RB-15/16.

#### DS-8: Reference stores opened read-write instead of read-only *(= RM-11)*
- **Difficulty:** easy
- **Location:** `src/reference.rs:56`
- **Description:** `load_references` uses `Store::open()` (rwc, 4 connections) instead of `open_readonly()`. Race condition + unnecessary WAL writes. Previous audit (RM-2) fixed cross-project search but missed this path.
- **Suggested fix:** Change to `Store::open_readonly(&db_path)`.

#### DS-9: Convert filename conflict resolution TOCTOU
- **Difficulty:** medium
- **Location:** `src/convert/naming.rs:99-135`
- **Description:** `exists()` check then write — race window for concurrent `cqs convert`. Low risk for CLI.
- **Suggested fix:** Use `OpenOptions::new().create_new(true)` for atomic create-or-fail.

#### DS-10: `gather_cross_index` no model compatibility check between stores
- **Difficulty:** easy
- **Location:** `src/gather.rs:300-354`
- **Description:** Cross-index bridge search uses embeddings from two different stores. If built with different models, cosine similarity is meaningless — garbage scores with no error.
- **Suggested fix:** Compare `model_name` and `dimensions` metadata between stores before bridge search.

#### DS-11: `find_test_chunks_async` SQL string interpolation (safe, documenting)
- **Difficulty:** easy
- **Location:** `src/store/calls.rs:749-763`
- **Description:** Uses `format!` for SQL but all values are compile-time constants. Safe. Add comment.

#### DS-12: `review_diff` inconsistent error handling across store operations
- **Difficulty:** easy
- **Location:** `src/review.rs:107-113`
- **Description:** `get_call_graph` error aborts review, but `match_notes` and `check_origins_stale` silently degrade. Inconsistent — transient SQLite busy on graph kills review while same error on notes is swallowed.
- **Suggested fix:** Make all consistently degrade with warning flag in ReviewResult.

## Performance

#### PERF-12: Context command N+1 per-chunk caller/callee queries
- **Difficulty:** medium
- **Location:** `src/cli/commands/context.rs:100-143`
- **Description:** Full context mode calls `get_callers_full` + `get_callees_full` per chunk. 20 chunks = 40 queries. Compact mode already batches.
- **Suggested fix:** Add batch versions with `WHERE callee_name IN (...)`.

#### PERF-13: `analyze_diff_impact` per-function caller fetch — N+1
- **Difficulty:** easy
- **Location:** `src/impact/diff.rs:96-111`
- **Description:** `get_callers_with_context` called per changed function. 15 functions = 15 queries.
- **Suggested fix:** Add `get_callers_with_context_batch` with `WHERE callee_name IN (...)`.

#### PERF-14: Cross-index bridge search is sequential *(= RM-15)*
- **Difficulty:** medium
- **Location:** `src/gather.rs:383-418`
- **Description:** N serial `search_filtered` calls. 5 seeds × 10-50ms = 50-250ms. `cmd_query` already parallelizes with rayon.
- **Suggested fix:** Use `rayon::par_iter` over seeds.

#### PERF-15: Token counting per-chunk instead of batch
- **Difficulty:** easy
- **Location:** `src/embedder.rs:301-307`
- **Description:** Individual `encode()` calls. `tokenizers` crate supports `encode_batch()`.
- **Suggested fix:** Add `token_counts_batch()` method.

## Resource Management

#### RM-10: review_diff loads call graph + test chunks twice *(= CQ-1)*
- **Difficulty:** easy
- **Location:** `src/review.rs:89-122`
- **Description:** Same finding as CQ-1.

#### RM-11: Reference stores opened read-write *(= DS-8)*
- **Difficulty:** easy
- **Location:** `src/reference.rs:56`
- **Description:** Same finding as DS-8.

#### RM-12: find_transitive_callers N+1 queries *(= CQ-3)*
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:141-182`
- **Description:** Same finding as CQ-3.

#### RM-13: CHM/WebHelp unbounded memory accumulation
- **Difficulty:** easy
- **Location:** `src/convert/chm.rs:62-100`, `src/convert/webhelp.rs:75-110`
- **Description:** All pages accumulated in Vec<String> with no count/size limit. 500-page CHM → hundreds of MB.
- **Suggested fix:** Add page count limit (1000) and/or output size cap (100 MB).

#### RM-14: suggest_tests N+1 get_chunks_by_origin *(≈ PERF-13 pattern)*
- **Difficulty:** medium
- **Location:** `src/impact/analysis.rs:221`
- **Description:** Per untested caller file chunk loading without batching.
- **Suggested fix:** Batch-fetch with `get_chunks_by_origins_batch`.

#### RM-15: gather_cross_index N bridge searches *(= PERF-14)*
- **Difficulty:** medium
- **Location:** `src/gather.rs:382-418`
- **Description:** Same finding as PERF-14.

---

# Batch 3 Cross-Category Duplicates

1. **DS-7 = RB-15/16**: SQLite variable limit (fix once)
2. **DS-8 = RM-11**: Reference stores read-write (fix once)
3. **RM-10 = CQ-1**: review_diff double-load (fix once)
4. **RM-12 = CQ-3**: find_transitive_callers N+1 (fix once)
5. **PERF-14 = RM-15**: Cross-index bridge sequential (fix once)

**Batch 3 Total: 20 findings (14 unique after dedup)**
